//! Authored `engine.WaterBody` / `engine.Buoyancy` scenes -> Runtime -> floating and sinking
//! bodies, river drift, underwater camera state, CPU fallback meshes, the GPU render set and
//! shoreline attenuation over a terrain.
use engine_project::simulation::physics;
use engine_project::world::water::{
    self, UnderwaterView, WaterClock, WaterGpuRenderer, WaterRenderSet,
};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_terrain::{Heightfield, asset};
use engine_water::{GRAVITY, GerstnerSet, GerstnerWave, NoGround};
use glam::{Vec2, Vec3};
use serde_json::{Value, json};
use std::fs;

const DT: f32 = 0.02;

fn entity(id: &str, position: [f32; 3], components: Value) -> Value {
    let mut components = components;
    components["Transform"] = json!({ "position": position });
    json!({"id": id, "name": id, "components": components})
}
fn custom(pairs: &[(&str, Value)]) -> Value {
    let map: serde_json::Map<String, Value> = pairs
        .iter()
        .map(|(n, d)| ((*n).to_owned(), json!({"version":1,"data":d})))
        .collect();
    json!({ "custom": map })
}
fn water(data: Value) -> (&'static str, Value) {
    ("engine.WaterBody", data)
}
fn calm_lake() -> Value {
    json!({"kind":"lake","half_extent":[20,20],"waves":{"model":"none"}})
}
fn body(density: f32) -> (&'static str, Value) {
    (
        "engine.PhysicsBody3D",
        json!({"material":{"density":density}}),
    )
}
fn buoyancy(data: Value) -> (&'static str, Value) {
    ("engine.Buoyancy", data)
}

struct Scene {
    _dir: tempfile::TempDir,
    runtime: Runtime,
}
fn scene(entities: Vec<Value>, assets: Vec<Value>, files: &[(&str, Vec<u8>)]) -> Scene {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        fs::write(dir.path().join(name), bytes).unwrap();
    }
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"water","startup_scene":"scene.json","assets":assets}).to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let runtime = Runtime::new(project).unwrap();
    Scene { _dir: dir, runtime }
}
impl Scene {
    fn start(mut self) -> Self {
        self.runtime.start().unwrap();
        self
    }
    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.runtime.tick(DT, InputSnapshot::default()).unwrap();
        }
    }
    fn state(&self, id: &str) -> physics::BodyState {
        let e = self.runtime.entity(id).unwrap();
        physics::body_state(self.runtime.app().world(), e).unwrap()
    }
}

/// A 1 m box of density 0.5 dropped from 1 m above calm water settles with its centre on the
/// surface: half submerged within 0.03 (centre height within 0.03 m).
#[test]
fn half_density_box_floats_half_submerged() {
    let mut s = scene(
        vec![
            entity("lake", [0.0, 0.0, 0.0], custom(&[water(calm_lake())])),
            entity(
                "crate",
                [0.0, 1.0, 0.0],
                custom(&[body(0.5), buoyancy(json!({"linear_drag":2.0}))]),
            ),
        ],
        vec![],
        &[],
    )
    .start();
    s.ticks(575);
    let before = s.state("crate").position;
    s.ticks(25);
    let state = s.state("crate");
    let e = s.runtime.entity("crate").unwrap();
    let status = water::buoyancy_status(s.runtime.app().world(), e).unwrap();
    eprintln!(
        "centre y {:.4}, submerged {:.4}, volume {:.4} m^3, force {:.3} N, moved {:.4} m in 0.5 s",
        state.position.y,
        status.submerged_fraction,
        status.submerged_volume,
        status.buoyant_force,
        state.position.distance(before)
    );
    assert_eq!(status.points, 8);
    assert!(state.position.y.abs() < 0.03, "{state:?}");
    assert!((status.submerged_fraction - 0.5).abs() < 0.03, "{status:?}");
    // At rest: the centre moved less than 1 cm over the last half second.
    assert!(
        state.position.distance(before) < 0.01,
        "{before} -> {}",
        state.position
    );
    // Archimedes: the buoyant force balances the 0.5 kg weight.
    assert!((status.buoyant_force - 0.5 * GRAVITY).abs() < 0.05 * GRAVITY);
    // Upright: buoyancy at the points keeps the box level.
    assert!(state.rotation.angle_between(glam::Quat::IDENTITY) < 0.05);
}

#[test]
fn dense_body_sinks() {
    let mut s = scene(
        vec![
            entity("lake", [0.0, 0.0, 0.0], custom(&[water(calm_lake())])),
            entity(
                "anchor",
                [0.0, 0.0, 0.0],
                custom(&[body(3.0), buoyancy(json!({}))]),
            ),
        ],
        vec![],
        &[],
    )
    .start();
    s.ticks(150);
    let state = s.state("anchor");
    let e = s.runtime.entity("anchor").unwrap();
    let status = water::buoyancy_status(s.runtime.app().world(), e).unwrap();
    eprintln!(
        "after 3 s: y {:.2}, vy {:.2}",
        state.position.y, state.linear_velocity.y
    );
    assert!(state.position.y < -3.0, "{state:?}");
    assert!(state.linear_velocity.y < -0.5);
    assert!((status.submerged_fraction - 1.0).abs() < 1.0e-6);
    // Fully submerged: buoyancy is rho g V = 9.81 N, a third of the weight.
    assert!((status.buoyant_force - GRAVITY).abs() < 1.0e-3);
}

/// A floating box in a river (flowing +X, 1 m/s mean, 1.5 m/s on the centreline) drifts
/// downstream and reaches the local flow speed.
#[test]
fn floating_body_drifts_downstream_with_the_flow() {
    let river = json!({"kind":"river","waves":{"model":"none"},"river":{
        "points":[[-10,0,0],[60,0,0]],"widths":[8],"depths":[2],"flow_speed":1.0}});
    let mut s = scene(
        vec![
            entity("river", [0.0, 0.0, 0.0], custom(&[water(river)])),
            entity(
                "raft",
                [0.0, 0.0, 0.0],
                custom(&[body(0.5), buoyancy(json!({"flow_drag":2.0}))]),
            ),
        ],
        vec![],
        &[],
    )
    .start();
    s.ticks(300);
    let state = s.state("raft");
    let e = s.runtime.entity("river").unwrap();
    let status = water::status(s.runtime.app().world(), e).unwrap();
    // Points at z = +-0.25 m of an 8 m channel: 1.5 * (1 - (0.5 / 8)^2) = 1.494 m/s.
    let expected = 1.5 * (1.0 - (0.5f32 / 8.0).powi(2));
    eprintln!(
        "after 6 s: x {:.2} m, vx {:.3} m/s (flow {expected:.3}), z {:.3}, discharge {:?}",
        state.position.x, state.linear_velocity.x, state.position.z, status.discharge
    );
    assert!((status.discharge.unwrap() - 16.0).abs() < 1.0e-4);
    assert!(state.position.x > 6.0, "{state:?}");
    assert!((state.linear_velocity.x - expected).abs() < 0.05 * expected);
    assert!(state.position.z.abs() < 0.1);
    assert!(state.position.y.abs() < 0.05, "still floating");
}

#[test]
fn camera_below_the_surface_reports_underwater_fog() {
    let lake = json!({"kind":"lake","half_extent":[20,20],"waves":{"model":"none"},
        "optics":{"fog_color":[0.1,0.3,0.2],"fog_density":0.4}});
    let mut s = scene(
        vec![
            entity("lake", [0.0, 2.0, 0.0], custom(&[water(lake)])),
            entity("camera", [1.0, -1.0, 3.0], json!({"Camera3D": {}})),
        ],
        vec![],
        &[],
    )
    .start();
    s.ticks(1);
    let view = s.runtime.app().world().resource::<UnderwaterView>().clone();
    let under = view
        .underwater
        .expect("camera 3 m below the surface at y = 2");
    assert_eq!(view.water.as_deref(), Some("lake"));
    assert!((under.depth - 3.0).abs() < 1.0e-4);
    assert_eq!(under.fog_color, [0.1, 0.3, 0.2]);
    assert_eq!(under.fog_density, 0.4);
    assert!((under.fog_factor(5.0) - (1.0 - (-2.0f32).exp())).abs() < 1.0e-6);
    // The native query agrees.
    let (id, at) =
        water::underwater_at(s.runtime.app().world(), Vec3::new(1.0, -1.0, 3.0)).unwrap();
    assert_eq!((id.as_str(), at), ("lake", under));
    // Above the surface or outside the lake nothing is underwater.
    assert!(water::underwater_at(s.runtime.app().world(), Vec3::new(1.0, 2.5, 3.0)).is_none());
    assert!(water::underwater_at(s.runtime.app().world(), Vec3::new(30.0, -1.0, 3.0)).is_none());
}

fn surface_meshes(runtime: &Runtime) -> Vec<engine_project::rendering3d::MeshDraw3D> {
    runtime
        .extract_scene_frame()
        .unwrap()
        .meshes
        .into_iter()
        .filter(|m| m.id.starts_with("water:"))
        .collect()
}

/// Without the GPU renderer the surface is one transparent CPU mesh that animates; with the
/// marker it moves to the render set and the mesh entity goes away (no double draw).
#[test]
fn cpu_fallback_mesh_animates_and_yields_to_the_gpu_feature() {
    let lake = json!({"kind":"lake","half_extent":[16,16],"grid_resolution":32});
    let mut s = scene(
        vec![
            entity("lake", [0.0, 0.0, 0.0], custom(&[water(lake)])),
            entity("camera", [0.0, 10.0, 20.0], json!({"Camera3D": {}})),
        ],
        vec![],
        &[],
    )
    .start();
    s.ticks(1);
    let meshes = surface_meshes(&s.runtime);
    assert_eq!(meshes.len(), 1);
    let first = &meshes[0];
    assert_eq!(first.id, "water:lake/surface");
    assert!(first.material.transparent);
    assert!(first.material.tint[3] < 1.0);
    assert_eq!(first.mesh.vertices.len(), 33 * 33);
    assert_eq!(first.mesh.indices.len(), 32 * 32 * 6);
    // Default Gerstner waves: vertex heights match the analytic set at the clock's time.
    let world = s.runtime.app().world();
    let t = world.resource::<WaterClock>().time as f32;
    let set = GerstnerSet::deep(&water::WaveSettings::default().gerstner).unwrap();
    let v = &first.mesh.vertices[17 * 33 + 5];
    let x0 = water::grid_point(Vec2::splat(16.0), 32, 5, 17);
    let expected = set.evaluate(x0, t, 1.0).offset;
    assert!((v.position[1] - expected.y).abs() < 1.0e-5);
    assert!((v.position[0] - (x0.x + expected.x)).abs() < 1.0e-5);
    let lake_entity = s.runtime.entity("lake").unwrap();
    assert_eq!(
        water::status(world, lake_entity).unwrap().render_path,
        "cpu-mesh"
    );
    s.ticks(10);
    let later = surface_meshes(&s.runtime);
    assert_ne!(later[0].mesh.vertices, first.mesh.vertices, "waves move");
    // GPU renderer installed: no CPU mesh, one render item.
    s.runtime
        .app_mut()
        .world_mut()
        .insert_resource(WaterGpuRenderer);
    s.ticks(1);
    assert!(surface_meshes(&s.runtime).is_empty(), "no double draw");
    let world = s.runtime.app().world();
    let set = world.resource::<WaterRenderSet>();
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].id, "lake");
    assert_eq!(set.items[0].grid_resolution, 32);
    assert!(set.items[0].shore.is_none(), "no terrain: open water");
    assert_eq!(
        water::status(world, lake_entity).unwrap().render_path,
        "gpu-feature"
    );
}

/// Waves fade and foam appears toward a terrain beach rising through the water level.
#[test]
fn shoreline_follows_the_terrain_height() {
    // 33 x 33 samples, ground rising from -4 (x = -16) to +4 (x = +16).
    let heights = (0..33 * 33)
        .map(|i| -4.0 + 0.25 * (i % 33) as f32)
        .collect();
    let field = Heightfield::from_heights(33, 33, 1.0, heights).unwrap();
    let lake = json!({"kind":"lake","half_extent":[16,16],"grid_resolution":16,
        "waves":{"model":"gerstner","gerstner":[{"wavelength":6.0,"amplitude":0.3,"steepness":0.5}]}});
    let mut s = scene(
        vec![
            entity("lake", [0.0, 0.0, 0.0], custom(&[water(lake)])),
            entity("camera", [0.0, 20.0, 30.0], json!({"Camera3D": {}})),
            entity(
                "beach",
                [-16.0, 0.0, -16.0],
                custom(&[(
                    "engine.Terrain",
                    json!({"asset":"beach","collision":{"enabled":false}}),
                )]),
            ),
        ],
        vec![json!({"id":"beach","kind":"terrain","path":"beach.terrain"})],
        &[("beach.terrain", asset::encode(&field))],
    )
    .start();
    s.ticks(1);
    let world = s.runtime.app().world();
    let mut previous = 2.0;
    let mut amplitudes = Vec::new();
    for x in [-14.0f32, -10.0, -6.0, -2.0, 0.0, 6.0] {
        let q = water::water_at(world, Vec3::new(x, -10.0, 0.0)).unwrap();
        eprintln!(
            "x {x}: amplitude {:.3} shore foam {:.3}",
            q.amplitude, q.shore_foam
        );
        assert!(q.amplitude <= previous + 1.0e-6);
        previous = q.amplitude;
        amplitudes.push((q.amplitude, q.shore_foam, q.surface_height));
    }
    // Ground -3.5 at x = -14: 3.5 m deep, nearly full waves; at x = 0 the ground meets the
    // mean level: no waves, full foam; inland the surface stays flat at the mean level.
    assert!(amplitudes[0].0 > 0.9 && amplitudes[0].1 == 0.0);
    assert_eq!(amplitudes[4].0, 0.0);
    assert_eq!(amplitudes[4].1, 1.0);
    assert_eq!(amplitudes[5].2, 0.0);
    let lake_entity = s.runtime.entity("lake").unwrap();
    let status = water::status(world, lake_entity).unwrap();
    assert!(status.shoreline_ground);
    // The CPU mesh carries the attenuation: vertices over dry land are flat.
    let meshes = surface_meshes(&s.runtime);
    let row = &meshes[0].mesh.vertices[8 * 17..9 * 17];
    assert!(row[16].position[1].abs() < 1.0e-6, "x = +16 is inland");
    assert!(
        row.iter().any(|v| v.position[1].abs() > 0.01),
        "waves offshore"
    );
}

#[test]
fn native_queries_match_the_wave_model() {
    let waves =
        json!([{"wavelength":12.0,"amplitude":0.5,"steepness":0.6,"direction_degrees":30.0}]);
    let ocean = json!({"kind":"ocean","waves":{"model":"gerstner","gerstner":waves,"time_scale":2.0},
        "shoreline":{"enabled":false}});
    let mut s = scene(
        vec![entity("sea", [5.0, 1.0, -3.0], custom(&[water(ocean)]))],
        vec![],
        &[],
    )
    .start();
    s.ticks(25);
    let world = s.runtime.app().world();
    let clock = world.resource::<WaterClock>();
    assert_eq!(clock.ticks, 25);
    assert!((clock.time - 0.5).abs() < 1.0e-5);
    let set = GerstnerSet::deep(&[GerstnerWave {
        wavelength: 12.0,
        amplitude: 0.5,
        steepness: 0.6,
        direction_degrees: 30.0,
        phase: 0.0,
    }])
    .unwrap();
    for p in [
        Vec2::new(0.0, 0.0),
        Vec2::new(17.0, 4.5),
        Vec2::new(-300.0, 41.0),
    ] {
        let q = water::water_at(world, Vec3::new(p.x, -20.0, p.y)).unwrap();
        // Local = world - (5, 1, -3); wave time = 2 x 0.5 s.
        let (point, _) = set.sample(p - Vec2::new(5.0, -3.0), 1.0, 1.0);
        assert!(
            (q.surface_height - (1.0 + point.offset.y)).abs() < 1.0e-4,
            "{p}"
        );
        assert!((q.depth - (q.surface_height + 20.0)).abs() < 1.0e-5);
        assert!(q.residual <= engine_water::INVERSE_TOLERANCE);
    }
    let surface = water::surface(world, s.runtime.entity("sea").unwrap()).unwrap();
    assert!(surface.sample(Vec2::ZERO, &NoGround).is_some());
}

#[test]
fn fft_ocean_runs_and_reports_its_spectrum() {
    let ocean = json!({"kind":"ocean","half_extent":[32,32],"grid_resolution":16,
        "waves":{"model":"fft","fft":{"resolution":32,"patch_length":64,"wind_speed":8,"seed":7}}});
    let mut s = scene(
        vec![entity("sea", [0.0, 0.0, 0.0], custom(&[water(ocean)]))],
        vec![],
        &[],
    )
    .start();
    s.ticks(2);
    let world = s.runtime.app().world();
    let e = s.runtime.entity("sea").unwrap();
    let status = water::status(world, e).unwrap();
    let hs = status.significant_height.unwrap();
    eprintln!("FFT ocean Hs {hs:.3} m");
    assert!(hs > 0.1 && hs < 5.0);
    let a = water::water_at(world, Vec3::new(3.0, 0.0, 7.0))
        .unwrap()
        .surface_height;
    s.ticks(20);
    let b = water::water_at(s.runtime.app().world(), Vec3::new(3.0, 0.0, 7.0))
        .unwrap()
        .surface_height;
    assert_ne!(a, b, "the ocean evolves");
    // Tiled: the same height one patch away.
    let world = s.runtime.app().world();
    let here = water::water_at(world, Vec3::new(3.0, 0.0, 7.0)).unwrap();
    let there = water::water_at(world, Vec3::new(67.0, 0.0, -57.0)).unwrap();
    assert!((here.surface_height - there.surface_height).abs() < 1.0e-3);
}

#[test]
fn invalid_water_components_are_rejected() {
    for data in [
        json!({"kind":"lake","waves":{"gerstner":[{"wavelength":10,"amplitude":3}]}}),
        json!({"kind":"river","river":{"points":[[0,0,0]]}}),
        json!({"kind":"ocean","waves":{"model":"fft","fft":{"resolution":100}}}),
        json!({"kind":"ocean","unknown":1}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("scene.json"),
            json!({"schema":1,"scene_id":"main","entities":[
                entity("w", [0.0; 3], custom(&[water(data.clone())]))]})
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("project.json"),
            json!({"schema":1,"name":"w","startup_scene":"scene.json","assets":[]}).to_string(),
        )
        .unwrap();
        let result = Project::load(dir.path().join("project.json")).and_then(Runtime::new);
        assert!(result.is_err(), "{data}");
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            entity("b", [0.0; 3], custom(&[buoyancy(json!({"points":vec![[0,0,0];65]}))]))]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"w","startup_scene":"scene.json","assets":[]}).to_string(),
    )
    .unwrap();
    assert!(
        Project::load(dir.path().join("project.json"))
            .and_then(Runtime::new)
            .is_err()
    );
}
