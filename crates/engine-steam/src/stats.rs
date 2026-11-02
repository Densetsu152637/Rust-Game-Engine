//! Achievements and stats service: idempotent unlocks, clamped progress and a
//! throttled, batched `store_stats`.
//!
//! The service is plain logic over any [`SteamBackend`], so it is tested with the
//! fake backend. It requests the user stats lazily before the first read or write,
//! which guarantees request-before-read ordering.

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::types::StatValue;

/// A change made before Steam delivered the user stats.
#[derive(Debug, Clone, PartialEq)]
enum Pending {
    Unlock(String),
    SetStat(String, StatValue),
}

/// Most distinct changes held while Steam has not delivered the stats.
pub const MAX_PENDING_CHANGES: usize = 256;

/// Fixed ticks (10 seconds at 60 Hz) between automatic stat stores.
pub const DEFAULT_STORE_INTERVAL_TICKS: u64 = 600;

/// Stat and achievement bookkeeping for one Steam session.
#[derive(Debug, Clone)]
pub struct StatsService {
    requested: bool,
    dirty: bool,
    last_store: Option<u64>,
    interval_ticks: u64,
    /// Changes queued while the stats were not ready; applied by `tick`, or by the
    /// next call that finds the stats ready.
    pending: Vec<Pending>,
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
            pending: Vec::new(),
        }
    }

    fn check_room(&self) -> SteamResult<()> {
        if self.pending.len() >= MAX_PENDING_CHANGES {
            return Err(SteamError::Rejected(format!(
                "more than {MAX_PENDING_CHANGES} changes queued before Steam delivered the stats"
            )));
        }
        Ok(())
    }

    /// Number of changes waiting for Steam to deliver the stats.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Applies queued changes once the stats are ready (a not-ready backend keeps
    /// them queued). Unknown names are dropped. Called by `tick` and every other entry.
    pub fn apply_pending(&mut self, backend: &mut dyn SteamBackend) {
        if self.pending.is_empty() {
            return;
        }
        let queue = std::mem::take(&mut self.pending);
        let mut rest = queue.into_iter();
        while let Some(op) = rest.next() {
            let outcome = match &op {
                Pending::Unlock(name) => match backend.achievement(name) {
                    Ok(true) => Ok(false),
                    Ok(false) => backend.set_achievement(name, true).map(|()| true),
                    Err(e) => Err(e),
                },
                Pending::SetStat(name, value) => match backend.stat(name) {
                    Ok(current) if current == *value => Ok(false),
                    Ok(_) => backend.set_stat(name, *value).map(|()| true),
                    Err(e) => Err(e),
                },
            };
            match outcome {
                Ok(changed) => self.dirty |= changed,
                Err(SteamError::StatsNotReady | SteamError::StatsNotRequested) => {
                    self.pending.push(op);
                    self.pending.extend(rest);
                    return;
                }
                Err(_) => {} // unknown or refused: nothing to retry
            }
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
        self.apply_pending(backend);
        backend.achievement(name)
    }

    /// Unlocks an achievement. Returns `true` only when this call unlocked it;
    /// unlocking an already unlocked achievement does nothing. While Steam has not
    /// delivered the stats the unlock is queued and this returns `Ok(true)` even if the
    /// achievement turns out to be unlocked already (the queue then changes nothing).
    pub fn unlock(&mut self, backend: &mut dyn SteamBackend, name: &str) -> SteamResult<bool> {
        self.request(backend)?;
        self.apply_pending(backend);
        let queued = Pending::Unlock(name.to_owned());
        match backend.achievement(name) {
            Ok(true) => return Ok(false),
            Ok(false) => {}
            Err(SteamError::StatsNotReady) => {
                if self.pending.contains(&queued) {
                    return Ok(false);
                }
                self.check_room()?;
                self.pending.push(queued);
                return Ok(true);
            }
            Err(other) => return Err(other),
        }
        backend.set_achievement(name, true)?;
        self.dirty = true;
        Ok(true)
    }

    /// Reads a stat.
    pub fn stat(&mut self, backend: &mut dyn SteamBackend, name: &str) -> SteamResult<StatValue> {
        self.request(backend)?;
        self.apply_pending(backend);
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
        self.apply_pending(backend);
        match backend.stat(name) {
            Ok(current) if current == value => return Ok(()),
            Ok(_) => {}
            Err(SteamError::StatsNotReady) => {
                // The latest queued value for a stat wins.
                self.pending
                    .retain(|p| !matches!(p, Pending::SetStat(n, _) if n == name));
                self.check_room()?;
                self.pending.push(Pending::SetStat(name.to_owned(), value));
                return Ok(());
            }
            Err(other) => return Err(other),
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
        self.apply_pending(backend);
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
        self.apply_pending(backend);
        if !self.dirty {
            return Ok(false);
        }
        backend.store_stats()?;
        self.dirty = false;
        Ok(true)
    }
}
