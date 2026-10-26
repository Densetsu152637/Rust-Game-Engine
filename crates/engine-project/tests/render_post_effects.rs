use engine_project::{Project, Runtime, rendering3d::*};
use serde_json::json;
use std::fs;

fn project_with_camera(camera: serde_json::Value) -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("scene.json"), serde_json::to_vec(&json!({
        "schema":1,"scene_id":"scene","entities":[
            {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":camera}}
        ]
    })).unwrap()).unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"post-effects-test","startup_scene":"scene.json","assets":[]
        }))
        .unwrap(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    (dir, project)
}

#[test]
fn authored_effects_validate_load_extract_and_snapshot() {
    let (_dir, project) = project_with_camera(json!({
        "depth_of_field":{"focus_distance":2.5,"focus_range":0.75,"max_radius_pixels":9.0,"edge_tolerance":0.2},
        "motion_blur":{"shutter_scale":0.8,"max_radius_pixels":12.0,"edge_tolerance":0.4}
    }));
    let mut runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.unwrap().camera;
    assert_eq!(camera.depth_of_field.unwrap().focus_distance, 2.5);
    assert_eq!(camera.motion_blur.unwrap().shutter_scale, 0.8);
    assert!(camera.effective_render_features()[DEPTH_OF_FIELD_FEATURE]);
    assert!(camera.effective_render_features()[MOTION_BLUR_FEATURE]);
    let snapshot = runtime.try_snapshot().unwrap();
    let saved = snapshot.scene.entities[0]
        .components
        .camera3d
        .as_ref()
        .unwrap();
    assert_eq!(saved.depth_of_field, camera.depth_of_field);
    assert_eq!(saved.motion_blur, camera.motion_blur);
    let roundtrip: Camera3D = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert_eq!(&roundtrip, saved);
}

#[test]
fn optional_settings_default_off_and_explicit_feature_disables() {
    let camera = Camera3D::default();
    assert!(camera.depth_of_field.is_none() && camera.motion_blur.is_none());
    assert!(
        !camera
            .effective_render_features()
            .contains_key(DEPTH_OF_FIELD_FEATURE)
    );
    let mut camera = Camera3D {
        depth_of_field: Some(DepthOfField3D::default()),
        motion_blur: Some(MotionBlur3D::default()),
        ..Default::default()
    };
    camera
        .render_features
        .insert(DEPTH_OF_FIELD_FEATURE.into(), false);
    assert!(!camera.effective_render_features()[DEPTH_OF_FIELD_FEATURE]);
    assert!(camera.effective_render_features()[MOTION_BLUR_FEATURE]);
    camera.depth_of_field = None;
    camera.motion_blur = None;
    assert!(!camera.effective_render_features()[DEPTH_OF_FIELD_FEATURE]);
    assert!(
        !camera
            .effective_render_features()
            .contains_key(MOTION_BLUR_FEATURE)
    );
}

#[test]
fn invalid_effects_fail_before_gpu_and_schema_is_closed() {
    for bad in [
        json!({"depth_of_field":{"focus_distance":0.0}}),
        json!({"depth_of_field":{"max_radius_pixels":25.0}}),
        json!({"motion_blur":{"shutter_scale":2.1}}),
        json!({"motion_blur":{"edge_tolerance":-0.1}}),
    ] {
        let camera: Camera3D = serde_json::from_value(bad).unwrap();
        assert!(camera.validate().is_err());
    }
    for bad in [
        json!({"depth_of_field":{"unknown":1}}),
        json!({"motion_blur":{"unknown":1}}),
    ] {
        assert!(serde_json::from_value::<Camera3D>(bad).is_err());
    }
    assert!(
        DepthOfField3D {
            focus_distance: f32::NAN,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        MotionBlur3D {
            shutter_scale: f32::INFINITY,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let props = &doc.schema["properties"];
    for (field, bounds) in [
        ("depth_of_field", "focus_distance"),
        ("motion_blur", "shutter_scale"),
    ] {
        let shape = &props[field]["oneOf"][0];
        assert_eq!(shape["additionalProperties"], false);
        assert!(shape["properties"][bounds].is_object());
    }
    assert_eq!(doc.schema["additionalProperties"], false);
}
