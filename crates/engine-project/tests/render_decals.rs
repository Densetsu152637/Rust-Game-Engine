use engine_project::{Project, Runtime, rendering3d::*};
use serde_json::json;
use std::fs;

fn write_project(camera: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({
            "schema":1,"scene_id":"scene","entities":[
                {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":camera}}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"decals-test","startup_scene":"scene.json","assets":[]
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
fn authored_decals_validate_load_extract_and_snapshot() {
    let (_dir, project) = project_with_camera(json!({
        "decals":[
            {"position":[1.0,0.0,-2.0],"rotation":[0.0,0.0,0.0,1.0],"half_extents":[0.5,0.5,1.0],
             "albedo":[0.8,0.1,0.1],"opacity":0.75,"blend":"paint","emissive":[0.0,0.0,2.0],
             "max_angle":1.0,"angle_fade":0.25,"edge_fade":0.3},
            {}
        ]
    }));
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.unwrap().camera;
    assert_eq!(camera.decals.len(), 2);
    let first = camera.decals[0];
    assert_eq!(first.position, [1.0, 0.0, -2.0]);
    assert_eq!(first.half_extents, [0.5, 0.5, 1.0]);
    assert_eq!(first.albedo, [0.8, 0.1, 0.1]);
    assert_eq!(first.opacity, 0.75);
    assert_eq!(first.blend, DecalBlend3D::Paint);
    assert_eq!(first.emissive, [0.0, 0.0, 2.0]);
    assert_eq!(
        (first.max_angle, first.angle_fade, first.edge_fade),
        (1.0, 0.25, 0.3)
    );
    assert_eq!(camera.decals[1], Decal3D::default());
    assert!(camera.effective_render_features()[DECALS_FEATURE]);
    let snapshot = runtime.try_snapshot().unwrap();
    let saved = snapshot.scene.entities[0]
        .components
        .camera3d
        .as_ref()
        .unwrap();
    assert_eq!(saved.decals, camera.decals);
    let roundtrip: Camera3D = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert_eq!(&roundtrip, saved);
}

#[test]
fn decals_default_off_and_render_features_override() {
    let camera = Camera3D::default();
    assert!(camera.decals.is_empty());
    assert!(
        !camera
            .effective_render_features()
            .contains_key(DECALS_FEATURE)
    );
    // A camera without decals serializes without the key (existing projects are unchanged).
    assert!(
        serde_json::to_value(&camera)
            .unwrap()
            .get("decals")
            .is_none()
    );

    let mut camera = Camera3D {
        decals: vec![Decal3D::default()],
        ..Default::default()
    };
    assert!(camera.effective_render_features()[DECALS_FEATURE]);
    camera.render_features.insert(DECALS_FEATURE.into(), false);
    assert!(!camera.effective_render_features()[DECALS_FEATURE]);
    camera.render_features.clear();
    camera.decals.clear();
    assert!(
        !camera
            .effective_render_features()
            .contains_key(DECALS_FEATURE)
    );
}

#[test]
fn invalid_decals_fail_validation_and_the_schema_is_closed() {
    let bad_decals = [
        json!({"position":[0.0,0.0,1e9]}),
        json!({"rotation":[0.0,0.0,0.0,0.0]}),
        json!({"half_extents":[0.0,1.0,1.0]}),
        json!({"half_extents":[1.0,-1.0,1.0]}),
        json!({"half_extents":[1.0,1.0,1e9]}),
        json!({"albedo":[1.5,0.0,0.0]}),
        json!({"albedo":[0.0,-0.1,0.0]}),
        json!({"opacity":1.5}),
        json!({"opacity":-0.1}),
        json!({"emissive":[0.0,0.0,-1.0]}),
        json!({"emissive":[0.0,0.0,1001.0]}),
        json!({"max_angle":0.0}),
        json!({"max_angle":1.6}),
        json!({"max_angle":0.5,"angle_fade":0.6}),
        json!({"angle_fade":-0.1}),
        json!({"edge_fade":1.5}),
        json!({"edge_fade":-0.1}),
    ];
    for bad in bad_decals {
        let camera: Camera3D = serde_json::from_value(json!({"decals":[bad.clone()]})).unwrap();
        assert!(camera.validate().is_err(), "{bad}");
    }
    for decal in [
        Decal3D {
            position: [f32::NAN, 0.0, 0.0],
            ..Default::default()
        },
        Decal3D {
            rotation: [f32::INFINITY, 0.0, 0.0, 1.0],
            ..Default::default()
        },
        Decal3D {
            half_extents: [1.0, f32::NAN, 1.0],
            ..Default::default()
        },
        Decal3D {
            opacity: f32::NAN,
            ..Default::default()
        },
        Decal3D {
            max_angle: f32::NAN,
            ..Default::default()
        },
        Decal3D {
            edge_fade: f32::INFINITY,
            ..Default::default()
        },
    ] {
        assert!(decal.validate().is_err(), "{decal:?}");
    }
    // Boundary values are accepted.
    Decal3D {
        opacity: 0.0,
        edge_fade: 1.0,
        angle_fade: 0.0,
        max_angle: std::f32::consts::FRAC_PI_2,
        emissive: [1000.0; 3],
        ..Default::default()
    }
    .validate()
    .unwrap();

    // Overflow: 64 are accepted, 65 are rejected before any GPU work.
    let mut camera = Camera3D {
        decals: vec![Decal3D::default(); MAX_DECALS_3D],
        ..Default::default()
    };
    camera.validate().unwrap();
    camera.decals.push(Decal3D::default());
    assert!(camera.validate().is_err());

    assert!(
        serde_json::from_value::<Camera3D>(json!({"decals":[{"unknown":1}]})).is_err(),
        "unknown decal fields are rejected"
    );
    assert!(
        serde_json::from_value::<Camera3D>(json!({"decals":[{"blend":"screen"}]})).is_err(),
        "unknown blend modes are rejected"
    );

    // An invalid authored value is rejected when the project loads.
    let dir = write_project(json!({"decals":[{"opacity":2.0}]}));
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(
        format!("{error:?}").contains("camera3d.decals.invalid"),
        "{error:?}"
    );

    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let list = &doc.schema["properties"]["decals"];
    assert_eq!(list["maxItems"], 64);
    let shape = &list["items"];
    assert_eq!(shape["additionalProperties"], false);
    for field in [
        "position",
        "rotation",
        "half_extents",
        "albedo",
        "opacity",
        "blend",
        "emissive",
        "max_angle",
        "angle_fade",
        "edge_fade",
    ] {
        assert!(shape["properties"][field].is_object(), "{field}");
    }
    assert_eq!(doc.schema["additionalProperties"], false);
}
