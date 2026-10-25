//! `physics_query` and `physics_debug`: headless scene simulation, bounded queries and
//! debug summaries through the MCP surface.
mod common;
use common::*;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

fn body(id: &str, position: [f32; 3], dim: u8, data: Value) -> Value {
    let name = if dim == 2 {
        "engine.PhysicsBody2D"
    } else {
        "engine.PhysicsBody3D"
    };
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},
        "custom":{name:{"version":1,"data":data}}}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{
        "custom":{"engine.PhysicsSettings":{"version":1,"data":data}}}})
}
fn project(entities: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("scenes")).unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"Physics Fixture","startup_scene":"scenes/world.json",
            "assets":[{"id":"scene:other","kind":"scene","path":"scenes/other.json"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scenes/world.json"),
        json!({"schema":1,"scene_id":"scene:world","entities":entities}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scenes/other.json"),
        json!({"schema":1,"scene_id":"scene:other","entities":[
            body("lonely", [0., 0., 0.], 3, json!({"motion":"fixed","shape":"ball","radius":1.0}))
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}
fn arena() -> tempfile::TempDir {
    project(vec![
        settings(json!({"layers":["world","trigger"]})),
        body(
            "ground",
            [0., -1., 0.],
            3,
            json!({"motion":"fixed","half_extents":[50.,1.,50.],"material":{"layer_name":"world"}}),
        ),
        body(
            "wall",
            [0., 1., 5.],
            3,
            json!({"motion":"fixed","half_extents":[1.,1.,0.5],"material":{"layer_name":"world"}}),
        ),
        body(
            "zone",
            [0., 1., 10.],
            3,
            json!({"motion":"fixed","shape":"ball","radius":1.0,
                "material":{"sensor":true,"layer_name":"trigger"}}),
        ),
        body(
            "crate",
            [5., 6., 0.],
            3,
            json!({"half_extents":[0.5,0.5,0.5],"material":{"layer_name":"world"}}),
        ),
    ])
}
fn files(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn both_tools_are_listed_read_only_and_never_write() {
    let dir = arena();
    let before = files(dir.path());
    let responses = exchange(
        dir.path(),
        read_only(),
        &[modern(1, "tools/list", json!({}))],
    );
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    for name in ["physics_query", "physics_debug"] {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], false, "{name}");
        assert!(tool["description"].as_str().unwrap().len() > 200);
        assert!(tool["outputSchema"].is_object());
    }
    let result = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1]}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(files(dir.path()), before, "queries must not write files");
}

#[test]
fn ray_queries_return_hits_with_authored_ids() {
    let dir = arena();
    let closest = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1]}),
    );
    assert_eq!(closest["count"], 1, "{closest}");
    assert_eq!(closest["hits"][0]["entity"], "wall");
    assert_eq!(closest["hits"][0]["distance"], 4.5);
    assert_eq!(closest["hits"][0]["point"], json!([0.0, 1.0, 4.5]));
    assert_eq!(closest["hits"][0]["normal"], json!([0.0, 0.0, -1.0]));
    assert_eq!(closest["dimension"], "3d");
    let all = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"all":true}),
    );
    let names: Vec<_> = all["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["entity"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["wall", "zone"]);
    let solid = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"all":true,
            "filter":{"sensors":"exclude","exclude":["wall"]}}),
    );
    assert_eq!(solid["count"], 0, "{solid}");
    let layered = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"all":true,
            "filter":{"layers":["trigger"]}}),
    );
    assert_eq!(layered["hits"][0]["entity"], "zone");
    assert_eq!(layered["count"], 1);
    let capped = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"all":true,"limit":1}),
    );
    assert_eq!(capped["count"], 1);
    assert_eq!(capped["truncated"], true);
}

#[test]
fn simulation_ticks_move_bodies_before_the_query() {
    let dir = arena();
    let at_start = data(
        dir.path(),
        "physics_query",
        json!({"kind":"point","point":[5,6,0],"contains":true}),
    );
    assert_eq!(at_start["hits"][0]["entity"], "crate");
    // After one simulated second the crate has fallen about 4.9 m and no longer holds it.
    let later = data(
        dir.path(),
        "physics_query",
        json!({"kind":"point","point":[5,6,0],"contains":true,"ticks":60}),
    );
    assert_eq!(later["ticks"], 60);
    assert_eq!(later["count"], 0, "{later}");
    let nearest = data(
        dir.path(),
        "physics_query",
        json!({"kind":"point","point":[5,3,0],"ticks":60}),
    );
    assert_eq!(nearest["hits"][0]["entity"], "crate", "{nearest}");
}

#[test]
fn shape_cast_and_overlap_round_trip() {
    let dir = arena();
    let cast = data(
        dir.path(),
        "physics_query",
        json!({"kind":"shape_cast","shape":{"type":"ball","radius":0.5},
            "position":[20,5,20],"direction":[0,-1,0],"max_distance":50}),
    );
    assert_eq!(cast["ok"], true, "{cast}");
    assert_eq!(cast["hits"][0]["entity"], "ground");
    let toi = cast["hits"][0]["distance"].as_f64().unwrap();
    assert!((toi - 4.5).abs() < 1e-3, "{toi}");
    assert_eq!(cast["hits"][0]["penetrating"], false);
    let missed = data(
        dir.path(),
        "physics_query",
        json!({"kind":"shape_cast","shape":{"type":"box","half_extents":[0.5,0.5,0.5]},
            "position":[20,5,20],"direction":[0,1,0],"max_distance":50}),
    );
    assert_eq!(missed["count"], 0, "{missed}");
    let overlap = data(
        dir.path(),
        "physics_query",
        json!({"kind":"overlap","shape":{"type":"ball","radius":3.0},"position":[0,1,7],
            "filter":{"sensors":"exclude"}}),
    );
    let mut names: Vec<_> = overlap["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["entity"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["ground", "wall"]);
    let capsule = data(
        dir.path(),
        "physics_query",
        json!({"kind":"overlap","shape":{"type":"capsule","half_height":1.0,"radius":0.5},
            "position":[0,1,10],"rotation":[0,0,0,1],"filter":{"kinds":["fixed"]}}),
    );
    assert_eq!(capsule["hits"][0]["entity"], "zone", "{capsule}");
}

#[test]
fn two_dimensional_scenes_and_alternate_scenes_are_supported() {
    let dir = project(vec![
        body(
            "floor",
            [0., -1., 0.],
            2,
            json!({"motion":"fixed","half_extents":[20.,1.]}),
        ),
        body(
            "coin",
            [5., 2., 0.],
            2,
            json!({"motion":"fixed","shape":"ball","radius":1.0}),
        ),
    ]);
    let ray = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","dimension":"2d","origin":[0,2],"direction":[1,0]}),
    );
    assert_eq!(ray["hits"][0]["entity"], "coin", "{ray}");
    assert_eq!(ray["hits"][0]["distance"], 4.0);
    assert_eq!(ray["hits"][0]["point"], json!([4.0, 2.0]));
    let cast = data(
        dir.path(),
        "physics_query",
        json!({"kind":"shape_cast","dimension":"2d","shape":{"type":"ball","radius":0.5},
            "position":[-6,4],"rotation":[0.0],"direction":[0,-1]}),
    );
    assert_eq!(cast["hits"][0]["entity"], "floor", "{cast}");
    // A 3D query against a 2D-only scene finds nothing; a different scene is selectable.
    let none = data(
        dir.path(),
        "physics_query",
        json!({"kind":"ray","origin":[0,2,0],"direction":[1,0,0]}),
    );
    assert_eq!(none["count"], 0);
    let other = data(
        dir.path(),
        "physics_query",
        json!({"scene":"scenes/other.json","kind":"point","point":[0,0,0],"contains":true}),
    );
    assert_eq!(other["hits"][0]["entity"], "lonely", "{other}");
}

#[test]
fn debug_summary_counts_states_and_truncates() {
    let dir = arena();
    let summary = data(dir.path(), "physics_debug", json!({}));
    assert_eq!(summary["ok"], true, "{summary}");
    assert_eq!(summary["totals"]["colliders"], 4);
    assert_eq!(summary["states"]["fixed"], 2);
    assert_eq!(summary["states"]["sensor"], 1);
    assert_eq!(summary["states"]["dynamic"], 1);
    assert_eq!(summary["truncated"], false);
    assert!(summary["line_segments"].as_u64().unwrap() > 50);
    let entities: Vec<_> = summary["colliders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"].as_str().unwrap())
        .collect();
    assert_eq!(entities.len(), 4);
    assert!(entities.contains(&"crate"));
    let settled = data(
        dir.path(),
        "physics_debug",
        json!({"ticks":90,"max_items":2,"aabbs":true}),
    );
    assert_eq!(settled["colliders"].as_array().unwrap().len(), 2);
    assert_eq!(settled["totals"]["colliders"], 4);
    assert_eq!(settled["truncated"], true);
    assert!(
        settled["totals"]["contacts"].as_u64().unwrap() >= 1,
        "{settled}"
    );
    assert!(
        settled["line_segments"].as_u64().unwrap() > summary["line_segments"].as_u64().unwrap(),
        "AABBs add segments"
    );
}

#[test]
fn bad_arguments_fail_with_actionable_errors() {
    let dir = arena();
    let call = |args: Value| data(dir.path(), "physics_query", args);
    let reject = |args: Value| {
        let result = call(args.clone());
        assert_eq!(result["ok"], false, "{args}: {result}");
        result["error"]["message"].as_str().unwrap().to_owned()
    };
    let message = reject(json!({"kind":"ray","origin":[0,1],"direction":[0,0,1]}));
    assert!(message.contains("3 finite numbers"), "{message}");
    let message = reject(json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],
        "filter":{"layers":["ghost"]}}));
    assert!(message.to_lowercase().contains("ghost"), "{message}");
    let message = reject(json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],
        "filter":{"exclude":["nobody"]}}));
    assert!(message.contains("nobody"), "{message}");
    let message = reject(json!({"kind":"shape_cast","position":[0,1,0],"direction":[0,1,0]}));
    assert!(message.contains("shape_cast"), "{message}");
    let message = reject(
        json!({"kind":"shape_cast","shape":{"type":"cylinder","half_height":1,"radius":1},
        "dimension":"2d","position":[0,1],"direction":[0,1]}),
    );
    assert!(message.contains("3D only"), "{message}");
    let message = reject(json!({"scene":"scenes/missing.json","kind":"point","point":[0,0,0]}));
    assert!(!message.is_empty());
    // Out-of-range numbers are rejected by the input schema or the tool, never simulated.
    let responses = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"physics_query","arguments":
                {"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"ticks":100000}}),
        )],
    );
    let failed = responses[0]["error"].is_object()
        || responses[0]["result"]["structuredContent"]["ok"] == false;
    assert!(failed, "{}", responses[0]);
    let responses = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"physics_debug","arguments":{"dt":5.0}}),
        )],
    );
    let failed = responses[0]["error"].is_object()
        || responses[0]["result"]["structuredContent"]["ok"] == false;
    assert!(failed, "{}", responses[0]);
}
