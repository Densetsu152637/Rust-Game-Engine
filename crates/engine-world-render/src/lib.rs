//! Render features for world building, one module per feature, drawn through the render
//! extension API (`engine-render-pipeline`). Today: [`terrain`] (`world.terrain`) and
//! [`foliage`] (`world.foliage`); `world.water` will live here too. See docs/terrain.md and
//! docs/world-foliage.md.
//!
//! A host enables the terrain feature in two parts, both before the runner starts:
//!
//! ```ignore
//! // 1. Simulation side: extract terrain render data into the render world and mark the GPU
//! //    renderer as present, so `render_mode: auto` terrains stop spawning CPU chunk meshes.
//! engine_world_render::install(runtime.app_mut());
//! // 2. Renderer side: register the pipeline features.
//! let mut features = PipelineFeatures::default();
//! let status = engine_world_render::register(&mut features)?;
//! runtime.app_mut().world_mut().insert_resource(features);
//! ```
//!
//! [`enable`] does both and merges into an existing `PipelineFeatures` resource.
pub mod foliage;
pub mod terrain;

use anyhow::Result;
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use engine_render_pipeline::PipelineFeatures;

/// Feature names registered by this crate.
pub mod names {
    /// The terrain colour pass (`Opaque` stage, after `engine.opaque`).
    pub const TERRAIN: &str = "world.terrain";
    /// Terrain normals into the normals target (`Prepass` stage, after
    /// `engine.prepass.normals`); runs only when another feature needs normals.
    pub const TERRAIN_NORMALS: &str = "world.terrain.normals";
    /// Instanced foliage meshes and impostors with wind (`Opaque` stage, after `engine.opaque`).
    pub const FOLIAGE: &str = "world.foliage";
}

pub use terrain::{
    TerrainFeature, TerrainInstance, TerrainNormalsFeature, TerrainRenderStatus,
    TerrainRenderStatusHandle, extract_terrain,
};

/// Registers `world.terrain` and `world.terrain.normals`; returns a handle that reports what
/// the features drew in the last view.
pub fn register(features: &mut PipelineFeatures) -> Result<TerrainRenderStatusHandle> {
    foliage::register(features)?;
    terrain::register(features)
}

/// [`register`] returning the status handle of every feature.
pub fn register_all(features: &mut PipelineFeatures) -> Result<RenderStatusHandles> {
    let foliage = foliage::register(features)?;
    let terrain = terrain::register(features)?;
    Ok(RenderStatusHandles { terrain, foliage })
}

/// Status handles of the features [`register_all`] registered.
#[derive(Clone, Debug)]
pub struct RenderStatusHandles {
    pub terrain: TerrainRenderStatusHandle,
    pub foliage: foliage::FoliageRenderStatusHandle,
}

/// Adds the `Extract`-stage system copying `engine_project::world::terrain::TerrainRenderSet`
/// into the render world, and inserts the `TerrainGpuRenderer` marker so `auto` terrains use
/// the GPU path. Call once while configuring the app.
pub fn install(app: &mut EngineApp) {
    terrain::install(app);
    foliage::install(app);
}

/// [`install`] plus [`register`] into the world's `PipelineFeatures` resource (created when
/// missing). Use it when the game does not register other features itself.
pub fn enable(app: &mut EngineApp) -> Result<TerrainRenderStatusHandle> {
    install(app);
    let world = app.world_mut();
    let mut features = world
        .remove_resource::<PipelineFeatures>()
        .unwrap_or_default();
    let status = register(&mut features);
    world.insert_resource(features);
    status
}

/// Whether `world` has the terrain GPU renderer marker.
pub fn installed(world: &World) -> bool {
    world.contains_resource::<engine_project::world::terrain::TerrainGpuRenderer>()
}
