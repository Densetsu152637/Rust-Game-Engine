//! Level streaming / world partition: the `engine.WorldPartition` component, streaming
//! sources, data layers, asynchronous cell loading and HLOD proxies.
//!
//! A partition names a `partition` manifest asset (see `engine-streaming`) that lists cells; each
//! cell's content is an ordinary scene document. Every tick, in [`pre_simulation`], the cell
//! scheduler decides which cells to load or unload for the current sources (cameras and
//! `engine.StreamingSource` entities), cell files are read on background threads and finished
//! cells are spawned on the main thread within the per-tick budget. Unloading despawns exactly
//! the entities the cell spawned. Positions are handled in f64 absolute space via
//! [`WorldOrigin`](super::coordinates::WorldOrigin), so cells may be any distance from the
//! origin; rebasing the origin keeps streamed content in place because the coordinates service
//! shifts cell roots and proxies with every other root entity.
//!
//! See `docs/streaming.md`.
mod api;
mod component;
mod partition;
mod status;

use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

pub use api::{
    cell_infos, cell_state, force_load_cell, layer_enabled, partition_status, release_cell,
    request_cell, set_layer_enabled, snapshot,
};
pub use component::{
    DataLayer, DataLayerSetting, LAYER_NAME, MANIFEST_KIND, PARTITION_NAME, SOURCE_NAME,
    StreamingSource, WorldPartition, load_manifest, validate_scene,
};
pub use engine_streaming::{CellKey, CellState, PartitionManifest};
pub use partition::{
    EVENT_CELL_FAILED, EVENT_CELL_LOADED, EVENT_CELL_UNLOADED, StreamedCell, StreamingRuntime,
};
pub use status::{
    CellInfo, LayerStatus, MAX_DIAGNOSTICS, MAX_LISTED_CELLS, MAX_PARTITIONS, PartitionStatus,
    StreamingStatus,
};

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    component::descriptors()
}
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<WorldPartition>(app, PARTITION_NAME, 1)?;
    crate::register_component::<StreamingSource>(app, SOURCE_NAME, 1)?;
    crate::register_component::<DataLayer>(app, LAYER_NAME, 1)
}
pub fn install(world: &mut World) -> Result<()> {
    world.init_resource::<StreamingRuntime>();
    world.init_resource::<StreamingStatus>();
    Ok(())
}
pub fn pre_simulation(world: &mut World, _dt: f32) -> Result<()> {
    partition::run(world);
    Ok(())
}
pub fn post_transforms(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
