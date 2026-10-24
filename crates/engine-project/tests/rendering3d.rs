use engine_project::{Project, Runtime, rendering3d::*};
use serde_json::json;
use std::fs;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("material.wgsl"),
        "custom shader source snapshot",
    )
    .unwrap();
    fs::write(dir.path().join("scene.json"),serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":[
        {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{}}},
        {"id":"parent","name":"Parent","components":{"Transform":{"position":[1,0,0],"rotation":[0,0,0.70710677,0.70710677],"scale":[2,2,2]}}},
        {"id":"mesh","name":"Mesh","parent":"parent","components":{"Transform":{"position":[1,0,0]},"MeshRenderer":{"mesh":"mesh-asset"},"MeshMaterial":{"shader":"shader-asset","tint":[0.2,0.4,0.8,1.0]}}}
    ]})).unwrap()).unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"rendering-test","startup_scene":"scene.json","assets":[
                {"id":"mesh-asset","kind":"mesh","path":"mesh.cgmesh"},
                {"id":"shader-asset","kind":"shader","path":"material.wgsl"}
            ]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
#[test]
fn authored_3d_extracts_hierarchy_material_and_camera_and_snapshots_them() {
    let dir = fixture();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.sprites.draws.is_empty());
    assert_eq!(frame.meshes.len(), 1);
    assert_eq!(
        frame.camera3d.as_ref().unwrap().world[3],
        [0.0, 0.0, 4.0, 1.0]
    );
    let mesh = &frame.meshes[0];
    assert!((mesh.model[3][0] - 1.0).abs() < 1e-5 && (mesh.model[3][1] - 2.0).abs() < 1e-5);
    assert_eq!(
        mesh.shader.as_deref(),
        Some("custom shader source snapshot")
    );
    assert_eq!(mesh.material.tint, [0.2, 0.4, 0.8, 1.0]);
    let snapshot = runtime.try_snapshot().unwrap();
    assert!(
        snapshot
            .scene
            .entities
            .iter()
            .any(|e| e.components.camera3d.is_some())
    );
    assert!(
        snapshot
            .scene
            .entities
            .iter()
            .any(|e| e.components.material.is_some())
    );
    let camera = runtime.entity("camera").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(camera)
        .remove::<Camera3D>();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.camera3d.is_none() && frame.meshes.is_empty());
    assert_eq!(frame.sprites.draws.len(), 1);
    assert_eq!(
        frame.materials2d["mesh:mesh"].shader.as_deref(),
        Some("custom shader source snapshot")
    );
}
#[test]
fn invalid_camera_and_material_asset_kinds_fail_before_runtime() {
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap()
        .projection = Projection3D::Perspective {
        fov_y_radians: 1.0,
        near: 0.0,
        far: 100.0,
    };
    assert!(project.validate_scene(&project.scene).is_err());
    project.scene.entities[0].components.camera3d = Some(Camera3D::default());
    project.scene.entities[2]
        .components
        .material
        .as_mut()
        .unwrap()
        .shader = Some("mesh-asset".into());
    assert!(
        project
            .validate_scene(&project.scene)
            .unwrap_err()
            .to_string()
            .contains("must be shader")
    );
}
#[test]
fn pbr_and_shadow_controls_validate_and_round_trip_through_authored_components() {
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    let material = project.scene.entities[2]
        .components
        .material
        .as_mut()
        .unwrap();
    material.metallic = 0.8;
    material.roughness = 0.25;
    material.cast_shadows = false;
    let camera = project.scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap();
    camera.lighting.shadows.enabled = true;
    camera.lighting.shadows.resolution = 512;
    camera.lighting.intensity = 4.0;
    project.validate_scene(&project.scene).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.meshes[0].material.metallic, 0.8);
    assert!(!frame.meshes[0].material.cast_shadows);
    assert_eq!(
        frame.camera3d.unwrap().camera.lighting.shadows.resolution,
        512
    );
    let snapshot = runtime.try_snapshot().unwrap();
    let value = serde_json::to_value(
        snapshot.scene.entities[0]
            .components
            .camera3d
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["lighting"]["shadows"]["enabled"], true);
}
#[test]
fn invalid_pbr_light_and_shadow_values_are_rejected_at_boundary() {
    for value in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        let material = MeshMaterial {
            metallic: value,
            ..Default::default()
        };
        assert!(material.validate().is_err());
        let material = MeshMaterial {
            roughness: value,
            ..Default::default()
        };
        assert!(material.validate().is_err());
    }
    for resolution in [0, 63, 65, 8192] {
        let mut camera = Camera3D::default();
        camera.lighting.shadows.resolution = resolution;
        assert!(camera.validate().is_err());
    }
    let mut light = SceneLighting {
        direction: [0.0; 3],
        ..Default::default()
    };
    assert!(light.validate().is_err());
    light.direction = [0.0, 1.0, 0.0];
    light.validate().unwrap();
    light.shadows.bias = f32::NAN;
    assert!(light.validate().is_err());
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[2]
        .components
        .material
        .as_mut()
        .unwrap()
        .normal_texture = Some("mesh-asset".into());
    assert!(
        project
            .validate_scene(&project.scene)
            .unwrap_err()
            .to_string()
            .contains("must be texture")
    );
}
