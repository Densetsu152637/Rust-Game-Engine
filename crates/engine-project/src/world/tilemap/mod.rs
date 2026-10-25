//! World-building feature `tilemap`: the `engine.Tilemap` component, per-chunk render entities
//! drawn through the 2D sprite path, generated 2D colliders and the native painting API.
//! See `docs/world-tilemap.md`. The data model lives in the `engine-tilemap` crate.
use crate::Result;
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

mod assets;
mod component;
mod service;

pub use assets::{load_tilemap, load_tileset, texture_size};
pub use component::{
    TileChunkRender, TilemapCollision, TilemapComponent, TilemapGenerated, component_descriptors,
};
pub use service::{
    LoadedTileset, MAX_COLLIDERS, TileHit, TilemapStatus, Tilemaps, cell_to_world, erase,
    export_document, failure, fill_rect, flood_fill, paint, regenerate_collision, status,
    store_inline, sync_tilemaps, tile_at_world, with_tilemap, world_to_cell,
};

/// Registers the typed ECS adapter for `engine.Tilemap`.
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<TilemapComponent>(app, "engine.Tilemap", 1)
}
/// Installs the runtime tilemap service.
pub fn install(world: &mut World) -> Result<()> {
    service::install(world);
    Ok(())
}
/// Builds render chunks and colliders before the simulation step so scripts' edits of this
/// tick are visible to physics and to the next render extraction.
pub fn pre_simulation(world: &mut World, _dt: f32) -> Result<()> {
    sync_tilemaps(world)
}
/// Nothing to do after transforms: generated entities are children of the tilemap entity.
pub fn post_transforms(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
