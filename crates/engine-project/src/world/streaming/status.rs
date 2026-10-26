//! Bounded telemetry of the streaming systems.
use bevy_ecs::prelude::*;
use serde::Serialize;
use std::collections::BTreeMap;

/// Most partitions that stream at once in one world.
pub const MAX_PARTITIONS: usize = 16;
/// Longest list of loaded cell names in a status.
pub const MAX_LISTED_CELLS: usize = 64;
/// Most diagnostics kept per partition (newest last).
pub const MAX_DIAGNOSTICS: usize = 16;
/// Longest diagnostic message in characters.
pub const MAX_DIAGNOSTIC_CHARS: usize = 240;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LayerStatus {
    pub name: String,
    pub enabled: bool,
}

/// What one partition did, as of the end of the last `pre_simulation`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PartitionStatus {
    pub manifest: String,
    pub cells: usize,
    pub unloaded: usize,
    pub loading: usize,
    pub loaded: usize,
    pub unloading: usize,
    pub pinned: usize,
    /// Background reads that have not been applied yet (running or waiting for the budget).
    pub in_flight: usize,
    pub loaded_entities: usize,
    /// Visible HLOD proxies.
    pub proxies: usize,
    pub sources: usize,
    /// Cells that started loading in the last tick.
    pub started_last_tick: usize,
    /// Cells applied to the world in the last tick, and their cell-file bytes.
    pub applied_last_tick: usize,
    pub bytes_last_tick: u64,
    pub total_loaded: u64,
    pub total_unloaded: u64,
    pub total_failed: u64,
    /// Proxy mesh files read from disk so far (cache hits do not count).
    pub proxy_mesh_reads: u64,
    pub layers: Vec<LayerStatus>,
    /// Names (`x_y_z` or `x_y_z@layer`) of loaded cells, at most [`MAX_LISTED_CELLS`].
    pub loaded_cells: Vec<String>,
    pub diagnostics: Vec<String>,
}

/// Status of every partition keyed by the partition entity's authored id. A world-global
/// resource, always present; empty without partitions.
#[derive(Resource, Clone, Debug, Default, PartialEq, Serialize)]
pub struct StreamingStatus(pub BTreeMap<String, PartitionStatus>);

/// State of one cell, for the native query API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellInfo {
    pub key: [i32; 3],
    pub layer: String,
    pub state: engine_streaming::CellState,
    pub pinned: bool,
    /// Entities the cell currently owns in the world.
    pub entities: usize,
    pub proxy_visible: bool,
}

pub(super) fn clip(message: String) -> String {
    if message.chars().count() <= MAX_DIAGNOSTIC_CHARS {
        message
    } else {
        let mut cut: String = message.chars().take(MAX_DIAGNOSTIC_CHARS - 1).collect();
        cut.push('…');
        cut
    }
}
