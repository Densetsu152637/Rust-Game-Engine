//! Deterministic lobby model for [`crate::FakeBackend`]: asynchronous replies queued as
//! [`LobbyRaw`] callbacks, remote players scripted with `sim_*`, and delivery control
//! (hold, then release in order or reversed) to exercise out-of-order callbacks.

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};

use crate::error::{SteamError, SteamResult};
use crate::lobby::{EnterError, LobbyRaw};
use crate::types::{LobbyFilter, LobbyId, LobbyInfo, SteamId};

#[derive(Debug, Clone)]
struct Room {
    owner: SteamId,
    members: Vec<SteamId>,
    max: u32,
    data: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct FakeLobbies {
    rooms: BTreeMap<LobbyId, Room>,
    next_id: u64,
    raw: VecDeque<LobbyRaw>,
    held: Vec<LobbyRaw>,
    hold: bool,
    fail_create: Option<String>,
    /// Number of upcoming `info` reads that fail (transient empty reads).
    glitches: Cell<u32>,
}

impl Default for FakeLobbies {
    fn default() -> Self {
        Self {
            rooms: BTreeMap::new(),
            next_id: 109_775_240_000_000_001,
            raw: VecDeque::new(),
            held: Vec::new(),
            hold: false,
            fail_create: None,
            glitches: Cell::new(0),
        }
    }
}

fn not_found(lobby: LobbyId) -> SteamError {
    SteamError::NotFound(format!("lobby {}", lobby.0))
}

impl FakeLobbies {
    fn push(&mut self, raw: LobbyRaw) {
        if self.hold {
            self.held.push(raw);
        } else {
            self.raw.push_back(raw);
        }
    }

    pub(crate) fn create(&mut self, me: SteamId, max: u32) {
        if let Some(reason) = self.fail_create.take() {
            self.push(LobbyRaw::Created(Err(reason)));
            return;
        }
        let id = LobbyId(self.next_id);
        self.next_id += 1;
        self.rooms.insert(
            id,
            Room {
                owner: me,
                members: vec![me],
                max,
                data: BTreeMap::new(),
            },
        );
        self.push(LobbyRaw::Created(Ok(id)));
        self.push(LobbyRaw::Entered {
            lobby: id,
            result: Ok(()),
        });
    }

    pub(crate) fn join(&mut self, me: SteamId, lobby: LobbyId) {
        let result = match self.rooms.get_mut(&lobby) {
            None => Err(EnterError::NotFound),
            Some(room) if room.members.contains(&me) => Ok(()),
            Some(room) if room.members.len() >= room.max as usize => Err(EnterError::Full),
            Some(room) => {
                room.members.push(me);
                Ok(())
            }
        };
        let joined = result.is_ok();
        self.push(LobbyRaw::Entered { lobby, result });
        if joined {
            self.push(LobbyRaw::Changed(lobby));
        }
    }

    pub(crate) fn leave(&mut self, me: SteamId, lobby: LobbyId) -> SteamResult<()> {
        self.remove_member(lobby, me, false)
    }

    fn remove_member(&mut self, lobby: LobbyId, user: SteamId, notify: bool) -> SteamResult<()> {
        let room = self.rooms.get_mut(&lobby).ok_or_else(|| not_found(lobby))?;
        room.members.retain(|m| *m != user);
        if room.members.is_empty() {
            self.rooms.remove(&lobby);
        } else if room.owner == user {
            room.owner = room.members[0];
        }
        if notify {
            self.push(LobbyRaw::Changed(lobby));
        }
        Ok(())
    }

    pub(crate) fn request_list(&mut self, filter: &LobbyFilter) {
        let limit = if filter.max_results == 0 {
            usize::MAX
        } else {
            filter.max_results as usize
        };
        let ids = self
            .rooms
            .iter()
            .filter(|(_, r)| {
                filter
                    .string_equals
                    .iter()
                    .all(|(k, v)| r.data.get(k).is_some_and(|d| d == v))
                    && (r.max as usize).saturating_sub(r.members.len())
                        >= filter.open_slots as usize
            })
            .map(|(id, _)| *id)
            .take(limit)
            .collect();
        self.push(LobbyRaw::List(Ok(ids)));
    }

    pub(crate) fn set_data(
        &mut self,
        me: SteamId,
        lobby: LobbyId,
        key: &str,
        value: Option<&str>,
    ) -> SteamResult<()> {
        let room = self.rooms.get_mut(&lobby).ok_or_else(|| not_found(lobby))?;
        if room.owner != me {
            return Err(SteamError::Rejected("not the lobby owner".into()));
        }
        match value {
            Some(v) => room.data.insert(key.to_owned(), v.to_owned()),
            None => room.data.remove(key),
        };
        self.push(LobbyRaw::Changed(lobby));
        Ok(())
    }

    pub(crate) fn info(&self, lobby: LobbyId) -> Option<LobbyInfo> {
        if self.glitches.get() > 0 {
            self.glitches.set(self.glitches.get() - 1);
            return None;
        }
        self.rooms.get(&lobby).map(|r| LobbyInfo {
            id: lobby,
            owner: r.owner,
            members: r.members.clone(),
            max_members: r.max,
            data: r.data.clone(),
        })
    }

    pub(crate) fn poll(&mut self) -> Vec<LobbyRaw> {
        self.raw.drain(..).collect()
    }

    // ---- scripting ----

    pub(crate) fn glitch_info(&self, reads: u32) {
        self.glitches.set(reads);
    }

    pub(crate) fn fail_next_create(&mut self, reason: &str) {
        self.fail_create = Some(reason.to_owned());
    }

    pub(crate) fn sim_add(
        &mut self,
        owner: SteamId,
        max: u32,
        data: BTreeMap<String, String>,
    ) -> LobbyId {
        let id = LobbyId(self.next_id);
        self.next_id += 1;
        self.rooms.insert(
            id,
            Room {
                owner,
                members: vec![owner],
                max,
                data,
            },
        );
        id
    }

    pub(crate) fn sim_join(&mut self, lobby: LobbyId, user: SteamId) {
        if let Some(room) = self.rooms.get_mut(&lobby)
            && !room.members.contains(&user)
            && room.members.len() < room.max as usize
        {
            room.members.push(user);
            self.push(LobbyRaw::Changed(lobby));
        }
    }

    pub(crate) fn sim_leave(&mut self, lobby: LobbyId, user: SteamId) {
        let _ = self.remove_member(lobby, user, true);
    }

    pub(crate) fn sim_set_owner(&mut self, lobby: LobbyId, user: SteamId) {
        if let Some(room) = self.rooms.get_mut(&lobby)
            && room.members.contains(&user)
        {
            room.owner = user;
            self.push(LobbyRaw::Changed(lobby));
        }
    }

    pub(crate) fn sim_set_data(&mut self, lobby: LobbyId, key: &str, value: &str) {
        if let Some(room) = self.rooms.get_mut(&lobby) {
            room.data.insert(key.to_owned(), value.to_owned());
            self.push(LobbyRaw::Changed(lobby));
        }
    }

    pub(crate) fn sim_destroy(&mut self, lobby: LobbyId) {
        if self.rooms.remove(&lobby).is_some() {
            self.push(LobbyRaw::Changed(lobby));
        }
    }

    pub(crate) fn set_hold(&mut self, hold: bool) {
        self.hold = hold;
    }

    pub(crate) fn release(&mut self, reversed: bool) {
        let mut held = std::mem::take(&mut self.held);
        if reversed {
            held.reverse();
        }
        self.raw.extend(held);
    }

    /// Queues a duplicate of every pending callback (Steam may repeat notifications).
    pub(crate) fn duplicate_pending(&mut self) {
        let copy: Vec<LobbyRaw> = self.raw.iter().cloned().collect();
        self.raw.extend(copy);
    }
}
