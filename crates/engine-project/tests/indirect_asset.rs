use engine_project::{Project, indirect::load_indirect_assets};
use serde_json::json;
use std::fs;

fn project(face_value: f32) -> (tempfile::TempDir, serde_json::Value) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({
            "schema":1,"scene_id":"s","entities":[{
                "id":"camera","name":"camera","components":{
                    "Transform":{"position":[0,0,0]},
                    "Camera3D":{"lighting":{"indirect":"sky"}}
                }
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let faces: Vec<_> = (0..6)
        .map(|i| {
            let name = format!("face{i}.rgb32f");
            let bytes: Vec<u8> = (0..3).flat_map(|_| face_value.to_le_bytes()).collect();
            fs::write(dir.path().join(&name), bytes).unwrap();
            name
        })
        .collect();
    let descriptor = json!({
        "version":1,
        "environments":[{"edge":1,"diffuse":[[1.0,0.0,0.0],[0.0,0.0,0.0],[0.0,0.0,0.0],
            [0.0,0.0,0.0],[0.0,0.0,0.0],[0.0,0.0,0.0],[0.0,0.0,0.0],[0.0,0.0,0.0],[0.0,0.0,0.0]],
            "mips":[faces]}],
        "global":{"environment":0,"rotation":[0,0,0,1],"intensity":1},
        "probes":[{"id":"p","positionAbsolute":[999999999000.0,0.0,0.0],
            "scale":[1,1,1],"halfExtents":[2,2,2],"blendDistance":1,"priority":4,
            "lighting":{"environment":0,"rotation":[0,0,0,1],"intensity":1}}]
    });
    fs::write(
        dir.path().join("sky.json"),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"indirect-test","startup_scene":"scene.json",
            "assets":[{"id":"sky","path":"sky.json","kind":"indirect"}]
        }))
        .unwrap(),
    )
    .unwrap();
    (dir, descriptor)
}

#[test]
fn authored_faces_and_probe_metadata_load() {
    let (dir, _) = project(2.0);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let set = load_indirect_assets(&project)
        .unwrap()
        .remove("sky")
        .unwrap();
    assert_eq!(set.environments[0].mips[0][0].pixels, vec![[2.0; 3]]);
    assert_eq!(set.probes[0].position_absolute[0], 999999999000.0);
    assert_eq!(set.decoded_bytes, 72);
}

#[test]
fn authored_face_nonfinite_and_path_escape_are_rejected() {
    let (dir, mut descriptor) = project(f32::NAN);
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(error.to_string().contains("radiance"), "{error}");
    let face = descriptor["environments"][0]["mips"][0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let bytes: Vec<u8> = (0..3).flat_map(|_| 1.0f32.to_le_bytes()).collect();
    fs::write(dir.path().join(face), bytes).unwrap();
    descriptor["environments"][0]["mips"][0][0] = json!("../escaped.rgb32f");
    fs::write(
        dir.path().join("sky.json"),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    assert!(Project::load(dir.path().join("project.json")).is_err());
}

#[test]
fn missing_camera_reference_fails_project_validation() {
    let (dir, _) = project(1.0);
    let scene = dir.path().join("scene.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&scene).unwrap()).unwrap();
    value["entities"][0]["components"]["Camera3D"]["lighting"]["indirect"] = json!("missing");
    fs::write(scene, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(error.to_string().contains("missing"), "{error}");
}
