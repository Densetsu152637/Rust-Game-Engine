//! Authored post-processing: validation, schema documentation, document
//! edits (what MCP `component_set` applies), extraction, LUT assets and
//! status.
use engine_project::post::*;
use engine_project::rendering3d::RenderPipelineState;
use engine_project::{EditOperation, Project, Runtime, Scene, SceneDocument};
use serde_json::json;
use std::fs;

fn fixture(entities: serde_json::Value, assets: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"post","startup_scene":"scene.json","assets":assets}),
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
fn components_validate_and_reject_bad_data() {
    scene_with(json!({"PostProcess":{"tonemap":"aces","bloom":{"enabled":true}}})).unwrap();
    scene_with(json!({"PostProcessVolume":{"tonemap":"agx"}})).unwrap();
    assert!(scene_with(json!({"PostProcess":{"tonemap":"filmic"}})).is_err());
    assert!(scene_with(json!({"PostProcess":{"bloom":{"scatter":2}}})).is_err());
    assert!(scene_with(json!({"PostProcess":{"unknown":1}})).is_err());
    assert!(scene_with(json!({"PostProcessVolume":{"grading":{"saturation":9}}})).is_err());
    let error = scene_with(json!({"PostProcessVolume":{"shape":{"type":"sphere","radius":2}}}))
        .unwrap_err();
    assert!(error.contains("Transform"), "{error}");
    scene_with(json!({"Transform":{"position":[0,0,0]},
        "PostProcessVolume":{"shape":{"type":"sphere","radius":2}}}))
    .unwrap();
}

#[test]
fn both_components_are_documented_with_valid_examples() {
    let docs = engine_project::component_docs();
    for name in ["PostProcess", "PostProcessVolume"] {
        assert!(engine_project::COMPONENT_NAMES.contains(&name));
        let doc = docs.iter().find(|d| d.name == name).unwrap();
        assert_eq!(doc.schema["additionalProperties"], false);
        for group in ["exposure", "bloom", "grading", "vignette", "grain"] {
            assert!(
                doc.schema["properties"][group].is_object(),
                "{name}.{group}"
            );
        }
        let mut components = json!({"Transform":{"position":[0,0,0]}});
        components[name] = doc.example.clone();
        scene_with(components).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let volume = docs.iter().find(|d| d.name == "PostProcessVolume").unwrap();
    assert!(
        volume.schema["properties"]["bloom"]["properties"]["intensity"]
            .get("default")
            .is_none(),
        "override fields have no default: unset means not overridden"
    );
}

#[test]
fn component_edits_set_merge_and_remove() {
    let dir = fixture(
        json!([{"id":"camera","name":"Camera","components":
            {"Transform":{"position":[0,0,4]},"Camera3D":{}}}]),
        json!([]),
    );
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set =
        |component: &str, value: serde_json::Value, merge: bool| EditOperation::SetComponent {
            entity_id: "camera".into(),
            component: component.into(),
            value,
            merge,
        };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(
            set("PostProcess", json!({"tonemap":"aces"}), false),
            &revision,
        )
        .unwrap();
    let revision = document
        .apply(
            set(
                "PostProcess",
                json!({"bloom":{"enabled":true,"intensity":0.5}}),
                true,
            ),
            &revision,
        )
        .unwrap();
    let post = document.snapshot().entities[0]
        .components
        .post_process
        .clone()
        .unwrap();
    assert_eq!(post.tonemap, TonemapMode::Aces);
    assert!(post.bloom.enabled && post.bloom.intensity == 0.5);
    let error = document
        .apply(
            set("PostProcess", json!({"grain":{"intensity":3}}), true),
            &revision,
        )
        .unwrap_err();
    assert!(error.to_string().contains("PostProcess"), "{error}");
    let revision = document
        .apply(
            EditOperation::RemoveComponent {
                entity_id: "camera".into(),
                component: "PostProcess".into(),
            },
            &revision,
        )
        .unwrap();
    assert!(
        document.snapshot().entities[0]
            .components
            .post_process
            .is_none(),
        "{revision}"
    );
}

#[test]
fn extraction_resolves_volumes_at_the_camera_and_reports_status() {
    let dir = fixture(
        json!([
            {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,0]},
                "Camera3D":{},"PostProcess":{"tonemap":"aces"}}},
            {"id":"zone","name":"Zone","components":{"Transform":{"position":[0,0,0]},
                "PostProcessVolume":{"shape":{"type":"box","size":[4,4,4]},"priority":2,
                    "tonemap":"agx","vignette":{"enabled":true,"intensity":0.7}}}},
            {"id":"far","name":"Far","components":{"Transform":{"position":[100,0,0]},
                "PostProcessVolume":{"shape":{"type":"sphere","radius":1},"tonemap":"neutral"}}},
            {"id":"off","name":"Off","components":{"PostProcessVolume":{"enabled":false}}}
        ]),
        json!([]),
    );
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.post.volumes.len(), 2, "disabled volume skipped");
    assert_eq!(
        frame.post.camera.as_ref().unwrap().tonemap,
        TonemapMode::Aces
    );
    let resolved = resolve_post(frame.post.camera.as_ref(), &frame.post.volumes, [0.0; 3]).unwrap();
    assert_eq!(resolved.settings.tonemap, TonemapMode::Agx);
    assert!(resolved.settings.vignette.enabled);
    assert_eq!(resolved.active.len(), 1);
    assert_eq!(resolved.active[0].id, "zone");
    let away = resolve_post(
        frame.post.camera.as_ref(),
        &frame.post.volumes,
        [50.0, 0.0, 0.0],
    )
    .unwrap();
    assert_eq!(away.settings.tonemap, TonemapMode::Aces);
    assert!(away.active.is_empty());

    let status = runtime.post_status().unwrap();
    assert_eq!(status.state, RenderPipelineState::NotRendered);
    assert!(status.active);
    assert_eq!(status.volumes, 2);
    assert_eq!(status.tonemap, Some(TonemapMode::Agx));
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert_eq!(snapshot["post"]["active_volumes"][0]["id"], "zone");
    assert_eq!(snapshot["post"]["tonemap"], "agx");
    assert!(snapshot.to_string().len() < 64 * 1024);
}

#[test]
fn a_camera_without_post_data_reports_inactive() {
    let dir = fixture(
        json!([{"id":"camera","name":"Camera","components":
            {"Transform":{"position":[0,0,0]},"Camera3D":{}}}]),
        json!([]),
    );
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.post.camera.is_none() && frame.post.volumes.is_empty());
    let status = runtime.post_status().unwrap();
    assert!(!status.active);
}

#[test]
fn lut_assets_are_validated_and_loaded() {
    let dir = fixture(
        json!([{"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,0]},
            "Camera3D":{},"PostProcess":{"grading":{"enabled":true,"lut":"warm"}}}}]),
        json!([{"id":"warm","kind":"lut","path":"warm.cube"}]),
    );
    fs::write(dir.path().join("warm.cube"), "LUT_3D_SIZE 2\n").unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.post.luts["warm"].contains("LUT_3D_SIZE"));
    // An unknown or wrongly typed LUT asset is rejected at validation.
    let dir = fixture(
        json!([{"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,0]},
            "Camera3D":{},"PostProcess":{"grading":{"lut":"missing"}}}}]),
        json!([]),
    );
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(error.to_string().contains("Unknown asset missing"), "{error}");
}
