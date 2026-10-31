//! Without the `text-scripting` feature the data shape stays valid but nothing parses
//! or runs a `.rhai` source: starting a world that holds a component fails with a
//! coded error and the check API says why.
#![cfg(not(feature = "text-scripting"))]
use engine_project::{scripting, *};
use serde_json::json;
use std::fs;

#[test]
fn text_scripts_are_refused_when_the_backend_is_not_compiled_in() {
    let error = scripting::check_text_source(b"fn on_start() {}").unwrap_err();
    assert_eq!(error.code, "script.text.disabled");
    assert!(!scripting::TEXT_SCRIPTING);
    assert!(scripting::text_limits().is_none());

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("s.rhai"), "fn on_start() {}").unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema": 1, "name": "off", "startup_scene": "scene.json",
            "assets": [{"id": "s", "kind": "text_script", "path": "s.rhai"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema": 1, "scene_id": "main", "entities": []}).to_string(),
    )
    .unwrap();
    // A project that only carries the asset fails validation with the coded reason.
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "script.text.disabled");
}
