//! Deterministic, scriptable in-memory backend for tests of later stages.
//!
//! It is always compiled. Tests script state with the `with_*`/`push_event`
//! methods and inspect the recorded call order through [`FakeBackend::calls`].

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use crate::backend::{SteamBackend, valid_cloud_name};
use crate::error::{SteamError, SteamResult};
use crate::fake_lobby::FakeLobbies;
use crate::fake_net::FakeNet;
use crate::fake_ugc::FakeUgc;
use crate::lobby::{LobbyBackend, LobbyRaw};
use crate::net::*;
use crate::types::*;
use crate::workshop::{
    ItemState, ItemUpdate, RequestId, UgcQuery, UgcRaw, UpdateProgress, WorkshopItem,
};

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
    stats_requested: bool,
    stats_delayed: bool,
    stats_ready: bool,
    cloud_enabled: bool,
    cloud_account_enabled: bool,
    cloud_quota: usize,
    cloud: BTreeMap<String, (Vec<u8>, i64)>,
    clock: i64,
    lobbies: BTreeMap<LobbyId, LobbyInfo>,
    next_lobby: u64,
    subscribed: BTreeSet<WorkshopItemId>,
    installed: BTreeMap<WorkshopItemId, InstalledItem>,
    controllers: Vec<ControllerId>,
    action_sets: BTreeMap<ControllerId, String>,
    ugc: FakeUgc,
    input_ready: bool,
    digital: BTreeMap<(ControllerId, String), bool>,
    analog: BTreeMap<(ControllerId, String), (f32, f32)>,
    set_defs: BTreeMap<String, BTreeSet<String>>,
    glyphs: BTreeMap<(String, String), Vec<ActionGlyph>>,
    canceled_tickets: BTreeSet<u32>,
    net: FakeNet,
    lobby_world: FakeLobbies,
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
            stats_requested: false,
            stats_delayed: false,
            stats_ready: true,
            cloud_enabled: true,
            cloud_account_enabled: true,
            cloud_quota: usize::MAX,
            cloud: BTreeMap::new(),
            clock: 1_000,
            lobbies: BTreeMap::new(),
            next_lobby: 1,
            subscribed: BTreeSet::new(),
            installed: BTreeMap::new(),
            controllers: Vec::new(),
            action_sets: BTreeMap::new(),
            ugc: FakeUgc::default(),
            input_ready: false,
            digital: BTreeMap::new(),
            analog: BTreeMap::new(),
            set_defs: BTreeMap::new(),
            glyphs: BTreeMap::new(),
            canceled_tickets: BTreeSet::new(),
            net: FakeNet::default(),
            lobby_world: FakeLobbies::default(),
        }
    }

    /// Changes the status and queues a `StatusChanged` event.
    pub fn set_status(&mut self, status: SteamStatus) {
        self.events.push(SteamEvent::StatusChanged(status.clone()));
        self.status = status;
    }

    /// The relay network reports `Pending` for the first `polls` calls to `net_poll_events`.
    pub fn with_relay_delay(mut self, polls: u32) -> Self {
        self.net.set_relay_delay(polls);
        self
    }

    /// The relay network reports `Failed(message)`.
    pub fn with_relay_failure(mut self, message: &str) -> Self {
        self.net.set_relay_failure(Some(message.to_owned()));
        self
    }

    /// Caps the bytes a connection may hold unread before `net_send` reports `SendQueueFull`.
    pub fn with_net_queue_limit(mut self, bytes: usize) -> Self {
        self.net.set_queue_limit(bytes);
        self
    }

    /// Lowers the largest accepted message size.
    pub fn with_net_max_message(mut self, bytes: usize) -> Self {
        self.net.set_max_message(bytes);
        self
    }

    /// Makes `BeginAuthSession` verdicts negative.
    pub fn with_auth_rejection(mut self, reject: bool) -> Self {
        self.net.set_reject_auth(reject);
        self
    }

    /// While enabled, unreliable messages are queued ahead of earlier ones (reversed
    /// order), so receivers can be tested against stale data.
    pub fn set_net_reorder(&mut self, enabled: bool) {
        self.net.set_reorder(enabled);
    }

    /// Queues a revocation verdict for an auth session, as Steam does when a user
    /// goes offline.
    pub fn revoke_net_auth(&mut self, user: SteamId) {
        self.net.revoke_auth(user);
    }

    /// Number of networking connections currently open in the model.
    pub fn net_open_connections(&self) -> usize {
        self.net.open_connections()
    }

    /// Number of active `BeginAuthSession` sessions.
    pub fn net_auth_sessions(&self) -> usize {
        self.net.active_auth_sessions()
    }

    /// A lobby owned by a remote player, joinable and listable.
    pub fn sim_add_lobby(
        &mut self,
        owner: SteamId,
        max_members: u32,
        data: &[(&str, &str)],
    ) -> LobbyId {
        let data = data
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        self.lobby_world.sim_add(owner, max_members, data)
    }

    /// A remote player joins a lobby (a change callback reaches the local user).
    pub fn sim_lobby_join(&mut self, lobby: LobbyId, user: SteamId) {
        self.lobby_world.sim_join(lobby, user);
    }

    /// A remote player leaves; ownership migrates to the first remaining member.
    pub fn sim_lobby_leave(&mut self, lobby: LobbyId, user: SteamId) {
        self.lobby_world.sim_leave(lobby, user);
    }

    pub fn sim_lobby_set_owner(&mut self, lobby: LobbyId, user: SteamId) {
        self.lobby_world.sim_set_owner(lobby, user);
    }

    pub fn sim_lobby_set_data(&mut self, lobby: LobbyId, key: &str, value: &str) {
        self.lobby_world.sim_set_data(lobby, key, value);
    }

    /// The lobby disappears (all members gone).
    pub fn sim_lobby_destroy(&mut self, lobby: LobbyId) {
        self.lobby_world.sim_destroy(lobby);
    }

    /// The next `reads` snapshot reads of any lobby come back empty, as a transient
    /// Steam hiccup would.
    pub fn glitch_lobby_info(&mut self, reads: u32) {
        self.lobby_world.glitch_info(reads);
    }

    /// The next `lobby_create` fails with this reason.
    pub fn fail_next_lobby_create(&mut self, reason: &str) {
        self.lobby_world.fail_next_create(reason);
    }

    /// While held, lobby callbacks accumulate instead of being delivered.
    pub fn hold_lobby_callbacks(&mut self, hold: bool) {
        self.lobby_world.set_hold(hold);
    }

    /// Delivers held lobby callbacks, optionally in reverse order.
    pub fn release_lobby_callbacks(&mut self, reversed: bool) {
        self.lobby_world.release(reversed);
    }

    /// Repeats every pending lobby callback once.
    pub fn duplicate_lobby_callbacks(&mut self) {
        self.lobby_world.duplicate_pending();
    }

    /// Makes `restart_app_if_necessary` return `true`.
    pub fn with_restart_required(mut self, required: bool) -> Self {
        self.restart_required = required;
        self
    }

    /// Stats stay `StatsNotReady` after the request until [`FakeBackend::deliver_stats`].
    pub fn with_delayed_stats(mut self) -> Self {
        self.stats_delayed = true;
        self.stats_ready = false;
        self
    }

    /// Delivers the user stats, as Steam does some time after the request.
    pub fn deliver_stats(&mut self) {
        self.stats_ready = true;
        self.events.push(SteamEvent::StatsReceived);
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

    /// Enables or disables Steam Cloud for the account.
    pub fn with_cloud_account_enabled(mut self, enabled: bool) -> Self {
        self.cloud_account_enabled = enabled;
        self
    }

    /// Caps total cloud bytes.
    pub fn with_cloud_quota(mut self, bytes: usize) -> Self {
        self.cloud_quota = bytes;
        self
    }

    /// Puts a file straight into the cloud store.
    pub fn with_cloud_file(mut self, name: &str, data: &[u8]) -> Self {
        self.clock += 1;
        let stamp = self.clock;
        self.cloud.insert(name.to_owned(), (data.to_vec(), stamp));
        self
    }

    /// Puts a file into the cloud store with an explicit Unix timestamp.
    pub fn with_cloud_file_at(mut self, name: &str, data: &[u8], timestamp: i64) -> Self {
        self.cloud
            .insert(name.to_owned(), (data.to_vec(), timestamp));
        self
    }

    /// Contents of a cloud file.
    pub fn cloud_file(&self, name: &str) -> Option<&[u8]> {
        self.cloud.get(name).map(|(data, _)| data.as_slice())
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

    /// Adds an item to the fake Workshop catalog (what queries can find).
    pub fn with_workshop_item(mut self, item: WorkshopItem) -> Self {
        self.ugc.catalog.insert(item.id, item);
        self
    }

    /// Polls a download needs before it completes (default 2).
    pub fn with_download_polls(mut self, polls: u32) -> Self {
        self.ugc.download_polls = polls.max(1);
        self
    }

    /// Finished downloads send no completion reply, only the changed item state.
    pub fn with_silent_downloads(mut self) -> Self {
        self.ugc.silent_downloads = true;
        self
    }

    /// A catalog item as the fake currently holds it.
    pub fn workshop_item(&self, id: WorkshopItemId) -> Option<&WorkshopItem> {
        self.ugc.catalog.get(&id)
    }

    /// Updates the fake applied, in order (a completed submission records its update).
    pub fn applied_updates(&self) -> &[(WorkshopItemId, ItemUpdate)] {
        &self.ugc.applied
    }

    /// Plugs a controller in while running (idempotent).
    pub fn plug_controller(&mut self, controller: ControllerId) {
        if !self.controllers.contains(&controller) {
            self.controllers.push(controller);
        }
    }

    /// Unplugs a controller and forgets its action set and scripted state.
    pub fn unplug_controller(&mut self, controller: ControllerId) {
        self.controllers.retain(|c| *c != controller);
        self.action_sets.remove(&controller);
        self.digital.retain(|(c, _), _| *c != controller);
        self.analog.retain(|(c, _), _| *c != controller);
    }

    /// Scripts a digital action press (`true`) or release.
    pub fn set_digital(&mut self, controller: ControllerId, action: &str, down: bool) {
        self.digital.insert((controller, action.to_owned()), down);
    }

    /// Scripts an analog action value.
    pub fn set_analog(&mut self, controller: ControllerId, action: &str, x: f32, y: f32) {
        self.analog.insert((controller, action.to_owned()), (x, y));
    }

    /// Declares which actions an action set contains. Once any set is declared, an
    /// action reads as inactive unless the controller's active set contains it.
    pub fn with_action_set(mut self, set: &str, actions: &[&str]) -> Self {
        self.set_defs.insert(
            set.to_owned(),
            actions.iter().map(|a| (*a).to_owned()).collect(),
        );
        self
    }

    /// Scripts the origins `input_glyphs` reports for an action within a set.
    pub fn with_action_glyphs(mut self, set: &str, action: &str, glyphs: Vec<ActionGlyph>) -> Self {
        self.glyphs
            .insert((set.to_owned(), action.to_owned()), glyphs);
        self
    }

    fn action_active(&self, controller: ControllerId, action: &str) -> bool {
        if !self.controllers.contains(&controller) {
            return false;
        }
        if self.set_defs.is_empty() {
            return true;
        }
        self.action_sets
            .get(&controller)
            .and_then(|set| self.set_defs.get(set))
            .is_some_and(|actions| actions.contains(action))
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

    fn user_id(&self) -> SteamResult<SteamId> {
        match &self.status {
            SteamStatus::Ready { user, .. } => Ok(user.id),
            _ => Err(self.unavailable()),
        }
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

    fn require_stats(&self) -> SteamResult<()> {
        if !self.stats_requested {
            Err(SteamError::StatsNotRequested)
        } else if !self.stats_ready {
            Err(SteamError::StatsNotReady)
        } else {
            Ok(())
        }
    }

    fn cloud_bytes(&self) -> usize {
        self.cloud.values().map(|(data, _)| data.len()).sum()
    }
}

impl FakeBackend {
    fn local_user(&self) -> SteamResult<SteamId> {
        match &self.status {
            SteamStatus::Ready { user, .. } => Ok(user.id),
            _ => Err(self.unavailable()),
        }
    }
}

impl LobbyBackend for FakeBackend {
    fn lobby_create(&mut self, _visibility: LobbyVisibility, max_members: u32) -> SteamResult<()> {
        self.record("lobby_create");
        let me = self.local_user()?;
        self.lobby_world.create(me, max_members);
        Ok(())
    }

    fn lobby_join(&mut self, lobby: LobbyId) -> SteamResult<()> {
        self.record("lobby_join");
        let me = self.local_user()?;
        self.lobby_world.join(me, lobby);
        Ok(())
    }

    fn lobby_leave(&mut self, lobby: LobbyId) -> SteamResult<()> {
        self.record("lobby_leave");
        let me = self.local_user()?;
        self.lobby_world.leave(me, lobby)
    }

    fn lobby_request_list(&mut self, filter: &LobbyFilter) -> SteamResult<()> {
        self.record("lobby_request_list");
        self.local_user()?;
        self.lobby_world.request_list(filter);
        Ok(())
    }

    fn lobby_set_data(
        &mut self,
        lobby: LobbyId,
        key: &str,
        value: Option<&str>,
    ) -> SteamResult<()> {
        self.record("lobby_set_data");
        let me = self.local_user()?;
        self.lobby_world.set_data(me, lobby, key, value)
    }

    fn lobby_info(&self, lobby: LobbyId) -> Option<LobbyInfo> {
        self.lobby_world.info(lobby)
    }

    fn lobby_poll(&mut self) -> Vec<LobbyRaw> {
        self.lobby_world.poll()
    }
}

impl SteamBackend for FakeBackend {
    fn lobby_backend(&mut self) -> Option<&mut dyn LobbyBackend> {
        Some(self)
    }

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

    fn cancel_auth_ticket(&mut self, handle: u32) -> SteamResult<()> {
        self.record("cancel_auth_ticket");
        self.require_ready()?;
        self.canceled_tickets.insert(handle);
        Ok(())
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
        self.require_ready()?;
        self.stats_requested = true;
        Ok(())
    }

    fn achievement(&self, name: &str) -> SteamResult<bool> {
        self.require_ready()?;
        self.require_stats()?;
        self.achievements
            .get(name)
            .copied()
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn set_achievement(&mut self, name: &str, unlocked: bool) -> SteamResult<()> {
        self.record("set_achievement");
        self.require_ready()?;
        self.require_stats()?;
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
        self.require_stats()?;
        self.stats
            .get(name)
            .copied()
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn set_stat(&mut self, name: &str, value: StatValue) -> SteamResult<()> {
        self.record("set_stat");
        self.require_ready()?;
        self.require_stats()?;
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

    fn cloud_enabled_for_app(&self) -> SteamResult<bool> {
        self.require_ready()?;
        Ok(self.cloud_enabled)
    }

    fn cloud_enabled_for_account(&self) -> SteamResult<bool> {
        self.require_ready()?;
        Ok(self.cloud_account_enabled)
    }

    fn cloud_timestamp(&self, name: &str) -> SteamResult<i64> {
        self.require_ready()?;
        if !valid_cloud_name(name) {
            return Err(SteamError::InvalidName(name.to_owned()));
        }
        self.cloud
            .get(name)
            .map(|(_, stamp)| *stamp)
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
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
            .map(|(data, _)| data.clone())
            .ok_or_else(|| SteamError::NotFound(name.to_owned()))
    }

    fn cloud_write(&mut self, name: &str, data: &[u8]) -> SteamResult<()> {
        self.record("cloud_write");
        self.require_ready()?;
        if !valid_cloud_name(name) {
            return Err(SteamError::InvalidName(name.to_owned()));
        }
        if !self.cloud_enabled || !self.cloud_account_enabled {
            return Err(SteamError::CloudDisabled);
        }
        let existing = self.cloud.get(name).map_or(0, |(data, _)| data.len());
        if self.cloud_bytes() - existing + data.len() > self.cloud_quota {
            return Err(SteamError::QuotaExceeded);
        }
        self.clock += 1;
        self.cloud
            .insert(name.to_owned(), (data.to_vec(), self.clock));
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
        if !self.input_ready {
            // Like the real backend: no controller list before Steam Input starts.
            return Err(SteamError::Rejected(
                "Steam Input is not initialised".into(),
            ));
        }
        Ok(self.controllers.clone())
    }

    fn activate_action_set(&mut self, controller: ControllerId, set: &str) -> SteamResult<()> {
        self.record("activate_action_set");
        self.require_ready()?;
        if !self.controllers.contains(&controller) {
            return Err(SteamError::NotFound(format!("controller {}", controller.0)));
        }
        if !self.set_defs.is_empty() && !self.set_defs.contains_key(set) {
            return Err(SteamError::NotFound(format!("action set {set:?}")));
        }
        self.action_sets.insert(controller, set.to_owned());
        Ok(())
    }

    fn ugc_create_item(&mut self) -> SteamResult<RequestId> {
        self.record("ugc_create_item");
        let owner = self.user_id()?;
        let request = self.ugc.request();
        let id = self.ugc.create(owner);
        self.ugc.replies.push(UgcRaw::Created {
            request,
            result: Ok((id, false)),
        });
        Ok(request)
    }

    fn ugc_submit_item(
        &mut self,
        item: WorkshopItemId,
        update: &ItemUpdate,
    ) -> SteamResult<RequestId> {
        self.record("ugc_submit_item");
        let owner = self.user_id()?;
        let request = self.ugc.request();
        match self.ugc.catalog.get(&item) {
            Some(entry) if entry.owner == owner => {
                self.ugc.updates.insert(request, (item, update.clone(), 0));
            }
            Some(_) => self.ugc.replies.push(UgcRaw::Submitted {
                request,
                result: Err("only the owner can update an item".into()),
            }),
            None => self.ugc.replies.push(UgcRaw::Submitted {
                request,
                result: Err("no such item".into()),
            }),
        }
        Ok(request)
    }

    fn ugc_update_progress(&self, request: RequestId) -> Option<UpdateProgress> {
        self.ugc.progress(request)
    }

    fn ugc_subscribe(&mut self, item: WorkshopItemId, subscribe: bool) -> SteamResult<RequestId> {
        self.record(if subscribe {
            "ugc_subscribe"
        } else {
            "ugc_unsubscribe"
        });
        self.require_ready()?;
        let request = self.ugc.request();
        let known = self.ugc.catalog.contains_key(&item) || self.installed.contains_key(&item);
        let result = if known {
            if subscribe {
                self.subscribed.insert(item);
            } else {
                self.subscribed.remove(&item);
            }
            Ok(())
        } else {
            Err("no such item".to_owned())
        };
        self.ugc.replies.push(UgcRaw::Subscription {
            request,
            item,
            subscribed: subscribe,
            result,
        });
        Ok(request)
    }

    fn ugc_query(&mut self, query: &UgcQuery) -> SteamResult<RequestId> {
        self.record("ugc_query");
        let me = self.user_id()?;
        let request = self.ugc.request();
        let page = self.ugc.query(query, me);
        self.ugc.replies.push(UgcRaw::Queried {
            request,
            result: Ok(page),
        });
        Ok(request)
    }

    fn ugc_item_state(&self, item: WorkshopItemId) -> SteamResult<ItemState> {
        self.require_ready()?;
        let installed = self.installed.get(&item);
        Ok(ItemState {
            subscribed: self.subscribed.contains(&item),
            installed: installed.is_some(),
            needs_update: installed.is_some_and(|i| i.needs_update),
            downloading: self.ugc.downloads.contains_key(&item),
            download_pending: false,
        })
    }

    fn ugc_download(&mut self, item: WorkshopItemId, _high_priority: bool) -> SteamResult<()> {
        self.record("ugc_download");
        self.require_ready()?;
        if !self.subscribed.contains(&item) {
            return Err(SteamError::Rejected("the item is not subscribed".into()));
        }
        let polls = self.ugc.download_polls;
        self.ugc.downloads.insert(item, (polls, polls));
        Ok(())
    }

    fn ugc_download_progress(&self, item: WorkshopItemId) -> Option<(u64, u64)> {
        let (left, polls) = *self.ugc.downloads.get(&item)?;
        let total = self.installed.get(&item).map_or(100, |i| i.size_bytes);
        Some((total * u64::from(polls - left) / u64::from(polls), total))
    }

    fn ugc_forget_update(&mut self, request: RequestId) {
        self.ugc.updates.remove(&request);
    }

    fn ugc_poll(&mut self) -> Vec<UgcRaw> {
        self.ugc.step_updates();
        let mut done = Vec::new();
        for (item, (left, _)) in &mut self.ugc.downloads {
            *left = left.saturating_sub(1);
            if *left == 0 {
                done.push(*item);
            }
        }
        for item in done {
            self.ugc.downloads.remove(&item);
            let result = match self.installed.get_mut(&item) {
                Some(installed) => {
                    installed.needs_update = false;
                    Ok(())
                }
                None => Err("the item has no downloadable content".to_owned()),
            };
            if !self.ugc.silent_downloads {
                self.ugc.replies.push(UgcRaw::Downloaded { item, result });
            }
        }
        std::mem::take(&mut self.ugc.replies)
    }

    fn input_init(&mut self, _manifest: Option<&std::path::Path>) -> SteamResult<()> {
        self.record("input_init");
        self.require_ready()?;
        self.input_ready = true;
        Ok(())
    }

    fn input_run_frame(&mut self) -> SteamResult<()> {
        self.record("input_run_frame");
        self.require_ready()?;
        if self.input_ready {
            Ok(())
        } else {
            Err(SteamError::Rejected(
                "Steam Input is not initialised".into(),
            ))
        }
    }

    fn input_digital(
        &self,
        controller: ControllerId,
        action: &str,
    ) -> SteamResult<DigitalActionState> {
        self.require_ready()?;
        let active = self.action_active(controller, action);
        let down = active
            && self
                .digital
                .get(&(controller, action.to_owned()))
                .copied()
                .unwrap_or(false);
        Ok(DigitalActionState { down, active })
    }

    fn input_analog(
        &self,
        controller: ControllerId,
        action: &str,
    ) -> SteamResult<AnalogActionState> {
        self.require_ready()?;
        let active = self.action_active(controller, action);
        let (x, y) = if active {
            self.analog
                .get(&(controller, action.to_owned()))
                .copied()
                .unwrap_or((0.0, 0.0))
        } else {
            (0.0, 0.0)
        };
        Ok(AnalogActionState { x, y, active })
    }

    fn input_glyphs(
        &self,
        controller: ControllerId,
        set: &str,
        action: &str,
        _kind: InputActionKind,
    ) -> SteamResult<Vec<ActionGlyph>> {
        self.require_ready()?;
        if !self.controllers.contains(&controller) {
            return Err(SteamError::NotFound(format!("controller {}", controller.0)));
        }
        Ok(self
            .glyphs
            .get(&(set.to_owned(), action.to_owned()))
            .cloned()
            .unwrap_or_default())
    }

    fn net_init_relay(&mut self) -> SteamResult<()> {
        self.record("net_init_relay");
        self.require_ready()?;
        self.net.init_relay();
        Ok(())
    }

    fn net_relay_status(&self) -> SteamResult<RelayStatus> {
        self.require_ready()?;
        Ok(self.net.relay_status())
    }

    fn net_max_message_size(&self) -> SteamResult<usize> {
        self.require_ready()?;
        Ok(self.net.max_message())
    }

    fn net_listen_p2p(&mut self, virtual_port: i32) -> SteamResult<NetListenId> {
        self.record("net_listen_p2p");
        self.require_ready()?;
        Ok(self.net.listen_p2p(virtual_port))
    }

    fn net_listen_ip(&mut self, addr: SocketAddr) -> SteamResult<NetListenId> {
        self.record("net_listen_ip");
        self.require_ready()?;
        self.net.listen_ip(addr)
    }

    fn net_close_listen(&mut self, listen: NetListenId) -> SteamResult<()> {
        self.record("net_close_listen");
        self.require_ready()?;
        self.net.close_listen(listen)
    }

    fn net_connect_p2p(&mut self, peer: SteamId, virtual_port: i32) -> SteamResult<NetConnId> {
        self.record("net_connect_p2p");
        let SteamStatus::Ready { user, .. } = &self.status else {
            return Err(self.unavailable());
        };
        let me = user.id;
        self.net.connect_p2p(me, peer, virtual_port)
    }

    fn net_connect_ip(&mut self, addr: SocketAddr) -> SteamResult<NetConnId> {
        self.record("net_connect_ip");
        self.require_ready()?;
        Ok(self.net.connect_ip(addr))
    }

    fn net_accept(&mut self, conn: NetConnId) -> SteamResult<()> {
        self.record("net_accept");
        self.require_ready()?;
        self.net.accept(conn)
    }

    fn net_close(&mut self, conn: NetConnId, reason: &str, _linger: bool) -> SteamResult<()> {
        self.record("net_close");
        self.require_ready()?;
        self.net.close(conn, reason)
    }

    fn net_send(&mut self, conn: NetConnId, data: &[u8], flags: NetSendFlags) -> SteamResult<()> {
        self.require_ready()?;
        self.net.send(conn, data, flags)
    }

    fn net_flush(&mut self, conn: NetConnId) -> SteamResult<()> {
        self.require_ready()?;
        if self.net.conn_stats(conn).is_some() {
            Ok(())
        } else {
            Err(SteamError::ConnectionClosed)
        }
    }

    fn net_receive(&mut self, conn: NetConnId, max: usize) -> SteamResult<Vec<Vec<u8>>> {
        self.require_ready()?;
        self.net.receive(conn, max)
    }

    fn net_poll_events(&mut self) -> Vec<NetEvent> {
        self.net.poll_events()
    }

    fn net_conn_stats(&self, conn: NetConnId) -> Option<NetConnStats> {
        self.net.conn_stats(conn)
    }

    fn net_begin_auth(&mut self, user: SteamId, ticket: &[u8]) -> SteamResult<()> {
        self.record("net_begin_auth");
        self.require_ready()?;
        let valid = <[u8; 4]>::try_from(ticket)
            .map(u32::from_le_bytes)
            .is_ok_and(|h| h >= 1 && h < self.next_ticket && !self.canceled_tickets.contains(&h));
        self.net.begin_auth(user, valid)
    }

    fn net_end_auth(&mut self, user: SteamId) -> SteamResult<()> {
        self.record("net_end_auth");
        self.require_ready()?;
        self.net.end_auth(user);
        Ok(())
    }
}
