//! Font assets and world text through the MCP tools: `font_face` and
//! `font_config` assets are listed and described, and `engine.WorldText` is an
//! ordinary component for `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};
use std::fs;

const START: &str = "scenes/start.json";

fn add_fonts(root: &std::path::Path) {
    fs::write(
        root.join("assets/ui.ttf"),
        epaint_default_fonts::UBUNTU_LIGHT,
    )
    .unwrap();
    fs::write(
        root.join("assets/fonts.json"),
        json!({"schema":1,"families":{"ui":{"size":20}},"fallback":{"default":["ui"]}}).to_string(),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
    let assets = manifest["assets"].as_array_mut().unwrap();
    assets.push(json!({"id":"ui","kind":"font_face","path":"assets/ui.ttf"}));
    assets.push(json!({"id":"fonts","kind":"font_config","path":"assets/fonts.json"}));
    fs::write(root.join("project.json"), manifest.to_string()).unwrap();
}

#[test]
fn font_face_and_config_assets_are_listed_and_described() {
    let dir = fixture();
    add_fonts(dir.path());
    let faces = data(dir.path(), "asset_list", json!({"kind":"font_face"}));
    assert_eq!(faces["count"], 1, "{faces}");
    let info = data(dir.path(), "asset_info", json!({"asset":"ui"}));
    assert_eq!(info["details"]["faces"][0]["family"], "Ubuntu", "{info}");
    assert_eq!(info["details"]["faces"][0]["italic"], false);
    assert!(info["details"]["bytes"].as_u64().unwrap() > 10_000);
    let config = data(dir.path(), "asset_info", json!({"asset":"fonts"}));
    assert_eq!(config["details"]["families"], 1, "{config}");
    assert_eq!(config["details"]["default_size"], 16.0);
    // The kind filter accepts the new kinds; a bad file reports a typed error.
    let configs = data(dir.path(), "asset_list", json!({"kind":"font_config"}));
    assert_eq!(configs["count"], 1);
}

#[test]
fn world_text_is_editable_through_component_set() {
    let dir = fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.WorldText"}),
    );
    assert_eq!(
        schema["components"][0]["name"], "engine.WorldText",
        "{schema}"
    );
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
            "component":"engine.WorldText",
            "value":{"version":1,"data":{"version":1,"text":"[b]Hello[/b]","markup":true,
                "font":"ui","size":0.5,"billboard":true,"maxWidth":4.0}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.WorldText"]["data"];
    assert_eq!(stored["billboard"], true);
    assert_eq!(stored["maxWidth"], 4.0);
    let bad = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
            "component":"engine.WorldText",
            "value":{"version":1,"data":{"version":1,"text":"[b]open","markup":true}}}),
    );
    assert_eq!(bad["ok"], false, "{bad}");
}
