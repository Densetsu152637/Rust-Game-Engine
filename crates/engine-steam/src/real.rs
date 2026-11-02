//! Real Steamworks backend over `steamworks::Client` (core: init, identity,
//! auth tickets, overlay, rich presence, callback events).
//!
//! Every call must be made from the tick thread. Callbacks registered with
//! Steam push into a shared queue that `run_callbacks` drains. Stages S4+ add
//! the remaining `SteamBackend` methods.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use steamworks::networking_types::NetworkingIdentity;
use steamworks::{
    CallbackHandle, Client, GameOverlayActivated, PersonaStateChange, SteamAPIInitError,
    SteamServersConnected, SteamServersDisconnected, UserAchievementStored, UserStatsReceived,
    UserStatsStored, ValidateAuthTicketResponse,
};

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::net::*;
use crate::real_net::{AuthEvents, RealNet};
use crate::types::{
    ActionGlyph, AnalogActionState, AppId, AuthTicket, ControllerId, DigitalActionState,
    InputActionKind, InstalledItem, StatValue, SteamEvent, SteamId, SteamStatus, SteamUser,
    WorkshopItemId,
};
use crate::workshop::{ItemState, ItemUpdate, RequestId, UgcQuery, UgcRaw, UpdateProgress};

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
    stats_received: Arc<AtomicBool>,
    net: RealNet,
    input_ready: bool,
    pub(crate) ugc: crate::real_ugc::RealUgc,
    pub(crate) lobby_events: crate::real_lobby::LobbyEvents,
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
            stats_received: Arc::new(AtomicBool::new(false)),
            net: RealNet::default(),
            input_ready: false,
            ugc: Default::default(),
            lobby_events: Arc::default(),
        }
    }

    pub(crate) fn client(&self) -> SteamResult<&Client> {
        match (&self.live, self.status.is_ready()) {
            (Some(live), true) => Ok(&live.client),
            _ => Err(self.unavailable()),
        }
    }

    /// The client, once Steam Input is initialised.
    fn input_client(&self) -> SteamResult<&Client> {
        let client = self.client()?;
        if self.input_ready {
            Ok(client)
        } else {
            Err(SteamError::Rejected(
                "Steam Input is not initialised".into(),
            ))
        }
    }

    /// The client, once the stats request has been made.
    fn stats_client(&self) -> SteamResult<&Client> {
        if !self.stats_requested {
            return Err(SteamError::StatsNotRequested);
        }
        self.client()
    }

    /// `StatsNotReady` until Steam has delivered the stats, otherwise the given error.
    fn pending_or(&self, error: SteamError) -> SteamError {
        if self.stats_received.load(Ordering::SeqCst) {
            error
        } else {
            SteamError::StatsNotReady
        }
    }

    fn not_found_or_pending(&self, what: String) -> SteamError {
        self.pending_or(SteamError::NotFound(what))
    }

    fn register_callbacks(
        client: &Client,
        me: u64,
        events: &EventQueue,
        auth: &AuthEvents,
        stats_received: &Arc<AtomicBool>,
    ) -> Vec<CallbackHandle> {
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
        let flag = Arc::clone(stats_received);
        let received = client.register_callback(move |cb: UserStatsReceived| {
            if cb.steam_id.raw() == me && cb.result.is_ok() {
                flag.store(true, Ordering::SeqCst);
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
        let queue = Arc::clone(auth);
        let validated = client.register_callback(move |cb: ValidateAuthTicketResponse| {
            let (accepted, reason) = match &cb.response {
                Ok(()) => (true, String::new()),
                Err(error) => (false, error.to_string()),
            };
            queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(NetEvent::AuthResult {
                    user: SteamId(cb.steam_id.raw()),
                    accepted,
                    reason,
                });
        });
        vec![
            overlay,
            persona,
            connected,
            disconnected,
            received,
            stored,
            achievement,
            validated,
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

impl Drop for RealBackend {
    fn drop(&mut self) {
        // Steam Input must be shut down before the client; fields drop after this runs.
        if self.input_ready
            && let Some(live) = &self.live
        {
            live.client.input().shutdown();
        }
    }
}

impl SteamBackend for RealBackend {
    fn lobby_backend(&mut self) -> Option<&mut dyn crate::LobbyBackend> {
        Some(self)
    }

    fn subscribed_items(&self) -> SteamResult<Vec<WorkshopItemId>> {
        crate::real_ugc::subscribed(self)
    }

    fn installed_item(&self, item: WorkshopItemId) -> SteamResult<Option<InstalledItem>> {
        crate::real_ugc::installed(self, item)
    }

    fn ugc_create_item(&mut self) -> SteamResult<RequestId> {
        crate::real_ugc::create_item(self)
    }

    fn ugc_submit_item(
        &mut self,
        item: WorkshopItemId,
        update: &ItemUpdate,
    ) -> SteamResult<RequestId> {
        crate::real_ugc::submit_item(self, item, update)
    }

    fn ugc_update_progress(&self, request: RequestId) -> Option<UpdateProgress> {
        crate::real_ugc::update_progress(self, request)
    }

    fn ugc_subscribe(&mut self, item: WorkshopItemId, subscribe: bool) -> SteamResult<RequestId> {
        crate::real_ugc::subscribe(self, item, subscribe)
    }

    fn ugc_query(&mut self, query: &UgcQuery) -> SteamResult<RequestId> {
        crate::real_ugc::query(self, query)
    }

    fn ugc_item_state(&self, item: WorkshopItemId) -> SteamResult<ItemState> {
        crate::real_ugc::item_state(self, item)
    }

    fn ugc_download(&mut self, item: WorkshopItemId, high_priority: bool) -> SteamResult<()> {
        crate::real_ugc::download(self, item, high_priority)
    }

    fn ugc_download_progress(&self, item: WorkshopItemId) -> Option<(u64, u64)> {
        crate::real_ugc::download_progress(self, item)
    }

    fn ugc_poll(&mut self) -> Vec<UgcRaw> {
        crate::real_ugc::poll(self)
    }

    fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
        crate::real_input::controllers(self.input_client()?)
    }

    fn activate_action_set(&mut self, controller: ControllerId, set: &str) -> SteamResult<()> {
        crate::real_input::activate_action_set(self.input_client()?, controller, set)
    }

    fn input_init(&mut self, manifest: Option<&std::path::Path>) -> SteamResult<()> {
        if self.input_ready {
            return Ok(());
        }
        crate::real_input::init(self.client()?, manifest)?;
        self.input_ready = true;
        Ok(())
    }

    fn input_run_frame(&mut self) -> SteamResult<()> {
        self.input_client()?.input().run_frame();
        Ok(())
    }

    fn input_digital(
        &self,
        controller: ControllerId,
        action: &str,
    ) -> SteamResult<DigitalActionState> {
        crate::real_input::digital(self.input_client()?, controller, action)
    }

    fn input_analog(
        &self,
        controller: ControllerId,
        action: &str,
    ) -> SteamResult<AnalogActionState> {
        crate::real_input::analog(self.input_client()?, controller, action)
    }

    fn input_glyphs(
        &self,
        controller: ControllerId,
        set: &str,
        action: &str,
        kind: InputActionKind,
    ) -> SteamResult<Vec<ActionGlyph>> {
        crate::real_input::glyphs(self.input_client()?, controller, set, action, kind)
    }

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
                let mut handles = Self::register_callbacks(
                    &client,
                    me,
                    &self.events,
                    &self.net.auth_events,
                    &self.stats_received,
                );
                handles.extend(crate::real_lobby::register(&client, &self.lobby_events));
                handles.extend(crate::real_ugc::register(&client, &self.ugc));
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
        client
            .user_stats()
            .achievement(name)
            .get()
            .map_err(|()| self.not_found_or_pending(format!("achievement {name:?}")))
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
        result.map_err(|()| {
            self.pending_or(SteamError::Rejected(format!(
                "achievement {name:?} was refused"
            )))
        })
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
            .map_err(|()| self.not_found_or_pending(format!("stat {name:?}")))
    }

    fn set_stat(&mut self, name: &str, value: StatValue) -> SteamResult<()> {
        reject_nul(name)?;
        let stats = self.stats_client()?.user_stats();
        let result = match value {
            StatValue::Int(v) => stats.set_stat_i32(name, v),
            StatValue::Float(v) => stats.set_stat_f32(name, v),
        };
        result.map_err(|()| {
            self.pending_or(SteamError::Rejected(format!(
                "stat {name:?} was refused (unknown or wrong type)"
            )))
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

    fn net_init_relay(&mut self) -> SteamResult<()> {
        let client = self.client()?.clone();
        self.net.init_relay(&client);
        Ok(())
    }

    fn net_relay_status(&self) -> SteamResult<RelayStatus> {
        Ok(self.net.relay_status(self.client()?))
    }

    fn net_max_message_size(&self) -> SteamResult<usize> {
        self.client()?;
        Ok(self.net.max_message())
    }

    fn net_listen_p2p(&mut self, virtual_port: i32) -> SteamResult<NetListenId> {
        let client = self.client()?.clone();
        self.net.listen_p2p(&client, virtual_port)
    }

    fn net_listen_ip(&mut self, addr: SocketAddr) -> SteamResult<NetListenId> {
        let client = self.client()?.clone();
        self.net.listen_ip(&client, addr)
    }

    fn net_close_listen(&mut self, listen: NetListenId) -> SteamResult<()> {
        self.net.close_listen(listen)
    }

    fn net_connect_p2p(&mut self, peer: SteamId, virtual_port: i32) -> SteamResult<NetConnId> {
        let client = self.client()?.clone();
        self.net.connect_p2p(&client, peer, virtual_port)
    }

    fn net_connect_ip(&mut self, addr: SocketAddr) -> SteamResult<NetConnId> {
        let client = self.client()?.clone();
        self.net.connect_ip(&client, addr)
    }

    fn net_accept(&mut self, conn: NetConnId) -> SteamResult<()> {
        self.net.accept(conn)
    }

    fn net_close(&mut self, conn: NetConnId, reason: &str, linger: bool) -> SteamResult<()> {
        self.net.close(conn, reason, linger)
    }

    fn net_send(&mut self, conn: NetConnId, data: &[u8], flags: NetSendFlags) -> SteamResult<()> {
        self.net.send(conn, data, flags)
    }

    fn net_flush(&mut self, conn: NetConnId) -> SteamResult<()> {
        self.net.flush(conn)
    }

    fn net_receive(&mut self, conn: NetConnId, max: usize) -> SteamResult<Vec<Vec<u8>>> {
        self.net.receive(conn, max)
    }

    fn net_poll_events(&mut self) -> Vec<NetEvent> {
        self.net.poll_events()
    }

    fn net_conn_stats(&self, conn: NetConnId) -> Option<NetConnStats> {
        self.net.conn_stats(self.client().ok()?, conn)
    }

    fn net_begin_auth(&mut self, user: SteamId, ticket: &[u8]) -> SteamResult<()> {
        self.client()?
            .user()
            .begin_authentication_session(steamworks::SteamId::from_raw(user.0), ticket)
            .map_err(|e| SteamError::Rejected(e.to_string()))
    }

    fn net_end_auth(&mut self, user: SteamId) -> SteamResult<()> {
        self.client()?
            .user()
            .end_authentication_session(steamworks::SteamId::from_raw(user.0));
        Ok(())
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
