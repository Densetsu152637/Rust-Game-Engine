//! `terrain_*` tools and the `engine.Terrain` component through `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

fn register(root: &Path, asset: Value) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
    manifest["assets"].as_array_mut().unwrap().push(asset);
    fs::write(root.join("project.json"), manifest.to_string()).unwrap();
}
fn file_bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}
/// A 33 x 33 terrain with layers grass and rock, registered as asset `land`.
fn land(dir: &tempfile::TempDir) -> Value {
    let made = write(
        dir.path(),
        "terrain_create",
        json!({"output":"assets/land.terrain","columns":33,"rows":33,"spacing":1.0,"height":2.0,
            "layers":[{"name":"grass","tint":[0.2,0.6,0.2,1.0]},{"name":"rock","uv_tiling":4.0}]}),
    );
    assert_eq!(made["ok"], true, "{made}");
    assert_eq!(made["asset"]["kind"], "terrain");
    assert_eq!(made["samples"], json!([33, 33]));
    register(
        dir.path(),
        json!({"id":"land","kind":"terrain","path":"assets/land.terrain"}),
    );
    made
}
fn info(dir: &tempfile::TempDir, samples: Value) -> Value {
    let info = data(
        dir.path(),
        "terrain_info",
        json!({"path":"land","samples":samples}),
    );
    assert_eq!(info["ok"], true, "{info}");
    info
}

#[test]
fn create_sculpt_paint_and_sample_a_terrain() {
    let dir = fixture();
    land(&dir);
    let base = info(&dir, json!([[16, 16]]));
    assert_eq!(base["columns"], 33);
    assert_eq!(base["extent"], json!([32.0, 32.0]));
    assert_eq!(base["height_range"], json!([2.0, 2.0]));
    assert_eq!(base["samples"][0]["height"], 2.0);
    assert_eq!(
        base["samples"][0]["weights"],
        json!({"grass":1.0,"rock":0.0})
    );
    let sculpted = write(
        dir.path(),
        "terrain_sculpt",
        json!({"path":"land","expected_hash":base["hash"],"ops":[
            {"op":"raise","center":[16,16],"radius":6,"strength":3,"falloff":0.5},
            {"op":"set_height","center":[4,4],"radius":2,"height":-1},
            {"op":"smooth","center":[16,16],"radius":3,"strength":0.5},
            {"op":"noise","center":[28,28],"radius":3,"strength":1,"seed":2,"frequency":0.5}]}),
    );
    assert_eq!(sculpted["ok"], true, "{sculpted}");
    assert!(sculpted["changed"].as_u64().unwrap() > 50);
    let after = info(&dir, json!([[16, 16], [4, 4], [16, 23]]));
    assert!(after["samples"][0]["height"].as_f64().unwrap() > 4.5);
    assert_eq!(after["samples"][1]["height"], -1.0);
    assert_eq!(after["samples"][2]["height"], 2.0, "outside every brush");
    // A stale hash is rejected without writing.
    let before = file_bytes(dir.path(), "assets/land.terrain");
    let stale = write(
        dir.path(),
        "terrain_sculpt",
        json!({"path":"land","expected_hash":base["hash"],"ops":[{"op":"raise","center":[1,1],"radius":1}]}),
    );
    assert_eq!(error_code(&stale), "stale_hash");
    // All or nothing: a bad op later in the list leaves the file unchanged.
    let partial = write(
        dir.path(),
        "terrain_sculpt",
        json!({"path":"land","ops":[{"op":"raise","center":[1,1],"radius":1},
            {"op":"smooth","center":[1,1],"radius":1,"strength":5}]}),
    );
    assert_eq!(error_code(&partial), "terrain.brush.invalid");
    assert!(
        partial["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("ops[1]")
    );
    assert_eq!(file_bytes(dir.path(), "assets/land.terrain"), before);
    // Paint rock at full strength in the centre, add a layer, cut holes.
    let painted = write(
        dir.path(),
        "terrain_paint",
        json!({"path":"land","ops":[
            {"op":"paint","layer":"rock","center":[10,10],"radius":3,"strength":1,"falloff":0},
            {"op":"add_layer","name":"snow","tint":[1,1,1,1]},
            {"op":"set_layer","name":"grass","texture":"asset:texture"}]}),
    );
    assert_eq!(painted["ok"], true, "{painted}");
    let holes = write(
        dir.path(),
        "terrain_holes",
        json!({"path":"land","ops":[{"center":[20.0,20.0],"radius":0.75}]}),
    );
    assert_eq!(holes["ok"], true, "{holes}");
    assert_eq!(holes["changed"], 4);
    assert_eq!(holes["region"], json!({"min":[19,19],"max":[21,21]}));
    let final_info = info(&dir, json!([[10, 10], [19.5, 19.5], [30, 30]]));
    let samples = &final_info["samples"];
    assert_eq!(
        samples[0]["weights"],
        json!({"grass":0.0,"rock":1.0,"snow":0.0})
    );
    assert_eq!(samples[1]["hole"], true);
    assert_eq!(samples[2]["hole"], false);
    assert_eq!(final_info["holes"], 4);
    assert_eq!(final_info["layers"][0]["texture"], "asset:texture");
    assert_eq!(final_info["layers"].as_array().unwrap().len(), 3);
    // The asset loads through the project like the runtime does.
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    let field = engine_project::world::terrain::load_terrain(&project, "land").unwrap();
    assert!(field.is_hole(19, 19));
    // asset_info describes terrain assets.
    let asset = data(dir.path(), "asset_info", json!({"asset":"land"}));
    assert_eq!(asset["details"]["samples"], json!([33, 33]), "{asset}");
}

#[test]
fn inputs_are_bounded_and_files_are_never_clobbered() {
    let dir = fixture();
    land(&dir);
    let again = write(
        dir.path(),
        "terrain_create",
        json!({"output":"assets/land.terrain","columns":33,"rows":33}),
    );
    assert_eq!(error_code(&again), "invalid_argument");
    for bad in [
        json!({"output":"assets/big.terrain","columns":4097,"rows":4097}),
        json!({"output":"assets/zero.terrain","columns":1,"rows":4}),
        json!({"output":"assets/x.json","columns":4,"rows":4}),
        json!({"output":"assets/l.terrain","columns":4,"rows":4,"layers":[{"name":"a"},{"name":"a"}]}),
        json!({"output":"assets/n.terrain","columns":4,"rows":4,"noise":{"amplitude":1,"frequency":0}}),
    ] {
        let result = write(dir.path(), "terrain_create", bad.clone());
        assert_eq!(result["ok"], false, "{bad}");
    }
    let ops: Vec<Value> = (0..257)
        .map(|_| json!({"op":"raise","center":[1,1],"radius":1}))
        .collect();
    let many = write(
        dir.path(),
        "terrain_sculpt",
        json!({"path":"land","ops":ops}),
    );
    assert_eq!(error_code(&many), "invalid_argument");
    let unknown = write(
        dir.path(),
        "terrain_paint",
        json!({"path":"land","ops":[{"op":"paint","layer":"lava","center":[1,1],"radius":1}]}),
    );
    assert_eq!(error_code(&unknown), "terrain.layer.unknown");
    // A non-terrain file is not a target, and a corrupt one is reported, not rewritten.
    let wrong = write(
        dir.path(),
        "terrain_holes",
        json!({"path":"assets/white.ppm","ops":[{"center":[1,1],"radius":1}]}),
    );
    assert_eq!(error_code(&wrong), "invalid_argument");
    fs::write(dir.path().join("assets/bad.terrain"), b"ETRN garbage").unwrap();
    let corrupt = data(
        dir.path(),
        "terrain_info",
        json!({"path":"assets/bad.terrain"}),
    );
    assert_eq!(error_code(&corrupt), "terrain.asset.malformed");
    let points: Vec<Value> = (0..257).map(|i| json!([i, 0])).collect();
    let too_many = data(
        dir.path(),
        "terrain_info",
        json!({"path":"land","samples":points}),
    );
    assert_eq!(error_code(&too_many), "invalid_argument");
}

#[test]
fn terrain_navmesh_writes_a_navigation_document_navigation_query_routes() {
    let dir = fixture();
    land(&dir);
    // A wall of holes across x = 16 except a gap near z = 30.
    let ops: Vec<Value> = (0..28)
        .map(|z| json!({"center":[16.5, z as f32 + 0.5],"radius":0.6}))
        .collect();
    let cut = write(
        dir.path(),
        "terrain_holes",
        json!({"path":"land","ops":ops}),
    );
    assert_eq!(cut["ok"], true, "{cut}");
    let baked = write(
        dir.path(),
        "terrain_navmesh",
        json!({"path":"land","output":"assets/land.nav.json","origin":[100.0,0.0,0.0],
            "settings":{"cell_size":0.5,"agent_radius":0.25}}),
    );
    assert_eq!(baked["ok"], true, "{baked}");
    assert_eq!(baked["asset"]["kind"], "navigation");
    assert!(baked["hole_cells"].as_u64().unwrap() > 0);
    assert!(baked["polygons"].as_u64().unwrap() > 1000);
    let route = data(
        dir.path(),
        "navigation_query",
        json!({"path":"assets/land.nav.json","start":[104.0,2.0,4.0],"goal":[128.0,2.0,4.0]}),
    );
    assert_eq!(route["ok"], true, "{route}");
    assert_eq!(route["status"], "reached", "{route}");
    let points = route["points"].as_array().unwrap();
    // The route detours through the gap at z > 28.
    assert!(
        points.iter().any(|p| p[2].as_f64().unwrap() > 28.0),
        "{points:?}"
    );
    let again = write(
        dir.path(),
        "terrain_navmesh",
        json!({"path":"land","output":"assets/land.nav.json"}),
    );
    assert_eq!(error_code(&again), "invalid_argument");
    let huge = write(
        dir.path(),
        "terrain_navmesh",
        json!({"path":"land","output":"assets/fine.nav.json","settings":{"cell_size":0.1}}),
    );
    assert_eq!(error_code(&huge), "terrain.navigation.capacity");
}

#[test]
fn engine_terrain_component_is_editable_through_component_set() {
    let dir = fixture();
    land(&dir);
    let revision = || revision(dir.path(), START);
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.Terrain"}),
    );
    let properties = &schema["components"][0]["schema"]["properties"]["data"]["properties"];
    for key in [
        "asset",
        "lod",
        "update_distance",
        "materials",
        "collision",
        "render_mode",
        "visible",
        "surface",
    ] {
        assert!(properties.get(key).is_some(), "{key}: {schema}");
    }
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Terrain",
            "value":{"version":1,"data":{"asset":"land","lod":{"node_resolution":16,"lod_distance":5},
                "render_mode":"cpu","collision":{"tile_cells":16,"layer":2},
                "materials":[{"tint":[1,0,0,1]}]}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.Terrain"]["data"];
    assert_eq!(stored["lod"]["node_resolution"], 16);
    assert_eq!(stored["render_mode"], "cpu");
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Terrain",
            "value":{"version":1,"data":{"asset":"land","visible":false,"render_mode":"gpu"}}}),
    );
    assert_eq!(merged["ok"], true, "{merged}");
    let stored = &merged["entity"]["components"]["custom"]["engine.Terrain"]["data"];
    assert_eq!(stored["render_mode"], "gpu");
    assert_eq!(stored["visible"], false);
    let before = file_bytes(dir.path(), START);
    for bad in [
        json!({"asset":"land","lod":{"node_resolution":12}}),
        json!({"asset":"land","lod":{"lod_distance":1}}),
        json!({"asset":"land","render_mode":"raytraced"}),
        json!({"asset":"land","unknown":true}),
        json!({"asset":"land","collision":{"tile_cells":4}}),
        json!({"asset":"missing"}),
    ] {
        let result = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Terrain",
                "value":{"version":1,"data":bad}}),
        );
        assert_eq!(result["ok"], false, "{bad}");
        assert_eq!(file_bytes(dir.path(), START), before);
    }
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
}
