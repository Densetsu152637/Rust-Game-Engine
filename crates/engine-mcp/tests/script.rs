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
