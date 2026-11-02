//! Steam Cloud mirror of the local save slots.
//!
//! [`CloudSync`] keeps `PersistenceStore` slots (`<root>/<project>/saves/<slot>.json`)
//! and Steam Remote Storage files `saves/<slot>.json` in step: it pulls at startup
//! before the game loads anything, and pushes after each successful save. The local
//! store stays authoritative; any cloud failure leaves local saves untouched.
//!
//! Conflict policy for a slot present on both sides with different bytes: the higher
//! `content_version` wins; on a tie the newer modification time (whole seconds) wins;
//! on a full tie the local copy is kept and a `CloudConflict` event is reported.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use engine_project::persistence::PersistenceStore;

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::types::SteamEvent;

/// What a sync did to one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// The cloud copy replaced the local file (the old file is the `.bak`).
    Pulled,
    /// The local file was uploaded.
    Pushed,
    /// Both sides already agree.
    Unchanged,
    /// Same version and time, different bytes: local kept, `CloudConflict` raised.
    Conflict,
}

/// Result of [`CloudSync::sync_all`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloudReport {
    /// True when Cloud was unavailable or disabled and nothing was synced.
    pub local_only: bool,
    /// Per-slot outcomes, sorted by slot.
    pub slots: Vec<(String, SyncAction)>,
    /// Slots whose sync failed, with the reason.
    pub failures: Vec<(String, String)>,
    /// Events for the game (`CloudConflict`, `CloudSyncFailed`).
    pub events: Vec<SteamEvent>,
}

/// Mirrors a `PersistenceStore`'s save slots to Steam Cloud.
#[derive(Clone)]
pub struct CloudSync {
    store: Arc<PersistenceStore>,
}

fn prefix_name(slot: &str) -> String {
    format!("saves/{slot}.json")
}

/// The slot a cloud file name belongs to, if it is a safe `saves/<slot>.json` name.
fn slot_of(name: &str) -> Option<&str> {
    let slot = name.strip_prefix("saves/")?.strip_suffix(".json")?;
    (!slot.is_empty()
        && slot.len() <= 64
        && slot
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'))
    .then_some(slot)
}

fn store_error(error: engine_project::Error) -> SteamError {
    let first = error.diagnostics.first();
    match first.map(|d| d.code.as_str()) {
        Some("persistence.slot.invalid") => {
            SteamError::InvalidName(first.map(|d| d.message.clone()).unwrap_or_default())
        }
        _ => SteamError::Rejected(error.to_string()),
    }
}

fn is_corrupt(error: &engine_project::Error) -> bool {
    error
        .diagnostics
        .first()
        .is_some_and(|d| d.code == "persistence.corrupt")
}

impl CloudSync {
    /// Mirrors the slots of `store`.
    pub fn new(store: Arc<PersistenceStore>) -> Self {
        Self { store }
    }

    /// True when Steam is ready and Cloud is enabled for both the app and account.
    pub fn available(backend: &dyn SteamBackend) -> bool {
        backend.status().is_ready() && backend.cloud_enabled() == Ok(true)
    }

    /// Syncs every slot found locally or in the cloud. Call at startup, before the
    /// game loads a save. Never fails: problems are reported per slot.
    pub fn sync_all(&self, backend: &mut dyn SteamBackend) -> CloudReport {
        let mut report = CloudReport::default();
        if !Self::available(backend) {
            report.local_only = true;
            return report;
        }
        let mut slots = self.store.slots().unwrap_or_default();
        for name in backend.cloud_list().unwrap_or_default() {
            if let Some(slot) = slot_of(&name)
                && !slots.iter().any(|s| s == slot)
            {
                slots.push(slot.to_owned());
            }
        }
        slots.sort();
        for slot in slots {
            match self.sync_slot(backend, &slot) {
                Ok(action) => {
                    if action == SyncAction::Conflict {
                        report
                            .events
                            .push(SteamEvent::CloudConflict { name: slot.clone() });
                    }
                    report.slots.push((slot, action));
                }
                Err(error) => {
                    let message = error.to_string();
                    report.events.push(SteamEvent::CloudSyncFailed {
                        slot: slot.clone(),
                        message: message.clone(),
                    });
                    report.failures.push((slot, message));
                }
            }
        }
        report
    }

    /// Uploads a saved slot. Call after a successful save. A disabled Cloud is not
    /// an error for the game: it returns `Err(CloudDisabled)` for the caller to ignore.
    pub fn push_slot(&self, backend: &mut dyn SteamBackend, slot: &str) -> SteamResult<()> {
        let path = self.store.slot_path(slot).map_err(store_error)?;
        if !Self::available(backend) {
            return Err(SteamError::CloudDisabled);
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| SteamError::Rejected(format!("reading local slot {slot:?}: {e}")))?;
        backend.cloud_write(&prefix_name(slot), &bytes)
    }

    fn local_seconds(&self, slot: &str) -> SteamResult<i64> {
        let path = self.store.slot_path(slot).map_err(store_error)?;
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|e| SteamError::Rejected(format!("local slot {slot:?} time: {e}")))?;
        Ok(modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64))
    }

    fn sync_slot(&self, backend: &mut dyn SteamBackend, slot: &str) -> SteamResult<SyncAction> {
        let name = prefix_name(slot);
        // A local slot that exists but does not parse is treated as absent, so a valid
        // cloud copy heals it (`install_slot_bytes` keeps the damaged file as `.bak`).
        // Other failures (invalid name, links, I/O) still stop the sync.
        let (local, corrupt) = match self.store.slot_header(slot) {
            Ok(header) => (header, None),
            Err(error) if is_corrupt(&error) => (None, Some(store_error(error))),
            Err(error) => return Err(store_error(error)),
        };
        let cloud_stamp = match backend.cloud_timestamp(&name) {
            Ok(stamp) => Some(stamp),
            Err(SteamError::NotFound(_)) => None,
            Err(other) => return Err(other),
        };
        if let Some(error) = corrupt
            && cloud_stamp.is_none()
        {
            // Nothing to heal from; never upload the damaged file.
            return Err(error);
        }
        let (Some(local), Some(cloud_stamp)) = (local, cloud_stamp) else {
            return match (local.is_some(), cloud_stamp.is_some()) {
                (true, false) => self.push(backend, slot).map(|()| SyncAction::Pushed),
                (false, true) => self.pull(backend, slot).map(|()| SyncAction::Pulled),
                _ => Ok(SyncAction::Unchanged),
            };
        };
        let path = self.store.slot_path(slot).map_err(store_error)?;
        let local_bytes = std::fs::read(&path)
            .map_err(|e| SteamError::Rejected(format!("reading local slot {slot:?}: {e}")))?;
        let cloud_bytes = backend.cloud_read(&name)?;
        if local_bytes == cloud_bytes {
            return Ok(SyncAction::Unchanged);
        }
        let cloud = self.store.parse_header(&cloud_bytes).map_err(store_error)?;
        let pull_wins = match cloud.content_version.cmp(&local.content_version) {
            std::cmp::Ordering::Greater => Some(true),
            std::cmp::Ordering::Less => Some(false),
            std::cmp::Ordering::Equal => match cloud_stamp.cmp(&self.local_seconds(slot)?) {
                std::cmp::Ordering::Greater => Some(true),
                std::cmp::Ordering::Less => Some(false),
                std::cmp::Ordering::Equal => None,
            },
        };
        match pull_wins {
            Some(true) => self
                .store
                .install_slot_bytes(slot, &cloud_bytes)
                .map_err(store_error)
                .map(|()| SyncAction::Pulled),
            Some(false) => backend
                .cloud_write(&name, &local_bytes)
                .map(|()| SyncAction::Pushed),
            None => Ok(SyncAction::Conflict),
        }
    }

    fn push(&self, backend: &mut dyn SteamBackend, slot: &str) -> SteamResult<()> {
        let path = self.store.slot_path(slot).map_err(store_error)?;
        let bytes = std::fs::read(&path)
            .map_err(|e| SteamError::Rejected(format!("reading local slot {slot:?}: {e}")))?;
        backend.cloud_write(&prefix_name(slot), &bytes)
    }

    fn pull(&self, backend: &mut dyn SteamBackend, slot: &str) -> SteamResult<()> {
        let bytes = backend.cloud_read(&prefix_name(slot))?;
        self.store
            .install_slot_bytes(slot, &bytes)
            .map_err(store_error)
    }
}
