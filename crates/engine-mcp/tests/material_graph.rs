mod common;
use common::*;
use serde_json::json;

#[test]
fn material_graph_schema_and_inline_validation_are_available_over_mcp() {
    let dir = fixture();
    let schema = data(dir.path(), "graph_schema", json!({"kind":"material"}));
    assert_eq!(schema["ok"], true, "{schema}");
    assert_eq!(schema["kinds"][0]["name"], "material");
    let types = schema["kinds"][0]["node_types"].as_array().unwrap();
    assert!(types.iter().any(|node| node["name"] == "material_output"));
    assert!(types.iter().any(|node| node["name"] == "texture_color"));
    assert_eq!(schema["kinds"][0]["limits"]["max_nodes"], 64);
    assert_eq!(schema["kinds"][0]["limits"]["max_edges"], 128);

    let valid = data(
        dir.path(),
        "graph_validate",
        json!({"document":{
            "version":1,
            "kind":"material",
            "nodes":[
                {"id":"color","node_type":"vec3","params":{"value":[0.8,0.1,0.2]}},
                {"id":"out","node_type":"material_output"}
            ],
            "edges":[{
                "from":{"node":"color","port":"value"},
                "to":{"node":"out","port":"base_color"}
            }]
        }}),
    );
    assert_eq!(valid["ok"], true, "{valid}");
    assert_eq!(valid["valid"], true, "{valid}");
}
