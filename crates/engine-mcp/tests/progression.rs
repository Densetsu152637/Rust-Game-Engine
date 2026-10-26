//! Inventories, quest logs and dialogues authored through the existing MCP tools.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

fn register(dir: &Path, id: &str, kind: &str, path: &str) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":id,"kind":kind,"path":path}));
    fs::write(dir.join("project.json"), manifest.to_string()).unwrap();
}

fn set(dir: &Path, component: &str, data: Value) -> Value {
    write(
        dir,
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir, START),"entity":"player",
            "component":component,"value":{"version":1,"data":data}}),
    )
}

fn dialogue_edits() -> Value {
    json!([
        {"op":"add_node","id":"start","node_type":"start"},
        {"op":"add_node","id":"hello","node_type":"line","params":{"speaker":"elder","text":"elder.hello"}},
        {"op":"add_node","id":"ask","node_type":"choice"},
        {"op":"add_node","id":"yes","node_type":"option","params":{"text":"opt.yes",
            "conditions":[{"type":"item","item":"coin","count":1}]}},
        {"op":"add_node","id":"bye","node_type":"end"},
        {"op":"connect","from":{"node":"start","port":"next"},"to":{"node":"hello","port":"parent"}},
        {"op":"connect","from":{"node":"hello","port":"next"},"to":{"node":"ask","port":"parent"}},
        {"op":"connect","from":{"node":"ask","port":"options"},"to":{"node":"yes","port":"owner"}},
        {"op":"connect","from":{"node":"yes","port":"next"},"to":{"node":"bye","port":"parent"}}
    ])
}

#[test]
fn dialogues_are_authored_with_the_graph_tools_and_the_components_with_component_set() {
    let dir = fixture();
    // The dialogue kind is listed next to the other kinds.
    let overview = data(dir.path(), "graph_schema", json!({}));
    let kinds: Vec<&str> = overview["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"dialogue"), "{kinds:?}");
    let full = data(dir.path(), "graph_schema", json!({"kind":"dialogue"}));
    let types: Vec<&str> = full["kinds"][0]["node_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "start",
        "line",
        "choice",
        "option",
        "give_item",
        "jump",
        "end",
    ] {
        assert!(types.contains(&expected), "{types:?}");
    }

    // A choice without options is a dialogue diagnostic, not a graph one.
    let broken = json!({"version":1,"kind":"dialogue","nodes":[
        {"id":"start","node_type":"start"},{"id":"ask","node_type":"choice"}],
        "edges":[{"from":{"node":"start","port":"next"},"to":{"node":"ask","port":"parent"}}]});
    let result = data(dir.path(), "graph_validate", json!({"document":broken}));
    assert_eq!(result["valid"], false);
    assert_eq!(result["diagnostics"][0]["code"], "dlg.choice.empty");

    let created = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/elder.graph.json","kind":"dialogue","edits":dialogue_edits()}),
    );
    assert_eq!(created["ok"], true, "{created}");
    register(dir.path(), "elder", "graph", "assets/elder.graph.json");
    fs::write(
        dir.path().join("assets/defs.gameplay.json"),
        json!({"version":1,"items":[{"id":"coin","stack":100}],
            "quests":[{"id":"q","stages":[{"id":"s","objectives":[
                {"type":"flag","id":"o","flag":"f"}]}]}]})
        .to_string(),
    )
    .unwrap();
    register(dir.path(), "defs", "gameplay", "assets/defs.gameplay.json");

    let inventory = set(
        dir.path(),
        "engine.Inventory",
        json!({"definitions":"defs","capacity":{"slots":8,"max_weight":50},
            "items":[{"item":"coin","count":30}],
            "equipment":[{"name":"main_hand","tag":"weapon"}]}),
    );
    assert_eq!(inventory["ok"], true, "{inventory}");
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.Inventory",
            json!({"definitions":"defs","items":[{"item":"ghost","count":1}]})
        )),
        "progression.component.invalid"
    );
    let log = set(
        dir.path(),
        "engine.QuestLog",
        json!({"definitions":"defs","start":["q"],"flags":[]}),
    );
    assert_eq!(log["ok"], true, "{log}");
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.QuestLog",
            json!({"definitions":"defs","start":["nope"]})
        )),
        "progression.component.invalid"
    );
    let participant = set(
        dir.path(),
        "engine.DialogueParticipant",
        json!({"graph":"elder","definitions":"defs"}),
    );
    assert_eq!(participant["ok"], true, "{participant}");
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.DialogueParticipant",
            json!({"graph":"missing"})
        )),
        "project.asset.unknown"
    );
    let info = data(dir.path(), "asset_info", json!({"asset":"elder"}));
    assert_eq!(info["details"]["kind"], "dialogue", "{info}");
    for name in [
        "engine.Inventory",
        "engine.QuestLog",
        "engine.DialogueParticipant",
    ] {
        let schema = data(dir.path(), "component_schema", json!({"component":name}));
        assert_eq!(schema["components"][0]["name"], name, "{schema}");
    }
}
