//! `steam_status`: read-only, never starts Steam, reports no secrets.
mod common;
use common::*;
use serde_json::{Value, json};
use std::fs;

fn with_steam_block(root: &std::path::Path) {
    let path = root.join("project.json");
    let mut manifest: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    manifest["steam"] = json!({"app_id": 480, "required": true, "cloud": true});
    fs::write(&path, manifest.to_string()).unwrap();
}

#[test]
fn a_project_without_a_steam_block_reports_undeclared() {
    let dir = fixture();
    let status = result(dir.path(), "steam_status", json!({}));
    let data = &status["structuredContent"];
    assert_eq!(data["ok"], true);
    assert_eq!(data["declared"], false);
    assert!(data["app_id"].is_null());
    assert_eq!(data["compiled"], engine_steam::STEAM_COMPILED);
}

#[test]
fn a_declared_block_reports_its_settings_without_initialising_steam() {
    let dir = fixture();
    with_steam_block(dir.path());
    let status = result(dir.path(), "steam_status", json!({}));
    let data = &status["structuredContent"];
    assert_eq!(data["declared"], true);
    assert_eq!(data["app_id"], 480);
    assert_eq!(data["required"], true);
    assert_eq!(data["cloud"], true);
    assert_eq!(data["initialized"], false);
    assert_eq!(data["status"], "not_started");
    assert!(data["user_name"].is_null());
    let text = data.to_string();
    assert!(!text.contains("ticket") && !text.contains("token"));
}
