//! Authored terrain -> Runtime with the terrain renderer installed -> render world extract,
//! no CPU chunk entities (no double draw), feature registration.
use engine_project::world::terrain::{self, TerrainRenderSet};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_render_pipeline::PipelineFeatures;
use engine_terrain::{Heightfield, asset};
use engine_world_render::names;
use serde_json::json;
use std::fs;

fn project(render_mode: &str) -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("land.terrain"),
        asset::encode(&Heightfield::new(65, 65, 1.0).unwrap()),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"camera","components":{"Transform":{"position":[32,10,32]},"Camera3D":{}}},
            {"id":"land","name":"land","components":{"Transform":{"position":[0,0,0]},
                "custom":{"engine.Terrain":{"version":1,"data":{"asset":"land","render_mode":render_mode,
                    "lod":{"node_resolution":16}}}}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"terrain-render","startup_scene":"scene.json","assets":[
            {"id":"land","kind":"terrain","path":"land.terrain"}]})
        .to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    (dir, project)
}

fn chunk_meshes(runtime: &Runtime) -> usize {
    runtime
        .extract_scene_frame()
        .unwrap()
        .meshes
        .iter()
        .filter(|m| m.id.starts_with("terrain:land/chunk/"))
        .count()
}

#[test]
fn installed_renderer_extracts_terrains_and_suppresses_cpu_chunks() {
    let (_dir, project) = project("auto");
    let mut runtime = Runtime::new(project).unwrap();
    let status = engine_world_render::enable(runtime.app_mut()).unwrap();
    assert!(engine_world_render::installed(runtime.app().world()));
    let features = runtime.app().world().resource::<PipelineFeatures>();
    let registered = features.0.names();
    assert!(registered.contains(&names::TERRAIN));
    assert!(registered.contains(&names::TERRAIN_NORMALS));
    runtime.start().unwrap();
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert_eq!(chunk_meshes(&runtime), 0, "the GPU feature draws instead");
    let set = runtime
        .app()
        .render_world()
        .get_resource::<TerrainRenderSet>()
        .expect("extracted");
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].field.columns(), 65);
    let land = runtime.entity("land").unwrap();
    assert_eq!(
        terrain::status(runtime.app().world(), land)
            .unwrap()
            .render_path,
        "gpu-feature"
    );
    // Nothing rendered headlessly.
    assert_eq!(status.get().views, 0);
    // A second registration is rejected (stable unique names).
    let mut more = PipelineFeatures::default();
    engine_world_render::register(&mut more).unwrap();
    assert!(engine_world_render::register(&mut more).is_err());
}

#[test]
fn cpu_render_mode_keeps_the_fallback_with_the_renderer_installed() {
    let (_dir, project) = project("cpu");
    let mut runtime = Runtime::new(project).unwrap();
    engine_world_render::install(runtime.app_mut());
    runtime.start().unwrap();
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert!(chunk_meshes(&runtime) > 0);
    let set = runtime
        .app()
        .render_world()
        .get_resource::<TerrainRenderSet>()
        .expect("extracted");
    assert!(set.items.is_empty(), "the GPU feature skips cpu terrains");
}

#[test]
fn a_failed_registration_leaves_no_gpu_markers_and_the_cpu_fallback_drawing() {
    let (_dir, project) = project("auto");
    let mut runtime = Runtime::new(project).unwrap();
    // The game already registered the world features (and not the renderer): `enable`
    // cannot register them again.
    let mut features = PipelineFeatures::default();
    engine_world_render::register(&mut features).unwrap();
    runtime.app_mut().world_mut().insert_resource(features);
    assert!(engine_world_render::enable(runtime.app_mut()).is_err());
    let world = runtime.app().world();
    assert!(!engine_world_render::installed(world));
    assert!(!world.contains_resource::<engine_project::world::foliage::FoliageGpuRenderer>());
    assert!(!world.contains_resource::<engine_project::world::water::WaterGpuRenderer>());
    runtime.start().unwrap();
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert!(
        chunk_meshes(&runtime) > 0,
        "auto terrain falls back to CPU chunks"
    );
}
