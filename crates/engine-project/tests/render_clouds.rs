use engine_project::{Project, Runtime, rendering3d::*};
use serde_json::json;
use std::fs;

fn project_with_camera(camera: serde_json::Value) -> (tempfile::TempDir, Project) {
    let (dir, project) = try_project_with_camera(camera);
    (dir, project.unwrap())
}

fn try_project_with_camera(
    camera: serde_json::Value,
) -> (tempfile::TempDir, engine_project::Result<Project>) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({
            "schema":1,"scene_id":"scene","entities":[
                {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,2,4]},"Camera3D":camera}}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"clouds-test","startup_scene":"scene.json","assets":[]
        }))
        .unwrap(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json"));
    (dir, project)
}

#[test]
fn authored_clouds_validate_load_extract_and_round_trip() {
    let (_dir, project) = project_with_camera(json!({
        "clouds":{
            "base_altitude":1200.0,"top_altitude":2600.0,"density":0.006,"coverage":0.65,
            "albedo":[0.9,0.95,1.0],"anisotropy":-0.2,"sun_intensity":2.0,
            "ambient_intensity":0.5,"multiple_scattering":false,"noise_amount":0.8,
            "noise_scale":900.0,"noise_octaves":2,"noise_seed":4000000000u64,
            "wind":[12.0,0.0,-6.0],"max_distance":30000.0,"fade_distance":25000.0,
            "primary_steps":32,"light_steps":4,"light_distance":500.0
        }
    }));
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.unwrap().camera;
    let clouds = camera.clouds.unwrap();
    assert_eq!(clouds.base_altitude, 1200.0);
    assert_eq!(clouds.top_altitude, 2600.0);
    assert_eq!(clouds.coverage, 0.65);
    assert_eq!(clouds.albedo, [0.9, 0.95, 1.0]);
    assert!(!clouds.multiple_scattering);
    assert_eq!(clouds.noise_seed, 4_000_000_000);
    assert_eq!(clouds.wind, [12.0, 0.0, -6.0]);
    assert_eq!((clouds.primary_steps, clouds.light_steps), (32, 4));
    assert!(camera.effective_render_features()[CLOUDS_FEATURE]);

    let snapshot = runtime.try_snapshot().unwrap();
    let saved = snapshot.scene.entities[0]
        .components
        .camera3d
        .as_ref()
        .unwrap();
    assert_eq!(saved.clouds, camera.clouds);
    let roundtrip: Camera3D = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert_eq!(&roundtrip, saved);
}

#[test]
fn an_empty_object_means_the_defaults_and_absence_means_off() {
    let (_dir, project) = project_with_camera(json!({"clouds":{}}));
    let frame = Runtime::new(project)
        .unwrap()
        .extract_scene_frame()
        .unwrap();
    let camera = frame.camera3d.unwrap().camera;
    assert_eq!(camera.clouds, Some(Clouds3D::default()));
    camera.validate().unwrap();

    let plain = Camera3D::default();
    assert!(plain.clouds.is_none());
    assert!(
        !plain
            .effective_render_features()
            .contains_key(CLOUDS_FEATURE)
    );
    // Unset settings serialize to nothing, so existing projects are unchanged.
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("clouds")
            .is_none()
    );
    let null: Camera3D = serde_json::from_value(json!({"clouds":null})).unwrap();
    assert!(null.clouds.is_none());
}

#[test]
fn the_explicit_feature_override_wins_over_the_automatic_enable() {
    let mut camera = Camera3D {
        clouds: Some(Clouds3D::default()),
        ..Default::default()
    };
    assert!(camera.effective_render_features()[CLOUDS_FEATURE]);
    camera.render_features.insert(CLOUDS_FEATURE.into(), false);
    assert!(!camera.effective_render_features()[CLOUDS_FEATURE]);
    // Fog and clouds enable independently.
    let both = Camera3D {
        clouds: Some(Clouds3D::default()),
        fog: Some(Fog3D::default()),
        ..Default::default()
    };
    let features = both.effective_render_features();
    assert!(features[CLOUDS_FEATURE] && features[FOG_FEATURE]);
}

#[test]
fn invalid_clouds_fail_before_the_gpu_and_unknown_fields_are_rejected() {
    let bad = |value: serde_json::Value| {
        let camera: Camera3D = serde_json::from_value(json!({"clouds": value})).unwrap();
        camera.validate().is_err()
    };
    // Base at or above the top, and out-of-range thicknesses.
    assert!(bad(json!({"base_altitude":2000.0,"top_altitude":2000.0})));
    assert!(bad(json!({"base_altitude":3000.0,"top_altitude":1000.0})));
    assert!(bad(json!({"base_altitude":0.0,"top_altitude":0.5})));
    assert!(bad(json!({"base_altitude":0.0,"top_altitude":200000.0})));
    assert!(bad(json!({"base_altitude":-2.0e7,"top_altitude":0.0})));
    assert!(bad(json!({"density":-0.1})));
    assert!(bad(json!({"density":1.5})));
    assert!(bad(json!({"coverage":-0.1})));
    assert!(bad(json!({"coverage":1.1})));
    assert!(bad(json!({"albedo":[0,0,1.5]})));
    assert!(bad(json!({"albedo":[-0.1,0,0]})));
    assert!(bad(json!({"anisotropy":0.995})));
    assert!(bad(json!({"anisotropy":-1.0})));
    assert!(bad(json!({"sun_intensity":-1.0})));
    assert!(bad(json!({"ambient_intensity":5000.0})));
    assert!(bad(json!({"noise_amount":1.5})));
    assert!(bad(json!({"noise_scale":0.5})));
    assert!(bad(json!({"noise_scale":2.0e6})));
    assert!(bad(json!({"noise_octaves":0})));
    assert!(bad(json!({"noise_octaves":5})));
    assert!(bad(json!({"wind":[0,0,250]})));
    assert!(bad(json!({"wind":[-201,0,0]})));
    assert!(bad(json!({"max_distance":0.0})));
    assert!(bad(json!({"max_distance":2.0e6})));
    assert!(bad(json!({"fade_distance":-1.0})));
    assert!(bad(json!({"fade_distance":2.0e7})));
    assert!(bad(json!({"primary_steps":0})));
    assert!(bad(json!({"primary_steps":97})));
    assert!(bad(json!({"light_steps":0})));
    assert!(bad(json!({"light_steps":13})));
    assert!(bad(json!({"light_distance":0.0})));
    assert!(bad(json!({"light_distance":2.0e5})));
    for nan in [
        Clouds3D {
            density: f32::NAN,
            ..Default::default()
        },
        Clouds3D {
            coverage: f32::INFINITY,
            ..Default::default()
        },
        Clouds3D {
            wind: [0.0, f32::NAN, 0.0],
            ..Default::default()
        },
        Clouds3D {
            base_altitude: f32::NAN,
            ..Default::default()
        },
        Clouds3D {
            top_altitude: f32::INFINITY,
            ..Default::default()
        },
        Clouds3D {
            noise_scale: f32::NAN,
            ..Default::default()
        },
    ] {
        assert!(nan.validate().is_err(), "{nan:?}");
    }
    Clouds3D::default().validate().unwrap();
    // Zero is a valid extreme for the optional terms.
    Clouds3D {
        density: 0.0,
        coverage: 0.0,
        sun_intensity: 0.0,
        ambient_intensity: 0.0,
        noise_amount: 0.0,
        fade_distance: 0.0,
        ..Default::default()
    }
    .validate()
    .unwrap();

    assert!(serde_json::from_value::<Camera3D>(json!({"clouds":{"unknown":1}})).is_err());
    // A project with bad clouds is rejected when it loads.
    let (_dir, project) =
        try_project_with_camera(json!({"clouds":{"base_altitude":5.0,"top_altitude":1.0}}));
    assert!(project.is_err(), "loading validates the authored camera");
}

#[test]
fn the_scene_clock_follows_the_runtime_ticks_for_the_wind() {
    let (_dir, project) = project_with_camera(json!({"clouds":{"wind":[1,0,0]}}));
    let mut runtime = Runtime::new(project).unwrap();
    assert_eq!(runtime.extract_scene_frame().unwrap().scene_time, 0.0);
    for _ in 0..4 {
        runtime
            .tick(0.25, engine_project::InputSnapshot::default())
            .unwrap();
    }
    assert_eq!(runtime.extract_scene_frame().unwrap().scene_time, 1.0);
}

#[test]
fn schema_documents_every_field_as_a_closed_object() {
    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let shape = &doc.schema["properties"]["clouds"]["oneOf"][0];
    assert_eq!(shape["additionalProperties"], false);
    let fields = [
        "base_altitude",
        "top_altitude",
        "density",
        "coverage",
        "albedo",
        "anisotropy",
        "sun_intensity",
        "ambient_intensity",
        "multiple_scattering",
        "noise_amount",
        "noise_scale",
        "noise_octaves",
        "noise_seed",
        "wind",
        "max_distance",
        "fade_distance",
        "primary_steps",
        "light_steps",
        "light_distance",
    ];
    for field in fields {
        assert!(shape["properties"][field].is_object(), "{field} documented");
        assert!(
            shape["properties"][field]["description"].is_string(),
            "{field} described"
        );
    }
    assert_eq!(shape["properties"].as_object().unwrap().len(), fields.len());
    // The documented fields are exactly the struct's serialized fields, and
    // each documented default is the struct's default.
    let serialized = serde_json::to_value(Clouds3D::default()).unwrap();
    let mut keys: Vec<_> = serialized.as_object().unwrap().keys().cloned().collect();
    let mut documented: Vec<_> = fields.iter().map(|f| f.to_string()).collect();
    keys.sort();
    documented.sort();
    assert_eq!(keys, documented);
    for field in fields {
        let (default, actual) = (&shape["properties"][field]["default"], &serialized[field]);
        match (default.as_f64(), actual.as_f64()) {
            (Some(a), Some(b)) => assert!((a - b).abs() < 1e-6, "{field}: {a} vs {b}"),
            _ if default.is_boolean() => assert_eq!(default, actual, "{field}"),
            _ => {
                let close = |a: &serde_json::Value, b: &serde_json::Value| {
                    a.as_array().zip(b.as_array()).is_some_and(|(a, b)| {
                        a.iter()
                            .zip(b)
                            .all(|(x, y)| (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-6)
                    })
                };
                assert!(close(default, actual), "{field}: {default} vs {actual}");
            }
        }
    }
    assert_eq!(
        doc.schema["properties"]["clouds"]["oneOf"][1]["type"],
        "null"
    );
}
