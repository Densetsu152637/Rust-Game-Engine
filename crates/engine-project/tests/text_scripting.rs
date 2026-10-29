//! `engine.TextScript` end to end: Rhai handlers, declared host functions, timers,
//! fault policy, capabilities with staged writes, last-good reload, shared fuel
//! and telemetry.
use bevy_ecs::prelude::*;
use engine_project::{
    Result,
    runtime::RuntimeLog,
    scripting::{TextScript, register_script_fn},
    *,
};
use engine_script::{Effects, FnSig, NativeCx, ScriptError, Value as Script, ValueType};
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

fn text_entity(id: &str, data: Value) -> Value {
    let bindings: Vec<Value> = RELAY_EVENTS
        .iter()
        .map(|e| json!({"event": e, "handler": "record"}))
        .collect();
    json!({"id": id, "name": id, "components": {
        "Transform": {"position": [1, 0, 1]},
        "EventBindings": {"bindings": bindings},
        "custom": {"engine.TextScript": {"version": 1, "data": data}}}})
}

/// A project whose assets are `.rhai` text scripts (`id`, source).
fn project_with(
    scripts: &[(&str, &str)],
    entities: Vec<Value>,
    functions: Option<Value>,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut assets = Vec::new();
    for (id, source) in scripts {
        let file = format!("{id}.rhai");
        fs::write(dir.path().join(&file), source).unwrap();
        assets.push(json!({"id": id, "kind": "text_script", "path": file}));
    }
    let mut manifest =
        json!({"schema": 1, "name": "text-test", "startup_scene": "scene.json", "assets": assets});
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
fn runtime_log(runtime: &Runtime) -> Vec<String> {
    runtime.app().world().resource::<RuntimeLog>().0.clone()
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

const RELAY: &str = r#"
    const events = ["collision.enter"];
    fn on_start() { this.hits = 0; }
    fn on_event(name, payload, source) {
        game::chime(3);
        this.hits += 1;
        engine::emit("game.hit", #{ n: this.hits, from: name }, engine::self());
    }
"#;

fn relay_dir() -> tempfile::TempDir {
    project_with(
        &[("relay", RELAY)],
        vec![text_entity("hero", json!({"script": "relay"}))],
        Some(declared()),
    )
}

#[test]
fn a_text_script_reacts_to_an_engine_event_and_emits_next_tick() {
    let dir = relay_dir();
    let mut runtime = runtime(&dir, true);
    runtime.start().unwrap();
    let hero = runtime.entity("hero").unwrap();
    send(&mut runtime, "collision.enter", hero);
    tick(&mut runtime, 1).unwrap();
    assert!(
        scripts(&runtime)["instances"][0]["fuel_used"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(runtime.app().world().resource::<Calls>().0, [3]);
    assert!(
        log(&runtime).is_empty(),
        "the emit is delivered a tick later"
    );
    tick(&mut runtime, 1).unwrap();
    assert_eq!(
        log(&runtime),
        [(
            "game.hit".to_owned(),
            json!({"n": 1, "from": "collision.enter"})
        )]
    );
    let telemetry = scripts(&runtime);
    assert_eq!(telemetry["total"], 1);
    assert_eq!(telemetry["instances"][0]["entity"], "hero");
    assert_eq!(telemetry["instances"][0]["graph"], "relay");
    assert_eq!(telemetry["instances"][0]["ticks"], 2);
    assert!(telemetry["instances"][0]["fault"].is_null());
    assert!(telemetry["instances"][0]["state_bytes"].as_u64().unwrap() > 0);

    // The same scenario twice gives the same trace and telemetry.
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
fn undeclared_host_functions_fail_deterministically_at_run_time() {
    // The project declares nothing: game::chime does not exist for the script.
    let dir = project_with(
        &[("relay", RELAY)],
        vec![text_entity("hero", json!({"script": "relay"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    let hero = runtime.entity("hero").unwrap();
    send(&mut runtime, "collision.enter", hero);
    tick(&mut runtime, 1).unwrap();
    let fault = &scripts(&runtime)["instances"][0]["fault"];
    assert_eq!(fault["code"], "script.host.failed", "{fault}");
    assert!(runtime.app().world().resource::<Calls>().0.is_empty());
}

#[test]
fn timers_state_and_stop_handlers_run_in_order() {
    let source = r#"
        fn on_start() { this.fired = 0; engine::log("start"); engine::after(0.25, "later"); }
        fn later() {
            this.fired += 1;
            engine::emit("game.pong", #{ fired: this.fired }, engine::self());
        }
        fn on_stop() { engine::log("stop"); }
    "#;
    let dir = project_with(
        &[("timer", source)],
        vec![text_entity("hero", json!({"script": "timer"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 2).unwrap();
    assert!(
        log(&runtime).is_empty(),
        "the 0.25 s timer is not due after 0.2 s"
    );
    tick(&mut runtime, 2).unwrap();
    assert_eq!(
        log(&runtime),
        [("game.pong".to_owned(), json!({"fired": 1}))]
    );
    assert_eq!(scripts(&runtime)["instances"][0]["pending"], 0);
    runtime.stop().unwrap();
    assert_eq!(
        runtime_log(&runtime)
            .into_iter()
            .filter(|l| l.starts_with("script: "))
            .collect::<Vec<_>>(),
        ["script: start", "script: stop"]
    );
}

#[test]
fn removing_the_component_runs_on_stop_once_and_drops_the_instance() {
    let source = r#"fn on_stop() { engine::log("stopped"); }"#;
    let dir = project_with(
        &[("s", source)],
        vec![text_entity("hero", json!({"script": "s"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    let hero = runtime.entity("hero").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(hero)
        .remove::<TextScript>();
    tick(&mut runtime, 2).unwrap();
    let stops = runtime_log(&runtime)
        .into_iter()
        .filter(|l| l == "script: stopped")
        .count();
    assert_eq!(stops, 1);
    assert!(runtime.try_snapshot().unwrap().scripts.is_none());
    runtime.stop().unwrap();
    assert_eq!(
        runtime_log(&runtime)
            .into_iter()
            .filter(|l| l == "script: stopped")
            .count(),
        1,
        "a removed instance is not stopped a second time"
    );
}

fn spinning(policy: &str) -> Value {
    json!({"script": "spin", "fault_policy": policy})
}
const SPIN: &str = "fn on_update(dt) { loop {} }";

#[test]
fn a_faulting_script_is_disabled_by_default_and_can_stop_the_runtime() {
    let dir = project_with(
        &[
            ("spin", SPIN),
            ("calm", "fn on_update(dt) { engine::log(\"calm\"); }"),
        ],
        vec![
            text_entity("a-spinner", spinning("disable")),
            text_entity("b-calm", json!({"script": "calm"})),
        ],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 4).unwrap();
    let telemetry = scripts(&runtime);
    assert_eq!(
        telemetry["faults"], 1,
        "a disabled instance does not fault again"
    );
    let fault = &telemetry["instances"][0]["fault"];
    assert_eq!(fault["code"], "script.fuel.exhausted");
    assert!(
        fault["site"].is_null() || fault["site"].as_str().unwrap().contains(':'),
        "{fault}"
    );
    assert!(telemetry["instances"][1]["fault"].is_null());
    // The neighbour kept running every tick, including the tick of the fault.
    let calm = runtime_log(&runtime)
        .into_iter()
        .filter(|l| l == "script: calm")
        .count();
    assert_eq!(calm, 4);
    let faulted: Vec<_> = log(&runtime)
        .into_iter()
        .filter(|(n, _)| n == "script.faulted")
        .collect();
    assert_eq!(faulted.len(), 1);
    assert_eq!(faulted[0].1["code"], "script.fuel.exhausted");
    assert_eq!(faulted[0].1["graph"], "spin");

    let dir = project_with(
        &[("spin", SPIN)],
        vec![text_entity("hero", spinning("stop_runtime"))],
        None,
    );
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
    let source = r#"
        fn on_start() {
            engine::set_position(engine::self(), [9.0, 0.0, 0.0]);
            engine::component_set("engine.TextScript", "seed", 1);
        }
    "#;
    let dir = project_with(
        &[("deny", source)],
        vec![text_entity("hero", json!({"script": "deny"}))],
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
    assert_eq!(
        runtime.app().world().get::<TextScript>(hero).unwrap().seed,
        0
    );
}

#[test]
fn granted_writes_commit_when_the_run_succeeds() {
    let source = r#"
        fn on_start() {
            engine::set_position(engine::self(), [9.0, 0.0, 0.0]);
            engine::component_set("engine.TextScript", "seed", 7);
            let read = engine::component_get("engine.TextScript", "seed");
            if read == 0 { engine::log("saw old seed"); }
        }
    "#;
    let dir = project_with(
        &[("mover", source)],
        vec![text_entity(
            "hero",
            json!({"script": "mover", "capabilities": {
                "read": ["engine.TextScript"], "write": ["engine.TextScript"]}}),
        )],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    let hero = runtime.entity("hero").unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(hero)
            .unwrap()
            .position,
        [9.0, 0.0, 0.0]
    );
    assert!(
        scripts(&runtime)["instances"][0]["fault"].is_null(),
        "{}",
        scripts(&runtime)
    );
    assert_eq!(
        runtime.app().world().get::<TextScript>(hero).unwrap().seed,
        7
    );
    // Reads see the world as of the start of the run: the staged 7 is not visible.
    assert!(runtime_log(&runtime).contains(&"script: saw old seed".to_owned()));
}

#[test]
fn reload_keeps_the_last_good_program_and_restarts_on_success() {
    let source = |tag: &str| {
        format!(
            r#"
            fn on_start() {{ engine::log("start {tag}"); }}
            fn on_stop() {{ engine::log("stop {tag}"); }}
            fn on_update(dt) {{ engine::log("tick {tag}"); }}
        "#
        )
    };
    let dir = project_with(
        &[("s", &source("v1"))],
        vec![text_entity("hero", json!({"script": "s"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 2).unwrap();

    // A candidate that does not parse is rejected; v1 keeps running.
    fs::write(dir.path().join("s.rhai"), "fn on_start( {").unwrap();
    let error = runtime.reload_script_asset("s").unwrap_err();
    assert_eq!(code_of(&error), "script.text.parse");
    tick(&mut runtime, 1).unwrap();
    // A handler with the wrong arity is rejected too.
    fs::write(dir.path().join("s.rhai"), "fn on_update() {}").unwrap();
    let error = runtime.reload_script_asset("s").unwrap_err();
    assert_eq!(code_of(&error), "script.text.handler_missing");
    tick(&mut runtime, 1).unwrap();

    // A good candidate stops v1 and starts v2 fresh at the next tick boundary.
    fs::write(dir.path().join("s.rhai"), source("v2")).unwrap();
    runtime.reload_script_asset("s").unwrap();
    tick(&mut runtime, 1).unwrap();
    let lines: Vec<String> = runtime_log(&runtime)
        .into_iter()
        .filter(|l| l.starts_with("script: "))
        .collect();
    assert_eq!(
        lines,
        [
            "script: start v1",
            "script: tick v1",
            "script: tick v1",
            "script: tick v1",
            "script: tick v1",
            "script: stop v1",
            "script: start v2",
            "script: tick v2",
        ]
    );
}

#[test]
fn reload_recovers_a_faulted_instance_without_touching_neighbours() {
    let dir = project_with(
        &[
            ("bad", SPIN),
            ("calm", "fn on_update(dt) { engine::log(\"calm\"); }"),
        ],
        vec![
            text_entity("a", json!({"script": "bad"})),
            text_entity("b", json!({"script": "calm"})),
        ],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 2).unwrap();
    assert_eq!(
        scripts(&runtime)["instances"][0]["fault"]["code"],
        "script.fuel.exhausted"
    );
    fs::write(
        dir.path().join("bad.rhai"),
        "fn on_update(dt) { engine::log(\"fixed\"); }",
    )
    .unwrap();
    runtime.reload_script_asset("bad").unwrap();
    tick(&mut runtime, 2).unwrap();
    let fixed = runtime_log(&runtime)
        .into_iter()
        .filter(|l| l == "script: fixed")
        .count();
    assert_eq!(fixed, 2);
    assert!(scripts(&runtime)["instances"][0]["fault"].is_null());
    let calm = runtime_log(&runtime)
        .into_iter()
        .filter(|l| l == "script: calm")
        .count();
    assert_eq!(calm, 4);
}

#[test]
fn text_instances_share_the_global_fuel_budget() {
    // Each instance may burn 1,000,000 fuel; the global budget is 2,000,000, so
    // when all spin the third is skipped, not run.
    let spin = |name: &str| {
        text_entity(
            name,
            json!({"script": "spin", "fuel_per_tick": 1_000_000,
        "fault_policy": "disable"}),
        )
    };
    let dir = project_with(
        &[("spin", SPIN)],
        vec![spin("a"), spin("b"), spin("c")],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 1).unwrap();
    let telemetry = scripts(&runtime);
    assert_eq!(telemetry["total"], 3);
    assert_eq!(telemetry["faults"], 2, "{telemetry}");
    assert_eq!(telemetry["skipped"], 1, "{telemetry}");
}

#[test]
fn project_validation_rejects_malformed_text_scripts() {
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "fn on_start( {",
            json!({"script": "s"}),
            "script.text.parse",
        ),
        (
            "import \"x\" as y;",
            json!({"script": "s"}),
            "script.text.parse",
        ),
        (
            "fn on_update() {}",
            json!({"script": "s"}),
            "script.text.handler_missing",
        ),
        (
            "const events = [\"a.b\"];",
            json!({"script": "s"}),
            "script.text.handler_missing",
        ),
        (
            "fn on_start() {}",
            json!({"script": "s", "capabilities": {"spawn": 1}}),
            "component.schema.invalid",
        ),
        (
            "fn on_start() {}",
            json!({"script": "s", "fuel_per_tick": 5}),
            "component.schema.invalid",
        ),
        (
            "fn on_start() {}",
            json!({"script": ""}),
            "script.component.invalid",
        ),
        (
            "fn on_start() {}",
            json!({"script": "s", "capabilities": {"read": ["engine.NoSuchComponent"]}}),
            "script.capability.unknown_component",
        ),
    ];
    for (source, data, code) in cases {
        let dir = project_with(
            &[("s", source)],
            vec![text_entity("hero", data.clone())],
            None,
        );
        let error = Project::load(dir.path().join("project.json")).unwrap_err();
        assert_eq!(code_of(&error), code, "{source} / {data}");
    }
}

#[test]
fn project_validation_rejects_unknown_wrong_kind_and_misnamed_text_assets() {
    // The component names an asset that is not in the catalog.
    let dir = project_with(
        &[("s", "fn on_start() {}")],
        vec![text_entity("hero", json!({"script": "missing"}))],
        None,
    );
    assert!(Project::load(dir.path().join("project.json")).is_err());

    // A text_script asset must be a .rhai file.
    let dir = project_with(&[("s", "fn on_start() {}")], vec![], None);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    fs::write(dir.path().join("s.txt"), "fn on_start() {}").unwrap();
    manifest["assets"][0]["path"] = json!("s.txt");
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(code_of(&error), "project.script.language");

    // An oversize or non-UTF-8 source never reaches the parser.
    for (bytes, code) in [
        (vec![b' '; 256 * 1024 + 1], "project.file.too_large"),
        (vec![0xff, 0xfe, b'f', b'n'], "script.text.parse"),
    ] {
        let dir = project_with(&[("s", "")], vec![], None);
        fs::write(dir.path().join("s.rhai"), bytes).unwrap();
        let error = Project::load(dir.path().join("project.json")).unwrap_err();
        assert_eq!(code_of(&error), code);
    }
}

#[test]
fn a_valid_but_never_started_project_runs_no_script_code() {
    // Top-level statements are never evaluated: not at load, not at start.
    let source = r#"
        engine::log("top-level must not run");
        fn on_start() {}
    "#;
    let dir = project_with(
        &[("s", source)],
        vec![text_entity("hero", json!({"script": "s"}))],
        None,
    );
    let mut runtime = runtime(&dir, false);
    runtime.start().unwrap();
    tick(&mut runtime, 2).unwrap();
    assert!(
        !runtime_log(&runtime)
            .iter()
            .any(|l| l.contains("top-level")),
        "{:?}",
        runtime_log(&runtime)
    );
}
