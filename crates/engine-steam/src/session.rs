//! The process-wide Steam session: owns the backend and is also the game-facing
//! handle, inserted into the game world as a resource.

use std::sync::{Arc, Mutex, MutexGuard};

use bevy_ecs::prelude::Resource;

use crate::backend::SteamBackend;
use crate::error::SteamResult;
use crate::stats::StatsService;
use crate::types::{StatValue, SteamStatus};

pub(crate) struct SessionState {
    pub(crate) backend: Box<dyn SteamBackend + Send>,
    pub(crate) started: bool,
    pub(crate) relaunching: bool,
    pub(crate) stats: StatsService,
}

/// The process-wide owner of the Steam backend. Clone it into every runner
/// generation: the first `before_graphics` performs the restart check and `init`,
/// later generations reuse the result without calling Steam again. The backend is
/// dropped (Steam shuts down) when the last clone is dropped.
///
/// The same type is inserted into the game world as a resource. Its methods are
/// safe to call whether or not Steam is present: with no Steam they return
/// `SteamError::Unavailable`. Call them from the tick thread only.
#[derive(Clone, Resource)]
pub struct SteamSession {
    inner: Arc<Mutex<SessionState>>,
}

impl SteamSession {
    /// Wraps a backend.
    pub fn new(backend: Box<dyn SteamBackend + Send>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionState {
                backend,
                started: false,
                relaunching: false,
                stats: StatsService::default(),
            })),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, SessionState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The backend current status.
    pub fn status(&self) -> SteamStatus {
        self.lock().backend.status()
    }

    /// Whether an achievement is unlocked.
    pub fn is_achievement_unlocked(&self, name: &str) -> SteamResult<bool> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.is_unlocked(backend.as_mut(), name)
    }

    /// Unlocks an achievement; `Ok(true)` only for the call that unlocked it.
    pub fn unlock_achievement(&self, name: &str) -> SteamResult<bool> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.unlock(backend.as_mut(), name)
    }

    /// Reads a stat.
    pub fn stat(&self, name: &str) -> SteamResult<StatValue> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.stat(backend.as_mut(), name)
    }

    /// Sets a stat. Changes are stored in batches (see `StatsService::tick`).
    pub fn set_stat(&self, name: &str, value: StatValue) -> SteamResult<()> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.set_stat(backend.as_mut(), name, value)
    }

    /// Sets an integer progress stat clamped to `0..=max`; returns the stored value.
    pub fn set_progress(&self, name: &str, value: i32, max: i32) -> SteamResult<i32> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.set_progress(backend.as_mut(), name, value, max)
    }

    /// Adds to an integer progress stat, clamped to `0..=max`; returns the new value.
    pub fn add_progress(&self, name: &str, delta: i32, max: i32) -> SteamResult<i32> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.add_progress(backend.as_mut(), name, delta, max)
    }

    /// Stores pending stat changes immediately instead of waiting for the throttle.
    pub fn store_stats_now(&self) -> SteamResult<bool> {
        let mut state = self.lock();
        let SessionState { backend, stats, .. } = &mut *state;
        stats.flush(backend.as_mut())
    }
}
