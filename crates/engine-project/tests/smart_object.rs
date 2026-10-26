use bevy_ecs::prelude::*;
use engine_ai::smart_object::{ReleaseReason, SlotError, SlotRef, SlotState};
use engine_project::Result;
use engine_project::ai::{
    env_query::run_query,
    smart_object::{claim_slot, find_slots, occupy_slot, release_slot, slot_info},
};
use engine_project::simulation::{BakeInput, BakeSettings, NavMeshDocument, Obstacle};
use engine_project::*;
use serde_json::{Value, json};
use std::fs;

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

fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}

/// A bench at `at` with two seats (`left` -0.6 and `right` +0.6 in x) and a work spot.
fn bench(at: [f32; 3], timeout: f32) -> Value {
    let mut e = entity(
        "bench",
        at,
        component(
            "engine.SmartObject",
            json!({"claim_timeout":timeout,"slots":[
                {"name":"left","offset":[-0.6,0,0],"tags":["sit","bench"],"behavior":"sit_down"},
                {"name":"right","offset":[0.6,0,0],"tags":["sit","bench"],"behavior":"sit_down"},
                {"name":"desk","offset":[0,0,1.5],"tags":["work"],"behavior":"type"}]}),
        ),
    );
    e["components"]["EventBindings"] = json!({"bindings":[
        {"event":"smart_object.claimed","handler":"record"},
        {"event":"smart_object.activated","handler":"record"},
        {"event":"smart_object.released","handler":"record"}]});
    e
}

/// A state tree: use a smart object, then `done`; `denied` when that fails; `idle` on
/// the `stop` event.
fn use_graph(params: Value) -> Value {
    let node = |id: &str, ty: &str, p: Value| json!({"id":id,"node_type":ty,"params":p});
    let edge = |from: &str, port: &str, to: &str, to_port: &str| json!({"from":{"node":from,"port":port},"to":{"node":to,"port":to_port}});
    json!({"version":1,"kind":"state_tree",
        "variables":[{"name":"how","type":"string"},{"name":"spot","type":"vec3"}],
        "nodes":[
            {"id":"root","node_type":"root"},
            node("use","state",json!({})),
            node("task","native_task",json!({"name":"engine.use_smart_object","params":params})),
            node("ok","transition",json!({"trigger":"on_succeeded","target":"state","state":"done"})),
            node("ko","transition",json!({"trigger":"on_failed","target":"state","state":"denied"})),
            node("halt","transition",json!({"trigger":"on_event","event":"stop","target":"state","state":"idle"})),
            node("done","state",json!({})),
            node("denied","state",json!({})),
            node("idle","state",json!({}))],
        "edges":[
            edge("root","children","use","parent"),
            edge("use","tasks","task","owner"),
            edge("use","transitions","ok","owner"),
            edge("use","transitions","ko","owner"),
            edge("use","transitions","halt","owner"),
            edge("root","children","done","parent"),
            edge("root","children","denied","parent"),
            edge("root","children","idle","parent")]})
}

fn user(id: &str, at: [f32; 3], nav: bool) -> Value {
    let mut parts = vec![component("engine.StateTreeAgent", json!({"graph":"use"}))];
    if nav {
        parts.push(component("engine.NavigationAgent", json!({"speed":4})));
    }
    let mut e = entity(id, at, merged(parts));
    e["components"]["EventBindings"] = json!({"bindings":[{"event":"stop","handler":"record"}]});
    e
}

fn project(graphs: &[(&str, Value)], entities: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let document = NavMeshDocument {
        version: 1,
        input: BakeInput {
            vertices: vec![[0., 0., 0.], [10., 0., 0.], [10., 0., 10.], [0., 0., 10.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            obstacles: vec![Obstacle {
                min: [4., -1., 6.],
                max: [6., 3., 8.],
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
        json!({"schema":1,"name":"smart","startup_scene":"scene.json","assets":assets}).to_string(),
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

fn log(runtime: &Runtime) -> Vec<(String, Value)> {
    runtime.app().world().resource::<Log>().0.clone()
}

fn paths(runtime: &Runtime) -> Vec<(String, Value)> {
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    trees["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["entity"].as_str().unwrap().to_owned(),
                a["active_path"][0].clone(),
            )
        })
        .collect()
}

fn smart(runtime: &Runtime) -> Value {
    serde_json::to_value(runtime.try_snapshot().unwrap().smart_objects).unwrap()
}

fn slot(runtime: &Runtime, index: u32) -> SlotRef {
    SlotRef {
        object: runtime.entity("bench").unwrap().to_bits(),
        slot: index,
    }
}

fn names(events: &[(String, Value)]) -> Vec<String> {
    events
        .iter()
        .map(|(n, p)| {
            format!(
                "{}:{}",
                n.trim_start_matches("smart_object."),
                p["user"].as_str().unwrap_or("-")
            )
        })
        .collect()
}

#[test]
fn claims_are_served_in_entity_order_and_slots_are_released_after_the_duration() {
    // Three sitters for two seats; they are all next to the bench and tick in entity order.
    let params = json!({"tags":["sit"],"duration":1.0,"arrive_distance":5,"behavior_key":"how"});
    let dir = project(
        &[("use", use_graph(params))],
        vec![
            bench([0.0, 0.0, 0.0], 10.0),
            user("a", [-1.0, 0.0, 0.0], false),
            user("b", [1.0, 0.0, 0.0], false),
            user("c", [0.0, 0.0, 0.5], false),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 2);
    // Agents tick in entity order and claims are served in call order: the first two
    // entities get a seat, the last one is denied.
    let mut by_entity: Vec<(Entity, &str)> = ["a", "b", "c"]
        .into_iter()
        .map(|id| (runtime.entity(id).unwrap(), id))
        .collect();
    by_entity.sort();
    let (winners, loser) = ([by_entity[0].1, by_entity[1].1], by_entity[2].1);
    let state_of = |runtime: &Runtime, id: &str| -> Value {
        paths(runtime).into_iter().find(|(e, _)| e == id).unwrap().1
    };
    assert_eq!(
        state_of(&runtime, loser),
        json!("denied"),
        "no seat left for the third"
    );
    for id in winners {
        assert_eq!(state_of(&runtime, id), json!("use"), "{id} sits");
    }
    let events = log(&runtime);
    assert_eq!(
        names(&events),
        [
            format!("claimed:{}", winners[0]),
            format!("activated:{}", winners[0]),
            format!("claimed:{}", winners[1]),
            format!("activated:{}", winners[1]),
        ]
    );
    let seats_taken: Vec<&str> = events
        .iter()
        .filter(|(n, _)| n == "smart_object.claimed")
        .map(|(_, p)| p["slot"].as_str().unwrap())
        .collect();
    assert_ne!(
        seats_taken[0], seats_taken[1],
        "two users never share a slot"
    );
    assert!(seats_taken.iter().all(|s| *s == "left" || *s == "right"));
    assert_eq!(events[0].1["behavior"], "sit_down");
    let status = smart(&runtime);
    assert_eq!(
        (status["occupied"].as_u64(), status["free"].as_u64()),
        (Some(2), Some(1))
    );
    let holders: Vec<&str> = status["listed"][0]["slots"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["user"].as_str())
        .collect();
    assert_eq!(holders.len(), 2);
    // After the one-second duration both leave and the seats are free again.
    tick(&mut runtime, 12);
    let all = log(&runtime);
    let released: Vec<&Value> = all
        .iter()
        .filter(|(n, _)| n == "smart_object.released")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(released.len(), 2);
    assert!(released.iter().all(|p| p["reason"] == "completed"));
    for id in winners {
        assert_eq!(state_of(&runtime, id), json!("done"));
    }
    assert_eq!(state_of(&runtime, loser), json!("denied"));
    let status = smart(&runtime);
    assert_eq!(
        (status["free"].as_u64(), status["occupied"].as_u64()),
        (Some(3), Some(0))
    );
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    let how: Vec<&Value> = trees["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["entity"] == winners[0])
        .map(|a| &a["blackboard"]["how"])
        .collect();
    assert_eq!(how[0], "sit_down");
}

#[test]
fn an_agent_walks_to_the_slot_before_occupying_it() {
    let params =
        json!({"tags":["work"],"duration":0.5,"arrive_distance":0.8,"walk":true,"navmesh":"nav"});
    let dir = project(
        &[("use", use_graph(params))],
        vec![
            bench([8.0, 0.0, 1.0], 10.0),
            user("walker", [1.0, 0.0, 1.0], true),
        ],
    );
    let mut runtime = started(&dir);
    let walker = runtime.entity("walker").unwrap();
    let mut activated_at = None;
    let mut x_at_activation = 0.0;
    for t in 1..=60 {
        tick(&mut runtime, 1);
        let x = world_transform(runtime.app().world(), walker)
            .unwrap()
            .position()[0];
        if activated_at.is_none()
            && log(&runtime)
                .iter()
                .any(|(n, _)| n == "smart_object.activated")
        {
            activated_at = Some(t);
            x_at_activation = x;
        }
    }
    let at = activated_at.expect("the walker arrives and occupies the desk");
    assert!(at > 10, "the 6 m walk takes more than a second: tick {at}");
    assert!(x_at_activation > 6.0, "x = {x_at_activation}");
    let names = names(&log(&runtime));
    assert_eq!(names[0], "claimed:walker", "{names:?}");
    assert_eq!(names.last().map(String::as_str), Some("released:walker"));
    assert_eq!(paths(&runtime)[0].1, json!("done"));
    let nav = runtime
        .app()
        .world()
        .get::<engine_project::simulation::NavigationAgent>(walker)
        .unwrap();
    assert!(nav.target.is_none(), "navigation is released with the slot");
}

#[test]
fn an_unoccupied_claim_times_out_and_the_task_fails() {
    // The sitter stays far away and never arrives; the 1 s claim expires.
    let params = json!({"tags":["sit"],"duration":1.0,"arrive_distance":0.5});
    let dir = project(
        &[("use", use_graph(params))],
        vec![
            bench([0.0, 0.0, 0.0], 1.0),
            user("late", [9.0, 0.0, 9.0], false),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 8);
    assert_eq!(
        paths(&runtime)[0].1,
        json!("use"),
        "still waiting before the timeout"
    );
    tick(&mut runtime, 6);
    assert_eq!(paths(&runtime)[0].1, json!("denied"));
    let events = log(&runtime);
    let released = events
        .iter()
        .find(|(n, _)| n == "smart_object.released")
        .unwrap();
    assert_eq!(released.1["reason"], "timeout");
    assert_eq!(smart(&runtime)["free"], 3);
}

#[test]
fn leaving_the_state_or_despawning_releases_the_slot() {
    let params = json!({"tags":["sit"],"arrive_distance":5});
    let dir = project(
        &[("use", use_graph(params))],
        vec![
            bench([0.0, 0.0, 0.0], 10.0),
            user("a", [-1.0, 0.0, 0.0], false),
            user("b", [1.0, 0.0, 0.0], false),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 3);
    assert_eq!(smart(&runtime)["occupied"], 2);
    // `a` is told to stop: its state exits, aborting the task.
    let a = runtime.entity("a").unwrap();
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: "stop".into(),
            source: None,
            target: Some(a),
            payload: json!({}),
        },
    )
    .unwrap();
    tick(&mut runtime, 2);
    assert_eq!(paths(&runtime)[0].1, json!("idle"));
    let reasons = |runtime: &Runtime| -> Vec<String> {
        log(runtime)
            .iter()
            .filter(|(n, _)| n == "smart_object.released")
            .map(|(_, p)| p["reason"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(reasons(&runtime), ["aborted"]);
    assert_eq!(smart(&runtime)["occupied"], 1);
    // `b` disappears: the slot is freed without anyone asking.
    let b = runtime.entity("b").unwrap();
    runtime.app_mut().world_mut().despawn(b);
    tick(&mut runtime, 2);
    assert_eq!(reasons(&runtime), ["aborted", "removed"]);
    assert_eq!(smart(&runtime)["free"], 3);
}

#[test]
fn natives_find_claim_occupy_and_release_slots() {
    let dir = project(
        &[],
        vec![
            bench([0.0, 0.0, 0.0], 10.0),
            entity("a", [3.0, 0.0, 0.0], json!({})),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 1);
    let a = runtime.entity("a").unwrap();
    let (left, right, desk) = (slot(&runtime, 0), slot(&runtime, 1), slot(&runtime, 2));
    let w = runtime.app_mut().world_mut();
    let sit = vec!["sit".to_owned()];
    // Ordered by slot, then by distance when a position is given (a stands at x = 3).
    let by_slot = find_slots(w, &sit, true, None).unwrap();
    assert_eq!(
        by_slot.iter().map(|s| s.slot).collect::<Vec<_>>(),
        [left, right]
    );
    let by_distance = find_slots(w, &sit, true, Some([3.0, 0.0, 0.0])).unwrap();
    assert_eq!(by_distance[0].slot, right);
    assert!((by_distance[0].position[0] - 0.6).abs() < 1e-4);
    assert_eq!(by_distance[0].behavior, "sit_down");
    assert_eq!(
        find_slots(w, &["nope".to_owned()], true, None)
            .unwrap()
            .len(),
        0
    );
    // Lifecycle.
    assert_eq!(claim_slot(w, right, a).unwrap(), Ok(()));
    assert_eq!(
        claim_slot(w, right, a).unwrap(),
        Err(SlotError::Taken(a.to_bits()))
    );
    assert_eq!(
        find_slots(w, &sit, true, None).unwrap().len(),
        1,
        "free only"
    );
    assert_eq!(find_slots(w, &sit, false, None).unwrap().len(), 2);
    assert_eq!(occupy_slot(w, left, a).unwrap(), Err(SlotError::NotClaimed));
    assert_eq!(occupy_slot(w, right, a).unwrap(), Ok(()));
    assert_eq!(
        slot_info(w, right).unwrap().unwrap().state,
        SlotState::Occupied { user: a.to_bits() }
    );
    assert_eq!(
        release_slot(w, right, a, ReleaseReason::Completed).unwrap(),
        Ok(())
    );
    assert_eq!(slot_info(w, desk).unwrap().unwrap().state, SlotState::Free);
    assert!(
        slot_info(w, SlotRef { object: 1, slot: 0 })
            .unwrap()
            .is_none()
    );
}

#[test]
fn smart_object_slots_are_an_environment_query_generator() {
    let templates = json!([{"name":"seat","generator":{"type":"smart_objects","tags":["sit"]},
        "tests":[{"type":"distance","score":"lower_better"}]}]);
    let dir = project(
        &[],
        vec![
            bench([0.0, 0.0, 0.0], 10.0),
            entity(
                "ai",
                [0.0; 3],
                component("engine.EnvironmentQueries", json!({"queries":templates})),
            ),
            entity("a", [3.0, 0.0, 0.0], json!({})),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 1);
    let a = runtime.entity("a").unwrap();
    let bench_entity = runtime.entity("bench").unwrap();
    let right = slot(&runtime, 1);
    let w = runtime.app_mut().world_mut();
    let best = run_query(w, a, "seat", None).unwrap();
    assert_eq!(best.items[0].item.subject, Some(bench_entity.to_bits()));
    assert_eq!(best.items[0].item.slot, Some(1), "the right seat is nearer");
    // A claimed seat drops out of the free-only generator.
    claim_slot(w, right, a).unwrap().unwrap();
    let next = run_query(w, a, "seat", None).unwrap();
    assert_eq!(next.generated, 1);
    assert_eq!(next.items[0].item.slot, Some(0));
}

#[test]
fn smart_object_data_is_validated() {
    let load = |slots: Value, timeout: Value| {
        let dir = project(
            &[],
            vec![entity(
                "o",
                [0.0; 3],
                component(
                    "engine.SmartObject",
                    json!({"slots":slots,"claim_timeout":timeout}),
                ),
            )],
        );
        Project::load(dir.path().join("project.json"))
            .map(|_| ())
            .unwrap_err()
    };
    let seat = |name: &str| json!({"name":name,"offset":[0,0,0],"tags":["sit"]});
    assert_eq!(
        load(json!([seat("a"), seat("a")]), json!(5)).diagnostics[0].code,
        "smart_object.value.invalid",
        "slot names repeat"
    );
    assert_eq!(
        load(json!([seat("bad name")]), json!(5)).diagnostics[0].code,
        "smart_object.value.invalid"
    );
    assert_eq!(
        load(json!([seat("a")]), json!(-1)).diagnostics[0].code,
        "component.schema.invalid"
    );
    let many: Vec<Value> = (0..17).map(|i| seat(&format!("s{i}"))).collect();
    assert_eq!(
        load(Value::Array(many), json!(5)).diagnostics[0].code,
        "component.schema.invalid"
    );
    let dir = project(&[], vec![bench([0.0; 3], 5.0)]);
    let loaded = Project::load(dir.path().join("project.json")).unwrap();
    assert_eq!(loaded.component_catalog.0["engine.SmartObject"].version, 1);
}

fn trace() -> (Vec<(String, Value)>, Value) {
    let params =
        json!({"tags":["work"],"duration":0.5,"arrive_distance":0.8,"walk":true,"navmesh":"nav"});
    let dir = project(
        &[("use", use_graph(params))],
        vec![
            bench([8.0, 0.0, 1.0], 10.0),
            user("w1", [1.0, 0.0, 1.0], true),
            user("w2", [1.0, 0.0, 2.0], true),
        ],
    );
    let mut runtime = started(&dir);
    tick(&mut runtime, 60);
    (log(&runtime), smart(&runtime))
}

#[test]
fn two_runs_give_identical_events_and_telemetry() {
    let (events, status) = trace();
    let (events2, status2) = trace();
    assert_eq!(events, events2);
    assert_eq!(status, status2);
    // Only one worker fits the single desk; the other is denied at once.
    let claimed = events
        .iter()
        .filter(|(n, _)| n == "smart_object.claimed")
        .count();
    assert_eq!(claimed, 1);
}
