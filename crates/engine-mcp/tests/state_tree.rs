//! State trees through the existing graph tools and `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

/// Edits that build a complete two-state state tree: idle waits, then goes to alert.
fn build_state_tree() -> Value {
    json!([
        {"op":"add_node","id":"root","node_type":"root"},
        {"op":"add_node","id":"idle","node_type":"state"},
        {"op":"add_node","id":"alert","node_type":"state"},
        {"op":"add_node","id":"nap","node_type":"wait","params":{"seconds":2.0}},
        {"op":"add_node","id":"wake","node_type":"transition",
            "params":{"target":"state","state":"alert","priority":10}},
        {"op":"add_variable","variable":{"name":"danger","type":"bool","default":false}},
        {"op":"connect","from":{"node":"root","port":"children"},"to":{"node":"idle","port":"parent"}},
        {"op":"connect","from":{"node":"root","port":"children"},"to":{"node":"alert","port":"parent"}},
        {"op":"connect","from":{"node":"idle","port":"tasks"},"to":{"node":"nap","port":"owner"}},
        {"op":"connect","from":{"node":"idle","port":"transitions"},"to":{"node":"wake","port":"owner"}}
    ])
}

/// A complete one-node behavior tree, for the wrong-kind reference case.
fn behavior_edits() -> Value {
    json!([
        {"op":"add_node","id":"root","node_type":"root"},
        {"op":"add_node","id":"wait","node_type":"wait"},
        {"op":"connect","from":{"node":"root","port":"child"},"to":{"node":"wait","port":"parent"}}
    ])
}

fn register_asset(dir: &Path, id: &str, path: &str) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":id,"kind":"graph","path":path}));
    fs::write(dir.join("project.json"), manifest.to_string()).unwrap();
}

fn codes(result: &Value) -> Vec<String> {
    result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn state_trees_are_described_validated_and_edited_with_the_graph_tools() {
    let dir = fixture();
    let overview = data(dir.path(), "graph_schema", json!({}));
    let kinds: Vec<&str> = overview["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"behavior_tree")
            && kinds.contains(&"state_tree")
            && kinds.contains(&"animation")
            && kinds.contains(&"dialogue")
    );
    assert!(kinds.windows(2).all(|pair| pair[0] < pair[1]));
    let full = data(dir.path(), "graph_schema", json!({"kind":"state_tree"}));
    assert_eq!(full["ok"], true, "{full}");
    let types: Vec<&str> = full["kinds"][0]["node_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in ["state", "transition", "wait", "compute_evaluator"] {
        assert!(types.contains(&expected), "{types:?}");
    }
    let transition = data(
        dir.path(),
        "graph_schema",
        json!({"kind":"state_tree","node_type":"transition"}),
    );
    let params: Vec<&str> = transition["kinds"][0]["node_types"][0]["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        params,
        [
            "trigger",
            "event",
            "target",
            "state",
            "priority",
            "delay",
            "delay_variance"
        ]
    );

    // Inline validation: valid, then a transition aimed at a state that is not there.
    let valid = json!({"version":1,"kind":"state_tree","nodes":[
        {"id":"root","node_type":"root"},{"id":"s","node_type":"state"}],
        "edges":[{"from":{"node":"root","port":"children"},"to":{"node":"s","port":"parent"}}]});
    let result = data(
        dir.path(),
        "graph_validate",
        json!({"document":valid.clone()}),
    );
    assert_eq!(result["valid"], true, "{result}");
    assert_eq!(result["kind"], "state_tree");
    let mut dangling = valid;
    dangling["nodes"].as_array_mut().unwrap().push(
        json!({"id":"t","node_type":"transition","params":{"target":"state","state":"ghost"}}),
    );
    dangling["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from":{"node":"s","port":"transitions"},"to":{"node":"t","port":"owner"}}));
    let result = data(dir.path(), "graph_validate", json!({"document":dangling}));
    assert_eq!(result["valid"], false);
    assert_eq!(codes(&result), ["st.transition.target_unknown"]);
    assert_eq!(result["diagnostics"][0]["path"], "/nodes/t/params/state");

    // graph_edit creates, then edits with a revision, rejecting bad batches whole.
    let created = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/mood.graph.json","kind":"state_tree","edits":build_state_tree()}),
    );
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(
        (
            created["nodes"].as_u64(),
            created["edges"].as_u64(),
            created["variables"].as_u64()
        ),
        (Some(5), Some(4), Some(1))
    );
    let revision = created["revision"].as_str().unwrap().to_owned();
    let path = dir.path().join("assets/mood.graph.json");
    let before = fs::read(&path).unwrap();
    for (edits, code) in [
        (
            json!([{"op":"set_param","node":"wake","name":"state","value":"ghost"}]),
            "st.transition.target_unknown",
        ),
        (
            json!([{"op":"set_param","node":"wake","name":"target","value":"sideways"}]),
            "graph.param.range",
        ),
        (
            // alert moves under a `self` state and nothing transitions to it any more.
            json!([
                {"op":"add_node","id":"hidden","node_type":"state","params":{"selection":"self"}},
                {"op":"disconnect","from":{"node":"root","port":"children"},"to":{"node":"alert","port":"parent"}},
                {"op":"connect","from":{"node":"root","port":"children"},"to":{"node":"hidden","port":"parent"}},
                {"op":"connect","from":{"node":"hidden","port":"children"},"to":{"node":"alert","port":"parent"}},
                {"op":"set_param","node":"wake","name":"target","value":"succeeded"},
                {"op":"set_param","node":"wake","name":"state","value":null}
            ]),
            "st.state.unreachable",
        ),
    ] {
        let rejected = write(
            dir.path(),
            "graph_edit",
            json!({"path":"assets/mood.graph.json","expected_revision":revision,"edits":edits}),
        );
        assert_eq!(error_code(&rejected), code, "{rejected}");
    }
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "rejected edits change nothing"
    );
    let edited = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/mood.graph.json","expected_revision":revision,"edits":[
            {"op":"set_param","node":"wake","name":"delay","value":0.5},
            {"op":"add_node","id":"fight","node_type":"transition",
                "params":{"trigger":"on_event","event":"enemy.seen","target":"state","state":"alert"}},
            {"op":"connect","from":{"node":"idle","port":"transitions"},"to":{"node":"fight","port":"owner"},"index":0}
        ]}),
    );
    assert_eq!(edited["ok"], true, "{edited}");
    assert_eq!(edited["created"], false);
    let document: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(document["kind"], "state_tree");
    let wake = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "wake")
        .unwrap();
    assert_eq!(wake["params"]["delay"], 0.5);
    let validated = data(
        dir.path(),
        "graph_validate",
        json!({"path":"assets/mood.graph.json"}),
    );
    assert_eq!(validated["valid"], true, "{validated}");
    assert_eq!(validated["revision"], edited["revision"]);
}

#[test]
fn state_tree_agent_references_are_validated_when_components_are_set() {
    let dir = fixture();
    let created = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/mood.graph.json","kind":"state_tree","edits":build_state_tree()}),
    );
    assert_eq!(created["ok"], true, "{created}");
    register_asset(dir.path(), "mood", "assets/mood.graph.json");
    let behavior = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/patrol.graph.json","kind":"behavior_tree","edits":behavior_edits()}),
    );
    assert_eq!(behavior["ok"], true, "{behavior}");
    register_asset(dir.path(), "patrol", "assets/patrol.graph.json");
    let set = |graph: &str, extra: Value| {
        let mut data = json!({"graph":graph,"blackboard":{"danger":true},"tick_interval":0.25});
        data.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
                "component":"engine.StateTreeAgent","value":{"version":1,"data":data}}),
        )
    };
    let ok = set("mood", json!({}));
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(
        ok["entity"]["components"]["custom"]["engine.StateTreeAgent"]["data"]["graph"],
        "mood"
    );
    assert_eq!(
        error_code(&set("missing", json!({}))),
        "project.asset.unknown"
    );
    assert_eq!(
        error_code(&set("asset:mesh", json!({}))),
        "project.asset.kind"
    );
    assert_eq!(
        error_code(&set("patrol", json!({}))),
        "state_tree.graph.kind",
        "a behavior tree cannot back a state tree agent"
    );
    assert_eq!(
        error_code(&set("mood", json!({"blackboard":{"nope":1}}))),
        "state_tree.blackboard.invalid"
    );
    assert_eq!(
        error_code(&set("mood", json!({"tick_interval":-1}))),
        "component.schema.invalid"
    );
    let info = data(dir.path(), "asset_info", json!({"asset":"mood"}));
    assert_eq!(info["used_by"][0]["component"], "engine.StateTreeAgent");
    assert_eq!(info["used_by"][0]["field"], "graph");
    assert_eq!(
        info["details"],
        json!({"kind":"state_tree","nodes":5,"edges":4,"variables":1}),
        "{info}"
    );
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.StateTreeAgent"}),
    );
    assert_eq!(schema["components"][0]["example"]["data"]["enabled"], true);
}
