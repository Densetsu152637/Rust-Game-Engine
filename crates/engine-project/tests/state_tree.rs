use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::{
    ai::{
        NativeTask, TaskContext, TaskStatus, register_state_tree_evaluator,
        register_state_tree_task,
    },
    simulation::{BakeInput, BakeSettings, NavMeshDocument, NavigationAgent, Obstacle},
    *,
};
use serde_json::{Value, json};
use std::fs;

/// Minimal state tree document builder.
struct Tree {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    variables: Vec<Value>,
}
impl Tree {
    fn new() -> Self {
        Self {
            nodes: vec![json!({"id":"root","node_type":"root"})],
            edges: vec![],
            variables: vec![],
        }
    }
    fn add(mut self, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id":id,"node_type":ty,"params":params}));
        self
    }
    fn edge(mut self, from: &str, port: &str, to: &str, to_port: &str) -> Self {
        self.edges
            .push(json!({"from":{"node":from,"port":port},"to":{"node":to,"port":to_port}}));
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
    fn state(self, id: &str, parent: &str, params: Value) -> Self {
        self.add(id, "state", params)
            .edge(parent, "children", id, "parent")
    }
    fn task(self, owner: &str, id: &str, ty: &str, params: Value) -> Self {
        self.add(id, ty, params).edge(owner, "tasks", id, "owner")
    }
    fn go(self, owner: &str, id: &str, params: Value) -> Self {
        self.add(id, "transition", params)
            .edge(owner, "transitions", id, "owner")
    }
    fn cond(self, owner: &str, id: &str, key: &str, op: &str, value: Value) -> Self {
        self.add(
            id,
            "blackboard_condition",
            json!({"key":key,"operator":op,"value":value}),
        )
        .edge(owner, "conditions", id, "owner")
    }
    fn eval(self, id: &str, ty: &str, params: Value) -> Self {
        self.add(id, ty, params)
            .edge("root", "evaluators", id, "owner")
    }
    fn json(self) -> Value {
        json!({"version":1,"kind":"state_tree","nodes":self.nodes,"edges":self.edges,"variables":self.variables})
    }
}

/// patrol { leg_a: move A; leg_b: move B; announce: emit patrol.rest } -> (event) idle
/// idle: wait 0.5 -> patrol
fn guard() -> Value {
    Tree::new()
        .state("patrol", "root", json!({}))
        .go(
            "patrol",
            "rest",
            json!({"trigger":"on_event","event":"patrol.rest","target":"state","state":"idle"}),
        )
        .state("leg_a", "patrol", json!({}))
        .task("leg_a", "ma", "move_to", json!({"target":[2.0,0.0,1.0]}))
        .go(
            "leg_a",
            "a_done",
            json!({"trigger":"on_succeeded","target":"next_sibling"}),
        )
        .state("leg_b", "patrol", json!({}))
        .task("leg_b", "mb", "move_to", json!({"target":[8.0,0.0,1.0]}))
        .go(
            "leg_b",
            "b_done",
            json!({"trigger":"on_succeeded","target":"next_sibling"}),
        )
        .state("announce", "patrol", json!({}))
        .task(
            "announce",
            "say",
            "emit_event",
            json!({"event":"patrol.rest","payload":{"at":"b"}}),
        )
        .task("announce", "hold", "wait", json!({"seconds":100.0}))
        .state("idle", "root", json!({}))
        .task("idle", "nap", "wait", json!({"seconds":0.5}))
        .go(
            "idle",
            "wake",
            json!({"trigger":"on_completed","target":"state","state":"patrol"}),
        )
        .json()
}

const BINDINGS: [&str; 7] = [
    "navigation.repathed",
    "navigation.arrived",
    "navigation.failed",
    "patrol.rest",
    "state_tree.state_entered",
    "state_tree.state_exited",
    "state_tree.finished",
];

fn agent_entity(id: &str, agent: Value, nav: bool) -> Value {
    let mut custom = json!({"engine.StateTreeAgent":{"version":1,"data":agent}});
    if nav {
        custom["engine.NavigationAgent"] = json!({"version":1,"data":{"speed":4}});
    }
    let bindings: Vec<Value> = BINDINGS
        .iter()
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
        json!({"schema":1,"name":"st-test","startup_scene":"scene.json","assets":assets})
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
/// Recorded events that are not navigation chatter, as `name` or `name:state`.
fn flow(events: &[(String, Value)]) -> Vec<String> {
    events
        .iter()
        .filter(|(n, _)| !n.starts_with("navigation."))
        .map(|(n, p)| match p.get("state").and_then(Value::as_str) {
            Some(state) => format!("{}:{state}", n.trim_start_matches("state_tree.")),
            None => n.clone(),
        })
        .collect()
}
fn state_trees(runtime: &Runtime) -> Value {
    serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap()
}

fn guard_dir() -> tempfile::TempDir {
    project(
        &[("guard", guard())],
        vec![agent_entity(
            "guard",
            json!({"graph":"guard","navmesh":"nav"}),
            true,
        )],
    )
}

/// One run of the patrol scenario: events, telemetry and the x position per tick.
fn patrol_run() -> (Vec<(String, Value)>, Value, Vec<f32>) {
    let dir = guard_dir();
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    let guard = runtime.entity("guard").unwrap();
    let mut xs = Vec::new();
    for _ in 0..200 {
        tick(&mut runtime, 1, 0.1);
        xs.push(
            world_transform(runtime.app().world(), guard)
                .unwrap()
                .position()[0],
        );
    }
    let telemetry = state_trees(&runtime);
    (log(&runtime), telemetry, xs)
}

#[test]
fn a_state_tree_patrols_idles_and_switches_on_an_emitted_event() {
    let (events, telemetry, xs) = patrol_run();
    let flow = flow(&events);
    // The first lap: A, B, the announcement, then the emitted event idles the guard.
    assert_eq!(
        flow[..11],
        [
            "state_entered:patrol",
            "state_entered:leg_a",
            "state_exited:leg_a",
            "state_entered:leg_b",
            "state_exited:leg_b",
            "state_entered:announce",
            "patrol.rest",
            "state_exited:announce",
            "state_exited:patrol",
            "state_entered:idle",
            "state_exited:idle",
        ],
        "{flow:?}"
    );
    // The guard then wakes up and patrols again.
    assert_eq!(flow[11], "state_entered:patrol");
    assert_eq!(flow[12], "state_entered:leg_a");
    let rest = events.iter().find(|(n, _)| n == "patrol.rest").unwrap();
    assert_eq!(rest.1, json!({"at":"b"}));
    let entered = events
        .iter()
        .find(|(n, _)| n == "state_tree.state_entered")
        .unwrap();
    assert_eq!(entered.1, json!({"graph":"guard","state":"patrol"}));
    assert!(xs.iter().cloned().fold(0.0f32, f32::max) > 7.9, "reaches B");
    assert!(
        xs[40..].iter().cloned().fold(f32::MAX, f32::min) < 2.1,
        "the guard heads back to A after resting"
    );

    // Identical runs give identical events, telemetry and motion.
    let (events2, telemetry2, xs2) = patrol_run();
    assert_eq!(events, events2);
    assert_eq!(telemetry, telemetry2);
    assert_eq!(xs, xs2);

    assert_eq!(telemetry["total"], 1);
    assert_eq!(telemetry["dropped_events"], 0);
    let agent = &telemetry["agents"][0];
    assert_eq!(agent["entity"], "guard");
    assert_eq!(agent["graph"], "guard");
    assert_eq!(agent["enabled"], true);
    assert_eq!(agent["ticks"], 200);
    assert_eq!(agent["phase"], "running");
    assert!(
        agent["active_path"][0] == "patrol" || agent["active_path"][0] == "idle",
        "{agent}"
    );
    let transitions = agent["transitions"].as_array().unwrap();
    assert!(!transitions.is_empty() && transitions.len() <= 8);
    assert!(transitions.iter().all(|t| t["event"] == "transition"));
    assert!(transitions.iter().any(|t| t["id"] == "rest"));
}

#[test]
fn disabling_the_agent_exits_its_states_and_cancels_navigation() {
    let dir = guard_dir();
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 2, 0.1);
    let guard = runtime.entity("guard").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::ai::StateTreeAgent>(guard)
        .unwrap()
        .enabled = false;
    tick(&mut runtime, 2, 0.1);
    let exits: Vec<String> = flow(&log(&runtime))
        .into_iter()
        .filter(|f| f.starts_with("state_exited"))
        .collect();
    assert_eq!(
        exits,
        ["state_exited:leg_a", "state_exited:patrol"],
        "child before parent, once each"
    );
    let telemetry = state_trees(&runtime);
    assert_eq!(telemetry["agents"][0]["enabled"], false);
    assert_eq!(telemetry["agents"][0]["active_path"], json!([]));
    assert!(telemetry["agents"][0]["phase"].is_null());
    let nav = runtime.app().world().get::<NavigationAgent>(guard).unwrap();
    assert!(nav.target.is_none() && nav.navmesh.is_none());
}

#[test]
fn a_finished_tree_reports_once_and_stops_ticking() {
    let graph = Tree::new()
        .state("only", "root", json!({}))
        .task("only", "w", "wait", json!({"seconds":0.25}))
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity("g", json!({"graph":"g"}), false)],
    );
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 10, 0.1);
    let events = log(&runtime);
    let finished: Vec<_> = events
        .iter()
        .filter(|(n, _)| n == "state_tree.finished")
        .collect();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].1["status"], "success");
    assert_eq!(finished[0].1["graph"], "g");
    assert_eq!(
        flow(&events),
        [
            "state_entered:only",
            "state_exited:only",
            "state_tree.finished"
        ]
    );
    let telemetry = state_trees(&runtime);
    assert_eq!(
        telemetry["agents"][0]["phase"],
        json!({"finished":"success"})
    );
    assert_eq!(telemetry["agents"][0]["last_finished"], "success");
    assert_eq!(
        telemetry["agents"][0]["ticks"], 4,
        "ticks stop with the tree"
    );
}

struct Ticker;
impl NativeTask<World> for Ticker {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("evaluator.start".into(), Value::Null));
        cx.blackboard.set_json("ticks", &json!(1)).unwrap();
        TaskStatus::Running
    }
    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        let ticks = match cx.blackboard.get("ticks") {
            Some(engine_project::ai::BbValue::Int(n)) => *n,
            _ => 0,
        };
        cx.blackboard.set_json("ticks", &json!(ticks + 1)).unwrap();
        TaskStatus::Running
    }
    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("evaluator.stop".into(), Value::Null));
    }
}
fn ticker(_: &mut World, _: u64) -> std::result::Result<Box<dyn NativeTask<World>>, String> {
    Ok(Box::new(Ticker))
}

struct Mark;
impl NativeTask<World> for Mark {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("task.start".into(), cx.params.clone()));
        TaskStatus::Running
    }
    fn tick(&mut self, _: &mut TaskContext<'_, World>) -> TaskStatus {
        TaskStatus::Running
    }
    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        cx.world
            .resource_mut::<Log>()
            .0
            .push(("task.abort".into(), Value::Null));
    }
}
fn mark(_: &mut World, _: u64) -> std::result::Result<Box<dyn NativeTask<World>>, String> {
    Ok(Box::new(Mark))
}

#[test]
fn native_evaluators_and_tasks_run_against_the_world_and_feed_transitions() {
    let graph = Tree::new()
        .var("ticks", "int", Value::Null)
        .state("count", "root", json!({}))
        .go(
            "count",
            "enough",
            json!({"trigger":"on_tick","target":"state","state":"done"}),
        )
        .cond("enough", "enough_c", "ticks", "ge", json!(3))
        .state("done", "root", json!({}))
        .task(
            "done",
            "mark",
            "native_task",
            json!({"name":"mark","params":{"n":7}}),
        )
        .eval("ev", "native_evaluator", json!({"name":"ticker"}))
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity("g", json!({"graph":"g"}), false)],
    );
    let mut runtime = runtime(&dir);
    register_state_tree_evaluator(runtime.app_mut(), "ticker", ticker).unwrap();
    register_state_tree_task(runtime.app_mut(), "mark", mark).unwrap();
    assert!(
        register_state_tree_task(runtime.app_mut(), "mark", mark).is_err(),
        "task names are unique"
    );
    assert!(
        register_state_tree_evaluator(runtime.app_mut(), "ticker", ticker).is_err(),
        "evaluator names are unique"
    );
    // Tasks and evaluators have separate namespaces.
    register_state_tree_evaluator(runtime.app_mut(), "mark", mark).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime, 3, 0.1);
    let names = names(&runtime);
    // start sets ticks to 1, two ticks later it is 3 and the transition fires
    // within the same tick; the task starts in that tick.
    assert_eq!(
        names
            .iter()
            .filter(|n| n.starts_with("evaluator.") || n.starts_with("task."))
            .collect::<Vec<_>>(),
        ["evaluator.start", "task.start"],
        "{names:?}"
    );
    let events = log(&runtime);
    let started = events.iter().find(|(n, _)| n == "task.start").unwrap();
    assert_eq!(started.1, json!({"n":7}));
    let telemetry = state_trees(&runtime);
    assert_eq!(telemetry["agents"][0]["blackboard"]["ticks"], 3);
    assert_eq!(telemetry["agents"][0]["active_path"], json!(["done"]));
    runtime.stop().unwrap();
    let names = self::names(&runtime);
    let tail: Vec<_> = names.iter().rev().take(2).collect();
    assert!(
        tail.contains(&&"task.abort".to_owned()) && tail.contains(&&"evaluator.stop".to_owned()),
        "stopping aborts the running task and stops the evaluator: {names:?}"
    );
}

#[test]
fn an_unregistered_native_name_fails_start() {
    let task = Tree::new()
        .state("s", "root", json!({}))
        .task("s", "n", "native_task", json!({"name":"missing"}))
        .json();
    let dir = project(
        &[("g", task)],
        vec![agent_entity("g", json!({"graph":"g"}), false)],
    );
    let error = runtime(&dir).start().unwrap_err();
    assert_eq!(error.diagnostics[0].code, "state_tree.native.unregistered");
    assert!(
        error.diagnostics[0]
            .message
            .contains("register_state_tree_task")
    );
    let evaluator = Tree::new()
        .state("s", "root", json!({}))
        .eval("e", "native_evaluator", json!({"name":"missing"}))
        .json();
    let dir = project(
        &[("g", evaluator)],
        vec![agent_entity("g", json!({"graph":"g"}), false)],
    );
    let mut runtime = runtime(&dir);
    // A task of that name does not satisfy an evaluator.
    register_state_tree_task(runtime.app_mut(), "missing", mark).unwrap();
    let error = runtime.start().unwrap_err();
    assert_eq!(error.diagnostics[0].code, "state_tree.native.unregistered");
    assert!(
        error.diagnostics[0]
            .message
            .contains("register_state_tree_evaluator")
    );
}

#[test]
fn host_inputs_feed_evaluators_and_transitions_in_the_same_tick() {
    // Walk toward B; as soon as the guard is within 3 m (evaluators run before
    // transitions), the state changes, long before the move arrives.
    let graph = Tree::new()
        .var("pos", "vec3", Value::Null)
        .var("goal", "vec3", json!([8.0, 0.0, 1.0]))
        .var("dist", "float", Value::Null)
        .var("close", "float", json!(3.0))
        .state("walk", "root", json!({}))
        .task("walk", "mv", "move_to", json!({"key":"goal"}))
        .go(
            "walk",
            "near",
            json!({"trigger":"on_tick","target":"state","state":"arrived"}),
        )
        .cond("near", "near_c", "dist", "lt", json!(3.0))
        .state("arrived", "root", json!({}))
        .eval(
            "e_pos",
            "copy_evaluator",
            json!({"input":"self.position","key":"pos"}),
        )
        .eval(
            "e_dist",
            "compute_evaluator",
            json!({"op":"distance","a":"pos","b":"goal","key":"dist"}),
        )
        .json();
    let dir = project(
        &[("g", graph)],
        vec![agent_entity(
            "g",
            json!({"graph":"g","navmesh":"nav"}),
            true,
        )],
    );
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    let guard = runtime.entity("g").unwrap();
    let mut switched_at = None;
    for t in 1..=100 {
        tick(&mut runtime, 1, 0.1);
        let path = state_trees(&runtime)["agents"][0]["active_path"].clone();
        if path == json!(["arrived"]) {
            switched_at = Some(t);
            break;
        }
    }
    let t = switched_at.expect("the guard gets within 3 m");
    let x = world_transform(runtime.app().world(), guard)
        .unwrap()
        .position()[0];
    assert!((4.5..=6.0).contains(&x), "switched at x={x}, tick {t}");
    assert!(
        !names(&runtime).contains(&"navigation.arrived".to_owned()),
        "before the move finished"
    );
    let telemetry = state_trees(&runtime);
    assert_eq!(
        telemetry["agents"][0]["blackboard"]["goal"],
        json!([8.0, 0.0, 1.0])
    );
    assert!(
        telemetry["agents"][0]["blackboard"]["dist"]
            .as_f64()
            .unwrap()
            < 3.0
    );
}

#[test]
fn state_events_are_bounded_per_agent_and_per_tick() {
    // Two states that ping-pong 8 times per tick change 16+ states per tick.
    let graph = Tree::new()
        .state("a", "root", json!({}))
        .go(
            "a",
            "ab",
            json!({"trigger":"on_tick","target":"state","state":"b"}),
        )
        .state("b", "root", json!({}))
        .go(
            "b",
            "ba",
            json!({"trigger":"on_tick","target":"state","state":"a"}),
        )
        .json();
    let entities: Vec<Value> = (0..20)
        .map(|i| agent_entity(&format!("a{i:02}"), json!({"graph":"spin"}), false))
        .collect();
    let dir = project(&[("spin", graph)], entities);
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    let mut previous = 0;
    for _ in 0..30 {
        tick(&mut runtime, 1, 0.1);
        let total = names(&runtime).len();
        assert!(
            total - previous <= 64,
            "at most 64 state tree events per tick, got {}",
            total - previous
        );
        previous = total;
    }
    assert!(previous > 0);
    let telemetry = state_trees(&runtime);
    assert!(
        telemetry["dropped_events"].as_u64().unwrap() > 0,
        "overflow is counted"
    );
    assert_eq!(telemetry["total"], 20);
    for agent in telemetry["agents"].as_array().unwrap() {
        assert!(agent["transitions"].as_array().unwrap().len() <= 8);
    }
}

#[test]
fn tick_interval_and_blackboard_overrides_apply() {
    let graph = Tree::new()
        .var("alert", "bool", json!(false))
        .state("calm", "root", json!({}))
        .cond("calm", "calm_c", "alert", "eq", json!(false))
        .state("tense", "root", json!({}))
        .cond("tense", "tense_c", "alert", "eq", json!(true))
        .json();
    for (alert, expected) in [(true, "tense"), (false, "calm")] {
        let dir = project(
            &[("g", graph.clone())],
            vec![agent_entity(
                "g",
                json!({"graph":"g","blackboard":{"alert":alert},"tick_interval":0.5}),
                false,
            )],
        );
        let mut runtime = runtime(&dir);
        runtime.start().unwrap();
        tick(&mut runtime, 20, 0.1);
        let telemetry = state_trees(&runtime);
        assert_eq!(telemetry["agents"][0]["active_path"], json!([expected]));
        assert_eq!(
            telemetry["agents"][0]["ticks"], 4,
            "0.5 s interval at 0.1 s ticks: every fifth"
        );
    }
}

#[test]
fn behavior_trees_and_state_trees_run_side_by_side() {
    let state = Tree::new().state("idle", "root", json!({})).json();
    let behavior = json!({"version":1,"kind":"behavior_tree",
        "nodes":[{"id":"root","node_type":"root"},{"id":"w","node_type":"wait","params":{"seconds":100.0}}],
        "edges":[{"from":{"node":"root","port":"child"},"to":{"node":"w","port":"parent"}}]});
    let mut entity = agent_entity("both", json!({"graph":"st"}), false);
    entity["components"]["custom"]["engine.BehaviorTreeAgent"] =
        json!({"version":1,"data":{"graph":"bt"}});
    let dir = project(&[("st", state), ("bt", behavior)], vec![entity]);
    let mut runtime = runtime(&dir);
    runtime.start().unwrap();
    tick(&mut runtime, 3, 0.1);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert_eq!(snapshot["state_trees"]["agents"][0]["graph"], "st");
    assert_eq!(snapshot["behavior_trees"]["agents"][0]["graph"], "bt");
}

// -------------------------------------------------------------- validation

#[test]
fn project_validation_rejects_bad_state_tree_references() {
    let load = |graph: Option<Value>, agent: Value, nav: bool| {
        let graphs: Vec<(&str, Value)> = graph.into_iter().map(|g| ("g", g)).collect();
        let dir = project(&graphs, vec![agent_entity("e", agent, nav)]);
        Project::load(dir.path().join("project.json"))
            .map(|_| ())
            .unwrap_err()
    };
    let idle = || Tree::new().state("s", "root", json!({})).json();
    assert_eq!(
        load(None, json!({"graph":"g"}), false).diagnostics[0].code,
        "project.asset.unknown"
    );
    assert_eq!(
        load(Some(idle()), json!({}), false).diagnostics[0].code,
        "component.schema.invalid",
        "graph is required by the schema"
    );
    assert_eq!(
        load(Some(idle()), json!({"graph":""}), false).diagnostics[0].code,
        "state_tree.graph.missing"
    );
    // A tree with a bad transition target is a graph problem.
    let broken = Tree::new()
        .state("s", "root", json!({}))
        .go(
            "s",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"ghost"}),
        )
        .json();
    assert_eq!(
        load(Some(broken), json!({"graph":"g"}), false).diagnostics[0].code,
        "st.transition.target_unknown"
    );
    // A behavior tree cannot run as a state tree.
    let behavior = json!({"version":1,"kind":"behavior_tree",
        "nodes":[{"id":"root","node_type":"root"},{"id":"w","node_type":"wait"}],
        "edges":[{"from":{"node":"root","port":"child"},"to":{"node":"w","port":"parent"}}]});
    assert_eq!(
        load(Some(behavior), json!({"graph":"g"}), false).diagnostics[0].code,
        "state_tree.graph.kind"
    );
    let typed = Tree::new()
        .var("hp", "int", Value::Null)
        .state("s", "root", json!({}))
        .json();
    for overrides in [json!({"hp":"x"}), json!({"nope":1})] {
        assert_eq!(
            load(
                Some(typed.clone()),
                json!({"graph":"g","blackboard":overrides}),
                false
            )
            .diagnostics[0]
                .code,
            "state_tree.blackboard.invalid"
        );
    }
    // Unknown host input names are caught before the game runs.
    let input = Tree::new()
        .var("v", "vec3", Value::Null)
        .state("s", "root", json!({}))
        .eval(
            "e",
            "copy_evaluator",
            json!({"input":"self.nowhere","key":"v"}),
        )
        .json();
    assert_eq!(
        load(Some(input), json!({"graph":"g"}), false).diagnostics[0].code,
        "state_tree.input.unknown"
    );
    // move_to needs a NavigationAgent and a navmesh.
    let moving = Tree::new()
        .state("s", "root", json!({}))
        .task("s", "m", "move_to", json!({"target":[1,0,1]}))
        .json();
    assert_eq!(
        load(
            Some(moving.clone()),
            json!({"graph":"g","navmesh":"nav"}),
            false
        )
        .diagnostics[0]
            .code,
        "state_tree.navigation.missing"
    );
    assert_eq!(
        load(Some(moving.clone()), json!({"graph":"g"}), true).diagnostics[0].code,
        "state_tree.navigation.missing"
    );
    assert_eq!(
        load(Some(moving), json!({"graph":"g","navmesh":"g"}), true).diagnostics[0].code,
        "project.asset.kind"
    );
    assert_eq!(
        load(Some(idle()), json!({"graph":"g","bogus":1}), false).diagnostics[0].code,
        "component.schema.invalid"
    );
    assert_eq!(
        load(Some(idle()), json!({"graph":"g","tick_interval":-1}), false).diagnostics[0].code,
        "component.schema.invalid"
    );
}

#[test]
fn a_valid_project_loads_and_projects_without_agents_have_no_telemetry() {
    let dir = guard_dir();
    Project::load(dir.path().join("project.json")).unwrap();
    let plain = project(
        &[],
        vec![json!({"id":"e","name":"e","components":{"Transform":{"position":[0,0,0]}}})],
    );
    let mut runtime = runtime(&plain);
    runtime.start().unwrap();
    tick(&mut runtime, 1, 0.1);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert!(snapshot.get("state_trees").is_none());
}

#[test]
fn the_component_is_listed_with_a_schema_and_defaults() {
    let dir = guard_dir();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let descriptor = &project.component_catalog.0["engine.StateTreeAgent"];
    assert_eq!(descriptor.version, 1);
    assert_eq!(descriptor.defaults["enabled"], true);
    assert_eq!(descriptor.defaults["tick_interval"], 0.0);
    assert_eq!(descriptor.schema["required"], json!(["graph"]));
    // The graph kind is registered next to behavior_tree.
    let kinds: Vec<&str> = engine_project::graphs::kinds().names().collect();
    assert!(
        kinds.contains(&"behavior_tree")
            && kinds.contains(&"state_tree")
            && kinds.contains(&"animation")
    );
    assert!(kinds.windows(2).all(|pair| pair[0] < pair[1]));
}
