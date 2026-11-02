//! Deterministic, scriptable in-memory backend for tests of later stages.
//!
//! It is always compiled. Tests script state with the `with_*`/`push_event`
//! methods and inspect the recorded call order through [`FakeBackend::calls`].

use std::collections::{BTreeMap, BTreeSet};

use crate::backend::SteamBackend;
use crate::error::{SteamError, SteamResult};
use crate::types::*;

/// In-memory Steam double.
#[derive(Debug, Clone)]
pub struct FakeBackend {
    status: SteamStatus,
    calls: Vec<String>,
    events: Vec<SteamEvent>,
    restart_required: bool,
    next_ticket: u32,
    rich_presence: BTreeMap<String, String>,
    achievements: BTreeMap<String, bool>,
    stats: BTreeMap<String, StatValue>,
    stored_stats: BTreeMap<String, StatValue>,
    stored_achievements: BTreeMap<String, bool>,
    cloud_enabled: bool,
    cloud_quota: usize,
    cloud: BTreeMap<String, Vec<u8>>,
    lobbies: BTreeMap<LobbyId, LobbyInfo>,
    next_lobby: u64,
    subscribed: BTreeSet<WorkshopItemId>,
    installed: BTreeMap<WorkshopItemId, InstalledItem>,
    controllers: Vec<ControllerId>,
    action_sets: BTreeMap<ControllerId, String>,
}

impl FakeBackend {
    /// A ready backend for the given app and user.
    pub fn ready(app_id: AppId, user_name: &str) -> Self {
        Self::with_status(SteamStatus::Ready {
            app_id,
            user: SteamUser {
                id: SteamId(76_561_198_000_000_001),
                name: user_name.to_owned(),
            },
        })
    }

    /// A backend with an explicit status (use a non-ready one to script absence).
    pub fn with_status(status: SteamStatus) -> Self {
        Self {
            status,
            calls: Vec::new(),
            events: Vec::new(),
            restart_required: false,
            next_ticket: 1,
            rich_presence: BTreeMap::new(),
            achievements: BTreeMap::new(),
            stats: BTreeMap::new(),
            stored_stats: BTreeMap::new(),
            stored_achievements: BTreeMap::new(),
            cloud_enabled: true,
            cloud_quota: usize::MAX,
            cloud: BTreeMap::new(),
            lobbies: BTreeMap::new(),
            next_lobby: 1,
            subscribed: BTreeSet::new(),
            installed: BTreeMap::new(),
            controllers: Vec::new(),
            action_sets: BTreeMap::new(),
        }
    }

    /// Changes the status and queues a `StatusChanged` event.
    pub fn set_status(&mut self, status: SteamStatus) {
        self.events.push(SteamEvent::StatusChanged(status.clone()));
        self.status = status;
    }

    /// Makes `restart_app_if_necessary` return `true`.
    pub fn with_restart_required(mut self, required: bool) -> Self {
        self.restart_required = required;
        self
    }

    /// Declares an achievement (locked).
    pub fn with_achievement(mut self, name: &str) -> Self {
        self.achievements.insert(name.to_owned(), false);
        self
    }

    /// Declares a stat with an initial value.
    pub fn with_stat(mut self, name: &str, value: StatValue) -> Self {
        self.stats.insert(name.to_owned(), value);
        self
    }

    /// Enables or disables Steam Cloud.
    pub fn with_cloud_enabled(mut self, enabled: bool) -> Self {
        self.cloud_enabled = enabled;
        self
    }

    /// Caps total cloud bytes.
    pub fn with_cloud_quota(mut self, bytes: usize) -> Self {
        self.cloud_quota = bytes;
        self
    }

    /// Puts a file straight into the cloud store.
    pub fn with_cloud_file(mut self, name: &str, data: &[u8]) -> Self {
        self.cloud.insert(name.to_owned(), data.to_vec());
        self
    }

    /// Registers an installed Workshop item.
    pub fn with_installed_item(mut self, item: InstalledItem) -> Self {
        self.installed.insert(item.id, item);
        self
    }

    /// Plugs in a controller.
    pub fn with_controller(mut self, controller: ControllerId) -> Self {
        self.controllers.push(controller);
        self
    }

    /// Queues an event for the next `run_callbacks`.
    pub fn push_event(&mut self, event: SteamEvent) {
        self.events.push(event);
    }

    /// Recorded call names, in order. Mutating and status-dependent calls are
    /// recorded even when they fail.
    pub fn calls(&self) -> &[String] {
        &self.calls
    }

    /// Number of recorded calls with exactly this name.
    pub fn call_count(&self, name: &str) -> usize {
        self.calls.iter().filter(|c| c.as_str() == name).count()
    }

    /// Rich presence as set by the game.
    pub fn rich_presence(&self) -> &BTreeMap<String, String> {
        &self.rich_presence
    }

    /// Stats as persisted by the last successful `store_stats`.
    pub fn stored_stats(&self) -> &BTreeMap<String, StatValue> {
        &self.stored_stats
    }

    /// Achievements as persisted by the last successful `store_stats`.
    pub fn stored_achievements(&self) -> &BTreeMap<String, bool> {
        &self.stored_achievements
    }

    /// Active action set of a controller.
    pub fn action_set(&self, controller: ControllerId) -> Option<&str> {
        self.action_sets.get(&controller).map(String::as_str)
    }

    fn record(&mut self, name: &str) {
        self.calls.push(name.to_owned());
    }

    fn require_ready(&self) -> SteamResult<()> {
        if self.status.is_ready() {
            Ok(())
        } else {
            Err(self.unavailable())
        }
    }

    fn cloud_bytes(&self) -> usize {
        self.cloud.values().map(Vec::len).sum()
    }
}

fn valid_cloud_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl SteamBackend for FakeBackend {
    fn status(&self) -> SteamStatus {
        self.status.clone()
    }

    fn init(&mut self, _app_id: Option<AppId>) -> SteamStatus {
        self.record("init");
        self.status.clone()
    }

    fn run_callbacks(&mut self) -> Vec<SteamEvent> {
        self.record("run_callbacks");
        std::mem::take(&mut self.events)
    }

    fn restart_app_if_necessary(&mut self, _app_id: AppId) -> SteamResult<bool> {
        self.record("restart_app_if_necessary");
        Ok(self.restart_required)
    }

    fn auth_session_ticket(&mut self) -> SteamResult<AuthTicket> {
        self.record("auth_session_ticket");
        self.require_ready()?;
        let handle = self.next_ticket;
        self.next_ticket += 1;
        Ok(AuthTicket {
            handle,
            data: handle.to_le_bytes().to_vec(),
        })
    }

    fn cancel_auth_ticket(&mut self, _handle: u32) -> SteamResult<()> {
        self.record("cancel_auth_ticket");
        self.require_ready()
    }

    fn activate_overlay(&mut self, _dialog: &str) -> SteamResult<()> {
        self.record("activate_overlay");
        self.require_ready()?;
        self.events.push(SteamEvent::OverlayToggled(true));
        Ok(())
    }

    fn set_rich_presence(&mut self, key: &str, value: Option<&str>) -> SteamResult<()> {
        self.record("set_rich_presence");
        self.require_ready()?;
        match value {
            Some(v) => {
                self.rich_presence.insert(key.to_owned(), v.to_owned());
            }
            None => {
                self.rich_presence.remove(key);
            }
        }
        Ok(())
    }

    fn request_current_stats(&mut self) -> SteamResult<()> {
        self.record("request_current_stats");
        self.require_ready()
    }

    fn achievement(&self, name: &str) -> SteamResult<bool> {
        self.require_ready()?;
        self.achievements
            .get(name)
            .copied()
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn set_achievement(&mut self, name: &str, unlocked: bool) -> SteamResult<()> {
        self.record("set_achievement");
        self.require_ready()?;
        let slot = self
            .achievements
            .get_mut(name)
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))?;
        let newly = unlocked && !*slot;
        *slot = unlocked;
        if newly {
            self.events
                .push(SteamEvent::AchievementUnlocked(name.to_owned()));
        }
        Ok(())
    }

    fn stat(&self, name: &str) -> SteamResult<StatValue> {
        self.require_ready()?;
        self.stats
            .get(name)
            .copied()
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn set_stat(&mut self, name: &str, value: StatValue) -> SteamResult<()> {
        self.record("set_stat");
        self.require_ready()?;
        let slot = self
            .stats
            .get_mut(name)
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))?;
        if std::mem::discriminant(slot) != std::mem::discriminant(&value) {
            return Err(SteamError::Rejected(format!(
                "stat {name} has a different type"
            )));
        }
        *slot = value;
        Ok(())
    }

    fn store_stats(&mut self) -> SteamResult<()> {
        self.record("store_stats");
        self.require_ready()?;
        self.stored_stats = self.stats.clone();
        self.stored_achievements = self.achievements.clone();
        self.events.push(SteamEvent::StatsStored);
        Ok(())
    }

    fn cloud_enabled(&self) -> SteamResult<bool> {
        self.require_ready()?;
        Ok(self.cloud_enabled)
    }

    fn cloud_list(&self) -> SteamResult<Vec<String>> {
        self.require_ready()?;
        Ok(self.cloud.keys().cloned().collect())
    }

    fn cloud_read(&self, name: &str) -> SteamResult<Vec<u8>> {
        self.require_ready()?;
        if !valid_cloud_name(name) {
            return Err(SteamError::InvalidName(name.to_owned()));
        }
        self.cloud
            .get(name)
            .cloned()
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn cloud_write(&mut self, name: &str, data: &[u8]) -> SteamResult<()> {
        self.record("cloud_write");
        self.require_ready()?;
        if !valid_cloud_name(name) {
            return Err(SteamError::InvalidName(name.to_owned()));
        }
        if !self.cloud_enabled {
            return Err(SteamError::CloudDisabled);
        }
        let existing = self.cloud.get(name).map_or(0, Vec::len);
        if self.cloud_bytes() - existing + data.len() > self.cloud_quota {
            return Err(SteamError::QuotaExceeded);
        }
        self.cloud.insert(name.to_owned(), data.to_vec());
        Ok(())
    }

    fn cloud_delete(&mut self, name: &str) -> SteamResult<()> {
        self.record("cloud_delete");
        self.require_ready()?;
        if !valid_cloud_name(name) {
            return Err(SteamError::InvalidName(name.to_owned()));
        }
        self.cloud
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn create_lobby(
        &mut self,
        _visibility: LobbyVisibility,
        max_members: u32,
    ) -> SteamResult<LobbyId> {
        self.record("create_lobby");
        self.require_ready()?;
        let SteamStatus::Ready { user, .. } = &self.status else {
            return Err(self.unavailable());
        };
        let owner = user.id;
        let id = LobbyId(self.next_lobby);
        self.next_lobby += 1;
        self.lobbies.insert(
            id,
            LobbyInfo {
                id,
                owner,
                members: vec![owner],
                max_members,
                data: BTreeMap::new(),
            },
        );
        self.events.push(SteamEvent::LobbyCreated(id));
        Ok(id)
    }

    fn join_lobby(&mut self, lobby: LobbyId) -> SteamResult<()> {
        self.record("join_lobby");
        self.require_ready()?;
        let SteamStatus::Ready { user, .. } = &self.status else {
            return Err(self.unavailable());
        };
        let me = user.id;
        let info = self
            .lobbies
            .get_mut(&lobby)
            .ok_or_else(|| SteamError::NotFound(format!("lobby {}", lobby.0)))?;
        if info.members.contains(&me) {
            return Ok(());
        }
        if info.members.len() >= info.max_members as usize {
            return Err(SteamError::LobbyFull);
        }
        info.members.push(me);
        self.events.push(SteamEvent::LobbyJoined(lobby));
        Ok(())
    }

    fn leave_lobby(&mut self, lobby: LobbyId) -> SteamResult<()> {
        self.record("leave_lobby");
        self.require_ready()?;
        let SteamStatus::Ready { user, .. } = &self.status else {
            return Err(self.unavailable());
        };
        let me = user.id;
        let info = self
            .lobbies
            .get_mut(&lobby)
            .ok_or_else(|| SteamError::NotFound(format!("lobby {}", lobby.0)))?;
        info.members.retain(|m| *m != me);
        if info.members.is_empty() {
            self.lobbies.remove(&lobby);
        } else if info.owner == me {
            info.owner = info.members[0];
        }
        self.events
            .push(SteamEvent::LobbyMemberLeft { lobby, user: me });
        Ok(())
    }

    fn list_lobbies(&mut self, filter: &LobbyFilter) -> SteamResult<Vec<LobbyInfo>> {
        self.record("list_lobbies");
        self.require_ready()?;
        let limit = if filter.max_results == 0 {
            usize::MAX
        } else {
            filter.max_results as usize
        };
        Ok(self
            .lobbies
            .values()
            .filter(|l| {
                filter
                    .string_equals
                    .iter()
                    .all(|(k, v)| l.data.get(k).is_some_and(|d| d == v))
            })
            .take(limit)
            .cloned()
            .collect())
    }

    fn set_lobby_data(&mut self, lobby: LobbyId, key: &str, value: &str) -> SteamResult<()> {
        self.record("set_lobby_data");
        self.require_ready()?;
        let info = self
            .lobbies
            .get_mut(&lobby)
            .ok_or_else(|| SteamError::NotFound(format!("lobby {}", lobby.0)))?;
        info.data.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn subscribe_item(&mut self, item: WorkshopItemId) -> SteamResult<()> {
        self.record("subscribe_item");
        self.require_ready()?;
        self.subscribed.insert(item);
        if self.installed.contains_key(&item) {
            self.events.push(SteamEvent::WorkshopItemInstalled(item));
        }
        Ok(())
    }

    fn unsubscribe_item(&mut self, item: WorkshopItemId) -> SteamResult<()> {
        self.record("unsubscribe_item");
        self.require_ready()?;
        self.subscribed.remove(&item);
        Ok(())
    }

    fn subscribed_items(&self) -> SteamResult<Vec<WorkshopItemId>> {
        self.require_ready()?;
        Ok(self.subscribed.iter().copied().collect())
    }

    fn installed_item(&self, item: WorkshopItemId) -> SteamResult<Option<InstalledItem>> {
        self.require_ready()?;
        Ok(self
            .subscribed
            .contains(&item)
            .then(|| self.installed.get(&item).cloned())
            .flatten())
    }

    fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
        self.require_ready()?;
        Ok(self.controllers.clone())
    }

    fn activate_action_set(&mut self, controller: ControllerId, set: &str) -> SteamResult<()> {
        self.record("activate_action_set");
        self.require_ready()?;
        if !self.controllers.contains(&controller) {
            return Err(SteamError::NotFound(format!("controller {}", controller.0)));
        }
        self.action_sets.insert(controller, set.to_owned());
        Ok(())
    }
}
