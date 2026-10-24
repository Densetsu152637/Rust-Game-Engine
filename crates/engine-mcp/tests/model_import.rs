mod common;
#[path = "../../engine-project/tests/support/model_fixture.rs"]
#[allow(dead_code)]
mod fixture;
use common::*;
use serde_json::json;

#[test]
fn mcp_import_is_write_gated_and_reloads_a_renderable_prefab() {
    let temp = fixture();
    fixture::write_source(temp.path(), false);
    let response = exchange(
        temp.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"asset_import_model","arguments":{"source":"source/model.gltf","prefix":"mcp"}}),
        )],
    );
    assert_eq!(response[0]["error"]["code"], -32602);
    let result = write(
        temp.path(),
        "asset_import_model",
        json!({"source":"source/model.gltf","prefix":"mcp"}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["prefab"], "mcp:prefab");
    let project = engine_project::Project::load(temp.path().join("project.json")).unwrap();
    let prefab =
        engine_project::load_scene(&project.root, &project.assets["mcp:prefab"].path).unwrap();
    assert_eq!(prefab.entities.len(), 4);
    let texture =
        engine_project::content::load_texture(&project.resolve_asset("mcp:texture:0").unwrap())
            .unwrap();
    assert_eq!(texture.pixels, vec![255, 32, 16, 255]);
    let mesh = data(temp.path(), "asset_info", json!({"asset":"mcp:mesh:0:0"}));
    assert_eq!(mesh["ok"], true, "{mesh}");
}
#[test]
fn mcp_model_info_reports_generated_skin_binding_use() {
    let temp = fixture();
    fixture::write_skinned_source(temp.path());
    let result = write(
        temp.path(),
        "asset_import_model",
        json!({"source":"source/model.gltf","prefix":"skin"}),
    );
    assert_eq!(result["ok"], true, "{result}");
    let info = data(temp.path(), "asset_info", json!({"asset":"skin:model"}));
    assert_eq!(info["ok"], true, "{info}");
    assert_eq!(info["details"]["skins"], 1);
    assert!(
        info["used_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["component"] == "engine.SkinBinding" && entry["field"] == "model")
    );
}
