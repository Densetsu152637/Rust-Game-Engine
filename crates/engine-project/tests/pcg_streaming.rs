//! PCG output owned by a streamed cell's volume must not outlive the cell.
use engine_pcg::{GraphBuilder, Registry};
use engine_project::world::streaming::StreamedCell;
use engine_project::{AuthoredId, InputSnapshot, Project, Runtime, Transform};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;

const MESH: &str = "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n";

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cells")).unwrap();
    fs::write(dir.path().join("rock.cgmesh"), MESH).unwrap();
    let graph = GraphBuilder::new(&Registry::builtin(), 3)
        .node("grid", "grid_sampler", [("spacing", json!(4.0))])
        .node(
            "spawn",
            "spawner",
            [("entries", json!([{"asset":"rock","kind":"mesh"}]))],
        )
        .connect("grid.points", "spawn.points")
        .build()
        .unwrap()
        .to_json();
    fs::write(dir.path().join("scatter.pcg.json"), graph).unwrap();
    let cell = json!({"schema":1,"scene_id":"cell0","entities":[
        {"id":"vol","name":"vol","components":{"Transform":{"position":[50,0,50]},
            "custom":{"engine.PcgVolume":{"version":1,"data":{"graph":"scatter","seed":1,"size":[16,64,16]}}}}}
    ]})
    .to_string();
    fs::write(dir.path().join("cells/0.json"), &cell).unwrap();
    fs::write(
        dir.path().join("world.partition.json"),
        json!({"schema":1,"cell_size":100.0,"cells":[
            {"key":[0,0,0],"layer":"","file":"cells/0.json","entities":1,"bytes":cell.len()}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"cam","name":"cam","components":{"Transform":{"position":[0,0,0]},"Camera3D":{}}},
            {"id":"scout","name":"scout","components":{"Transform":{"position":[50,0,50]},
                "custom":{"engine.StreamingSource":{"version":1,"data":{}}}}},
            {"id":"streaming","name":"streaming","components":{"Transform":{"position":[0,0,0]},
                "custom":{"engine.WorldPartition":{"version":1,"data":{"manifest":"world",
                    "load_radius":150.0,"unload_radius":250.0,"camera_source":false,
                    "synchronous":true,"hlod":false}}}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"pcg-stream","startup_scene":"scene.json","assets":[
            {"id":"rock","kind":"mesh","path":"rock.cgmesh"},
            {"id":"scatter","kind":"pcg","path":"scatter.pcg.json"},
            {"id":"world","kind":"partition","path":"world.partition.json"}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

fn tick(runtime: &mut Runtime) {
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
}
fn generated(runtime: &mut Runtime) -> usize {
    let world = runtime.app_mut().world_mut();
    let mut query = world.query::<&AuthoredId>();
    query
        .iter(world)
        .filter(|a| a.id.starts_with("pcg:vol:"))
        .count()
}
/// Entity count and the number of duplicated authored ids.
fn total(runtime: &mut Runtime) -> (usize, usize) {
    let world = runtime.app_mut().world_mut();
    let mut query = world.query::<&AuthoredId>();
    let ids: Vec<String> = query.iter(world).map(|a| a.id.clone()).collect();
    let unique: BTreeSet<&String> = ids.iter().collect();
    (world.iter_entities().count(), ids.len() - unique.len())
}
fn move_scout(runtime: &mut Runtime, x: f32) {
    let scout = runtime.entity("scout").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(scout)
        .unwrap()
        .position = [x, 0.0, 50.0];
}

#[test]
fn unloading_and_reloading_a_cell_with_a_pcg_volume_leaves_no_stale_output() {
    let dir = fixture();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    for _ in 0..3 {
        tick(&mut runtime);
    }
    let first = generated(&mut runtime);
    assert!(first > 4, "the volume generated {first} entities");
    let (entities, duplicates) = total(&mut runtime);
    assert_eq!(duplicates, 0);
    for round in 0..3 {
        move_scout(&mut runtime, 5000.0);
        for _ in 0..4 {
            tick(&mut runtime);
        }
        assert_eq!(
            generated(&mut runtime),
            0,
            "round {round}: output left behind"
        );
        move_scout(&mut runtime, 50.0);
        for _ in 0..4 {
            tick(&mut runtime);
        }
        assert_eq!(generated(&mut runtime), first, "round {round}");
        assert_eq!(total(&mut runtime), (entities, 0), "round {round}");
    }
    let marked = runtime
        .app()
        .world()
        .iter_entities()
        .filter(|e| e.get::<StreamedCell>().is_some())
        .count();
    assert_eq!(marked, 1);
}
