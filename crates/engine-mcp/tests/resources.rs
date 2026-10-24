//! `resources/list`, `resources/read`, `resources/templates/list` and the
//! authored-component schema drift guard.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use engine_mcp::{COMPONENT_NAMES, Limits, ServerConfig, component_docs};
use engine_project::{Components, Scene};
use serde_json::{Value, json};
use std::fs;

fn list(root: &std::path::Path, params: Value) -> Value {
    exchange(root, read_only(), &[modern(1, "resources/list", params)]).remove(0)
}

fn read(root: &std::path::Path, uri: &str) -> Value {
    exchange(
        root,
        read_only(),
        &[modern(1, "resources/read", json!({"uri": uri}))],
    )
    .remove(0)
}

fn uris(response: &Value) -> Vec<String> {
    response["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn resources_list_exposes_project_files_with_mime_types() {
    let dir = fixture();
    let response = list(dir.path(), json!({}));
    assert_eq!(response["result"]["resultType"], "complete");
    let resources = response["result"]["resources"].as_array().unwrap();
    let find = |uri: &str| resources.iter().find(|r| r["uri"] == uri).unwrap();
    assert_eq!(
        find("project://project.json")["mimeType"],
        "application/json"
    );
    assert_eq!(find("project://project.json")["name"], "project.json");
    assert_eq!(find("project://src/lib.rs")["mimeType"], "text/x-rust");
    assert_eq!(find("project://Cargo.toml")["mimeType"], "application/toml");
    assert_eq!(
        find("project://assets/white.ppm")["mimeType"],
        "image/x-portable-pixmap"
    );
    assert_eq!(
        find("project://assets/blob.bin")["mimeType"],
        "application/octet-stream"
    );
    assert_eq!(
        find("project://notes/readme.md")["mimeType"],
        "text/markdown"
    );
    assert_eq!(find("project://notes/readme.md")["size"], 45);
    assert!(response["result"].get("nextCursor").is_none());
}

#[test]
fn resources_list_is_sorted_and_skips_hidden_directories_and_links() {
    let dir = fixture();
    for hidden in [".git", ".derived", "target"] {
        fs::create_dir(dir.path().join(hidden)).unwrap();
        fs::write(dir.path().join(hidden).join("x.txt"), "x").unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "s").unwrap();
    directory_link(outside.path(), &dir.path().join("escape"));
    let listed = uris(&list(dir.path(), json!({})));
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted);
    assert!(
        listed
            .iter()
            .all(|u| !u.contains("x.txt") && !u.contains("secret"))
    );
}

#[test]
fn resources_list_paginates_with_cursors() {
    let dir = fixture();
    fs::create_dir(dir.path().join("many")).unwrap();
    for i in 0..130 {
        fs::write(dir.path().join(format!("many/f{i:03}.txt")), "x").unwrap();
    }
    let first = list(dir.path(), json!({}));
    assert_eq!(first["result"]["resources"].as_array().unwrap().len(), 100);
    let cursor = first["result"]["nextCursor"].as_str().unwrap();
    let second = list(dir.path(), json!({"cursor": cursor}));
    let rest = second["result"]["resources"].as_array().unwrap().len();
    assert!(rest > 30 && rest < 100, "{rest}");
    assert!(second["result"].get("nextCursor").is_none());
    let all: Vec<_> = uris(&first).into_iter().chain(uris(&second)).collect();
    let unique: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "pages do not overlap");
}

#[test]
fn resources_list_rejects_bad_cursors() {
    let dir = fixture();
    for cursor in [json!("garbage"), json!("o-1"), json!(5)] {
        let response = list(dir.path(), json!({"cursor": cursor}));
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
}

#[test]
fn resources_read_returns_text_with_a_normalised_uri() {
    let dir = fixture();
    let response = read(dir.path(), "project://notes/readme.md");
    let content = &response["result"]["contents"][0];
    assert_eq!(content["uri"], "project://notes/readme.md");
    assert_eq!(content["mimeType"], "text/markdown");
    assert!(content["text"].as_str().unwrap().starts_with("# Notes"));
    assert_eq!(response["result"]["resultType"], "complete");
}

#[test]
fn resources_read_returns_binary_as_base64_blobs() {
    let dir = fixture();
    let response = read(dir.path(), "project://assets/blob.bin");
    let content = &response["result"]["contents"][0];
    assert_eq!(content["mimeType"], "application/octet-stream");
    assert!(content.get("text").is_none());
    assert_eq!(
        STANDARD.decode(content["blob"].as_str().unwrap()).unwrap(),
        [0, 1, 2, 255, 0, 7]
    );
}

#[test]
fn resources_read_unknown_extensions_with_text_are_plain_text() {
    let dir = fixture();
    fs::write(dir.path().join("notes/data.custom"), "hello").unwrap();
    let response = read(dir.path(), "project://notes/data.custom");
    assert_eq!(response["result"]["contents"][0]["mimeType"], "text/plain");
}

#[test]
fn resources_read_handles_percent_encoded_paths() {
    let dir = fixture();
    fs::write(dir.path().join("notes/my file é.txt"), "spaced").unwrap();
    let listed = uris(&list(dir.path(), json!({})));
    assert!(
        listed.contains(&"project://notes/my%20file%20%C3%A9.txt".to_owned()),
        "{listed:?}"
    );
    let response = read(dir.path(), "project://notes/my%20file%20%C3%A9.txt");
    assert_eq!(response["result"]["contents"][0]["text"], "spaced");
}

#[test]
fn resources_read_refuses_everything_outside_the_project() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP-SECRET").unwrap();
    directory_link(outside.path(), &dir.path().join("escape"));
    for uri in [
        "project://../secret.txt",
        "project://notes/../../secret.txt",
        "project://escape/secret.txt",
        "project:///etc/passwd",
        "project://%2e%2e/secret.txt",
        "project://notes%2F..%2F..%2Fsecret.txt",
        "project://notes",
        "project://missing.txt",
        "project://.git/config",
        "project://",
        "project://notes\\readme.md",
        "project://%5C",
        "project://%ZZ",
    ] {
        let response = read(dir.path(), uri);
        assert_eq!(response["error"]["code"], -32002, "{uri}: {response}");
        assert!(!response.to_string().contains("TOP-SECRET"));
        assert_eq!(response["error"]["message"], "resource not found");
    }
}

#[test]
fn resources_read_rejects_other_schemes_queries_and_missing_uris() {
    let dir = fixture();
    for uri in [
        "file:///etc/passwd",
        "https://example.com/x",
        "notes/readme.md",
    ] {
        assert_eq!(read(dir.path(), uri)["error"]["code"], -32602, "{uri}");
    }
    assert_eq!(
        read(dir.path(), "project://notes/readme.md?x=1")["error"]["code"],
        -32602
    );
    assert_eq!(
        read(dir.path(), "project://notes/readme.md#frag")["error"]["code"],
        -32602
    );
    let missing = exchange(
        dir.path(),
        read_only(),
        &[modern(1, "resources/read", json!({}))],
    );
    assert_eq!(missing[0]["error"]["code"], -32602);
    let numeric = exchange(
        dir.path(),
        read_only(),
        &[modern(1, "resources/read", json!({"uri": 5}))],
    );
    assert_eq!(numeric[0]["error"]["code"], -32602);
}

#[test]
fn resources_read_enforces_the_resource_size_limit() {
    let dir = fixture();
    let config = ServerConfig::new().with_limits(Limits {
        max_resource_bytes: 20,
        ..Default::default()
    });
    let response = exchange(
        dir.path(),
        config.clone(),
        &[modern(
            1,
            "resources/read",
            json!({"uri":"project://notes/readme.md"}),
        )],
    );
    assert_eq!(response[0]["error"]["code"], -32602);
    assert!(
        response[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("limit")
    );
    let ok = exchange(
        dir.path(),
        config,
        &[modern(
            1,
            "resources/read",
            json!({"uri":"project://assets/blob.bin"}),
        )],
    );
    assert!(ok[0].get("result").is_some());
}

#[test]
fn resources_templates_describe_the_project_scheme() {
    let dir = fixture();
    let response = exchange(
        dir.path(),
        read_only(),
        &[modern(1, "resources/templates/list", json!({}))],
    );
    let template = &response[0]["result"]["resourceTemplates"][0];
    assert_eq!(template["uriTemplate"], "project://{+path}");
    assert_eq!(template["name"], "project_file");
    assert!(template["description"].as_str().unwrap().contains("Hidden"));
}

#[test]
fn resources_work_in_the_legacy_session_too() {
    let dir = fixture();
    let responses = exchange(
        dir.path(),
        read_only(),
        &[
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":1,"method":"resources/list"}),
            json!({"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"project://src/lib.rs"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"resources/templates/list"}),
        ],
    );
    assert!(
        responses[1]["result"]["resources"]
            .as_array()
            .unwrap()
            .len()
            > 5
    );
    assert!(responses[1]["result"].get("resultType").is_none());
    assert!(
        responses[2]["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("configure")
    );
    assert!(responses[3]["result"]["resourceTemplates"].is_array());
}

#[test]
fn resources_before_the_legacy_handshake_are_refused() {
    let dir = fixture();
    let responses = exchange(
        dir.path(),
        read_only(),
        &[json!({"jsonrpc":"2.0","id":1,"method":"resources/list"})],
    );
    assert_eq!(responses[0]["error"]["code"], -32002);
}

// ---- component schema drift guard ------------------------------------------------------

#[test]
fn documented_component_names_match_the_scene_schema() {
    let full = json!({
        "Transform": {"position": [0, 0, 0]},
        "Script": {"asset": "a"},
        "MeshRenderer": {"mesh": "m"},
        "SpriteRenderer": {"texture": "t"},
        "Camera2D": {},
        "Camera3D": {},
        "MeshMaterial": {},
        "Collider2D": {"size": [1, 1]},
        "EventBindings": {"bindings": []},
        "DirectionalLight": {},
        "PointLight": {},
        "SpotLight": {},
        "AreaLight": {},
    });
    let components: Components = serde_json::from_value(full).unwrap();
    let serialized = serde_json::to_value(&components).unwrap();
    let mut keys: Vec<_> = serialized.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    let mut documented: Vec<_> = COMPONENT_NAMES.iter().map(|n| (*n).to_owned()).collect();
    documented.sort();
    assert_eq!(
        keys, documented,
        "every serializable component must be documented"
    );
    assert_eq!(component_docs().len(), COMPONENT_NAMES.len());
}

#[test]
fn every_component_example_deserializes_and_validates() {
    for doc in component_docs() {
        let entity = json!({"id":"e","name":"E","components":{
            "Transform": {"position":[0,0,0]},
            doc.name: doc.example.clone()}});
        let scene: Scene =
            serde_json::from_value(json!({"schema":1,"scene_id":"s","entities":[entity]}))
                .unwrap_or_else(|e| panic!("{} example rejected: {e}", doc.name));
        scene
            .validate()
            .unwrap_or_else(|e| panic!("{} example invalid: {e}", doc.name));
    }
}

#[test]
fn every_documented_property_is_accepted_by_the_strict_deserializer() {
    for doc in component_docs() {
        let properties = doc.schema["properties"].as_object().unwrap();
        let mut value = doc.example.as_object().unwrap().clone();
        for (name, property) in properties {
            if let Some(default) = property.get("default") {
                value.insert(name.clone(), default.clone());
            }
        }
        let components = json!({"Transform":{"position":[0,0,0]}, doc.name: value});
        serde_json::from_value::<Components>(components)
            .unwrap_or_else(|e| panic!("{} documented defaults rejected: {e}", doc.name));
        let mut bogus = doc.example.as_object().unwrap().clone();
        bogus.insert("definitely_not_a_field".into(), json!(1));
        let rejected = serde_json::from_value::<Components>(json!({doc.name: bogus}));
        assert!(rejected.is_err(), "{} must stay closed", doc.name);
    }
}

#[test]
fn serialized_defaults_only_use_documented_properties() {
    use engine_project::{Camera2D, SpriteRenderer, Transform, rendering3d::Camera3D};
    let defaults = [
        (
            "Transform",
            serde_json::to_value(Transform::default()).unwrap(),
        ),
        (
            "SpriteRenderer",
            serde_json::to_value(SpriteRenderer::default()).unwrap(),
        ),
        (
            "Camera2D",
            serde_json::to_value(Camera2D::default()).unwrap(),
        ),
        (
            "Camera3D",
            serde_json::to_value(Camera3D::default()).unwrap(),
        ),
    ];
    for (name, value) in defaults {
        let doc = component_docs()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap();
        let properties = doc.schema["properties"].as_object().unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                properties.contains_key(key),
                "{name}.{key} is serialized but undocumented"
            );
        }
        for (key, property) in properties {
            if let Some(default) = property.get("default")
                && let Some(actual) = value.get(key)
            {
                assert_eq!(
                    serde_json::to_string(actual).unwrap().replace(".0", ""),
                    serde_json::to_string(default).unwrap().replace(".0", ""),
                    "{name}.{key} documented default differs from the engine default"
                );
            }
        }
    }
}

#[test]
fn required_fields_are_exactly_those_without_serde_defaults() {
    // Components that fail to deserialize without these fields.
    for (name, empty_is_valid) in [
        ("Transform", false),
        ("Script", false),
        ("MeshRenderer", false),
        ("SpriteRenderer", false),
        ("Camera2D", true),
        ("Collider2D", false),
    ] {
        let parsed = serde_json::from_value::<Components>(json!({ name: {} }));
        assert_eq!(parsed.is_ok(), empty_is_valid, "{name}");
        let doc = component_docs()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap();
        assert_eq!(
            doc.schema["required"].as_array().unwrap().is_empty(),
            empty_is_valid,
            "{name}"
        );
    }
}
