//! Render features for world building, one module per feature, drawn through the render
//! extension API (`engine-render-pipeline`): [`terrain`] (`world.terrain`, docs/terrain.md),
//! [`foliage`] (`world.foliage`, docs/world-foliage.md) and [`water`] (`world.water`,
//! `world.water.underwater`, docs/world-water.md).
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
pub mod water;

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
    /// Terrain depth into the view's terrain shadow map (`Shadow` stage, after
    /// `engine.shadows`), one layer per cascade of the first directional light.
    pub const TERRAIN_SHADOW: &str = "world.terrain.shadow";
    /// Terrain motion vectors (`Opaque` stage, after `world.terrain`); runs only when a
    /// feature or the host needs motion.
    pub const TERRAIN_MOTION: &str = "world.terrain.motion";
    /// Instanced foliage meshes and impostors with wind (`Opaque` stage, after `engine.opaque`).
    pub const FOLIAGE: &str = "world.foliage";
    /// Water surfaces (`Transparent` stage, before `engine.transparent`).
    pub const WATER: &str = "world.water";
    /// Underwater fog (`Transparent` stage, after `engine.transparent` and `world.water`).
    pub const WATER_UNDERWATER: &str = "world.water.underwater";
    /// The terrain module's features, in registration order.
    pub const TERRAIN_ALL: [&str; 4] = [TERRAIN_SHADOW, TERRAIN_NORMALS, TERRAIN, TERRAIN_MOTION];
    /// The water module's features, in registration order.
    pub const WATER_ALL: [&str; 2] = [WATER, WATER_UNDERWATER];
    /// Every feature [`crate::register`] registers.
    pub const ALL: [&str; 7] = [
        TERRAIN_SHADOW,
        TERRAIN_NORMALS,
        TERRAIN,
        TERRAIN_MOTION,
        FOLIAGE,
        WATER,
        WATER_UNDERWATER,
    ];
}

pub use terrain::{
    TerrainFeature, TerrainFeatures, TerrainInstance, TerrainMotionFeature, TerrainNormalsFeature,
    TerrainRenderStatus, TerrainRenderStatusHandle, TerrainShadowFeature, extract_terrain,
};

/// Registers every world feature (`world.terrain.shadow`, `world.terrain.normals`,
/// `world.terrain`, `world.terrain.motion`, `world.foliage`, `world.water`,
/// `world.water.underwater`); returns the terrain status handle. Use
/// [`register_all`] for every feature's handle.
pub fn register(features: &mut PipelineFeatures) -> Result<TerrainRenderStatusHandle> {
    foliage::register(features)?;
    water::register(features)?;
    terrain::register(features)
}

/// [`register`] returning the status handle of every feature.
pub fn register_all(features: &mut PipelineFeatures) -> Result<RenderStatusHandles> {
    let foliage = foliage::register(features)?;
    let water = water::register(features)?;
    let terrain = terrain::register(features)?;
    Ok(RenderStatusHandles {
        terrain,
        foliage,
        water,
    })
}

/// Status handles of the features [`register_all`] registered.
#[derive(Clone, Debug)]
pub struct RenderStatusHandles {
    pub terrain: TerrainRenderStatusHandle,
    pub foliage: foliage::FoliageRenderStatusHandle,
    pub water: water::WaterRenderStatusHandle,
}

/// Adds the `Extract`-stage system copying `engine_project::world::terrain::TerrainRenderSet`
/// into the render world, and inserts the `TerrainGpuRenderer` marker so `auto` terrains use
/// the GPU path. Call once while configuring the app.
pub fn install(app: &mut EngineApp) {
    terrain::install(app);
    foliage::install(app);
    water::install(app);
}

/// [`register`] into the world's `PipelineFeatures` resource (created when missing), then
/// [`install`]. Use it when the game does not register other features itself. The GPU markers
/// are inserted only after registration succeeded, so a failed registration leaves the CPU
/// fallbacks drawing instead of silently turning them off.
pub fn enable(app: &mut EngineApp) -> Result<TerrainRenderStatusHandle> {
    let world = app.world_mut();
    let mut features = world
        .remove_resource::<PipelineFeatures>()
        .unwrap_or_default();
    let status = register(&mut features);
    world.insert_resource(features);
    let status = status?;
    install(app);
    Ok(status)
}

/// Status handles of the modules [`register_missing`] registered (`None`: the module was
/// already registered, by the game or an earlier call).
#[derive(Clone, Debug, Default)]
pub struct MissingStatusHandles {
    pub terrain: Option<TerrainRenderStatusHandle>,
    pub foliage: Option<foliage::FoliageRenderStatusHandle>,
    pub water: Option<water::WaterRenderStatusHandle>,
}

/// Host default: registers each world module (terrain, foliage, water) none of whose feature
/// names `features` already contains. A module of which any feature is present (the game
/// registered it itself, perhaps differently) is left alone, so this merges with
/// game-supplied features without duplicate names and is idempotent.
pub fn register_missing(features: &mut PipelineFeatures) -> Result<MissingStatusHandles> {
    let absent = |features: &PipelineFeatures, group: &[&str]| {
        let present = features.0.names();
        !group.iter().any(|name| present.contains(name))
    };
    let mut handles = MissingStatusHandles::default();
    if absent(features, &names::TERRAIN_ALL) {
        handles.terrain = Some(terrain::register(features)?);
    }
    if absent(features, &[names::FOLIAGE]) {
        handles.foliage = Some(foliage::register(features)?);
    }
    if absent(features, &names::WATER_ALL) {
        handles.water = Some(water::register(features)?);
    }
    Ok(handles)
}

/// Host default: [`install`] for each module whose GPU renderer marker
/// (`TerrainGpuRenderer`, `FoliageGpuRenderer`, `WaterGpuRenderer`) is missing, so a game's
/// own `install` is not repeated. Idempotent.
pub fn install_missing(app: &mut EngineApp) {
    use engine_project::world::{foliage::FoliageGpuRenderer, water::WaterGpuRenderer};
    if !installed(app.world()) {
        terrain::install(app);
    }
    if !app.world().contains_resource::<FoliageGpuRenderer>() {
        foliage::install(app);
    }
    if !app.world().contains_resource::<WaterGpuRenderer>() {
        water::install(app);
    }
}

/// Whether `world` has the terrain GPU renderer marker.
pub fn installed(world: &World) -> bool {
    world.contains_resource::<engine_project::world::terrain::TerrainGpuRenderer>()
}
