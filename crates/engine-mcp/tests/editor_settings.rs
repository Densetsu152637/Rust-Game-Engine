//! Typed editor settings through native MCP transcripts; no running editor required.
mod common;
use common::*;
use serde_json::{Value, json};
use std::fs;

fn get(root: &std::path::Path) -> Value {
    data(root, "editor_settings_get", json!({}))
}
fn update(root: &std::path::Path, patch: Value, revision: Value) -> Value {
    let mut args = json!({"patch":patch});
    if !revision.is_null() {
        args["expected_revision"] = revision;
    }
    write(root, "editor_settings_update", args)
}
#[test]
fn discovery_access_and_missing_defaults_are_typed_and_read_only() {
    let dir = fixture();
    for config in [read_only(), writable()] {
        let can_write = config.access.write;
        let replies = exchange(dir.path(), config, &[modern(1, "tools/list", json!({}))]);
        let tools = replies[0]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "editor_settings_get"));
        assert_eq!(
            tools.iter().any(|t| t["name"] == "editor_settings_update"),
            can_write
        );
    }
    let denied = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"editor_settings_update","arguments":{"patch":{}}}),
        )],
    );
    assert_eq!(denied[0]["error"]["code"], -32602);
    let settings = get(dir.path());
    assert_eq!(settings["ok"], true, "{settings}");
    assert_eq!(settings["exists"], false);
    assert!(settings["revision"].is_null());
    assert_eq!(settings["settings"]["schema"], 1);
    assert_eq!(settings["settings"]["snapping"]["translate_step"], 0.5);
    assert!(!dir.path().join("editor.json").exists());
}
#[test]
fn typed_update_roundtrips_partial_sections_and_preserves_bookmarks() {
    let dir = fixture();
    let bookmark = json!({"name":"top","three_dimensional":true,"center":[1,2,3],"yaw":0.5,"pitch":0,"distance":10,"pixels_per_unit":16});
    let created = update(
        dir.path(),
        json!({"snapping":{"enabled":true},"bookmarks":[bookmark.clone()]}),
        Value::Null,
    );
    assert_eq!(created["ok"], true, "{created}");
    let changed = update(
        dir.path(),
        json!({"grid":{"spacing":2},"snapping":{"translate_step":0.25}}),
        created["revision"].clone(),
    );
    assert_eq!(changed["ok"], true, "{changed}");
    assert_eq!(
        changed["settings"]["bookmarks"],
        created["settings"]["bookmarks"]
    );
    assert_eq!(changed["settings"]["snapping"]["enabled"], true);
    let persisted = engine_editor_settings::EditorSettings::load(dir.path()).unwrap();
    assert_eq!(persisted.grid.spacing, 2.0);
    assert_eq!(persisted.snapping.translate_step, 0.25);
    assert_eq!(get(dir.path())["revision"], changed["revision"]);
}
#[test]
fn invalid_patches_revision_mismatches_and_invalid_files_preserve_bytes() {
    let dir = fixture();
    let created = update(dir.path(), json!({}), Value::Null);
    assert_eq!(created["ok"], true, "{created}");
    let before = fs::read(dir.path().join("editor.json")).unwrap();
    for patch in [
        json!({"snapping":{"translate_step":0}}),
        json!({"gizmo":{"mode":"unknown"}}),
        json!({"extra":1}),
        json!({"schema":2}),
        json!({"bookmarks":[{}]}),
    ] {
        let failed = update(dir.path(), patch, created["revision"].clone());
        assert_eq!(error_code(&failed), "editor.settings.invalid", "{failed}");
        assert_eq!(fs::read(dir.path().join("editor.json")).unwrap(), before);
    }
    assert_eq!(
        error_code(&update(dir.path(), json!({}), Value::Null)),
        "revision_required"
    );
    assert_eq!(
        error_code(&update(dir.path(), json!({}), json!("stale"))),
        "revision_conflict"
    );
    fs::write(dir.path().join("editor.json"), b"invalid existing settings").unwrap();
    assert_eq!(error_code(&get(dir.path())), "editor.settings.invalid");
    assert_eq!(
        error_code(&update(dir.path(), json!({}), created["revision"].clone())),
        "editor.settings.invalid"
    );
    assert_eq!(
        fs::read(dir.path().join("editor.json")).unwrap(),
        b"invalid existing settings"
    );
}
#[test]
fn stale_revision_after_external_change_and_expected_revision_for_missing_are_rejected() {
    let dir = fixture();
    assert_eq!(
        error_code(&update(dir.path(), json!({}), json!("not-existing"))),
        "revision_conflict"
    );
    assert!(!dir.path().join("editor.json").exists());
    let created = update(dir.path(), json!({}), Value::Null);
    let external = b"{\"schema\":1,\"grid\":{\"spacing\":3}}\n";
    fs::write(dir.path().join("editor.json"), external).unwrap();
    assert_eq!(
        error_code(&update(
            dir.path(),
            json!({"grid":{"spacing":4}}),
            created["revision"].clone()
        )),
        "revision_conflict"
    );
    assert_eq!(fs::read(dir.path().join("editor.json")).unwrap(), external);
}
#[test]
fn oversized_documents_and_patches_are_bounded() {
    let dir = fixture();
    let oversized = vec![b' '; engine_editor_settings::MAX_DOCUMENT_BYTES as usize + 1];
    fs::write(dir.path().join("editor.json"), &oversized).unwrap();
    assert_eq!(error_code(&get(dir.path())), "editor.settings.too_large");
    assert_eq!(
        error_code(&update(dir.path(), json!({}), json!("revision"))),
        "editor.settings.too_large"
    );
    assert_eq!(fs::read(dir.path().join("editor.json")).unwrap(), oversized);
    fs::remove_file(dir.path().join("editor.json")).unwrap();
    let patch = json!({"unknown":"x".repeat(engine_editor_settings::MAX_DOCUMENT_BYTES as usize)});
    assert_eq!(
        error_code(&update(dir.path(), patch, Value::Null)),
        "editor.settings.too_large"
    );
    assert!(!dir.path().join("editor.json").exists());
}
#[test]
fn no_caller_path_or_link_can_escape_the_fixed_settings_target() {
    let dir = fixture();
    for path in ["../escape.json", ".git/config", "C:/escape.json"] {
        let replies = exchange(
            dir.path(),
            writable(),
            &[modern(
                1,
                "tools/call",
                json!({"name":"editor_settings_update","arguments":{"patch":{},"path":path}}),
            )],
        );
        assert_eq!(replies[0]["error"]["code"], -32602);
    }
    assert!(!dir.path().join("editor.json").exists());
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("external.json");
    fs::write(&target, b"{\"schema\":1}").unwrap();
    if !file_link(&target, &dir.path().join("editor.json")) {
        eprintln!("SKIPPED settings file-link test: platform did not grant symlink privilege");
        return;
    }
    assert_eq!(error_code(&get(dir.path())), "path_link");
    assert_eq!(
        error_code(&update(dir.path(), json!({}), Value::Null)),
        "path_link"
    );
    assert_eq!(fs::read(&target).unwrap(), b"{\"schema\":1}");
}
