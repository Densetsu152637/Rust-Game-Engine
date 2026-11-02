//! Achievements and stats service: idempotent unlocks, clamped progress and a
//! throttled, batched `store_stats`.
//!
//! The service is plain logic over any [`SteamBackend`], so it is tested with the
//! fake backend. It requests the user stats lazily before the first read or write,
//! which guarantees request-before-read ordering.

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::types::StatValue;

/// Fixed ticks (10 seconds at 60 Hz) between automatic stat stores.
pub const DEFAULT_STORE_INTERVAL_TICKS: u64 = 600;

/// Stat and achievement bookkeeping for one Steam session.
#[derive(Debug, Clone)]
pub struct StatsService {
    requested: bool,
    dirty: bool,
    last_store: Option<u64>,
    interval_ticks: u64,
}

impl Default for StatsService {
    fn default() -> Self {
        Self::new(DEFAULT_STORE_INTERVAL_TICKS)
    }
}

impl StatsService {
    /// A service that stores changed stats at most once per `interval_ticks`.
    pub fn new(interval_ticks: u64) -> Self {
        Self {
            requested: false,
            dirty: false,
            last_store: None,
            interval_ticks,
        }
    }

    /// True when changes are waiting for the next store.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Requests the user stats once; later calls are no-ops.
    pub fn request(&mut self, backend: &mut dyn SteamBackend) -> SteamResult<()> {
        if !self.requested {
            backend.request_current_stats()?;
            self.requested = true;
        }
        Ok(())
    }

    /// Whether an achievement is unlocked.
    pub fn is_unlocked(&mut self, backend: &mut dyn SteamBackend, name: &str) -> SteamResult<bool> {
        self.request(backend)?;
        backend.achievement(name)
    }

    /// Unlocks an achievement. Returns `true` only when this call unlocked it;
    /// unlocking an already unlocked achievement does nothing.
    pub fn unlock(&mut self, backend: &mut dyn SteamBackend, name: &str) -> SteamResult<bool> {
        self.request(backend)?;
        if backend.achievement(name)? {
            return Ok(false);
        }
        backend.set_achievement(name, true)?;
        self.dirty = true;
        Ok(true)
    }

    /// Reads a stat.
    pub fn stat(&mut self, backend: &mut dyn SteamBackend, name: &str) -> SteamResult<StatValue> {
        self.request(backend)?;
        backend.stat(name)
    }

    /// Sets a stat; marks the batch dirty only when the value changed.
    pub fn set_stat(
        &mut self,
        backend: &mut dyn SteamBackend,
        name: &str,
        value: StatValue,
    ) -> SteamResult<()> {
        self.request(backend)?;
        if backend.stat(name)? == value {
            return Ok(());
        }
        backend.set_stat(name, value)?;
        self.dirty = true;
        Ok(())
    }

    /// Sets an integer progress stat clamped to `0..=max` and returns the stored value.
    pub fn set_progress(
        &mut self,
        backend: &mut dyn SteamBackend,
        name: &str,
        value: i32,
        max: i32,
    ) -> SteamResult<i32> {
        if max < 0 {
            return Err(SteamError::Rejected("progress maximum is negative".into()));
        }
        let clamped = value.clamp(0, max);
        self.set_stat(backend, name, StatValue::Int(clamped))?;
        Ok(clamped)
    }

    /// Adds `delta` to an integer progress stat, saturating and clamping to `0..=max`.
    pub fn add_progress(
        &mut self,
        backend: &mut dyn SteamBackend,
        name: &str,
        delta: i32,
        max: i32,
    ) -> SteamResult<i32> {
        let StatValue::Int(current) = self.stat(backend, name)? else {
            return Err(SteamError::Rejected(format!(
                "stat {name} is not an integer"
            )));
        };
        self.set_progress(backend, name, current.saturating_add(delta), max)
    }

    /// Stores pending changes if `interval_ticks` have passed since the last store
    /// (the first store is immediate). Returns whether a store was issued. A failed
    /// store keeps the batch dirty and waits a full interval before retrying.
    pub fn tick(&mut self, backend: &mut dyn SteamBackend, tick: u64) -> SteamResult<bool> {
        if !self.dirty {
            return Ok(false);
        }
        if let Some(last) = self.last_store
            && tick.saturating_sub(last) < self.interval_ticks
        {
            return Ok(false);
        }
        self.last_store = Some(tick);
        backend.store_stats()?;
        self.dirty = false;
        Ok(true)
    }

    /// Stores pending changes now (for shutdown). Returns whether a store was issued.
    pub fn flush(&mut self, backend: &mut dyn SteamBackend) -> SteamResult<bool> {
        if !self.dirty {
            return Ok(false);
        }
        backend.store_stats()?;
        self.dirty = false;
        Ok(true)
    }
}
