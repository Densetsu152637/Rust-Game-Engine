use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::{simulation::*, *};
use serde_json::{Value, json};
use std::fs;
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn fixture(entities: Vec<Value>, clip: Option<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut assets = vec![];
    if let Some(clip) = clip {
        fs::write(
            dir.path().join("clip.json"),
            serde_json::to_vec(&clip).unwrap(),
        )
        .unwrap();
        assets.push(json!({"id":"clip","kind":"animation","path":"clip.json"}));
    }
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(dir.path().join("project.json"),serde_json::to_vec(&json!({"schema":1,"name":"simulation-test","startup_scene":"scene.json","assets":assets})).unwrap()).unwrap();
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap()
}
fn tick(runtime: &mut Runtime, dt: f32) {
    runtime.tick(dt, InputSnapshot::default()).unwrap();
}
#[derive(Resource, Default)]
struct Calls(Vec<String>);
fn callback(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world.resource_mut::<Calls>().0.push(event.name.clone());
    Ok(())
}
#[test]
fn builtins_discoverable_and_authored_2d_3d_bodies_fall_with_parent_conversion() {
    let mut parent = entity("parent", [10., 5., 0.], json!({}));
    parent["components"]["Transform"]["rotation"] =
        json!(glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array());
    let mut child = entity(
        "two",
        [0., 3., 0.],
        json!({"engine.PhysicsBody2D":value(json!({}))}),
    );
    child["parent"] = json!("parent");
    let dir = fixture(
        vec![
            parent,
            child,
            entity(
                "three",
                [0., 10., 0.],
                json!({"engine.PhysicsBody3D":value(json!({}))}),
            ),
        ],
        None,
    );
    let mut runtime = runtime(&dir);
    assert!(
        runtime
            .project()
            .component_catalog
            .0
            .contains_key("engine.AnimationPlayer")
    );
    runtime.start().unwrap();
    let two = runtime.entity("two").unwrap();
    let three = runtime.entity("three").unwrap();
    for _ in 0..20 {
        tick(&mut runtime, 0.02);
    }
    let world = runtime.app().world();
    let p = world_transform(world, two).unwrap().position();
    assert!((p[0] - 7.).abs() < 0.001);
    assert!(p[1] < 4.5);
    assert!(world.get::<Transform>(two).unwrap().position[0] < -0.5);
    assert!(world.get::<Transform>(three).unwrap().position[1] < 9.5);
}
#[test]
fn invalid_native_pose_returns_before_extraction_instead_of_panicking() {
    let dir = fixture(vec![entity("entity", [0.; 3], json!({}))], None);
    let mut runtime = runtime(&dir);
    runtime
        .app_mut()
        .schedule_mut(engine_core::Stage::Update)
        .add_systems(|mut transforms: Query<&mut Transform>| {
            for mut t in &mut transforms {
                t.scale = [0.; 3];
            }
        });
    runtime
        .app_mut()
        .schedule_mut(engine_core::Stage::Extract)
        .add_systems(|| panic!("invalid pose must stop before extraction"));
    assert!(runtime.tick(0.02, InputSnapshot::default()).is_err());
}
#[test]
fn animation_and_navigation_update_and_deliver_callbacks_next_tick() {
    let mut animated = entity(
        "animated",
        [0.; 3],
        json!({"engine.AnimationPlayer":value(json!({"clip":"clip","looping":false}))}),
    );
    animated["components"]["EventBindings"] =
        json!({"bindings":[{"event":"animation.event","handler":"record"}]});
    let mut nav = entity(
        "nav",
        [0.; 3],
        json!({"engine.NavigationAgent":value(json!({"waypoints":[[1.,0.,0.]],"speed":2.}))}),
    );
    nav["components"]["EventBindings"] =
        json!({"bindings":[{"event":"navigation.arrived","handler":"record"}]});
    let dir = fixture(
        vec![animated, nav],
        Some(
            json!({"version":1,"duration":1.,"joints":1,"tracks":[{"joint":0,"translation":{"times":[0.,1.],"values":[[0.,0.,0.],[4.,0.,0.]],"interpolation":"linear"}}],"events":[{"time":0.25,"name":"footstep"}]}),
        ),
    );
    let mut runtime = runtime(&dir);
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Calls::default());
    register_event_handler(runtime.app_mut(), "record", callback).unwrap();
    tick(&mut runtime, 0.25);
    let animated = runtime.entity("animated").unwrap();
    let nav = runtime.entity("nav").unwrap();
    assert!(
        (runtime
            .app()
            .world()
            .get::<Transform>(animated)
            .unwrap()
            .position[0]
            - 1.)
            .abs()
            < 0.001
    );
    assert!(
        (runtime
            .app()
            .world()
            .get::<Transform>(nav)
            .unwrap()
            .position[0]
            - 0.5)
            .abs()
            < 0.001
    );
    assert!(runtime.app().world().resource::<Calls>().0.is_empty());
    tick(&mut runtime, 0.25);
    tick(&mut runtime, 0.1);
    assert_eq!(
        runtime.app().world().resource::<Calls>().0,
        vec!["animation.event", "navigation.arrived"]
    );
}
#[test]
fn native_update_inputs_reach_kinematic_sensor_before_extraction() {
    let mut sensor = entity(
        "sensor",
        [0.; 3],
        json!({"engine.PhysicsBody2D":value(json!({"motion":"fixed","shape":"ball","radius":1.,"material":{"sensor":true}}))}),
    );
    sensor["components"]["EventBindings"] =
        json!({"bindings":[{"event":"physics.contact.start","handler":"record"}]});
    let dir = fixture(
        vec![
            sensor,
            entity(
                "mover",
                [3., 0., 0.],
                json!({"engine.PhysicsBody2D":value(json!({"motion":"kinematic"}))}),
            ),
        ],
        None,
    );
    let mut runtime = runtime(&dir);
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Calls::default());
    register_event_handler(runtime.app_mut(), "record", callback).unwrap();
    runtime
        .app_mut()
        .schedule_mut(engine_core::Stage::Update)
        .add_systems(|mut transforms: Query<(&AuthoredId, &mut Transform)>| {
            for (id, mut t) in &mut transforms {
                if id.id == "mover" {
                    t.position = [0.; 3];
                }
            }
        });
    tick(&mut runtime, 0.02);
    tick(&mut runtime, 0.02);
    assert!(
        runtime
            .app()
            .world()
            .resource::<Calls>()
            .0
            .contains(&"physics.contact.start".into())
    );
}
#[test]
fn disabled_services_and_conflicting_pose_writers_fail_explicitly() {
    let dir = fixture(
        vec![entity(
            "body",
            [0.; 3],
            json!({"engine.PhysicsBody2D":value(json!({})),"engine.NavigationAgent":value(json!({}))}),
        )],
        None,
    );
    let project = Project::load(dir.path().join("project.json")).unwrap();
    assert!(Runtime::new(project.clone()).unwrap().start().is_err());
    assert!(
        Runtime::new_with_simulation(project, None)
            .unwrap()
            .start()
            .is_err()
    );
}
#[test]
fn prefab_binding_remap_preserves_node_order() {
    let mut value = json!({"bindings":["node:0","node:1"]});
    remap_animation_bindings(
        &mut value,
        &[
            ("node:0".into(), "a:node:0".into()),
            ("node:1".into(), "a:node:1".into()),
        ]
        .into(),
    );
    assert_eq!(value["bindings"], json!(["a:node:0", "a:node:1"]));
}
