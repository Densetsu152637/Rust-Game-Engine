//! The native streaming API for game code: pin and force cells, toggle data layers, query cell
//! state and read the bounded status. Cell load and unload notifications are ordinary engine
//! events (`streaming.cell_loaded`, `streaming.cell_unloaded`, `streaming.cell_failed`).
use super::component::WorldPartition;
use super::partition::{StreamingRuntime, emit, force_load, with_runtime};
use super::status::{CellInfo, PartitionStatus, StreamingStatus};
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use engine_streaming::CellState;

fn unknown_partition(partition: &str) -> Error {
    Error::new(
        "streaming.partition.unknown",
        format!("no streaming partition `{partition}` (missing, or its manifest is not loadable)"),
    )
}

fn unknown_cell(key: [i32; 3], layer: &str) -> Error {
    Error::new(
        "streaming.cell.unknown",
        format!(
            "the manifest has no cell {},{},{} in layer `{layer}`",
            key[0], key[1], key[2]
        ),
    )
}

/// Enables or disables a data layer of `partition` (the partition entity's authored id).
///
/// This edits the entity's `engine.WorldPartition.layers`, so the change is visible to
/// `component_get` and persists in snapshots. Content of a disabled layer unloads on the next
/// tick; enabling streams it back in under the normal budgets. Unknown layer names are
/// accepted (no cell may use them yet).
pub fn set_layer_enabled(
    world: &mut World,
    partition: &str,
    layer: &str,
    enabled: bool,
) -> Result<()> {
    let entity = find_partition(world, partition)?;
    let mut component = world
        .get_mut::<WorldPartition>(entity)
        .ok_or_else(|| unknown_partition(partition))?;
    if layer.is_empty() || layer.len() > 64 || layer.chars().any(char::is_control) {
        return Err(Error::new(
            "streaming.layer.invalid",
            "layer names are 1-64 characters without control characters",
        ));
    }
    if component.layers.len() >= 64 && !component.layers.iter().any(|l| l.name == layer) {
        return Err(Error::new("streaming.layer.invalid", "at most 64 layers"));
    }
    component.set_layer(layer, enabled);
    Ok(())
}

/// Whether `layer` is enabled for `partition`.
pub fn layer_enabled(world: &mut World, partition: &str, layer: &str) -> Result<bool> {
    let entity = find_partition(world, partition)?;
    world
        .get::<WorldPartition>(entity)
        .map(|c| c.layer_enabled(layer))
        .ok_or_else(|| unknown_partition(partition))
}

fn find_partition(world: &mut World, partition: &str) -> Result<Entity> {
    crate::services::find_entity(world, partition).ok_or_else(|| unknown_partition(partition))
}

/// Keeps a cell loaded regardless of sources until [`release_cell`]: it is requested like a
/// source-driven cell (normal budgets and order, highest priority) and never evicted. The
/// cell's layer must also be enabled. `layer` is `""` for the default layer.
pub fn request_cell(world: &mut World, partition: &str, key: [i32; 3], layer: &str) -> Result<()> {
    pin(world, partition, key, layer, true)
}

/// Undoes [`request_cell`] / [`force_load_cell`]; the cell then follows the sources again.
pub fn release_cell(world: &mut World, partition: &str, key: [i32; 3], layer: &str) -> Result<()> {
    pin(world, partition, key, layer, false)
}

fn pin(world: &mut World, partition: &str, key: [i32; 3], layer: &str, pinned: bool) -> Result<()> {
    with_runtime(world, |_, rt| {
        let p = rt
            .partitions
            .get_mut(partition)
            .ok_or_else(|| unknown_partition(partition))?;
        let id = p
            .resolve(key, layer)
            .ok_or_else(|| unknown_cell(key, layer))?;
        p.pin(id, pinned)
    })
}

/// Loads a cell immediately (reading its file on the calling thread, ignoring the per-tick and
/// in-flight budgets) and pins it. The cell's `streaming.cell_loaded` event is delivered on the
/// next event dispatch. Fails if the layer is disabled or the cell cannot be read.
pub fn force_load_cell(
    world: &mut World,
    partition: &str,
    key: [i32; 3],
    layer: &str,
) -> Result<()> {
    let events = with_runtime(world, |world, rt| {
        let enabled = {
            let entity = rt
                .partitions
                .get(partition)
                .ok_or_else(|| unknown_partition(partition))?
                .entity;
            world
                .get::<WorldPartition>(entity)
                .is_some_and(|c| c.layer_enabled(layer))
        };
        if !enabled {
            return Err(Error::new(
                "streaming.layer.disabled",
                format!("layer `{layer}` is disabled"),
            ));
        }
        let p = rt
            .partitions
            .get_mut(partition)
            .ok_or_else(|| unknown_partition(partition))?;
        let id = p
            .resolve(key, layer)
            .ok_or_else(|| unknown_cell(key, layer))?;
        p.pin(id, true)?;
        force_load(world, p, id)
    })?;
    emit(world, events);
    Ok(())
}

/// The state of one cell, or `None` when the partition or the cell is unknown.
pub fn cell_state(world: &World, partition: &str, key: [i32; 3], layer: &str) -> Option<CellState> {
    world
        .get_resource::<StreamingRuntime>()?
        .partitions
        .get(partition)?
        .state(key, layer)
}

/// Every cell of a partition with its state, ordered by key then layer.
pub fn cell_infos(world: &World, partition: &str) -> Option<Vec<CellInfo>> {
    Some(
        world
            .get_resource::<StreamingRuntime>()?
            .partitions
            .get(partition)?
            .infos(),
    )
}

/// The telemetry of one partition as of the end of the last tick.
pub fn partition_status(world: &World, partition: &str) -> Option<PartitionStatus> {
    world
        .get_resource::<StreamingStatus>()?
        .0
        .get(partition)
        .cloned()
}

/// Streaming status for the runtime snapshot; `None` when no partition is active or broken.
pub fn snapshot(world: &World) -> Option<StreamingStatus> {
    let status = world.get_resource::<StreamingStatus>()?;
    (!status.0.is_empty()).then(|| status.clone())
}
