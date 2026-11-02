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
            "schema":1,"name":"fog-test","startup_scene":"scene.json","assets":[]
        }))
        .unwrap(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json"));
    (dir, project)
}

#[test]
fn authored_fog_validates_loads_extracts_and_round_trips() {
    let (_dir, project) = project_with_camera(json!({
        "fog":{
            "density":0.02,"height_falloff":0.25,"base_height":-3.5,
            "albedo":[0.5,0.6,0.7],"anisotropy":-0.4,
            "sun_intensity":2.0,"ambient_intensity":0.5,"sky_distance":2500.0,
            "noise_amount":0.6,"noise_scale":8.0,"noise_distance":150.0,
            "noise_steps":24,"noise_seed":4000000000u64,"wind":[3.0,0.0,-1.5]
        }
    }));
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.unwrap().camera;
    let fog = camera.fog.unwrap();
    assert_eq!(fog.density, 0.02);
    assert_eq!(fog.base_height, -3.5);
    assert_eq!(fog.albedo, [0.5, 0.6, 0.7]);
    assert_eq!(fog.noise_steps, 24);
    assert_eq!(fog.noise_seed, 4_000_000_000);
    assert_eq!(fog.wind, [3.0, 0.0, -1.5]);
    assert!(camera.effective_render_features()[FOG_FEATURE]);

    let snapshot = runtime.try_snapshot().unwrap();
    let saved = snapshot.scene.entities[0]
        .components
        .camera3d
        .as_ref()
        .unwrap();
    assert_eq!(saved.fog, camera.fog);
    let roundtrip: Camera3D = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert_eq!(&roundtrip, saved);
}

#[test]
fn an_empty_object_means_the_defaults_and_absence_means_off() {
    let (_dir, project) = project_with_camera(json!({"fog":{}}));
    let frame = Runtime::new(project)
        .unwrap()
        .extract_scene_frame()
        .unwrap();
    let camera = frame.camera3d.unwrap().camera;
    assert_eq!(camera.fog, Some(Fog3D::default()));
    camera.validate().unwrap();
    // The defaults are pure height fog: no noise, no wind.
    let fog = camera.fog.unwrap();
    assert_eq!((fog.noise_amount, fog.wind), (0.0, [0.0; 3]));

    let plain = Camera3D::default();
    assert!(plain.fog.is_none());
    assert!(!plain.effective_render_features().contains_key(FOG_FEATURE));
    // Unset settings serialize to nothing, so existing projects are unchanged.
    assert!(serde_json::to_value(&plain).unwrap().get("fog").is_none());
    let null: Camera3D = serde_json::from_value(json!({"fog":null})).unwrap();
    assert!(null.fog.is_none());
}

#[test]
fn the_explicit_feature_override_wins_over_the_automatic_enable() {
    let mut camera = Camera3D {
        fog: Some(Fog3D::default()),
        ..Default::default()
    };
    assert!(camera.effective_render_features()[FOG_FEATURE]);
    camera.render_features.insert(FOG_FEATURE.into(), false);
    assert!(!camera.effective_render_features()[FOG_FEATURE]);
}

#[test]
fn the_scene_clock_follows_the_runtime_ticks_not_the_wall() {
    let (_dir, project) = project_with_camera(json!({"fog":{"wind":[1,0,0]}}));
    let mut runtime = Runtime::new(project).unwrap();
    assert_eq!(runtime.extract_scene_frame().unwrap().scene_time, 0.0);
    for _ in 0..4 {
        runtime
            .tick(0.25, engine_project::InputSnapshot::default())
            .unwrap();
    }
    assert_eq!(runtime.extract_scene_frame().unwrap().scene_time, 1.0);
    // Extracting again does not advance it.
    assert_eq!(runtime.extract_scene_frame().unwrap().scene_time, 1.0);
}

#[test]
fn invalid_fogs_fail_before_the_gpu_and_unknown_fields_are_rejected() {
    let bad = |value: serde_json::Value| {
        let camera: Camera3D = serde_json::from_value(json!({"fog": value})).unwrap();
        camera.validate().is_err()
    };
    assert!(bad(json!({"density":-0.1})));
    assert!(bad(json!({"density":11.0})));
    assert!(bad(json!({"height_falloff":-1.0})));
    assert!(bad(json!({"height_falloff":2.0})));
    assert!(bad(json!({"base_height":2.0e7})));
    assert!(bad(json!({"albedo":[0,0,1.5]})));
    assert!(bad(json!({"albedo":[-0.1,0,0]})));
    assert!(bad(json!({"anisotropy":0.995})));
    assert!(bad(json!({"anisotropy":-1.0})));
    assert!(bad(json!({"sun_intensity":-1.0})));
    assert!(bad(json!({"ambient_intensity":5000.0})));
    assert!(bad(json!({"sky_distance":-5.0})));
    assert!(bad(json!({"sky_distance":2.0e6})));
    assert!(bad(json!({"noise_amount":1.5})));
    assert!(bad(json!({"noise_scale":0.1})));
    assert!(bad(json!({"noise_distance":0.0})));
    assert!(bad(json!({"noise_distance":1.0e6})));
    assert!(bad(json!({"noise_steps":0})));
    assert!(bad(json!({"noise_steps":65})));
    assert!(bad(json!({"wind":[0,0,250]})));
    assert!(bad(json!({"wind":[-201,0,0]})));
    for nan in [
        Fog3D {
            density: f32::NAN,
            ..Default::default()
        },
        Fog3D {
            height_falloff: f32::INFINITY,
            ..Default::default()
        },
        Fog3D {
            wind: [0.0, f32::NAN, 0.0],
            ..Default::default()
        },
        Fog3D {
            noise_scale: f32::NAN,
            ..Default::default()
        },
        Fog3D {
            base_height: f32::NEG_INFINITY,
            ..Default::default()
        },
    ] {
        assert!(nan.validate().is_err(), "{nan:?}");
    }
    Fog3D::default().validate().unwrap();
    // Zero is a valid extreme for the optional terms.
    Fog3D {
        density: 0.0,
        height_falloff: 0.0,
        sun_intensity: 0.0,
        ambient_intensity: 0.0,
        sky_distance: 0.0,
        noise_amount: 0.0,
        ..Default::default()
    }
    .validate()
    .unwrap();

    assert!(serde_json::from_value::<Camera3D>(json!({"fog":{"unknown":1}})).is_err());
    // A project with a bad fog is rejected when it loads.
    let (_dir, project) = try_project_with_camera(json!({"fog":{"density":-2.0}}));
    assert!(project.is_err(), "loading validates the authored camera");
}

#[test]
fn schema_documents_every_field_as_a_closed_object() {
    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let shape = &doc.schema["properties"]["fog"]["oneOf"][0];
    assert_eq!(shape["additionalProperties"], false);
    let fields = [
        "density",
        "height_falloff",
        "base_height",
        "albedo",
        "anisotropy",
        "sun_intensity",
        "ambient_intensity",
        "sky_distance",
        "noise_amount",
        "noise_scale",
        "noise_distance",
        "noise_steps",
        "noise_seed",
        "wind",
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
    // each documented numeric default is the struct's default.
    let serialized = serde_json::to_value(Fog3D::default()).unwrap();
    let mut keys: Vec<_> = serialized.as_object().unwrap().keys().cloned().collect();
    let mut documented: Vec<_> = fields.iter().map(|f| f.to_string()).collect();
    keys.sort();
    documented.sort();
    assert_eq!(keys, documented);
    for field in fields {
        let (default, actual) = (&shape["properties"][field]["default"], &serialized[field]);
        match (default.as_f64(), actual.as_f64()) {
            (Some(a), Some(b)) => assert!((a - b).abs() < 1e-6, "{field}: {a} vs {b}"),
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
    assert_eq!(doc.schema["properties"]["fog"]["oneOf"][1]["type"], "null");
}
