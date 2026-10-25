//! Authored `engine.WorldPartition` scene -> Runtime -> streamed cells, layers, HLOD, events.
use engine_project::world::coordinates::WorldOrigin;
use engine_project::world::streaming::{
    self, CellState, EVENT_CELL_LOADED, EVENT_CELL_UNLOADED, PartitionStatus, StreamedCell,
    WorldPartition,
};
use engine_project::{InputSnapshot, Project, Runtime, Transform, world_transform};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const MESH: &str = "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n";
const PROXY: &str = r#"{"vertices":[{"position":[0,0,0],"uv":[0,0],"color":[1,1,1,1]},{"position":[100,0,0],"uv":[1,0],"color":[1,1,1,1]},{"position":[0,0,100],"uv":[0,1],"color":[1,1,1,1]}],"indices":[0,1,2]}"#;

struct Options {
    /// `(x, layer)` cells, one `x` per cell along +x (z = 0).
    cells: Vec<(i32, &'static str)>,
    partition: Value,
    scout: [f32; 3],
    hlod: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            cells: (0..10).map(|x| (x, "")).collect(),
            partition: json!({}),
            scout: [50.0, 0.0, 50.0],
            hlod: false,
        }
    }
}

fn entity_id(x: i32, layer: &str, name: &str) -> String {
    format!("c{x}{layer}_{name}")
}

fn write_cell(dir: &Path, x: i32, layer: &str) -> (String, u64) {
    let file = format!("cells/{x}{layer}.json");
    let (a, b) = (entity_id(x, layer, "a"), entity_id(x, layer, "b"));
    let text = json!({"schema":1,"scene_id":format!("cell{x}{layer}"),"entities":[
        {"id":a,"name":"a","components":{"Transform":{"position":[10.25,0,10.25]},"MeshRenderer":{"mesh":"rock"}}},
        {"id":b,"name":"b","parent":a,"components":{"Transform":{"position":[0,1,0]},"MeshRenderer":{"mesh":"rock"}}}
    ]})
    .to_string();
    fs::write(dir.join(&file), &text).unwrap();
    (file, text.len() as u64)
}

fn fixture(options: &Options) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cells")).unwrap();
    fs::write(dir.path().join("rock.cgmesh"), MESH).unwrap();
    let mut cells = Vec::new();
    for (x, layer) in &options.cells {
        let (file, bytes) = write_cell(dir.path(), *x, layer);
        let mut cell = json!({"key":[x,0,0],"layer":layer,"file":file,"entities":2,"bytes":bytes});
        if options.hlod {
            let proxy = format!("cells/{x}{layer}.hlod.json");
            fs::write(dir.path().join(&proxy), PROXY).unwrap();
            cell["hlod"] = json!({"file":proxy,"triangles":1,"source_triangles":2});
        }
        cells.push(cell);
    }
    fs::write(
        dir.path().join("world.partition.json"),
        json!({"schema":1,"cell_size":100.0,"cells":cells}).to_string(),
    )
    .unwrap();
    let mut partition = json!({"manifest":"world","load_radius":150.0,"unload_radius":250.0,
        "loads_per_tick":2,"max_in_flight":64,"camera_source":false,"synchronous":true,"hlod":options.hlod});
    for (k, v) in options.partition.as_object().unwrap() {
        partition[k] = v.clone();
    }
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"cam","name":"cam","components":{"Transform":{"position":[0,0,0]},"Camera3D":{}}},
            {"id":"scout","name":"scout","components":{
                "Transform":{"position":options.scout},
                "custom":{"engine.StreamingSource":{"version":1,"data":{}}}}},
            {"id":"streaming","name":"streaming","components":{
                "Transform":{"position":[0,0,0]},
                "custom":{"engine.WorldPartition":{"version":1,"data":partition}}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"streaming-test","startup_scene":"scene.json","assets":[
            {"id":"rock","kind":"mesh","path":"rock.cgmesh"},
            {"id":"world","kind":"partition","path":"world.partition.json"}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

fn start(dir: &tempfile::TempDir, origin: [f64; 3]) -> Runtime {
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(WorldOrigin { offset: origin });
    runtime.start().unwrap();
    runtime
}

fn tick(runtime: &mut Runtime) {
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
}

fn status(runtime: &Runtime) -> PartitionStatus {
    streaming::partition_status(runtime.app().world(), "streaming").expect("partition status")
}

fn move_scout(runtime: &mut Runtime, position: [f32; 3]) {
    let scout = runtime.entity("scout").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(scout)
        .unwrap()
        .position = position;
}

/// Cell numbers (`x`) of the default-layer cells in `state`.
fn cells_in(runtime: &Runtime, state: CellState) -> Vec<i32> {
    let mut found: Vec<i32> = streaming::cell_infos(runtime.app().world(), "streaming")
        .unwrap_or_default()
        .iter()
        .filter(|c| c.layer.is_empty() && c.state == state)
        .map(|c| c.key[0])
        .collect();
    found.sort_unstable();
    found
}

fn loaded(runtime: &Runtime) -> Vec<i32> {
    cells_in(runtime, CellState::Loaded)
}

fn has(runtime: &Runtime, x: i32, layer: &str, name: &str) -> bool {
    runtime.entity(&entity_id(x, layer, name)).is_some()
}

fn cell_entity_count(runtime: &Runtime) -> usize {
    runtime
        .app()
        .world()
        .iter_entities()
        .filter(|e| e.get::<StreamedCell>().is_some_and(|c| !c.proxy))
        .count()
}

#[test]
fn cells_load_and_unload_with_a_moving_source_within_the_tick_budget() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    assert_eq!(
        cell_entity_count(&runtime),
        0,
        "nothing before the first tick"
    );

    // Source at x = 50: cells 0 (d 0), 1 (d 50), 2 (d 150) are in the 150 m load radius, two per tick.
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [0, 1]);
    let first = status(&runtime);
    assert_eq!((first.started_last_tick, first.applied_last_tick), (2, 2));
    assert_eq!(first.loaded_entities, 4);
    assert_eq!(cell_entity_count(&runtime), 4);
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [0, 1, 2]);
    tick(&mut runtime);
    let idle = status(&runtime);
    assert_eq!((idle.started_last_tick, idle.applied_last_tick), (0, 0));
    assert_eq!(cell_entity_count(&runtime), 6);

    // x = 300: cell 0 is 200 m away, inside the hysteresis band, and stays; 3 and 4 load.
    move_scout(&mut runtime, [300.0, 0.0, 50.0]);
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [0, 1, 2, 3, 4]);
    assert!(has(&runtime, 0, "", "a"));

    // x = 400: cell 0 is 300 m away and unloads; its two entities are gone, the rest remain.
    move_scout(&mut runtime, [400.0, 0.0, 50.0]);
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [1, 2, 3, 4, 5]);
    assert!(!has(&runtime, 0, "", "a") && !has(&runtime, 0, "", "b"));
    assert!(has(&runtime, 1, "", "a"));
    assert_eq!(cell_entity_count(&runtime), 10);
    assert_eq!(cells_in(&runtime, CellState::Unloaded), [0, 6, 7, 8, 9]);
    let s = status(&runtime);
    assert_eq!((s.total_loaded, s.total_unloaded), (6, 1));
    assert_eq!(s.loaded_cells.len(), 5);
    assert_eq!(s.diagnostics, Vec::<String>::new());

    // The per-tick budget held on every tick above (at most 2 applied).
}

#[test]
fn applying_never_exceeds_the_budget_while_walking() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    let mut max_applied = 0;
    let mut max_loaded = 0;
    for step in 0..60 {
        move_scout(&mut runtime, [50.0 + step as f32 * 15.0, 0.0, 50.0]);
        tick(&mut runtime);
        let s = status(&runtime);
        max_applied = max_applied.max(s.applied_last_tick);
        max_loaded = max_loaded.max(s.loaded);
        assert!(
            s.applied_last_tick <= 2 && s.started_last_tick <= 2,
            "{s:?}"
        );
        assert_eq!(cell_entity_count(&runtime), s.loaded_entities);
    }
    assert_eq!(max_applied, 2);
    assert!(max_loaded >= 3);
}

#[test]
fn memory_budget_caps_loaded_cells_and_evicts_the_farthest() {
    let options = Options {
        partition: json!({"max_loaded_cells":2,"loads_per_tick":8}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    assert_eq!(
        loaded(&runtime),
        [0, 1],
        "cell 2 does not fit in a budget of two"
    );
    // x = 250: ranks d(1)=50, d(2)=0, d(3)=50; the farthest loaded cell (0) makes room for 2.
    move_scout(&mut runtime, [250.0, 0.0, 50.0]);
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [1, 2]);
    assert!(!has(&runtime, 0, "", "a"));
    for step in 0..40 {
        move_scout(&mut runtime, [250.0 + step as f32 * 20.0, 0.0, 50.0]);
        tick(&mut runtime);
        let s = status(&runtime);
        assert!(s.loaded + s.loading + s.unloading <= 2, "{s:?}");
    }
}

#[test]
fn data_layers_unload_and_reload_their_cells() {
    let mut cells: Vec<(i32, &'static str)> = (0..4).map(|x| (x, "")).collect();
    cells.extend((0..3).map(|x| (x, "props")));
    let options = Options {
        cells,
        partition: json!({"loads_per_tick":16}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    for x in 0..3 {
        assert!(
            has(&runtime, x, "", "a") && has(&runtime, x, "props", "a"),
            "{x}"
        );
    }
    // Disable the layer through the native API: only its content unloads.
    streaming::set_layer_enabled(runtime.app_mut().world_mut(), "streaming", "props", false)
        .unwrap();
    let before = cell_entity_count(&runtime);
    tick(&mut runtime);
    assert_eq!(
        cell_entity_count(&runtime),
        before - 6,
        "3 cells x 2 entities"
    );
    for x in 0..3 {
        assert!(has(&runtime, x, "", "a"));
        assert!(!has(&runtime, x, "props", "a") && !has(&runtime, x, "props", "b"));
    }
    let s = status(&runtime);
    assert_eq!(s.layers.len(), 1);
    assert!(!s.layers[0].enabled);
    tick(&mut runtime);
    assert!(
        !has(&runtime, 0, "props", "a"),
        "stays unloaded while disabled"
    );
    // Re-enable: content streams back in.
    streaming::set_layer_enabled(runtime.app_mut().world_mut(), "streaming", "props", true)
        .unwrap();
    tick(&mut runtime);
    assert!(has(&runtime, 0, "props", "a") && has(&runtime, 2, "props", "b"));
    assert_eq!(cell_entity_count(&runtime), before);
    // The toggle is ordinary authored data.
    let partition = runtime.entity("streaming").unwrap();
    let component = runtime
        .app()
        .world()
        .get::<WorldPartition>(partition)
        .unwrap()
        .clone();
    assert!(component.layer_enabled("props"));
    assert!(
        streaming::set_layer_enabled(runtime.app_mut().world_mut(), "missing", "x", true).is_err()
    );
}

#[test]
fn layers_authored_disabled_never_load() {
    let options = Options {
        cells: vec![(0, ""), (0, "night")],
        partition: json!({"layers":[{"name":"night","enabled":false}]}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    assert!(has(&runtime, 0, "", "a"));
    assert!(!has(&runtime, 0, "night", "a"));
    assert_eq!(
        streaming::cell_state(runtime.app().world(), "streaming", [0, 0, 0], "night"),
        Some(CellState::Unloaded)
    );
}

#[test]
fn events_report_loads_and_unloads_with_cell_and_count() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    tick(&mut runtime);
    let events = runtime.snapshot().events;
    let loads: Vec<_> = events
        .iter()
        .filter(|e| e.name == EVENT_CELL_LOADED)
        .collect();
    assert_eq!(
        loads.len(),
        2,
        "events of tick 1 are delivered on tick 2: {events:?}"
    );
    assert_eq!(loads[0].source.as_deref(), Some("streaming"));
    assert_eq!(loads[0].payload["partition"], "streaming");
    assert_eq!(loads[0].payload["entities"], 2);
    let mut cells: Vec<i64> = loads
        .iter()
        .map(|e| e.payload["cell"][0].as_i64().unwrap())
        .collect();
    cells.sort_unstable();
    assert_eq!(cells, [0, 1]);

    move_scout(&mut runtime, [2000.0, 0.0, 50.0]);
    tick(&mut runtime);
    tick(&mut runtime);
    let events = runtime.snapshot().events;
    let unloads: Vec<_> = events
        .iter()
        .filter(|e| e.name == EVENT_CELL_UNLOADED)
        .collect();
    assert!(
        unloads.iter().all(|e| e.payload["entities"] == 2),
        "{events:?}"
    );
    assert!(unloads.len() >= 2, "{events:?}");
}

#[test]
fn background_loading_reaches_the_same_state_within_budget() {
    let options = Options {
        partition: json!({"synchronous":false,"max_in_flight":2}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while loaded(&runtime) != [0, 1, 2] {
        tick(&mut runtime);
        let s = status(&runtime);
        assert!(
            s.applied_last_tick <= 2 && s.in_flight <= 2 && s.loading <= 2,
            "{s:?}"
        );
        assert!(Instant::now() < deadline, "background load stalled: {s:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(cell_entity_count(&runtime), 6);
    // Moving away cancels or unloads; nothing leaks.
    move_scout(&mut runtime, [5000.0, 0.0, 50.0]);
    for _ in 0..3 {
        tick(&mut runtime);
    }
    assert_eq!(cell_entity_count(&runtime), 0);
    assert_eq!(status(&runtime).in_flight, 0);
}

#[test]
fn cells_far_from_the_origin_keep_exact_f32_local_positions() {
    // Cell 500000 starts at x = 50,000,000 m; naive f32 positions there are 4 m apart.
    let options = Options {
        cells: vec![(500_000, ""), (500_001, "")],
        scout: [50.0, 0.0, 50.0],
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [50_000_000.0, 0.0, 0.0]);
    tick(&mut runtime);
    assert!(has(&runtime, 500_000, "", "a"));
    let a = runtime.entity(&entity_id(500_000, "", "a")).unwrap();
    let world = runtime.app().world();
    assert_eq!(
        world.get::<Transform>(a).unwrap().position,
        [10.25, 0.0, 10.25]
    );
    let absolute = world_transform(world, a).unwrap().position();
    assert_eq!(absolute, [10.25, 0.0, 10.25]);
    let b = runtime.entity(&entity_id(500_000, "", "b")).unwrap();
    assert_eq!(
        world_transform(world, b).unwrap().position(),
        [10.25, 1.0, 10.25]
    );
    // Cell 500001 is 50 m + 50 m from the source: loaded as well, one cell further along.
    let next = runtime.entity(&entity_id(500_001, "", "a")).unwrap();
    assert_eq!(
        world.get::<Transform>(next).unwrap().position,
        [110.25, 0.0, 10.25]
    );
}

#[test]
fn a_far_source_with_an_unshifted_origin_streams_the_same_cell() {
    let options = Options {
        cells: vec![(150, "")],
        scout: [15_050.0, 0.0, 50.0],
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    let a = runtime.entity(&entity_id(150, "", "a")).unwrap();
    assert_eq!(
        runtime.app().world().get::<Transform>(a).unwrap().position,
        [15_010.25, 0.0, 10.25]
    );
}

#[test]
fn rebasing_the_origin_keeps_streamed_content_in_place() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    let a = runtime.entity(&entity_id(1, "", "a")).unwrap();
    assert_eq!(
        runtime.app().world().get::<Transform>(a).unwrap().position,
        [110.25, 0.0, 10.25]
    );
    // Shift the origin by +100 m along x and keep the source at the same absolute place.
    runtime.app_mut().world_mut().insert_resource(WorldOrigin {
        offset: [100.0, 0.0, 0.0],
    });
    move_scout(&mut runtime, [-50.0, 0.0, 50.0]);
    tick(&mut runtime);
    assert_eq!(
        runtime.app().world().get::<Transform>(a).unwrap().position,
        [10.25, 0.0, 10.25],
        "same absolute position, new local coordinates"
    );
    assert_eq!(loaded(&runtime), [0, 1, 2]);
    assert_eq!(status(&runtime).total_unloaded, 0);
    // Content loaded after the rebase is placed consistently.
    move_scout(&mut runtime, [250.0, 0.0, 50.0]);
    tick(&mut runtime);
    let later = runtime.entity(&entity_id(4, "", "a")).unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(later)
            .unwrap()
            .position,
        [310.25, 0.0, 10.25]
    );
}

#[test]
fn request_force_and_release_override_the_sources() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    let world = runtime.app_mut().world_mut();
    // Force-load a cell 850 m away immediately, without ticking.
    streaming::force_load_cell(world, "streaming", [9, 0, 0], "").unwrap();
    assert_eq!(
        streaming::cell_state(world, "streaming", [9, 0, 0], ""),
        Some(CellState::Loaded)
    );
    assert!(runtime.entity(&entity_id(9, "", "a")).is_some());
    // Pinned: survives ticks with the source far away.
    for _ in 0..3 {
        tick(&mut runtime);
    }
    assert!(has(&runtime, 9, "", "a"));
    // A requested cell loads through the normal path.
    let world = runtime.app_mut().world_mut();
    streaming::request_cell(world, "streaming", [7, 0, 0], "").unwrap();
    tick(&mut runtime);
    assert!(has(&runtime, 7, "", "a"));
    // Released cells follow the sources again.
    let world = runtime.app_mut().world_mut();
    streaming::release_cell(world, "streaming", [9, 0, 0], "").unwrap();
    streaming::release_cell(world, "streaming", [7, 0, 0], "").unwrap();
    tick(&mut runtime);
    assert!(!has(&runtime, 9, "", "a") && !has(&runtime, 7, "", "a"));
    // Unknown cells and partitions are errors.
    let world = runtime.app_mut().world_mut();
    assert!(streaming::request_cell(world, "streaming", [99, 0, 0], "").is_err());
    assert!(streaming::request_cell(world, "nope", [0, 0, 0], "").is_err());
    streaming::set_layer_enabled(world, "streaming", "off", false).unwrap();
    assert!(streaming::force_load_cell(world, "streaming", [5, 0, 0], "off").is_err());
}

#[test]
fn requests_made_before_the_first_tick_are_honoured() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    let world = runtime.app_mut().world_mut();
    streaming::request_cell(world, "streaming", [9, 0, 0], "").unwrap();
    assert_eq!(
        streaming::cell_state(world, "streaming", [9, 0, 0], ""),
        Some(CellState::Unloaded)
    );
    tick(&mut runtime);
    assert_eq!(
        loaded(&runtime),
        [0, 9],
        "pinned cells rank first; two per tick"
    );
    assert!(has(&runtime, 9, "", "a"));
}

#[test]
fn a_cell_that_cannot_be_read_fails_with_a_diagnostic_and_backs_off() {
    let dir = fixture(&Options::default());
    // The file disappears after the project validated.
    let project = Project::load(dir.path().join("project.json")).unwrap();
    fs::remove_file(dir.path().join("cells/1.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    for _ in 0..12 {
        tick(&mut runtime);
    }
    assert_eq!(loaded(&runtime), [0, 2]);
    let s = status(&runtime);
    assert_eq!(
        s.total_failed, 3,
        "three attempts with growing backoff, then it gives up"
    );
    assert!(
        s.diagnostics.iter().any(|d| d.contains("1_0_0")),
        "{:?}",
        s.diagnostics
    );
}

#[test]
fn entity_id_collisions_with_the_world_fail_the_cell_without_partial_spawns() {
    let dir = fixture(&Options::default());
    // Cell 0's entity id equals an id already in the scene.
    let clash = fs::read_to_string(dir.path().join("cells/0.json"))
        .unwrap()
        .replace("c0_a", "scout");
    fs::write(dir.path().join("cells/0.json"), clash).unwrap();
    let mut runtime = start(&dir, [0.0; 3]);
    for _ in 0..3 {
        tick(&mut runtime);
    }
    assert_eq!(loaded(&runtime), [1, 2]);
    assert!(!has(&runtime, 0, "", "b"), "no partial spawn");
    assert!(runtime.entity("scout").is_some());
    assert!(
        status(&runtime)
            .diagnostics
            .iter()
            .any(|d| d.contains("already exists"))
    );
}

#[test]
fn camera_is_a_source_when_enabled() {
    let options = Options {
        partition: json!({"camera_source":true}),
        scout: [5000.0, 0.0, 0.0],
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    // The scout is far away; the camera at (0,0,0) pulls cells 0 and 1.
    assert_eq!(loaded(&runtime), [0, 1]);
    assert_eq!(status(&runtime).sources, 2);
}

#[test]
fn source_priority_and_scale_decide_the_order() {
    let options = Options {
        partition: json!({"loads_per_tick":1}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    // A second, high-priority source over cell 8.
    let world = runtime.app_mut().world_mut();
    world.spawn((
        Transform {
            position: [850.0, 0.0, 50.0],
            ..Transform::default()
        },
        streaming::StreamingSource {
            priority: 10,
            radius_scale: 0.5,
            ..streaming::StreamingSource::default()
        },
    ));
    tick(&mut runtime);
    assert_eq!(
        loaded(&runtime),
        [8],
        "priority 10 first, even though cell 0 is nearer the camera"
    );
    tick(&mut runtime);
    assert_eq!(
        loaded(&runtime),
        [7, 8],
        "its neighbours are 50 m away, inside its 75 m radius"
    );
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [7, 8, 9]);
    tick(&mut runtime);
    assert_eq!(
        loaded(&runtime),
        [0, 7, 8, 9],
        "only then the priority-0 source"
    );
}

#[test]
fn hlod_proxy_is_visible_exactly_when_the_cell_is_not_loaded() {
    let options = Options {
        hlod: true,
        partition: json!({"loads_per_tick":1}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    let proxy_ids = |runtime: &Runtime| -> Vec<i32> {
        let frame = runtime.extract_scene_frame().unwrap();
        let mut found: Vec<i32> = frame
            .meshes
            .iter()
            .filter_map(|m| m.id.strip_prefix("hlod:streaming:"))
            .map(|name| name.split('_').next().unwrap().parse().unwrap())
            .collect();
        found.sort_unstable();
        found
    };
    let check = |runtime: &Runtime| {
        let not_loaded: Vec<i32> = (0..10).filter(|x| !loaded(runtime).contains(x)).collect();
        assert_eq!(proxy_ids(runtime), not_loaded, "proxies == not loaded");
        assert_eq!(status(runtime).proxies, not_loaded.len());
    };
    tick(&mut runtime);
    assert_eq!(loaded(&runtime), [0]);
    check(&runtime);
    for _ in 0..3 {
        tick(&mut runtime);
        check(&runtime);
    }
    assert_eq!(loaded(&runtime), [0, 1, 2]);
    assert_eq!(proxy_ids(&runtime), [3, 4, 5, 6, 7, 8, 9]);
    move_scout(&mut runtime, [650.0, 0.0, 50.0]);
    for _ in 0..8 {
        tick(&mut runtime);
        check(&runtime);
    }
    assert!(!loaded(&runtime).contains(&0));
    assert!(proxy_ids(&runtime).contains(&0));
    // The proxy sits at its cell's origin and carries the cell mesh geometry.
    let proxy = runtime.entity("hlod:streaming:0_0_0").unwrap();
    let world = runtime.app().world();
    assert_eq!(
        world.get::<Transform>(proxy).unwrap().position,
        [0.0, 0.0, 0.0]
    );
    let geometry = world
        .get::<engine_project::GeometryOverride>(proxy)
        .unwrap();
    assert_eq!(geometry.0.indices.len(), 3);
    let marker = world.get::<StreamedCell>(proxy).unwrap();
    assert!(marker.proxy && marker.key == [0, 0, 0]);
    // Cell 2's proxy is at x = 200.
    let far = runtime.entity("hlod:streaming:2_0_0").unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(far)
            .unwrap()
            .position,
        [200.0, 0.0, 0.0]
    );
}

#[test]
fn hlod_respects_radius_layers_and_the_switch() {
    let mut cells: Vec<(i32, &'static str)> = (0..6).map(|x| (x, "")).collect();
    cells.push((5, "props"));
    let options = Options {
        cells,
        hlod: true,
        partition: json!({"hlod_radius":300.0,"loads_per_tick":8}),
        ..Options::default()
    };
    let dir = fixture(&options);
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    let proxy =
        |runtime: &Runtime, name: &str| runtime.entity(&format!("hlod:streaming:{name}")).is_some();
    // Source at x = 50: loaded 0..2; proxies only within 300 m: cells 3 (d 250) and not 4 (d 350).
    assert!(proxy(&runtime, "3_0_0") && !proxy(&runtime, "4_0_0") && !proxy(&runtime, "0_0_0"));
    assert!(!proxy(&runtime, "5_0_0@props"));
    // Disabling a layer hides its proxy too (enable it near the cell first).
    move_scout(&mut runtime, [450.0, 0.0, 50.0]);
    tick(&mut runtime);
    assert!(
        !proxy(&runtime, "5_0_0@props"),
        "cell 5 props is loaded: {}",
        status(&runtime).loaded
    );
    streaming::set_layer_enabled(runtime.app_mut().world_mut(), "streaming", "props", false)
        .unwrap();
    tick(&mut runtime);
    assert!(
        !proxy(&runtime, "5_0_0@props"),
        "disabled layers show nothing"
    );
    // Turning hlod off removes every proxy.
    let partition = runtime.entity("streaming").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<WorldPartition>(partition)
        .unwrap()
        .hlod = false;
    tick(&mut runtime);
    assert_eq!(status(&runtime).proxies, 0);
}

#[test]
fn invalid_authoring_is_rejected_before_running() {
    for bad in [
        json!({"load_radius":300.0,"unload_radius":100.0}),
        json!({"loads_per_tick":0}),
        json!({"max_in_flight":65}),
        json!({"layers":[{"name":"a"},{"name":"a"}]}),
        json!({"manifest":"missing"}),
    ] {
        let options = Options {
            partition: bad.clone(),
            ..Options::default()
        };
        let dir = fixture(&options);
        assert!(
            Project::load(dir.path().join("project.json"))
                .and_then(|project| project.validate())
                .is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_broken_manifest_is_reported_not_retried_every_tick() {
    let dir = fixture(&Options::default());
    fs::write(
        dir.path().join("world.partition.json"),
        r#"{"schema":1,"cell_size":100.0,"cells":[{"key":[0,0,0],"file":"../x.json"}]}"#,
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json"));
    // The project loads (assets are validated on demand) but validation catches it.
    if let Ok(project) = project {
        assert!(project.validate().is_err());
        let mut runtime = Runtime::new(project).unwrap();
        runtime.start().unwrap();
        for _ in 0..3 {
            tick(&mut runtime);
        }
        let s = status(&runtime);
        assert!(s.diagnostics[0].contains("project-relative"), "{s:?}");
        assert_eq!(s.cells, 0);
        assert_eq!(
            runtime
                .snapshot()
                .logs
                .iter()
                .filter(|l| l.contains("streaming"))
                .count(),
            1
        );
    }
}

#[test]
fn removing_the_partition_entity_unloads_its_content() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    assert_eq!(cell_entity_count(&runtime), 4);
    let partition = runtime.entity("streaming").unwrap();
    runtime.app_mut().world_mut().despawn(partition);
    tick(&mut runtime);
    assert_eq!(cell_entity_count(&runtime), 0);
    assert!(!has(&runtime, 0, "", "a"));
    assert!(streaming::partition_status(runtime.app().world(), "streaming").is_none());
}

#[test]
fn snapshot_carries_bounded_streaming_status() {
    let dir = fixture(&Options::default());
    let mut runtime = start(&dir, [0.0; 3]);
    tick(&mut runtime);
    let snapshot = runtime.snapshot();
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        value["streaming"]["streaming"]["loaded"], 2,
        "{}",
        value["streaming"]
    );
    assert!(serde_json::to_vec(&value["streaming"]).unwrap().len() < 8 * 1024);
}
