//! Authored cloth, soft bodies and fluids: entity pins, rigid coupling (one- and two-way),
//! fluid fill/settle/buoyancy, capacity events, component_set editing, render data,
//! debug segments and determinism.
use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{self, Cloth, PhysicsRenderData};
use engine_project::*;
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

const H: f32 = 1. / 60.;

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn settings(extra: Value) -> Value {
    let mut data = json!({"fixed_timestep":H});
    for (key, v) in extra.as_object().into_iter().flatten() {
        data[key] = v.clone();
    }
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn body(id: &str, position: [f32; 3], data: Value) -> Value {
    entity(id, position, json!({"engine.PhysicsBody3D":value(data)}))
}
fn with(id: &str, position: [f32; 3], component: &str, data: Value) -> Value {
    entity(id, position, json!({component:value(data)}))
}
fn fixture(entities: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"deformable-test","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
#[derive(Resource, Default)]
struct Events(Vec<(String, Value)>);
fn record(world: &mut World, _target: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Events>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}
fn runtime_of(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Events::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    runtime
}
fn started(entities: Vec<Value>) -> (tempfile::TempDir, Runtime) {
    let dir = fixture(entities);
    let runtime = runtime_of(&dir);
    (dir, runtime)
}
fn rejection(entities: Vec<Value>) -> String {
    let dir = fixture(entities);
    Project::load(dir.path().join("project.json"))
        .and_then(Runtime::new)
        .and_then(|mut runtime| {
            runtime.start()?;
            runtime.tick(H, InputSnapshot::default())
        })
        .expect_err("scene must be rejected")
        .to_string()
}
fn ticks(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.tick(H, InputSnapshot::default()).unwrap();
    }
}
fn data(runtime: &Runtime) -> &PhysicsRenderData {
    runtime.app().world().resource::<PhysicsRenderData>()
}
fn id(runtime: &Runtime, name: &str) -> Entity {
    runtime.entity(name).unwrap()
}
fn velocity(runtime: &Runtime, name: &str) -> Vec3 {
    physics::body_state(runtime.app().world(), id(runtime, name))
        .unwrap()
        .linear_velocity
}
fn events(runtime: &Runtime, name: &str) -> Vec<Value> {
    runtime
        .app()
        .world()
        .resource::<Events>()
        .0
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .collect()
}

#[test]
fn cloth_attachment_follows_the_entity_exactly() {
    // Particle 0 of the grid sits at local (-0.5, 0, 0.5); the pin entity starts there.
    let (_dir, mut runtime) = started(vec![
        settings(json!({})),
        entity("pin", [-0.5, 2., 0.5], json!({})),
        with(
            "cloth",
            [0., 2., 0.],
            "engine.Cloth",
            json!({"size":[1.,1.],"resolution":[4,4],
                "attachments":[{"particle":0,"entity":"pin"}]}),
        ),
    ]);
    ticks(&mut runtime, 3);
    let pin = id(&runtime, "pin");
    let start = data(&runtime).cloths.values().next().unwrap().positions[0];
    assert!(start.abs_diff_eq(Vec3::new(-0.5, 2., 0.5), 1e-5), "{start}");
    for target in [Vec3::new(0.5, 3., -1.), Vec3::new(-2., 1.5, 0.25)] {
        runtime
            .app_mut()
            .world_mut()
            .get_mut::<Transform>(pin)
            .unwrap()
            .position = target.to_array();
        ticks(&mut runtime, 2);
        let cloth = data(&runtime).cloths.values().next().unwrap();
        assert!(
            cloth.positions[0].abs_diff_eq(target, 1e-5),
            "pinned particle {} must equal {target}",
            cloth.positions[0]
        );
        // The rest of the cloth hangs below the attachment instead of staying behind.
        assert!(cloth.positions[4].distance(target) < 1.6);
    }
}

#[test]
fn cloth_falls_onto_a_fixed_box_and_rests_on_top() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({})),
        body(
            "box",
            [0., 0.5, 0.],
            json!({"motion":"fixed","half_extents":[1.,0.5,1.]}),
        ),
        with(
            "cloth",
            [0., 1.6, 0.],
            "engine.Cloth",
            json!({"size":[1.,1.],"resolution":[8,8]}),
        ),
    ]);
    ticks(&mut runtime, 240);
    let cloth = data(&runtime).cloths.values().next().unwrap();
    let (min, max) = cloth
        .positions
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
    assert!(min >= 1. - 0.005, "no particle below the box top: {min}");
    assert!(max < 1.05, "cloth rests on the surface: {max}");
    assert!(
        cloth
            .positions
            .iter()
            .all(|p| p.x.abs() <= 1.0 && p.z.abs() <= 1.0)
    );
}

#[test]
fn collision_mask_excludes_rigid_bodies() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({})),
        body(
            "box",
            [0., 0.5, 0.],
            json!({"motion":"fixed","half_extents":[1.,0.5,1.]}),
        ),
        with(
            "cloth",
            [0., 1.6, 0.],
            "engine.Cloth",
            json!({"size":[1.,1.],"resolution":[8,8],"collision_mask":2}),
        ),
    ]);
    ticks(&mut runtime, 120);
    let cloth = data(&runtime).cloths.values().next().unwrap();
    let min = cloth.positions.iter().fold(f32::MAX, |lo, p| lo.min(p.y));
    assert!(min < 0.5, "cloth ignored the box on layer 1: {min}");
}

fn soft_scene(two_way: bool) -> Vec<Value> {
    vec![
        settings(json!({})),
        body(
            "box",
            [0., 0., 0.],
            json!({"half_extents":[0.5,0.5,0.5],"material":{"density":1.0}}),
        ),
        with(
            "soft",
            [0., 0.9, 0.],
            "engine.SoftBody",
            json!({"size":[0.4,0.4,0.4],"resolution":[2,2,2],"density":2000.0,
                "gravity_scale":3.0,"two_way":two_way}),
        ),
    ]
}

#[test]
fn soft_body_pushes_a_light_box_down_only_when_coupled() {
    let (_a, mut coupled) = started(soft_scene(true));
    let (_b, mut free) = started(soft_scene(false));
    ticks(&mut coupled, 40);
    ticks(&mut free, 40);
    let off = velocity(&free, "box");
    let on = velocity(&coupled, "box");
    let expected = -9.81 * 40. * H;
    assert!(
        (off.y - expected).abs() < 0.05,
        "uncoupled box free-falls: {} vs {expected}",
        off.y
    );
    assert!(
        on.y < off.y - 1.,
        "coupled box is pushed down: {} vs {}",
        on.y,
        off.y
    );
}

fn fluid_scene(fill: bool, two_way: bool) -> Vec<Value> {
    let fills = if fill {
        json!([{"center":[0.,0.15,0.],"half_extents":[0.3,0.15,0.3]}])
    } else {
        json!([])
    };
    vec![
        settings(json!({})),
        with(
            "water",
            [0., 0., 0.],
            "engine.FluidVolume",
            json!({"domain_min":[-0.3,0.,-0.3],"domain_max":[0.3,0.9,0.3],"particle_radius":0.03,
                "capacity":4000,"fills":fills,"two_way":two_way}),
        ),
        body(
            "ball",
            [0., 0.5, 0.],
            json!({"shape":"ball","radius":0.08,"material":{"density":1000.0}}),
        ),
    ]
}

#[test]
fn fluid_fills_settles_and_buoys_a_dropped_ball() {
    let (_a, mut wet) = started(fluid_scene(true, true));
    let (_b, mut dry) = started(fluid_scene(false, true));
    let (_c, mut one_way) = started(fluid_scene(true, false));
    for runtime in [&mut wet, &mut dry, &mut one_way] {
        ticks(runtime, 45);
    }
    let air = velocity(&dry, "ball");
    assert!(
        (air.y - (-9.81 * 45. * H)).abs() < 0.05,
        "free fall in air: {air}"
    );
    let buoyed = velocity(&wet, "ball");
    assert!(
        buoyed.y > air.y + 0.5,
        "upward impulse from the fluid: {} vs {}",
        buoyed.y,
        air.y
    );
    let passive = velocity(&one_way, "ball");
    assert!(
        (passive.y - air.y).abs() < 1e-3,
        "one-way coupling leaves the ball alone: {} vs {}",
        passive.y,
        air.y
    );

    // The fluid itself filled 10 x 5 x 10 particles and stays inside its domain.
    let fluid = data(&wet).fluids.values().next().unwrap();
    assert_eq!(fluid.positions.len(), 500);
    assert!(
        fluid
            .positions
            .iter()
            .all(|p| p.cmpge(Vec3::new(-0.3, 0., -0.3) - 1e-3).all()
                && p.cmple(Vec3::new(0.3, 0.9, 0.3) + 1e-3).all())
    );
}

#[test]
fn fluid_settles_to_rest_in_its_domain() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({})),
        with(
            "water",
            [0., 0., 0.],
            "engine.FluidVolume",
            json!({"domain_min":[-0.3,0.,-0.3],"domain_max":[0.3,0.9,0.3],"particle_radius":0.03,
                "capacity":4000,"fills":[{"center":[0.,0.3,0.],"half_extents":[0.15,0.15,0.15]}]}),
        ),
    ]);
    ticks(&mut runtime, 240);
    let fluid = data(&runtime).fluids.values().next().unwrap();
    assert_eq!(fluid.positions.len(), 125);
    let top = fluid.positions.iter().fold(f32::MIN, |m, p| m.max(p.y));
    assert!(top < 0.35, "the column collapsed into a pool: {top}");
    let speed = fluid.velocities.iter().fold(0f32, |m, v| m.max(v.length()));
    assert!(speed < 1., "settled: max speed {speed}");
}

#[test]
fn capacity_event_fires_once_when_the_emitter_saturates() {
    let mut volume = json!({"domain_min":[-0.5,0.,-0.5],"domain_max":[0.5,1.,0.5],"particle_radius":0.03,
        "capacity":200,"emitters":[{"kind":"rate","center":[0.,0.7,0.],"half_extents":[0.2,0.1,0.2],
            "per_second":3000.0}]});
    volume["gravity_scale"] = json!(0.0);
    let mut water = with("water", [0., 0., 0.], "engine.FluidVolume", volume);
    water["components"]["EventBindings"] = json!({"bindings":[
        {"event":"physics.fluid.capacity_reached","handler":"record"}]});
    let (_dir, mut runtime) = started(vec![settings(json!({})), water]);
    ticks(&mut runtime, 90);
    let reached = events(&runtime, "physics.fluid.capacity_reached");
    assert_eq!(reached.len(), 1, "{reached:?}");
    assert_eq!(reached[0]["capacity"], 200);
    assert_eq!(
        data(&runtime)
            .fluids
            .values()
            .next()
            .unwrap()
            .positions
            .len(),
        200
    );
}

fn hanging_cloth(extra: Value) -> Vec<Value> {
    let mut cloth = json!({"size":[1.,1.],"resolution":[6,6],"fixed":"max_z",
        "drag":2.0,"lift":0.0,"substeps":10});
    for (key, v) in extra.as_object().into_iter().flatten() {
        cloth[key] = v.clone();
    }
    vec![
        settings(json!({})),
        with("cloth", [0., 2., 0.], "engine.Cloth", cloth),
    ]
}
fn mean_z(runtime: &Runtime) -> f32 {
    let cloth = data(runtime).cloths.values().next().unwrap();
    cloth.positions.iter().map(|p| p.z).sum::<f32>() / cloth.positions.len() as f32
}

#[test]
fn component_set_edits_validate_and_wind_changes_at_runtime() {
    let dir = fixture(hanging_cloth(json!({})));
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |data: Value| EditOperation::SetComponent {
        entity_id: "cloth".into(),
        component: "engine.Cloth".into(),
        value: value(data),
        merge: true,
    };
    let revision = document.revision().to_owned();
    for bad in [
        json!({"substeps":0}),
        json!({"resolution":[0,4]}),
        json!({"tether_slack":0.5}),
        json!({"wind":[1.0,2.0]}),
        json!({"unknown":1}),
    ] {
        assert!(
            document.apply(set(bad.clone()), &revision).is_err(),
            "{bad}"
        );
    }
    let revision = document
        .apply(set(json!({"wind":[0.,0.,12.]})), &revision)
        .unwrap();
    document.save(&revision).unwrap();

    let mut windy = runtime_of(&dir);
    ticks(&mut windy, 180);
    let blown = mean_z(&windy);

    let (_still_dir, mut still) = started(hanging_cloth(json!({})));
    ticks(&mut still, 180);
    assert!(
        blown > mean_z(&still) + 0.05,
        "authored wind pushes the cloth: {blown}"
    );

    // A live edit on the running component takes effect without rebuilding the cloth.
    let cloth = id(&still, "cloth");
    let before = mean_z(&still);
    still
        .app_mut()
        .world_mut()
        .get_mut::<Cloth>(cloth)
        .unwrap()
        .wind = [0., 0., 12.];
    ticks(&mut still, 180);
    assert!(
        mean_z(&still) > before + 0.3,
        "live wind edit pushes the same cloth: {before} -> {}",
        mean_z(&still)
    );
}

#[test]
fn render_data_counts_and_debug_segments() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({"debug":true})),
        with(
            "cloth",
            [0., 3., 0.],
            "engine.Cloth",
            json!({"resolution":[8,8]}),
        ),
        with(
            "soft",
            [3., 3., 0.],
            "engine.SoftBody",
            json!({"size":[1.,1.,1.],"resolution":[2,2,2]}),
        ),
        with(
            "water",
            [6., 0., 0.],
            "engine.FluidVolume",
            json!({"domain_min":[-0.3,0.,-0.3],"domain_max":[0.3,0.6,0.3],"particle_radius":0.03,
                "fills":[{"center":[0.,0.15,0.],"half_extents":[0.15,0.15,0.15]}]}),
        ),
    ]);
    ticks(&mut runtime, 2);
    let render = data(&runtime);
    let cloth = &render.cloths[&id(&runtime, "cloth")];
    assert_eq!(cloth.positions.len(), 81);
    assert_eq!(cloth.normals.len(), 81);
    assert_eq!(cloth.triangles.len(), 128);
    let soft = &render.soft_bodies[&id(&runtime, "soft")];
    assert_eq!(soft.positions.len(), 27);
    assert_eq!(soft.triangles.len(), 48);
    assert!((soft.volume - soft.rest_volume).abs() < 0.05 * soft.rest_volume);
    let fluid = &render.fluids[&id(&runtime, "water")];
    assert_eq!(fluid.positions.len(), 125);
    assert_eq!(
        (
            fluid.velocities.len(),
            fluid.densities.len(),
            fluid.surface.len()
        ),
        (125, 125, 125)
    );
    assert_eq!(render.particle_count(), 81 + 27 + 125);
    assert!(render.frame >= 2);

    let segments = physics::deformable_debug_segments(runtime.app().world(), 100_000).unwrap();
    assert!(!segments.truncated);
    assert_eq!(segments.segments.len(), (128 + 48) * 3 + 125);
    let capped = physics::deformable_debug_segments(runtime.app().world(), 10).unwrap();
    assert!(capped.truncated && capped.segments.len() == 10);
}

#[test]
fn debug_segments_require_the_debug_setting() {
    let (_dir, mut runtime) = started(hanging_cloth(json!({})));
    ticks(&mut runtime, 1);
    let error = physics::deformable_debug_segments(runtime.app().world(), 10).unwrap_err();
    assert!(error.to_string().contains("debug"), "{error}");
}

#[test]
fn invalid_authoring_is_rejected_with_named_errors() {
    let cloth = |data: Value| vec![with("c", [0.; 3], "engine.Cloth", data)];
    let message = rejection(cloth(json!({"resolution":[0,4]})));
    assert!(
        message.contains("outside bounds"),
        "schema bound: {message}"
    );
    let message = rejection(cloth(
        json!({"source":"mesh","positions":[[0,0,0],[1,0,0],[0,0,1]],"triangles":[[0,1,9]]}),
    ));
    assert!(
        message.contains("engine.Cloth") && message.contains("indices"),
        "{message}"
    );
    let message = rejection(cloth(
        json!({"attachments":[{"particle":0,"entity":"ghost"}]}),
    ));
    assert!(
        message.contains("ghost") && message.contains("Unknown entity reference"),
        "{message}"
    );
    let message = rejection(cloth(
        json!({"attachments":[{"particle":9999,"entity":"c"}]}),
    ));
    assert!(message.contains("attachment particle"), "{message}");
    let message = rejection(vec![with(
        "s",
        [0.; 3],
        "engine.SoftBody",
        json!({"source":"mesh","positions":[[0,0,0]],"tets":[[0,1,2,3]]}),
    )]);
    assert!(message.contains("engine.SoftBody"), "{message}");
    let message = rejection(vec![with(
        "w",
        [0.; 3],
        "engine.FluidVolume",
        json!({"domain_min":[0,0,0],"domain_max":[0.01,0.01,0.01]}),
    )]);
    assert!(message.contains("engine.FluidVolume"), "{message}");
}

fn mixed_scene() -> Vec<Value> {
    vec![
        settings(json!({})),
        body(
            "box",
            [0., 0.5, 0.],
            json!({"motion":"fixed","half_extents":[1.,0.5,1.]}),
        ),
        with(
            "cloth",
            [0., 1.6, 0.],
            "engine.Cloth",
            json!({"resolution":[6,6],"wind":[1.,0.,2.],"fixed":"corners"}),
        ),
        with(
            "soft",
            [0.3, 2.5, 0.],
            "engine.SoftBody",
            json!({"size":[0.3,0.3,0.3],"resolution":[2,2,2]}),
        ),
        with(
            "water",
            [4., 0., 0.],
            "engine.FluidVolume",
            json!({"domain_min":[-0.3,0.,-0.3],"domain_max":[0.3,0.9,0.3],"particle_radius":0.03,
                "fills":[{"center":[0.,0.5,0.],"half_extents":[0.15,0.15,0.15]}]}),
        ),
        body(
            "ball",
            [4., 0.8, 0.],
            json!({"shape":"ball","radius":0.07,"material":{"density":300.0}}),
        ),
    ]
}
fn bits(runtime: &Runtime) -> Vec<u32> {
    let render = data(runtime);
    let mut out = Vec::new();
    for cloth in render.cloths.values() {
        out.extend(
            cloth
                .positions
                .iter()
                .flat_map(|p| p.to_array())
                .map(f32::to_bits),
        );
    }
    for soft in render.soft_bodies.values() {
        out.extend(
            soft.positions
                .iter()
                .flat_map(|p| p.to_array())
                .map(f32::to_bits),
        );
    }
    for fluid in render.fluids.values() {
        out.extend(
            fluid
                .positions
                .iter()
                .flat_map(|p| p.to_array())
                .map(f32::to_bits),
        );
    }
    out
}

#[test]
fn fixed_step_runs_are_bitwise_deterministic() {
    let mut runs = Vec::new();
    for _ in 0..2 {
        let (_dir, mut runtime) = started(mixed_scene());
        ticks(&mut runtime, 120);
        let ball = velocity(&runtime, "ball");
        runs.push((bits(&runtime), ball.to_array().map(f32::to_bits)));
    }
    assert!(!runs[0].0.is_empty());
    assert_eq!(runs[0], runs[1], "two runs must match bit for bit");
}

/// More rigid bodies near a deformable than the solver's proxy limit used to abort every tick
/// with an error; now the nearest are coupled, the rest are counted and reported once.
#[test]
fn bodies_beyond_the_proxy_limit_degrade_instead_of_failing_the_tick() {
    const BALLS: usize = 300;
    let mut settings_entity = settings(json!({}));
    settings_entity["components"]["EventBindings"] = json!({"bindings":[
        {"event":"physics.proxies.limited","handler":"record"}]});
    let mut entities = vec![
        settings_entity,
        with(
            "water",
            [0., 0., 0.],
            "engine.FluidVolume",
            json!({"domain_min":[-2.,0.,-2.],"domain_max":[2.,2.,2.],"particle_radius":0.05,
                "capacity":500,"fills":[{"center":[0.,0.2,0.],"half_extents":[0.15,0.1,0.15]}]}),
        ),
        with(
            "cloth",
            [0., 1., 0.],
            "engine.Cloth",
            json!({"size":[1.,1.],"resolution":[3,3]}),
        ),
    ];
    for i in 0..BALLS {
        let (x, z) = ((i % 20) as f32 * 0.18 - 1.7, (i / 20) as f32 * 0.18 - 1.7);
        entities.push(body(
            &format!("ball{i}"),
            [x, 1.5, z],
            json!({"motion":"fixed","shape":"ball","radius":0.05}),
        ));
    }
    let (_dir, mut runtime) = started(entities);
    ticks(&mut runtime, 5);
    let status = physics::pressure_status(runtime.app().world()).unwrap();
    assert!(
        status.proxies_omitted >= (BALLS - engine_fluids::MAX_PROXIES) as u64 * 5,
        "{status:?}"
    );
    assert_eq!(data(&runtime).fluids.len(), 1, "the fluid kept simulating");
    let warnings = events(&runtime, "physics.proxies.limited");
    assert_eq!(
        warnings.len(),
        1,
        "one warning per episode, not one per step: {warnings:?}"
    );
    assert!(warnings[0]["total"].as_u64().unwrap() > 0);
}

/// `physics.fluid.capacity_reached` goes through the event pressure handling: with a full
/// event queue it is dropped and counted instead of aborting the tick.
#[test]
fn the_fluid_capacity_event_is_dropped_and_counted_when_the_queue_is_full() {
    let mut volume = json!({"domain_min":[-0.5,0.,-0.5],"domain_max":[0.5,1.,0.5],"particle_radius":0.03,
        "capacity":100,"emitters":[{"kind":"rate","center":[0.,0.7,0.],"half_extents":[0.2,0.1,0.2],
            "per_second":3000.0}]});
    volume["gravity_scale"] = json!(0.0);
    let water = with("water", [0., 0., 0.], "engine.FluidVolume", volume);
    let (_dir, mut runtime) = started(vec![settings(json!({})), water]);
    // A queue that holds nothing: every physics event is dropped.
    runtime.app_mut().world_mut().insert_resource(EventLimits {
        queued: 0,
        payload_bytes: 65_536,
        deliveries_per_tick: 4096,
    });
    ticks(&mut runtime, 60);
    let status = physics::pressure_status(runtime.app().world()).unwrap();
    assert!(status.events_dropped >= 1, "{status:?}");
    assert_eq!(
        data(&runtime)
            .fluids
            .values()
            .next()
            .unwrap()
            .positions
            .len(),
        100,
        "the fluid kept simulating up to its capacity"
    );
}
