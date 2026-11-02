//! Optional adapter (feature `online`) from [`LobbyEvent`]s to the `engine-online`
//! notification vocabulary, so game code written against `engine_online::ServerBody`
//! can follow a Steam lobby the same way it follows a self-hosted session.
//!
//! Mapping: the session id is the low 53 bits of the lobby id (the protocol keeps ids
//! exactly representable as JSON numbers; Steam lobby ids are larger). A player is its
//! decimal SteamID. Steam reports only that a member is gone, so every departure is
//! `LeaveReason::Left`. Host changes raise the epoch by one. Created, joined, list and
//! failure events have no notification form and map to `None`: they answer requests the
//! game made, and the game already holds the typed result. There are no join tokens or
//! fingerprints because Steam authenticates the link itself.

use std::collections::BTreeMap;

use engine_online::protocol::{LeaveReason, MemberView, ServerBody};

use crate::lobby::{HOST_KEY, LobbyEvent, PORT_KEY, SteamLobbies};
use crate::types::{LobbyId, SteamId};

/// Stateful converter (it tracks the host epoch per lobby).
#[derive(Debug, Default)]
pub struct OnlineAdapter {
    epochs: BTreeMap<LobbyId, u64>,
}

impl OnlineAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `engine-online` session id for a lobby.
    pub fn session_id(lobby: LobbyId) -> u64 {
        lobby.0 & ((1 << 53) - 1)
    }

    /// The `engine-online` player name for a Steam user.
    pub fn player(user: SteamId) -> String {
        user.0.to_string()
    }

    /// Converts one lobby event; `lobbies` supplies the current view for context.
    pub fn convert(&mut self, lobbies: &SteamLobbies, event: &LobbyEvent) -> Option<ServerBody> {
        match event {
            LobbyEvent::MemberJoined { lobby, user } => Some(ServerBody::MemberJoined {
                session_id: Self::session_id(*lobby),
                member: MemberView {
                    player: Self::player(*user),
                    ready: false,
                    host: lobbies.lobby(*lobby).is_some_and(|v| v.owner == *user),
                    team: None,
                    candidates: None,
                },
            }),
            LobbyEvent::MemberLeft { lobby, user } => Some(ServerBody::MemberLeft {
                session_id: Self::session_id(*lobby),
                player: Self::player(*user),
                reason: LeaveReason::Left,
            }),
            LobbyEvent::OwnerChanged { lobby, owner } => {
                let epoch = self.epochs.entry(*lobby).or_insert(0);
                *epoch += 1;
                Some(ServerBody::HostChanged {
                    session_id: Self::session_id(*lobby),
                    epoch: *epoch,
                    host: Self::player(*owner),
                    you_are_host: *owner == lobbies.me(),
                    candidates: None,
                    host_fingerprint: None,
                    join_token: None,
                    state: None,
                    code: None,
                })
            }
            LobbyEvent::DataChanged { lobby, .. } => {
                let view = lobbies.lobby(*lobby)?;
                let attributes = view
                    .data
                    .iter()
                    .filter(|(k, _)| k.as_str() != HOST_KEY && k.as_str() != PORT_KEY)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                Some(ServerBody::AttributesChanged {
                    session_id: Self::session_id(*lobby),
                    epoch: self.epochs.get(lobby).copied().unwrap_or(0),
                    attributes,
                })
            }
            LobbyEvent::Left { lobby } => {
                self.epochs.remove(lobby);
                Some(ServerBody::Left {
                    session_id: Self::session_id(*lobby),
                })
            }
            _ => None,
        }
    }
}
