use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::{simulation::*, *};
use serde_json::{Value, json};
use std::fs;

fn write_project(floors: &[([f32; 3], [f32; 3])], entities: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut vertices = vec![];
    let mut triangles = vec![];
    for (min, max) in floors {
        let n = vertices.len() as u32;
        vertices.extend([
            min.to_owned(),
            [max[0], max[1], min[2]],
            *max,
            [min[0], min[1], max[2]],
        ]);
        triangles.extend([[n, n + 1, n + 2], [n, n + 2, n + 3]]);
    }
    let document = NavMeshDocument {
        version: 1,
        input: BakeInput {
            vertices,
            triangles,
            obstacles: vec![],
            settings: BakeSettings {
                cell_size: 0.5,
                agent_radius: 0.3,
                ..Default::default()
            },
        },
    };
    fs::write(
        dir.path().join("navigation.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    fs::write(dir.path().join("project.json"),json!({"schema":1,"name":"crowd-test","startup_scene":"scene.json","assets":[{"id":"nav","kind":"navigation","path":"navigation.json"}]}).to_string()).unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    dir
}
const EVENTS: [&str; 5] = [
    "navigation.repathed",
    "navigation.arrived",
    "navigation.failed",
    "navigation.link_entered",
    "navigation.link_exited",
];
fn bindings() -> Value {
    json!({"bindings":EVENTS.iter().map(|e| json!({"event":e,"handler":"record"})).collect::<Vec<_>>()})
}
fn agent(id: &str, from: [f32; 3], data: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":from},"EventBindings":bindings(),
        "custom":{"engine.CrowdAgent":{"version":1,"data":data}}}})
}
fn plain(id: &str, position: [f32; 3], component: &str, data: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},
        "custom":{component:{"version":1,"data":data}}}})
}
#[derive(Resource, Default)]
struct Calls(Vec<(String, Value)>);
fn record(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Calls>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}
fn start(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Calls::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    runtime
}
fn tick(runtime: &mut Runtime) {
    runtime.tick(0.05, InputSnapshot::default()).unwrap();
}
fn pos(runtime: &Runtime, id: &str) -> [f32; 3] {
    world_transform(runtime.app().world(), runtime.entity(id).unwrap())
        .unwrap()
        .position()
}
fn names(runtime: &Runtime) -> Vec<String> {
    runtime
        .app()
        .world()
        .resource::<Calls>()
        .0
        .iter()
        .map(|c| c.0.clone())
        .collect()
}

#[test]
fn authored_agents_swap_without_overlap_and_arrive() {
    let dir = write_project(
        &[([0., 0., 0.], [30., 0., 30.])],
        vec![
            agent(
                "a",
                [5., 0., 15.],
                json!({"navmesh":"nav","target":[25,0,15]}),
            ),
            agent(
                "b",
                [25., 0., 15.],
                json!({"navmesh":"nav","target":[5,0,15]}),
            ),
        ],
    );
    let mut runtime = start(&dir);
    let mut min_gap = f32::MAX;
    for _ in 0..400 {
        tick(&mut runtime);
        let (a, b) = (pos(&runtime, "a"), pos(&runtime, "b"));
        let d = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        min_gap = min_gap.min(d - 0.8);
    }
    println!("runtime head-on min gap {min_gap:.5}");
    assert!(min_gap >= -1e-3);
    let events = names(&runtime);
    assert_eq!(
        events.iter().filter(|n| *n == "navigation.arrived").count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|n| *n == "navigation.repathed")
            .count(),
        2
    );
    assert!((pos(&runtime, "a")[0] - 25.).abs() < 0.2);
}

#[test]
fn enabling_an_obstacle_carves_and_repaths_agents() {
    let dir = write_project(
        &[([0., 0., 0.], [20., 0., 20.])],
        vec![
            agent(
                "walker",
                [2., 0., 10.],
                json!({"navmesh":"nav","target":[18,0,10]}),
            ),
            plain(
                "block",
                [10., 1., 10.],
                "engine.NavigationObstacle",
                json!({"navmesh":"nav","half_extents":[2,1,4],"enabled":false}),
            ),
        ],
    );
    let mut runtime = start(&dir);
    for _ in 0..20 {
        tick(&mut runtime);
    }
    let block = runtime.entity("block").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<NavigationObstacle>(block)
        .unwrap()
        .enabled = true;
    let mut worst = f32::MIN;
    for _ in 0..600 {
        tick(&mut runtime);
        let p = pos(&runtime, "walker");
        worst = worst.max((p[0] - 8.).min(12. - p[0]).min((p[2] - 6.).min(14. - p[2])));
    }
    assert!(worst <= 0., "walker entered the obstacle: {worst}");
    let events = names(&runtime);
    assert_eq!(events.first().unwrap(), "navigation.repathed");
    assert_eq!(
        events
            .iter()
            .filter(|n| *n == "navigation.repathed")
            .count(),
        2
    );
    assert_eq!(events.last().unwrap(), "navigation.arrived");
    assert!((pos(&runtime, "walker")[0] - 18.).abs() < 0.2);
}

#[test]
fn off_mesh_link_entity_connects_islands_and_reports_traversal() {
    let dir = write_project(
        &[([0., 0., 0.], [5., 0., 5.]), ([10., 0., 0.], [15., 0., 5.])],
        vec![
            agent(
                "walker",
                [1.25, 0., 2.25],
                json!({"navmesh":"nav","target":[14.75,0,2.25]}),
            ),
            plain(
                "jump",
                [4.75, 0., 2.25],
                "engine.OffMeshLink",
                json!({"navmesh":"nav","end":[10.25,0,2.25],"cost":3}),
            ),
        ],
    );
    let mut runtime = start(&dir);
    for _ in 0..500 {
        tick(&mut runtime);
    }
    assert_eq!(
        names(&runtime),
        [
            "navigation.repathed",
            "navigation.link_entered",
            "navigation.link_exited",
            "navigation.arrived"
        ]
    );
    let calls = &runtime.app().world().resource::<Calls>().0;
    assert_eq!(calls[1].1["link"], "jump");
    let end = pos(&runtime, "walker");
    assert!((end[0] - 14.75).abs() < 0.5, "{end:?}");
}

#[test]
fn formation_members_reach_slots_and_invalid_combinations_fail() {
    let mut entities = vec![
        json!({"id":"form","name":"form","components":{"Transform":{"position":[15,0,15]},
        "custom":{"engine.Formation":{"version":1,"data":{"navmesh":"nav","slots":[[-1,0],[1,0],[0,-2]]}}}}}),
    ];
    for (i, x) in [3., 4., 5.].iter().enumerate() {
        entities.push(agent(
            &format!("m{i}"),
            [*x, 0., 3.],
            json!({"navmesh":"nav","formation":"form","arrival_radius":0.05}),
        ));
    }
    let dir = write_project(&[([0., 0., 0.], [30., 0., 30.])], entities);
    let mut runtime = start(&dir);
    for _ in 0..600 {
        tick(&mut runtime);
    }
    let mut spots: Vec<[f32; 2]> = (0..3)
        .map(|i| {
            let p = pos(&runtime, &format!("m{i}"));
            [(p[0] * 10.).round() / 10., (p[2] * 10.).round() / 10.]
        })
        .collect();
    spots.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(spots, vec![[14., 15.], [15., 13.], [16., 15.]]);

    let bad = write_project(
        &[([0., 0., 0.], [30., 0., 30.])],
        vec![
            json!({"id":"x","name":"x","components":{"Transform":{"position":[0,0,0]},"custom":{
            "engine.CrowdAgent":{"version":1,"data":{"navmesh":"nav"}},
            "engine.NavigationAgent":{"version":1,"data":{}}}}}),
        ],
    );
    let mut runtime =
        Runtime::new(Project::load(bad.path().join("project.json")).unwrap()).unwrap();
    assert!(
        runtime
            .start()
            .and_then(|_| runtime.tick(0.05, InputSnapshot::default()).map(|_| ()))
            .is_err()
    );
}
