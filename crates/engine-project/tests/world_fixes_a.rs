//! Regression tests for the world critic review (task parity-20261025/world/fixes-a):
//! per-entity failures, asset-mode foliage under a shifted origin, absolute foliage placement,
//! origin-shift behaviour of PCG, navigation and water, prefab remapping of world references,
//! f64 water clocks and anchors beyond the absolute limit.
use engine_foliage::{FoliageField, FoliageType, Instance};
use engine_project::simulation::BakeSettings;
use engine_project::world::coordinates::{self, WorldOrigin};
use engine_project::world::pcg::{PcgStatus, PcgVolume};
use engine_project::world::water::{self, WaterBodyComponent, WaterClock, WaterKind, WaterMesh};
use engine_project::world::{foliage, terrain, tilemap};
use engine_project::{EditOperation, InputSnapshot, Project, Runtime, SceneDocument, Transform};
use engine_terrain::{Heightfield, asset};
use engine_tilemap::{TileDef, TilesetDocument};
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

const DT: f32 = 0.02;
const MESH: &str = "CGMESH 1\nvertex -0.2 0 0 0 1\nvertex 0.2 0 0 1 1\nvertex 0 1 0 0.5 0\n";

fn entity(id: &str, position: [f32; 3], custom: &[(&str, Value)]) -> Value {
    let mut components = json!({ "Transform": { "position": position } });
    if !custom.is_empty() {
        let map: serde_json::Map<String, Value> = custom
            .iter()
            .map(|(n, d)| ((*n).to_owned(), json!({"version":1,"data":d})))
            .collect();
        components["custom"] = Value::Object(map);
    }
    json!({"id": id, "name": id, "components": components})
}

struct Scene {
    _dir: tempfile::TempDir,
    runtime: Runtime,
}
fn project(
    entities: Vec<Value>,
    assets: Vec<Value>,
    files: &[(&str, Vec<u8>)],
) -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("grass.cgmesh"), MESH).unwrap();
    for (name, bytes) in files {
        fs::write(dir.path().join(name), bytes).unwrap();
    }
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    let mut all = vec![json!({"id":"grass","kind":"mesh","path":"grass.cgmesh"})];
    all.extend(assets);
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"fixes-a","startup_scene":"scene.json","assets":all}).to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    (dir, project)
}
fn scene(entities: Vec<Value>, assets: Vec<Value>, files: &[(&str, Vec<u8>)]) -> Scene {
    let (dir, project) = project(entities, assets, files);
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    Scene { _dir: dir, runtime }
}
impl Scene {
    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.runtime.tick(DT, InputSnapshot::default()).unwrap();
        }
    }
    fn world(&self) -> &bevy_ecs::world::World {
        self.runtime.app().world()
    }
    fn world_mut(&mut self) -> &mut bevy_ecs::world::World {
        self.runtime.app_mut().world_mut()
    }
    fn entity(&self, id: &str) -> bevy_ecs::entity::Entity {
        self.runtime
            .entity(id)
            .unwrap_or_else(|| panic!("entity {id}"))
    }
}

/// A `side` x `side` terrain, spacing 1, height `a + b c + d r`.
fn plane(side: u32, a: f32, b: f32, d: f32) -> Vec<u8> {
    let heights = (0..side * side)
        .map(|i| a + b * (i % side) as f32 + d * (i / side) as f32)
        .collect();
    asset::encode(&Heightfield::from_heights(side, side, 1.0, heights).unwrap())
}
fn land_asset() -> Value {
    json!({"id":"land","kind":"terrain","path":"land.terrain"})
}

// ---- M1: capacity overflow is local to the entity ----------------------------------------------

#[test]
fn the_17th_terrain_is_skipped_with_a_reason_and_the_runtime_keeps_ticking() {
    let mut entities = vec![entity("camera", [0.0, 5.0, 0.0], &[])];
    for i in 0..17 {
        entities.push(entity(
            &format!("t{i:02}"),
            [i as f32 * 40.0, 0.0, 0.0],
            &[(
                "engine.Terrain",
                json!({"asset":"land","render_mode":"gpu","collision":{"enabled":false}}),
            )],
        ));
    }
    let mut s = scene(
        entities,
        vec![land_asset()],
        &[("land.terrain", plane(33, 1.0, 0.0, 0.0))],
    );
    s.ticks(3);
    for i in 0..16 {
        let e = s.entity(&format!("t{i:02}"));
        assert!(terrain::status(s.world(), e).is_some(), "terrain {i} runs");
        assert_eq!(terrain::failure(s.world(), e), None);
    }
    let last = s.entity("t16");
    assert!(terrain::status(s.world(), last).is_none());
    let reason = terrain::failure(s.world(), last).expect("a reason");
    assert!(reason.contains("At most 16"), "{reason}");
    let log = s.world().resource::<engine_project::runtime::RuntimeLog>();
    let logged = log.0.iter().filter(|l| l.contains("At most 16")).count();
    assert_eq!(logged, 1, "logged once, not every tick: {:?}", log.0);
}

#[test]
fn the_17th_foliage_is_skipped_with_a_reason_and_the_runtime_keeps_ticking() {
    let mut grass = FoliageType::new("grass", "grass");
    grass.density = 0.2;
    let mut entities = vec![entity(
        "land",
        [0.0; 3],
        &[(
            "engine.Terrain",
            json!({"asset":"land","render_mode":"gpu","collision":{"enabled":false}}),
        )],
    )];
    for i in 0..17 {
        entities.push(entity(
            &format!("f{i:02}"),
            [0.0; 3],
            &[(
                "engine.Foliage",
                json!({"types":[grass],"terrain":"land","center":[16,8,16],"size":[32,32,32],
                    "render_mode":"gpu"}),
            )],
        ));
    }
    let mut s = scene(
        entities,
        vec![land_asset()],
        &[("land.terrain", plane(33, 1.0, 0.0, 0.0))],
    );
    s.ticks(3);
    for i in 0..16 {
        let e = s.entity(&format!("f{i:02}"));
        let status = foliage::status(s.world(), e).expect("runs");
        assert!(status.instances > 0, "{status:?}");
    }
    let last = s.entity("f16");
    assert!(foliage::status(s.world(), last).is_none());
    let reason = foliage::failure(s.world(), last).expect("a reason");
    assert!(reason.contains("At most 16"), "{reason}");
}

#[test]
fn the_65th_water_body_is_skipped_with_a_reason_and_the_runtime_keeps_ticking() {
    let mut entities = Vec::new();
    for i in 0..65 {
        entities.push(entity(
            &format!("w{i:02}"),
            [i as f32 * 10.0, 0.0, 0.0],
            &[(
                "engine.WaterBody",
                json!({"kind":"lake","half_extent":[2,2],"render_mode":"gpu","waves":{"model":"none"}}),
            )],
        ));
    }
    let mut s = scene(entities, vec![], &[]);
    s.ticks(3);
    for i in 0..64 {
        let e = s.entity(&format!("w{i:02}"));
        assert!(water::status(s.world(), e).is_some(), "body {i} runs");
    }
    let last = s.entity("w64");
    assert!(water::status(s.world(), last).is_none());
    let reason = water::failure(s.world(), last).expect("a reason");
    assert!(
        reason.contains("At most 64") || reason.contains("at most 64"),
        "{reason}"
    );
}

fn tileset_json() -> Vec<u8> {
    let mut doc = TilesetDocument::new("test", "atlas", [16, 16]);
    doc.tiles = vec![TileDef {
        id: 1,
        name: "tile".into(),
        source: [0, 0, 16, 16],
        collision: engine_tilemap::CollisionKind::None,
        properties: Default::default(),
    }];
    engine_tilemap::Tileset::from_document(doc)
        .unwrap()
        .to_json()
        .into_bytes()
}

#[test]
fn the_65th_tilemap_is_skipped_with_a_reason_and_the_runtime_keeps_ticking() {
    let mut entities = vec![entity("camera", [0.0; 3], &[])];
    for i in 0..65 {
        entities.push(entity(
            &format!("m{i:02}"),
            [i as f32, 0.0, 0.0],
            &[("engine.Tilemap", json!({"tileset":"tiles"}))],
        ));
    }
    let mut atlas = Vec::new();
    image::RgbaImage::new(16, 16)
        .write_to(
            &mut std::io::Cursor::new(&mut atlas),
            image::ImageFormat::Png,
        )
        .unwrap();
    let mut s = scene(
        entities,
        vec![
            json!({"id":"atlas","kind":"texture","path":"atlas.png"}),
            json!({"id":"tiles","kind":"tileset","path":"tiles.json"}),
        ],
        &[("atlas.png", atlas), ("tiles.json", tileset_json())],
    );
    s.ticks(3);
    let first = s.entity("m00");
    assert!(tilemap::status(s.world(), first).is_some());
    assert_eq!(tilemap::failure(s.world(), first), None);
    let last = s.entity("m64");
    assert!(tilemap::status(s.world(), last).is_none());
    let reason = tilemap::failure(s.world(), last).expect("a reason");
    assert!(reason.contains("At most 64"), "{reason}");
}

// ---- M5 / M8: foliage frames --------------------------------------------------------------------

fn grass_type() -> FoliageType {
    let mut t = FoliageType::new("grass", "grass");
    t.density = 0.4;
    t
}

fn instances_absolute(s: &mut Scene, id: &str) -> Vec<[f64; 3]> {
    let e = s.entity(id);
    let offset = coordinates::origin(s.world());
    let field = foliage::field(s.world_mut(), e).unwrap();
    let mut out: Vec<[f64; 3]> = field
        .iter()
        .map(|i| std::array::from_fn(|a| f64::from(i.position[a]) + offset[a]))
        .collect();
    out.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[2].total_cmp(&b[2])));
    out
}

#[test]
fn asset_mode_foliage_keeps_its_instances_and_heights_when_the_origin_is_shifted() {
    let types = vec![grass_type()];
    let mut field = FoliageField::new(32.0).unwrap();
    let mut wanted = Vec::new();
    for i in 0..24 {
        let (x, z) = (6.0 + 2.0 * i as f32, 10.0 + 1.5 * i as f32);
        field.insert(Instance::new(0, [x, 100.0, z])).unwrap();
        wanted.push((x, z));
    }
    let bytes = engine_foliage::asset::encode(&types, &field).unwrap();
    let mut s = scene(
        vec![
            entity(
                "land",
                [0.0; 3],
                &[(
                    "engine.Terrain",
                    json!({"asset":"land","render_mode":"gpu","collision":{"enabled":false}}),
                )],
            ),
            entity(
                "plants",
                [0.0; 3],
                &[(
                    "engine.Foliage",
                    json!({"mode":"asset","instances":"inst","terrain":"land","render_mode":"gpu"}),
                )],
            ),
        ],
        vec![
            land_asset(),
            json!({"id":"inst","kind":"foliage","path":"inst.foliage"}),
        ],
        &[
            ("land.terrain", plane(129, 1.0, 0.1, 0.05)),
            ("inst.foliage", bytes),
        ],
    );
    // The world is at offset (2048, 0, 0) before the foliage loads its absolute positions.
    coordinates::rebase_now(s.world_mut(), [2048.0, 0.0, 0.0]).unwrap();
    s.ticks(3);
    let found = instances_absolute(&mut s, "plants");
    assert_eq!(
        found.len(),
        wanted.len(),
        "no instance is lost to the frame mix-up"
    );
    for (p, (x, z)) in found.iter().zip(&wanted) {
        assert!(
            (p[0] - f64::from(*x)).abs() < 1e-3 && (p[2] - f64::from(*z)).abs() < 1e-3,
            "{p:?}"
        );
        let height = 1.0 + 0.1 * f64::from(*x) + 0.05 * f64::from(*z);
        assert!((p[1] - height).abs() < 1e-3, "{} vs {height}", p[1]);
    }
}

#[test]
fn generated_foliage_is_the_same_absolute_forest_at_any_origin_offset() {
    let entities = || {
        vec![
            entity(
                "land",
                [0.0; 3],
                &[(
                    "engine.Terrain",
                    json!({"asset":"land","render_mode":"gpu","collision":{"enabled":false}}),
                )],
            ),
            entity(
                "plants",
                [0.0; 3],
                &[(
                    "engine.Foliage",
                    json!({"types":[grass_type()],"terrain":"land",
                "seed":7,"center":[32,16,32],"size":[64,64,64],"render_mode":"gpu"}),
                )],
            ),
        ]
    };
    let files = || vec![("land.terrain", plane(65, 1.0, 0.05, 0.02))];
    let mut near = scene(entities(), vec![land_asset()], &files());
    near.ticks(2);
    let mut far = scene(entities(), vec![land_asset()], &files());
    coordinates::rebase_now(far.world_mut(), [2048.0, 0.0, -512.0]).unwrap();
    far.ticks(2);
    let a = instances_absolute(&mut near, "plants");
    let b = instances_absolute(&mut far, "plants");
    assert!(a.len() > 100, "{} instances", a.len());
    assert_eq!(a.len(), b.len());
    for p in &a {
        let nearest = b
            .iter()
            .map(|q| (0..3).map(|i| (p[i] - q[i]).abs()).fold(0.0, f64::max))
            .fold(f64::INFINITY, f64::min);
        assert!(nearest < 1e-3, "{p:?}: nearest partner {nearest} m away");
    }
}

// ---- m1: a water body without an authored id ----------------------------------------------------

#[test]
fn a_water_body_without_an_authored_id_gets_no_dangling_mesh_and_does_not_fail_ticks() {
    let mut s = scene(vec![entity("camera", [0.0; 3], &[])], vec![], &[]);
    let body = WaterBodyComponent {
        kind: WaterKind::Lake,
        half_extent: [4.0, 4.0],
        render_mode: water::WaterRenderMode::Cpu,
        ..WaterBodyComponent::default()
    };
    let e = s.world_mut().spawn((Transform::default(), body)).id();
    s.ticks(3);
    let status = water::status(s.world(), e).expect("simulated");
    assert_eq!(status.render_path, "hidden");
    assert!(
        status.notes.iter().any(|n| n.contains("authored ID")),
        "{:?}",
        status.notes
    );
    let meshes = s.world_mut().query::<&WaterMesh>().iter(s.world()).count();
    assert_eq!(meshes, 0);
}

// ---- m2: an anchor beyond MAX_ABSOLUTE -----------------------------------------------------------

#[test]
fn an_anchor_beyond_the_absolute_limit_blocks_the_rebase_instead_of_failing_the_tick() {
    let mut s = scene(
        vec![
            entity(
                "rules",
                [0.0; 3],
                &[(
                    "engine.LargeWorld",
                    json!({"threshold":1000.0,"snap":512,"anchor":"far"}),
                )],
            ),
            entity("far", [3.0e12, 0.0, 0.0], &[]),
        ],
        vec![],
        &[],
    );
    s.ticks(3);
    let status = coordinates::status(s.world()).expect("status");
    assert_eq!(status.shifts, 0);
    let blocked = status.blocked.expect("a reason");
    assert!(blocked.contains("1e12"), "{blocked}");
    assert!(!status.diagnostics.is_empty());
}

// ---- m3 / m5: terrain across a shift ------------------------------------------------------------

fn noise_graph() -> Vec<u8> {
    use engine_pcg::{GraphBuilder, Registry};
    let registry = Registry::builtin();
    GraphBuilder::new(&registry, 4)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node("n", "noise_field", [("frequency", json!(0.21))])
        .node("keep", "density_filter", [("threshold", json!(0.55))])
        .node(
            "spawn",
            "spawner",
            [("entries", json!([{"asset": "grass", "kind": "mesh"}]))],
        )
        .connect("grid.points", "keep.points")
        .connect("n.field", "keep.field")
        .connect("keep.points", "spawn.points")
        .build()
        .unwrap()
        .to_json()
        .into_bytes()
}

fn terrain_scene() -> Scene {
    scene(
        vec![
            entity("camera", [16.0, 8.0, 16.0], &[]),
            entity(
                "land",
                [0.0; 3],
                &[(
                    "engine.Terrain",
                    json!({"asset":"land","render_mode":"cpu","lod":{"node_resolution":16}}),
                )],
            ),
            entity(
                "forest",
                [16.0, 0.0, 16.0],
                &[(
                    "engine.PcgVolume",
                    json!({"graph":"noise","seed":7,"size":[16,64,16],"surface":"terrain"}),
                )],
            ),
        ],
        vec![
            land_asset(),
            json!({"id":"noise","kind":"pcg","path":"noise.pcg.json"}),
        ],
        &[
            ("land.terrain", plane(33, 2.0, 0.0, 0.0)),
            ("noise.pcg.json", noise_graph()),
        ],
    )
}

fn generated_pcg(s: &Scene) -> Vec<[i64; 3]> {
    let mut out = Vec::new();
    for n in 0..100_000 {
        let Some(entity) = s.runtime.entity(&format!("pcg:forest:{n}")) else {
            break;
        };
        let p = coordinates::absolute_position(s.world(), entity).unwrap();
        out.push(p.map(|v| (v * 1000.0).round() as i64));
    }
    out.sort_unstable();
    out
}

#[test]
fn an_origin_shift_does_not_regenerate_pcg_volumes_standing_on_a_terrain() {
    let mut s = terrain_scene();
    s.ticks(10);
    let forest = s.entity("forest");
    let before = s.world().get::<PcgVolume>(forest).unwrap().regenerate;
    let placed = generated_pcg(&s);
    assert!(!placed.is_empty());
    coordinates::rebase_now(s.world_mut(), [512.0, 0.0, -1024.0]).unwrap();
    s.ticks(10);
    let after = s.world().get::<PcgVolume>(forest).unwrap().regenerate;
    assert_eq!(
        before, after,
        "a pure translation of the origin is not a ground change"
    );
    let moved = generated_pcg(&s);
    assert_eq!(moved.len(), placed.len());
    for (p, q) in placed.iter().zip(&moved) {
        assert!((0..3).all(|i| (p[i] - q[i]).abs() <= 2), "{p:?} vs {q:?}");
    }
    assert!(
        s.world().resource::<PcgStatus>().0["forest"]
            .diagnostics
            .is_empty()
    );
    // A real edit of the ground still regenerates.
    let land = s.entity("land");
    terrain::with_heightfield(s.world_mut(), land, |f| f.set_height(3, 3, 9.0).unwrap()).unwrap();
    s.ticks(3);
    assert!(s.world().get::<PcgVolume>(forest).unwrap().regenerate > after);
}

#[test]
fn terrain_navigation_input_is_absolute_across_an_origin_shift() {
    let mut s = terrain_scene();
    s.ticks(2);
    let land = s.entity("land");
    let settings = BakeSettings {
        cell_size: 1.0,
        ..BakeSettings::default()
    };
    let region = Some([[2.0, 2.0], [10.0, 10.0]]);
    let before = terrain::navigation_input(s.world_mut(), land, settings.clone(), region).unwrap();
    coordinates::rebase_now(s.world_mut(), [512.0, 0.0, -1024.0]).unwrap();
    s.ticks(2);
    let after = terrain::navigation_input(s.world_mut(), land, settings.clone(), region).unwrap();
    assert_eq!(before.vertices.len(), after.vertices.len());
    for (a, b) in before.vertices.iter().zip(&after.vertices) {
        assert!((0..3).all(|i| (a[i] - b[i]).abs() < 1e-3), "{a:?} vs {b:?}");
    }
}

// ---- m6: prefab remapping of world references ----------------------------------------------------

#[test]
fn prefab_instancing_remaps_foliage_terrain_buoyancy_water_and_large_world_anchor() {
    let prefab = json!({"schema":1,"scene_id":"prefab","entities":[
        entity("land", [0.0; 3], &[("engine.Terrain", json!({"asset":"land"}))]),
        entity("plants", [0.0; 3], &[("engine.Foliage", json!({"types":[grass_type()],"terrain":"land"}))]),
        entity("meadow", [0.0; 3], &[("engine.Foliage", json!({"types":[grass_type()],"terrain":"","surface":"s"}))]),
        entity("pond", [0.0; 3], &[("engine.WaterBody", json!({"kind":"lake","half_extent":[4,4]}))]),
        entity("boat", [0.0; 3], &[("engine.PhysicsBody3D", json!({})), ("engine.Buoyancy", json!({"water":"pond"}))]),
        entity("anyboat", [0.0; 3], &[("engine.PhysicsBody3D", json!({})), ("engine.Buoyancy", json!({"water":""}))]),
        entity("rules", [0.0; 3], &[("engine.LargeWorld", json!({"anchor":"boat"}))]),
    ]});
    let (dir, _) = project(
        vec![entity("camera", [0.0; 3], &[])],
        vec![land_asset()],
        &[("land.terrain", plane(33, 1.0, 0.0, 0.0))],
    );
    fs::write(dir.path().join("prefab.json"), prefab.to_string()).unwrap();
    let manifest = dir.path().join("project.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"prefab","kind":"prefab","path":"prefab.json"}));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut document = SceneDocument::new(Project::load(&manifest).unwrap());
    let before = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "prefab".into(),
                prefix: "copy:".into(),
                parent: None,
            },
            &before,
        )
        .unwrap();
    let scene = document.snapshot();
    let data = |id: &str, name: &str| {
        scene
            .entities
            .iter()
            .find(|e| e.id == id)
            .unwrap()
            .components
            .custom[name]
            .data
            .clone()
    };
    assert_eq!(
        data("copy:plants", "engine.Foliage")["terrain"],
        "copy:land"
    );
    assert_eq!(
        data("copy:meadow", "engine.Foliage")["terrain"],
        "",
        "none stays none"
    );
    assert_eq!(data("copy:boat", "engine.Buoyancy")["water"], "copy:pond");
    assert_eq!(
        data("copy:anyboat", "engine.Buoyancy")["water"],
        "",
        "any stays any"
    );
    assert_eq!(
        data("copy:rules", "engine.LargeWorld")["anchor"],
        "copy:boat"
    );
    // A dangling reference is a validation error now.
    let mut bad = scene.clone();
    bad.entities
        .iter_mut()
        .find(|e| e.id == "copy:plants")
        .unwrap()
        .components
        .custom
        .get_mut("engine.Foliage")
        .unwrap()
        .data["terrain"] = json!("ghost");
    assert!(document_project(&manifest).validate_scene(&bad).is_err());
}
fn document_project(manifest: &std::path::Path) -> Project {
    Project::load(manifest).unwrap()
}

// ---- m7 / m9: water clock and origin shifts ---------------------------------------------------------

fn wave_body(waves: Value) -> Vec<Value> {
    vec![entity(
        "sea",
        [0.0; 3],
        &[(
            "engine.WaterBody",
            json!({"kind":"lake","half_extent":[60,60],"render_mode":"gpu","waves":waves}),
        )],
    )]
}
fn surface_at_clock(s: &mut Scene, time: f64, points: &[(f32, f32)]) -> Vec<f32> {
    s.world_mut().resource_mut::<WaterClock>().time = time - f64::from(DT);
    s.ticks(1);
    points
        .iter()
        .map(|(x, z)| {
            water::water_at(s.world(), Vec3::new(*x, -1.0, *z))
                .unwrap()
                .surface_height
        })
        .collect()
}
const POINTS: [(f32, f32); 4] = [(0.0, 0.0), (7.3, -2.1), (-12.0, 9.5), (20.5, 20.5)];

#[test]
fn gerstner_waves_after_ten_hours_match_the_same_phase_early_in_the_run() {
    // One wave, so its own period is exact for the comparison.
    let single = json!({"model":"gerstner","gerstner":[
        {"direction_degrees":30,"wavelength":20,"amplitude":0.5,"steepness":0.3,"phase":0.4}]});
    let set = engine_water::GerstnerSet::new(
        &[engine_water::GerstnerWave {
            direction_degrees: 30.0,
            wavelength: 20.0,
            amplitude: 0.5,
            steepness: 0.3,
            phase: 0.4,
        }],
        None,
        engine_water::GRAVITY,
    )
    .unwrap();
    let (_, omega) = set.dispersion(0).unwrap();
    let period = std::f64::consts::TAU / f64::from(omega);
    let late = 36_000.017_f64;
    let early = late.rem_euclid(period);
    let mut a = scene(wave_body(single.clone()), vec![], &[]);
    let mut b = scene(wave_body(single), vec![], &[]);
    let at_late = surface_at_clock(&mut a, late, &POINTS);
    let at_early = surface_at_clock(&mut b, early, &POINTS);
    for (l, e) in at_late.iter().zip(&at_early) {
        assert!((l - e).abs() < 1e-4, "{l} vs {e}");
    }
    assert!(
        at_late.iter().any(|h| h.abs() > 0.05),
        "the surface actually moves: {at_late:?}"
    );
}

#[test]
fn a_looping_fft_ocean_after_ten_hours_equals_the_surface_at_t_mod_the_loop_period() {
    let fft =
        json!({"model":"fft","fft":{"resolution":32,"patch_length":64,"loop_period":30,"seed":3}});
    let late = 36_000.017_f64;
    let early = late.rem_euclid(30.0);
    let mut a = scene(wave_body(fft.clone()), vec![], &[]);
    let mut b = scene(wave_body(fft), vec![], &[]);
    let at_late = surface_at_clock(&mut a, late, &POINTS);
    let at_early = surface_at_clock(&mut b, early, &POINTS);
    for (l, e) in at_late.iter().zip(&at_early) {
        assert!((l - e).abs() < 1e-4, "{l} vs {e}");
    }
    assert!(at_late.iter().any(|h| h.abs() > 0.01), "{at_late:?}");
}

#[test]
fn a_floating_body_and_the_water_surface_are_unchanged_in_absolute_terms_across_a_rebase() {
    let build = || {
        scene(
            vec![
                entity(
                    "settings",
                    [0.0; 3],
                    &[("engine.PhysicsSettings", json!({"fixed_timestep": DT}))],
                ),
                entity(
                    "sea",
                    [0.0; 3],
                    &[(
                        "engine.WaterBody",
                        json!({"kind":"lake","half_extent":[60,60],"render_mode":"gpu",
                    "waves":{"model":"gerstner","gerstner":[{"direction_degrees":20,"wavelength":18,"amplitude":0.3,"steepness":0.3}]}}),
                    )],
                ),
                entity(
                    "box",
                    [3.0, 0.6, 1.0],
                    &[
                        (
                            "engine.PhysicsBody3D",
                            json!({"shape":"box","half_extents":[0.5,0.5,0.5],"material":{"density":0.5}}),
                        ),
                        ("engine.Buoyancy", json!({})),
                    ],
                ),
            ],
            vec![],
            &[],
        )
    };
    let mut control = build();
    let mut shifted = build();
    control.ticks(150);
    shifted.ticks(150);
    coordinates::rebase_now(shifted.world_mut(), [1024.0, 0.0, -512.0]).unwrap();
    control.ticks(60);
    shifted.ticks(60);
    let absolute =
        |s: &Scene, id: &str| coordinates::absolute_position(s.world(), s.entity(id)).unwrap();
    let (a, b) = (absolute(&control, "box"), absolute(&shifted, "box"));
    assert!((0..3).all(|i| (a[i] - b[i]).abs() < 5e-3), "{a:?} vs {b:?}");
    let offset = coordinates::origin(shifted.world());
    assert_eq!(offset, [1024.0, 0.0, -512.0]);
    for (x, z) in POINTS {
        let here = water::water_at(control.world(), Vec3::new(x, -1.0, z)).unwrap();
        let there = water::water_at(
            shifted.world(),
            Vec3::new(
                (f64::from(x) - offset[0]) as f32,
                -1.0,
                (f64::from(z) - offset[2]) as f32,
            ),
        )
        .unwrap();
        assert!((here.surface_height - there.surface_height).abs() < 1e-3);
        assert!((here.normal - there.normal).length() < 1e-3);
    }
    let origin = *shifted.world().resource::<WorldOrigin>();
    assert_eq!(origin.offset, offset);
}
