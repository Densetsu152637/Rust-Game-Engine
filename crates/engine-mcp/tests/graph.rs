//! `graph_schema`, `graph_validate` and `graph_edit`: graph authoring over MCP.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

/// Edits that build a complete two-node behavior tree.
fn build_tree() -> Value {
    json!([
        {"op":"add_node","id":"root","node_type":"root","position":[0.0,0.0]},
        {"op":"add_node","id":"seq","node_type":"sequence"},
        {"op":"add_node","id":"wait","node_type":"wait","params":{"seconds":1.5}},
        {"op":"add_variable","variable":{"name":"alarm","type":"bool","default":false}},
        {"op":"connect","from":{"node":"root","port":"child"},"to":{"node":"seq","port":"parent"}},
        {"op":"connect","from":{"node":"seq","port":"children"},"to":{"node":"wait","port":"parent"}}
    ])
}

fn create(dir: &Path) -> Value {
    write(
        dir,
        "graph_edit",
        json!({"path":"assets/patrol.graph.json","kind":"behavior_tree","edits":build_tree()}),
    )
}

fn register(dir: &Path) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"patrol","kind":"graph","path":"assets/patrol.graph.json"}));
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
fn graph_schema_lists_kinds_and_describes_node_types() {
    let dir = fixture();
    let overview = data(dir.path(), "graph_schema", json!({}));
    assert_eq!(overview["ok"], true, "{overview}");
    assert_eq!(overview["document_version"], 1);
    let kinds = overview["kinds"].as_array().unwrap();
    let tree = kinds.iter().find(|k| k["name"] == "behavior_tree").unwrap();
    assert_eq!(tree["single_root"], true);
    assert!(
        tree["node_types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "blackboard_condition")
    );
    assert!(
        tree["node_types"][0].get("ports").is_none(),
        "the overview is brief"
    );
    assert_eq!(overview["edit_operations"].as_array().unwrap().len(), 10);
    assert!(
        overview["diagnostic_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "graph.cycle")
    );

    let full = data(dir.path(), "graph_schema", json!({"kind":"behavior_tree"}));
    let selector = full["kinds"][0]["node_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "sequence")
        .unwrap();
    let children = selector["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "children")
        .unwrap();
    assert_eq!(children["direction"], "out");
    assert_eq!(children["type"], "flow");
    assert_eq!(children["multiple"], true);

    let one = data(
        dir.path(),
        "graph_schema",
        json!({"kind":"behavior_tree","node_type":"cooldown"}),
    );
    let types = one["kinds"][0]["node_types"].as_array().unwrap();
    assert_eq!(types.len(), 1);
    let duration = types[0]["params"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "duration")
        .unwrap();
    assert_eq!(duration["type"], "float");
    assert_eq!(duration["max"], 3600.0);
    assert_eq!(duration["default"], 1.0);

    assert_eq!(
        error_code(&data(dir.path(), "graph_schema", json!({"kind":"nope"}))),
        "graph.kind.unknown"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "graph_schema",
            json!({"kind":"behavior_tree","node_type":"nope"})
        )),
        "graph.node.unknown_type"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "graph_schema",
            json!({"node_type":"wait"})
        )),
        "invalid_argument"
    );
}

#[test]
fn graph_validate_reports_coded_diagnostics_for_inline_documents_assets_and_files() {
    let dir = fixture();
    let valid = json!({"version":1,"kind":"behavior_tree","nodes":[
        {"id":"root","node_type":"root"},{"id":"w","node_type":"wait"}],
        "edges":[{"from":{"node":"root","port":"child"},"to":{"node":"w","port":"parent"}}]});
    let result = data(dir.path(), "graph_validate", json!({"document":valid}));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["valid"], true);
    assert_eq!(result["kind"], "behavior_tree");
    assert_eq!(
        (result["nodes"].as_u64(), result["edges"].as_u64()),
        (Some(2), Some(1))
    );

    let invalid = json!({"version":1,"kind":"behavior_tree","nodes":[
        {"id":"root","node_type":"root"},{"id":"w","node_type":"wait","params":{"seconds":-2}},
        {"id":"orphan","node_type":"wait"}],
        "edges":[{"from":{"node":"root","port":"child"},"to":{"node":"w","port":"parent"}}]});
    let result = data(
        dir.path(),
        "graph_validate",
        json!({"document":invalid.clone()}),
    );
    assert_eq!(result["ok"], true, "validation reports problems as data");
    assert_eq!(result["valid"], false);
    assert_eq!(
        codes(&result),
        [
            "graph.param.range",
            "graph.port.unconnected",
            "graph.node.unreachable"
        ]
    );
    assert_eq!(result["diagnostics"][0]["path"], "/nodes/w/params/seconds");
    // The structure level ignores completeness rules, not bad values.
    let structure = data(
        dir.path(),
        "graph_validate",
        json!({"document":invalid,"level":"structure"}),
    );
    assert_eq!(codes(&structure), ["graph.param.range"]);

    // Files: by path and by asset id, with a revision for graph_edit.
    assert_eq!(create(dir.path())["ok"], true);
    register(dir.path());
    let by_path = data(
        dir.path(),
        "graph_validate",
        json!({"path":"assets/patrol.graph.json"}),
    );
    assert_eq!(by_path["valid"], true, "{by_path}");
    assert_eq!(by_path["path"], "assets/patrol.graph.json");
    let by_asset = data(dir.path(), "graph_validate", json!({"asset":"patrol"}));
    assert_eq!(by_asset["revision"], by_path["revision"]);

    // A broken file is reported, not hidden by a project load failure.
    fs::write(dir.path().join("assets/patrol.graph.json"), "{not json").unwrap();
    let broken = data(dir.path(), "graph_validate", json!({"asset":"patrol"}));
    assert_eq!(broken["ok"], true);
    assert_eq!(broken["valid"], false);
    assert_eq!(codes(&broken), ["graph.document.malformed"]);
    assert_eq!(broken["diagnostics"][0]["file"], "assets/patrol.graph.json");

    // Exactly one source; unknown assets fail properly.
    assert_eq!(
        error_code(&data(dir.path(), "graph_validate", json!({}))),
        "invalid_argument"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "graph_validate",
            json!({"asset":"patrol","path":"assets/patrol.graph.json"})
        )),
        "invalid_argument"
    );
    assert_eq!(
        error_code(&data(dir.path(), "graph_validate", json!({"asset":"nope"}))),
        "asset_not_found"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "graph_validate",
            json!({"asset":"asset:mesh"})
        )),
        "asset_kind"
    );
    let unknown_kind = data(
        dir.path(),
        "graph_validate",
        json!({"document":{"version":1,"kind":"nope"}}),
    );
    assert_eq!(codes(&unknown_kind), ["graph.kind.unknown"]);
}

#[test]
fn graph_edit_creates_a_graph_that_the_project_accepts() {
    let dir = fixture();
    let created = create(dir.path());
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(created["applied"], 6);
    assert_eq!(
        (
            created["nodes"].as_u64(),
            created["edges"].as_u64(),
            created["variables"].as_u64()
        ),
        (Some(3), Some(2), Some(1))
    );
    assert_eq!(created["asset"]["kind"], "graph");
    let revision = created["revision"].as_str().unwrap().to_owned();

    register(dir.path());
    assert!(engine_project::Project::load(dir.path().join("project.json")).is_ok());
    let listed = data(dir.path(), "asset_list", json!({"kind":"graph"}));
    assert_eq!(listed["count"], 1, "{listed}");
    let info = data(dir.path(), "asset_info", json!({"asset":"patrol"}));
    assert_eq!(
        info["details"],
        json!({"kind":"behavior_tree","nodes":3,"edges":2,"variables":1}),
        "{info}"
    );

    // Edit the existing graph with the revision.
    let edited = write(
        dir.path(),
        "graph_edit",
        json!({"asset":"patrol","expected_revision":revision,"edits":[
            {"op":"set_param","node":"wait","name":"seconds","value":2.5},
            {"op":"add_node","id":"hello","node_type":"emit_event","params":{"event":"guard.hello"}},
            {"op":"connect","from":{"node":"seq","port":"children"},"to":{"node":"hello","port":"parent"},"index":0}
        ]}),
    );
    assert_eq!(edited["ok"], true, "{edited}");
    assert_eq!(edited["created"], false);
    assert_eq!(edited["previous_revision"], revision);
    assert_ne!(edited["revision"], revision);
    assert!(edited.get("asset").is_none());
    let text = fs::read_to_string(dir.path().join("assets/patrol.graph.json")).unwrap();
    let document: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(document["nodes"][2]["params"]["seconds"], 2.5);
    // index 0 put the new child first.
    assert_eq!(document["edges"][1]["to"]["node"], "hello");
    let validated = data(dir.path(), "graph_validate", json!({"asset":"patrol"}));
    assert_eq!(validated["revision"], edited["revision"]);
    assert_eq!(validated["valid"], true);
}

#[test]
fn graph_edit_rejects_invalid_edits_without_touching_the_file() {
    let dir = fixture();
    let revision = create(dir.path())["revision"].as_str().unwrap().to_owned();
    let path = dir.path().join("assets/patrol.graph.json");
    let before = fs::read(&path).unwrap();
    let attempts = [
        (
            json!([{"op":"add_node","id":"x","node_type":"wait","params":{"seconds":-1}}]),
            "graph.param.range",
            Some(0),
        ),
        (
            json!([
                {"op":"add_node","id":"x","node_type":"wait"},
                {"op":"connect","from":{"node":"seq","port":"children"},"to":{"node":"x","port":"nope"}}
            ]),
            "graph.edge.unknown_port",
            Some(1),
        ),
        (
            json!([{"op":"connect","from":{"node":"wait","port":"parent"},"to":{"node":"seq","port":"parent"}}]),
            "graph.edge.direction",
            Some(0),
        ),
        (
            json!([{"op":"remove_node","id":"ghost"}]),
            "graph.edit.node_missing",
            Some(0),
        ),
        (
            json!([{"op":"add_node","id":"x","node_type":"unheard_of"}]),
            "graph.node.unknown_type",
            Some(0),
        ),
        // Operations that are fine one by one but leave the graph incomplete.
        (
            json!([{"op":"add_node","id":"stray","node_type":"wait"}]),
            "graph.port.unconnected",
            None,
        ),
        (
            json!([{"op":"remove_node","id":"wait"},{"op":"add_node","id":"e","node_type":"sequence"},
                {"op":"connect","from":{"node":"seq","port":"children"},"to":{"node":"e","port":"parent"}}]),
            "bt.composite.empty",
            None,
        ),
    ];
    for (edits, code, op_index) in attempts {
        let result = write(
            dir.path(),
            "graph_edit",
            json!({"path":"assets/patrol.graph.json","expected_revision":revision,"edits":edits}),
        );
        assert_eq!(error_code(&result), code, "{result}");
        assert_eq!(
            result["error"]["details"]["op_index"],
            json!(op_index),
            "{result}"
        );
        assert_eq!(result["error"]["details"]["written"], false);
        assert!(!result["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "{code} must not change the file"
        );
    }
}

#[test]
fn graph_edit_checks_the_revision() {
    let dir = fixture();
    let revision = create(dir.path())["revision"].as_str().unwrap().to_owned();
    let edit = json!([{"op":"set_metadata","key":"note","value":"hi"}]);
    // Missing revision.
    let result = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/patrol.graph.json","edits":edit}),
    );
    assert_eq!(error_code(&result), "hash_required");
    // Stale revision after another writer changed the file.
    let first = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/patrol.graph.json","expected_revision":revision,"edits":edit}),
    );
    assert_eq!(first["ok"], true, "{first}");
    let stale = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/patrol.graph.json","expected_revision":revision,
            "edits":[{"op":"set_metadata","key":"note","value":"again"}]}),
    );
    assert_eq!(error_code(&stale), "revision_conflict");
    let document: Value =
        serde_json::from_slice(&fs::read(dir.path().join("assets/patrol.graph.json")).unwrap())
            .unwrap();
    assert_eq!(
        document["metadata"]["note"], "hi",
        "the stale edit was not applied"
    );
    // A revision for a file that does not exist.
    let result = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/new.graph.json","kind":"behavior_tree","expected_revision":"abc","edits":build_tree()}),
    );
    assert_eq!(error_code(&result), "file_not_found");
}

#[test]
fn graph_edit_is_write_gated_and_confined() {
    let dir = fixture();
    let args =
        json!({"path":"assets/patrol.graph.json","kind":"behavior_tree","edits":build_tree()});
    let gated = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"graph_edit","arguments":args}),
        )],
    );
    assert_eq!(gated[0]["error"]["code"], -32602, "{}", gated[0]);
    assert!(!dir.path().join("assets/patrol.graph.json").exists());

    for path in [
        "../escape.json",
        ".git/graph.json",
        "target/graph.json",
        "C:/graph.json",
        "assets/notes.txt",
        "missing/dir/graph.json",
        "assets/white.ppm",
    ] {
        let result = write(
            dir.path(),
            "graph_edit",
            json!({"path":path,"kind":"behavior_tree","edits":build_tree()}),
        );
        assert_eq!(result["ok"], false, "{path} should be refused: {result}");
    }
    assert!(!dir.path().join("assets/notes.txt").exists());
    // New files need a kind; kinds must exist; asset and path are exclusive.
    for (args, code) in [
        (
            json!({"path":"assets/a.json","edits":build_tree()}),
            "invalid_argument",
        ),
        (
            json!({"path":"assets/a.json","kind":"nope","edits":build_tree()}),
            "graph.kind.unknown",
        ),
        (
            json!({"asset":"x","path":"assets/a.json","edits":build_tree()}),
            "invalid_argument",
        ),
        (json!({"edits":build_tree()}), "invalid_argument"),
        (
            json!({"path":"assets/a.json","kind":"behavior_tree","edits":[]}),
            "invalid_argument",
        ),
        (
            json!({"path":"assets/a.json","kind":"behavior_tree","edits":[{"op":"explode"}]}),
            "invalid_argument",
        ),
        (
            json!({"path":"assets/a.json","kind":"behavior_tree","edits":[{"op":"remove_node","id":"a","extra":1}]}),
            "invalid_argument",
        ),
        (
            json!({"asset":"asset:mesh","edits":build_tree()}),
            "asset_kind",
        ),
        (
            json!({"asset":"missing","edits":build_tree()}),
            "asset_not_found",
        ),
    ] {
        let result = write(dir.path(), "graph_edit", args.clone());
        assert_eq!(error_code(&result), code, "{args}: {result}");
    }
    assert!(!dir.path().join("assets/a.json").exists());
    // A new file never replaces an existing one without a revision.
    create(dir.path());
    let again = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/patrol.graph.json","kind":"behavior_tree","edits":build_tree()}),
    );
    assert_eq!(error_code(&again), "hash_required");
}

#[test]
fn graph_references_are_validated_when_components_are_set() {
    let dir = fixture();
    create(dir.path());
    register(dir.path());
    let set = |graph: &str| {
        write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
                "component":"engine.BehaviorTreeAgent",
                "value":{"version":1,"data":{"graph":graph,"blackboard":{"alarm":true},"tick_interval":0.25}}}),
        )
    };
    let ok = set("patrol");
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(
        ok["entity"]["components"]["custom"]["engine.BehaviorTreeAgent"]["data"]["graph"],
        "patrol"
    );
    assert_eq!(error_code(&set("missing")), "project.asset.unknown");
    assert_eq!(error_code(&set("asset:mesh")), "project.asset.kind");
    let info = data(dir.path(), "asset_info", json!({"asset":"patrol"}));
    assert_eq!(info["used_by"][0]["component"], "engine.BehaviorTreeAgent");
    assert_eq!(info["used_by"][0]["field"], "graph");
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.BehaviorTreeAgent"}),
    );
    assert_eq!(schema["components"][0]["example"]["data"]["enabled"], true);
}

#[test]
fn animation_palette_uses_shared_graph_schema() {
    let dir = fixture();
    let schema = data(dir.path(), "graph_schema", json!({"kind":"animation"}));
    assert_eq!(schema["ok"], true, "{schema}");
    let types = schema["kinds"][0]["node_types"].as_array().unwrap();
    for name in [
        "clip",
        "blend",
        "blend1d",
        "blend2d",
        "state_machine",
        "output",
    ] {
        assert!(types.iter().any(|node| node["name"] == name));
    }
    let blend = types.iter().find(|node| node["name"] == "blend").unwrap();
    assert!(
        blend["ports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|port| port["name"] == "a")
    );
}

#[test]
fn audio_palette_and_emitter_references_share_project_registration() {
    let dir = fixture();
    let schema = data(dir.path(), "graph_schema", json!({"kind":"audio"}));
    assert_eq!(schema["ok"], true, "{schema}");
    let types = schema["kinds"][0]["node_types"].as_array().unwrap();
    for name in [
        "constant",
        "parameter",
        "oscillator",
        "noise",
        "mix",
        "gain",
        "adsr",
        "lowpass",
        "delay",
        "output",
    ] {
        assert!(
            types.iter().any(|node| node["name"] == name),
            "{name}: {schema}"
        );
    }
    let created = write(
        dir.path(),
        "graph_edit",
        json!({"path":"assets/tone.graph.json","kind":"audio","edits":[
            {"op":"add_node","id":"tone","node_type":"constant","params":{"value":0.25}},
            {"op":"add_node","id":"out","node_type":"output"},
            {"op":"connect","from":{"node":"tone","port":"signal"},"to":{"node":"out","port":"left"}},
            {"op":"connect","from":{"node":"tone","port":"signal"},"to":{"node":"out","port":"right"}}
        ]}),
    );
    assert_eq!(created["ok"], true, "{created}");
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&38u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&2u32.to_le_bytes());
    wav.extend_from_slice(&0i16.to_le_bytes());
    fs::write(dir.path().join("assets/tone.wav"), wav).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    manifest["assets"].as_array_mut().unwrap().extend([
        json!({"id":"tone-graph","kind":"graph","path":"assets/tone.graph.json"}),
        json!({"id":"tone-clip","kind":"audio","path":"assets/tone.wav"}),
    ]);
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    let set = |value: Value| {
        write(
            dir.path(),
            "component_set",
            json!({
                "scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
                "component":"engine.AudioEmitter","value":{"version":1,"data":value}
            }),
        )
    };
    for (field, id, value) in [
        (
            "clip",
            "tone-clip",
            json!({"clip":"tone-clip","streamed":true,"autoplay":false}),
        ),
        (
            "graph",
            "tone-graph",
            json!({"graph":"tone-graph","autoplay":false}),
        ),
    ] {
        let result = set(value);
        assert_eq!(result["ok"], true, "{result}");
        let info = data(dir.path(), "asset_info", json!({"asset":id}));
        assert_eq!(
            info["used_by"][0]["component"], "engine.AudioEmitter",
            "{info}"
        );
        assert_eq!(info["used_by"][0]["field"], field, "{info}");
    }
    let clip = data(dir.path(), "asset_info", json!({"asset":"tone-clip"}));
    assert_eq!(
        clip["used_by"],
        json!([]),
        "replacing the reference removes old uses: {clip}"
    );
    let empty = set(json!({}));
    assert_eq!(empty["ok"], true, "empty authoring defaults: {empty}");
    let graph = data(dir.path(), "asset_info", json!({"asset":"tone-graph"}));
    assert_eq!(
        graph["used_by"],
        json!([]),
        "empty defaults do not create uses: {graph}"
    );
}
