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
            "schema":1,"name":"atmosphere-test","startup_scene":"scene.json","assets":[]
        }))
        .unwrap(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json"));
    (dir, project)
}

#[test]
fn authored_atmosphere_validates_loads_extracts_and_round_trips() {
    let (_dir, project) = project_with_camera(json!({
        "atmosphere":{
            "planet_radius":1_000_000.0,"atmosphere_radius":1_060_000.0,
            "rayleigh_scattering":[1e-5,2e-5,4e-5],"rayleigh_scale_height":6000.0,
            "mie_scattering":5e-6,"mie_absorption":1e-6,"mie_scale_height":1500.0,
            "mie_anisotropy":-0.2,"ozone_absorption":[0,0,0],
            "ozone_center_height":20000.0,"ozone_width":9000.0,
            "ground_albedo":[0.1,0.2,0.3],"sun_angular_diameter":0.02,
            "intensity":2.5,"aerial_perspective_scale":8.0
        }
    }));
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.unwrap().camera;
    let atmosphere = camera.atmosphere.unwrap();
    assert_eq!(atmosphere.planet_radius, 1_000_000.0);
    assert_eq!(atmosphere.mie_anisotropy, -0.2);
    assert_eq!(atmosphere.ground_albedo, [0.1, 0.2, 0.3]);
    assert_eq!(atmosphere.aerial_perspective_scale, 8.0);
    assert!(camera.effective_render_features()[ATMOSPHERE_FEATURE]);
    assert!(camera.effective_render_features()[ATMOSPHERE_AERIAL_FEATURE]);

    let snapshot = runtime.try_snapshot().unwrap();
    let saved = snapshot.scene.entities[0]
        .components
        .camera3d
        .as_ref()
        .unwrap();
    assert_eq!(saved.atmosphere, camera.atmosphere);
    let roundtrip: Camera3D = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert_eq!(&roundtrip, saved);
}

#[test]
fn an_empty_object_means_earth_defaults_and_absence_means_off() {
    let (_dir, project) = project_with_camera(json!({"atmosphere":{}}));
    let frame = Runtime::new(project)
        .unwrap()
        .extract_scene_frame()
        .unwrap();
    let camera = frame.camera3d.unwrap().camera;
    assert_eq!(camera.atmosphere, Some(Atmosphere3D::default()));
    camera.validate().unwrap();
    assert_eq!(camera.atmosphere.unwrap().planet_radius, 6_360_000.0);

    let plain = Camera3D::default();
    assert!(plain.atmosphere.is_none());
    let features = plain.effective_render_features();
    assert!(!features.contains_key(ATMOSPHERE_FEATURE));
    assert!(!features.contains_key(ATMOSPHERE_AERIAL_FEATURE));
    // Unset settings serialize to nothing, so existing projects are unchanged.
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("atmosphere")
            .is_none()
    );
    let null: Camera3D = serde_json::from_value(json!({"atmosphere":null})).unwrap();
    assert!(null.atmosphere.is_none());
}

#[test]
fn explicit_feature_overrides_win_over_the_automatic_enable() {
    let mut camera = Camera3D {
        atmosphere: Some(Atmosphere3D::default()),
        ..Default::default()
    };
    camera
        .render_features
        .insert(ATMOSPHERE_AERIAL_FEATURE.into(), false);
    let features = camera.effective_render_features();
    assert!(features[ATMOSPHERE_FEATURE]);
    assert!(!features[ATMOSPHERE_AERIAL_FEATURE]);
    camera
        .render_features
        .insert(ATMOSPHERE_FEATURE.into(), false);
    assert!(!camera.effective_render_features()[ATMOSPHERE_FEATURE]);
}

#[test]
fn invalid_atmospheres_fail_before_the_gpu_and_unknown_fields_are_rejected() {
    let bad = |value: serde_json::Value| {
        let camera: Camera3D = serde_json::from_value(json!({"atmosphere": value})).unwrap();
        camera.validate().is_err()
    };
    assert!(bad(json!({"planet_radius":10.0})));
    assert!(bad(json!({"planet_radius":1.0e9})));
    assert!(bad(json!({"atmosphere_radius":6_360_000.0})));
    assert!(bad(json!({"atmosphere_radius":6_360_050.0})));
    assert!(bad(json!({"atmosphere_radius":3.0e7})));
    assert!(bad(json!({"rayleigh_scattering":[-1e-6,0,0]})));
    assert!(bad(json!({"rayleigh_scattering":[0,0,0.5]})));
    assert!(bad(json!({"mie_scattering":0.5})));
    assert!(bad(json!({"mie_absorption":-1.0})));
    assert!(bad(json!({"ozone_absorption":[0,1.0,0]})));
    assert!(bad(json!({"rayleigh_scale_height":0.0})));
    assert!(bad(json!({"mie_scale_height":2.0e6})));
    assert!(bad(json!({"mie_anisotropy":0.995})));
    assert!(bad(json!({"mie_anisotropy":-1.0})));
    assert!(bad(json!({"ozone_width":0.0})));
    assert!(bad(json!({"ozone_center_height":-5.0})));
    assert!(bad(json!({"ground_albedo":[0,0,1.5]})));
    assert!(bad(json!({"sun_angular_diameter":0.6})));
    assert!(bad(json!({"intensity":-1.0})));
    assert!(bad(json!({"intensity":5000.0})));
    assert!(bad(json!({"aerial_perspective_scale":-0.5})));
    assert!(
        Atmosphere3D {
            intensity: f32::NAN,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Atmosphere3D {
            planet_radius: f32::INFINITY,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Atmosphere3D {
            ground_albedo: [f32::NAN, 0.0, 0.0],
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    Atmosphere3D::default().validate().unwrap();
    // Zero is a valid extreme for the optional visual terms.
    Atmosphere3D {
        sun_angular_diameter: 0.0,
        aerial_perspective_scale: 0.0,
        intensity: 0.0,
        mie_scattering: 0.0,
        ..Default::default()
    }
    .validate()
    .unwrap();

    assert!(serde_json::from_value::<Camera3D>(json!({"atmosphere":{"unknown":1}})).is_err());
    // A project with a bad atmosphere is rejected when the runtime starts.
    let (_dir, project) = try_project_with_camera(json!({"atmosphere":{"mie_anisotropy":2.0}}));
    assert!(project.is_err(), "loading validates the authored camera");
}

#[test]
fn schema_documents_every_field_as_a_closed_object() {
    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let shape = &doc.schema["properties"]["atmosphere"]["oneOf"][0];
    assert_eq!(shape["additionalProperties"], false);
    let fields = [
        "planet_radius",
        "atmosphere_radius",
        "rayleigh_scattering",
        "rayleigh_scale_height",
        "mie_scattering",
        "mie_absorption",
        "mie_scale_height",
        "mie_anisotropy",
        "ozone_absorption",
        "ozone_center_height",
        "ozone_width",
        "ground_albedo",
        "sun_angular_diameter",
        "intensity",
        "aerial_perspective_scale",
    ];
    for field in fields {
        assert!(shape["properties"][field].is_object(), "{field} documented");
        assert!(
            shape["properties"][field]["description"].is_string(),
            "{field} described"
        );
    }
    assert_eq!(
        shape["properties"].as_object().unwrap().len(),
        fields.len(),
        "the schema lists exactly the serialized fields"
    );
    // The documented fields are exactly the struct's serialized fields.
    let serialized = serde_json::to_value(Atmosphere3D::default()).unwrap();
    let mut keys: Vec<_> = serialized.as_object().unwrap().keys().cloned().collect();
    let mut documented: Vec<_> = fields.iter().map(|f| f.to_string()).collect();
    keys.sort();
    documented.sort();
    assert_eq!(keys, documented);
    assert_eq!(
        doc.schema["properties"]["atmosphere"]["oneOf"][1]["type"],
        "null"
    );
}
