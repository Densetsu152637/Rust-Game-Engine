//! Real Steamworks backend over `steamworks::Client` (core: init, identity,
//! auth tickets, overlay, rich presence, callback events).
//!
//! Every call must be made from the tick thread. Callbacks registered with
//! Steam push into a shared queue that `run_callbacks` drains. Stages S4+ add
//! the remaining `SteamBackend` methods.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use steamworks::networking_types::NetworkingIdentity;
use steamworks::{
    CallbackHandle, Client, GameOverlayActivated, PersonaStateChange, SteamAPIInitError,
    SteamServersConnected, SteamServersDisconnected, UserAchievementStored, UserStatsReceived,
    UserStatsStored,
};

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::types::{AppId, AuthTicket, StatValue, SteamEvent, SteamId, SteamStatus, SteamUser};

/// `SteamAPI_Init` may only be paired with one `SteamAPI_Shutdown` per process.
static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Resets [`INITIALISED`] when dropped. Declared last in [`Live`] so the flag
/// clears only after the client has shut Steam down.
struct InitGuard;

impl Drop for InitGuard {
    fn drop(&mut self) {
        INITIALISED.store(false, Ordering::SeqCst);
    }
}

/// An initialised Steam client. Field order is the drop order: callback
/// handles unregister first, the client shuts Steam down, then the guard clears.
struct Live {
    _handles: Vec<CallbackHandle>,
    client: Client,
    _guard: InitGuard,
}

type EventQueue = Arc<Mutex<Vec<SteamEvent>>>;

fn lock(queue: &EventQueue) -> MutexGuard<'_, Vec<SteamEvent>> {
    queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Backend over `steamworks::Client`.
///
/// Created uninitialised; call [`SteamBackend::init`] (the host does this before
/// creating graphics). Only one `RealBackend` can be initialised per process.
pub struct RealBackend {
    live: Option<Live>,
    status: SteamStatus,
    events: EventQueue,
    tickets: BTreeMap<u32, steamworks::AuthTicket>,
    next_ticket: u32,
    stats_requested: bool,
}

impl RealBackend {
    /// An uninitialised backend; its status is `InitFailed` until `init` succeeds.
    pub fn new() -> Self {
        Self {
            live: None,
            status: SteamStatus::InitFailed("Steam has not been initialised".into()),
            events: Arc::new(Mutex::new(Vec::new())),
            tickets: BTreeMap::new(),
            next_ticket: 1,
            stats_requested: false,
        }
    }

    fn client(&self) -> SteamResult<&Client> {
        match (&self.live, self.status.is_ready()) {
            (Some(live), true) => Ok(&live.client),
            _ => Err(self.unavailable()),
        }
    }

    /// The client, once the stats request has been made.
    fn stats_client(&self) -> SteamResult<&Client> {
        if !self.stats_requested {
            return Err(SteamError::StatsNotRequested);
        }
        self.client()
    }

    fn register_callbacks(client: &Client, me: u64, events: &EventQueue) -> Vec<CallbackHandle> {
        let queue = Arc::clone(events);
        let overlay = client.register_callback(move |cb: GameOverlayActivated| {
            lock(&queue).push(SteamEvent::OverlayToggled(cb.active));
        });
        let queue = Arc::clone(events);
        let persona = client.register_callback(move |cb: PersonaStateChange| {
            if cb.steam_id.raw() == me {
                lock(&queue).push(SteamEvent::PersonaChanged(SteamId(me)));
            }
        });
        let queue = Arc::clone(events);
        let connected = client.register_callback(move |_: SteamServersConnected| {
            lock(&queue).push(SteamEvent::ConnectionChanged { connected: true });
        });
        let queue = Arc::clone(events);
        let disconnected = client.register_callback(move |_: SteamServersDisconnected| {
            lock(&queue).push(SteamEvent::ConnectionChanged { connected: false });
        });
        let queue = Arc::clone(events);
        let received = client.register_callback(move |cb: UserStatsReceived| {
            if cb.steam_id.raw() == me && cb.result.is_ok() {
                lock(&queue).push(SteamEvent::StatsReceived);
            }
        });
        let queue = Arc::clone(events);
        let stored = client.register_callback(move |cb: UserStatsStored| {
            if cb.result.is_ok() {
                lock(&queue).push(SteamEvent::StatsStored);
            }
        });
        let queue = Arc::clone(events);
        let achievement = client.register_callback(move |cb: UserAchievementStored| {
            // Progress notifications (current below maximum) are not unlocks.
            if cb.max_progress == 0 || cb.current_progress >= cb.max_progress {
                lock(&queue).push(SteamEvent::AchievementUnlocked(cb.achievement_name));
            }
        });
        vec![
            overlay,
            persona,
            connected,
            disconnected,
            received,
            stored,
            achievement,
        ]
    }
}

impl Default for RealBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// The steamworks crate builds C strings with `unwrap`, so reject NUL bytes first.
fn reject_nul(text: &str) -> SteamResult<()> {
    if text.contains('\0') {
        Err(SteamError::InvalidName(text.replace('\0', "\\0")))
    } else {
        Ok(())
    }
}

impl SteamBackend for RealBackend {
    fn status(&self) -> SteamStatus {
        self.status.clone()
    }

    fn init(&mut self, app_id: Option<AppId>) -> SteamStatus {
        if self.live.is_some() || !matches!(self.status, SteamStatus::InitFailed(_)) {
            return self.status();
        }
        if INITIALISED.swap(true, Ordering::SeqCst) {
            self.status =
                SteamStatus::InitFailed("Steam is already initialised in this process".into());
            return self.status();
        }
        let guard = InitGuard;
        let result = match app_id {
            Some(id) => Client::init_app(steamworks::AppId(id.0)),
            None => Client::init(),
        };
        match result {
            Ok(client) => {
                let me = client.user().steam_id().raw();
                let user = SteamUser {
                    id: SteamId(me),
                    name: client.friends().name(),
                };
                let app_id = AppId(client.utils().app_id().0);
                let handles = Self::register_callbacks(&client, me, &self.events);
                self.live = Some(Live {
                    _handles: handles,
                    client,
                    _guard: guard,
                });
                self.status = SteamStatus::Ready { app_id, user };
            }
            Err(err) => {
                drop(guard);
                self.status = match err {
                    SteamAPIInitError::NoSteamClient(_) => SteamStatus::NotRunning,
                    SteamAPIInitError::FailedGeneric(msg)
                    | SteamAPIInitError::VersionMismatch(msg) => SteamStatus::InitFailed(msg),
                };
            }
        }
        self.status()
    }

    fn run_callbacks(&mut self) -> Vec<SteamEvent> {
        if let Some(live) = &self.live {
            live.client.run_callbacks();
        }
        std::mem::take(&mut *lock(&self.events))
    }

    fn restart_app_if_necessary(&mut self, app_id: AppId) -> SteamResult<bool> {
        // Static call: valid before `init`, and works whether or not Steam runs.
        Ok(steamworks::restart_app_if_necessary(steamworks::AppId(
            app_id.0,
        )))
    }

    fn auth_session_ticket(&mut self) -> SteamResult<AuthTicket> {
        let (ticket, data) = self
            .client()?
            .user()
            .authentication_session_ticket(NetworkingIdentity::new());
        if data.is_empty() {
            return Err(SteamError::Rejected(
                "Steam returned no auth ticket yet; pump run_callbacks and retry".into(),
            ));
        }
        let handle = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        self.tickets.insert(handle, ticket);
        Ok(AuthTicket { handle, data })
    }

    fn cancel_auth_ticket(&mut self, handle: u32) -> SteamResult<()> {
        let user = self.client()?.user();
        let ticket = self
            .tickets
            .remove(&handle)
            .ok_or_else(|| SteamError::NotFound(format!("auth ticket {handle}")))?;
        user.cancel_authentication_ticket(ticket);
        Ok(())
    }

    fn activate_overlay(&mut self, dialog: &str) -> SteamResult<()> {
        reject_nul(dialog)?;
        self.client()?.friends().activate_game_overlay(dialog);
        Ok(())
    }

    fn set_rich_presence(&mut self, key: &str, value: Option<&str>) -> SteamResult<()> {
        reject_nul(key)?;
        if let Some(value) = value {
            reject_nul(value)?;
        }
        if self.client()?.friends().set_rich_presence(key, value) {
            Ok(())
        } else {
            Err(SteamError::Rejected(format!(
                "rich presence key {key:?} was refused"
            )))
        }
    }

    /// The `steamworks` 0.13.1 binding has no `RequestCurrentStats` call: with the
    /// bundled SDK (1.61 or newer) Steam delivers the user stats automatically
    /// after init and signals it with `SteamEvent::StatsReceived`. This only marks
    /// the request as made so reads are ordered after it.
    fn request_current_stats(&mut self) -> SteamResult<()> {
        self.client()?;
        self.stats_requested = true;
        Ok(())
    }

    fn achievement(&self, name: &str) -> SteamResult<bool> {
        reject_nul(name)?;
        let client = self.stats_client()?;
        client.user_stats().achievement(name).get().map_err(|()| {
            SteamError::NotFound(format!("achievement {name:?} (or stats not received yet)"))
        })
    }

    fn set_achievement(&mut self, name: &str, unlocked: bool) -> SteamResult<()> {
        reject_nul(name)?;
        let client = self.stats_client()?;
        let user_stats = client.user_stats();
        let achievement = user_stats.achievement(name);
        let result = if unlocked {
            achievement.set()
        } else {
            achievement.clear()
        };
        result.map_err(|()| SteamError::Rejected(format!("achievement {name:?} was refused")))
    }

    fn stat(&self, name: &str) -> SteamResult<StatValue> {
        reject_nul(name)?;
        let stats = self.stats_client()?.user_stats();
        if let Ok(value) = stats.get_stat_i32(name) {
            return Ok(StatValue::Int(value));
        }
        stats
            .get_stat_f32(name)
            .map(StatValue::Float)
            .map_err(|()| {
                SteamError::NotFound(format!("stat {name:?} (or stats not received yet)"))
            })
    }

    fn set_stat(&mut self, name: &str, value: StatValue) -> SteamResult<()> {
        reject_nul(name)?;
        let stats = self.stats_client()?.user_stats();
        let result = match value {
            StatValue::Int(v) => stats.set_stat_i32(name, v),
            StatValue::Float(v) => stats.set_stat_f32(name, v),
        };
        result.map_err(|()| {
            SteamError::Rejected(format!("stat {name:?} was refused (unknown or wrong type)"))
        })
    }

    fn store_stats(&mut self) -> SteamResult<()> {
        self.stats_client()?
            .user_stats()
            .store_stats()
            .map_err(|()| SteamError::Rejected("storing stats failed".into()))
    }

    fn cloud_enabled_for_app(&self) -> SteamResult<bool> {
        Ok(self.client()?.remote_storage().is_cloud_enabled_for_app())
    }

    fn cloud_enabled_for_account(&self) -> SteamResult<bool> {
        Ok(self
            .client()?
            .remote_storage()
            .is_cloud_enabled_for_account())
    }

    fn cloud_list(&self) -> SteamResult<Vec<String>> {
        Ok(self
            .client()?
            .remote_storage()
            .files()
            .into_iter()
            .map(|file| file.name)
            .collect())
    }

    fn cloud_timestamp(&self, name: &str) -> SteamResult<i64> {
        check_cloud_name(name)?;
        let file = self.client()?.remote_storage().file(name);
        if file.exists() {
            Ok(file.timestamp())
        } else {
            Err(SteamError::NotFound(name.to_owned()))
        }
    }

    fn cloud_read(&self, name: &str) -> SteamResult<Vec<u8>> {
        use std::io::Read;
        check_cloud_name(name)?;
        let file = self.client()?.remote_storage().file(name);
        if !file.exists() {
            return Err(SteamError::NotFound(name.to_owned()));
        }
        let mut data = Vec::new();
        file.read()
            .read_to_end(&mut data)
            .map_err(|e| SteamError::Rejected(format!("reading {name:?} failed: {e}")))?;
        Ok(data)
    }

    fn cloud_write(&mut self, name: &str, data: &[u8]) -> SteamResult<()> {
        use std::io::Write;
        check_cloud_name(name)?;
        let storage = self.client()?.remote_storage();
        if !(storage.is_cloud_enabled_for_app() && storage.is_cloud_enabled_for_account()) {
            return Err(SteamError::CloudDisabled);
        }
        let mut writer = storage.file(name).write();
        // Steam reports a write failure for exceeded quota or an invalid name; the
        // binding exposes no quota query, so both surface as `QuotaExceeded`.
        writer
            .write_all(data)
            .map_err(|_| SteamError::QuotaExceeded)?;
        drop(writer);
        if storage.file(name).exists() {
            Ok(())
        } else {
            Err(SteamError::Rejected(format!("{name:?} was not stored")))
        }
    }

    fn cloud_delete(&mut self, name: &str) -> SteamResult<()> {
        check_cloud_name(name)?;
        let file = self.client()?.remote_storage().file(name);
        if file.delete() {
            Ok(())
        } else {
            Err(SteamError::NotFound(name.to_owned()))
        }
    }
}

/// The `steamworks` crate builds C strings with `unwrap`, so names are checked first.
fn check_cloud_name(name: &str) -> SteamResult<()> {
    if crate::backend::valid_cloud_name(name) {
        Ok(())
    } else {
        Err(SteamError::InvalidName(name.to_owned()))
    }
}
