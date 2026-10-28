//! `engine.ScriptGraph` end to end: events in and out, declared host functions,
//! fault policy, capabilities with staged writes, last-good reload and telemetry.
use bevy_ecs::prelude::*;
use engine_project::{
    Result,
    scripting::{register_script_fn, signature_table},
    *,
};
use engine_script::{Effects, FnSig, NativeCx, ScriptError, Value as Script, ValueType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;

const RELAY_EVENTS: [&str; 3] = ["game.hit", "script.faulted", "game.pong"];

fn declared() -> Value {
    json!([{
        "name": "game.chime",
        "params": [{"name": "n", "type": "int"}],
        "cost": 2,
        "effects": ["write_self"]
    }])
}

fn script_entity(id: &str, data: Value) -> Value {
    let bindings: Vec<Value> = RELAY_EVENTS
        .iter()
        .map(|e| json!({"event": e, "handler": "record"}))
        .collect();
    json!({"id": id, "name": id, "components": {
        "Transform": {"position": [1, 0, 1]},
        "EventBindings": {"bindings": bindings},
        "custom": {"engine.ScriptGraph": {"version": 1, "data": data}}}})
}

fn project_with(
    graphs: &[(&str, Value)],
    entities: Vec<Value>,
    functions: Option<Value>,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut assets = Vec::new();
    for (id, graph) in graphs {
        let file = format!("{id}.graph.json");
        fs::write(
            dir.path().join(&file),
            serde_json::to_vec_pretty(graph).unwrap(),
        )
        .unwrap();
        assets.push(json!({"id": id, "kind": "graph", "path": file}));
    }
    let mut manifest = json!({"schema": 1, "name": "script-test", "startup_scene": "scene.json", "assets": assets});
    if let Some(functions) = functions {
        fs::write(dir.path().join("functions.json"), functions.to_string()).unwrap();
        manifest["scriptFunctions"] = json!("functions.json");
    }
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema": 1, "scene_id": "main", "entities": entities}).to_string(),
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

#[derive(Resource, Default)]
struct Calls(Vec<i64>);
fn chime(
    cx: &mut NativeCx<'_, World>,
    args: &[Script],
) -> std::result::Result<Script, ScriptError> {
    if let Script::Int(n) = args[0] {
        cx.world.resource_mut::<Calls>().0.push(n);
    }
    Ok(Script::Unit)
}
fn chime_sig() -> FnSig {
    FnSig::new(&[("n", ValueType::Int)], None)
        .cost(2)
        .effects(Effects::WRITE_SELF)
}

fn runtime(dir: &tempfile::TempDir, register_chime: bool) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Calls::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    if register_chime {
        register_script_fn(runtime.app_mut(), "game.chime", chime_sig(), chime).unwrap();
    }
    runtime
}
fn tick(runtime: &mut Runtime, n: usize) -> Result<()> {
    for _ in 0..n {
        runtime.tick(0.1, InputSnapshot::default())?;
    }
    Ok(())
}
fn log(runtime: &Runtime) -> Vec<(String, Value)> {
    runtime.app().world().resource::<Log>().0.clone()
}
fn scripts(runtime: &Runtime) -> Value {
    serde_json::to_value(runtime.try_snapshot().unwrap().scripts).unwrap()
}
fn send(runtime: &mut Runtime, name: &str, target: Entity) {
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: name.to_owned(),
            source: None,
            target: Some(target),
            payload: json!({}),
        },
    )
    .unwrap();
}
fn code_of(error: &Error) -> String {
    error.diagnostics[0].code.clone()
}

// --- graphs -------------------------------------------------------------------------------

fn node(id: &str, ty: &str, params: Value) -> Value {
    json!({"id": id, "node_type": ty, "params": params})
}
fn edge(a: &str, ap: &str, b: &str, bp: &str) -> Value {
    json!({"from": {"node": a, "port": ap}, "to": {"node": b, "port": bp}})
}
fn graph(nodes: Vec<Value>, edges: Vec<Value>, variables: Vec<Value>) -> Value {
    json!({"version": 1, "kind": "script_graph", "nodes": nodes, "edges": edges, "variables": variables})
}
fn port(name: &str, direction: &str, ty: &str, multiple: bool) -> Value {
    let ty = if ty == "json" {
        json!({"custom": "script.json"})
    } else {
        json!(ty)
    };
    json!({"name": name, "direction": direction, "type": ty, "multiple": multiple})
}
fn call_node(id: &str, function: &str, extra: Vec<Value>) -> Value {
    let mut ports = vec![
        port("exec", "in", "flow", true),
        port("then", "out", "flow", true),
    ];
    ports.extend(extra);
    json!({"id": id, "node_type": "call", "params": {"function": function}, "ports": ports})
}

/// collision.enter -> chime(3), then emit game.hit to self.
fn relay() -> Value {
    graph(
        vec![
            node("ev", "on_event", json!({"event": "collision.enter"})),
            call_node("chime", "game.chime", vec![port("n", "in", "int", false)]),
            node("three", "const", json!({"type": "int", "value": 3})),
            node(
                "hit",
                "emit_event",
                json!({"event": "game.hit", "payload": {"n": 3}}),
            ),
            node("me", "self", json!({})),
        ],
        vec![
            edge("ev", "then", "chime", "exec"),
            edge("three", "value", "chime", "n"),
            edge("chime", "then", "hit", "exec"),
            edge("me", "entity", "hit", "target"),
        ],
        vec![],
    )
}

fn relay_dir() -> tempfile::TempDir {
    project_with(
        &[("relay", relay())],
        vec![script_entity("hero", json!({"graph": "relay"}))],
        Some(declared()),
    )
}

#[test]
fn a_graph_script_reacts_to_an_engine_event_and_emits_next_tick() {
    let dir = relay_dir();
    let mut runtime = runtime(&dir, true);
    runtime.start().unwrap();
    let hero = runtime.entity("hero").unwrap();
    send(&mut runtime, "collision.enter", hero);
    tick(&mut runtime, 1).unwrap();
    // The script ran in the tick that delivered collision.enter: the native
    // function ran, but its event is only delivered one tick later.
    assert_eq!(runtime.app().world().resource::<Calls>().0, [3]);
    assert!(log(&runtime).is_empty());
    tick(&mut runtime, 1).unwrap();
    assert_eq!(log(&runtime), [("game.hit".to_owned(), json!({"n": 3}))]);
    let telemetry = scripts(&runtime);
    assert_eq!(telemetry["total"], 1);
    assert_eq!(telemetry["instances"][0]["entity"], "hero");
    assert_eq!(telemetry["instances"][0]["ticks"], 2);
    assert!(telemetry["instances"][0]["fault"].is_null());

    // Two runs of the same scenario are identical.
    let again = {
        let dir = relay_dir();
        let mut runtime = self::runtime(&dir, true);
        runtime.start().unwrap();
        let hero = runtime.entity("hero").unwrap();
        send(&mut runtime, "collision.enter", hero);
        tick(&mut runtime, 2).unwrap();
        (log(&runtime), scripts(&runtime))
    };
    assert_eq!((log(&runtime), scripts(&runtime)), again);
}

#[test]
fn unregistered_or_mismatched_functions_fail_start() {
    let dir = relay_dir();
    let mut unregistered = runtime(&dir, false);
    let error = unregistered.start().unwrap_err();
    assert_eq!(code_of(&error), "script.function.unregistered");

    let mut mismatched = runtime(&dir, false);
    register_script_fn(
        mismatched.app_mut(),
        "game.chime",
        FnSig::new(&[("n", ValueType::Float)], None)
            .cost(2)
            .effects(Effects::WRITE_SELF),
        chime,
    )
    .unwrap();
    let error = mismatched.start().unwrap_err();
    assert_eq!(code_of(&error), "script.function.signature");

    // Reserved names and duplicates are rejected at registration.
    let mut ok = runtime(&dir, true);
    let error = register_script_fn(ok.app_mut(), "game.chime", chime_sig(), chime).unwrap_err();
    assert_eq!(code_of(&error), "script.function.duplicate");
    let error = register_script_fn(ok.app_mut(), "engine.mine", chime_sig(), chime).unwrap_err();
    assert_eq!(code_of(&error), "script.function.invalid");
    ok.start().unwrap();
}

#[test]
fn project_validation_applies_the_declared_functions() {
    // The graph calls game.chime but the project declares nothing.
    let dir = project_with(
        &[("relay", relay())],
        vec![script_entity("hero", json!({"graph": "relay"}))],
        None,
    );
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(code_of(&error), "script.call.unknown");
    // A bad declaration fails the project load.
    let dir = project_with(&[], vec![], Some(json!([{"name": "engine.sneaky"}])));
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(code_of(&error), "script.function.invalid");
    // The built-ins are always declared.
    let dir = project_with(&[], vec![], None);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    assert!(
        signature_table(&project)
            .iter()
            .any(|(n, _)| n == "engine.set_position")
    );
}

fn spinner(policy: &str) -> (Value, Value) {
    (
        graph(
            vec![
                node("u", "on_update", json!({})),
                node("w", "while", json!({"max_iterations": 10_000})),
                node("t", "const", json!({"type": "bool", "value": true})),
            ],
            vec![
                edge("u", "then", "w", "exec"),
                edge("t", "value", "w", "condition"),
            ],
            vec![],
        ),
        json!({"graph": "spin", "fault_policy": policy}),
    )
}

#[test]
fn a_faulting_script_is_disabled_by_default_and_can_stop_the_runtime() {
    let (spin, data) = spinner("disable");
    let dir = project_with(&[("spin", spin)], vec![script_entity("hero", data)], None);
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    tick(&mut runtime, 3).unwrap();
    let telemetry = scripts(&runtime);
    assert_eq!(
        telemetry["faults"], 1,
        "disabled instances do not fault again"
    );
    let fault = &telemetry["instances"][0]["fault"];
    assert_eq!(fault["code"], "script.fuel.exhausted");
    // The trap names the node whose op ran out of fuel (the loop condition).
    assert_eq!(fault["site"], "node:t");
    // The faulted event is delivered to the entity on the following tick.
    let faulted: Vec<_> = log(&runtime)
        .into_iter()
        .filter(|(n, _)| n == "script.faulted")
        .collect();
    assert_eq!(faulted.len(), 1);
    assert_eq!(faulted[0].1["code"], "script.fuel.exhausted");
    assert_eq!(faulted[0].1["graph"], "spin");

    let (spin, data) = spinner("stop_runtime");
    let dir = project_with(&[("spin", spin)], vec![script_entity("hero", data)], None);
    let mut runtime = self::runtime(&dir, false);
    runtime.start().unwrap();
    let error = tick(&mut runtime, 1).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|d| d.message.contains("script.fuel.exhausted")),
        "{error:?}"
    );
}

#[test]
fn a_denied_capability_discards_every_staged_write_of_the_run() {
    // set_position (allowed) and then component_set of a component the script
    // was not granted: the trap discards the staged position too.
    let set_pos = call_node(
        "pos",
        "engine.set_position",
        vec![
            port("entity", "in", "entity", false),
            port("position", "in", "vec3", false),
        ],
    );
    let denied = call_node(
        "deny",
        "engine.component_set",
        vec![
            port("name", "in", "string", false),
            port("field", "in", "string", false),
            port("value", "in", "json", false),
        ],
    );
    let g = graph(
        vec![
            node("s", "on_start", json!({})),
            set_pos,
            denied,
            node("me", "self", json!({})),
            node("x", "const", json!({"type": "float", "value": 9.0})),
            node("v", "vec3_make", json!({})),
            node(
                "name",
                "const",
                json!({"type": "string", "value": "engine.ScriptGraph"}),
            ),
            node("field", "const", json!({"type": "string", "value": "seed"})),
            node("val", "const", json!({"type": "json", "value": 1})),
        ],
        vec![
            edge("s", "then", "pos", "exec"),
            edge("me", "entity", "pos", "entity"),
            edge("x", "value", "v", "x"),
            edge("v", "result", "pos", "position"),
            edge("pos", "then", "deny", "exec"),
            edge("name", "value", "deny", "name"),
            edge("field", "value", "deny", "field"),
            edge("val", "value", "deny", "value"),
        ],
        vec![],
    );
    let dir = project_with(
        &[("deny", g)],
        vec![script_entity("hero", json!({"graph": "deny"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    let hero = runtime.entity("hero").unwrap();
    let position = runtime
        .app()
        .world()
        .get::<Transform>(hero)
        .unwrap()
        .position;
    assert_eq!(
        position,
        [1.0, 0.0, 1.0],
        "the staged write did not survive"
    );
    assert_eq!(
        scripts(&runtime)["instances"][0]["fault"]["code"],
        "script.capability.denied"
    );
}

#[test]
fn staged_writes_commit_when_the_run_succeeds_and_component_writes_are_checked() {
    let set_pos = call_node(
        "pos",
        "engine.set_position",
        vec![
            port("entity", "in", "entity", false),
            port("position", "in", "vec3", false),
        ],
    );
    let g = graph(
        vec![
            node("s", "on_start", json!({})),
            set_pos,
            node("me", "self", json!({})),
            node("x", "const", json!({"type": "float", "value": 9.0})),
            node("v", "vec3_make", json!({})),
        ],
        vec![
            edge("s", "then", "pos", "exec"),
            edge("me", "entity", "pos", "entity"),
            edge("x", "value", "v", "x"),
            edge("v", "result", "pos", "position"),
        ],
        vec![],
    );
    let dir = project_with(
        &[("mover", g)],
        vec![script_entity("hero", json!({"graph": "mover"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    let hero = runtime.entity("hero").unwrap();
    let position = runtime
        .app()
        .world()
        .get::<Transform>(hero)
        .unwrap()
        .position;
    assert_eq!(position, [9.0, 0.0, 0.0]);
}

#[derive(Component, Serialize, Deserialize)]
struct NativeChoice {
    mode: Choice,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Choice {
    Ok,
}

#[test]
fn a_later_native_decode_failure_discards_earlier_staged_position() {
    let position = call_node(
        "pos",
        "engine.set_position",
        vec![
            port("entity", "in", "entity", false),
            port("position", "in", "vec3", false),
        ],
    );
    let component = call_node(
        "component",
        "engine.component_set",
        vec![
            port("name", "in", "string", false),
            port("field", "in", "string", false),
            port("value", "in", "json", false),
        ],
    );
    let g = graph(
        vec![
            node("s", "on_start", json!({})),
            position,
            component,
            node("me", "self", json!({})),
            node("x", "const", json!({"type":"float","value":9.0})),
            node("v", "vec3_make", json!({})),
            node(
                "name",
                "const",
                json!({"type":"string","value":"game.Choice"}),
            ),
            node("field", "const", json!({"type":"string","value":"mode"})),
            node("bad", "const", json!({"type":"json","value":"bad"})),
        ],
        vec![
            edge("s", "then", "pos", "exec"),
            edge("me", "entity", "pos", "entity"),
            edge("x", "value", "v", "x"),
            edge("v", "result", "pos", "position"),
            edge("pos", "then", "component", "exec"),
            edge("name", "value", "component", "name"),
            edge("field", "value", "component", "field"),
            edge("bad", "value", "component", "value"),
        ],
        vec![],
    );
    let mut hero = script_entity(
        "hero",
        json!({"graph":"transaction","capabilities":{"write":["game.Choice"]}}),
    );
    hero["components"]["custom"]["game.Choice"] = json!({"version":1,"data":{"mode":"ok"}});
    let dir = project_with(&[("transaction", g)], vec![hero], None);
    fs::write(
        dir.path().join("components.json"),
        json!([{"name":"game.Choice","version":1,"schema":{"type":"object","properties":{"mode":{"type":"string"}},"required":["mode"],"additionalProperties":false},"defaults":{"mode":"ok"},"dataOnly":false}]).to_string(),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    manifest["componentSchemas"] = json!("components.json");
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    let mut runtime = runtime(&dir, false);
    register_component::<NativeChoice>(runtime.app_mut(), "game.Choice", 1).unwrap();
    runtime.start().unwrap();
    assert!(tick(&mut runtime, 1).is_err());
    let hero = runtime.entity("hero").unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(hero)
            .unwrap()
            .position,
        [1.0, 0.0, 1.0]
    );
    assert!(matches!(
        runtime
            .app()
            .world()
            .get::<NativeChoice>(hero)
            .unwrap()
            .mode,
        Choice::Ok
    ));
}

#[test]
fn reload_keeps_the_last_good_program_and_restarts_on_success() {
    let counter = |step: i64| {
        graph(
            vec![
                node("u", "on_update", json!({})),
                node("get", "get_var", json!({"variable": "n"})),
                node("k", "const", json!({"type": "int", "value": step})),
                node("add", "add", json!({"type": "int"})),
                node("set", "set_var", json!({"variable": "n"})),
            ],
            vec![
                edge("u", "then", "set", "exec"),
                edge("get", "value", "add", "a"),
                edge("k", "value", "add", "b"),
                edge("add", "result", "set", "value"),
            ],
            vec![json!({"name": "n", "type": "int", "default": 0})],
        )
    };
    let dir = project_with(
        &[("count", counter(1))],
        vec![script_entity("hero", json!({"graph": "count"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 3).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(3)"
    );

    // A broken candidate is rejected; the old program keeps running.
    fs::write(dir.path().join("count.graph.json"), "{ not json").unwrap();
    let error = runtime.reload_script_asset("count").unwrap_err();
    assert!(!error.diagnostics.is_empty());
    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(4)"
    );

    // A good candidate restarts the instance fresh at the next tick boundary.
    fs::write(
        dir.path().join("count.graph.json"),
        serde_json::to_vec(&counter(10)).unwrap(),
    )
    .unwrap();
    runtime.reload_script_asset("count").unwrap();
    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(10)"
    );
}

#[test]
fn reload_rejects_incompatible_variable_overrides_before_stopping_last_good() {
    let counter = graph(
        vec![
            node("u", "on_update", json!({})),
            node("get", "get_var", json!({"variable": "n"})),
            node("k", "const", json!({"type": "int", "value": 1})),
            node("add", "add", json!({"type": "int"})),
            node("set", "set_var", json!({"variable": "n"})),
        ],
        vec![
            edge("u", "then", "set", "exec"),
            edge("get", "value", "add", "a"),
            edge("k", "value", "add", "b"),
            edge("add", "result", "set", "value"),
        ],
        vec![json!({"name": "n", "type": "int", "default": 0})],
    );
    let dir = project_with(
        &[("count", counter)],
        vec![script_entity(
            "hero",
            json!({"graph": "count", "variables": {"n": 2}}),
        )],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(3)"
    );

    let incompatible = graph(vec![node("u", "on_update", json!({}))], vec![], vec![]);
    fs::write(
        dir.path().join("count.graph.json"),
        serde_json::to_vec(&incompatible).unwrap(),
    )
    .unwrap();
    let error = runtime.reload_script_asset("count").unwrap_err();
    assert_eq!(code_of(&error), "script.variable.invalid");
    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(4)"
    );
}

#[test]
fn pending_reload_is_cancelled_if_component_changes_to_reject_candidate() {
    let original = graph(
        vec![
            node("u", "on_update", json!({})),
            node("k", "const", json!({"type":"int","value":1})),
            node("set", "set_var", json!({"variable":"n"})),
        ],
        vec![
            edge("u", "then", "set", "exec"),
            edge("k", "value", "set", "value"),
        ],
        vec![json!({"name":"n","type":"int","default":0})],
    );
    let dir = project_with(
        &[("count", original)],
        vec![script_entity("hero", json!({"graph":"count"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();

    let candidate = graph(vec![node("u", "on_update", json!({}))], vec![], vec![]);
    fs::write(
        dir.path().join("count.graph.json"),
        serde_json::to_vec(&candidate).unwrap(),
    )
    .unwrap();
    runtime.reload_script_asset("count").unwrap();
    let hero = runtime.entity("hero").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::scripting::ScriptGraph>(hero)
        .unwrap()
        .variables
        .insert("n".into(), json!(5));

    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(1)"
    );
    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["variables"]["n"],
        "Int(1)"
    );
}

#[test]
fn telemetry_is_absent_without_scripts() {
    let dir = project_with(&[], vec![], None);
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    assert!(runtime.try_snapshot().unwrap().scripts.is_none());
}

#[test]
fn component_shape_is_checked_at_validation() {
    for data in [
        json!({"graph": "relay", "capabilities": {"spawn": 1}}),
        json!({"graph": "relay", "fuel_per_tick": 5}),
        json!({"graph": "relay", "capabilities": {"read": ["engine.NoSuchComponent"]}}),
        json!({"graph": "missing"}),
        json!({"graph": "relay", "variables": {"nope": 1}}),
    ] {
        let dir = project_with(
            &[("relay", relay())],
            vec![script_entity("hero", data.clone())],
            Some(declared()),
        );
        assert!(
            Project::load(dir.path().join("project.json")).is_err(),
            "{data} must be rejected"
        );
    }
}
