use engine_project::{Project, Runtime, indirect::load_indirect_assets, world::coordinates};
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
fn authored_face_nonfinite_is_rejected() {
    let (dir, _) = project(f32::NAN);
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(error.to_string().contains("radiance"), "{error}");
}

#[test]
fn valid_outside_face_is_rejected_for_path_escape() {
    let (dir, mut descriptor) = project(1.0);
    let outside = tempfile::tempdir_in(dir.path().parent().unwrap()).unwrap();
    let bytes: Vec<u8> = (0..3).flat_map(|_| 1.0f32.to_le_bytes()).collect();
    fs::write(outside.path().join("escaped.rgb32f"), bytes).unwrap();
    let outside_name = outside.path().file_name().unwrap().to_str().unwrap();
    descriptor["environments"][0]["mips"][0][0] =
        json!(format!("../{outside_name}/escaped.rgb32f"));
    fs::write(
        dir.path().join("sky.json"),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "project.path.outside_root");
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

#[test]
fn multiple_sets_load_and_camera_extracts_selected_set_with_applied_origin() {
    let (dir, _) = project(1.0);
    fs::copy(dir.path().join("sky.json"), dir.path().join("other.json")).unwrap();
    let manifest = dir.path().join("project.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["assets"].as_array_mut().unwrap().push(json!({
        "id":"other","path":"other.json","kind":"indirect"
    }));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let project = Project::load(&manifest).unwrap();
    let sets = load_indirect_assets(&project).unwrap();
    assert_eq!(sets.len(), 2);
    assert_eq!(sets.values().map(|set| set.decoded_bytes).sum::<u64>(), 144);
    let mut runtime = Runtime::new(project).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<coordinates::WorldOrigin>()
        .offset = [999_999_998_000.0, 0.0, 0.0];
    runtime.start().unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.applied_origin[0], 999_999_998_000.0);
    assert_eq!(
        frame.indirect.unwrap().probes[0].position_absolute[0],
        999_999_999_000.0
    );
}

#[test]
fn aggregate_payload_budget_rejects_many_valid_sparse_faces_before_decoding() {
    let (dir, _) = project(1.0);
    let mut mips = Vec::new();
    for level in 0..9 {
        let edge = 256u64 >> level;
        let mut faces = Vec::new();
        for face in 0..6 {
            let name = format!("mip-{level}-face-{face}.rgb32f");
            fs::File::create(dir.path().join(&name))
                .unwrap()
                .set_len(edge * edge * 12)
                .unwrap();
            faces.push(name);
        }
        mips.push(faces);
    }
    let mut descriptor: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("sky.json")).unwrap()).unwrap();
    descriptor["environments"][0]["edge"] = json!(256);
    descriptor["environments"][0]["mips"] = json!(mips);
    let mut assets = Vec::new();
    for i in 0..22 {
        let name = format!("set-{i}.json");
        fs::write(
            dir.path().join(&name),
            serde_json::to_vec(&descriptor).unwrap(),
        )
        .unwrap();
        assets.push(json!({"id":format!("set-{i}"),"path":name,"kind":"indirect"}));
    }
    let manifest = dir.path().join("project.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["assets"] = json!(assets);
    value["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"sky","path":"sky.json","kind":"indirect"}));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = Project::load(&manifest).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("project payloads exceed 128 MiB"),
        "{error}"
    );
}
