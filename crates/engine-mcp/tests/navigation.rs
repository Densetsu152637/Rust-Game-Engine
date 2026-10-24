mod common;
use common::*;
use serde_json::json;
use std::fs;
fn nav_fixture() -> tempfile::TempDir {
    let dir = fixture();
    fs::write(
        dir.path().join("assets/triangle.mesh"),
        json!({"vertices":[
        {"position":[0,0,0],"uv":[0,0],"color":[1,1,1,1]},
        {"position":[10,0,0],"uv":[0,0],"color":[1,1,1,1]},
        {"position":[10,0,10],"uv":[0,0],"color":[1,1,1,1]},
        {"position":[0,0,10],"uv":[0,0],"color":[1,1,1,1]}],"indices":[0,1,2,0,2,3]})
        .to_string(),
    )
    .unwrap();
    dir
}
fn bake_args() -> serde_json::Value {
    json!({"meshes":["asset:mesh"],"output":"assets/nav.json","obstacles":[{"min":[4,-1,3],"max":[6,3,7]}]})
}
#[test]
fn mcp_bakes_queries_and_registers_shared_navigation_asset() {
    let dir = nav_fixture();
    let result = write(dir.path(), "navigation_bake", bake_args());
    assert_eq!(result["ok"], true, "{result}");
    assert!(result["polygons"].as_u64().unwrap() > 0);
    let result = data(
        dir.path(),
        "navigation_query",
        json!({"path":"assets/nav.json","start":[2,0,5],"goal":[8,0,5]}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["status"], "reached");
    assert!(result["points"].as_array().unwrap().len() > 2);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"nav","kind":"navigation","path":"assets/nav.json"}));
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    let info = data(dir.path(), "asset_info", json!({"asset":"nav"}));
    assert!(info["details"]["polygons"].as_u64().unwrap() > 0, "{info}");
    let listed = data(dir.path(), "asset_list", json!({"kind":"navigation"}));
    assert_eq!(listed["count"], 1, "{listed}");
    assert!(
        engine_project::simulation::load_navmesh(&project, "nav")
            .unwrap()
            .polygon_count()
            > 0
    );
    let result = data(
        dir.path(),
        "navigation_query",
        json!({"path":"assets/nav.json","start":[2,0,5],"goal":[8,0,5],"query":{"max_expansions":0}}),
    );
    assert_eq!(result["status"], "budget_exceeded", "{result}");
}
#[test]
fn bake_is_write_gated_confined_and_never_overwrites() {
    let dir = nav_fixture();
    let result = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"navigation_bake","arguments":bake_args()}),
        )],
    );
    assert_eq!(result[0]["error"]["code"], -32602);
    assert!(!dir.path().join("assets/nav.json").exists());
    for output in [
        "../nav.json",
        ".git/nav.json",
        "target/nav.json",
        "assets/triangle.mesh",
        "C:/nav.json",
    ] {
        let mut args = bake_args();
        args["output"] = json!(output);
        assert_eq!(write(dir.path(), "navigation_bake", args)["ok"], false);
    }
    assert_eq!(
        write(dir.path(), "navigation_bake", bake_args())["ok"],
        true
    );
    let before = fs::read(dir.path().join("assets/nav.json")).unwrap();
    assert_eq!(
        write(dir.path(), "navigation_bake", bake_args())["ok"],
        false
    );
    assert_eq!(
        fs::read(dir.path().join("assets/nav.json")).unwrap(),
        before
    );
    let result = data(
        dir.path(),
        "navigation_query",
        json!({"path":"../nav.json","start":[0,0,0],"goal":[1,0,1]}),
    );
    assert_eq!(result["ok"], false);
    let mut args = bake_args();
    args["output"] = json!("assets/invalid.json");
    args["settings"] = json!({"cell_size":0});
    assert_eq!(write(dir.path(), "navigation_bake", args)["ok"], false);
    assert!(!dir.path().join("assets/invalid.json").exists());
}
