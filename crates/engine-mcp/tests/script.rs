//! `script_schema` and the project-aware `graph_validate` / `graph_edit` for script graphs.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

fn declare_functions(dir: &Path) {
    fs::write(
        dir.join("functions.json"),
        json!([{"name": "game.chime", "params": [{"name": "n", "type": "int"}],
            "cost": 2, "effects": ["write_self"]}])
        .to_string(),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    manifest["scriptFunctions"] = json!("functions.json");
    fs::write(dir.join("project.json"), manifest.to_string()).unwrap();
}

fn chime_edits(function: &str) -> Value {
    json!([
        {"op":"add_node","id":"s","node_type":"on_start"},
        {"op":"add_node","id":"c","node_type":"call","params":{"function":function},
         "ports":[{"name":"exec","direction":"in","type":"flow","multiple":true},
                  {"name":"then","direction":"out","type":"flow","multiple":true},
                  {"name":"n","direction":"in","type":"int"}]},
        {"op":"add_node","id":"k","node_type":"const","params":{"type":"int","value":3}},
        {"op":"connect","from":{"node":"s","port":"then"},"to":{"node":"c","port":"exec"}},
        {"op":"connect","from":{"node":"k","port":"value"},"to":{"node":"c","port":"n"}}
    ])
}

#[test]
fn script_schema_describes_the_kind_functions_limits_and_codes() {
    let dir = fixture();
    declare_functions(dir.path());
    let schema = data(dir.path(), "script_schema", json!({}));
    assert_eq!(schema["ok"], true, "{schema}");
    assert_eq!(schema["kind"]["name"], "script_graph");
    assert!(schema["kind"]["node_types"].as_array().unwrap().len() >= 40);
    assert_eq!(schema["component"]["name"], "engine.ScriptGraph");
    assert_eq!(schema["project_functions_loaded"], true);
    let functions = schema["functions"].as_array().unwrap();
    let find = |name: &str| functions.iter().find(|f| f["name"] == name).unwrap();
    assert_eq!(find("engine.set_position")["builtin"], true);
    assert_eq!(find("game.chime")["builtin"], false);
    assert_eq!(find("game.chime")["cost"], 2);
    assert_eq!(find("game.chime")["params"][0]["type"], "int");
    let codes: Vec<&str> = schema["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(codes.contains(&"script.fuel.exhausted") && codes.contains(&"script.call.unknown"));
    assert_eq!(schema["limits"]["max_call_depth"], 16);
    assert!(schema["limits"]["spawn"].as_str().is_some());

    let one = data(dir.path(), "script_schema", json!({"node_type":"branch"}));
    assert_eq!(one["kind"]["node_types"].as_array().unwrap().len(), 1);
    let missing = data(dir.path(), "script_schema", json!({"node_type":"nope"}));
    assert_eq!(error_code(&missing), "graph.node.unknown_type");
}

#[test]
fn script_graphs_are_checked_against_the_declared_functions() {
    let dir = fixture();
    declare_functions(dir.path());
    let created = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/chime.graph.json","kind":"script_graph","edits":chime_edits("game.chime")}),
    );
    assert_eq!(created["ok"], true, "{created}");
    let validated = data(
        dir.path(),
        "graph_validate",
        json!({"path":"assets/chime.graph.json"}),
    );
    assert_eq!(validated["valid"], true, "{validated}");
    assert_eq!(validated["project_checked"], true);

    // An undeclared function is rejected by graph_edit and nothing is written.
    let bad = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/bad.graph.json","kind":"script_graph","edits":chime_edits("game.nope")}),
    );
    assert_eq!(error_code(&bad), "script.call.unknown", "{bad}");
    assert!(!dir.path().join("assets/bad.graph.json").exists());

    // graph_validate on an inline document reports the same code.
    let document = json!({"version":1,"kind":"script_graph","nodes":[
        {"id":"s","node_type":"on_start"},
        {"id":"c","node_type":"call","params":{"function":"game.nope"},
         "ports":[
            {"name":"exec","direction":"in","type":"flow","multiple":true},
            {"name":"then","direction":"out","type":"flow","multiple":true}]}],
        "edges":[{"from":{"node":"s","port":"then"},"to":{"node":"c","port":"exec"}}]});
    let result = data(dir.path(), "graph_validate", json!({"document":document}));
    assert_eq!(result["valid"], false);
    assert_eq!(result["diagnostics"][0]["code"], "script.call.unknown");

    // Kind rules alone (no project function lookup needed) still apply.
    let empty = data(
        dir.path(),
        "graph_validate",
        json!({"document":{"version":1,"kind":"script_graph","nodes":[],"edges":[]}}),
    );
    assert_eq!(
        empty["diagnostics"][0]["code"],
        "script.graph.entry_missing"
    );
    assert_eq!(empty["project_checked"], false);
}

fn add_text_asset(dir: &Path, id: &str, path: &str, source: &[u8]) {
    fs::write(dir.join(path), source).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": id, "kind": "text_script", "path": path}));
    fs::write(dir.join("project.json"), manifest.to_string()).unwrap();
}

const GOOD: &str = r#"
    const events = ["player.hit"];
    fn on_start() { engine::log("hi"); }
    fn on_event(name, payload, source) {}
    fn helper(a, b) { a + b }
"#;

#[test]
fn script_check_validates_text_scripts_without_running_them() {
    let dir = fixture();
    let root = dir.path();
    add_text_asset(root, "asset:good", "assets/good.rhai", GOOD.as_bytes());
    add_text_asset(root, "asset:bad", "assets/bad.rhai", b"fn on_start( {");
    // A top-level infinite loop would hang a checker that evaluated the script.
    add_text_asset(
        root,
        "asset:loop",
        "assets/loop.rhai",
        b"loop {} fn on_start() {}",
    );

    for args in [
        json!({"asset": "asset:good"}),
        json!({"path": "assets/good.rhai"}),
        json!({"source": GOOD}),
    ] {
        let result = data(root, "script_check", args.clone());
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["valid"], true, "{args}: {result}");
        assert_eq!(result["diagnostics"], json!([]));
        assert_eq!(result["events"], json!(["player.hit"]));
        let functions = result["functions"].as_array().unwrap();
        let find = |name: &str| functions.iter().find(|f| f["name"] == name).unwrap();
        assert_eq!(find("on_event")["parameters"], 3);
        assert_eq!(find("helper")["parameters"], 2);
        assert_eq!(result["source_bytes"], GOOD.len());
        assert_eq!(result["limits"]["max_source_bytes"], 256 * 1024);
        if args.get("source").is_none() {
            assert_eq!(result["path"], "assets/good.rhai");
            assert_eq!(result["revision"].as_str().unwrap().len(), 64);
        }
    }

    let looped = data(root, "script_check", json!({"asset": "asset:loop"}));
    assert_eq!(looped["valid"], true, "{looped}");

    let bad = data(root, "script_check", json!({"asset": "asset:bad"}));
    assert_eq!(bad["ok"], true, "{bad}");
    assert_eq!(bad["valid"], false);
    assert_eq!(bad["diagnostics"][0]["code"], "script.text.parse");
    assert_eq!(bad["diagnostics"][0]["file"], "assets/bad.rhai");
    assert_eq!(bad["diagnostics"][0]["path"], "1:14", "{bad}");

    for (source, code) in [
        ("fn on_update() {}", "script.text.handler_missing"),
        (
            "const events = [\"a.b\"]; fn on_start() {}",
            "script.text.handler_missing",
        ),
        (
            "const events = [5]; fn on_event(a, b, c) {}",
            "script.text.events",
        ),
        ("import \"x\" as y;", "script.text.parse"),
        ("fn on_start() { eval(\"1\"); }", "script.text.parse"),
    ] {
        let result = data(root, "script_check", json!({"source": source}));
        assert_eq!(result["valid"], false, "{source}: {result}");
        assert_eq!(result["diagnostics"][0]["code"], code, "{source}");
    }
}

#[test]
fn script_check_rejects_bad_arguments_and_oversize_sources() {
    let dir = fixture();
    let root = dir.path();
    add_text_asset(root, "asset:good", "assets/good.rhai", GOOD.as_bytes());
    fs::write(root.join("assets/huge.rhai"), vec![b' '; 256 * 1024 + 1]).unwrap();
    fs::write(root.join("assets/binary.rhai"), [0xff, 0xfe, 0x00]).unwrap();
    fs::write(root.join("assets/notes.txt"), "fn on_start() {}").unwrap();
    add_text_asset(
        root,
        "asset:huge",
        "assets/huge.rhai",
        &vec![b' '; 256 * 1024 + 1],
    );

    // Exactly one source.
    for args in [
        json!({}),
        json!({"asset": "asset:good", "source": GOOD}),
        json!({"path": "assets/good.rhai", "source": GOOD}),
    ] {
        let result = data(root, "script_check", args.clone());
        assert_eq!(error_code(&result), "invalid_argument", "{args}: {result}");
    }
    // Unknown asset, wrong kind and escaping paths are refused before any read.
    let result = data(root, "script_check", json!({"asset": "asset:none"}));
    assert_eq!(error_code(&result), "asset_not_found", "{result}");
    let result = data(root, "script_check", json!({"asset": "asset:script"}));
    assert!(
        ["asset_kind", "asset_not_found"].contains(&error_code(&result).as_str()),
        "{result}"
    );
    for path in [
        "../outside.rhai",
        "/etc/passwd.rhai",
        "assets/missing.rhai",
        ".git/config.rhai",
    ] {
        let result = data(root, "script_check", json!({"path": path}));
        assert_eq!(error_code(&result), "path_invalid", "{path}: {result}");
    }
    let result = data(root, "script_check", json!({"path": "assets/notes.txt"}));
    assert_eq!(error_code(&result), "invalid_argument", "{result}");
    // Oversize and non-UTF-8 input is a parse diagnostic or a bounded-read failure,
    // never a read of the whole file.
    for args in [
        json!({"path": "assets/huge.rhai"}),
        json!({"asset": "asset:huge"}),
        json!({"source": "x".repeat(256 * 1024 + 1)}),
    ] {
        let result = data(root, "script_check", args.clone());
        assert_eq!(
            error_code(&result),
            "script.text.parse",
            "{args:.60}: {result:.200}"
        );
    }
    let result = data(root, "script_check", json!({"path": "assets/binary.rhai"}));
    assert_eq!(result["valid"], false, "{result}");
    assert_eq!(result["diagnostics"][0]["code"], "script.text.parse");
}

#[test]
fn script_schema_describes_the_text_backend() {
    let dir = fixture();
    let schema = data(dir.path(), "script_schema", json!({}));
    let text = &schema["text"];
    assert_eq!(text["enabled"], true, "{schema}");
    assert_eq!(text["asset_kind"], "text_script");
    assert_eq!(text["component"]["name"], "engine.TextScript");
    assert!(text["component"]["schema"]["properties"]["script"].is_object());
    assert!(text["component"]["schema"]["properties"]["graph"].is_null());
    assert_eq!(text["limits"]["max_call_levels"], 16);
    assert_eq!(text["limits"]["max_source_bytes"], 256 * 1024);
    let codes: Vec<&str> = schema["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(codes.contains(&"script.text.parse") && codes.contains(&"script.text.disabled"));
}
