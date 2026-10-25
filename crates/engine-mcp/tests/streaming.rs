//! `streaming_partition`, `streaming_hlod`, `streaming_info` and the streaming components
//! through `component_set`, ending with a partitioned scene running in the engine runtime.
mod common;
use common::*;
use engine_project::world::coordinates::WorldOrigin;
use engine_project::world::streaming as runtime_streaming;
use engine_project::{InputSnapshot, Project, Runtime, Transform};
use engine_streaming::{CellKey, PartitionManifest};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const WORLD: &str = "scenes/world.json";
const MANIFEST: &str = "cells/partition.json";

fn add_asset(root: &Path, asset: Value) {
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
    manifest["assets"].as_array_mut().unwrap().push(asset);
    fs::write(root.join("project.json"), manifest.to_string()).unwrap();
}

/// The shared fixture plus `scenes/world.json`: trees in three cells (one far away, one on a
/// data layer), a child, an excluded rock, a camera, a streaming source and a transform-less
/// entity.
fn world_fixture() -> tempfile::TempDir {
    let dir = fixture();
    let mesh = |position: [f64; 3]| json!({"Transform": {"position": position}, "MeshRenderer": {"mesh": "asset:mesh"}});
    fs::write(
        dir.path().join(WORLD),
        json!({"schema":1,"scene_id":"scene:world","entities":[
            {"id":"cam","name":"cam","components":{"Transform":{"position":[0,0,0]},"Camera3D":{}}},
            {"id":"scout","name":"scout","components":{"Transform":{"position":[0,0,0]},
                "custom":{"engine.StreamingSource":{"version":1,"data":{}}}}},
            {"id":"tree0","name":"tree0","components":mesh([10.0,0.0,10.0])},
            {"id":"leaf0","name":"leaf0","parent":"tree0","components":{"Transform":{"position":[0,1,0]},"MeshRenderer":{"mesh":"asset:mesh"}}},
            {"id":"tree1","name":"tree1","components":{
                "Transform":{"position":[150.0,0.0,20.0]},"MeshRenderer":{"mesh":"asset:mesh"},
                "custom":{"engine.DataLayer":{"version":1,"data":{"layer":"props"}}}}},
            {"id":"tree2","name":"tree2","components":mesh([1_000_050.5,0.0,30.0])},
            {"id":"rock","name":"rock","components":mesh([20.0,0.0,20.0])},
            {"id":"ghost","name":"ghost","components":{}}
        ]})
        .to_string(),
    )
    .unwrap();
    add_asset(
        dir.path(),
        json!({"id":"world","kind":"scene","path":WORLD}),
    );
    dir
}

fn partition_args(extra: Value) -> Value {
    let mut args =
        json!({"scene": WORLD, "output_dir": "cells", "cell_size": 100.0, "exclude": ["rock"]});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

fn read_manifest(root: &Path) -> PartitionManifest {
    PartitionManifest::from_json(&fs::read(root.join(MANIFEST)).unwrap()).unwrap()
}

fn read_json(root: &Path, relative: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(relative)).unwrap()).unwrap()
}

fn files_under(root: &Path, dir: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join(dir))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn partition_splits_entities_by_position_into_cell_documents() {
    let dir = world_fixture();
    let root = dir.path();
    let plan = write(
        root,
        "streaming_partition",
        partition_args(json!({"dry_run": true})),
    );
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(plan["written"], false);
    assert_eq!(plan["cells"], 3);
    assert_eq!(plan["moved_entities"], 4, "tree0, leaf0, tree1, tree2");
    assert_eq!(plan["kept_entities"], 4, "cam, scout, rock, ghost");
    assert!(!root.join("cells").exists(), "a dry run writes nothing");

    let done = write(root, "streaming_partition", partition_args(json!({})));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["manifest"], MANIFEST);
    assert_eq!(done["asset"]["kind"], "partition");
    assert_eq!(
        files_under(root, "cells"),
        [
            "cell_0_0_0.json",
            "cell_10000_0_0.json",
            "cell_1_0_0.props.json",
            "partition.json"
        ]
    );
    let reasons: Vec<_> = done["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s["reason"].as_str().unwrap()))
        .collect();
    assert!(reasons.contains(&("rock", "excluded")));
    assert!(reasons.contains(&("ghost", "no Transform")));
    assert!(reasons.iter().any(|(id, _)| *id == "cam"));

    let manifest = read_manifest(root);
    assert_eq!((manifest.cell_size, manifest.cells.len()), (100.0, 3));
    let find = |key: [i32; 3], layer: &str| {
        manifest
            .cells
            .iter()
            .find(|c| c.key == CellKey(key) && c.layer == layer)
            .unwrap_or_else(|| panic!("cell {key:?} {layer} in {manifest:?}"))
    };
    let near = find([0, 0, 0], "");
    assert_eq!(near.entities, 2);
    assert_eq!(
        near.bytes,
        fs::metadata(root.join(&near.file)).unwrap().len()
    );
    assert_eq!(done["hash"], read_hash(root));

    // Roots are cell-relative, children untouched, and the far cell keeps f32 precision.
    let cell = read_json(root, &near.file);
    assert_eq!(cell["entities"][0]["id"], "tree0");
    assert_eq!(
        cell["entities"][0]["components"]["Transform"]["position"],
        json!([10.0, 0.0, 10.0])
    );
    assert_eq!(cell["entities"][1]["parent"], "tree0");
    assert_eq!(
        cell["entities"][1]["components"]["Transform"]["position"],
        json!([0.0, 1.0, 0.0])
    );
    let props = read_json(root, &find([1, 0, 0], "props").file);
    assert_eq!(
        props["entities"][0]["components"]["Transform"]["position"],
        json!([50.0, 0.0, 20.0])
    );
    let far = read_json(root, &find([10_000, 0, 0], "").file);
    assert_eq!(
        far["entities"][0]["components"]["Transform"]["position"],
        json!([50.5, 0.0, 30.0])
    );

    // Never overwrites, and a refused call leaves the directory as it was.
    let before = files_under(root, "cells");
    let again = write(root, "streaming_partition", partition_args(json!({})));
    assert_eq!(error_code(&again), "streaming_target_exists");
    assert_eq!(files_under(root, "cells"), before);

    // The source scene is untouched by default.
    let scene = read_json(root, WORLD);
    assert_eq!(scene["entities"].as_array().unwrap().len(), 8);
}

fn read_hash(root: &Path) -> String {
    data(root, "streaming_info", json!({"manifest": MANIFEST}))["hash"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn origin_offset_and_xyz_planes_change_the_assignment() {
    let dir = world_fixture();
    let root = dir.path();
    // Shifting the scene origin by 90 m moves tree0 (x = 10) across the x = 100 cell border.
    let shifted = write(
        root,
        "streaming_partition",
        partition_args(json!({"origin_offset": [90.0, 0.0, 0.0], "output_dir": "shifted"})),
    );
    assert_eq!(shifted["ok"], true, "{shifted}");
    let manifest: PartitionManifest =
        PartitionManifest::from_json(&fs::read(root.join("shifted/partition.json")).unwrap())
            .unwrap();
    let keys: Vec<[i32; 3]> = manifest.cells.iter().map(|c| c.key.0).collect();
    assert!(keys.contains(&[1, 0, 0]), "{keys:?}");
    let cell = read_json(root, "shifted/cell_1_0_0.json");
    assert_eq!(cell["entities"][0]["id"], "tree0");
    assert_eq!(
        cell["entities"][0]["components"]["Transform"]["position"],
        json!([0.0, 0.0, 10.0])
    );

    let xyz = write(
        root,
        "streaming_partition",
        partition_args(json!({"plane": "xyz", "cell_size": 5.0, "output_dir": "fine"})),
    );
    assert_eq!(xyz["ok"], true, "{xyz}");
    let fine =
        PartitionManifest::from_json(&fs::read(root.join("fine/partition.json")).unwrap()).unwrap();
    assert_eq!(fine.plane, engine_streaming::GridPlane::Xyz);
    assert!(
        fine.cells.iter().any(|c| c.key == CellKey([2, 0, 2])),
        "tree0 at (10,0,10) / 5"
    );
}

#[test]
fn strip_removes_moved_entities_only_with_a_matching_revision() {
    let dir = world_fixture();
    let root = dir.path();
    let missing = write(
        root,
        "streaming_partition",
        partition_args(json!({"strip": true})),
    );
    assert_eq!(error_code(&missing), "invalid_argument");
    let stale = write(
        root,
        "streaming_partition",
        partition_args(json!({"strip": true, "expected_revision": "0".repeat(64)})),
    );
    assert_eq!(error_code(&stale), "revision_conflict");
    assert!(!root.join("cells").exists());

    let done = write(
        root,
        "streaming_partition",
        partition_args(json!({"strip": true, "expected_revision": revision(root, WORLD)})),
    );
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["stripped"], 4);
    assert_eq!(done["revision"], revision(root, WORLD));
    let ids: Vec<String> = read_json(root, WORLD)["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["cam", "scout", "rock", "ghost"]);
}

#[test]
fn invalid_requests_are_rejected_without_side_effects() {
    let dir = world_fixture();
    let root = dir.path();
    for (bad, code) in [
        (json!({"cell_size": 0.5}), "invalid_argument"),
        (
            json!({"cell_size": 100.0, "plane": "yz"}),
            "invalid_argument",
        ),
        (
            json!({"cell_size": 100.0, "origin_offset": [1, 2]}),
            "invalid_argument",
        ),
        (
            json!({"cell_size": 100.0, "output_dir": "../escape"}),
            "path_invalid",
        ),
        (
            json!({"cell_size": 100.0, "output_dir": ".git/x"}),
            "path_invalid",
        ),
        (
            json!({"cell_size": 100.0, "scene": "scenes/missing.json"}),
            "path_invalid",
        ),
    ] {
        let response = write(root, "streaming_partition", partition_args(bad.clone()));
        assert_eq!(error_code(&response), code, "{bad}: {response}");
    }
    assert!(!root.join("cells").exists());
    assert!(!root.parent().unwrap().join("escape").exists());
    // A layer name that cannot be a file name is reported, not mangled.
    let mut scene = read_json(root, WORLD);
    scene["entities"][4]["components"]["custom"]["engine.DataLayer"]["data"]["layer"] =
        json!("a/b");
    fs::write(root.join(WORLD), scene.to_string()).unwrap();
    let layer = write(root, "streaming_partition", partition_args(json!({})));
    assert_eq!(error_code(&layer), "streaming_layer_invalid", "{layer}");
    assert!(!root.join("cells").exists());
    // Read-only servers do not list the writers.
    let denied = exchange(
        root,
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name": "streaming_partition", "arguments": partition_args(json!({}))}),
        )],
    );
    assert_eq!(denied[0]["error"]["code"], -32602, "{}", denied[0]);
}

#[test]
fn hlod_builds_merged_simplified_proxies_and_updates_the_manifest() {
    let dir = world_fixture();
    let root = dir.path();
    assert_eq!(
        write(root, "streaming_partition", partition_args(json!({})))["ok"],
        true
    );
    let before = read_hash(root);

    let dry = write(
        root,
        "streaming_hlod",
        json!({"manifest": MANIFEST, "proxy_ratio": 1.0, "dry_run": true}),
    );
    assert_eq!(dry["ok"], true, "{dry}");
    assert_eq!(dry["saved"], false);
    assert_eq!(read_hash(root), before, "a dry run changes nothing");
    assert!(!root.join("cells/hlod").exists());

    let built = write(
        root,
        "streaming_hlod",
        json!({"manifest": MANIFEST, "proxy_ratio": 1.0, "expected_hash": before}),
    );
    assert_eq!(built["ok"], true, "{built}");
    assert_eq!(built["built"], 3);
    assert_eq!(
        built["source_triangles"], 4,
        "tree0 + leaf0 in one cell, tree1 and tree2 alone"
    );
    assert_eq!(built["previous_hash"], before);
    assert_ne!(built["hash"], before);
    assert_eq!(built["hash"], read_hash(root));

    let manifest = read_manifest(root);
    let near = manifest
        .cells
        .iter()
        .find(|c| c.key == CellKey([0, 0, 0]))
        .unwrap();
    let proxy = near.hlod.as_ref().expect("proxy recorded");
    assert_eq!(proxy.file, "cells/hlod/cell_0_0_0.mesh.json");
    assert_eq!((proxy.source_triangles, proxy.triangles), (2, 2));
    // Cell-local geometry: the triangle (0,0,0)-(2,0,0)-(0,3,0) at (10,0,10), plus the leaf one up.
    let mesh = read_json(root, &proxy.file);
    let vertices = mesh["vertices"].as_array().unwrap();
    let axis = |i: usize| -> (f64, f64) {
        let values: Vec<f64> = vertices
            .iter()
            .map(|v| v["position"][i].as_f64().unwrap())
            .collect();
        (
            values.iter().copied().fold(f64::MAX, f64::min),
            values.iter().copied().fold(f64::MIN, f64::max),
        )
    };
    let (x, y, z) = (axis(0), axis(1), axis(2));
    assert!(
        (x.0 - 10.0).abs() < 1e-3 && (x.1 - 12.0).abs() < 1e-3,
        "{x:?}"
    );
    assert!(y.0.abs() < 1e-3 && (y.1 - 4.0).abs() < 1e-3, "{y:?}");
    assert!(
        (z.0 - 10.0).abs() < 1e-3 && (z.1 - 10.0).abs() < 1e-3,
        "{z:?}"
    );
    assert_eq!(mesh["indices"].as_array().unwrap().len(), 6);

    let stale = write(
        root,
        "streaming_hlod",
        json!({"manifest": MANIFEST, "expected_hash": before}),
    );
    assert_eq!(error_code(&stale), "revision_conflict");
    let rerun = write(
        root,
        "streaming_hlod",
        json!({"manifest": MANIFEST, "proxy_ratio": 0.5, "max_proxy_triangles": 1}),
    );
    assert_eq!(
        rerun["ok"], true,
        "files named by the manifest are replaced: {rerun}"
    );
    let proxy = read_manifest(root)
        .cells
        .iter()
        .find(|c| c.key == CellKey([0, 0, 0]))
        .unwrap()
        .hlod
        .clone()
        .unwrap();
    assert!(proxy.triangles <= proxy.source_triangles && proxy.triangles >= 1);
    assert_eq!(proxy.source_triangles, 2);

    let info = data(root, "streaming_info", json!({"manifest": MANIFEST}));
    assert_eq!(info["hlod_cells"], 3);
    assert_eq!(info["missing_files"], json!([]));
}

#[test]
fn hlod_simplifies_dense_meshes_to_the_requested_ratio() {
    let dir = fixture();
    let root = dir.path();
    // A flat 20 x 20 quad grid (800 triangles, shared vertices) spanning 40 m.
    let n = 21usize;
    let vertices: Vec<Value> = (0..n * n)
        .map(|i| {
            let (x, z) = ((i % n) as f64 * 2.0, (i / n) as f64 * 2.0);
            json!({"position": [x, 0.0, z], "uv": [x / 40.0, z / 40.0], "color": [1, 1, 1, 1]})
        })
        .collect();
    let mut indices = Vec::new();
    for z in 0..n - 1 {
        for x in 0..n - 1 {
            let a = (z * n + x) as u32;
            let (b, c, d) = (a + 1, a + n as u32, a + n as u32 + 1);
            indices.extend([a, c, b, b, c, d]);
        }
    }
    fs::write(
        root.join("assets/grid.mesh"),
        json!({"vertices": vertices, "indices": indices}).to_string(),
    )
    .unwrap();
    add_asset(
        root,
        json!({"id": "grid", "kind": "mesh", "path": "assets/grid.mesh"}),
    );
    fs::write(
        root.join(WORLD),
        json!({"schema":1,"scene_id":"scene:world","entities":[
            {"id":"ground","name":"ground","components":{"Transform":{"position":[10.0,0.0,10.0]},"MeshRenderer":{"mesh":"grid"}}}
        ]})
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        write(root, "streaming_partition", partition_args(json!({})))["ok"],
        true
    );
    let built = write(
        root,
        "streaming_hlod",
        json!({"manifest": MANIFEST, "proxy_ratio": 0.1}),
    );
    assert_eq!(built["ok"], true, "{built}");
    let proxy = read_manifest(root).cells[0].hlod.clone().unwrap();
    assert_eq!(proxy.source_triangles, 800);
    assert!(
        proxy.triangles >= 2 && proxy.triangles <= 160,
        "{} triangles",
        proxy.triangles
    );
    let mesh = read_json(root, &proxy.file);
    let xs: Vec<f64> = mesh["vertices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["position"][0].as_f64().unwrap())
        .collect();
    assert!(xs.iter().cloned().fold(f64::MAX, f64::min) >= 10.0 - 1e-3);
    assert!(xs.iter().cloned().fold(f64::MIN, f64::max) <= 50.0 + 1e-3);
    let capped = write(
        root,
        "streaming_hlod",
        json!({"manifest": MANIFEST, "proxy_ratio": 1.0, "max_proxy_triangles": 100}),
    );
    assert_eq!(capped["ok"], true, "{capped}");
    let again = read_manifest(root).cells[0].hlod.clone().unwrap();
    assert!(
        again.triangles <= 120,
        "capped near 100: {}",
        again.triangles
    );
}

#[test]
fn hlod_never_replaces_files_it_does_not_own() {
    let dir = world_fixture();
    let root = dir.path();
    assert_eq!(
        write(root, "streaming_partition", partition_args(json!({})))["ok"],
        true
    );
    fs::create_dir(root.join("cells/hlod")).unwrap();
    fs::write(root.join("cells/hlod/cell_0_0_0.mesh.json"), "keep me").unwrap();
    let refused = write(root, "streaming_hlod", json!({"manifest": MANIFEST}));
    assert_eq!(error_code(&refused), "streaming_target_exists", "{refused}");
    assert_eq!(
        fs::read_to_string(root.join("cells/hlod/cell_0_0_0.mesh.json")).unwrap(),
        "keep me"
    );
    assert!(
        read_manifest(root).cells.iter().all(|c| c.hlod.is_none()),
        "manifest untouched"
    );
}

#[test]
fn info_summarises_pages_and_answers_what_would_load() {
    let dir = world_fixture();
    let root = dir.path();
    assert_eq!(
        write(root, "streaming_partition", partition_args(json!({})))["ok"],
        true
    );
    let info = data(
        root,
        "streaming_info",
        json!({"manifest": MANIFEST, "limit": 2}),
    );
    assert_eq!(info["ok"], true, "{info}");
    assert_eq!(
        (info["cell_size"].as_f64(), info["plane"].as_str()),
        (Some(100.0), Some("xz"))
    );
    assert_eq!(
        (info["cell_count"].as_u64(), info["entities"].as_u64()),
        (Some(3), Some(4))
    );
    assert_eq!(info["layers"].as_array().unwrap().len(), 2);
    assert_eq!(
        info["bounds"],
        json!({"min": [0, 0, 0], "max": [10_000, 0, 0]})
    );
    assert_eq!(info["cells"].as_array().unwrap().len(), 2);
    assert_eq!(info["truncated"], true);
    let page = data(
        root,
        "streaming_info",
        json!({"manifest": MANIFEST, "offset": 2, "limit": 2}),
    );
    assert_eq!(page["cells"].as_array().unwrap().len(), 1);
    assert_eq!(page["truncated"], false);

    let near = data(
        root,
        "streaming_info",
        json!({"manifest": MANIFEST, "position": [50.0, 0.0, 50.0], "radius": 120.0}),
    );
    assert_eq!(near["within_count"], 2);
    assert_eq!(near["within"][0]["key"], json!([0, 0, 0]));
    assert_eq!(near["within"][0]["distance"], 0.0);
    assert_eq!(near["within"][1]["key"], json!([1, 0, 0]));
    assert_eq!(near["within"][1]["layer"], "props");
    assert_eq!(near["within"][1]["distance"], 50.0);
    let far = data(
        root,
        "streaming_info",
        json!({"manifest": MANIFEST, "position": [1_000_050.0, 0.0, 50.0], "radius": 10.0}),
    );
    assert_eq!(far["within_count"], 1);
    assert_eq!(far["within"][0]["key"], json!([10_000, 0, 0]));

    // Missing cell files are reported.
    fs::remove_file(root.join("cells/cell_1_0_0.props.json")).unwrap();
    let missing = data(root, "streaming_info", json!({"manifest": MANIFEST}));
    assert_eq!(
        missing["missing_files"][0]["file"],
        "cells/cell_1_0_0.props.json"
    );
    // Broken manifests are failures with a code.
    fs::write(
        root.join("cells/bad.json"),
        r#"{"schema":1,"cell_size":-1,"cells":[]}"#,
    )
    .unwrap();
    let bad = data(
        root,
        "streaming_info",
        json!({"manifest": "cells/bad.json"}),
    );
    assert_eq!(error_code(&bad), "streaming_manifest_invalid");
}

#[test]
fn partition_assets_are_described_by_asset_info() {
    let dir = world_fixture();
    let root = dir.path();
    assert_eq!(
        write(root, "streaming_partition", partition_args(json!({})))["ok"],
        true
    );
    add_asset(
        root,
        json!({"id": "world-cells", "kind": "partition", "path": MANIFEST}),
    );
    let info = data(root, "asset_info", json!({"asset": "world-cells"}));
    assert_eq!(info["details"]["cells"], 3, "{info}");
    assert_eq!(info["details"]["layers"], json!(["", "props"]));
    let listed = data(root, "asset_list", json!({"kind": "partition"}));
    assert_eq!(listed["count"], 1, "{listed}");
    let validated = data(root, "project_validate", json!({}));
    assert_eq!(validated["ok"], true, "{validated}");
}

#[test]
fn component_set_edits_the_streaming_components_and_rejects_bad_values() {
    let dir = world_fixture();
    let root = dir.path();
    assert_eq!(
        write(root, "streaming_partition", partition_args(json!({})))["ok"],
        true
    );
    add_asset(
        root,
        json!({"id": "world-cells", "kind": "partition", "path": MANIFEST}),
    );
    for name in [
        "engine.WorldPartition",
        "engine.StreamingSource",
        "engine.DataLayer",
    ] {
        let schema = data(root, "component_schema", json!({"component": name}));
        let component = &schema["components"][0];
        assert_eq!(component["name"], name, "{schema}");
        assert_eq!(component["schema"]["additionalProperties"], false);
    }
    let set = |entity: &str, component: &str, value: Value| {
        write(
            root,
            "component_set",
            json!({"scene": WORLD, "expected_revision": revision(root, WORLD),
                "entity": entity, "component": component, "value": value}),
        )
    };
    let ok = set(
        "cam",
        "engine.WorldPartition",
        json!({"version": 1, "data": {"manifest": "world-cells", "load_radius": 300.0, "unload_radius": 400.0,
            "layers": [{"name": "props", "enabled": false}]}}),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let stored = &ok["entity"]["components"]["custom"]["engine.WorldPartition"]["data"];
    assert_eq!(stored["manifest"], "world-cells");
    assert_eq!(stored["layers"][0]["enabled"], false);
    assert_eq!(
        set(
            "scout",
            "engine.StreamingSource",
            json!({"version": 1, "data": {"priority": 5, "radius_scale": 2.0}})
        )["ok"],
        true
    );
    assert_eq!(
        set(
            "tree0",
            "engine.DataLayer",
            json!({"version": 1, "data": {"layer": "props"}})
        )["ok"],
        true
    );
    for bad in [
        ("engine.WorldPartition", json!({"manifest": "missing"})),
        ("engine.WorldPartition", json!({"manifest": "asset:mesh"})),
        (
            "engine.WorldPartition",
            json!({"load_radius": 500.0, "unload_radius": 100.0}),
        ),
        ("engine.WorldPartition", json!({"load_radius": 0.0})),
        ("engine.WorldPartition", json!({"loads_per_tick": 0})),
        (
            "engine.WorldPartition",
            json!({"layers": [{"name": "a"}, {"name": "a"}]}),
        ),
        ("engine.WorldPartition", json!({"unknown": true})),
        ("engine.StreamingSource", json!({"radius_scale": 0.0})),
        ("engine.StreamingSource", json!({"priority": 1.5})),
    ] {
        let response = set("cam", bad.0, json!({"version": 1, "data": bad.1.clone()}));
        assert_eq!(response["ok"], false, "{:?}", bad);
    }
}

fn tick(runtime: &mut Runtime) {
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
}

/// The tools' output drives the real runtime: partition, build proxies, stream, toggle a layer.
#[test]
fn partitioned_scene_streams_in_the_runtime_with_proxies_and_layers() {
    let dir = world_fixture();
    let root = dir.path();
    let revision = revision(root, WORLD);
    assert_eq!(
        write(
            root,
            "streaming_partition",
            partition_args(json!({"strip": true, "expected_revision": revision}))
        )["ok"],
        true
    );
    assert_eq!(
        write(
            root,
            "streaming_hlod",
            json!({"manifest": MANIFEST, "proxy_ratio": 1.0})
        )["ok"],
        true
    );
    add_asset(
        root,
        json!({"id": "world-cells", "kind": "partition", "path": MANIFEST}),
    );
    let configured = write(
        root,
        "component_set",
        json!({"scene": WORLD, "expected_revision": common::revision(root, WORLD),
            "entity": "ghost", "component": "engine.WorldPartition",
            "value": {"version": 1, "data": {"manifest": "world-cells", "load_radius": 200.0, "unload_radius": 250.0,
                "camera_source": false, "synchronous": true, "loads_per_tick": 8}}}),
    );
    assert_eq!(configured["ok"], true, "{configured}");
    // The scout stands 50 m from the (rebased) origin.
    let mut scene = read_json(root, WORLD);
    let scout = scene["entities"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["id"] == "scout")
        .unwrap();
    scout["components"]["Transform"]["position"] = json!([50.0, 0.0, 50.0]);
    fs::write(root.join(WORLD), scene.to_string()).unwrap();
    // Make the scene the startup scene.
    let mut project: Value = read_json(root, "project.json");
    project["startup_scene"] = json!(WORLD);
    fs::write(root.join("project.json"), project.to_string()).unwrap();

    let project = Project::load(root.join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.app_mut().world_mut().insert_resource(WorldOrigin {
        offset: [1_000_000.0, 0.0, 0.0],
    });
    runtime.start().unwrap();
    tick(&mut runtime);
    let world = runtime.app().world();
    let state =
        |key: [i32; 3], layer: &str| runtime_streaming::cell_state(world, "ghost", key, layer);
    use runtime_streaming::CellState::{Loaded, Unloaded};
    assert_eq!(state([10_000, 0, 0], ""), Some(Loaded));
    assert_eq!(state([0, 0, 0], ""), Some(Unloaded));
    assert_eq!(state([1, 0, 0], "props"), Some(Unloaded));
    let tree = runtime.entity("tree2").expect("tree2 streamed in");
    assert_eq!(
        world.get::<Transform>(tree).unwrap().position,
        [50.5, 0.0, 30.0]
    );
    // The unloaded cells show their proxies; the loaded one does not.
    let proxies: Vec<String> = runtime
        .extract_scene_frame()
        .unwrap()
        .meshes
        .iter()
        .filter(|m| m.id.starts_with("hlod:"))
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(proxies.len(), 2, "{proxies:?}");
    assert!(proxies.iter().all(|id| !id.contains("10000")));
    let proxy = runtime.entity("hlod:ghost:0_0_0").unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(proxy)
            .unwrap()
            .position,
        [-1_000_000.0, 0.0, 0.0],
        "the proxy sits at the cell corner in origin-relative space"
    );
    let status = runtime_streaming::partition_status(runtime.app().world(), "ghost").unwrap();
    assert_eq!(
        (status.loaded, status.proxies, status.diagnostics.len()),
        (1, 2, 0)
    );
}

#[test]
fn runtime_layer_toggle_works_on_tool_made_cells() {
    let dir = world_fixture();
    let root = dir.path();
    let revision = revision(root, WORLD);
    assert_eq!(
        write(
            root,
            "streaming_partition",
            partition_args(
                json!({"strip": true, "expected_revision": revision, "cell_size": 1000.0})
            )
        )["ok"],
        true
    );
    add_asset(
        root,
        json!({"id": "world-cells", "kind": "partition", "path": MANIFEST}),
    );
    let configured = write(
        root,
        "component_set",
        json!({"scene": WORLD, "expected_revision": common::revision(root, WORLD),
            "entity": "ghost", "component": "engine.WorldPartition",
            "value": {"version": 1, "data": {"manifest": "world-cells", "load_radius": 2000.0, "unload_radius": 3000.0,
                "camera_source": false, "synchronous": true, "loads_per_tick": 8}}}),
    );
    assert_eq!(configured["ok"], true, "{configured}");
    let mut project: Value = read_json(root, "project.json");
    project["startup_scene"] = json!(WORLD);
    fs::write(root.join("project.json"), project.to_string()).unwrap();
    let project = Project::load(root.join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime);
    assert!(
        runtime.entity("tree1").is_some(),
        "layer `props` streams by default"
    );
    runtime_streaming::set_layer_enabled(runtime.app_mut().world_mut(), "ghost", "props", false)
        .unwrap();
    tick(&mut runtime);
    assert!(runtime.entity("tree1").is_none());
    assert!(runtime.entity("tree0").is_some());
    runtime_streaming::set_layer_enabled(runtime.app_mut().world_mut(), "ghost", "props", true)
        .unwrap();
    tick(&mut runtime);
    assert!(runtime.entity("tree1").is_some());
}
