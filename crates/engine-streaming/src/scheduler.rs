//! The deterministic cell state machine and its priority/budget scheduler.
//!
//! Cells move `Unloaded -> Loading -> Loaded -> Unloading -> Unloaded`. [`Streamer::plan`]
//! decides, once per tick, which cells start loading and which start unloading; the host does
//! the actual work and reports back with `complete_load`, `fail_load` and `complete_unload`.
//!
//! Rules (all deterministic; ties break on [`CellId`] order):
//! * A cell is *wanted* when its layer is enabled and it is within the **load radius** of any
//!   source (or pinned). A loaded cell is *retained* while its layer is enabled and it is within
//!   the **unload radius** of any source (or pinned). Because `unload >= load` the band between
//!   the two radii is hysteresis: nothing there loads or unloads.
//! * A cell's *rank* is the best `(priority descending, distance ascending)` over the sources
//!   whose unload radius reaches it. Wanted cells start loading in rank order.
//! * Starting loads is limited per tick by a [`TickBudget`] (cells and bytes) and by
//!   `max_in_flight` (cells in `Loading`).
//! * `max_loaded` bounds cells that are `Loading`, `Loaded` or `Unloading`. When full, the
//!   worst-ranked unpinned `Loaded` cell is evicted for a strictly better candidate, so the
//!   farthest (lowest priority first) cells go first. Cells starting to unload in this plan
//!   count as freed: the host must finish them in the same tick.
use crate::budget::TickBudget;
use crate::grid::{CellKey, Grid};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

/// A load fails this many times before it stops being retried automatically.
pub const MAX_AUTO_RETRIES: u32 = 3;
const MAX_BACKOFF_TICKS: u64 = 32;

/// Interned data layer handle. [`Layer::DEFAULT`] is the unnamed layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Layer(pub u16);
impl Layer {
    pub const DEFAULT: Layer = Layer(0);
}

/// One cell: a position in the grid plus the data layer holding its content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId {
    pub key: CellKey,
    pub layer: Layer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellState {
    Unloaded,
    Loading,
    Loaded,
    Unloading,
}

/// A point that pulls cells in: a camera or any other streaming source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    /// Absolute position in metres.
    pub position: [f64; 3],
    /// Higher priority sources are served first.
    pub priority: i32,
    pub load_radius: f64,
    /// Raised to `load_radius` when smaller.
    pub unload_radius: f64,
}

/// Scheduler limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most cells that may start loading (or be applied) per tick, at least 1.
    pub cells_per_tick: usize,
    /// Byte budget per tick; `u64::MAX` is unlimited.
    pub bytes_per_tick: u64,
    /// Most cells in `Loading` at once, at least 1.
    pub max_in_flight: usize,
    /// Memory budget in cells (`Loading + Loaded + Unloading`), at least 1.
    pub max_loaded: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cells_per_tick: 4,
            bytes_per_tick: u64::MAX,
            max_in_flight: 4,
            max_loaded: 64,
        }
    }
}

/// Position of a cell in the loading order; smaller is more important.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rank {
    neg_priority: i64,
    distance: f64,
}
impl Rank {
    const NONE: Rank = Rank {
        neg_priority: i64::MAX,
        distance: f64::INFINITY,
    };
    const PINNED: Rank = Rank {
        neg_priority: i64::MIN,
        distance: 0.0,
    };
    /// Distance to the deciding source in metres (infinite when no source reaches the cell).
    pub fn distance(&self) -> f64 {
        self.distance
    }
    /// Priority of the deciding source (`i64::MAX` for pinned cells).
    pub fn priority(&self) -> i64 {
        self.neg_priority.saturating_neg()
    }
}
impl Eq for Rank {}
impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Rank {
    fn cmp(&self, other: &Self) -> Ordering {
        self.neg_priority
            .cmp(&other.neg_priority)
            .then(self.distance.total_cmp(&other.distance))
    }
}

/// What one [`Streamer::plan`] decided. The state changes are already applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// `Unloaded -> Loading`, in the order they should be served.
    pub start_loads: Vec<CellId>,
    /// `Loading -> Unloaded`: the cell is no longer wanted; discard the result of its load.
    pub cancel_loads: Vec<CellId>,
    /// `Loaded -> Unloading`: despawn the content and call `complete_unload`.
    pub start_unloads: Vec<CellId>,
    /// Bytes admitted by `start_loads`.
    pub bytes: u64,
}

/// Number of cells per state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub unloaded: usize,
    pub loading: usize,
    pub loaded: usize,
    pub unloading: usize,
    pub pinned: usize,
}

/// A misuse of the state machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamError(pub String);
impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for StreamError {}

#[derive(Clone, Debug)]
struct Entry {
    bytes: u64,
    state: CellState,
    failures: u32,
    retry_at: u64,
    pinned: bool,
    rank: Rank,
}

#[derive(Clone, Debug)]
struct LayerEntry {
    name: String,
    enabled: bool,
}

/// The scheduler. Holds every known cell and its state.
#[derive(Clone, Debug)]
pub struct Streamer {
    grid: Grid,
    limits: Limits,
    layers: Vec<LayerEntry>,
    cells: BTreeMap<CellId, Entry>,
    tick: u64,
}

impl Streamer {
    pub fn new(grid: Grid, limits: Limits) -> Self {
        Self {
            grid,
            limits: sanitize(limits),
            layers: vec![LayerEntry {
                name: String::new(),
                enabled: true,
            }],
            cells: BTreeMap::new(),
            tick: 0,
        }
    }
    pub fn grid(&self) -> &Grid {
        &self.grid
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = sanitize(limits);
    }
    /// Number of plans made so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    // ---- layers -----------------------------------------------------------------------
    /// The handle of the layer `name`, creating it (enabled) when new. `""` is the default layer.
    pub fn layer(&mut self, name: &str) -> Result<Layer, StreamError> {
        if let Some(found) = self.layer_by_name(name) {
            return Ok(found);
        }
        let index = u16::try_from(self.layers.len())
            .map_err(|_| StreamError("too many data layers".into()))?;
        self.layers.push(LayerEntry {
            name: name.to_owned(),
            enabled: true,
        });
        Ok(Layer(index))
    }
    pub fn layer_by_name(&self, name: &str) -> Option<Layer> {
        self.layers
            .iter()
            .position(|l| l.name == name)
            .map(|i| Layer(i as u16))
    }
    pub fn layer_name(&self, layer: Layer) -> &str {
        self.layers
            .get(usize::from(layer.0))
            .map_or("", |l| l.name.as_str())
    }
    pub fn layer_enabled(&self, layer: Layer) -> bool {
        self.layers
            .get(usize::from(layer.0))
            .is_some_and(|l| l.enabled)
    }
    /// Enables or disables a layer. Cells of a disabled layer stop being retained and unload on
    /// the next plan.
    pub fn set_layer_enabled(&mut self, layer: Layer, enabled: bool) {
        if let Some(entry) = self.layers.get_mut(usize::from(layer.0)) {
            entry.enabled = enabled;
        }
    }
    /// Names and states of all layers in handle order.
    pub fn layers(&self) -> impl Iterator<Item = (Layer, &str, bool)> {
        self.layers
            .iter()
            .enumerate()
            .map(|(i, l)| (Layer(i as u16), l.name.as_str(), l.enabled))
    }

    // ---- cells ------------------------------------------------------------------------
    /// Registers a cell with its content size in bytes. Fails on duplicates.
    pub fn add_cell(&mut self, id: CellId, bytes: u64) -> Result<(), StreamError> {
        if usize::from(id.layer.0) >= self.layers.len() {
            return Err(StreamError(format!("unknown layer {}", id.layer.0)));
        }
        if self.cells.contains_key(&id) {
            return Err(StreamError(format!("duplicate cell {}", id.key)));
        }
        self.cells.insert(
            id,
            Entry {
                bytes,
                state: CellState::Unloaded,
                failures: 0,
                retry_at: 0,
                pinned: false,
                rank: Rank::NONE,
            },
        );
        Ok(())
    }
    pub fn state(&self, id: CellId) -> Option<CellState> {
        self.cells.get(&id).map(|e| e.state)
    }
    pub fn bytes(&self, id: CellId) -> Option<u64> {
        self.cells.get(&id).map(|e| e.bytes)
    }
    pub fn failures(&self, id: CellId) -> u32 {
        self.cells.get(&id).map_or(0, |e| e.failures)
    }
    pub fn is_pinned(&self, id: CellId) -> bool {
        self.cells.get(&id).is_some_and(|e| e.pinned)
    }
    /// Rank computed by the last plan (use it to order work such as applying finished loads).
    pub fn rank(&self, id: CellId) -> Option<Rank> {
        self.cells.get(&id).map(|e| e.rank)
    }
    pub fn cells(&self) -> impl Iterator<Item = (CellId, CellState)> + '_ {
        self.cells.iter().map(|(id, e)| (*id, e.state))
    }
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for e in self.cells.values() {
            match e.state {
                CellState::Unloaded => counts.unloaded += 1,
                CellState::Loading => counts.loading += 1,
                CellState::Loaded => counts.loaded += 1,
                CellState::Unloading => counts.unloading += 1,
            }
            counts.pinned += usize::from(e.pinned);
        }
        counts
    }

    // ---- requests ---------------------------------------------------------------------
    /// Keeps the cell wanted regardless of sources until [`unpin`](Self::unpin). Pinned cells
    /// are never evicted; a disabled layer still wins. Clears the failure history.
    pub fn pin(&mut self, id: CellId) -> Result<(), StreamError> {
        let entry = self.entry_mut(id)?;
        entry.pinned = true;
        entry.failures = 0;
        entry.retry_at = 0;
        Ok(())
    }
    pub fn unpin(&mut self, id: CellId) -> Result<(), StreamError> {
        self.entry_mut(id)?.pinned = false;
        Ok(())
    }
    /// Clears the failure history so a failed cell may be retried.
    pub fn reset_failures(&mut self, id: CellId) -> Result<(), StreamError> {
        let entry = self.entry_mut(id)?;
        entry.failures = 0;
        entry.retry_at = 0;
        Ok(())
    }
    /// Starts loading an `Unloaded` cell now, outside the budgets (for forced loads).
    pub fn force_begin_load(&mut self, id: CellId) -> Result<(), StreamError> {
        let entry = self.entry_mut(id)?;
        expect(entry.state, CellState::Unloaded, "force_begin_load", id)?;
        entry.state = CellState::Loading;
        Ok(())
    }

    // ---- host reports -----------------------------------------------------------------
    /// `Loading -> Loaded`.
    pub fn complete_load(&mut self, id: CellId) -> Result<(), StreamError> {
        let entry = self.entry_mut(id)?;
        expect(entry.state, CellState::Loading, "complete_load", id)?;
        entry.state = CellState::Loaded;
        entry.failures = 0;
        entry.retry_at = 0;
        Ok(())
    }
    /// `Loading -> Unloaded` after a failed load; retried after an exponential backoff
    /// (2, 4, 8 ... ticks, at most 32) and not at all after [`MAX_AUTO_RETRIES`] failures.
    pub fn fail_load(&mut self, id: CellId) -> Result<(), StreamError> {
        let tick = self.tick;
        let entry = self.entry_mut(id)?;
        expect(entry.state, CellState::Loading, "fail_load", id)?;
        entry.state = CellState::Unloaded;
        entry.failures += 1;
        let backoff = (1u64 << entry.failures.min(6)).min(MAX_BACKOFF_TICKS);
        entry.retry_at = tick + backoff;
        Ok(())
    }
    /// `Unloading -> Unloaded`.
    pub fn complete_unload(&mut self, id: CellId) -> Result<(), StreamError> {
        let entry = self.entry_mut(id)?;
        expect(entry.state, CellState::Unloading, "complete_unload", id)?;
        entry.state = CellState::Unloaded;
        Ok(())
    }
    /// `Loaded -> Unloading` on request (for forced unloads); also drops the pin.
    pub fn force_begin_unload(&mut self, id: CellId) -> Result<(), StreamError> {
        let entry = self.entry_mut(id)?;
        expect(entry.state, CellState::Loaded, "force_begin_unload", id)?;
        entry.state = CellState::Unloading;
        entry.pinned = false;
        Ok(())
    }

    fn entry_mut(&mut self, id: CellId) -> Result<&mut Entry, StreamError> {
        self.cells
            .get_mut(&id)
            .ok_or_else(|| StreamError(format!("unknown cell {}", id.key)))
    }

    // ---- planning ---------------------------------------------------------------------
    /// Decides this tick's transitions for `sources` and applies them to the state machine.
    pub fn plan(&mut self, sources: &[Source]) -> Plan {
        self.tick += 1;
        let tick = self.tick;
        let mut wanted = BTreeMap::new();
        let mut retained = BTreeMap::new();
        for (id, entry) in &mut self.cells {
            let enabled = self.layers[usize::from(id.layer.0)].enabled;
            let (mut in_load, mut in_keep) = (false, false);
            let mut rank = Rank::NONE;
            for source in sources {
                let d = self.grid.distance(source.position, id.key);
                let keep = source.unload_radius.max(source.load_radius);
                if d <= source.load_radius {
                    in_load = true;
                }
                if d <= keep {
                    in_keep = true;
                    let candidate = Rank {
                        neg_priority: -i64::from(source.priority),
                        distance: d,
                    };
                    if candidate < rank {
                        rank = candidate;
                    }
                }
            }
            if entry.pinned {
                rank = Rank::PINNED;
            }
            entry.rank = rank;
            retained.insert(*id, enabled && (in_keep || entry.pinned));
            wanted.insert(
                *id,
                enabled
                    && (in_load || entry.pinned)
                    && entry.retry_at <= tick
                    && (entry.failures < MAX_AUTO_RETRIES || entry.pinned),
            );
        }

        let mut plan = Plan::default();
        for (id, entry) in &mut self.cells {
            if retained[id] {
                continue;
            }
            match entry.state {
                CellState::Loaded => {
                    entry.state = CellState::Unloading;
                    plan.start_unloads.push(*id);
                }
                CellState::Loading => {
                    entry.state = CellState::Unloaded;
                    plan.cancel_loads.push(*id);
                }
                _ => {}
            }
        }

        let counts = self.counts();
        let mut occupancy =
            counts.loaded + counts.loading + counts.unloading - plan.start_unloads.len();
        let mut in_flight = counts.loading;

        while occupancy > self.limits.max_loaded {
            let Some(victim) = self.worst_loaded(None) else {
                break;
            };
            self.begin_evict(victim, &mut plan);
            occupancy -= 1;
        }

        let mut candidates: Vec<(Rank, CellId)> = self
            .cells
            .iter()
            .filter(|(id, e)| e.state == CellState::Unloaded && wanted[id])
            .map(|(id, e)| (e.rank, *id))
            .collect();
        candidates.sort();
        let mut budget = TickBudget::new(self.limits.cells_per_tick, self.limits.bytes_per_tick);
        for (rank, id) in candidates {
            let bytes = self.cells[&id].bytes;
            if in_flight >= self.limits.max_in_flight || !budget.can_take(bytes) {
                break;
            }
            if occupancy >= self.limits.max_loaded {
                let Some(victim) = self.worst_loaded(Some(rank)) else {
                    break;
                };
                self.begin_evict(victim, &mut plan);
                occupancy -= 1;
            }
            budget.try_take(bytes);
            if let Some(entry) = self.cells.get_mut(&id) {
                entry.state = CellState::Loading;
            }
            plan.start_loads.push(id);
            plan.bytes = plan.bytes.saturating_add(bytes);
            occupancy += 1;
            in_flight += 1;
        }
        plan
    }

    /// The worst-ranked unpinned `Loaded` cell, which must rank strictly worse than `than`.
    fn worst_loaded(&self, than: Option<Rank>) -> Option<CellId> {
        self.cells
            .iter()
            .filter(|(_, e)| e.state == CellState::Loaded && !e.pinned)
            .filter(|(_, e)| than.is_none_or(|rank| e.rank > rank))
            .max_by(|a, b| a.1.rank.cmp(&b.1.rank).then(a.0.cmp(b.0)))
            .map(|(id, _)| *id)
    }

    fn begin_evict(&mut self, id: CellId, plan: &mut Plan) {
        if let Some(entry) = self.cells.get_mut(&id) {
            entry.state = CellState::Unloading;
        }
        plan.start_unloads.push(id);
    }
}

fn sanitize(limits: Limits) -> Limits {
    Limits {
        cells_per_tick: limits.cells_per_tick.max(1),
        bytes_per_tick: limits.bytes_per_tick,
        max_in_flight: limits.max_in_flight.max(1),
        max_loaded: limits.max_loaded.max(1),
    }
}

fn expect(actual: CellState, wanted: CellState, call: &str, id: CellId) -> Result<(), StreamError> {
    if actual == wanted {
        Ok(())
    } else {
        Err(StreamError(format!(
            "{call}: cell {} is {actual:?}, expected {wanted:?}",
            id.key
        )))
    }
}
