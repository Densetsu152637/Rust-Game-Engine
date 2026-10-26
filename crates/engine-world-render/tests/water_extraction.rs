//! Authored water -> Runtime with the world renderer installed -> render world extract, no
//! CPU surface mesh (no double draw), feature registration and ordering.
use engine_project::world::water::{self, WaterRenderSet};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_render_pipeline::{PipelineFeatures, Stage};
use engine_world_render::names;
use serde_json::json;
use std::fs;

fn project(render_mode: &str) -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"camera","components":{"Transform":{"position":[0,10,20]},"Camera3D":{}}},
            {"id":"lake","name":"lake","components":{"Transform":{"position":[0,0,0]},
                "custom":{"engine.WaterBody":{"version":1,"data":{"kind":"lake","render_mode":render_mode}}}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"water-render","startup_scene":"scene.json","assets":[]})
            .to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    (dir, project)
}

fn surface_meshes(runtime: &Runtime) -> usize {
    runtime
        .extract_scene_frame()
        .unwrap()
        .meshes
        .iter()
        .filter(|m| m.id.starts_with("water:"))
        .count()
}

#[test]
fn installed_renderer_extracts_water_and_suppresses_the_cpu_mesh() {
    let (_dir, project) = project("auto");
    let mut runtime = Runtime::new(project).unwrap();
    engine_world_render::enable(runtime.app_mut()).unwrap();
    let features = runtime.app().world().resource::<PipelineFeatures>();
    let registered = features.0.names();
    assert!(registered.contains(&names::WATER));
    assert!(registered.contains(&names::WATER_UNDERWATER));
    let surface = features.0.descriptor(names::WATER).unwrap();
    assert_eq!(surface.stage, Stage::Transparent);
    runtime.start().unwrap();
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert_eq!(surface_meshes(&runtime), 0, "the GPU feature draws instead");
    let set = runtime
        .app()
        .render_world()
        .get_resource::<WaterRenderSet>()
        .expect("extracted");
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].id, "lake");
    let lake = runtime.entity("lake").unwrap();
    assert_eq!(
        water::status(runtime.app().world(), lake)
            .unwrap()
            .render_path,
        "gpu-feature"
    );
}

#[test]
fn cpu_render_mode_keeps_the_fallback_mesh() {
    let (_dir, project) = project("cpu");
    let mut runtime = Runtime::new(project).unwrap();
    engine_world_render::install(runtime.app_mut());
    runtime.start().unwrap();
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert_eq!(surface_meshes(&runtime), 1);
    let set = runtime
        .app()
        .render_world()
        .get_resource::<WaterRenderSet>()
        .expect("extracted");
    assert!(set.items.is_empty(), "the GPU feature skips cpu bodies");
}
