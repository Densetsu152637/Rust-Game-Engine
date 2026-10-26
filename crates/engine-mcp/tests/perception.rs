//! Perception components authored through `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};

const START: &str = "scenes/start.json";

fn set(dir: &std::path::Path, component: &str, data: Value) -> Value {
    write(
        dir,
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir, START),"entity":"player",
            "component":component,"value":{"version":1,"data":data}}),
    )
}

#[test]
fn perception_components_are_authored_and_validated_with_component_set() {
    let dir = fixture();
    let ok = set(
        dir.path(),
        "engine.PerceptionSensor",
        json!({"team":1,"sight":{"range":15,"half_angle":60},"hearing":{},"damage":{},
            "forward":[0,0,-1]}),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let stored = &ok["entity"]["components"]["custom"]["engine.PerceptionSensor"]["data"];
    assert_eq!(stored["sight"]["range"], 15);
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.PerceptionSensor",
            json!({"sight":{"range":0}})
        )),
        "perception.value.invalid"
    );
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.PerceptionSensor",
            json!({"sight":{"range":10,"lose_range":5}})
        )),
        "perception.value.invalid"
    );
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.PerceptionSensor",
            json!({"blackboard":{"target":"enemy"},"sight":{}})
        )),
        "perception.blackboard.agent",
        "keys need a tree agent to write to"
    );
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.PerceptionStimulus",
            json!({"team":-1})
        )),
        "component.schema.invalid"
    );
    let stimulus = set(
        dir.path(),
        "engine.PerceptionStimulus",
        json!({"team":2,"height_offset":1.6}),
    );
    assert_eq!(stimulus["ok"], true, "{stimulus}");
    let noise = set(
        dir.path(),
        "engine.PerceptionNoise",
        json!({"loudness":0.5,"range":12,"interval":2}),
    );
    assert_eq!(noise["ok"], true, "{noise}");
    let settings = set(
        dir.path(),
        "engine.PerceptionSettings",
        json!({"relations":[{"a":1,"b":2,"affiliation":"hostile"}],"max_rays_per_tick":16,
            "occlusion":"none"}),
    );
    assert_eq!(settings["ok"], true, "{settings}");
    for name in [
        "engine.PerceptionSensor",
        "engine.PerceptionStimulus",
        "engine.PerceptionNoise",
        "engine.PerceptionSettings",
    ] {
        let schema = data(dir.path(), "component_schema", json!({"component":name}));
        assert_eq!(schema["components"][0]["name"], name, "{schema}");
        assert!(schema["components"][0]["example"]["data"].is_object());
    }
}
