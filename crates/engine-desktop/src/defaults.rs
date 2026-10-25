//! Render features the desktop host provides by default: the world features
//! (`engine-world-render`: terrain, foliage, water) and the physics features
//! (`engine-physics-render`: debug lines, cloth, soft bodies, fragments, fluids).
//!
//! They draw nothing until matching components exist (terrains, foliage, water
//! bodies, physics debug output, cloth, ...), add no prepass or overlay, and can
//! be disabled per camera with `Camera3D.render_features`. Game-supplied
//! features win: a group of which the game registered any feature itself is left
//! alone, so the merge never produces duplicate names.

use anyhow::Result;
use engine_core::EngineApp;
use engine_physics_render::{PhysicsRenderConfig, PhysicsRenderStatusHandle};
use engine_render_pipeline::PipelineFeatures;

/// Status handles of the default features a call registered (`None` / empty
/// when the game or an earlier call registered the group).
#[derive(Clone, Debug, Default)]
pub struct DefaultRenderFeatures {
    pub world: engine_world_render::MissingStatusHandles,
    pub physics: Option<PhysicsRenderStatusHandle>,
}

impl DefaultRenderFeatures {
    /// Whether the call registered nothing.
    pub fn is_empty(&self) -> bool {
        self.world.terrain.is_none()
            && self.world.foliage.is_none()
            && self.world.water.is_none()
            && self.physics.is_none()
    }
}

/// Registers the default world and physics features missing from `features`
/// (descriptors only: no extraction, see [`install_default_render_features`]).
/// The physics features use `physics` (the installed [`PhysicsRenderConfig`]
/// when there is one). Idempotent.
pub fn register_default_render_features(
    features: &mut PipelineFeatures,
    physics: &PhysicsRenderConfig,
) -> Result<DefaultRenderFeatures> {
    let world = engine_world_render::register_missing(features)?;
    let present = features.0.names();
    let physics = if engine_physics_render::names::ALL
        .iter()
        .any(|name| present.contains(name))
    {
        None
    } else {
        Some(engine_physics_render::register_physics_features_with(
            features, physics,
        )?)
    };
    Ok(DefaultRenderFeatures { world, physics })
}

/// What a GPU host does before the runtime starts: installs the world and
/// physics render extraction (each only when the game has not, which also
/// switches `render_mode: auto` terrains and water to the GPU features and
/// hides the intact meshes of fractured bodies behind their fragments) and
/// merges the default features into the world's [`PipelineFeatures`]
/// resource. Idempotent.
pub fn install_default_render_features(app: &mut EngineApp) -> Result<DefaultRenderFeatures> {
    engine_world_render::install_missing(app);
    let config = match app.world().get_resource::<PhysicsRenderConfig>() {
        Some(config) => config.clone(),
        None => {
            let config = PhysicsRenderConfig::default();
            engine_physics_render::install_physics_extraction(app, config.clone())?;
            config
        }
    };
    merge_into_world(app, &config)
}

/// Headless runs never render: the default features join the world's
/// [`PipelineFeatures`] so the preview status lists what a GPU host would run,
/// but nothing is installed (CPU fallbacks keep drawing into `RenderFrame`).
pub(crate) fn register_headless_defaults(app: &mut EngineApp) -> Result<DefaultRenderFeatures> {
    let config = app
        .world()
        .get_resource::<PhysicsRenderConfig>()
        .cloned()
        .unwrap_or_default();
    merge_into_world(app, &config)
}

fn merge_into_world(
    app: &mut EngineApp,
    physics: &PhysicsRenderConfig,
) -> Result<DefaultRenderFeatures> {
    let world = app.world_mut();
    let mut features = world
        .remove_resource::<PipelineFeatures>()
        .unwrap_or_default();
    let result = register_default_render_features(&mut features, physics);
    world.insert_resource(features);
    result
}
