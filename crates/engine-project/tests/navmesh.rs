use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::{simulation::*, *};
use serde_json::json;
use std::fs;
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let document = NavMeshDocument {
        version: 1,
        input: BakeInput {
            vertices: vec![[0., 0., 0.], [10., 0., 0.], [10., 0., 10.], [0., 0., 10.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            obstacles: vec![Obstacle {
                min: [4., -1., 3.],
                max: [6., 3., 7.],
            }],
            settings: BakeSettings::default(),
        },
    };
    fs::write(
        dir.path().join("navigation.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    fs::write(dir.path().join("project.json"),json!({"schema":1,"name":"nav-test","startup_scene":"scene.json","assets":[{"id":"nav","kind":"navigation","path":"navigation.json"}]}).to_string()).unwrap();
    fs::write(dir.path().join("scene.json"),json!({"schema":1,"scene_id":"main","entities":[{"id":"agent","name":"agent","components":{"Transform":{"position":[2,0,5]},"EventBindings":{"bindings":[{"event":"navigation.repathed","handler":"record"},{"event":"navigation.arrived","handler":"record"},{"event":"navigation.failed","handler":"record"}]},"custom":{"engine.NavigationAgent":{"version":1,"data":{"navmesh":"nav","target":[8,0,5],"speed":4}}}}}]}).to_string()).unwrap();
    dir
}
#[derive(Resource, Default)]
struct Calls(Vec<String>);
fn callback(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world.resource_mut::<Calls>().0.push(event.name.clone());
    Ok(())
}
#[test]
fn runtime_uses_baked_paths_and_native_repath_arrival_failure_callbacks() {
    let dir = fixture();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let mesh = load_navmesh(&project, "nav").unwrap();
    assert!(matches!(
        mesh.find_path([2., 0., 5.], [8., 0., 5.], QuerySettings::default())
            .unwrap(),
        NavPath::Reached { .. }
    ));
    let mut runtime = Runtime::new(project).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Calls::default());
    register_event_handler(runtime.app_mut(), "record", callback).unwrap();
    runtime.start().unwrap();
    let entity = runtime.entity("agent").unwrap();
    for _ in 0..40 {
        runtime.tick(0.1, InputSnapshot::default()).unwrap();
        let p = world_transform(runtime.app().world(), entity)
            .unwrap()
            .position();
        assert!(
            p[0] <= 3.75 + 1e-5
                || p[0] >= 6.25 - 1e-5
                || p[2] <= 2.75 + 1e-5
                || p[2] >= 7.25 - 1e-5,
            "{p:?}"
        );
    }
    assert_eq!(
        runtime.app().world().resource::<Calls>().0,
        vec!["navigation.repathed", "navigation.arrived"]
    );
    {
        let mut config = runtime
            .app_mut()
            .world_mut()
            .get_mut::<NavigationAgent>(entity)
            .unwrap();
        config.target = Some([2., 0., 5.]);
        config.repath += 1;
    }
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
    assert_eq!(
        runtime.app().world().resource::<Calls>().0.last().unwrap(),
        "navigation.repathed"
    );
    {
        let mut config = runtime
            .app_mut()
            .world_mut()
            .get_mut::<NavigationAgent>(entity)
            .unwrap();
        config.target = Some([100., 0., 100.]);
        config.repath += 1;
    }
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
    assert_eq!(
        runtime.app().world().resource::<Calls>().0.last().unwrap(),
        "navigation.failed"
    );
}
#[test]
fn navigation_assets_reject_escape_malformed_and_invalid_versions() {
    let dir = fixture();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    assert!(load_navmesh_file(&project.root, std::path::Path::new("../navigation.json")).is_err());
    fs::write(
        dir.path().join("navigation.json"),
        "{\"version\":99,\"input\":{}}",
    )
    .unwrap();
    assert!(Project::load(dir.path().join("project.json")).is_err());
    assert!(load_navmesh(&project, "nav").is_err());
}
