//! `foliage_*` tools and the `engine.Foliage` component through `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";
const MESH: &str = "CGMESH 1\nvertex -0.2 0 0 0 1\nvertex 0.2 0 0 1 1\nvertex 0 1 0 0.5 0\n";

fn register(root: &Path, asset: Value) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
    manifest["assets"].as_array_mut().unwrap().push(asset);
    fs::write(root.join("project.json"), manifest.to_string()).unwrap();
}
fn file_bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}
/// A project with the `grass` and `rock` meshes and a 65 x 65 terrain `land` whose left half
/// (x < 32) is steep (1.5 m per metre) and right half flat; layer `mud` covers z < 32.
fn project() -> tempfile::TempDir {
    let dir = fixture();
    fs::write(dir.path().join("assets/grass.cgmesh"), MESH).unwrap();
    register(
        dir.path(),
        json!({"id":"grass","kind":"mesh","path":"assets/grass.cgmesh"}),
    );
    let mut field = engine_terrain::Heightfield::from_heights(
        65,
        65,
        1.0,
        (0..65 * 65)
            .map(|i| {
                let c = (i % 65) as f32;
                if c < 32.0 { c * 1.5 } else { 48.0 }
            })
            .collect(),
    )
    .unwrap();
    field
        .add_layer(engine_terrain::TerrainLayer::new(
            "mud",
            [0.4, 0.3, 0.2, 1.0],
        ))
        .unwrap();
    for r in 0..65 {
        for c in 0..65 {
            field
                .set_sample_weights(c, r, if r < 32 { &[0, 255] } else { &[255, 0] })
                .unwrap();
        }
    }
    for r in 40..48 {
        for c in 40..48 {
            field.set_hole(c, r, true).unwrap();
        }
    }
    fs::write(
        dir.path().join("assets/land.terrain"),
        engine_terrain::asset::encode(&field),
    )
    .unwrap();
    register(
        dir.path(),
        json!({"id":"land","kind":"terrain","path":"assets/land.terrain"}),
    );
    dir
}
fn grass(density: f64) -> Value {
    json!({"name":"grass","mesh":"grass","density":density,"slope_degrees":[0,30]})
}
fn info(dir: &tempfile::TempDir, extra: Value) -> Value {
    let mut args = json!({"path":"assets/field.foliage"});
    for (k, v) in extra.as_object().into_iter().flatten() {
        args[k] = v.clone();
    }
    let info = data(dir.path(), "foliage_info", args);
    assert_eq!(info["ok"], true, "{info}");
    info
}

#[test]
fn scatter_on_a_terrain_respects_slope_holes_and_density_and_reports_stats() {
    let dir = project();
    let made = write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/field.foliage","types":[grass(1.0)],"seed":4,
            "center":[32,64,32],"size":[64,128,64],"ground":{"terrain":"land"}}),
    );
    assert_eq!(made["ok"], true, "{made}");
    assert_eq!(made["asset"]["kind"], "foliage");
    // Flat right half 33 x 64 minus the 8 x 8 hole: about 2048 instances.
    let n = made["instances"].as_u64().unwrap() as f64;
    assert!((n - 2048.0).abs() < 6.0 * 2048f64.sqrt(), "{n}");
    assert_eq!(made["per_type"][0]["accepted"].as_u64().unwrap() as f64, n);
    assert!(made["per_type"][0]["rejected_slope"].as_u64().unwrap() > 1000);
    assert!(made["per_type"][0]["rejected_hole"].as_u64().unwrap() > 20);
    let stats = info(&dir, json!({}));
    assert_eq!(stats["instances"].as_f64().unwrap(), n);
    assert_eq!(stats["hash"], made["hash"]);
    assert_eq!(stats["types"][0]["name"], "grass");
    assert!(stats["bounds"]["min"][0].as_f64().unwrap() >= 31.9);
    // The hole stays empty.
    let hole = info(&dir, json!({"area":{"center":[44.0,44.0],"radius":3.5}}));
    assert_eq!(hole["area"]["instances"], 0);
    let flat = info(&dir, json!({"area":{"center":[52.0,10.0],"radius":6.0}}));
    let density = flat["area"]["density_per_m2"].as_f64().unwrap();
    assert!((density - 1.0).abs() < 0.2, "{density}");
    // Never overwrites.
    let again = write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/field.foliage","types":[grass(1.0)]}),
    );
    assert_eq!(again["ok"], false);
    // Deterministic: the same call on a new path gives the same bytes.
    let twin = write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/twin.foliage","types":[grass(1.0)],"seed":4,
            "center":[32,64,32],"size":[64,128,64],"ground":{"terrain":"land"}}),
    );
    assert_eq!(twin["hash"], made["hash"]);
}

#[test]
fn a_paint_layer_drives_density_and_bad_input_is_rejected() {
    let dir = project();
    let mut reeds = grass(1.0);
    reeds["density_map"] = json!({"kind":"layer","layer":"mud"});
    reeds["slope_degrees"] = json!([0, 90]);
    let made = write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/field.foliage","types":[reeds.clone()],
            "center":[32,64,32],"size":[64,128,64],"ground":{"terrain":"land"}}),
    );
    assert_eq!(made["ok"], true, "{made}");
    let north = info(&dir, json!({"area":{"center":[20.0,16.0],"radius":8.0}}));
    let south = info(&dir, json!({"area":{"center":[20.0,50.0],"radius":8.0}}));
    assert!(north["area"]["instances"].as_u64().unwrap() > 150);
    assert_eq!(south["area"]["instances"], 0);
    for bad in [
        json!({"output":"assets/bad.foliage","types":[{"name":"x","mesh":"grass","density":999}]}),
        json!({"output":"assets/bad.foliage","types":[{"name":"x","mesh":"grass","unknown":1}]}),
        json!({"output":"assets/bad.json","types":[grass(1.0)]}),
        json!({"output":"assets/bad.foliage","types":[grass(1.0)],"ground":{"terrain":"missing"}}),
        json!({"output":"assets/bad.foliage","types":[reeds],"ground":{"height":0}}),
        json!({"output":"assets/bad.foliage","types":[grass(8.0)],"size":[500,100,500],"max_instances":100}),
    ] {
        let result = write(dir.path(), "foliage_scatter", bad.clone());
        assert_eq!(result["ok"], false, "{bad}");
        assert!(!dir.path().join("assets/bad.foliage").exists());
    }
}

#[test]
fn paint_and_erase_edit_the_asset_atomically_and_deterministically() {
    let dir = project();
    write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/field.foliage","types":[grass(0.5)],"seed":1,
            "center":[50,64,10],"size":[30,128,20],"ground":{"terrain":"land"}}),
    );
    let base = info(&dir, json!({}));
    let n0 = base["instances"].as_u64().unwrap();
    let painted = write(
        dir.path(),
        "foliage_paint",
        json!({"path":"assets/field.foliage","expected_hash":base["hash"],
            "ground":{"terrain":"land"},"center":[50,64,10],"size":[30,128,20],
            "ops":[{"op":"add","type":"grass","center":[50,10],"radius":5,"seed":9},
                   {"op":"erase","center":[40,10],"radius":4,"strength":1,"falloff":0}]}),
    );
    assert_eq!(painted["ok"], true, "{painted}");
    assert!(painted["added"].as_u64().unwrap() > 20);
    assert!(painted["removed"].as_u64().unwrap() > 10);
    assert_eq!(
        painted["instances"].as_u64().unwrap(),
        n0 + painted["added"].as_u64().unwrap() - painted["removed"].as_u64().unwrap()
    );
    let cleared = info(&dir, json!({"area":{"center":[40.0,10.0],"radius":3.9}}));
    assert_eq!(cleared["area"]["instances"], 0);
    // Stale hash and bad ops leave the file untouched.
    let before = file_bytes(dir.path(), "assets/field.foliage");
    let stale = write(
        dir.path(),
        "foliage_paint",
        json!({"path":"assets/field.foliage","expected_hash":base["hash"],"ops":[{"op":"erase","center":[50,10],"radius":3}]}),
    );
    assert_eq!(error_code(&stale), "stale_hash");
    let partial = write(
        dir.path(),
        "foliage_paint",
        json!({"path":"assets/field.foliage","ops":[{"op":"erase","center":[50,10],"radius":3},
            {"op":"add","type":"nope","center":[50,10],"radius":3}]}),
    );
    assert_eq!(partial["ok"], false);
    assert!(
        partial["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("ops[1]")
    );
    assert_eq!(file_bytes(dir.path(), "assets/field.foliage"), before);
    // The same stroke again with the same seed adds nothing.
    let repeat = write(
        dir.path(),
        "foliage_paint",
        json!({"path":"assets/field.foliage","ground":{"terrain":"land"},"center":[50,64,10],"size":[30,128,20],
            "ops":[{"op":"add","type":"grass","center":[50,10],"radius":5,"seed":9}]}),
    );
    assert_eq!(repeat["added"], 0, "{repeat}");
}

#[test]
fn engine_foliage_component_is_editable_through_component_set() {
    let dir = project();
    let revision = || revision(dir.path(), START);
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.Foliage"}),
    );
    let properties = &schema["components"][0]["schema"]["properties"]["data"]["properties"];
    for key in [
        "types",
        "terrain",
        "surface",
        "seed",
        "mode",
        "instances",
        "center",
        "size",
        "exclusion_zones",
        "cell_size",
        "max_instances",
        "regenerate",
        "culling",
        "wind",
        "render_mode",
        "cpu_vertex_budget",
        "visible",
    ] {
        assert!(properties.get(key).is_some(), "{key}: {schema}");
    }
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Foliage",
            "value":{"version":1,"data":{"terrain":"","seed":3,"types":[
                {"name":"grass","mesh":"grass","density":2.5,"density_map":{"kind":"layer","layer":"mud","threshold":0.1},
                 "lod_meshes":["grass"],"lod_distances":[30],"slope_degrees":[0,35]}],
                "wind":{"strength":0.4,"direction_degrees":45},"culling":{"max_distance":120},"render_mode":"cpu"}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.Foliage"]["data"];
    assert_eq!(stored["types"][0]["density"], 2.5);
    assert_eq!(stored["wind"]["strength"], 0.4);
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Foliage",
            "value":{"version":1,"data":{"visible":false,"regenerate":2}}}),
    );
    assert_eq!(merged["ok"], true, "{merged}");
    let stored = &merged["entity"]["components"]["custom"]["engine.Foliage"]["data"];
    assert_eq!(stored["visible"], false);
    assert_eq!(stored["regenerate"], 2);
    let before = file_bytes(dir.path(), START);
    for bad in [
        json!({"types":[{"name":"g","mesh":"grass","density":100}]}),
        json!({"types":[{"name":"g","mesh":"missing"}]}),
        json!({"types":[{"name":"g","mesh":"grass","bogus":1}]}),
        json!({"render_mode":"raytraced"}),
        json!({"mode":"asset"}),
        json!({"cell_size":0.1}),
        json!({"wind":{"strength":-1}}),
        json!({"culling":{"hysteresis":0.9}}),
        json!({"unknown":true}),
    ] {
        let result = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Foliage",
                "value":{"version":1,"data":bad}}),
        );
        assert_eq!(result["ok"], false, "{bad}");
        assert_eq!(file_bytes(dir.path(), START), before);
    }
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
}

/// A huge area at a high density would draw millions of candidates before any cap applied:
/// scatter and paint refuse up front with a clear limit error and write nothing.
#[test]
fn candidate_explosions_are_refused_before_drawing_by_scatter_and_paint() {
    let dir = project();
    let dense = json!({"name":"dense","mesh":"grass","density":64.0});
    let started = std::time::Instant::now();
    let refused = write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/huge.foliage","types":[dense.clone()],
            "center":[0,64,0],"size":[500,128,500],"ground":{"height":0}}),
    );
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(refused.to_string().contains("candidates"), "{refused}");
    assert!(!dir.path().join("assets/huge.foliage").exists());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let small = write(
        dir.path(),
        "foliage_scatter",
        json!({"output":"assets/small.foliage","types":[dense],
            "center":[0,64,0],"size":[4,128,4],"ground":{"height":0}}),
    );
    assert_eq!(small["ok"], true, "{small}");
    let stats = data(
        dir.path(),
        "foliage_info",
        json!({"path":"assets/small.foliage"}),
    );
    let painted = write(
        dir.path(),
        "foliage_paint",
        json!({"path":"assets/small.foliage","expected_hash":stats["hash"],
            "ground":{"height":0},
            "ops":[{"op":"add","type":"dense","center":[0,0],"radius":400,"seed":1}]}),
    );
    assert_eq!(painted["ok"], false, "{painted}");
    assert!(painted.to_string().contains("candidates"), "{painted}");
    let after = data(
        dir.path(),
        "foliage_info",
        json!({"path":"assets/small.foliage"}),
    );
    assert_eq!(after["hash"], stats["hash"], "the asset is untouched");
}
