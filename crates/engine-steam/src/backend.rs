//! The backend trait. Every method defaults to "unavailable" for the backend's
//! current status, so a backend only overrides what it supports.

use std::net::SocketAddr;

use crate::error::{SteamError, SteamResult};
use crate::net::*;
use crate::types::*;
use crate::workshop::{ItemState, ItemUpdate, RequestId, UgcQuery, UgcRaw, UpdateProgress};

/// Abstraction over the Steamworks API.
///
/// All calls are made from the tick thread; `run_callbacks` is called once per
/// tick and returns the events produced since the last call.
pub trait SteamBackend {
    /// Current availability.
    fn status(&self) -> SteamStatus;

    /// Error for a call made while unavailable (or not supported by this backend).
    fn unavailable(&self) -> SteamError {
        match self.status().unavailable_reason() {
            Some(reason) => SteamError::Unavailable(reason),
            None => SteamError::Rejected("operation not supported by this backend".into()),
        }
    }

    /// Initialises the backend (idempotent) and returns the resulting status.
    ///
    /// `app_id` forces an App ID; `None` uses the launch environment
    /// (`steam_appid.txt` for development runs). Backends that need no setup
    /// keep the default, which only reports the current status.
    fn init(&mut self, _app_id: Option<AppId>) -> SteamStatus {
        self.status()
    }

    /// Pumps Steam callbacks and returns new events.
    fn run_callbacks(&mut self) -> Vec<SteamEvent> {
        Vec::new()
    }

    /// Returns `true` when Steam is relaunching the game through the client.
    fn restart_app_if_necessary(&mut self, _app_id: AppId) -> SteamResult<bool> {
        Err(self.unavailable())
    }
    fn auth_session_ticket(&mut self) -> SteamResult<AuthTicket> {
        Err(self.unavailable())
    }
    fn cancel_auth_ticket(&mut self, _handle: u32) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn activate_overlay(&mut self, _dialog: &str) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Sets (`Some`) or clears (`None`) a rich presence key.
    fn set_rich_presence(&mut self, _key: &str, _value: Option<&str>) -> SteamResult<()> {
        Err(self.unavailable())
    }

    // Achievements and stats.
    fn request_current_stats(&mut self) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn achievement(&self, _name: &str) -> SteamResult<bool> {
        Err(self.unavailable())
    }
    fn set_achievement(&mut self, _name: &str, _unlocked: bool) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn stat(&self, _name: &str) -> SteamResult<StatValue> {
        Err(self.unavailable())
    }
    fn set_stat(&mut self, _name: &str, _value: StatValue) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn store_stats(&mut self) -> SteamResult<()> {
        Err(self.unavailable())
    }

    // Remote storage (Steam Cloud).
    fn cloud_enabled_for_app(&self) -> SteamResult<bool> {
        Err(self.unavailable())
    }
    fn cloud_enabled_for_account(&self) -> SteamResult<bool> {
        Err(self.unavailable())
    }
    /// True when Cloud is enabled for both the app and the account.
    fn cloud_enabled(&self) -> SteamResult<bool> {
        Ok(self.cloud_enabled_for_app()? && self.cloud_enabled_for_account()?)
    }
    /// Last-modified time of a cloud file, in Unix seconds.
    fn cloud_timestamp(&self, _name: &str) -> SteamResult<i64> {
        Err(self.unavailable())
    }
    fn cloud_list(&self) -> SteamResult<Vec<String>> {
        Err(self.unavailable())
    }
    fn cloud_read(&self, _name: &str) -> SteamResult<Vec<u8>> {
        Err(self.unavailable())
    }
    fn cloud_write(&mut self, _name: &str, _data: &[u8]) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn cloud_delete(&mut self, _name: &str) -> SteamResult<()> {
        Err(self.unavailable())
    }

    // Lobbies.
    fn create_lobby(
        &mut self,
        _visibility: LobbyVisibility,
        _max_members: u32,
    ) -> SteamResult<LobbyId> {
        Err(self.unavailable())
    }
    fn join_lobby(&mut self, _lobby: LobbyId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn leave_lobby(&mut self, _lobby: LobbyId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn list_lobbies(&mut self, _filter: &LobbyFilter) -> SteamResult<Vec<LobbyInfo>> {
        Err(self.unavailable())
    }
    fn set_lobby_data(&mut self, _lobby: LobbyId, _key: &str, _value: &str) -> SteamResult<()> {
        Err(self.unavailable())
    }

    // Workshop (user generated content).
    fn subscribe_item(&mut self, _item: WorkshopItemId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn unsubscribe_item(&mut self, _item: WorkshopItemId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn subscribed_items(&self) -> SteamResult<Vec<WorkshopItemId>> {
        Err(self.unavailable())
    }
    fn installed_item(&self, _item: WorkshopItemId) -> SteamResult<Option<InstalledItem>> {
        Err(self.unavailable())
    }

    // Workshop service (see `UgcService`). Requests are asynchronous and answered through
    // `ugc_poll`; the sync `subscribe_item`/`unsubscribe_item` above are the S1 sketch.
    fn ugc_create_item(&mut self) -> SteamResult<RequestId> {
        Err(self.unavailable())
    }
    /// Updates and submits an item in one step (Steam's start-update then submit).
    fn ugc_submit_item(
        &mut self,
        _item: WorkshopItemId,
        _update: &ItemUpdate,
    ) -> SteamResult<RequestId> {
        Err(self.unavailable())
    }
    /// Progress of an in-flight submission, or `None` when unknown or finished.
    fn ugc_update_progress(&self, _request: RequestId) -> Option<UpdateProgress> {
        None
    }
    fn ugc_subscribe(&mut self, _item: WorkshopItemId, _subscribe: bool) -> SteamResult<RequestId> {
        Err(self.unavailable())
    }
    /// An empty `ById` list is answered with an empty page.
    fn ugc_query(&mut self, _query: &UgcQuery) -> SteamResult<RequestId> {
        Err(self.unavailable())
    }
    fn ugc_item_state(&self, _item: WorkshopItemId) -> SteamResult<ItemState> {
        Err(self.unavailable())
    }
    fn ugc_download(&mut self, _item: WorkshopItemId, _high_priority: bool) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Bytes `(done, total)` of an item that is downloading.
    fn ugc_download_progress(&self, _item: WorkshopItemId) -> Option<(u64, u64)> {
        None
    }
    /// Drains finished requests.
    fn ugc_poll(&mut self) -> Vec<UgcRaw> {
        Vec::new()
    }

    // Steam Input.
    fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
        Err(self.unavailable())
    }
    fn activate_action_set(&mut self, _controller: ControllerId, _set: &str) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Initialises Steam Input for explicit `input_run_frame` calls (idempotent).
    /// `manifest` loads a local action manifest (development; shipped games get theirs
    /// from the app's Steamworks configuration).
    fn input_init(&mut self, _manifest: Option<&std::path::Path>) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Synchronises Steam Input with the latest controller state; call once per tick
    /// before reading `controllers` or action data.
    fn input_run_frame(&mut self) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn input_digital(
        &self,
        _controller: ControllerId,
        _action: &str,
    ) -> SteamResult<DigitalActionState> {
        Err(self.unavailable())
    }
    fn input_analog(
        &self,
        _controller: ControllerId,
        _action: &str,
    ) -> SteamResult<AnalogActionState> {
        Err(self.unavailable())
    }
    /// Display data for the controls an action is bound to within an action set.
    fn input_glyphs(
        &self,
        _controller: ControllerId,
        _set: &str,
        _action: &str,
        _kind: InputActionKind,
    ) -> SteamResult<Vec<ActionGlyph>> {
        Err(self.unavailable())
    }

    /// The lobby surface of this backend, if it has one (see [`crate::SteamLobbies`]).
    fn lobby_backend(&mut self) -> Option<&mut dyn crate::LobbyBackend> {
        None
    }

    // Networking Sockets. Connections and listen sockets are identified by backend-owned
    // ids; `net_poll_events` reports what happened since the last call.

    /// Starts the asynchronous Steam Datagram Relay initialisation (idempotent).
    fn net_init_relay(&mut self) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn net_relay_status(&self) -> SteamResult<RelayStatus> {
        Err(self.unavailable())
    }
    /// Largest message `net_send` accepts.
    fn net_max_message_size(&self) -> SteamResult<usize> {
        Err(self.unavailable())
    }
    /// Listens for `ConnectP2P` peers on a virtual port.
    fn net_listen_p2p(&mut self, _virtual_port: i32) -> SteamResult<NetListenId> {
        Err(self.unavailable())
    }
    /// Listens for `ConnectByIPAddress` clients on a UDP address.
    fn net_listen_ip(&mut self, _addr: SocketAddr) -> SteamResult<NetListenId> {
        Err(self.unavailable())
    }
    fn net_close_listen(&mut self, _listen: NetListenId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn net_connect_p2p(&mut self, _peer: SteamId, _virtual_port: i32) -> SteamResult<NetConnId> {
        Err(self.unavailable())
    }
    fn net_connect_ip(&mut self, _addr: SocketAddr) -> SteamResult<NetConnId> {
        Err(self.unavailable())
    }
    fn net_accept(&mut self, _conn: NetConnId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Closes (or rejects) a connection; `linger` tries to flush queued reliable data first.
    fn net_close(&mut self, _conn: NetConnId, _reason: &str, _linger: bool) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Queues one message. `SendQueueFull` means Steam's send buffer is full.
    fn net_send(
        &mut self,
        _conn: NetConnId,
        _data: &[u8],
        _flags: NetSendFlags,
    ) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Sends anything held back by the Nagle timer.
    fn net_flush(&mut self, _conn: NetConnId) -> SteamResult<()> {
        Err(self.unavailable())
    }
    /// Dequeues up to `max` received messages.
    fn net_receive(&mut self, _conn: NetConnId, _max: usize) -> SteamResult<Vec<Vec<u8>>> {
        Err(self.unavailable())
    }
    fn net_poll_events(&mut self) -> Vec<NetEvent> {
        Vec::new()
    }
    fn net_conn_stats(&self, _conn: NetConnId) -> Option<NetConnStats> {
        None
    }
    /// Starts validating a remote user's auth session ticket; the verdict arrives as
    /// `NetEvent::AuthResult`.
    fn net_begin_auth(&mut self, _user: SteamId, _ticket: &[u8]) -> SteamResult<()> {
        Err(self.unavailable())
    }
    fn net_end_auth(&mut self, _user: SteamId) -> SteamResult<()> {
        Err(self.unavailable())
    }
}

/// Steam Cloud file names are relative, `/`-separated and may not escape upward.
pub(crate) fn valid_cloud_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
