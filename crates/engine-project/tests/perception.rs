use bevy_ecs::prelude::*;
use engine_ai::perception::Sense;
use engine_project::Result;
use engine_project::ai::{Perceptions, register_hearing_attenuation, report_damage, report_noise};
use engine_project::*;
use serde_json::{Value, json};
use std::fs;

/// A state tree that chases while the sensor reports a sensed target.
fn react_graph() -> Value {
    let node = |id: &str, ty: &str, params: Value| json!({"id":id,"node_type":ty,"params":params});
    let edge = |from: &str, port: &str, to: &str, to_port: &str| json!({"from":{"node":from,"port":port},"to":{"node":to,"port":to_port}});
    json!({"version":1,"kind":"state_tree",
        "variables":[
            {"name":"enemy","type":"entity"},
            {"name":"last_pos","type":"vec3"},
            {"name":"seen","type":"bool","default":false}],
        "nodes":[
            {"id":"root","node_type":"root"},
            node("calm","state",json!({})),
            node("spot","transition",json!({"trigger":"on_tick","target":"state","state":"chase"})),
            node("spot_c","blackboard_condition",json!({"key":"seen","operator":"eq","value":true})),
            node("chase","state",json!({})),
            node("shout","emit_event",json!({"event":"chase.started"})),
            node("lose","transition",json!({"trigger":"on_tick","target":"state","state":"calm"})),
            node("lose_c","blackboard_condition",json!({"key":"seen","operator":"eq","value":false}))],
        "edges":[
            edge("root","children","calm","parent"),
            edge("calm","transitions","spot","owner"),
            edge("spot","conditions","spot_c","owner"),
            edge("root","children","chase","parent"),
            edge("chase","tasks","shout","owner"),
            edge("chase","transitions","lose","owner"),
            edge("lose","conditions","lose_c","owner")]})
}

const BINDINGS: [&str; 5] = [
    "perception.sensed",
    "perception.lost",
    "perception.forgotten",
    "chase.started",
    "state_tree.state_entered",
];

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

fn sight(extra: Value) -> Value {
    let mut sight =
        json!({"range":10,"lose_range":12,"half_angle":45,"peripheral_half_angle":60,"max_age":1});
    sight
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    sight
}

/// A guard at the origin facing -Z with the given sensor data and the reacting state tree.
fn guard(sensor: Value) -> Value {
    let mut e = entity(
        "guard",
        [0.0, 0.0, 0.0],
        merged(vec![
            component("engine.PerceptionSensor", sensor),
            component("engine.StateTreeAgent", json!({"graph":"react"})),
        ]),
    );
    let bindings: Vec<Value> = BINDINGS
        .iter()
        .map(|n| json!({"event":n,"handler":"record"}))
        .collect();
    e["components"]["EventBindings"] = json!({"bindings":bindings});
    e
}

fn sensor() -> Value {
    json!({"team":1,"sight":sight(json!({})),
        "blackboard":{"target":"enemy","position":"last_pos","sensed":"seen"}})
}

fn target(id: &str, position: [f32; 3], team: u32) -> Value {
    entity(
        id,
        position,
        component("engine.PerceptionStimulus", json!({"team":team})),
    )
}

fn settings(data: Value) -> Value {
    entity(
        "settings",
        [0.0; 3],
        component("engine.PerceptionSettings", data),
    )
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

fn wall(at: [f32; 3]) -> Value {
    entity(
        "wall",
        at,
        component(
            "engine.PhysicsBody3D",
            json!({"motion":"fixed","half_extents":[1.5,2.0,0.25],"material":{"layer_name":"world"}}),
        ),
    )
}

fn project(entities: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("react.graph.json"),
        serde_json::to_vec_pretty(&react_graph()).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"perception","startup_scene":"scene.json",
            "assets":[{"id":"react","kind":"graph","path":"react.graph.json"}]})
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

fn tick(runtime: &mut Runtime, n: usize, dt: f32) {
    for _ in 0..n {
        runtime.tick(dt, InputSnapshot::default()).unwrap();
    }
}

fn names(runtime: &Runtime) -> Vec<String> {
    runtime
        .app()
        .world()
        .resource::<Log>()
        .0
        .iter()
        .map(|(n, _)| n.clone())
        .collect()
}

fn log(runtime: &Runtime) -> Vec<(String, Value)> {
    runtime.app().world().resource::<Log>().0.clone()
}

fn perception(runtime: &Runtime) -> Value {
    serde_json::to_value(runtime.try_snapshot().unwrap().perception).unwrap()
}

fn move_to(runtime: &mut Runtime, id: &str, position: [f32; 3]) {
    let e = runtime.entity(id).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(e)
        .unwrap()
        .position = position;
}

fn sensed(runtime: &Runtime) -> bool {
    let guard = runtime.entity("guard").unwrap();
    runtime
        .app()
        .world()
        .resource::<Perceptions>()
        .known(guard)
        .iter()
        .any(|k| k.sensed && k.sense == Sense::Sight)
}

#[test]
fn a_wall_blocks_sight_and_a_clear_line_lets_the_state_tree_chase() {
    // Blocked: the intruder stands in the cone but behind a collider.
    let dir = project(vec![
        physics(),
        wall([0.0, 0.0, -3.0]),
        guard(sensor()),
        target("intruder", [0.0, 0.0, -6.0], 2),
    ]);
    let mut blocked = started(&dir);
    tick(&mut blocked, 5, 0.1);
    assert!(!sensed(&blocked));
    assert!(!names(&blocked).contains(&"perception.sensed".to_owned()));
    assert!(!names(&blocked).contains(&"chase.started".to_owned()));
    let status = perception(&blocked);
    assert_eq!(status["perceivers"][0]["known"], 0);
    assert!(status["rays_cast"].as_u64().unwrap() >= 1, "{status}");
    // Stepping aside clears the line: the sensor reports it and the tree reacts.
    move_to(&mut blocked, "intruder", [4.0, 0.0, -6.0]);
    tick(&mut blocked, 3, 0.1);
    assert!(sensed(&blocked), "{}", perception(&blocked));

    // Clear without the collider.
    let dir = project(vec![
        guard(sensor()),
        target("intruder", [0.0, 0.0, -6.0], 2),
    ]);
    let mut clear = started(&dir);
    tick(&mut clear, 3, 0.1);
    assert!(sensed(&clear));
    let events = log(&clear);
    let sensed_event = events
        .iter()
        .find(|(n, _)| n == "perception.sensed")
        .unwrap();
    assert_eq!(sensed_event.1["subject"], "intruder");
    assert_eq!(sensed_event.1["sense"], "sight");
    assert_eq!(sensed_event.1["position"], json!([0.0, 0.0, -6.0]));
    // End to end: perception wrote the blackboard before the tree ticked, so the
    // chase began on the first tick.
    assert!(names(&clear).contains(&"chase.started".to_owned()));
    let first_entered: Vec<_> = events
        .iter()
        .filter(|(n, _)| n == "state_tree.state_entered")
        .map(|(_, p)| p["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(first_entered, ["calm", "chase"]);
    let status = perception(&clear);
    let guard = &status["perceivers"][0];
    assert_eq!(guard["entity"], "guard");
    assert_eq!(guard["sensed"], 1);
    assert_eq!(guard["best"]["subject"], "intruder");
    assert_eq!(guard["best"]["sensed"], true);
    let trees = serde_json::to_value(clear.try_snapshot().unwrap().state_trees).unwrap();
    let bb = &trees["agents"][0]["blackboard"];
    assert_eq!(bb["enemy"], "intruder");
    assert_eq!(bb["seen"], true);
    assert_eq!(bb["last_pos"], json!([0.0, 0.0, -6.0]));
    assert_eq!(trees["agents"][0]["active_path"], json!(["chase"]));
}

#[test]
fn losing_sight_remembers_then_forgets_and_the_tree_calms_down() {
    let dir = project(vec![
        guard(sensor()),
        target("intruder", [0.0, 0.0, -6.0], 2),
    ]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 2, 0.1);
    assert!(sensed(&runtime));
    move_to(&mut runtime, "intruder", [0.0, 0.0, 20.0]);
    tick(&mut runtime, 2, 0.1);
    assert!(!sensed(&runtime));
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    let bb = &trees["agents"][0]["blackboard"];
    assert_eq!(bb["seen"], false, "remembered, not sensed");
    assert_eq!(bb["enemy"], "intruder", "the memory still names the target");
    assert_eq!(bb["last_pos"], json!([0.0, 0.0, -6.0]));
    assert_eq!(trees["agents"][0]["active_path"], json!(["calm"]));
    assert!(names(&runtime).contains(&"perception.lost".to_owned()));
    // max_age is 1 s: ten 0.1 s ticks after the loss the memory expires.
    tick(&mut runtime, 12, 0.1);
    assert!(names(&runtime).contains(&"perception.forgotten".to_owned()));
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    assert!(
        trees["agents"][0]["blackboard"].get("enemy").is_none(),
        "the target key is cleared with the memory"
    );
    assert_eq!(perception(&runtime)["perceivers"][0]["known"], 0);
}

#[test]
fn the_cone_edges_hold_at_runtime() {
    // (x, z, sensed): the guard at the origin looks down -Z; range 10, half angle 45.
    let cases = [
        (0.0, -9.9, true),
        (0.0, -10.1, false),
        (4.9, -5.0, true),
        (5.1, -5.0, false),
        (-4.9, -5.0, true),
        (0.0, 5.0, false),
    ];
    for (x, z, expected) in cases {
        let dir = project(vec![guard(sensor()), target("t", [x, 0.0, z], 2)]);
        let mut runtime = started(&dir);
        tick(&mut runtime, 2, 0.1);
        assert_eq!(sensed(&runtime), expected, "({x}, {z})");
    }
}

#[test]
fn rotation_turns_the_cone() {
    // Facing +X: rotate the guard 90 degrees about Y (-Z maps to -X for +90, +X for -90).
    let mut g = guard(sensor());
    let half = std::f32::consts::FRAC_PI_4;
    g["components"]["Transform"]["rotation"] = json!([0.0, -half.sin(), 0.0, half.cos()]);
    let dir = project(vec![g, target("t", [6.0, 0.0, 0.0], 2)]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 2, 0.1);
    assert!(sensed(&runtime), "the target is in front after turning");
}

#[test]
fn teams_and_settings_decide_who_is_perceived() {
    let dir = project(vec![
        settings(json!({"relations":[{"a":1,"b":3,"affiliation":"neutral"}]})),
        guard(sensor()),
        target("enemy", [0.0, 0.0, -5.0], 2),
        target("ally", [1.0, 0.0, -5.0], 1),
        target("bystander", [2.0, 0.0, -5.0], 3),
        target("nobody", [3.0, 0.0, -5.0], 0),
    ]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 2, 0.1);
    let guard_entity = runtime.entity("guard").unwrap();
    let known = runtime
        .app()
        .world()
        .resource::<Perceptions>()
        .known(guard_entity);
    assert_eq!(known.len(), 1, "only the hostile team is sensed: {known:?}");
    let enemy = runtime.entity("enemy").unwrap();
    assert_eq!(known[0].subject, enemy.to_bits());
    // With every affiliation allowed all four are perceived.
    let mut all = sensor();
    all["sight"]["affiliations"] = json!(["hostile", "neutral", "friendly"]);
    let dir = project(vec![
        settings(json!({"relations":[{"a":1,"b":3,"affiliation":"neutral"}]})),
        guard(all),
        target("enemy", [0.0, 0.0, -5.0], 2),
        target("ally", [1.0, 0.0, -5.0], 1),
        target("bystander", [2.0, 0.0, -5.0], 3),
        target("nobody", [3.0, 0.0, -5.0], 0),
    ]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 2, 0.1);
    let status = perception(&runtime);
    assert_eq!(status["perceivers"][0]["known"], 4);
    // The blackboard still follows only hostile targets (the default best-target filter).
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    assert_eq!(trees["agents"][0]["blackboard"]["enemy"], "enemy");
}

fn hearing_guard() -> Value {
    guard(
        json!({"team":1,"hearing":{"range":30,"max_age":1,"affiliations":["hostile","neutral"]},
        "blackboard":{"target":"enemy","position":"last_pos","sensed":"seen"}}),
    )
}

#[test]
fn noises_are_heard_within_range_through_events_natives_and_emitters() {
    // Event: perception.noise from a source entity.
    let dir = project(vec![hearing_guard(), target("walker", [0.0, 0.0, -8.0], 2)]);
    let mut runtime = started(&dir);
    let walker = runtime.entity("walker").unwrap();
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: "perception.noise".into(),
            source: Some(walker),
            target: None,
            payload: json!({"loudness":1.0,"range":10.0}),
        },
    )
    .unwrap();
    tick(&mut runtime, 3, 0.1);
    let events = log(&runtime);
    let heard = events
        .iter()
        .find(|(n, _)| n == "perception.sensed")
        .unwrap();
    assert_eq!(heard.1["sense"], "hearing");
    assert_eq!(heard.1["subject"], "walker");
    assert!((heard.1["strength"].as_f64().unwrap() - 0.2).abs() < 1e-4);
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    assert_eq!(trees["agents"][0]["blackboard"]["enemy"], "walker");

    // Native: in and out of range.
    for (distance, expected) in [(5.0, true), (11.0, false)] {
        let dir = project(vec![hearing_guard()]);
        let mut runtime = started(&dir);
        report_noise(
            runtime.app_mut().world_mut(),
            None,
            [distance, 0.0, 0.0],
            1.0,
            10.0,
        )
        .unwrap();
        tick(&mut runtime, 2, 0.1);
        let known = perception(&runtime)["perceivers"][0]["known"]
            .as_u64()
            .unwrap();
        assert_eq!(known == 1, expected, "distance {distance}");
    }

    // Emitter component: a noise every 0.5 s, heard while in range.
    let emitter = entity(
        "radio",
        [0.0, 0.0, -4.0],
        merged(vec![
            component(
                "engine.PerceptionStimulus",
                json!({"team":2,"visible":false}),
            ),
            component(
                "engine.PerceptionNoise",
                json!({"loudness":1.0,"range":10.0,"interval":0.5}),
            ),
        ]),
    );
    let dir = project(vec![hearing_guard(), emitter]);
    let mut runtime = started(&dir);
    tick(&mut runtime, 12, 0.1);
    let known = perception(&runtime);
    assert_eq!(known["perceivers"][0]["best"]["subject"], "radio");
    assert_eq!(known["perceivers"][0]["best"]["sense"], "hearing");
    assert!(known["noises"].as_u64().unwrap() <= 1);
}

fn halve(_: &World, _: Entity, _: [f32; 3], _: Option<Entity>, _: [f32; 3]) -> f32 {
    0.5
}

#[test]
fn the_hearing_hook_attenuates_noise_like_an_acoustics_provider_would() {
    let dir = project(vec![hearing_guard()]);
    let mut runtime = started(&dir);
    register_hearing_attenuation(runtime.app_mut(), halve).unwrap();
    assert!(
        register_hearing_attenuation(runtime.app_mut(), halve).is_err(),
        "one hook"
    );
    report_noise(
        runtime.app_mut().world_mut(),
        None,
        [5.0, 0.0, 0.0],
        1.0,
        10.0,
    )
    .unwrap();
    tick(&mut runtime, 1, 0.1);
    let status = perception(&runtime);
    let best = &status["perceivers"][0]["best"];
    assert!(
        (best["strength"].as_f64().unwrap() - 0.25).abs() < 1e-4,
        "{best}"
    );
}

#[test]
fn damage_reveals_an_attacker_outside_the_view() {
    let g = guard(json!({"team":1,"damage":{"max_age":2},
        "blackboard":{"target":"enemy","position":"last_pos","sensed":"seen"}}));
    let dir = project(vec![g, target("sniper", [0.0, 0.0, 40.0], 2)]);
    let mut runtime = started(&dir);
    let guard_entity = runtime.entity("guard").unwrap();
    let sniper = runtime.entity("sniper").unwrap();
    report_damage(
        runtime.app_mut().world_mut(),
        guard_entity,
        Some(sniper),
        5.0,
    )
    .unwrap();
    tick(&mut runtime, 2, 0.1);
    let status = perception(&runtime);
    assert_eq!(status["perceivers"][0]["best"]["sense"], "damage");
    assert_eq!(status["perceivers"][0]["best"]["subject"], "sniper");
    let trees = serde_json::to_value(runtime.try_snapshot().unwrap().state_trees).unwrap();
    assert_eq!(trees["agents"][0]["blackboard"]["enemy"], "sniper");
    // The same through an engine event.
    let dir = project(vec![
        guard(json!({"team":1,"damage":{"max_age":2}})),
        target("sniper", [0.0, 0.0, 40.0], 2),
    ]);
    let mut runtime = started(&dir);
    let guard_entity = runtime.entity("guard").unwrap();
    let sniper = runtime.entity("sniper").unwrap();
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: "perception.damage".into(),
            source: Some(sniper),
            target: Some(guard_entity),
            payload: json!({"amount":3}),
        },
    )
    .unwrap();
    tick(&mut runtime, 2, 0.1);
    assert_eq!(
        perception(&runtime)["perceivers"][0]["best"]["subject"],
        "sniper"
    );
}

#[test]
fn the_ray_budget_spreads_sight_checks_over_ticks() {
    let mut entities = vec![
        settings(json!({"max_rays_per_tick":1})),
        guard(json!({"team":1,"sight":sight(json!({}))})),
    ];
    for i in 0..3 {
        entities.push(target(&format!("t{i}"), [i as f32 - 1.0, 0.0, -5.0], 2));
    }
    let dir = project(entities);
    let mut runtime = started(&dir);
    let mut counts = Vec::new();
    for _ in 0..4 {
        tick(&mut runtime, 1, 0.1);
        let status = perception(&runtime);
        assert!(status["rays_cast"].as_u64().unwrap() <= 1);
        counts.push(status["perceivers"][0]["sensed"].as_u64().unwrap());
    }
    assert_eq!(counts, [1, 2, 3, 3]);
    assert_eq!(
        perception(&runtime)["rays_deferred"],
        2,
        "three pairs, one ray"
    );
}

#[test]
fn events_are_rate_limited_per_perceiver() {
    let mut entities = vec![guard(
        json!({"team":1,"sight":sight(json!({"range":20,"lose_range":24}))}),
    )];
    for i in 0..10 {
        entities.push(target(
            &format!("t{i:02}"),
            [i as f32 * 0.5 - 2.0, 0.0, -5.0],
            2,
        ));
    }
    let dir = project(entities);
    let mut runtime = started(&dir);
    tick(&mut runtime, 3, 0.1);
    let sensed = names(&runtime)
        .iter()
        .filter(|n| *n == "perception.sensed")
        .count();
    assert_eq!(sensed, 4, "at most 4 events per perceiver per tick");
    let status = perception(&runtime);
    assert_eq!(status["perceivers"][0]["known"], 10);
    assert_eq!(status["dropped_events"], 6);
}

fn trace() -> (Vec<(String, Value)>, Value) {
    let dir = project(vec![
        physics(),
        wall([2.0, 0.0, -3.0]),
        guard(
            json!({"team":1,"sight":sight(json!({})),"hearing":{"range":30,"max_age":1},
            "blackboard":{"target":"enemy","position":"last_pos","sensed":"seen"}}),
        ),
        target("a", [0.0, 0.0, -6.0], 2),
        target("b", [3.0, 0.0, -6.0], 2),
        entity(
            "c",
            [0.0, 0.0, -5.0],
            merged(vec![
                component("engine.PerceptionStimulus", json!({"team":2})),
                component("engine.PerceptionNoise", json!({"interval":0.7,"range":12})),
            ]),
        ),
    ]);
    let mut runtime = started(&dir);
    for step in 0..120 {
        let phase = step as f32 * 0.05;
        move_to(&mut runtime, "a", [phase.sin() * 5.0, 0.0, -6.0]);
        move_to(
            &mut runtime,
            "b",
            [3.0 - phase.cos() * 4.0, 0.0, -4.0 - phase.sin() * 3.0],
        );
        tick(&mut runtime, 1, 1.0 / 30.0);
    }
    (log(&runtime), perception(&runtime))
}

#[test]
fn two_runs_give_identical_events_and_telemetry() {
    let (events, status) = trace();
    let (events2, status2) = trace();
    assert_eq!(events, events2);
    assert_eq!(status, status2);
    assert!(events.iter().any(|(n, _)| n == "perception.sensed"));
    assert!(events.iter().any(|(n, _)| n == "perception.lost"));
}

#[test]
fn project_validation_rejects_bad_perception_data() {
    let load = |entities: Vec<Value>| {
        let dir = project(entities);
        Project::load(dir.path().join("project.json"))
            .map(|_| ())
            .unwrap_err()
    };
    // Component schema.
    let bad_sight = guard(json!({"sight":{"range":0}}));
    assert_eq!(
        load(vec![bad_sight]).diagnostics[0].code,
        "perception.value.invalid"
    );
    let negative = guard(json!({"sight":{"range":-1}}));
    assert_eq!(
        load(vec![negative]).diagnostics[0].code,
        "component.schema.invalid"
    );
    let bad_cone = guard(json!({"sight":{"range":10,"lose_range":5}}));
    assert_eq!(
        load(vec![bad_cone]).diagnostics[0].code,
        "perception.value.invalid"
    );
    let unknown = guard(json!({"bogus":1}));
    assert_eq!(
        load(vec![unknown]).diagnostics[0].code,
        "component.schema.invalid"
    );
    // Two settings entities.
    let two = vec![
        settings(json!({})),
        entity(
            "settings2",
            [0.0; 3],
            component("engine.PerceptionSettings", json!({})),
        ),
    ];
    assert_eq!(
        load(two).diagnostics[0].code,
        "perception.settings.duplicate"
    );
    // Blackboard keys: wrong type, and no agent to write to.
    let wrong = guard(json!({"sight":{}, "blackboard":{"target":"seen"}}));
    assert_eq!(
        load(vec![wrong]).diagnostics[0].code,
        "perception.blackboard.invalid"
    );
    let mut lonely = entity(
        "lonely",
        [0.0; 3],
        component(
            "engine.PerceptionSensor",
            json!({"sight":{}, "blackboard":{"target":"enemy"}}),
        ),
    );
    lonely["components"]["EventBindings"] = json!({"bindings":[]});
    assert_eq!(
        load(vec![lonely]).diagnostics[0].code,
        "perception.blackboard.agent"
    );
    // Noise and stimulus bounds.
    let loud = entity(
        "n",
        [0.0; 3],
        component("engine.PerceptionNoise", json!({"loudness":2})),
    );
    assert_eq!(
        load(vec![loud]).diagnostics[0].code,
        "component.schema.invalid"
    );
    let wide = entity(
        "n",
        [0.0; 3],
        component("engine.PerceptionNoise", json!({"range":0})),
    );
    assert_eq!(
        load(vec![wide]).diagnostics[0].code,
        "perception.value.invalid"
    );
}

#[test]
fn the_components_are_listed_and_projects_without_sensors_have_no_telemetry() {
    let dir = project(vec![guard(sensor()), target("t", [0.0, 0.0, -5.0], 2)]);
    let project_loaded = Project::load(dir.path().join("project.json")).unwrap();
    for name in [
        "engine.PerceptionSensor",
        "engine.PerceptionStimulus",
        "engine.PerceptionNoise",
        "engine.PerceptionSettings",
    ] {
        let d = &project_loaded.component_catalog.0[name];
        assert_eq!(d.version, 1);
        assert_eq!(d.schema["additionalProperties"], false, "{name}");
    }
    let plain = project(vec![entity("e", [0.0; 3], json!({}))]);
    let mut runtime = started(&plain);
    tick(&mut runtime, 1, 0.1);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert!(snapshot.get("perception").is_none());
}
