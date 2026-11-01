//! Authored `MeshLods` and `HiZOccluder`: schema validation, runtime
//! extraction into draw options, and runtime geometry overrides.
use engine_project::content::{Mesh, Vertex};
use engine_project::{GeometryOverride, Project, Runtime};
use serde_json::json;
use std::fs;
use std::sync::Arc;

fn mesh_json(half: f32) -> Vec<u8> {
    let vertex = |x: f32, y: f32| json!({"position": [x, y, 0.0], "uv": [0.0, 0.0], "color": [1.0, 1.0, 1.0, 1.0]});
    serde_json::to_vec(&json!({
        "vertices": [vertex(-half, -half), vertex(half, -half), vertex(half, half), vertex(-half, half)],
        "indices": [0, 1, 2, 0, 2, 3]
    }))
    .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, half) in [("fine", 0.5), ("coarse", 0.25), ("coarser", 0.1)] {
        fs::write(dir.path().join(format!("{name}.json")), mesh_json(half)).unwrap();
    }
    fs::write(dir.path().join("scene.json"), serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":[
        {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{
            "render_features":{"engine.hiz.visibility":true}}}},
        {"id":"lod","name":"Lod","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"fine"},
            "MeshLods":{"meshes":["coarse","coarser"],"thresholds":[0.5,0.2,0.05],"hysteresis":0.15},
            "HiZOccluder":{}}},
        {"id":"marked","name":"Marked","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"fine"},"HiZOccluder":{}}},
        {"id":"disabled","name":"Disabled","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"fine"},"HiZOccluder":{"enabled":false}}},
        {"id":"plain","name":"Plain","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"fine"}}}
    ]})).unwrap()).unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"lods","startup_scene":"scene.json","assets":[
                {"id":"fine","kind":"mesh","path":"fine.json"},
                {"id":"coarse","kind":"mesh","path":"coarse.json"},
                {"id":"coarser","kind":"mesh","path":"coarser.json"}
            ]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

fn load(dir: &tempfile::TempDir) -> Project {
    Project::load(dir.path().join("project.json")).unwrap()
}

#[test]
fn authored_levels_policy_and_occluder_marks_reach_the_render_frame() {
    let dir = fixture();
    let runtime = Runtime::new(load(&dir)).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.meshes.len(), 4);
    assert!(
        frame
            .camera3d
            .as_ref()
            .unwrap()
            .camera
            .effective_render_features()["engine.hiz.visibility"]
    );
    let lod = &frame.mesh_options["lod"];
    assert_eq!(lod.levels.len(), 2);
    assert_eq!(lod.policy.thresholds, vec![0.5, 0.2, 0.05]);
    assert_eq!(lod.policy.hysteresis, 0.15);
    assert!(lod.policy.cull_below.is_none(), "authored LODs never cull");
    assert!(lod.occluder);
    // Level meshes are the catalog assets, finest first.
    assert_eq!(lod.levels[0].vertices[2].position[0], 0.25);
    assert_eq!(lod.levels[1].vertices[2].position[0], 0.1);
    let marked = &frame.mesh_options["marked"];
    assert!(marked.levels.is_empty() && marked.occluder);
    assert_eq!(marked.policy.thresholds, vec![1.0]);
    assert!(
        !frame.mesh_options.contains_key("disabled"),
        "an explicitly disabled marker is a plain draw"
    );
    assert!(!frame.mesh_options.contains_key("plain"));
    // The authored components survive a snapshot.
    let snapshot = runtime.try_snapshot().unwrap();
    let entity = snapshot
        .scene
        .entities
        .iter()
        .find(|e| e.id == "lod")
        .unwrap();
    assert_eq!(
        entity.components.mesh_lods.as_ref().unwrap().meshes.len(),
        2
    );
    assert!(entity.components.hiz_occluder.as_ref().unwrap().enabled);
}

#[test]
fn runtime_geometry_overrides_are_level_zero_and_never_certified() {
    let dir = fixture();
    let mut runtime = Runtime::new(load(&dir)).unwrap();
    let entity = runtime.entity("lod").unwrap();
    let replacement = Arc::new(Mesh {
        uv2: vec![],
        normals: vec![],
        vertices: [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]]
            .into_iter()
            .map(|position| Vertex {
                position,
                uv: [0.0; 2],
                color: [1.0; 4],
            })
            .collect(),
        indices: vec![0, 1, 2],
    });
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .insert(GeometryOverride(replacement));
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(
        !frame.mesh_options.contains_key("lod"),
        "static levels and the occluder mark do not apply to generated geometry"
    );
    assert!(frame.mesh_options.contains_key("marked"));
}

fn invalid(mutate: impl FnOnce(&mut Project)) -> String {
    let dir = fixture();
    let mut project = load(&dir);
    mutate(&mut project);
    project
        .validate_scene(&project.scene)
        .expect_err("the edit must be rejected")
        .to_string()
}

fn lod_entity(project: &mut Project) -> &mut engine_project::SceneEntity {
    project
        .scene
        .entities
        .iter_mut()
        .find(|e| e.id == "lod")
        .unwrap()
}

#[test]
fn invalid_lod_chains_and_markers_fail_before_runtime() {
    let message = invalid(|p| {
        lod_entity(p)
            .components
            .mesh_lods
            .as_mut()
            .unwrap()
            .thresholds = vec![0.5, 0.2];
    });
    assert!(message.contains("threshold"), "{message}");
    let message = invalid(|p| {
        lod_entity(p)
            .components
            .mesh_lods
            .as_mut()
            .unwrap()
            .thresholds = vec![0.2, 0.5, 0.05];
    });
    assert!(message.contains("decreasing"), "{message}");
    let message = invalid(|p| {
        lod_entity(p)
            .components
            .mesh_lods
            .as_mut()
            .unwrap()
            .hysteresis = 1.0;
    });
    assert!(message.contains("MeshLods"), "{message}");
    let message = invalid(|p| {
        lod_entity(p).components.mesh_lods.as_mut().unwrap().meshes[0] = "missing".into();
    });
    assert!(message.contains("missing"), "{message}");
    let message = invalid(|p| lod_entity(p).components.mesh_renderer = None);
    assert!(message.contains("MeshRenderer"), "{message}");
    let message = invalid(|p| {
        let lods = lod_entity(p).components.mesh_lods.as_mut().unwrap();
        lods.meshes = vec!["coarse".into(); 8];
        lods.thresholds = (1..=9).rev().map(|i| i as f32 / 10.0).collect();
    });
    assert!(message.contains("MeshLods"), "{message}");
    let message = invalid(|p| {
        lod_entity(p)
            .components
            .hiz_occluder
            .as_mut()
            .unwrap()
            .version = 2;
    });
    assert!(message.contains("HiZOccluder"), "{message}");
}
