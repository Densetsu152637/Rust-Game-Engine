//! The desktop host's default render features: the world (terrain, foliage, water) and physics
//! features are registered by default, merge with game-supplied features without duplicate
//! names, are listed by headless runs, and a default host renders an authored terrain. GPU
//! tests are ignored by default; run with `--ignored --test-threads=1`.

use engine_desktop::*;
use engine_physics_render::PhysicsRenderConfig;
use engine_project::rendering3d::{RenderPipelineState, RenderPipelineStatus};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_terrain::{Heightfield, asset};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;

fn names(features: &PipelineFeatures) -> Vec<String> {
    features.0.names().into_iter().map(String::from).collect()
}

fn assert_unique(names: &[String]) {
    let unique: BTreeSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "duplicate names in {names:?}");
}

#[test]
fn default_registration_lists_the_world_and_physics_features() {
    let mut features = PipelineFeatures::default();
    let added =
        register_default_render_features(&mut features, &PhysicsRenderConfig::default()).unwrap();
    let registered = names(&features);
    for name in engine_world_render::names::ALL
        .into_iter()
        .chain(engine_physics_render::names::ALL)
    {
        assert!(registered.iter().any(|n| n == name), "{name} registered");
    }
    assert_eq!(registered.len(), 7 + 5);
    assert!(added.world.terrain.is_some() && added.physics.is_some());
    // A second call adds nothing and keeps the names unique.
    let again =
        register_default_render_features(&mut features, &PhysicsRenderConfig::default()).unwrap();
    assert!(again.is_empty());
    assert_eq!(names(&features), registered);
}

#[test]
fn game_supplied_features_win_without_duplicate_names() {
    // The game registered the world features itself (and a physics feature of its own set).
    let mut features = PipelineFeatures::default();
    engine_world_render::register(&mut features).unwrap();
    engine_physics_render::register_physics_features(&mut features).unwrap();
    let before = names(&features);
    let added =
        register_default_render_features(&mut features, &PhysicsRenderConfig::default()).unwrap();
    assert!(added.is_empty(), "nothing registered twice");
    assert_eq!(names(&features), before);

    // Only the terrain module from the game: foliage, water and physics are added.
    let mut features = PipelineFeatures::default();
    let status = engine_world_render::TerrainRenderStatusHandle::default();
    engine_world_render::TerrainFeature::all(status)
        .register(&mut features)
        .unwrap();
    let added =
        register_default_render_features(&mut features, &PhysicsRenderConfig::default()).unwrap();
    assert!(added.world.terrain.is_none());
    assert!(added.world.foliage.is_some() && added.world.water.is_some());
    assert!(added.physics.is_some());
    let merged = names(&features);
    assert_unique(&merged);
    assert_eq!(merged.len(), 12);
}

/// A project with a 65 x 65 terrain (a ridge along Z), a camera looking at it and a sun.
fn terrain_project(camera_features: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let heights = (0..65 * 65)
        .map(|i| (8.0 - 0.25 * ((i % 65) as f32 - 32.0).abs()).max(0.0))
        .collect();
    let field = Heightfield::from_heights(65, 65, 1.0, heights).unwrap();
    fs::write(dir.path().join("land.terrain"), asset::encode(&field)).unwrap();
    let camera_rotation = glam::Quat::from_rotation_x(-0.45).to_array();
    let sun_rotation =
        (glam::Quat::from_rotation_y(-0.6) * glam::Quat::from_rotation_x(-0.9)).to_array();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"camera","components":{
                "Transform":{"position":[32,24,72],"rotation":camera_rotation},
                "Camera3D":{"render_features":camera_features}}},
            {"id":"sun","name":"sun","components":{
                "Transform":{"position":[0,0,0],"rotation":sun_rotation},
                "DirectionalLight":{"lux":3,"shadows":{"enabled":true}}}},
            {"id":"land","name":"land","components":{"Transform":{"position":[0,0,0]},
                "custom":{"engine.Terrain":{"version":1,"data":{"asset":"land",
                    "lod":{"node_resolution":16}}}}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"default-terrain","startup_scene":"scene.json","assets":[
            {"id":"land","kind":"terrain","path":"land.terrain"}]})
        .to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn headless_runs_list_the_default_features_without_installing_them() {
    let dir = terrain_project(json!({}));
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let report = run(
        runtime,
        RunOptions {
            headless: true,
            ticks: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let status = report.scene.render_pipeline.expect("a 3D camera");
    let listed: Vec<&str> = status.features.iter().map(|f| f.name.as_str()).collect();
    for name in [
        engine_world_render::names::TERRAIN,
        engine_world_render::names::TERRAIN_SHADOW,
        engine_world_render::names::WATER,
        engine_physics_render::names::DEBUG_LINES,
        engine_physics_render::names::FLUID,
    ] {
        assert!(listed.contains(&name), "{name} in {listed:?}");
    }
    assert_eq!(status.state, RenderPipelineState::NotRendered);
}

#[test]
fn installing_the_defaults_moves_auto_terrain_to_the_gpu_feature() {
    let dir = terrain_project(json!({}));
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let added = install_default_render_features(runtime.app_mut()).unwrap();
    assert!(added.world.terrain.is_some() && added.physics.is_some());
    // Idempotent: a second install (for example a game that also calls it) adds nothing.
    assert!(
        install_default_render_features(runtime.app_mut())
            .unwrap()
            .is_empty()
    );
    runtime.start().unwrap();
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(
        !frame
            .meshes
            .iter()
            .any(|m| m.id.starts_with("terrain:land/chunk/")),
        "no CPU chunks: the GPU feature draws the terrain"
    );
}

/// Renders the terrain project through a default host: defaults installed before start, the
/// world's `PipelineFeatures` handed to the renderer, the render world supplied (what the
/// windowed runner and the screenshot path do).
fn render_default_host(
    camera_features: Value,
) -> anyhow::Result<(Vec<u8>, RenderPipelineStatus, DefaultRenderFeatures)> {
    let dir = terrain_project(camera_features);
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    let added = install_default_render_features(runtime.app_mut())?;
    runtime.start()?;
    runtime.tick(0.02, InputSnapshot::default())?;
    let frame = runtime.extract_scene_frame()?;
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, 160, 120)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let features = runtime
        .app_mut()
        .world_mut()
        .remove_resource::<PipelineFeatures>()
        .unwrap_or_default();
    renderer.set_features(features)?;
    renderer.render_scene(&frame, &target.view, Some(runtime.app().render_world()))?;
    let pixels = target.read_rgba(&renderer.gpu)?;
    if let Ok(path) = std::env::var("DEFAULT_TERRAIN_DUMP") {
        image::save_buffer(path, &pixels, 160, 120, image::ColorType::Rgba8)?;
    }
    let status = renderer.pipeline_status().clone();
    runtime.stop()?;
    Ok((pixels, status, added))
}

fn lit_fraction(pixels: &[u8], rows: std::ops::Range<usize>) -> f32 {
    let row_bytes = 160 * 4;
    let region = &pixels[rows.start * row_bytes..rows.end * row_bytes];
    let lit = region
        .chunks(4)
        .filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8)
        .count();
    lit as f32 / (region.len() / 4) as f32
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_default_host_renders_terrain_and_cameras_can_disable_it() -> anyhow::Result<()> {
    let (pixels, status, added) = render_default_host(json!({}))?;
    let feature = |name: &str| {
        status
            .features
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} in {:?}", status.features))
            .clone()
    };
    let terrain = feature(engine_world_render::names::TERRAIN);
    assert!(terrain.enabled && terrain.activity == "ran", "{terrain:?}");
    let shadow = feature(engine_world_render::names::TERRAIN_SHADOW);
    assert!(shadow.enabled && shadow.activity == "ran", "{shadow:?}");
    // The physics features are registered and inert (nothing to draw in this scene).
    for name in engine_physics_render::names::ALL {
        assert!(feature(name).enabled, "{name} enabled");
    }
    let terrain_status = added.world.terrain.expect("registered by default").get();
    eprintln!("terrain status {terrain_status:?}, draws {}", status.draws);
    assert_eq!(terrain_status.terrains, 1);
    assert_eq!(terrain_status.draw_calls, 1);
    assert!(terrain_status.nodes > 0 && terrain_status.shadow_pass);
    assert_eq!(status.draws, 0, "no engine meshes: no CPU chunk fallback");
    let covered = lit_fraction(&pixels, 0..120);
    let above = lit_fraction(&pixels, 0..10);
    eprintln!("lit fraction: image {covered:.3}, top rows {above:.3}");
    assert!(covered > 0.15, "terrain covers the image: {covered}");
    assert!(above < 0.01, "sky above the terrain: {above}");

    // Camera3D.render_features disables the default feature for the camera.
    let (pixels, status, _) = render_default_host(json!({"world.terrain": false}))?;
    let terrain = status
        .features
        .iter()
        .find(|f| f.name == engine_world_render::names::TERRAIN)
        .unwrap();
    assert!(!terrain.enabled && terrain.activity == "disabled");
    let covered = lit_fraction(&pixels, 0..120);
    eprintln!("with world.terrain disabled: image {covered:.3}");
    assert!(covered < 0.001, "no terrain drawn: {covered}");
    Ok(())
}
