//! Design record test 22: `acoustics_inspect`, `acoustics_query` and
//! `acoustics_bake` on the acoustics showcase example.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

/// A copy of `examples/acoustics-showcase` in a temporary project root.
fn showcase() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/acoustics-showcase");
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
        }
    }
    dir
}

fn query_args() -> Value {
    json!({
        "scene": "room.scene.json",
        "listener": {"position": [-3, 1.6, 0]},
        "sources": [
            {"position": [3, 1.2, 0]},
            {"position": [3, 1.2, 2.5]},
        ],
    })
}

#[test]
fn inspect_counts_the_showcase_world_and_its_proxy() {
    let dir = showcase();
    let room = data(dir.path(), "acoustics_inspect", json!({}));
    assert_eq!(room["ok"], true, "{room}");
    assert_eq!(room["scene"], "room");
    assert_eq!(room["environment"]["backend"], "auto");
    // The room shell (12 triangles) and the divider (2).
    assert_eq!(room["world"]["triangles"], 14, "{room}");
    assert_eq!(room["world"]["instances"], 2);
    assert!(room["materials"].as_array().unwrap().len() >= 2);
    let names: Vec<_> = room["contributors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["divider", "room"]);
    assert!(room.get("proxy").is_none());
    assert_eq!(room["warnings"], json!([]));

    let side = data(
        dir.path(),
        "acoustics_inspect",
        json!({"scene":"side.scene.json"}),
    );
    assert_eq!(side["ok"], true, "{side}");
    assert_eq!(side["environment"]["proxy2d"]["plane"], "side_view");
    assert!(side["proxy"]["air"]["volume_m3"].as_f64().unwrap() > 100.0);
    assert!(side["world"]["triangles"].as_u64().unwrap() > 20);
    assert_eq!(side["contributors"].as_array().unwrap().len(), 5);

    let brief = data(
        dir.path(),
        "acoustics_inspect",
        json!({"include_entities": false}),
    );
    assert!(brief.get("contributors").is_none());
}

#[test]
fn inspect_reports_a_scene_without_an_environment() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "acoustics_inspect",
        json!({"scene":"scenes/start.json"}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert!(result["environment"].is_null());
    assert!(
        result["warnings"][0]
            .as_str()
            .unwrap()
            .contains("no enabled")
    );
}

#[test]
fn query_shows_the_divider_occluding_high_frequencies_and_is_deterministic() {
    let dir = showcase();
    let first = data(dir.path(), "acoustics_query", query_args());
    assert_eq!(first["ok"], true, "{first}");
    let second = data(dir.path(), "acoustics_query", query_args());
    assert_eq!(first, second, "identical queries give identical JSON");
    assert_eq!(first["backend"], "cpu");

    let behind = &first["sources"][0];
    let clear = &first["sources"][1];
    assert!(
        behind["visible_fraction"].as_f64().unwrap() < 0.1,
        "{behind}"
    );
    assert!(clear["visible_fraction"].as_f64().unwrap() > 0.9, "{clear}");
    let gains: Vec<f64> = behind["direct_gain_db"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(gains.len(), 4);
    assert!(
        gains[3] < gains[0] - 3.0,
        "high bands are quieter: {gains:?}"
    );
    assert!(behind["distance_m"].as_f64().unwrap() > 5.9);
    assert!(behind["reflections"].as_array().unwrap().len() <= 16);
    assert!(clear["reflections"].as_array().unwrap().len() > 0);

    let rt60: Vec<f64> = first["reverb"]["rt60_s"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(rt60.iter().all(|t| *t > 0.05 && *t < 20.0), "{rt60:?}");
    assert!(first["reverb"]["mean_free_path_m"].as_f64().unwrap() > 1.0);
}

#[test]
fn query_maps_2d_positions_through_the_proxy() {
    let dir = showcase();
    let result = data(
        dir.path(),
        "acoustics_query",
        json!({
            "scene": "side.scene.json",
            "listener": {"position": [-3, 0, 0]},
            "sources": [{"position": [3, -1, 0]}],
        }),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert!(result["sources"][0]["distance_m"].as_f64().unwrap() > 5.0);
    assert!(result["reverb"]["enabled"].as_bool().unwrap());
}

#[test]
fn query_rejects_bad_requests_with_coded_errors() {
    let dir = showcase();
    let mut too_many = query_args();
    too_many["sources"] = json!(
        (0..17)
            .map(|i| json!({"position":[i, 1, 0]}))
            .collect::<Vec<_>>()
    );
    let mut none = query_args();
    none["sources"] = json!([]);
    let mut huge = query_args();
    huge["quality"] = json!({"reverb_rays": 1_000_000});
    let mut unknown = query_args();
    unknown["quality"] = json!({"mystery": 1});
    for (name, args) in [
        ("too many", too_many),
        ("none", none),
        ("huge", huge),
        ("unknown", unknown),
    ] {
        let result = data(dir.path(), "acoustics_query", args);
        assert_eq!(result["ok"], false, "{name}: {result}");
    }
    // A scene without an environment has no world to query.
    let fixture = fixture();
    let mut args = query_args();
    args["scene"] = json!("scenes/start.json");
    let result = data(fixture.path(), "acoustics_query", args);
    assert_eq!(error_code(&result), "acoustics.environment.missing");
    // Paths stay confined to the project.
    let mut outside = query_args();
    outside["scene"] = json!("../room.scene.json");
    assert_eq!(data(dir.path(), "acoustics_query", outside)["ok"], false);
}

#[test]
fn bake_writes_a_new_document_that_loads_as_an_acoustics_asset() {
    let dir = showcase();
    let result = write(
        dir.path(),
        "acoustics_bake",
        json!({"scene":"room.scene.json","output":"baked.json"}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["triangles"], 14);
    assert_eq!(result["asset"]["kind"], "acoustics");
    assert!(
        result["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("fnv1a64:")
    );

    let bytes = fs::read(dir.path().join("baked.json")).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["instances"].as_array().unwrap().len(), 2);
    assert_eq!(document["content_hash"], result["content_hash"]);
    assert_eq!(document["version"], 1);

    // Register it, then the project (and the asset tools) accept it.
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"baked","kind":"acoustics","path":"baked.json"}));
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    let listed = data(dir.path(), "asset_list", json!({"kind":"acoustics"}));
    assert_eq!(listed["count"], 1, "{listed}");

    let side = write(
        dir.path(),
        "acoustics_bake",
        json!({"scene":"side.scene.json","output":"side-baked.json","include":{"meshes":false}}),
    );
    assert_eq!(side["ok"], true, "{side}");
    let side_doc: Value =
        serde_json::from_slice(&fs::read(dir.path().join("side-baked.json")).unwrap()).unwrap();
    assert!(side_doc["proxy"].is_object());
    assert_eq!(side_doc["instances"], json!([]));
    let without_proxy = write(
        dir.path(),
        "acoustics_bake",
        json!({"scene":"side.scene.json","output":"no-proxy.json","include":{"proxy2d":false}}),
    );
    assert_eq!(without_proxy["triangles"], 0, "{without_proxy}");
}

#[test]
fn bake_is_write_gated_confined_and_never_overwrites() {
    let dir = showcase();
    let args = json!({"scene":"room.scene.json","output":"baked.json"});
    let denied = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"acoustics_bake","arguments":args}),
        )],
    );
    assert_eq!(denied[0]["error"]["code"], -32602);
    assert!(!dir.path().join("baked.json").exists());
    for output in [
        "../baked.json",
        ".git/baked.json",
        "target/baked.json",
        "hum.wav",
        "missing/baked.json",
        "baked.txt",
        "C:/baked.json",
    ] {
        let result = write(
            dir.path(),
            "acoustics_bake",
            json!({"scene":"room.scene.json","output":output}),
        );
        assert_eq!(result["ok"], false, "{output}: {result}");
    }
    assert_eq!(
        write(dir.path(), "acoustics_bake", args.clone())["ok"],
        true
    );
    let before = fs::read(dir.path().join("baked.json")).unwrap();
    assert_eq!(write(dir.path(), "acoustics_bake", args)["ok"], false);
    assert_eq!(fs::read(dir.path().join("baked.json")).unwrap(), before);
    assert_eq!(fs::read(dir.path().join("hum.wav")).unwrap().len(), 4044);

    // No environment: nothing to bake and nothing written.
    let plain = fixture();
    let result = write(
        plain.path(),
        "acoustics_bake",
        json!({"scene":"scenes/start.json","output":"assets/baked.json"}),
    );
    assert_eq!(error_code(&result), "acoustics.environment.missing");
    assert!(!plain.path().join("assets/baked.json").exists());
}
