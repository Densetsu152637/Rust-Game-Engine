use engine_project::{Project, Runtime, rendering3d::*};
use serde_json::json;
use std::fs;

fn write_project(camera: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("scene.json"), serde_json::to_vec(&json!({
        "schema":1,"scene_id":"scene","entities":[
            {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":camera}}
        ]
    })).unwrap()).unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"ssr-test","startup_scene":"scene.json","assets":[]
        }))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn project_with_camera(camera: serde_json::Value) -> (tempfile::TempDir, Project) {
    let dir = write_project(camera);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    (dir, project)
}

#[test]
fn authored_reflections_validate_load_extract_and_snapshot() {
    let (_dir, project) = project_with_camera(json!({
        "screen_space_reflections":{
            "max_steps":96,"max_distance":40.0,"thickness":0.6,"roughness_cutoff":0.5,
            "edge_fade":0.05,"intensity":1.5}
    }));
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.unwrap().camera;
    let settings = camera.screen_space_reflections.unwrap();
    assert_eq!(settings.max_steps, 96);
    assert_eq!(settings.max_distance, 40.0);
    assert_eq!(settings.thickness, 0.6);
    assert_eq!(settings.roughness_cutoff, 0.5);
    assert_eq!(settings.edge_fade, 0.05);
    assert_eq!(settings.intensity, 1.5);
    assert!(camera.effective_render_features()[SSR_FEATURE]);
    let snapshot = runtime.try_snapshot().unwrap();
    let saved = snapshot.scene.entities[0]
        .components
        .camera3d
        .as_ref()
        .unwrap();
    assert_eq!(
        saved.screen_space_reflections,
        camera.screen_space_reflections
    );
    let roundtrip: Camera3D = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert_eq!(&roundtrip, saved);
}

#[test]
fn reflections_default_off_empty_object_means_defaults_and_features_override() {
    let camera = Camera3D::default();
    assert!(camera.screen_space_reflections.is_none());
    assert!(!camera.effective_render_features().contains_key(SSR_FEATURE));
    // A camera without the key serializes without it (existing projects are unchanged).
    let value = serde_json::to_value(&camera).unwrap();
    assert!(value.get("screen_space_reflections").is_none());

    let defaults: Camera3D =
        serde_json::from_value(json!({"screen_space_reflections":{}})).unwrap();
    assert_eq!(
        defaults.screen_space_reflections,
        Some(ScreenSpaceReflections3D::default())
    );
    defaults.validate().unwrap();
    let null: Camera3D = serde_json::from_value(json!({"screen_space_reflections":null})).unwrap();
    assert!(null.screen_space_reflections.is_none());

    let mut camera = Camera3D {
        screen_space_reflections: Some(ScreenSpaceReflections3D::default()),
        ..Default::default()
    };
    assert!(camera.effective_render_features()[SSR_FEATURE]);
    camera.render_features.insert(SSR_FEATURE.into(), false);
    assert!(!camera.effective_render_features()[SSR_FEATURE]);
    camera.render_features.clear();
    camera.screen_space_reflections = None;
    assert!(!camera.effective_render_features().contains_key(SSR_FEATURE));
}

#[test]
fn invalid_reflections_fail_before_gpu_and_schema_is_closed() {
    for bad in [
        json!({"screen_space_reflections":{"max_steps":0}}),
        json!({"screen_space_reflections":{"max_steps":257}}),
        json!({"screen_space_reflections":{"max_distance":0.0}}),
        json!({"screen_space_reflections":{"max_distance":10001.0}}),
        json!({"screen_space_reflections":{"thickness":0.0}}),
        json!({"screen_space_reflections":{"thickness":101.0}}),
        json!({"screen_space_reflections":{"roughness_cutoff":0.0}}),
        json!({"screen_space_reflections":{"roughness_cutoff":1.1}}),
        json!({"screen_space_reflections":{"edge_fade":0.6}}),
        json!({"screen_space_reflections":{"edge_fade":-0.1}}),
        json!({"screen_space_reflections":{"intensity":-1.0}}),
        json!({"screen_space_reflections":{"intensity":4.5}}),
    ] {
        let camera: Camera3D = serde_json::from_value(bad.clone()).unwrap();
        assert!(camera.validate().is_err(), "{bad}");
    }
    assert!(
        serde_json::from_value::<Camera3D>(json!({"screen_space_reflections":{"unknown":1}}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<Camera3D>(json!({"screen_space_reflections":{"max_steps":-1}}))
            .is_err()
    );
    for settings in [
        ScreenSpaceReflections3D {
            max_distance: f32::NAN,
            ..Default::default()
        },
        ScreenSpaceReflections3D {
            thickness: f32::INFINITY,
            ..Default::default()
        },
        ScreenSpaceReflections3D {
            roughness_cutoff: f32::NAN,
            ..Default::default()
        },
        ScreenSpaceReflections3D {
            edge_fade: f32::NAN,
            ..Default::default()
        },
        ScreenSpaceReflections3D {
            intensity: f32::INFINITY,
            ..Default::default()
        },
    ] {
        assert!(settings.validate().is_err(), "{settings:?}");
    }
    ScreenSpaceReflections3D {
        max_steps: MAX_SSR_STEPS,
        edge_fade: 0.0,
        intensity: 0.0,
        roughness_cutoff: 1.0,
        ..Default::default()
    }
    .validate()
    .unwrap();

    // An invalid authored value is rejected when the project loads and runs.
    let dir = write_project(json!({"screen_space_reflections":{"max_steps":0}}));
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(
        format!("{error:?}").contains("camera3d.screen_space_reflections.invalid"),
        "{error:?}"
    );

    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let shape = &doc.schema["properties"]["screen_space_reflections"]["oneOf"][0];
    assert_eq!(shape["additionalProperties"], false);
    for field in [
        "max_steps",
        "max_distance",
        "thickness",
        "roughness_cutoff",
        "edge_fade",
        "intensity",
    ] {
        assert!(shape["properties"][field].is_object(), "{field}");
    }
    assert_eq!(doc.schema["additionalProperties"], false);
}
