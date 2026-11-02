//! Steam lobbies and matchmaking.
//!
//! [`LobbyBackend`] is the raw, asynchronous Steam surface: requests return at once and
//! the outcome arrives later as [`LobbyRaw`] callbacks, in whatever order Steam delivers
//! them. [`SteamLobbies`] turns that into a small state machine with its own
//! [`LobbyEvent`]s. Normalisation rule: raw events only say *that* something changed;
//! the truth is a snapshot read with [`LobbyBackend::lobby_info`], diffed against the
//! last snapshot this crate reported. Duplicate, late or reordered callbacks therefore
//! produce no duplicate and no missing events.
//!
//! The host publishes its P2P connect identity (its SteamID and virtual port) in the
//! lobby metadata; [`SteamLobbies::connect_client`] turns a joined lobby into a
//! [`SteamClientEndpoint`](crate::SteamClientEndpoint).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use bevy_ecs::prelude::Resource;
use engine_network::Error as NetError;

use crate::error::{SteamError, SteamResult};
use crate::session::SteamSession;
use crate::transport::{SteamClientConfig, SteamClientEndpoint};
use crate::types::{LobbyFilter, LobbyId, LobbyInfo, LobbyVisibility, SteamId, SteamStatus};

/// Metadata key holding the host's SteamID (decimal).
pub const HOST_KEY: &str = "engine.p2p_host";
/// Metadata key holding the host's P2P virtual port (decimal).
pub const PORT_KEY: &str = "engine.p2p_port";

/// Steam's lobby limits.
pub const MAX_LOBBY_MEMBERS: u32 = 250;
const MAX_KEY_BYTES: usize = 255;
const MAX_VALUE_BYTES: usize = 8192;
/// Change callbacks held for a lobby this crate does not know yet.
const MAX_EARLY: usize = 64;

/// Why a join was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterError {
    NotFound,
    Full,
    NotAllowed,
    Other(String),
}

impl std::fmt::Display for EnterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("the lobby does not exist"),
            Self::Full => f.write_str("the lobby is full"),
            Self::NotAllowed => f.write_str("joining the lobby is not allowed"),
            Self::Other(why) => f.write_str(why),
        }
    }
}

/// One Steam callback, as delivered (possibly out of order).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LobbyRaw {
    /// Reply to `lobby_create`.
    Created(Result<LobbyId, String>),
    /// Reply to `lobby_join` (also delivered to the creator of a lobby).
    Entered {
        lobby: LobbyId,
        result: Result<(), EnterError>,
    },
    /// Reply to `lobby_request_list`.
    List(Result<Vec<LobbyId>, String>),
    /// Members, owner or metadata of the lobby changed; read `lobby_info` for the truth.
    Changed(LobbyId),
}

/// The Steam lobby surface. All calls are made from the tick thread; requests are
/// asynchronous and answered through [`LobbyBackend::lobby_poll`].
pub trait LobbyBackend {
    fn lobby_create(&mut self, visibility: LobbyVisibility, max_members: u32) -> SteamResult<()>;
    fn lobby_join(&mut self, lobby: LobbyId) -> SteamResult<()>;
    /// Leaves at once; Steam sends no callback for the local user.
    fn lobby_leave(&mut self, lobby: LobbyId) -> SteamResult<()>;
    fn lobby_request_list(&mut self, filter: &LobbyFilter) -> SteamResult<()>;
    /// Sets (`Some`) or deletes (`None`) a lobby metadata key; the owner only.
    fn lobby_set_data(&mut self, lobby: LobbyId, key: &str, value: Option<&str>)
    -> SteamResult<()>;
    /// The current state of a lobby this client can see, or `None`.
    fn lobby_info(&self, lobby: LobbyId) -> Option<LobbyInfo>;
    /// Drains callbacks received since the last call.
    fn lobby_poll(&mut self) -> Vec<LobbyRaw>;
}

/// Normalised lobby notifications. Each is delivered exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LobbyEvent {
    /// The lobby this client created exists and is joined.
    Created {
        lobby: LobbyId,
    },
    CreateFailed {
        reason: String,
    },
    /// A join succeeded.
    Joined {
        lobby: LobbyId,
    },
    JoinFailed {
        lobby: LobbyId,
        reason: EnterError,
    },
    /// Results of `list`, with metadata.
    ListReceived {
        lobbies: Vec<LobbyInfo>,
    },
    ListFailed {
        reason: String,
    },
    MemberJoined {
        lobby: LobbyId,
        user: SteamId,
    },
    MemberLeft {
        lobby: LobbyId,
        user: SteamId,
    },
    OwnerChanged {
        lobby: LobbyId,
        owner: SteamId,
    },
    /// A metadata key changed (`value` is `None` when it was removed).
    DataChanged {
        lobby: LobbyId,
        key: String,
        value: Option<String>,
    },
    /// This client is no longer in the lobby (it left, was removed, or the lobby ended).
    Left {
        lobby: LobbyId,
    },
}

/// The host a joined lobby tells clients to dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectIdentity {
    pub host: SteamId,
    pub virtual_port: i32,
}

/// What this client knows about a lobby it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyView {
    pub id: LobbyId,
    pub owner: SteamId,
    pub members: BTreeSet<SteamId>,
    pub max_members: u32,
    pub data: BTreeMap<String, String>,
}

impl LobbyView {
    fn from_info(info: LobbyInfo) -> Self {
        Self {
            id: info.id,
            owner: info.owner,
            members: info.members.into_iter().collect(),
            max_members: info.max_members,
            data: info.data,
        }
    }
}

/// Lobby state machine over a [`SteamSession`]'s lobby backend.
#[derive(Resource)]
pub struct SteamLobbies {
    session: SteamSession,
    me: SteamId,
    /// Virtual ports to publish for lobbies being created, in request order.
    creating: VecDeque<Option<i32>>,
    joining: BTreeSet<LobbyId>,
    lists: usize,
    joined: BTreeMap<LobbyId, LobbyView>,
    /// Change callbacks for lobbies not known yet; replayed when they become known.
    early: VecDeque<LobbyId>,
    out: Vec<LobbyEvent>,
}

fn unsupported() -> SteamError {
    SteamError::Rejected("this backend has no lobby support".into())
}

fn check_text(key: &str, value: Option<&str>) -> SteamResult<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.contains('\0') {
        return Err(SteamError::InvalidName(key.replace('\0', "\\0")));
    }
    if let Some(value) = value
        && (value.len() > MAX_VALUE_BYTES || value.contains('\0'))
    {
        return Err(SteamError::InvalidName(format!("value of {key:?}")));
    }
    Ok(())
}

impl SteamLobbies {
    /// Lobbies over a ready session.
    pub fn new(session: SteamSession) -> SteamResult<Self> {
        let SteamStatus::Ready { user, .. } = session.status() else {
            return Err(SteamError::Unavailable(
                session
                    .status()
                    .unavailable_reason()
                    .unwrap_or(crate::UnavailableReason::NotRunning),
            ));
        };
        Ok(Self {
            session,
            me: user.id,
            creating: VecDeque::new(),
            joining: BTreeSet::new(),
            lists: 0,
            joined: BTreeMap::new(),
            early: VecDeque::new(),
            out: Vec::new(),
        })
    }

    fn backend<R>(
        &self,
        f: impl FnOnce(&mut dyn LobbyBackend) -> SteamResult<R>,
    ) -> SteamResult<R> {
        self.session.with_backend(|b| match b.lobby_backend() {
            Some(lobby) => f(lobby),
            None => Err(unsupported()),
        })
    }

    /// Creates a lobby; `Created` (or `CreateFailed`) arrives from `poll`.
    pub fn create(&mut self, visibility: LobbyVisibility, max_members: u32) -> SteamResult<()> {
        self.create_inner(visibility, max_members, None)
    }

    /// Creates a lobby and publishes this player as the P2P host on `virtual_port`, so
    /// joiners can use [`Self::connect_client`].
    pub fn create_p2p(
        &mut self,
        visibility: LobbyVisibility,
        max_members: u32,
        virtual_port: i32,
    ) -> SteamResult<()> {
        self.create_inner(visibility, max_members, Some(virtual_port))
    }

    fn create_inner(
        &mut self,
        visibility: LobbyVisibility,
        max_members: u32,
        port: Option<i32>,
    ) -> SteamResult<()> {
        if max_members == 0 || max_members > MAX_LOBBY_MEMBERS {
            return Err(SteamError::Rejected(format!(
                "a lobby holds 1 to {MAX_LOBBY_MEMBERS} members"
            )));
        }
        self.backend(|b| b.lobby_create(visibility, max_members))?;
        self.creating.push_back(port);
        Ok(())
    }

    /// Joins a lobby; `Joined` (or `JoinFailed`) arrives from `poll`. Joining a lobby
    /// this client is already in or joining is a no-op.
    pub fn join(&mut self, lobby: LobbyId) -> SteamResult<()> {
        if self.joined.contains_key(&lobby) || self.joining.contains(&lobby) {
            return Ok(());
        }
        self.backend(|b| b.lobby_join(lobby))?;
        self.joining.insert(lobby);
        Ok(())
    }

    /// Leaves a lobby; `Left` is returned by the next `poll`.
    pub fn leave(&mut self, lobby: LobbyId) -> SteamResult<()> {
        if self.joined.remove(&lobby).is_none() && !self.joining.remove(&lobby) {
            return Err(SteamError::NotFound(format!("lobby {}", lobby.0)));
        }
        let result = self.backend(|b| b.lobby_leave(lobby));
        self.out.push(LobbyEvent::Left { lobby });
        result
    }

    /// Requests a lobby list; `ListReceived` arrives from `poll`.
    pub fn list(&mut self, filter: &LobbyFilter) -> SteamResult<()> {
        for (key, value) in &filter.string_equals {
            check_text(key, Some(value))?;
        }
        self.backend(|b| b.lobby_request_list(filter))?;
        self.lists += 1;
        Ok(())
    }

    /// Sets or removes lobby metadata. Only the owner may; the change is reported by
    /// `DataChanged` once Steam confirms it.
    pub fn set_data(&mut self, lobby: LobbyId, key: &str, value: Option<&str>) -> SteamResult<()> {
        check_text(key, value)?;
        if key == HOST_KEY || key == PORT_KEY {
            return Err(SteamError::InvalidName(format!(
                "{key} is managed by publish_host"
            )));
        }
        self.set_raw(lobby, key, value)
    }

    fn set_raw(&mut self, lobby: LobbyId, key: &str, value: Option<&str>) -> SteamResult<()> {
        let view = self
            .joined
            .get(&lobby)
            .ok_or_else(|| SteamError::NotFound(format!("lobby {}", lobby.0)))?;
        if view.owner != self.me {
            return Err(SteamError::Rejected(
                "only the lobby owner can change lobby data".into(),
            ));
        }
        self.backend(|b| b.lobby_set_data(lobby, key, value))
    }

    /// Publishes this player as the lobby's P2P host (after becoming owner by migration,
    /// call this again so joiners dial the new host).
    pub fn publish_host(&mut self, lobby: LobbyId, virtual_port: i32) -> SteamResult<()> {
        let me = self.me;
        self.set_raw(lobby, HOST_KEY, Some(&me.0.to_string()))?;
        self.set_raw(lobby, PORT_KEY, Some(&virtual_port.to_string()))
    }

    pub fn me(&self) -> SteamId {
        self.me
    }

    /// A joined lobby as last reported.
    pub fn lobby(&self, lobby: LobbyId) -> Option<&LobbyView> {
        self.joined.get(&lobby)
    }

    /// Joined lobbies.
    pub fn joined(&self) -> impl Iterator<Item = &LobbyView> {
        self.joined.values()
    }

    /// The connect identity in a lobby's metadata, only when it names the lobby's current
    /// owner (metadata left by a previous host is stale and ignored).
    pub fn connect_identity(&self, lobby: LobbyId) -> Option<ConnectIdentity> {
        identity_of(self.joined.get(&lobby)?)
    }

    /// Builds a client endpoint that dials the host of a joined lobby.
    pub fn connect_client(
        &self,
        lobby: LobbyId,
        config: SteamClientConfig,
    ) -> engine_network::Result<SteamClientEndpoint> {
        let identity = self
            .connect_identity(lobby)
            .ok_or(NetError::Invalid("the lobby has no current P2P host"))?;
        if identity.host == self.me {
            return Err(NetError::Invalid("this player hosts the lobby"));
        }
        SteamClientEndpoint::connect_p2p(
            self.session.clone(),
            identity.host,
            identity.virtual_port,
            config,
        )
    }

    /// Pumps Steam callbacks into normalised events.
    pub fn poll(&mut self) -> Vec<LobbyEvent> {
        let raws = self
            .session
            .with_backend(|b| b.lobby_backend().map(|l| l.lobby_poll()))
            .unwrap_or_default();
        for raw in raws {
            self.apply(raw);
        }
        std::mem::take(&mut self.out)
    }

    fn snapshot(&self, lobby: LobbyId) -> Option<LobbyView> {
        self.session
            .with_backend(|b| b.lobby_backend().and_then(|l| l.lobby_info(lobby)))
            .map(LobbyView::from_info)
    }

    fn apply(&mut self, raw: LobbyRaw) {
        match raw {
            LobbyRaw::Created(Ok(id)) => {
                let port = self.creating.pop_front().flatten();
                self.enter(id);
                self.out.push(LobbyEvent::Created { lobby: id });
                if let Some(port) = port {
                    let _ = self.publish_host(id, port);
                }
                self.replay(id);
            }
            LobbyRaw::Created(Err(reason)) => {
                self.creating.pop_front();
                self.out.push(LobbyEvent::CreateFailed { reason });
            }
            LobbyRaw::Entered {
                lobby,
                result: Ok(()),
            } => {
                if self.joined.contains_key(&lobby) {
                    return; // The creator is also told it entered; already handled.
                }
                if self.joining.remove(&lobby) {
                    self.enter(lobby);
                    self.out.push(LobbyEvent::Joined { lobby });
                    self.replay(lobby);
                } else if !self.creating.is_empty() {
                    // Arrived before `Created`: keep it for the lobby's creation.
                    self.hold(lobby);
                }
            }
            LobbyRaw::Entered {
                lobby,
                result: Err(reason),
            } => {
                if self.joining.remove(&lobby) {
                    self.out.push(LobbyEvent::JoinFailed { lobby, reason });
                }
            }
            LobbyRaw::List(result) => {
                if self.lists == 0 {
                    return;
                }
                self.lists -= 1;
                match result {
                    Ok(ids) => {
                        let lobbies = ids
                            .into_iter()
                            .filter_map(|id| {
                                self.session.with_backend(|b| {
                                    b.lobby_backend().and_then(|l| l.lobby_info(id))
                                })
                            })
                            .collect();
                        self.out.push(LobbyEvent::ListReceived { lobbies });
                    }
                    Err(reason) => self.out.push(LobbyEvent::ListFailed { reason }),
                }
            }
            LobbyRaw::Changed(lobby) => {
                if self.joined.contains_key(&lobby) {
                    self.refresh(lobby);
                } else if self.joining.contains(&lobby) || !self.creating.is_empty() {
                    self.hold(lobby);
                }
            }
        }
    }

    fn enter(&mut self, lobby: LobbyId) {
        let view = self.snapshot(lobby).unwrap_or(LobbyView {
            id: lobby,
            owner: self.me,
            members: BTreeSet::from([self.me]),
            max_members: 0,
            data: BTreeMap::new(),
        });
        self.joined.insert(lobby, view);
    }

    fn hold(&mut self, lobby: LobbyId) {
        if self.early.len() >= MAX_EARLY {
            self.early.pop_front();
        }
        self.early.push_back(lobby);
    }

    /// Re-applies change callbacks that arrived before the lobby was known.
    fn replay(&mut self, lobby: LobbyId) {
        let held: Vec<LobbyId> = self.early.drain(..).collect();
        for id in held {
            if id == lobby {
                self.refresh(id);
            } else {
                self.early.push_back(id);
            }
        }
    }

    /// Diffs the lobby's current snapshot against what was last reported.
    fn refresh(&mut self, lobby: LobbyId) {
        let Some(old) = self.joined.get(&lobby).cloned() else {
            return;
        };
        let Some(new) = self.snapshot(lobby) else {
            // The lobby no longer exists for us.
            self.joined.remove(&lobby);
            self.out.push(LobbyEvent::Left { lobby });
            return;
        };
        if !new.members.contains(&self.me) {
            self.joined.remove(&lobby);
            self.out.push(LobbyEvent::Left { lobby });
            return;
        }
        for user in new.members.difference(&old.members) {
            self.out
                .push(LobbyEvent::MemberJoined { lobby, user: *user });
        }
        for user in old.members.difference(&new.members) {
            self.out.push(LobbyEvent::MemberLeft { lobby, user: *user });
        }
        if new.owner != old.owner {
            self.out.push(LobbyEvent::OwnerChanged {
                lobby,
                owner: new.owner,
            });
        }
        for (key, value) in &new.data {
            if old.data.get(key) != Some(value) {
                self.out.push(LobbyEvent::DataChanged {
                    lobby,
                    key: key.clone(),
                    value: Some(value.clone()),
                });
            }
        }
        for key in old.data.keys().filter(|k| !new.data.contains_key(*k)) {
            self.out.push(LobbyEvent::DataChanged {
                lobby,
                key: key.clone(),
                value: None,
            });
        }
        self.joined.insert(lobby, new);
    }
}

fn identity_of(view: &LobbyView) -> Option<ConnectIdentity> {
    let host = SteamId(view.data.get(HOST_KEY)?.parse().ok()?);
    let virtual_port = view.data.get(PORT_KEY)?.parse().ok()?;
    (host == view.owner).then_some(ConnectIdentity { host, virtual_port })
}

/// Identity published by a lobby listing (before joining), when it names the owner.
pub fn listed_identity(info: &LobbyInfo) -> Option<ConnectIdentity> {
    let host = SteamId(info.data.get(HOST_KEY)?.parse().ok()?);
    let virtual_port = info.data.get(PORT_KEY)?.parse().ok()?;
    // Listings may not expose the owner; the join re-checks it against the owner.
    Some(ConnectIdentity { host, virtual_port })
}
