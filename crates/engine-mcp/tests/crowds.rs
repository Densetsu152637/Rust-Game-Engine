//! Crowd components through the generic MCP tools: they are ordinary `engine.*` components for
//! `component_set`; no dedicated tool exists.
mod common;
use common::*;
use serde_json::json;

const START: &str = "scenes/start.json";

#[test]
fn crowd_components_are_editable_through_component_set() {
    let dir = fixture();
    for (name, data) in [
        (
            "engine.CrowdAgent",
            json!({"navmesh":"nav","target":[1,0,2],"radius":0.5,"max_speed":3}),
        ),
        (
            "engine.NavigationObstacle",
            json!({"navmesh":"nav","shape":"cylinder","radius":2}),
        ),
        (
            "engine.OffMeshLink",
            json!({"navmesh":"nav","end":[4,0,4],"cost":2.5,"bidirectional":false}),
        ),
        (
            "engine.Formation",
            json!({"navmesh":"nav","slots":[[0,0],[1,-1]],"yaw":0.5}),
        ),
    ] {
        let schema = data_of(dir.path(), name);
        assert_eq!(schema["components"][0]["name"], name, "{schema}");
        let set = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
                "component":name,"value":{"version":1,"data":data}}),
        );
        assert_eq!(set["ok"], true, "{name}: {set}");
        assert_eq!(
            set["entity"]["components"]["custom"][name]["data"]["navmesh"],
            "nav"
        );
    }
    for (name, bad) in [
        ("engine.CrowdAgent", json!({"radius":0})),
        ("engine.CrowdAgent", json!({"colour":1})),
        ("engine.NavigationObstacle", json!({"shape":"sphere"})),
        ("engine.OffMeshLink", json!({"cost":-1})),
        ("engine.Formation", json!({"slots":[[1]]})),
    ] {
        let rejected = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
                "component":name,"value":{"version":1,"data":bad}}),
        );
        assert_eq!(rejected["ok"], false, "{name} {bad}: {rejected}");
    }
}

fn data_of(root: &std::path::Path, name: &str) -> serde_json::Value {
    data(root, "component_schema", json!({"component":name}))
}
