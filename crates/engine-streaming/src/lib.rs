//! World-partition streaming core: a cell grid in absolute f64 coordinates, streaming sources
//! with load/unload hysteresis, a priority and budget scheduler, data layers and the partition
//! manifest format. Pure data and logic: no ECS, no I/O, no threads. See the crate README.
mod budget;
mod grid;
mod manifest;
mod scheduler;

pub use budget::TickBudget;
pub use grid::{CellKey, Grid, GridError, GridPlane, MAX_CELL_SIZE};
pub use manifest::{
    HlodProxy, MANIFEST_SCHEMA, MAX_CELLS, ManifestCell, ManifestError, PartitionManifest,
};
pub use scheduler::{
    CellId, CellState, Counts, Layer, Limits, MAX_AUTO_RETRIES, Plan, Rank, Source, StreamError,
    Streamer,
};

#[cfg(test)]
mod tests;
