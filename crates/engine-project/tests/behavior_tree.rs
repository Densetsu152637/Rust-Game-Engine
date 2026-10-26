use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::{
    ai::{NativeTask, TaskContext, TaskStatus, register_bt_task},
    simulation::{BakeInput, BakeSettings, NavMeshDocument, NavigationAgent, Obstacle},
    *,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

/// Minimal behavior tree document builder.
struct Tree {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    variables: Vec<Value>,
    types: BTreeMap<String, String>,
}
impl Tree {
    fn new() -> Self {
        Self {
            nodes: vec![json!({"id":"root","node_type":"root"})],
            edges: vec![],
            variables: vec![],
            types: BTreeMap::from([("root".to_owned(), "root".to_owned())]),
        }
    }
    fn node(mut self, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id":id,"node_type":ty,"params":params}));
        self.types.insert(id.into(), ty.into());
        self
    }
    fn var(mut self, name: &str, ty: &str, default: Value) -> Self {
        let mut v = json!({"name":name,"type":ty});
        if !default.is_null() {
            v["default"] = default;
        }
        self.variables.push(v);
        self
    }
    fn link(mut self, parent: &str, child: &str) -> Self {
        let port = match self.types[parent].as_str() {
            "sequence" | "selector" | "parallel" | "random_selector" => "children",
            _ => "child",
        };
        self.edges
            .push(json!({"from":{"node":parent,"port":port},"to":{"node":child,"port":"parent"}}));
        self
    }
    fn service(mut self, owner: &str, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id":id,"node_type":ty,"params":params}));
        self.edges
            .push(json!({"from":{"node":owner,"port":"services"},"to":{"node":id,"port":"owner"}}));
        self
    }
    fn json(self) -> Value {
        json!({"version":1,"kind":"behavior_tree","nodes":self.nodes,"edges":self.edges,"variables":self.variables})
    }
}

/// patrol: sequence [move A, event a, move B, event b]
fn patrol() -> Value {
    Tree::new()
        .node("seq", "sequence", json!({}))
        .node("a", "move_to", json!({"target":[2.0,0.0,1.0]}))
        .node(
            "ea",
            "emit_event",
            json!({"event":"patrol.a","payload":{"leg":"a"}}),
        )
        .node("b", "move_to", json!({"target":[8.0,0.0,1.0]}))
        .node(
            "eb",
            "emit_event",
            json!({"event":"patrol.b","payload":{"leg":"b"}}),
        )
        .link("root", "seq")
        .link("seq", "a")
        .link("seq", "ea")
        .link("seq", "b")
        .link("seq", "eb")
        .json()
}

const BINDINGS: [&str; 7] = [
    "navigation.repathed",
    "navigation.arrived",
    "navigation.failed",
    "patrol.a",
    "patrol.b",
    "behavior_tree.finished",
    "behavior_tree.aborted",
];

fn agent_entity(id: &str, bt: Value, nav: bool, extra_events: &[&str]) -> Value {
    let mut custom = json!({"engine.BehaviorTreeAgent":{"version":1,"data":bt}});
    if nav {
        custom["engine.NavigationAgent"] = json!({"version":1,"data":{"speed":4}});
    }
    let bindings: Vec<Value> = BINDINGS
        .iter()
        .chain(extra_events)
        .map(|e| json!({"event":e,"handler":"record"}))
        .collect();
    json!({"id":id,"name":id,"components":{
        "Transform":{"position":[1,0,1]},
        "EventBindings":{"bindings":bindings},
        "custom":custom}})
}

fn project(graphs: &[(&str, Value)], entities: Vec<Value>) -> tempfile::TempDir {
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
    let mut assets = vec![json!({"id":"nav","kind":"navigation","path":"navigation.json"})];
    for (id, graph) in graphs {
        let file = format!("{id}.graph.json");
        fs::write(
            dir.path().join(&file),
            serde_json::to_vec_pretty(graph).unwrap(),
        )
        .unwrap();
        assets.push(json!({"id":id,"kind":"graph","path":file}));
    }
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"bt-test","startup_scene":"scene.json","assets":assets})
            .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    dir
}

#[derive(Resource, Default)]
struct Log(Vec<(String, Value)>);
fn record(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Log>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}

fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime
}
fn tick(runtime: &mut Runtime, n: usize, dt: f32) {
    for _ in 0..n {
        runtime.tick(dt, InputSnapshot::default()).unwrap();
    }
}
fn log(runtime: &Runtime) -> Vec<(String, Value)> {
    runtime.app().world().resource::<Log>().0.clone()
}
fn names(runtime: &Runtime) -> Vec<String> {
    log(runtime).into_iter().map(|(n, _)| n).collect()
}

fn patrol_dir() -> tempfile::TempDir {
    project(
        &[("patrol", patrol())],
        vec![agent_entity(
            "guard",
            json!({"graph":"patrol","navmesh":"nav"}),
            true,
            &[],
        )],
    )
}

/// One run of the patrol scenario: events and telemetry.
fn patrol_run() -> (Vec<(String, Value)>, Value, Vec<f32>) {
    let dir = patrol_dir();
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    let guard = runtime.entity("guard").unwrap();
    let mut xs = Vec::new();
    for _ in 0..140 {
        tick(&mut runtime, 1, 0.1);
        xs.push(
            world_transform(runtime.app().world(), guard)
                .unwrap()
                .position()[0],
        );
    }
    let snapshot = runtime.try_snapshot().unwrap();
    let telemetry = serde_json::to_value(&snapshot.behavior_trees).unwrap();
    (log(&runtime), telemetry, xs)
}

#[test]
fn a_behavior_tree_patrols_between_two_points_through_the_navigation_agent() {
    let (events, telemetry, xs) = patrol_run();
    let names: Vec<_> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        &names[..7],
        [
            "navigation.repathed",
            "navigation.arrived",
            "patrol.a",
            "navigation.repathed",
            "navigation.arrived",
            "patrol.b",
            "behavior_tree.finished",
        ],
        "first lap in order: {names:?}"
    );
    assert_eq!(events[2].1, json!({"leg":"a"}));
    assert_eq!(events[6].1["status"], "success");
    assert_eq!(events[6].1["graph"], "patrol");
    let laps = names
        .iter()
        .filter(|n| **n == "behavior_tree.finished")
        .count();
    assert!(laps >= 2, "the tree restarts and laps again: {names:?}");
    assert!(xs.iter().cloned().fold(0.0f32, f32::max) > 7.9, "reaches B");
    assert!(
        xs[40..].iter().cloned().fold(f32::MAX, f32::min) < 2.1,
        "returns to A"
    );

    // Identical runs give identical events and telemetry.
    let (events2, telemetry2, xs2) = patrol_run();
    assert_eq!(events, events2);
    assert_eq!(telemetry, telemetry2);
    assert_eq!(xs, xs2);

    assert_eq!(telemetry["total"], 1);
    let agent = &telemetry["agents"][0];
    assert_eq!(agent["entity"], "guard");
    assert_eq!(agent["graph"], "patrol");
    assert_eq!(agent["ticks"], 140);
    assert_eq!(agent["active_path"][0], "root");
    assert_eq!(telemetry["dropped_events"], 0);
}

#[test]
fn the_patrol_does_not_overflow_the_event_queue() {
    // Many agents each emitting every tick stay inside the per-tick budget.
    let graph = Tree::new()
        .node("seq", "sequence", json!({}))
        .node("e", "emit_event", json!({"event":"patrol.a"}))
        .link("root", "seq")
        .link("seq", "e")
        .json();
    let entities: Vec<Value> = (0..40)
        .map(|i| agent_entity(&format!("a{i:02}"), json!({"graph":"spam"}), false, &[]))
        .collect();
    let dir = project(&[("spam", graph)], entities);
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 30, 0.1);
    let delivered = names(&runtime).iter().filter(|n| *n == "patrol.a").count();
    assert!(delivered > 0);
    let telemetry = serde_json::to_value(runtime.try_snapshot().unwrap().behavior_trees).unwrap();
    assert!(
        telemetry["dropped_events"].as_u64().unwrap() > 0,
        "budget overflow is counted"
    );
    assert_eq!(telemetry["total"], 40);
    assert_eq!(
        telemetry["agents"].as_array().unwrap().len(),
        32,
        "telemetry is capped"
    );
}

struct Counter;
impl NativeTask<World> for Counter {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("native.start".into(), json!(cx.params)));
        cx.blackboard.set_json("seen", &json!(true)).unwrap();
        TaskStatus::Running
    }
    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("native.tick".into(), json!(cx.dt)));
        TaskStatus::Success
    }
    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("native.abort".into(), Value::Null));
    }
}
fn counter(_: &mut World, _: u64) -> std::result::Result<Box<dyn NativeTask<World>>, String> {
    Ok(Box::new(Counter))
}

#[test]
fn native_tasks_run_against_the_world_and_feed_the_blackboard() {
    let graph = Tree::new()
        .var("seen", "bool", Value::Null)
        .node("seq", "sequence", json!({}))
        .node("n", "native_task", json!({"name":"count","params":{"n":1}}))
        .node(
            "c",
            "blackboard_condition",
            json!({"key":"seen","operator":"eq","value":true}),
        )
        .node("w", "wait", json!({"seconds":100.0}))
        .link("root", "seq")
        .link("seq", "n")
        .link("seq", "c")
        .link("c", "w")
        .json();
    let dir = project(
        &[("native", graph)],
        vec![agent_entity("g", json!({"graph":"native"}), false, &[])],
    );
    let mut runtime = runtime(&dir);
    register_bt_task(runtime.app_mut(), "count", counter).unwrap();
    assert!(
        register_bt_task(runtime.app_mut(), "count", counter).is_err(),
        "names are unique"
    );
    runtime.start().unwrap();
    tick(&mut runtime, 3, 0.1);
    let names = names(&runtime);
    assert_eq!(names[..2], ["native.start", "native.tick"], "{names:?}");
    let telemetry = serde_json::to_value(runtime.try_snapshot().unwrap().behavior_trees).unwrap();
    assert_eq!(telemetry["agents"][0]["blackboard"]["seen"], true);
    assert_eq!(
        telemetry["agents"][0]["active_path"],
        json!(["root", "seq", "c", "w"])
    );
}

struct Hold;
impl NativeTask<World> for Hold {
    fn start(&mut self, _: &mut TaskContext<'_, World>) -> TaskStatus {
        TaskStatus::Running
    }
    fn tick(&mut self, _: &mut TaskContext<'_, World>) -> TaskStatus {
        TaskStatus::Running
    }
    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("native.abort".into(), Value::Null));
    }
}
fn hold(_: &mut World, _: u64) -> std::result::Result<Box<dyn NativeTask<World>>, String> {
    Ok(Box::new(Hold))
}

#[test]
fn stopping_the_runtime_aborts_running_native_tasks() {
    let graph = Tree::new()
        .node("n", "native_task", json!({"name":"hold"}))
        .link("root", "n")
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity("g", json!({"graph":"g"}), false, &[])],
    );
    let mut runtime = runtime(&dir);
    register_bt_task(runtime.app_mut(), "hold", hold).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime, 3, 0.1);
    assert!(!names(&runtime).contains(&"native.abort".to_owned()));
    runtime.stop().unwrap();
    assert_eq!(
        names(&runtime),
        ["native.abort"],
        "the running task is aborted once"
    );
}

#[test]
fn an_unregistered_native_task_fails_start() {
    let graph = Tree::new()
        .node("n", "native_task", json!({"name":"missing"}))
        .link("root", "n")
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity("g", json!({"graph":"g"}), false, &[])],
    );
    let mut runtime = runtime(&dir);
    let error = runtime.start().unwrap_err();
    assert_eq!(
        error.diagnostics[0].code,
        "behavior_tree.native.unregistered"
    );
}

#[test]
fn disabling_the_agent_aborts_the_tree_and_reports_it() {
    let dir = patrol_dir();
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 2, 0.1);
    let guard = runtime.entity("guard").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::ai::BehaviorTreeAgent>(guard)
        .unwrap()
        .enabled = false;
    tick(&mut runtime, 2, 0.1);
    let aborted: Vec<_> = log(&runtime)
        .into_iter()
        .filter(|(n, _)| n == "behavior_tree.aborted")
        .collect();
    assert_eq!(aborted.len(), 1);
    assert_eq!(aborted[0].1["reason"], "tree_stopped");
    assert_eq!(aborted[0].1["node"], "root");
    let telemetry = serde_json::to_value(runtime.try_snapshot().unwrap().behavior_trees).unwrap();
    assert_eq!(telemetry["agents"][0]["enabled"], false);
    assert_eq!(telemetry["agents"][0]["active_path"], json!([]));
    // The navigation request was cancelled with the tree.
    let nav = runtime.app().world().get::<NavigationAgent>(guard).unwrap();
    assert!(nav.target.is_none() && nav.navmesh.is_none());
}

#[test]
fn services_emit_events_at_their_interval() {
    let graph = Tree::new()
        .node("seq", "sequence", json!({}))
        .node("w", "wait", json!({"seconds": 100.0}))
        .service(
            "seq",
            "svc",
            "emit_event_service",
            json!({"event":"patrol.a","interval":0.3}),
        )
        .link("root", "seq")
        .link("seq", "w")
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity("g", json!({"graph":"g"}), false, &[])],
    );
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 10, 0.1);
    let count = names(&runtime).iter().filter(|n| *n == "patrol.a").count();
    assert_eq!(
        count, 3,
        "runs at ticks 1, 4, 7 and 10; the last is delivered next tick"
    );
}

#[test]
fn tick_interval_throttles_the_tree() {
    let graph = Tree::new()
        .node("w", "wait", json!({"seconds": 100.0}))
        .link("root", "w")
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity(
            "g",
            json!({"graph":"g","tick_interval":0.5}),
            false,
            &[],
        )],
    );
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 20, 0.1);
    let telemetry = serde_json::to_value(runtime.try_snapshot().unwrap().behavior_trees).unwrap();
    assert_eq!(
        telemetry["agents"][0]["ticks"], 4,
        "0.5 s interval at 0.1 s ticks: every fifth"
    );
}

#[test]
fn blackboard_overrides_and_observer_aborts_work_in_the_runtime() {
    let graph = Tree::new()
        .var("alarm", "bool", json!(false))
        .node("sel", "selector", json!({}))
        .node(
            "c",
            "blackboard_condition",
            json!({"key":"alarm","operator":"eq","value":true,"abort":"lower_priority"}),
        )
        .node("hit", "emit_event", json!({"event":"patrol.a"}))
        .node("w", "wait", json!({"seconds":100.0}))
        .link("root", "sel")
        .link("sel", "c")
        .link("c", "hit")
        .link("sel", "w")
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity(
            "g",
            json!({"graph":"g","blackboard":{"alarm":false}}),
            false,
            &[],
        )],
    );
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 3, 0.1);
    assert!(
        !names(&runtime).contains(&"patrol.a".to_owned()),
        "alarm override is false"
    );
    // A native system raises the key; the preemption lands in the next tick.
    let g = runtime.entity("g").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<engine_project::ai::BehaviorTrees>()
        .blackboard_mut(g)
        .unwrap()
        .set("alarm", engine_project::ai::BbValue::Bool(true))
        .unwrap();
    tick(&mut runtime, 2, 0.1);
    let events = log(&runtime);
    assert!(
        events.iter().any(|(n, _)| n == "patrol.a"),
        "the high priority branch ran: {events:?}"
    );
    let aborted = events
        .iter()
        .find(|(n, _)| n == "behavior_tree.aborted")
        .unwrap();
    assert_eq!(
        aborted.1,
        json!({"graph":"g","node":"w","reason":"observer_lower_priority","key":"alarm"})
    );
}

#[test]
fn a_blackboard_override_starts_the_tree_in_that_state() {
    let graph = Tree::new()
        .var("alarm", "bool", json!(false))
        .node(
            "c",
            "blackboard_condition",
            json!({"key":"alarm","operator":"eq","value":true}),
        )
        .node("hit", "emit_event", json!({"event":"patrol.a"}))
        .link("root", "c")
        .link("c", "hit")
        .json();
    for (alarm, expected) in [(true, true), (false, false)] {
        let dir = project(
            &[("g", graph.clone())],
            vec![agent_entity(
                "g",
                json!({"graph":"g","blackboard":{"alarm":alarm}}),
                false,
                &[],
            )],
        );
        let mut runtime = runtime(&dir);
        runtime.start().unwrap();
        tick(&mut runtime, 3, 0.1);
        assert_eq!(
            names(&runtime).contains(&"patrol.a".to_owned()),
            expected,
            "alarm={alarm}"
        );
    }
}

#[test]
fn project_validation_rejects_bad_graph_references() {
    let load = |graph: Option<Value>, bt: Value, nav: bool| {
        let graphs: Vec<(&str, Value)> = graph.into_iter().map(|g| ("g", g)).collect();
        let dir = project(&graphs, vec![agent_entity("e", bt, nav, &[])]);
        Project::load(dir.path().join("project.json"))
            .map(|_| ())
            .unwrap_err()
    };
    let wait = || {
        Tree::new()
            .node("w", "wait", json!({}))
            .link("root", "w")
            .json()
    };
    // unknown graph asset
    assert_eq!(
        load(None, json!({"graph":"g"}), false).diagnostics[0].code,
        "project.asset.unknown"
    );
    // empty graph id
    assert_eq!(
        load(Some(wait()), json!({}), false).diagnostics[0].code,
        "component.schema.invalid",
        "graph is required by the schema"
    );
    assert_eq!(
        load(Some(wait()), json!({"graph":""}), false).diagnostics[0].code,
        "behavior_tree.graph.missing"
    );
    // a graph that is not valid
    let broken = Tree::new()
        .node("w", "wait", json!({"seconds":-3}))
        .link("root", "w")
        .json();
    assert_eq!(
        load(Some(broken), json!({"graph":"g"}), false).diagnostics[0].code,
        "graph.param.range"
    );
    // another graph kind
    let mut other = wait();
    other["kind"] = json!("visual_script");
    assert_eq!(
        load(Some(other), json!({"graph":"g"}), false).diagnostics[0].code,
        "graph.kind.unknown"
    );
    // override type mismatch
    let typed = Tree::new()
        .var("hp", "int", Value::Null)
        .node("w", "wait", json!({}))
        .link("root", "w")
        .json();
    assert_eq!(
        load(
            Some(typed.clone()),
            json!({"graph":"g","blackboard":{"hp":"x"}}),
            false
        )
        .diagnostics[0]
            .code,
        "behavior_tree.blackboard.invalid"
    );
    assert_eq!(
        load(
            Some(typed),
            json!({"graph":"g","blackboard":{"nope":1}}),
            false
        )
        .diagnostics[0]
            .code,
        "behavior_tree.blackboard.invalid"
    );
    // move_to needs a NavigationAgent and a navmesh
    let moving = Tree::new()
        .node("m", "move_to", json!({"target":[1,0,1]}))
        .link("root", "m")
        .json();
    assert_eq!(
        load(
            Some(moving.clone()),
            json!({"graph":"g","navmesh":"nav"}),
            false
        )
        .diagnostics[0]
            .code,
        "behavior_tree.navigation.missing"
    );
    assert_eq!(
        load(Some(moving.clone()), json!({"graph":"g"}), true).diagnostics[0].code,
        "behavior_tree.navigation.missing"
    );
    assert_eq!(
        load(Some(moving), json!({"graph":"g","navmesh":"g"}), true).diagnostics[0].code,
        "project.asset.kind"
    );
    // schema: unknown field, bad interval
    assert_eq!(
        load(Some(wait()), json!({"graph":"g","bogus":1}), false).diagnostics[0].code,
        "component.schema.invalid"
    );
    assert_eq!(
        load(Some(wait()), json!({"graph":"g","tick_interval":-1}), false).diagnostics[0].code,
        "component.schema.invalid"
    );
}

#[test]
fn a_valid_project_loads_and_projects_without_agents_have_no_telemetry() {
    let dir = patrol_dir();
    Project::load(dir.path().join("project.json")).unwrap();
    let plain = project(
        &[],
        vec![json!({"id":"e","name":"e","components":{"Transform":{"position":[0,0,0]}}})],
    );
    let mut runtime = runtime(&plain);
    runtime.start().unwrap();
    tick(&mut runtime, 1, 0.1);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert!(snapshot.get("behavior_trees").is_none());
}

#[test]
fn the_component_is_listed_with_a_schema_and_defaults() {
    let dir = patrol_dir();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let descriptor = &project.component_catalog.0["engine.BehaviorTreeAgent"];
    assert_eq!(descriptor.version, 1);
    assert_eq!(descriptor.defaults["enabled"], true);
    assert_eq!(descriptor.defaults["tick_interval"], 0.0);
    assert_eq!(descriptor.schema["required"], json!(["graph"]));
}
