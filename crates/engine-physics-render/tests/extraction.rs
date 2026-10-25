//! The extraction path end to end on a real runtime: the simulation world's physics data
//! reaches the render world through `install_physics_extraction`. No GPU needed.

use bevy_ecs::prelude::*;
use engine_physics_render::{
    DebugState, PhysicsRenderConfig, PhysicsRenderExtract, build_extract,
    install_physics_extraction,
};
use engine_project::simulation::physics::{self, PhysicsRenderData};
use engine_project::*;
use serde_json::{Value, json};
use std::fs;

const H: f32 = 1. / 60.;

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn with(id: &str, position: [f32; 3], component: &str, data: Value) -> Value {
    entity(id, position, json!({component: value(data)}))
}
fn settings(data: Value) -> Value {
    let mut base = json!({"fixed_timestep":H});
    for (key, v) in data.as_object().into_iter().flatten() {
        base[key] = v.clone();
    }
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(base)}}})
}
fn started(entities: Vec<Value>) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"physics-render","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    install_physics_extraction(runtime.app_mut(), PhysicsRenderConfig::default()).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}
fn ticks(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.tick(H, InputSnapshot::default()).unwrap();
    }
}
fn extract(runtime: &Runtime) -> &PhysicsRenderExtract {
    runtime
        .app()
        .render_world()
        .get_resource::<PhysicsRenderExtract>()
        .expect("the Extract stage stored the physics extract")
}

#[test]
fn deformables_and_debug_segments_reach_the_render_world() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({"debug":true})),
        with(
            "cloth",
            [0., 2., 0.],
            "engine.Cloth",
            json!({"size":[1.,1.],"resolution":[4,4]}),
        ),
        with(
            "soft",
            [3., 2., 0.],
            "engine.SoftBody",
            json!({"size":[0.4,0.4,0.4],"resolution":[2,2,2]}),
        ),
        with(
            "water",
            [6., 0., 0.],
            "engine.FluidVolume",
            json!({"domain_min":[-0.3,0.,-0.3],"domain_max":[0.3,0.9,0.3],"particle_radius":0.05,
                "capacity":500,"fills":[{"center":[0.,0.2,0.],"half_extents":[0.15,0.1,0.15]}]}),
        ),
    ]);
    ticks(&mut runtime, 3);
    let source = runtime.app().world().resource::<PhysicsRenderData>();
    let copied = extract(&runtime);
    assert_eq!(copied.data.cloths.len(), 1);
    assert_eq!(copied.data.soft_bodies.len(), 1);
    assert_eq!(copied.data.fluids.len(), 1);
    assert_eq!(
        copied.data, *source,
        "the extract mirrors the simulation data"
    );
    let cloth = copied.data.cloths.values().next().unwrap();
    assert_eq!(cloth.positions.len(), 25);
    assert_eq!(cloth.normals.len(), 25);
    assert_eq!(cloth.triangles.len(), 32);
    assert_eq!(copied.debug_state, DebugState::On);
    // 32 cloth triangles x 3 edges, plus soft-body edges and one tick per particle.
    let particles = copied.data.fluids.values().next().unwrap().positions.len();
    assert!(particles > 0);
    assert!(
        copied.debug.len() >= 32 * 3 + particles,
        "{} debug segments for {particles} particles",
        copied.debug.len()
    );
    assert!(copied.revision > 0);
}

#[test]
fn quiet_frames_keep_the_revision_and_stepped_frames_change_it() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({})),
        with(
            "cloth",
            [0., 2., 0.],
            "engine.Cloth",
            json!({"size":[1.,1.],"resolution":[3,3]}),
        ),
    ]);
    ticks(&mut runtime, 2);
    let first = extract(&runtime).revision;
    assert_eq!(extract(&runtime).debug_state, DebugState::Off);
    assert!(
        extract(&runtime)
            .debug_reason
            .as_deref()
            .is_some_and(|r| r.contains("debug")),
        "the reason names the setting: {:?}",
        extract(&runtime).debug_reason
    );
    ticks(&mut runtime, 2);
    assert!(
        extract(&runtime).revision > first,
        "the cloth moved: new data"
    );
}

#[test]
fn fragments_and_the_fractured_owner_are_extracted_with_world_transforms() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({"gravity_2d":[0.,0.],"gravity_3d":[0.,0.,0.]})),
        with(
            "box",
            [4., 0., 0.],
            "engine.PhysicsBody3D",
            json!({"shape":"box"}),
        ),
    ]);
    let owner = runtime.entity("box").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(owner)
        .insert(physics::Destructible3D::default());
    ticks(&mut runtime, 2);
    assert!(extract(&runtime).fragments.is_empty());
    let report = physics::fracture_now(
        runtime.app_mut().world_mut(),
        owner,
        glam::Vec3::new(4., 0., 0.),
        1.,
    )
    .unwrap();
    ticks(&mut runtime, 2);
    let copied = extract(&runtime);
    assert_eq!(copied.fragments.len(), report.fragments.len());
    assert_eq!(copied.fractured, vec![owner]);
    for draw in &copied.fragments {
        assert_eq!(draw.owner, owner);
        let origin = draw.world_from_local.w_axis.truncate();
        assert!(
            (origin - glam::Vec3::new(4., 0., 0.)).length() < 1.0,
            "fragment origin near the box: {origin}"
        );
        assert_eq!(draw.mesh.interior.len(), draw.mesh.triangles.len());
    }
}

#[test]
fn disabled_features_are_not_extracted_and_a_world_without_physics_is_reported() {
    let mut world = World::new();
    world.insert_resource(PhysicsRenderData::default());
    let mut config = PhysicsRenderConfig::default();
    let off = build_extract(&mut world, &config, None);
    assert_eq!(off.debug_state, DebugState::Off);
    assert!(
        off.debug_reason.is_some(),
        "no physics service: stated, not silent"
    );
    config.enabled.debug_lines = false;
    let disabled = build_extract(&mut world, &config, None);
    assert_eq!(disabled.debug_state, DebugState::Disabled);
    assert!(disabled.fragments.is_empty());
}
