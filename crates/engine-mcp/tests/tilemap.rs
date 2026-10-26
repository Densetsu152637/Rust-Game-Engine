mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

/// The shared fixture plus a 64x64 atlas texture registered as asset `atlas`.
fn tile_fixture() -> tempfile::TempDir {
    let dir = fixture();
    let mut ppm = String::from("P3\n64 64\n255\n");
    ppm.push_str(&"200 200 200\n".repeat(64 * 64));
    fs::write(dir.path().join("assets/atlas.ppm"), ppm).unwrap();
    register(
        dir.path(),
        json!({"id":"atlas","kind":"texture","path":"assets/atlas.ppm"}),
    );
    dir
}
fn register(root: &Path, asset: Value) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
    manifest["assets"].as_array_mut().unwrap().push(asset);
    fs::write(root.join("project.json"), manifest.to_string()).unwrap();
}
fn file_bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}

/// Tileset with 16 plain grid tiles (1..=16), a full-collision wall (17) and a four-neighbour
/// rule tile (100) over tiles 1..=16, registered as asset `tiles`.
fn tileset(dir: &tempfile::TempDir) {
    let made = write(
        dir.path(),
        "tilemap_create_tileset",
        json!({"output":"assets/tiles.json","texture":"atlas","name":"test","tile_size":[16,16],
            "grid":{"first_id":1,"columns":4,"rows":4},
            "tiles":[{"id":17,"name":"wall","source":[0,0,16,16],"collision":"full",
                "properties":{"solid":true}}]}),
    );
    assert_eq!(made["ok"], true, "{made}");
    assert_eq!(made["tiles"], 17);
    assert_eq!(made["asset"]["kind"], "tileset");
    register(
        dir.path(),
        json!({"id":"tiles","kind":"tileset","path":"assets/tiles.json"}),
    );
    let rules = write(
        dir.path(),
        "tilemap_define_rules",
        json!({"path":"tiles","rule_tile":{"id":100,"name":"path","preset":"four_neighbour16","first_tile":1}}),
    );
    assert_eq!(rules["ok"], true, "{rules}");
    assert_eq!(rules["rules"], 16);
}
fn level(dir: &tempfile::TempDir) {
    let made = write(
        dir.path(),
        "tilemap_create",
        json!({"output":"assets/level.json","tileset":"tiles",
            "bounds":{"min":[-8,-8],"max":[8,8]},
            "layers":[{"name":"ground"},{"name":"walls","z_order":5,"opacity":0.5}]}),
    );
    assert_eq!(made["ok"], true, "{made}");
    assert_eq!(made["layers"], json!(["ground", "walls"]));
    assert_eq!(
        made["component"]["engine.Tilemap"]["data"]["tileset"],
        "tiles"
    );
    register(
        dir.path(),
        json!({"id":"level","kind":"tilemap","path":"assets/level.json"}),
    );
}

#[test]
fn author_paint_and_query_a_tilemap_end_to_end() {
    let dir = tile_fixture();
    tileset(&dir);
    level(&dir);
    let painted = write(
        dir.path(),
        "tilemap_paint",
        json!({"path":"level","tileset":"tiles","layer":"ground","ops":[
            {"op":"rect","rect":{"min":[0,0],"max":[4,4]},"tile":1},
            {"op":"set","cell":[0,0],"tile":100},
            {"op":"set","cell":[1,0],"tile":100},
            {"op":"set","cell":[2,0],"tile":100},
            {"op":"erase","cell":[3,3]},
            {"op":"flood","cell":[-8,-8],"tile":2,"limit":{"min":[-8,-8],"max":[-6,-6]}}]}),
    );
    assert_eq!(painted["ok"], true, "{painted}");
    // 16 rect cells + 3 rule tiles over existing cells (no new cells) - 1 erase + 4 flood.
    assert_eq!(painted["filled_cells"], 16 - 1 + 4);
    assert!(painted["dirty_chunks"].as_u64().unwrap() >= 2);
    let walls = write(
        dir.path(),
        "tilemap_paint",
        json!({"path":"level","tileset":"tiles","layer":"walls","ops":[
            {"op":"layer","name":"walls","opacity":0.75},
            {"op":"rect","rect":{"min":[5,-4],"max":[6,4]},"tile":17},
            {"op":"layer","name":"decor","z_order":9}]}),
    );
    assert_eq!(walls["ok"], true, "{walls}");
    assert_eq!(walls["changed"], 8);
    // Rule tiles resolve with the runtime rules: E, E+W and W neighbour masks.
    let cell = |x: i32| {
        data(
            dir.path(),
            "tilemap_query",
            json!({"path":"level","tileset":"tiles","layer":"ground","mode":"cell","cell":[x,0]}),
        )
    };
    let (a, b, c) = (cell(0), cell(1), cell(2));
    assert_eq!(a["cell"]["tile"], 100, "{a}");
    assert_eq!(
        (
            a["cell"]["shown"].as_u64(),
            b["cell"]["shown"].as_u64(),
            c["cell"]["shown"].as_u64()
        ),
        (Some(3), Some(11), Some(9))
    );
    let plain = cell(3);
    assert_eq!(
        (
            plain["cell"]["tile"].as_u64(),
            plain["cell"]["shown"].as_u64()
        ),
        (Some(1), Some(1))
    );
    // Rectangle listing is sorted bottom-up, left-to-right.
    let listed = data(
        dir.path(),
        "tilemap_query",
        json!({"path":"level","tileset":"tiles","layer":"ground","mode":"rect","rect":{"min":[0,0],"max":[2,2]}}),
    );
    let cells: Vec<_> = listed["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["cell"].clone())
        .collect();
    assert_eq!(
        cells,
        vec![json!([0, 0]), json!([1, 0]), json!([0, 1]), json!([1, 1])]
    );
    // Collision: the 1 x 8 wall column merges into one box of area 8 with one outline loop.
    let collision = data(
        dir.path(),
        "tilemap_query",
        json!({"path":"level","tileset":"tiles","layer":"walls","mode":"collision"}),
    );
    assert_eq!(
        collision["boxes"],
        json!([{"min":[5.0,-4.0],"max":[6.0,4.0]}])
    );
    assert_eq!(collision["outlines"].as_array().unwrap().len(), 1);
    assert_eq!(collision["outlines"][0].as_array().unwrap().len(), 4);
    // Info for both document kinds and the asset tools.
    let tileset_info = data(dir.path(), "tilemap_info", json!({"path":"tiles"}));
    assert_eq!(tileset_info["kind"], "tileset");
    assert_eq!(tileset_info["details"]["tiles"], 17);
    assert_eq!(
        tileset_info["details"]["collision"],
        json!({"full":1,"none":16})
    );
    assert_eq!(tileset_info["details"]["rule_tiles"][0]["rules"], 16);
    let map_info = data(
        dir.path(),
        "tilemap_info",
        json!({"path":"assets/level.json"}),
    );
    assert_eq!(map_info["kind"], "tilemap");
    let layers = map_info["details"]["layers"].as_array().unwrap();
    assert_eq!(
        layers
            .iter()
            .map(|l| l["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ground", "walls", "decor"]
    );
    assert_eq!(layers[1]["opacity"], 0.75);
    assert_eq!(layers[1]["cells"], 8);
    let asset = data(dir.path(), "asset_info", json!({"asset":"tiles"}));
    assert_eq!(asset["details"]["tiles"], 17);
    let listed = data(dir.path(), "asset_list", json!({"kind":"tilemap"}));
    assert_eq!(listed["count"], 1);
    // The project still validates with both assets registered.
    let validated = data(dir.path(), "project_validate", json!({}));
    assert_eq!(validated["ok"], true, "{validated}");
}

#[test]
fn paint_is_all_or_nothing_and_validates_everything() {
    let dir = tile_fixture();
    tileset(&dir);
    level(&dir);
    let before = file_bytes(dir.path(), "assets/level.json");
    let attempt = |ops: Value, layer: &str| {
        write(
            dir.path(),
            "tilemap_paint",
            json!({"path":"level","tileset":"tiles","layer":layer,"ops":ops}),
        )
    };
    for (ops, layer, code) in [
        (
            json!([{"op":"set","cell":[0,0],"tile":1},{"op":"set","cell":[99,0],"tile":1}]),
            "ground",
            "tilemap.bounds",
        ),
        (
            json!([{"op":"set","cell":[0,0],"tile":999}]),
            "ground",
            "tilemap.tile",
        ),
        (
            json!([{"op":"set","cell":[0,0],"tile":1}]),
            "missing",
            "unknown_layer",
        ),
        (
            json!([{"op":"rect","rect":{"min":[-8,-8],"max":[9,9]},"tile":1}]),
            "ground",
            "tilemap.fill",
        ),
        (json!([{"op":"explode"}]), "ground", "invalid_argument"),
        (json!([{"op":"set","tile":1}]), "ground", "invalid_argument"),
        (
            json!([{"op":"set","cell":[0,0],"tile":1,"flags":200}]),
            "ground",
            "tilemap.tile",
        ),
        (json!([]), "ground", "invalid_argument"),
    ] {
        let result = attempt(ops.clone(), layer);
        assert_eq!(error_code(&result), code, "{ops} -> {result}");
        assert_eq!(file_bytes(dir.path(), "assets/level.json"), before, "{ops}");
    }
    // Outside targets and unknown fields are refused.
    for path in ["../level.json", "assets/missing.json", ".git/config"] {
        let result = write(
            dir.path(),
            "tilemap_paint",
            json!({"path":path,"tileset":"tiles","layer":"ground","ops":[{"op":"set","cell":[0,0],"tile":1}]}),
        );
        assert_eq!(result["ok"], false, "{path}");
    }
    let result = attempt(
        json!([{"op":"set","cell":[0,0],"tile":1,"bogus":1}]),
        "ground",
    );
    assert_eq!(result["ok"], false);
    assert_eq!(file_bytes(dir.path(), "assets/level.json"), before);
}

#[test]
fn creation_never_overwrites_and_rules_are_validated() {
    let dir = tile_fixture();
    tileset(&dir);
    let again = write(
        dir.path(),
        "tilemap_create_tileset",
        json!({"output":"assets/tiles.json","texture":"atlas","tile_size":[16,16],"grid":{"columns":1,"rows":1}}),
    );
    assert_eq!(again["ok"], false);
    for (args, why) in [
        (
            json!({"output":"assets/a.json","texture":"nope","tile_size":[16,16],"grid":{"columns":1,"rows":1}}),
            "unknown texture",
        ),
        (
            json!({"output":"assets/a.json","texture":"atlas","tile_size":[16,16]}),
            "no tiles",
        ),
        (
            json!({"output":"assets/a.json","texture":"atlas","tile_size":[16,16],"grid":{"columns":5,"rows":1}}),
            "grid exceeds atlas",
        ),
        (
            json!({"output":"assets/a.txt","texture":"atlas","tile_size":[16,16],"grid":{"columns":1,"rows":1}}),
            "not json",
        ),
        (
            json!({"output":"../a.json","texture":"atlas","tile_size":[16,16],"grid":{"columns":1,"rows":1}}),
            "outside",
        ),
        (
            json!({"output":"missing/a.json","texture":"atlas","tile_size":[16,16],"grid":{"columns":1,"rows":1}}),
            "no directory",
        ),
        (
            json!({"output":"assets/a.json","texture":"atlas","tile_size":[16,16],"tiles":[{"id":1,"source":[0,0,16,16]},{"id":1,"source":[0,0,16,16]}]}),
            "duplicate id",
        ),
    ] {
        let result = write(dir.path(), "tilemap_create_tileset", args);
        assert_eq!(result["ok"], false, "{why}: {result}");
    }
    assert!(!dir.path().join("assets/a.json").exists());
    let before = file_bytes(dir.path(), "assets/tiles.json");
    // blob47 needs 47 concrete tiles; the tileset has only 17.
    let blob = write(
        dir.path(),
        "tilemap_define_rules",
        json!({"path":"tiles","rule_tile":{"id":101,"preset":"blob47","first_tile":1}}),
    );
    assert_eq!(error_code(&blob), "tileset.rule", "{blob}");
    assert_eq!(file_bytes(dir.path(), "assets/tiles.json"), before);
    // A custom rule with the first match winning and a rotated pattern.
    let custom = write(
        dir.path(),
        "tilemap_define_rules",
        json!({"path":"tiles","rule_tile":{"id":101,"name":"cap","default_tile":1,"rules":[
            {"neighbours":["any","this","any","not_this","not_this","any","not_this","any"],"output":2,"transform":"rotated"}]}}),
    );
    assert_eq!(custom["ok"], true, "{custom}");
    assert_eq!(custom["rules"], 1);
    for bad in [
        json!({"id":101}),
        json!({"id":101,"preset":"blob47"}),
        json!({"id":101,"preset":"nope","first_tile":1}),
        json!({"id":100,"default_tile":999,"rules":[]}),
    ] {
        let result = write(
            dir.path(),
            "tilemap_define_rules",
            json!({"path":"tiles","rule_tile":bad}),
        );
        assert_eq!(result["ok"], false, "{bad}");
    }
    let tilemap_again = write(
        dir.path(),
        "tilemap_create",
        json!({"output":"assets/level.json","tileset":"tiles"}),
    );
    assert_eq!(tilemap_again["ok"], true);
    let tilemap_overwrite = write(
        dir.path(),
        "tilemap_create",
        json!({"output":"assets/level.json","tileset":"tiles"}),
    );
    assert_eq!(tilemap_overwrite["ok"], false);
    let bad_layers = write(
        dir.path(),
        "tilemap_create",
        json!({"output":"assets/l2.json","tileset":"tiles","layers":[{"name":"a"},{"name":"a"}]}),
    );
    assert_eq!(bad_layers["ok"], false);
    let unknown_tileset = write(
        dir.path(),
        "tilemap_create",
        json!({"output":"assets/l3.json","tileset":"missing-tileset"}),
    );
    assert_eq!(unknown_tileset["ok"], false);
}

#[test]
fn edit_tools_reject_stale_expected_hash_and_accept_current_ones() {
    let dir = tile_fixture();
    tileset(&dir);
    level(&dir);
    let paint = |hash: &str, tile: u32| {
        write(
            dir.path(),
            "tilemap_paint",
            json!({"path":"level","tileset":"tiles","layer":"ground","expected_hash":hash,
                "ops":[{"op":"set","cell":[0,0],"tile":tile}]}),
        )
    };
    let current = data(dir.path(), "tilemap_info", json!({"path":"level"}))["details"]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = paint(&current, 1);
    assert_eq!(first["ok"], true, "{first}");
    // The old hash is now stale: nothing is written.
    let before = file_bytes(dir.path(), "assets/level.json");
    let stale = paint(&current, 2);
    assert_eq!(error_code(&stale), "stale_hash", "{stale}");
    assert_eq!(file_bytes(dir.path(), "assets/level.json"), before);
    // The hash returned by the write chains into the next edit.
    let chained = paint(first["hash"].as_str().unwrap(), 2);
    assert_eq!(chained["ok"], true, "{chained}");
    let tiles_hash = data(dir.path(), "tilemap_info", json!({"path":"tiles"}))["details"]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let rule = |hash: &str| {
        write(
            dir.path(),
            "tilemap_define_rules",
            json!({"path":"tiles","expected_hash":hash,
                "rule_tile":{"id":101,"name":"cap","default_tile":1,"rules":[
                    {"neighbours":["any","this","any","not_this","not_this","any","not_this","any"],"output":2}]}}),
        )
    };
    let before = file_bytes(dir.path(), "assets/tiles.json");
    let stale = rule("0000");
    assert_eq!(error_code(&stale), "stale_hash", "{stale}");
    assert_eq!(file_bytes(dir.path(), "assets/tiles.json"), before);
    let accepted = rule(&tiles_hash);
    assert_eq!(accepted["ok"], true, "{accepted}");
}

#[test]
fn write_tools_are_gated_and_read_tools_stay_available() {
    let dir = tile_fixture();
    tileset(&dir);
    level(&dir);
    let before = file_bytes(dir.path(), "assets/level.json");
    for (tool, args) in [
        (
            "tilemap_paint",
            json!({"path":"level","tileset":"tiles","layer":"ground","ops":[{"op":"set","cell":[0,0],"tile":1}]}),
        ),
        (
            "tilemap_create",
            json!({"output":"assets/x.json","tileset":"tiles"}),
        ),
        (
            "tilemap_define_rules",
            json!({"path":"tiles","rule_tile":{"id":101,"default_tile":1,"rules":[]}}),
        ),
        (
            "tilemap_create_tileset",
            json!({"output":"assets/x.json","texture":"atlas","tile_size":[16,16],"grid":{"columns":1,"rows":1}}),
        ),
    ] {
        let responses = exchange(
            dir.path(),
            read_only(),
            &[modern(
                1,
                "tools/call",
                json!({"name":tool,"arguments":args}),
            )],
        );
        assert_eq!(responses[0]["error"]["code"], -32602, "{tool}");
    }
    assert_eq!(file_bytes(dir.path(), "assets/level.json"), before);
    let info = data(dir.path(), "tilemap_info", json!({"path":"level"}));
    assert_eq!(info["kind"], "tilemap");
    let query = data(
        dir.path(),
        "tilemap_query",
        json!({"path":"level","tileset":"tiles","layer":"nope","mode":"cell","cell":[0,0]}),
    );
    assert_eq!(error_code(&query), "unknown_layer");
}

#[test]
fn engine_tilemap_component_is_editable_through_component_set() {
    let dir = tile_fixture();
    tileset(&dir);
    level(&dir);
    let revision = || revision(dir.path(), START);
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.Tilemap"}),
    );
    let properties = &schema["components"][0]["schema"]["properties"]["data"]["properties"];
    for key in [
        "tileset",
        "map",
        "inline",
        "cell_size",
        "tint",
        "visible",
        "render_layer",
        "collision",
    ] {
        assert!(properties.get(key).is_some(), "schema documents {key}");
    }
    assert_eq!(schema["components"][0]["requires"], json!(["Transform"]));
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Tilemap",
            "value":{"version":1,"data":{"tileset":"tiles","map":"level","cell_size":[2,2],
                "collision":{"layer":2,"layers":["walls"]}}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.Tilemap"]["data"];
    assert_eq!(stored["cell_size"], json!([2, 2]));
    assert_eq!(stored["collision"]["layers"], json!(["walls"]));
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Tilemap",
            "value":{"version":1,"data":{"visible":false}},"merge":true}),
    );
    // Scene validation now rejects a tilemap without a tileset, merged or not.
    assert_eq!(error_code(&merged), "tilemap.component.invalid", "{merged}");
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Tilemap",
            "value":{"version":1,"data":{"tileset":"tiles","visible":false}},"merge":true}),
    );
    assert_eq!(merged["ok"], true, "{merged}");
    let before = file_bytes(dir.path(), START);
    for bad in [
        json!({"tileset":"tiles","cell_size":[0,1]}),
        json!({"tileset":"tiles","unknown":true}),
        json!({"tileset":"tiles","tint":[2,0,0,0]}),
        json!({"tileset":7}),
    ] {
        let result = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(),"entity":"player","component":"engine.Tilemap",
                "value":{"version":1,"data":bad}}),
        );
        assert_eq!(result["ok"], false, "{bad}");
        assert_eq!(file_bytes(dir.path(), START), before);
    }
    // The edited scene loads in the runtime-facing model.
    let project = engine_project::Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
}
