//! Lobby half of the real backend over `steamworks::Matchmaking`.
//!
//! Requests are asynchronous: their replies and Steam's change notifications are pushed
//! into a shared queue by callbacks (run inside `Client::run_callbacks`) and drained by
//! [`LobbyBackend::lobby_poll`]. State is always read back with [`LobbyBackend::lobby_info`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use steamworks::{
    DistanceFilter, LobbyChatUpdate, LobbyDataUpdate, LobbyId as RawLobby, LobbyKey, LobbyType,
    StringFilter, StringFilterKind,
};

use crate::error::{SteamError, SteamResult};
use crate::lobby::{EnterError, LobbyBackend, LobbyRaw, MAX_LOBBY_MEMBERS, RawQueue};
use crate::real::RealBackend;
use crate::types::{LobbyFilter, LobbyId, LobbyInfo, LobbyVisibility, SteamId};

pub(crate) type LobbyEvents = Arc<Mutex<RawQueue>>;

fn push(queue: &LobbyEvents, raw: LobbyRaw) {
    queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(raw);
}

/// Registers the lobby change notifications; replies to requests use closures instead.
pub(crate) fn register(
    client: &steamworks::Client,
    queue: &LobbyEvents,
) -> Vec<steamworks::CallbackHandle> {
    let q = Arc::clone(queue);
    let members = client.register_callback(move |cb: LobbyChatUpdate| {
        push(&q, LobbyRaw::Changed(LobbyId(cb.lobby.raw())));
    });
    let q = Arc::clone(queue);
    let data = client.register_callback(move |cb: LobbyDataUpdate| {
        push(&q, LobbyRaw::Changed(LobbyId(cb.lobby.raw())));
    });
    vec![members, data]
}

impl LobbyBackend for RealBackend {
    fn lobby_create(&mut self, visibility: LobbyVisibility, max_members: u32) -> SteamResult<()> {
        if max_members == 0 || max_members > MAX_LOBBY_MEMBERS {
            return Err(SteamError::Rejected("lobby size out of range".into()));
        }
        let queue = Arc::clone(&self.lobby_events);
        let kind = match visibility {
            LobbyVisibility::Private => LobbyType::Private,
            LobbyVisibility::FriendsOnly => LobbyType::FriendsOnly,
            LobbyVisibility::Public => LobbyType::Public,
            LobbyVisibility::Invisible => LobbyType::Invisible,
        };
        self.client()?
            .matchmaking()
            .create_lobby(kind, max_members, move |result| {
                let raw = match result {
                    Ok(id) => LobbyRaw::Created(Ok(LobbyId(id.raw()))),
                    Err(error) => LobbyRaw::Created(Err(error.to_string())),
                };
                push(&queue, raw);
            });
        Ok(())
    }

    fn lobby_join(&mut self, lobby: LobbyId) -> SteamResult<()> {
        let queue = Arc::clone(&self.lobby_events);
        self.client()?
            .matchmaking()
            .join_lobby(RawLobby::from_raw(lobby.0), move |result| {
                let result = result.map(|_| ()).map_err(|()| {
                    EnterError::Other("Steam refused the join (full, closed or gone)".into())
                });
                push(&queue, LobbyRaw::Entered { lobby, result });
            });
        Ok(())
    }

    fn lobby_leave(&mut self, lobby: LobbyId) -> SteamResult<()> {
        self.client()?
            .matchmaking()
            .leave_lobby(RawLobby::from_raw(lobby.0));
        Ok(())
    }

    fn lobby_request_list(&mut self, filter: &LobbyFilter) -> SteamResult<()> {
        for (key, value) in &filter.string_equals {
            if key.is_empty() || key.len() > 255 || key.contains('\0') || value.contains('\0') {
                return Err(SteamError::InvalidName(key.replace('\0', "\\0")));
            }
        }
        let queue = Arc::clone(&self.lobby_events);
        let mm = self.client()?.matchmaking();
        for (key, value) in &filter.string_equals {
            mm.add_request_lobby_list_string_filter(StringFilter(
                LobbyKey::new(key),
                value,
                StringFilterKind::Equal,
            ));
        }
        if filter.open_slots > 0 {
            mm.set_request_lobby_list_slots_available_filter(filter.open_slots.min(250) as u8);
        }
        if filter.max_results > 0 {
            mm.set_request_lobby_list_result_count_filter(u64::from(filter.max_results));
        }
        mm.set_request_lobby_list_distance_filter(DistanceFilter::Worldwide);
        mm.request_lobby_list(move |result| {
            let raw = match result {
                Ok(ids) => {
                    LobbyRaw::List(Ok(ids.into_iter().map(|id| LobbyId(id.raw())).collect()))
                }
                Err(error) => LobbyRaw::List(Err(error.to_string())),
            };
            push(&queue, raw);
        });
        Ok(())
    }

    fn lobby_set_data(
        &mut self,
        lobby: LobbyId,
        key: &str,
        value: Option<&str>,
    ) -> SteamResult<()> {
        if key.is_empty() || key.contains('\0') || value.is_some_and(|v| v.contains('\0')) {
            return Err(SteamError::InvalidName(key.replace('\0', "\\0")));
        }
        let mm = self.client()?.matchmaking();
        let raw = RawLobby::from_raw(lobby.0);
        let accepted = match value {
            Some(value) => mm.set_lobby_data(raw, key, value),
            None => mm.delete_lobby_data(raw, key),
        };
        if accepted {
            Ok(())
        } else {
            Err(SteamError::Rejected(format!(
                "Steam refused lobby data {key:?} (not the owner, or too large)"
            )))
        }
    }

    fn lobby_info(&self, lobby: LobbyId) -> Option<LobbyInfo> {
        let mm = self.client().ok()?.matchmaking();
        let raw = RawLobby::from_raw(lobby.0);
        let mut data = BTreeMap::new();
        for index in 0..mm.lobby_data_count(raw) {
            if let Some((key, value)) = mm.lobby_data_by_index(raw, index) {
                data.insert(key, value);
            }
        }
        let members: Vec<SteamId> = mm
            .lobby_members(raw)
            .into_iter()
            .map(|id| SteamId(id.raw()))
            .collect();
        let max_members = mm.lobby_member_limit(raw).unwrap_or(0) as u32;
        if members.is_empty() && data.is_empty() && max_members == 0 {
            return None;
        }
        Some(LobbyInfo {
            id: lobby,
            owner: SteamId(mm.lobby_owner(raw).raw()),
            members,
            max_members,
            data,
        })
    }

    fn lobby_poll(&mut self) -> Vec<LobbyRaw> {
        self.lobby_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
    }
}
