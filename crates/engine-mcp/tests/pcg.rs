//! `pcg_create`, `pcg_edit`, `pcg_validate`, `pcg_run` and the `engine.PcgVolume` component
//! through `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const START: &str = "scenes/start.json";
const GRAPH: &str = "assets/trees.pcg.json";

fn pcg_fixture() -> tempfile::TempDir {
    let dir = fixture();
    let created = write(
        dir.path(),
        "pcg_create",
        json!({"output": GRAPH, "seed": 11, "template": "scatter", "assets": ["asset:mesh"]}),
    );
    assert_eq!(created["ok"], true, "{created}");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(created["asset"].clone());
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["id"] = json!("trees");
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    dir
}

fn hash(root: &Path) -> String {
    data(root, "pcg_validate", json!({"path": GRAPH}))["hash"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn summary(root: &Path, extra: Value) -> Value {
    let mut args = json!({"path": GRAPH, "run": true, "size": [30, 20, 30]});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let result = data(root, "pcg_validate", args);
    assert_eq!(result["ok"], true, "{result}");
    result["run"].clone()
}

#[test]
fn create_writes_a_valid_graph_and_never_overwrites() {
    let dir = pcg_fixture();
    let bytes = fs::read(dir.path().join(GRAPH)).unwrap();
    let graph = engine_pcg::Graph::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(graph.seed, 11);
    assert_eq!(graph.nodes.len(), 3);
    assert!(engine_pcg::validate(&graph, &engine_pcg::Registry::builtin()).is_empty());

    let again = write(dir.path(), "pcg_create", json!({"output": GRAPH}));
    assert_eq!(error_code(&again), "invalid_argument");
    assert_eq!(fs::read(dir.path().join(GRAPH)).unwrap(), bytes);
    let empty = write(
        dir.path(),
        "pcg_create",
        json!({"output": "assets/empty.pcg.json"}),
    );
    assert_eq!(empty["nodes"], 0, "{empty}");
    for bad in [
        json!({"output": "assets/a.pcg.json", "template": "scatter"}),
        json!({"output": "assets/a.pcg.json", "template": "scatter", "assets": ["missing"]}),
        json!({"output": "assets/a.pcg.json", "template": "scatter", "assets": ["asset:texture"]}),
        json!({"output": "../escape.json"}),
        json!({"output": "assets/a.txt"}),
    ] {
        let failed = write(dir.path(), "pcg_create", bad.clone());
        assert_eq!(failed["ok"], false, "{bad}");
        assert!(!dir.path().join("assets/a.pcg.json").exists());
    }
}

#[test]
fn registered_pcg_assets_are_described_by_asset_info() {
    let dir = pcg_fixture();
    let info = data(dir.path(), "asset_info", json!({"asset": "trees"}));
    assert_eq!(info["details"]["nodes"], 3, "{info}");
    assert_eq!(info["details"]["seed"], 11);
    let listed = data(dir.path(), "asset_list", json!({"kind": "pcg"}));
    assert_eq!(listed["count"], 1, "{listed}");
}

#[test]
fn validate_reports_catalog_and_a_deterministic_bounded_run() {
    let dir = pcg_fixture();
    let report = data(
        dir.path(),
        "pcg_validate",
        json!({"path": GRAPH, "kinds": true}),
    );
    assert_eq!(report["valid"], true, "{report}");
    let kinds: Vec<&str> = report["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kind"].as_str().unwrap())
        .collect();
    for kind in [
        "grid_sampler",
        "random_sampler",
        "poisson_sampler",
        "noise_field",
        "density_filter",
        "height_filter",
        "slope_filter",
        "bounds_filter",
        "self_prune",
        "transform_points",
        "merge",
        "difference",
        "spawner",
    ] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }

    let first = summary(dir.path(), json!({"seed": 4}));
    let second = summary(dir.path(), json!({"seed": 4}));
    let other = summary(dir.path(), json!({"seed": 5}));
    assert_eq!(first, second, "same inputs, same output");
    assert_ne!(first["hash"], other["hash"]);
    let count = first["instances"].as_u64().unwrap();
    // Poisson disk, radius 3 over 30 x 30 m: at least 30 * 30 / 3^2 * 0.5 points.
    assert!(count > 50 && count < 400, "{count} instances");
    assert_eq!(first["by_asset"]["asset:mesh"], count);
    assert_eq!(first["sample"].as_array().unwrap().len(), 8);
    let bounds = &first["bounds"];
    for axis in [0, 2] {
        assert!(bounds["min"][axis].as_f64().unwrap() >= -15.0);
        assert!(bounds["max"][axis].as_f64().unwrap() <= 15.0);
    }
    // The cap is enforced, not silently truncated.
    let capped = data(
        dir.path(),
        "pcg_validate",
        json!({"path": GRAPH, "run": true, "size": [30, 20, 30], "max_instances": 10}),
    );
    assert_eq!(error_code(&capped), "pcg_run_failed");
    assert_eq!(
        capped["error"]["details"]["problems"][0]["code"],
        "pcg.limit.instances"
    );
    // Ground height shifts the sampled y.
    let raised = summary(
        dir.path(),
        json!({"seed": 4, "ground_height": 5, "sample": 1}),
    );
    assert!((raised["sample"][0]["position"][1].as_f64().unwrap() - 5.0).abs() < 1e-4);
}

#[test]
fn edit_applies_operations_atomically_and_checks_the_hash() {
    let dir = pcg_fixture();
    let before = fs::read(dir.path().join(GRAPH)).unwrap();
    let original = hash(dir.path());
    let dense = summary(dir.path(), json!({"seed": 1}))["instances"]
        .as_u64()
        .unwrap();

    let edited = write(
        dir.path(),
        "pcg_edit",
        json!({"path": GRAPH, "expected_hash": original, "operations": [
            {"op": "set_parameter", "node": "points", "name": "min_distance", "value": 6.0},
            {"op": "add_node", "id": "low", "kind": "height_filter", "params": {"max": 1.0}},
            {"op": "disconnect", "from": {"node": "variation", "port": "points"}, "to": {"node": "spawn", "port": "points"}},
            {"op": "connect", "from": {"node": "variation", "port": "points"}, "to": {"node": "low", "port": "points"}},
            {"op": "connect", "from": {"node": "low", "port": "points"}, "to": {"node": "spawn", "port": "points"}},
        ]}),
    );
    assert_eq!(edited["ok"], true, "{edited}");
    assert_eq!(edited["saved"], true);
    assert_eq!(edited["problems"], json!([]));
    assert_eq!(edited["nodes"], 4);
    assert_eq!(edited["previous_hash"], original);
    assert_eq!(edited["hash"], hash(dir.path()));
    let sparse = summary(dir.path(), json!({"seed": 1}))["instances"]
        .as_u64()
        .unwrap();
    assert!(
        sparse < dense,
        "radius 6 packs fewer points than radius 3: {sparse} vs {dense}"
    );

    // Stale hash.
    let stale = write(
        dir.path(),
        "pcg_edit",
        json!({"path": GRAPH, "expected_hash": original, "operations": []}),
    );
    assert_eq!(error_code(&stale), "revision_conflict");

    // A failing operation anywhere leaves the file untouched.
    let current = fs::read(dir.path().join(GRAPH)).unwrap();
    assert_ne!(current, before);
    let rejected = write(
        dir.path(),
        "pcg_edit",
        json!({"path": GRAPH, "expected_hash": hash(dir.path()), "operations": [
            {"op": "set_seed", "seed": 99},
            {"op": "set_parameter", "node": "points", "name": "min_distance", "value": -1.0},
        ]}),
    );
    assert_eq!(error_code(&rejected), "pcg_edit_rejected");
    assert_eq!(rejected["error"]["details"]["operation_index"], 1);
    assert_eq!(
        rejected["error"]["details"]["problem"]["code"],
        "pcg.param.invalid"
    );
    assert_eq!(fs::read(dir.path().join(GRAPH)).unwrap(), current);
    for (operation, code) in [
        (
            json!({"op": "connect", "from": {"node": "spawn", "port": "instances"}, "to": {"node": "low", "port": "points"}}),
            "pcg.edge.type_mismatch",
        ),
        (
            json!({"op": "connect", "from": {"node": "spawn", "port": "instances"}, "to": {"node": "nobody", "port": "points"}}),
            "pcg.edge.unknown_node",
        ),
        (
            json!({"op": "add_node", "id": "x", "kind": "warp"}),
            "pcg.node.unknown_kind",
        ),
        (
            json!({"op": "add_node", "id": "points", "kind": "merge"}),
            "pcg.node.duplicate_id",
        ),
        (
            json!({"op": "remove_node", "id": "nobody"}),
            "pcg.edit.unknown_node",
        ),
        (
            json!({"op": "connect", "from": {"node": "spawn", "port": "instances"}, "to": {"node": "points", "port": "nothing"}}),
            "pcg.edge.unknown_port",
        ),
    ] {
        let failed = write(
            dir.path(),
            "pcg_edit",
            json!({"path": GRAPH, "expected_hash": hash(dir.path()), "operations": [operation]}),
        );
        assert_eq!(error_code(&failed), "pcg_edit_rejected", "{failed}");
        assert_eq!(
            failed["error"]["details"]["problem"]["code"], code,
            "{failed}"
        );
    }
    let malformed = write(
        dir.path(),
        "pcg_edit",
        json!({"path": GRAPH, "expected_hash": hash(dir.path()), "operations": [{"op": "explode"}]}),
    );
    assert_eq!(error_code(&malformed), "pcg_operation_invalid");
    assert_eq!(fs::read(dir.path().join(GRAPH)).unwrap(), current);

    // Dry run reports without saving; incomplete graphs are allowed with listed problems.
    let dry = write(
        dir.path(),
        "pcg_edit",
        json!({"path": GRAPH, "expected_hash": hash(dir.path()), "dry_run": true,
            "operations": [{"op": "remove_node", "id": "low"}]}),
    );
    assert_eq!(dry["saved"], false, "{dry}");
    assert!(
        dry["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "pcg.input.missing")
    );
    assert_eq!(fs::read(dir.path().join(GRAPH)).unwrap(), current);
}

#[test]
fn validate_lists_precise_problems_and_run_refuses_invalid_graphs() {
    let dir = pcg_fixture();
    fs::write(
        dir.path().join("assets/bad.pcg.json"),
        json!({"nodes": [
            {"id": "a", "kind": "self_prune"},
            {"id": "b", "kind": "self_prune", "params": {"min_distance": 0}},
            {"id": "c", "kind": "nope"}],
          "edges": [
            {"from": {"node": "a", "port": "points"}, "to": {"node": "b", "port": "points"}},
            {"from": {"node": "b", "port": "points"}, "to": {"node": "a", "port": "points"}}]})
        .to_string(),
    )
    .unwrap();
    let report = data(
        dir.path(),
        "pcg_validate",
        json!({"path": "assets/bad.pcg.json"}),
    );
    assert_eq!(report["valid"], false);
    let codes: Vec<&str> = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["code"].as_str().unwrap())
        .collect();
    for code in [
        "pcg.param.invalid",
        "pcg.node.unknown_kind",
        "pcg.graph.cycle",
    ] {
        assert!(codes.contains(&code), "{code} in {codes:?}");
    }
    let invalid_param = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["code"] == "pcg.param.invalid")
        .unwrap();
    assert_eq!(invalid_param["node"], "b");
    let refused = data(
        dir.path(),
        "pcg_validate",
        json!({"path": "assets/bad.pcg.json", "run": true}),
    );
    assert_eq!(error_code(&refused), "pcg_invalid");
    let run = write(
        dir.path(),
        "pcg_run",
        json!({"path": "assets/bad.pcg.json"}),
    );
    assert_eq!(error_code(&run), "pcg_invalid");
    fs::write(dir.path().join("assets/old.pcg.json"), r#"{"version": 7}"#).unwrap();
    let old = data(
        dir.path(),
        "pcg_validate",
        json!({"path": "assets/old.pcg.json"}),
    );
    assert_eq!(error_code(&old), "pcg_parse");
}

#[test]
fn run_writes_replaceable_entities_into_a_scene_in_one_revisioned_edit() {
    let dir = pcg_fixture();
    let before_entities = engine_project::Project::load(dir.path().join("project.json"))
        .unwrap()
        .scene
        .entities
        .len() as u64;
    let args = |seed: u64, revision: String| {
        json!({"path": GRAPH, "scene": START, "expected_revision": revision, "volume": "forest",
            "seed": seed, "size": [30, 20, 30]})
    };
    let first = write(dir.path(), "pcg_run", args(1, revision(dir.path(), START)));
    assert_eq!(first["ok"], true, "{first}");
    let written = first["written"].as_u64().unwrap();
    assert_eq!(written, first["run"]["instances"].as_u64().unwrap());
    assert_eq!(first["removed"], 0);
    assert!(written > 50);

    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    let scene = &project.scene;
    assert_eq!(scene.entities.len() as u64, before_entities + written);
    let tree = scene
        .entities
        .iter()
        .find(|e| e.id == "pcg:forest:0")
        .unwrap();
    assert_eq!(
        tree.components.mesh_renderer.as_ref().unwrap().mesh,
        "asset:mesh"
    );
    assert!(tree.components.transform.is_some());

    // Re-running replaces exactly the previous output.
    let second = write(dir.path(), "pcg_run", args(2, revision(dir.path(), START)));
    assert_eq!(second["removed"], written, "{second}");
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    let now = second["written"].as_u64().unwrap();
    assert_eq!(project.scene.entities.len() as u64, before_entities + now);

    // Stale revision, missing revision and dry runs change nothing.
    let on_disk = fs::read(dir.path().join(START)).unwrap();
    let stale = write(dir.path(), "pcg_run", args(3, "stale".into()));
    assert_eq!(error_code(&stale), "revision_conflict");
    let missing = write(
        dir.path(),
        "pcg_run",
        json!({"path": GRAPH, "scene": START}),
    );
    assert_eq!(error_code(&missing), "invalid_argument");
    let dry = {
        let mut a = args(3, revision(dir.path(), START));
        a["dry_run"] = json!(true);
        write(dir.path(), "pcg_run", a)
    };
    assert_eq!(dry["saved"], false, "{dry}");
    assert_eq!(fs::read(dir.path().join(START)).unwrap(), on_disk);
    // Summary-only runs touch no scene.
    let preview = write(dir.path(), "pcg_run", json!({"path": GRAPH, "seed": 9}));
    assert!(preview.get("scene").is_none() && preview["run"]["instances"].as_u64().unwrap() > 0);
    assert_eq!(fs::read(dir.path().join(START)).unwrap(), on_disk);
    // Bad volume ids are refused.
    let bad = write(
        dir.path(),
        "pcg_run",
        json!({"path": GRAPH, "volume": "no spaces"}),
    );
    assert_eq!(error_code(&bad), "invalid_argument");
}

#[test]
fn write_tools_are_hidden_from_read_only_servers_but_validate_is_not() {
    let dir = pcg_fixture();
    let names: Vec<String> = {
        let responses = exchange(
            dir.path(),
            read_only(),
            &[modern(1, "tools/list", json!({}))],
        );
        responses[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(names.contains(&"pcg_validate".to_owned()));
    for tool in ["pcg_create", "pcg_edit", "pcg_run"] {
        assert!(!names.contains(&tool.to_owned()), "{tool}");
    }
    // Every tool schema is closed, described and has an output schema.
    let responses = exchange(
        dir.path(),
        writable(),
        &[modern(1, "tools/list", json!({}))],
    );
    for tool in responses[0]["result"]["tools"].as_array().unwrap() {
        if tool["name"].as_str().unwrap().starts_with("pcg_") {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(tool["outputSchema"].is_object());
            assert!(tool["description"].as_str().unwrap().len() > 80);
        }
    }
}

#[test]
fn component_set_edits_a_pcg_volume_and_rejects_bad_values() {
    let dir = pcg_fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component": "engine.PcgVolume"}),
    );
    let component = &schema["components"][0];
    assert_eq!(component["name"], "engine.PcgVolume", "{schema}");
    assert_eq!(component["requires"], json!(["Transform"]));
    assert_eq!(component["schema"]["additionalProperties"], false);

    let set = |value: Value| {
        write(
            dir.path(),
            "component_set",
            json!({"scene": START, "expected_revision": revision(dir.path(), START),
                "entity": "player", "component": "engine.PcgVolume", "value": value}),
        )
    };
    let ok =
        set(json!({"version": 1, "data": {"graph": "trees", "seed": 3, "size": [10, 10, 10]}}));
    assert_eq!(ok["ok"], true, "{ok}");
    let stored = &ok["entity"]["components"]["custom"]["engine.PcgVolume"]["data"];
    assert_eq!(stored["graph"], "trees");
    assert_eq!(stored["seed"], 3);
    for bad in [
        json!({"version": 1, "data": {"graph": "missing"}}),
        json!({"version": 1, "data": {"graph": "asset:mesh"}}),
        json!({"version": 1, "data": {"size": [0, 1, 1]}}),
        json!({"version": 1, "data": {"mode": "never"}}),
        json!({"version": 1, "data": {"unknown": true}}),
        json!({"version": 1, "data": {"seed": -5}}),
    ] {
        assert_eq!(set(bad.clone())["ok"], false, "{bad}");
    }
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    let player = project
        .scene
        .entities
        .iter()
        .find(|e| e.id == "player")
        .unwrap();
    assert_eq!(
        player.components.custom["engine.PcgVolume"].data["graph"],
        "trees"
    );
}
