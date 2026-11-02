//! The backend trait. Every method defaults to "unavailable" for the backend's
//! current status, so a backend only overrides what it supports.

use crate::error::{SteamError, SteamResult};
use crate::types::*;

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

    // Steam Input.
    fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
        Err(self.unavailable())
    }
    fn activate_action_set(&mut self, _controller: ControllerId, _set: &str) -> SteamResult<()> {
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
