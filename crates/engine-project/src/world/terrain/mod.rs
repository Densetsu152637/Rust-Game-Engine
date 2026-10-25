//! World-building feature `terrain`: the `engine.Terrain` component over `terrain` assets
//! (`engine-terrain` heightfields), fixed physics collider tiles, `engine_pcg::Surface`
//! registration, CPU fallback chunk meshes with view-dependent LOD and the render set the GPU
//! feature (`engine-world-render`, `world.terrain`) draws. See docs/terrain.md.
//!
//! Render path rule: a terrain draws through the GPU feature when its `render_mode` is `gpu`,
//! or `auto` while the [`TerrainGpuRenderer`] marker resource exists (inserted by
//! `engine_world_render::install`); otherwise it spawns CPU chunk entities
//! (`MeshRenderer` + `GeometryOverride`). A terrain is never on both paths: GPU-path terrains
//! have no chunk entities and CPU-path terrains are not in [`TerrainRenderSet`].
use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

mod assets;
mod component;
mod navigation;
mod service;
mod surface;

pub use assets::{load_terrain, mapped};
pub use component::{
    COMPONENT_NAME, TerrainChunk, TerrainColliderTile, TerrainCollision, TerrainComponent,
    TerrainGenerated, TerrainMaterial, TerrainRenderMode, validate_scene,
};
pub use navigation::{navigation_document, navigation_input as navigation_input_for};
pub use service::{
    MAX_COLLIDER_TILES, MAX_TERRAINS, ResolvedLayer, TerrainGpuRenderer, TerrainRenderItem,
    TerrainRenderSet, TerrainStatus, Terrains, export_asset, height_at_world, heightfield,
    navigation_input, paint_holes, paint_layer, sculpt, status, sync_terrains,
    update_terrain_render, with_heightfield, world_to_local,
};
pub use surface::TerrainSurface;

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    vec![component::descriptor()]
}
/// Registers the typed ECS adapter for `engine.Terrain`.
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<TerrainComponent>(app, COMPONENT_NAME, 1)
}
/// Installs the terrain service and an empty render set.
pub fn install(world: &mut World) -> Result<()> {
    service::install(world);
    Ok(())
}
/// Loads terrains, rebuilds edited collider tiles and refreshes PCG surfaces before physics.
pub fn pre_simulation(world: &mut World, _dt: f32) -> Result<()> {
    sync_terrains(world)
}
/// Picks render paths and rebuilds CPU chunks for the active camera after transforms.
pub fn post_transforms(world: &mut World, _dt: f32) -> Result<()> {
    update_terrain_render(world)
}
