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
    SteamServersConnected, SteamServersDisconnected,
};

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::types::{AppId, AuthTicket, SteamEvent, SteamId, SteamStatus, SteamUser};

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
        }
    }

    fn client(&self) -> SteamResult<&Client> {
        match (&self.live, self.status.is_ready()) {
            (Some(live), true) => Ok(&live.client),
            _ => Err(self.unavailable()),
        }
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
        vec![overlay, persona, connected, disconnected]
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
}
