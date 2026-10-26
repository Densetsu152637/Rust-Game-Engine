use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::ai::env_query::{
    Poll, QueryHandle, cancel_query, run_query, submit_query, take_query,
};
use engine_project::simulation::{BakeInput, BakeSettings, NavMeshDocument, Obstacle};
use engine_project::*;
use serde_json::{Value, json};
use std::fs;

fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}

fn component(name: &str, data: Value) -> Value {
    json!({ name: {"version":1,"data":data}})
}

fn merged(parts: Vec<Value>) -> Value {
    let mut out = serde_json::Map::new();
    for part in parts {
        out.extend(part.as_object().unwrap().clone());
    }
    Value::Object(out)
}

fn queries(list: Value) -> Value {
    entity(
        "ai",
        [0.0; 3],
        component("engine.EnvironmentQueries", json!({"queries": list})),
    )
}

/// A guard at (1, 0, 1) with a navigation agent (no navmesh of its own).
fn guard(extra: Vec<Value>) -> Value {
    let mut parts = vec![component("engine.NavigationAgent", json!({"speed":4}))];
    parts.extend(extra);
    entity("guard", [1.0, 0.0, 1.0], merged(parts))
}

fn physics() -> Value {
    entity(
        "physics",
        [0.0; 3],
        component(
            "engine.PhysicsSettings",
            json!({"gravity_3d":[0.,0.,0.],"layers":["world"]}),
        ),
    )
}

fn wall() -> Value {
    entity(
        "wall",
        [5.0, 0.5, 2.0],
        component(
            "engine.PhysicsBody3D",
            json!({"motion":"fixed","half_extents":[0.25,1.5,3.0],"material":{"layer_name":"world"}}),
        ),
    )
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
        json!({"schema":1,"name":"queries","startup_scene":"scene.json","assets":assets})
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

fn started(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    runtime
}

fn tick(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.tick(0.1, InputSnapshot::default()).unwrap();
    }
}

fn status(runtime: &Runtime) -> Value {
    serde_json::to_value(runtime.try_snapshot().unwrap().env_queries).unwrap()
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
}

fn error_code(error: Error) -> String {
    error.diagnostics[0].code.clone()
}

#[test]
fn queries_project_onto_the_navmesh_and_test_physics_line_of_sight() {
    let list = json!([
        {"name":"ring","navmesh":"nav","project_to_navmesh":true,"snap_distance":0.2,
         "generator":{"type":"circle","center":[5,0,5],"radius":1.5,"count":8},
         "select":{"top_n":16}},
        {"name":"west","generator":{"type":"circle","center":[3,0,2],"radius":0.5,"count":4},
         "tests":[{"type":"line_of_sight","from_height":0.5,"to_height":0.5}],
         "select":{"top_n":16}},
        {"name":"east","generator":{"type":"circle","center":[8,0,2],"radius":0.5,"count":4},
         "tests":[{"type":"line_of_sight","from_height":0.5,"to_height":0.5}],
         "select":{"top_n":16}},
        {"name":"hidden","generator":{"type":"circle","center":[8,0,2],"radius":0.5,"count":4},
         "tests":[{"type":"line_of_sight","from_height":0.5,"to_height":0.5,"invert":true}],
         "select":{"top_n":16}},
        {"name":"near","navmesh":"nav",
         "generator":{"type":"circle","radius":4,"count":8,"rings":2},
         "tests":[{"type":"reachable"},
                  {"type":"path_length","max":8,"score":"lower_better"}],
         "select":{"top_n":3}}
    ]);
    let dir = project(&[], vec![physics(), wall(), queries(list), guard(vec![])]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 1);
    let guard = runtime.entity("guard").unwrap();
    let w = runtime.app_mut().world_mut();

    // Points inside the obstacle footprint cannot be snapped and are dropped.
    let ring = run_query(w, guard, "ring", None).unwrap();
    assert_eq!(ring.generated, 8);
    assert!(
        ring.rejected >= 2,
        "the points inside the obstacle are dropped: {}",
        ring.rejected
    );
    assert_eq!(ring.items.len() + ring.rejected, 8);
    assert!(ring.items.iter().all(|s| s.item.position[1].abs() < 0.05));

    // The wall at x = 5 blocks the guard's view of the east side only.
    assert_eq!(run_query(w, guard, "west", None).unwrap().items.len(), 4);
    assert_eq!(run_query(w, guard, "east", None).unwrap().items.len(), 0);
    assert_eq!(run_query(w, guard, "hidden", None).unwrap().items.len(), 4);

    // Reachable points ranked by path length, shortest first.
    let near_points = run_query(w, guard, "near", None).unwrap();
    assert_eq!(near_points.items.len(), 3);
    let scores: Vec<f32> = near_points.items.iter().map(|s| s.score).collect();
    assert!(scores.windows(2).all(|p| p[0] >= p[1]), "{scores:?}");

    // The telemetry lists the most recent queries, newest last.
    tick(&mut runtime, 1);
    let status = status(&runtime);
    assert_eq!(status["templates"], 5);
    assert_eq!(status["completed"], 5);
    let recent = status["recent"].as_array().unwrap();
    assert_eq!(recent.last().unwrap()["query"], "near");
    assert_eq!(recent.last().unwrap()["querier"], "guard");
}

#[test]
fn a_query_that_cannot_run_fails_with_a_reason() {
    let list = json!([
        {"name":"needs_mesh","project_to_navmesh":true,
         "generator":{"type":"circle","radius":2,"count":4}},
        {"name":"needs_target","generator":{"type":"circle","center":"target","radius":2,"count":4}},
        {"name":"needs_key","generator":{"type":"circle","center":{"key":"nope"},"radius":2,"count":4}}
    ]);
    let dir = project(&[], vec![queries(list), guard(vec![])]);
    let mut runtime = started(&dir);
    let guard = runtime.entity("guard").unwrap();
    let w = runtime.app_mut().world_mut();
    assert_eq!(
        error_code(run_query(w, guard, "missing", None).unwrap_err()),
        "env_query.unknown"
    );
    assert_eq!(
        error_code(run_query(w, guard, "needs_mesh", None).unwrap_err()),
        "env_query.navmesh.missing"
    );
    let no_target = run_query(w, guard, "needs_target", None).unwrap_err();
    assert_eq!(error_code(no_target), "env_query.start");
    assert!(run_query(w, guard, "needs_target", Some([0.0; 3])).is_ok());
    assert_eq!(
        error_code(run_query(w, guard, "needs_key", None).unwrap_err()),
        "env_query.start"
    );
}

#[test]
fn a_big_query_spreads_over_ticks_and_can_be_cancelled() {
    let list = json!([
        {"name":"big","navmesh":"nav","project_to_navmesh":true,
         "generator":{"type":"grid","center":[5,0,5],"half_extent":10,"spacing":1},
         "tests":[{"type":"distance","to":[5,0,5],"score":"lower_better"}],
         "select":{"top_n":2}}
    ]);
    let dir = project(&[], vec![queries(list), guard(vec![])]);
    let mut runtime = started(&dir);
    let guard = runtime.entity("guard").unwrap();
    let handle = submit_query(runtime.app_mut().world_mut(), guard, "big", None).unwrap();
    // 441 projections cost far more than the 256 units a new query spends at once.
    assert_eq!(
        take_query(runtime.app_mut().world_mut(), handle),
        Poll::Pending
    );
    assert_eq!(status(&runtime)["running"], 1);
    let mut result = None;
    for _ in 0..5 {
        tick(&mut runtime, 1);
        if let Poll::Ready(r) = take_query(runtime.app_mut().world_mut(), handle) {
            result = Some(r);
            break;
        }
    }
    let result = result.expect("finishes within a few ticks");
    assert!(result.steps >= 2, "{} steps", result.steps);
    assert_eq!(result.generated, 441);
    assert_eq!(result.items.len(), 2);
    assert_eq!(
        take_query(runtime.app_mut().world_mut(), handle),
        Poll::Unknown,
        "a result is taken once"
    );
    // Same result as the synchronous run.
    let again = run_query(runtime.app_mut().world_mut(), guard, "big", None).unwrap();
    assert_eq!(again.items, result.items);
    // Cancelling drops a running query.
    let h2 = submit_query(runtime.app_mut().world_mut(), guard, "big", None).unwrap();
    cancel_query(runtime.app_mut().world_mut(), h2);
    assert_eq!(take_query(runtime.app_mut().world_mut(), h2), Poll::Unknown);
    assert_eq!(
        take_query(runtime.app_mut().world_mut(), QueryHandle(9999)),
        Poll::Unknown
    );
}

fn seek_graph() -> Value {
    let node = |id: &str, ty: &str, params: Value| json!({"id":id,"node_type":ty,"params":params});
    let edge = |from: &str, port: &str, to: &str, to_port: &str| json!({"from":{"node":from,"port":port},"to":{"node":to,"port":to_port}});
    json!({"version":1,"kind":"state_tree",
        "variables":[
            {"name":"threat","type":"vec3","default":[8.0,0.0,8.0]},
            {"name":"cover","type":"vec3"},
            {"name":"who","type":"entity"}],
        "nodes":[
            {"id":"root","node_type":"root"},
            node("look","state",json!({})),
            node("pick","native_task",json!({"name":"engine.run_query",
                "params":{"query":"cover_pick","target_key":"threat","position_key":"cover"}})),
            node("found","transition",json!({"trigger":"on_succeeded","target":"state","state":"hide"})),
            node("lost","transition",json!({"trigger":"on_failed","target":"state","state":"stuck"})),
            node("hide","state",json!({})),
            node("stuck","state",json!({}))],
        "edges":[
            edge("root","children","look","parent"),
            edge("look","tasks","pick","owner"),
            edge("look","transitions","found","owner"),
            edge("look","transitions","lost","owner"),
            edge("root","children","hide","parent"),
            edge("root","children","stuck","parent")]})
}

fn cover_templates() -> Value {
    json!([
        {"name":"cover_pick","navmesh":"nav","project_to_navmesh":true,"snap_distance":0.5,
         "generator":{"type":"circle","center":"target","radius":3,"rings":2,"count":8},
         "tests":[{"type":"reachable"},
                  {"type":"distance","to":"target","score":"higher_better","weight":2},
                  {"type":"path_length","score":"lower_better"}],
         "select":{"top_n":1}},
        {"name":"empty","generator":{"type":"circle","radius":2,"count":4},
         "tests":[{"type":"distance","max":0.5}]}
    ])
}

#[test]
fn the_run_query_task_feeds_a_state_tree_blackboard() {
    let dir = project(
        &[("seek", seek_graph())],
        vec![
            queries(cover_templates()),
            guard(vec![component(
                "engine.StateTreeAgent",
                json!({"graph":"seek"}),
            )]),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 3);
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    assert_eq!(
        trees["agents"][0]["active_path"],
        json!(["hide"]),
        "{trees}"
    );
    let cover = &trees["agents"][0]["blackboard"]["cover"];
    // The same query run natively with the same target gives the same answer.
    let guard = runtime.entity("guard").unwrap();
    let native = run_query(
        runtime.app_mut().world_mut(),
        guard,
        "cover_pick",
        Some([8.0, 0.0, 8.0]),
    )
    .unwrap();
    let expected = native.items[0].item.position;
    let found: Vec<f64> = cover
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(
        near(
            [found[0] as f32, found[1] as f32, found[2] as f32],
            expected
        ),
        "{found:?} vs {expected:?}"
    );
}

#[test]
fn the_run_query_task_fails_when_nothing_qualifies_and_works_in_behavior_trees() {
    // No point lies within 0.5 of the querier on a radius-2 circle: the task fails and
    // the state tree takes the failure branch.
    let mut graph = seek_graph();
    graph["nodes"][2]["params"]["params"]["query"] = json!("empty");
    graph["nodes"][2]["params"]["params"]
        .as_object_mut()
        .unwrap()
        .remove("target_key");
    let dir = project(
        &[("seek", graph)],
        vec![
            queries(cover_templates()),
            guard(vec![component(
                "engine.StateTreeAgent",
                json!({"graph":"seek"}),
            )]),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 3);
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    assert_eq!(trees["agents"][0]["active_path"], json!(["stuck"]));

    // A behavior tree runs the same task through the same reserved name.
    let behavior = json!({"version":1,"kind":"behavior_tree",
        "variables":[{"name":"cover","type":"vec3"}],
        "nodes":[
            {"id":"root","node_type":"root"},
            {"id":"seq","node_type":"sequence"},
            {"id":"pick","node_type":"native_task","params":{"name":"engine.run_query",
                "params":{"query":"cover_pick","position_key":"cover"}}},
            {"id":"say","node_type":"emit_event","params":{"event":"cover.found"}}],
        "edges":[
            {"from":{"node":"root","port":"child"},"to":{"node":"seq","port":"parent"}},
            {"from":{"node":"seq","port":"children"},"to":{"node":"pick","port":"parent"}},
            {"from":{"node":"seq","port":"children"},"to":{"node":"say","port":"parent"}}]});
    // This query needs a target; give the template a literal center instead.
    let templates = json!([{"name":"cover_pick","navmesh":"nav","project_to_navmesh":true,
        "generator":{"type":"circle","center":[8,0,8],"radius":2,"count":8},
        "tests":[{"type":"distance","to":[8,0,8],"score":"higher_better"}]}]);
    let mut e = guard(vec![component(
        "engine.BehaviorTreeAgent",
        json!({"graph":"seek_bt"}),
    )]);
    e["components"]["EventBindings"] =
        json!({"bindings":[{"event":"cover.found","handler":"record"}]});
    let dir = project(&[("seek_bt", behavior)], vec![queries(templates), e]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 3);
    let log = &runtime.app().world().resource::<Log>().0;
    assert!(log.iter().any(|(n, _)| n == "cover.found"), "{log:?}");
    let bt = serde_json::to_value(runtime.try_snapshot().unwrap().behavior_trees).unwrap();
    assert!(bt["agents"][0]["blackboard"]["cover"].is_array(), "{bt}");
}

#[test]
fn perceived_actors_are_a_generator() {
    let sensor = component(
        "engine.PerceptionSensor",
        json!({"team":1,"sight":{"range":20,"lose_range":24},"forward":[1,0,0]}),
    );
    let stimulus = |team| component("engine.PerceptionStimulus", json!({"team":team}));
    let list = json!([{"name":"threats","generator":{"type":"actors","source":"perceived",
        "affiliations":["hostile"]},
        "tests":[{"type":"distance","score":"lower_better"}]},
        {"name":"all","generator":{"type":"actors","source":"stimuli"},"select":{"top_n":16}}]);
    let dir = project(
        &[],
        vec![
            queries(list),
            guard(vec![sensor]),
            entity("near", [4.0, 0.0, 1.0], stimulus(2)),
            entity("far", [9.0, 0.0, 1.0], stimulus(2)),
            entity("ally", [3.0, 0.0, 1.0], stimulus(1)),
            entity("behind", [-5.0, 0.0, 1.0], stimulus(2)),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 2);
    let guard = runtime.entity("guard").unwrap();
    let near_entity = runtime.entity("near").unwrap();
    let w = runtime.app_mut().world_mut();
    let threats = run_query(w, guard, "threats", None).unwrap();
    assert_eq!(
        threats.generated, 2,
        "near and far are seen; ally is friendly, behind unseen"
    );
    assert_eq!(threats.items[0].item.subject, Some(near_entity.to_bits()));
    assert!(near(threats.items[0].item.position, [4.0, 0.0, 1.0]));
    // Stimuli lists everyone with a stimulus except the querier, ordered by entity.
    let all = run_query(w, guard, "all", None).unwrap();
    assert_eq!(all.generated, 4);
}

#[test]
fn templates_are_validated_when_the_project_loads() {
    let load = |entities: Vec<Value>| {
        let dir = project(&[], entities);
        Project::load(dir.path().join("project.json"))
            .map(|_| ())
            .unwrap_err()
    };
    let circle = json!({"type":"circle","radius":2});
    let one = |q: Value| queries(json!([q]));
    assert_eq!(
        load(vec![one(json!({"name":"bad name","generator":circle}))]).diagnostics[0].code,
        "env_query.value.invalid"
    );
    assert_eq!(
        load(vec![one(
            json!({"name":"q","generator":circle,"tests":[{"type":"distance"}]})
        )])
        .diagnostics[0]
            .code,
        "env_query.value.invalid"
    );
    assert_eq!(
        load(vec![
            one(json!({"name":"q","generator":circle})),
            entity(
                "other",
                [0.0; 3],
                component(
                    "engine.EnvironmentQueries",
                    json!({"queries":[{"name":"q","generator":circle}]})
                )
            )
        ])
        .diagnostics[0]
            .code,
        "env_query.duplicate",
        "names are unique across the scene"
    );
    assert_eq!(
        load(vec![one(
            json!({"name":"q","generator":circle,"navmesh":"ghost"})
        )])
        .diagnostics[0]
            .code,
        "project.asset.unknown"
    );
    // Unknown fields and generator types do not parse.
    assert!(
        !load(vec![one(json!({"name":"q","generator":circle,"bogus":1}))])
            .diagnostics
            .is_empty()
    );
    assert!(
        !load(vec![one(json!({"name":"q","generator":{"type":"blob"}}))])
            .diagnostics
            .is_empty()
    );
    let many: Vec<Value> = (0..33)
        .map(|i| json!({"name":format!("q{i}"),"generator":circle}))
        .collect();
    assert_eq!(
        load(vec![queries(Value::Array(many))]).diagnostics[0].code,
        "component.schema.invalid"
    );
}

fn trace() -> (Value, Vec<(String, Value)>) {
    let dir = project(
        &[("seek", seek_graph())],
        vec![
            queries(cover_templates()),
            guard(vec![component(
                "engine.StateTreeAgent",
                json!({"graph":"seek"}),
            )]),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 4);
    let guard = runtime.entity("guard").unwrap();
    run_query(
        runtime.app_mut().world_mut(),
        guard,
        "cover_pick",
        Some([2.0, 0.0, 9.0]),
    )
    .unwrap();
    (
        status(&runtime),
        runtime.app().world().resource::<Log>().0.clone(),
    )
}

#[test]
fn two_runs_give_identical_results_and_telemetry() {
    let (first, log) = trace();
    let (second, log2) = trace();
    assert_eq!(first, second);
    assert_eq!(log, log2);
    assert_eq!(first["completed"], 2);
}

#[test]
fn the_component_is_listed_with_a_schema_and_defaults() {
    let dir = project(&[], vec![guard(vec![])]);
    let loaded = Project::load(dir.path().join("project.json")).unwrap();
    let d = &loaded.component_catalog.0["engine.EnvironmentQueries"];
    assert_eq!(d.version, 1);
    assert_eq!(d.defaults["queries"], json!([]));
    let plain = project(&[], vec![guard(vec![])]);
    let mut runtime = started(&plain);
    tick(&mut runtime, 1);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert!(snapshot.get("env_queries").is_none());
    assert!(snapshot.get("smart_objects").is_none());
}
