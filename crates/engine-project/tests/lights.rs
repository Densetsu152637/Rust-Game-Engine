//! Authored light components: validation, schema documentation, document
//! edits (what MCP `component_set` applies), extraction and status.
use engine_project::lights::*;
use engine_project::rendering3d::RenderPipelineState;
use engine_project::{Components, EditOperation, Project, Runtime, Scene, SceneDocument};
use serde_json::json;
use std::fs;

fn fixture(entities: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"lights","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

fn scene_with(components: serde_json::Value) -> Result<Scene, String> {
    let scene: Scene = serde_json::from_value(json!({"schema":1,"scene_id":"s","entities":[
        {"id":"e","name":"E","components":components}]}))
    .map_err(|e| e.to_string())?;
    scene.validate().map_err(|e| e.to_string())?;
    Ok(scene)
}

#[test]
fn defaults_validate_and_out_of_range_values_are_rejected() {
    DirectionalLight::default().validate().unwrap();
    PointLight::default().validate().unwrap();
    SpotLight::default().validate().unwrap();
    AreaLight::default().validate().unwrap();
    let bad_directional = [
        DirectionalLight {
            lux: -1.0,
            ..Default::default()
        },
        DirectionalLight {
            lux: f32::NAN,
            ..Default::default()
        },
        DirectionalLight {
            color: [1.5, 0.0, 0.0],
            ..Default::default()
        },
        DirectionalLight {
            version: 2,
            ..Default::default()
        },
    ];
    for light in bad_directional {
        assert!(light.validate().is_err(), "{light:?}");
    }
    let shadows = |edit: fn(&mut DirectionalShadows)| {
        let mut light = DirectionalLight::default();
        edit(&mut light.shadows);
        light.validate()
    };
    assert!(shadows(|s| s.cascades = 0).is_err());
    assert!(shadows(|s| s.cascades = 5).is_err());
    assert!(shadows(|s| s.resolution = 1000).is_err());
    assert!(shadows(|s| s.split_lambda = 1.5).is_err());
    assert!(shadows(|s| s.cascade_blend = 0.6).is_err());
    assert!(shadows(|s| s.pcf_radius = 5).is_err());
    assert!(shadows(|s| s.angular_diameter_radians = 0.5).is_err());
    assert!(shadows(|s| s.max_distance = 0.0).is_err());
    shadows(|s| {
        s.cascades = 1;
        s.filter = ShadowFilterMode::Pcss;
    })
    .unwrap();

    for light in [
        PointLight {
            range: 0.0,
            ..Default::default()
        },
        PointLight {
            lumens: 1e9,
            ..Default::default()
        },
        PointLight {
            range: f32::INFINITY,
            ..Default::default()
        },
    ] {
        assert!(light.validate().is_err(), "{light:?}");
    }
    let mut point = PointLight::default();
    point.shadows.normal_bias = 17.0;
    assert!(point.validate().is_err());
    point.shadows.normal_bias = 1.0;
    point.shadows.near = 0.0;
    assert!(point.validate().is_err());

    for (inner, outer) in [(0.6, 0.5), (0.5, 0.5), (-0.1, 0.5), (0.2, 1.7)] {
        let light = SpotLight {
            inner_angle_radians: inner,
            outer_angle_radians: outer,
            ..Default::default()
        };
        assert!(light.validate().is_err(), "{inner} {outer}");
    }
    let area = |width: f32, height: f32, range: f32| {
        AreaLight {
            width,
            height,
            range,
            ..Default::default()
        }
        .validate()
    };
    assert!(area(0.0, 1.0, 5.0).is_err());
    assert!(area(4.0, 3.0, 2.4).is_err(), "range inside the rectangle");
    area(4.0, 3.0, 2.6).unwrap();
}

#[test]
fn scenes_accept_documented_light_json_and_reject_unknown_fields_and_missing_transforms() {
    scene_with(
        json!({"DirectionalLight":{"lux":100000,"shadows":{"enabled":true,"cascades":2,
        "filter":"pcss","angular_diameter_radians":0.0093}}}),
    )
    .unwrap();
    scene_with(json!({"Transform":{"position":[0,2,0]},
        "PointLight":{"lumens":800,"range":5,"shadows":{"enabled":true,"resolution":256}},
        "SpotLight":{"inner_angle_radians":0.2,"outer_angle_radians":0.4},
        "AreaLight":{"width":2,"height":1,"range":4,"two_sided":true}}))
    .unwrap();
    let error = scene_with(json!({"PointLight":{}})).unwrap_err();
    assert!(error.contains("require Transform"), "{error}");
    assert!(scene_with(json!({"Transform":{"position":[0,0,0]},"SpotLight":{"cone":1}})).is_err());
    assert!(
        scene_with(json!({"DirectionalLight":{"shadows":{"filter":"vsm"}}})).is_err(),
        "unknown filter"
    );
    let error = scene_with(json!({"Transform":{"position":[0,0,0]},
        "SpotLight":{"inner_angle_radians":0.5,"outer_angle_radians":0.4}}))
    .unwrap_err();
    assert!(error.contains("SpotLight"), "{error}");
    // Round trip: defaults are omitted, edits survive.
    let components: Components = serde_json::from_value(json!({
        "Transform":{"position":[0,0,0]},"AreaLight":{"width":3,"range":5}}))
    .unwrap();
    let value = serde_json::to_value(&components).unwrap();
    assert_eq!(value["AreaLight"]["width"], 3.0);
    assert_eq!(value["AreaLight"]["height"], 1.0);
}

#[test]
fn every_light_is_documented_with_a_valid_example() {
    let docs = engine_project::component_docs();
    for name in ["DirectionalLight", "PointLight", "SpotLight", "AreaLight"] {
        assert!(engine_project::COMPONENT_NAMES.contains(&name));
        let doc = docs.iter().find(|d| d.name == name).unwrap();
        let properties = doc.schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("shadows") && properties.contains_key("color"));
        assert_eq!(doc.schema["additionalProperties"], false);
        let mut components = json!({"Transform":{"position":[0,0,0]}});
        components[name] = doc.example.clone();
        scene_with(components).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn component_edits_add_change_and_remove_lights() {
    let dir = fixture(json!([
        {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{}}},
        {"id":"lamp","name":"Lamp","components":{"Transform":{"position":[0,3,0]}}}
    ]));
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set =
        |component: &str, value: serde_json::Value, merge: bool| EditOperation::SetComponent {
            entity_id: "lamp".into(),
            component: component.into(),
            value,
            merge,
        };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(
            set("SpotLight", json!({"lumens": 1500, "range": 7}), false),
            &revision,
        )
        .unwrap();
    let revision = document
        .apply(
            set("SpotLight", json!({"outer_angle_radians": 0.7}), true),
            &revision,
        )
        .unwrap();
    let spot = document.snapshot().entities[1]
        .components
        .spot_light
        .clone()
        .unwrap();
    assert_eq!(
        (spot.lumens, spot.range, spot.outer_angle_radians),
        (1500.0, 7.0, 0.7)
    );
    let error = document
        .apply(set("SpotLight", json!({"range": -1}), true), &revision)
        .unwrap_err();
    assert!(error.to_string().contains("SpotLight"), "{error}");
    let revision = document
        .apply(
            EditOperation::RemoveComponent {
                entity_id: "lamp".into(),
                component: "SpotLight".into(),
            },
            &revision,
        )
        .unwrap();
    assert!(
        document.snapshot().entities[1]
            .components
            .spot_light
            .is_none()
    );
    document
        .apply(set("PointLight", json!({}), false), &revision)
        .unwrap();
    assert!(
        document.snapshot().entities[1]
            .components
            .point_light
            .is_some()
    );
}

#[test]
fn extraction_orders_lights_skips_disabled_ones_and_uses_world_transforms() {
    let dir = fixture(json!([
        {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{}}},
        {"id":"b-sun","name":"Sun","components":{"DirectionalLight":{"lux":2}}},
        {"id":"rig","name":"Rig","components":{"Transform":{"position":[1,2,3]}}},
        {"id":"a-lamp","name":"Lamp","parent":"rig","components":{"Transform":{"position":[0,1,0]},
            "PointLight":{"lumens":100,"range":4}}},
        {"id":"c-off","name":"Off","components":{"Transform":{"position":[0,0,0]},
            "SpotLight":{"enabled":false}}}
    ]));
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let ids: Vec<&str> = frame.lights.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["a-lamp", "b-sun"]);
    assert!(matches!(frame.lights[0].light, SceneLight::Point(_)));
    assert_eq!(frame.lights[0].world[3][..3], [1.0, 3.0, 3.0], "parented");
    assert_eq!(frame.lights[1].world[3][..3], [0.0, 0.0, 0.0], "identity");

    let status = runtime.lighting_status().unwrap();
    assert_eq!(status.state, RenderPipelineState::NotRendered);
    assert_eq!(status.authored, 2);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert_eq!(snapshot["lighting"]["authored"], 2);
    assert_eq!(snapshot["lighting"]["state"], "not_rendered");
    assert!(snapshot.to_string().len() < 64 * 1024);
}
