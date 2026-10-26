//! Environment query templates and smart objects authored through `component_set`.
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
fn query_templates_and_smart_objects_are_authored_and_validated_with_component_set() {
    let dir = fixture();
    let queries = json!({"queries":[{"name":"cover","project_to_navmesh":false,
        "generator":{"type":"circle","center":"target","radius":8,"rings":2,"count":12},
        "tests":[{"type":"distance","to":"target","min":2,"score":"lower_better"},
                 {"type":"line_of_sight","from":"target"}],
        "select":{"top_n":3}}]});
    let ok = set(dir.path(), "engine.EnvironmentQueries", queries);
    assert_eq!(ok["ok"], true, "{ok}");
    let stored = &ok["entity"]["components"]["custom"]["engine.EnvironmentQueries"]["data"];
    assert_eq!(stored["queries"][0]["name"], "cover");

    let bad = |queries: Value| {
        error_code(&set(
            dir.path(),
            "engine.EnvironmentQueries",
            json!({ "queries": queries }),
        ))
    };
    let circle = json!({"type":"circle","radius":2});
    assert_eq!(
        bad(json!([{"name":"x y","generator":circle}])),
        "env_query.value.invalid"
    );
    assert_eq!(
        bad(json!([{"name":"q","generator":circle,"tests":[{"type":"distance"}]}])),
        "env_query.value.invalid",
        "a test that does nothing"
    );
    assert_eq!(
        bad(json!([{"name":"q","generator":circle,"navmesh":"ghost"}])),
        "project.asset.unknown"
    );

    let object = set(
        dir.path(),
        "engine.SmartObject",
        json!({"claim_timeout":5,"slots":[
            {"name":"left","offset":[-0.5,0,0],"tags":["sit"],"behavior":"sit_down"}]}),
    );
    assert_eq!(object["ok"], true, "{object}");
    assert_eq!(
        error_code(&set(
            dir.path(),
            "engine.SmartObject",
            json!({"slots":[{"name":"a"},{"name":"a"}]})
        )),
        "smart_object.value.invalid"
    );
    for name in ["engine.EnvironmentQueries", "engine.SmartObject"] {
        let schema = data(dir.path(), "component_schema", json!({"component":name}));
        assert_eq!(schema["components"][0]["name"], name, "{schema}");
    }
}
