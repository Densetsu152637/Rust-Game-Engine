//! Authored ray-traced lighting: validation, schema documentation, document
//! edits (what MCP `component_set` applies), extraction and the status
//! before any frame.
use engine_project::rendering3d::RenderPipelineState;
use engine_project::{EditOperation, Project, Runtime, Scene, SceneDocument};
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
            &json!({"schema":1,"name":"rt","startup_scene":"scene.json","assets":[]}),
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
fn the_component_validates_and_is_documented() {
    scene_with(json!({"RayTracing":{"shadows":{"enabled":true,"sun_angular_radius":0.05}}}))
        .unwrap();
    assert!(scene_with(json!({"RayTracing":{"diffuse_gi":{"bounces":9}}})).is_err());
    assert!(scene_with(json!({"RayTracing":{"reflections":{"roughness_cutoff":2}}})).is_err());
    assert!(scene_with(json!({"RayTracing":{"shadows":{"soft":true}}})).is_err());
    let docs = engine_project::component_docs();
    assert!(engine_project::COMPONENT_NAMES.contains(&"RayTracing"));
    let doc = docs.iter().find(|d| d.name == "RayTracing").unwrap();
    for group in [
        "shadows",
        "ambient_occlusion",
        "reflections",
        "diffuse_gi",
        "denoiser",
    ] {
        assert_eq!(
            doc.schema["properties"][group]["additionalProperties"], false,
            "{group}"
        );
    }
    scene_with(json!({"RayTracing": doc.example.clone()})).unwrap();
}

#[test]
fn component_set_edits_and_the_status_before_a_frame() {
    let dir = fixture(json!([{"id":"camera","name":"Camera","components":
        {"Transform":{"position":[0,0,4]},"Camera3D":{}}}]));
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |value: serde_json::Value, merge: bool| EditOperation::SetComponent {
        entity_id: "camera".into(),
        component: "RayTracing".into(),
        value,
        merge,
    };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(set(json!({"shadows":{"enabled":true}}), false), &revision)
        .unwrap();
    let revision = document
        .apply(
            set(
                json!({"ambient_occlusion":{"enabled":true,"radius":2.0}}),
                true,
            ),
            &revision,
        )
        .unwrap();
    let rt = document.snapshot().entities[0]
        .components
        .ray_tracing
        .clone()
        .unwrap();
    assert!(rt.shadows.enabled && rt.ambient_occlusion.enabled);
    assert_eq!(rt.ambient_occlusion.radius, 2.0);
    let error = document
        .apply(
            set(json!({"ambient_occlusion":{"radius":0}}), true),
            &revision,
        )
        .unwrap_err();
    assert!(error.to_string().contains("RayTracing"), "{error}");
    document.save(&revision).unwrap();

    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.ray_tracing.as_ref().unwrap().shadows.enabled);
    let status = runtime.ray_tracing_status().unwrap();
    assert_eq!(status.state, RenderPipelineState::NotRendered);
    let modes: Vec<_> = status.effects.iter().map(|e| e.mode.as_str()).collect();
    assert_eq!(modes, ["fallback", "fallback", "disabled", "disabled"]);
    let snapshot = serde_json::to_value(runtime.snapshot()).unwrap();
    assert_eq!(snapshot["ray_tracing"]["effects"][0]["effect"], "shadows");
}
