//! `engine.LargeWorld` and `engine.AbsolutePosition` through `component_set`.
mod common;
use common::*;
use serde_json::json;
use std::fs;

const START: &str = "scenes/start.json";

#[test]
fn large_world_settings_are_editable_through_component_set() {
    let dir = fixture();
    let revision = || revision(dir.path(), START);
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.LargeWorld"}),
    );
    let properties = &schema["components"][0]["schema"]["properties"]["data"]["properties"];
    for key in ["enabled", "threshold", "snap", "anchor"] {
        assert!(properties.get(key).is_some(), "{key}: {schema}");
    }
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"camera","component":"engine.LargeWorld",
            "value":{"version":1,"data":{"threshold":4096.0,"snap":1024,"anchor":"player"}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.LargeWorld"]["data"];
    assert_eq!(stored["snap"], 1024);
    assert_eq!(stored["anchor"], "player");
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"camera","component":"engine.LargeWorld",
            "value":{"version":1,"data":{"enabled":false,"threshold":4096.0,"snap":1024}}}),
    );
    assert_eq!(merged["ok"], true, "{merged}");
    assert_eq!(
        merged["entity"]["components"]["custom"]["engine.LargeWorld"]["data"]["enabled"],
        false
    );
    let before = fs::read(dir.path().join(START)).unwrap();
    for bad in [
        json!({"snap": 300}),
        json!({"threshold": 0.5}),
        json!({"threshold": 100.0, "snap": 128}),
        json!({"anchor": 7}),
        json!({"unknown": true}),
    ] {
        let result = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(),"entity":"camera","component":"engine.LargeWorld",
                "value":{"version":1,"data":bad}}),
        );
        assert_eq!(result["ok"], false, "{bad}");
        assert_eq!(fs::read(dir.path().join(START)).unwrap(), before, "{bad}");
    }
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
}

#[test]
fn absolute_positions_are_editable_through_component_set() {
    let dir = fixture();
    let revision = || revision(dir.path(), START);
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.AbsolutePosition",
            "value":{"version":1,"data":{"position":[12345678.125, 2.0, -9876543.5]}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    assert_eq!(
        set["entity"]["components"]["custom"]["engine.AbsolutePosition"]["data"]["position"],
        json!([12345678.125, 2.0, -9876543.5])
    );
    for bad in [
        json!({"position": [1.0, 2.0]}),
        json!({"position": [2.0e12, 0.0, 0.0]}),
        json!({"position": [0.0, 0.0, 0.0], "local": true}),
    ] {
        let result = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.AbsolutePosition",
                "value":{"version":1,"data":bad}}),
        );
        assert_eq!(result["ok"], false, "{bad}");
    }
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
}
