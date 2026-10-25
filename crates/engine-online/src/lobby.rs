//! Lobby / session browser core (sans-IO). Peers are identified by transport [`PeerId`]; the
//! caller supplies the authenticated subject. Every operation validates ownership, epoch and
//! bounds; notifications accumulate in an outbox the caller drains with [`Lobby::drain_events`].
use crate::{
    limits::OnlineLimits,
    migration::{Candidate, InMemoryMigrationState, MigrationState, elect_host},
    protocol::{
        Attributes, Blob, CreateSession, Hosting, JoinTarget, LeaveReason, MemberView, ServerBody,
        SessionFilter, SessionId, SessionSummary, SessionView,
    },
};
use engine_network::PeerId;
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
};
use subtle::ConstantTimeEq;

/// Characters in a private join code: 16 symbols from a 32-letter alphabet (80 bits).
pub const CODE_LEN: usize = 16;
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyError {
    /// Unknown session, not a member, wrong code, or a private session addressed by id.
    NotFound,
    Forbidden,
    Full,
    Conflict,
    LimitExceeded,
    StaleEpoch,
    Invalid(&'static str),
    Internal,
}

/// The authenticated caller of a lobby operation.
#[derive(Clone, Debug)]
pub struct Actor {
    pub peer: PeerId,
    pub subject: String,
}

/// A notification and the peers it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct LobbyEvent {
    pub recipients: Vec<PeerId>,
    pub body: ServerBody,
}

/// One member of a matchmade session.
#[derive(Clone, Debug)]
pub struct MatchedMember {
    pub peer: PeerId,
    pub subject: String,
    pub team: u8,
    pub candidates: Vec<u8>,
    pub rtt_ms: Option<u32>,
}
/// A session created by matchmaking.
#[derive(Clone, Debug)]
pub struct MatchedSession {
    pub mode: String,
    pub region: String,
    pub hosting: Hosting,
    pub members: Vec<MatchedMember>,
}

struct Member {
    subject: String,
    ready: bool,
    candidates: Vec<u8>,
    join_seq: u64,
    rtt_ms: Option<u32>,
    team: Option<u8>,
}
struct Session {
    name: String,
    mode: String,
    capacity: u16,
    private: bool,
    code: Option<[u8; CODE_LEN]>,
    hosting: Hosting,
    host: Option<PeerId>,
    epoch: u64,
    attributes: Attributes,
    members: BTreeMap<PeerId, Member>,
}
impl Session {
    fn host_subject(&self) -> Option<String> {
        self.host
            .and_then(|h| self.members.get(&h))
            .map(|m| m.subject.clone())
    }
    fn member_view(&self, peer: PeerId, member: &Member) -> MemberView {
        MemberView {
            player: member.subject.clone(),
            ready: member.ready,
            host: self.host == Some(peer),
            team: member.team,
            candidates: blob(&member.candidates),
        }
    }
    fn code_text(&self) -> Option<String> {
        self.code
            .as_ref()
            .map(|c| c.iter().map(|&b| char::from(b)).collect())
    }
}
fn blob(bytes: &[u8]) -> Option<Blob> {
    (!bytes.is_empty()).then(|| Blob(bytes.to_vec()))
}

pub struct Lobby {
    limits: OnlineLimits,
    sessions: BTreeMap<SessionId, Session>,
    memberships: BTreeMap<PeerId, BTreeSet<SessionId>>,
    next_id: SessionId,
    join_seq: u64,
    rng: SystemRandom,
    state: Box<dyn MigrationState>,
    events: Vec<LobbyEvent>,
}
impl Lobby {
    pub fn new(limits: OnlineLimits) -> Self {
        let state =
            InMemoryMigrationState::new(limits.max_migration_state_bytes, limits.max_sessions);
        Self::with_migration_state(limits, Box::new(state))
    }
    pub fn with_migration_state(limits: OnlineLimits, state: Box<dyn MigrationState>) -> Self {
        Self {
            limits,
            sessions: BTreeMap::new(),
            memberships: BTreeMap::new(),
            next_id: 1,
            join_seq: 0,
            rng: SystemRandom::new(),
            state,
            events: Vec::new(),
        }
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
    pub fn is_member(&self, peer: PeerId, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|s| s.members.contains_key(&peer))
    }
    pub fn epoch(&self, session: SessionId) -> Option<u64> {
        self.sessions.get(&session).map(|s| s.epoch)
    }
    pub fn host(&self, session: SessionId) -> Option<PeerId> {
        self.sessions.get(&session).and_then(|s| s.host)
    }
    pub fn membership_count(&self, peer: PeerId) -> usize {
        self.memberships.get(&peer).map_or(0, BTreeSet::len)
    }
    pub fn has_capacity_for_session(&self) -> bool {
        self.sessions.len() < self.limits.max_sessions
    }
    /// Members of a session as `(peer, subject)`, in peer order.
    pub fn members(&self, session: SessionId) -> Vec<(PeerId, String)> {
        self.sessions.get(&session).map_or_else(Vec::new, |s| {
            s.members
                .iter()
                .map(|(&p, m)| (p, m.subject.clone()))
                .collect()
        })
    }
    pub fn drain_events(&mut self) -> Vec<LobbyEvent> {
        std::mem::take(&mut self.events)
    }

    /// Session view for `viewer`; the join code is included only for the current host.
    pub fn view(&self, session: SessionId, viewer: Option<PeerId>) -> Option<SessionView> {
        let s = self.sessions.get(&session)?;
        Some(SessionView {
            id: session,
            name: s.name.clone(),
            mode: s.mode.clone(),
            capacity: s.capacity,
            private: s.private,
            hosting: s.hosting,
            epoch: s.epoch,
            host: s.host_subject(),
            attributes: s.attributes.clone(),
            members: s
                .members
                .iter()
                .map(|(&p, m)| s.member_view(p, m))
                .collect(),
            code: if viewer.is_some() && viewer == s.host {
                s.code_text()
            } else {
                None
            },
        })
    }

    fn random_code(&self) -> Result<[u8; CODE_LEN], LobbyError> {
        for _ in 0..8 {
            let mut raw = [0u8; CODE_LEN];
            self.rng.fill(&mut raw).map_err(|_| LobbyError::Internal)?;
            let code = raw.map(|b| ALPHABET[usize::from(b & 31)]);
            if !self.sessions.values().any(|s| s.code == Some(code)) {
                return Ok(code);
            }
        }
        Err(LobbyError::Internal)
    }

    fn reserve_membership(&self, peer: PeerId) -> Result<(), LobbyError> {
        if self.membership_count(peer) >= self.limits.max_memberships_per_peer {
            return Err(LobbyError::LimitExceeded);
        }
        Ok(())
    }
    fn next_join_seq(&mut self) -> u64 {
        self.join_seq += 1;
        self.join_seq
    }

    pub fn create(&mut self, who: &Actor, spec: &CreateSession) -> Result<SessionId, LobbyError> {
        if spec.capacity == 0 || spec.capacity > self.limits.max_capacity {
            return Err(LobbyError::Invalid("capacity"));
        }
        if self.sessions.len() >= self.limits.max_sessions {
            return Err(LobbyError::LimitExceeded);
        }
        self.reserve_membership(who.peer)?;
        let code = if spec.private {
            Some(self.random_code()?)
        } else {
            None
        };
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(LobbyError::Internal)?;
        let join_seq = self.next_join_seq();
        let mut members = BTreeMap::new();
        members.insert(
            who.peer,
            Member {
                subject: who.subject.clone(),
                ready: false,
                candidates: spec
                    .candidates
                    .as_ref()
                    .map(|b| b.0.clone())
                    .unwrap_or_default(),
                join_seq,
                rtt_ms: None,
                team: None,
            },
        );
        self.sessions.insert(
            id,
            Session {
                name: spec.name.clone(),
                mode: spec.mode.clone(),
                capacity: spec.capacity,
                private: spec.private,
                code,
                hosting: Hosting::Player,
                host: Some(who.peer),
                epoch: 1,
                attributes: spec.attributes.clone(),
                members,
            },
        );
        self.memberships.entry(who.peer).or_default().insert(id);
        Ok(id)
    }

    /// Creates a private, code-less session for matched players. Nothing is created unless every
    /// member can be admitted.
    pub fn create_matched(&mut self, matched: &MatchedSession) -> Result<SessionId, LobbyError> {
        let count = matched.members.len();
        if count == 0 || count > usize::from(self.limits.max_capacity) {
            return Err(LobbyError::Invalid("match size"));
        }
        if self.sessions.len() >= self.limits.max_sessions {
            return Err(LobbyError::LimitExceeded);
        }
        let mut peers = BTreeSet::new();
        for member in &matched.members {
            if !peers.insert(member.peer) {
                return Err(LobbyError::Conflict);
            }
            self.reserve_membership(member.peer)?;
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(LobbyError::Internal)?;
        let host = match matched.hosting {
            Hosting::Dedicated => None,
            Hosting::Player => matched
                .members
                .iter()
                .enumerate()
                .min_by_key(|(index, m)| (m.rtt_ms.unwrap_or(u32::MAX), *index))
                .map(|(_, m)| m.peer),
        };
        let mut members = BTreeMap::new();
        for member in &matched.members {
            let join_seq = self.next_join_seq();
            members.insert(
                member.peer,
                Member {
                    subject: member.subject.clone(),
                    ready: false,
                    candidates: member.candidates.clone(),
                    join_seq,
                    rtt_ms: member.rtt_ms,
                    team: Some(member.team),
                },
            );
            self.memberships.entry(member.peer).or_default().insert(id);
        }
        self.sessions.insert(
            id,
            Session {
                name: matched.mode.clone(),
                mode: matched.mode.clone(),
                capacity: count as u16,
                private: true,
                code: None,
                hosting: matched.hosting,
                host,
                epoch: 1,
                attributes: Attributes::from([("region".to_owned(), matched.region.clone())]),
                members,
            },
        );
        Ok(id)
    }

    /// Public sessions in ascending ID order after `after`, at most `limit`, plus the cursor for
    /// the next page. Private sessions are never listed.
    pub fn list(
        &self,
        filter: &SessionFilter,
        after: Option<SessionId>,
        limit: usize,
    ) -> (Vec<SessionSummary>, Option<SessionId>) {
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut out = Vec::new();
        let mut more = false;
        for (&id, s) in self.sessions.range((start, Bound::Unbounded)) {
            if s.private
                || filter.mode.as_ref().is_some_and(|m| *m != s.mode)
                || filter
                    .name_prefix
                    .as_ref()
                    .is_some_and(|p| !s.name.starts_with(p.as_str()))
                || (filter.has_space && s.members.len() >= usize::from(s.capacity))
                || filter
                    .attributes
                    .iter()
                    .any(|(k, v)| s.attributes.get(k) != Some(v))
            {
                continue;
            }
            if out.len() == limit {
                more = true;
                break;
            }
            out.push(SessionSummary {
                id,
                name: s.name.clone(),
                mode: s.mode.clone(),
                capacity: s.capacity,
                members: s.members.len() as u16,
                hosting: s.hosting,
                host: s.host_subject(),
                attributes: s.attributes.clone(),
            });
        }
        let next = if more { out.last().map(|s| s.id) } else { None };
        (out, next)
    }

    /// Resolves a join code without an early exit: every private session's code is compared in
    /// constant time, so response time does not reveal how much of a guess was right.
    fn find_by_code(&self, code: &[u8; CODE_LEN]) -> Option<SessionId> {
        let mut found = None;
        for (&id, session) in &self.sessions {
            if let Some(stored) = &session.code
                && bool::from(stored.as_slice().ct_eq(code.as_slice()))
            {
                found = Some(id);
            }
        }
        found
    }

    pub fn join(
        &mut self,
        who: &Actor,
        target: &JoinTarget,
        candidates: Option<&Blob>,
    ) -> Result<SessionId, LobbyError> {
        let id = match target {
            JoinTarget::Id { session_id } => {
                let session = self.sessions.get(session_id).ok_or(LobbyError::NotFound)?;
                if session.private {
                    // Indistinguishable from a missing session: no existence oracle.
                    return Err(LobbyError::NotFound);
                }
                *session_id
            }
            JoinTarget::Code { code } => {
                let code = normalize_code(code).ok_or(LobbyError::NotFound)?;
                self.find_by_code(&code).ok_or(LobbyError::NotFound)?
            }
        };
        self.reserve_membership(who.peer)?;
        let join_seq = self.next_join_seq();
        let session = self.sessions.get_mut(&id).ok_or(LobbyError::NotFound)?;
        if session.members.contains_key(&who.peer) {
            return Err(LobbyError::Conflict);
        }
        if session.members.len() >= usize::from(session.capacity) {
            return Err(LobbyError::Full);
        }
        if session.members.values().any(|m| m.subject == who.subject) {
            return Err(LobbyError::Conflict);
        }
        let recipients: Vec<PeerId> = session.members.keys().copied().collect();
        let member = Member {
            subject: who.subject.clone(),
            ready: false,
            candidates: candidates.map(|b| b.0.clone()).unwrap_or_default(),
            join_seq,
            rtt_ms: None,
            team: None,
        };
        let view = session.member_view(who.peer, &member);
        session.members.insert(who.peer, member);
        self.memberships.entry(who.peer).or_default().insert(id);
        self.events.push(LobbyEvent {
            recipients,
            body: ServerBody::MemberJoined {
                session_id: id,
                member: view,
            },
        });
        Ok(id)
    }

    pub fn leave(&mut self, peer: PeerId, session: SessionId) -> Result<(), LobbyError> {
        if !self.is_member(peer, session) {
            return Err(LobbyError::NotFound);
        }
        self.remove_member(session, peer, LeaveReason::Left);
        Ok(())
    }

    /// Removes the peer from every session (disconnect), migrating hosts as needed.
    pub fn peer_gone(&mut self, peer: PeerId) {
        let sessions: Vec<SessionId> = self
            .memberships
            .get(&peer)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        for session in sessions {
            self.remove_member(session, peer, LeaveReason::Disconnected);
        }
        self.memberships.remove(&peer);
    }

    fn remove_member(&mut self, id: SessionId, peer: PeerId, reason: LeaveReason) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let Some(member) = session.members.remove(&peer) else {
            return;
        };
        if let Some(set) = self.memberships.get_mut(&peer) {
            set.remove(&id);
            if set.is_empty() {
                self.memberships.remove(&peer);
            }
        }
        if session.members.is_empty() {
            self.sessions.remove(&id);
            self.state.discard(id);
            return;
        }
        let was_host = session.host == Some(peer);
        let hosting = session.hosting;
        if was_host {
            session.host = None;
        }
        self.events.push(LobbyEvent {
            recipients: session.members.keys().copied().collect(),
            body: ServerBody::MemberLeft {
                session_id: id,
                player: member.subject,
                reason,
            },
        });
        if was_host && hosting == Hosting::Player {
            // Cannot fail: at least one member remains and none is excluded.
            let _ = self.migrate(id, None);
        }
    }

    /// Elects a new host, advances the epoch and notifies every member. `exclude` removes the
    /// outgoing host from the election when it is still connected.
    fn migrate(&mut self, id: SessionId, exclude: Option<PeerId>) -> Result<(), LobbyError> {
        let session = self.sessions.get_mut(&id).ok_or(LobbyError::NotFound)?;
        if session.hosting != Hosting::Player {
            return Err(LobbyError::Invalid("session has no player host"));
        }
        let candidates: Vec<Candidate> = session
            .members
            .iter()
            .filter(|(peer, _)| Some(**peer) != exclude)
            .map(|(&peer, m)| Candidate {
                peer,
                rtt_ms: m.rtt_ms,
                join_seq: m.join_seq,
            })
            .collect();
        let new_host = elect_host(&candidates).ok_or(LobbyError::Conflict)?;
        let epoch = session.epoch.checked_add(1).ok_or(LobbyError::Internal)?;
        session.epoch = epoch;
        session.host = Some(new_host);
        let host_member = session.members.get(&new_host).ok_or(LobbyError::Internal)?;
        let host_subject = host_member.subject.clone();
        let host_candidates = blob(&host_member.candidates);
        let state = self.state.restore(id, epoch);
        let code = session.code_text();
        for &peer in session.members.keys() {
            let you_are_host = peer == new_host;
            self.events.push(LobbyEvent {
                recipients: vec![peer],
                body: ServerBody::HostChanged {
                    session_id: id,
                    epoch,
                    host: host_subject.clone(),
                    you_are_host,
                    candidates: host_candidates.clone(),
                    join_token: None,
                    state: if you_are_host {
                        state.clone().map(Blob)
                    } else {
                        None
                    },
                    code: if you_are_host { code.clone() } else { None },
                },
            });
        }
        Ok(())
    }

    /// Operator hook: depose the current host (for example an unresponsive one that is still
    /// connected) and elect another member. The deposed host stays a member at the old epoch,
    /// so its host-only requests fail with [`LobbyError::StaleEpoch`].
    pub fn force_migrate(&mut self, id: SessionId) -> Result<(), LobbyError> {
        let host = self.sessions.get(&id).ok_or(LobbyError::NotFound)?.host;
        self.migrate(id, host)
    }

    fn member_session(&mut self, peer: PeerId, id: SessionId) -> Result<&mut Session, LobbyError> {
        let session = self.sessions.get_mut(&id).ok_or(LobbyError::NotFound)?;
        if !session.members.contains_key(&peer) {
            return Err(LobbyError::NotFound);
        }
        Ok(session)
    }
    /// Member, then epoch (so a deposed host learns why), then host check.
    fn host_session(
        &mut self,
        peer: PeerId,
        id: SessionId,
        epoch: u64,
    ) -> Result<&mut Session, LobbyError> {
        let session = self.member_session(peer, id)?;
        if epoch != session.epoch {
            return Err(LobbyError::StaleEpoch);
        }
        if session.host != Some(peer) {
            return Err(LobbyError::Forbidden);
        }
        Ok(session)
    }

    pub fn set_ready(
        &mut self,
        peer: PeerId,
        id: SessionId,
        ready: bool,
    ) -> Result<(), LobbyError> {
        let session = self.member_session(peer, id)?;
        let member = session.members.get_mut(&peer).ok_or(LobbyError::NotFound)?;
        member.ready = ready;
        let player = member.subject.clone();
        let recipients: Vec<PeerId> = session
            .members
            .keys()
            .copied()
            .filter(|&p| p != peer)
            .collect();
        self.events.push(LobbyEvent {
            recipients,
            body: ServerBody::MemberReady {
                session_id: id,
                player,
                ready,
            },
        });
        Ok(())
    }

    pub fn set_candidates(
        &mut self,
        peer: PeerId,
        id: SessionId,
        candidates: &Blob,
    ) -> Result<(), LobbyError> {
        if candidates.0.len() > self.limits.max_candidates_bytes {
            return Err(LobbyError::Invalid("candidates size"));
        }
        let session = self.member_session(peer, id)?;
        let member = session.members.get_mut(&peer).ok_or(LobbyError::NotFound)?;
        member.candidates = candidates.0.clone();
        let player = member.subject.clone();
        let recipients: Vec<PeerId> = session
            .members
            .keys()
            .copied()
            .filter(|&p| p != peer)
            .collect();
        self.events.push(LobbyEvent {
            recipients,
            body: ServerBody::MemberCandidates {
                session_id: id,
                player,
                candidates: candidates.clone(),
            },
        });
        Ok(())
    }

    pub fn kick(
        &mut self,
        peer: PeerId,
        id: SessionId,
        epoch: u64,
        player: &str,
    ) -> Result<(), LobbyError> {
        let session = self.host_session(peer, id, epoch)?;
        let target = session
            .members
            .iter()
            .find(|(_, m)| m.subject == player)
            .map(|(&p, _)| p)
            .ok_or(LobbyError::NotFound)?;
        if target == peer {
            return Err(LobbyError::Invalid("the host cannot kick itself"));
        }
        self.events.push(LobbyEvent {
            recipients: vec![target],
            body: ServerBody::Kicked { session_id: id },
        });
        self.remove_member(id, target, LeaveReason::Kicked);
        Ok(())
    }

    pub fn set_attributes(
        &mut self,
        peer: PeerId,
        id: SessionId,
        epoch: u64,
        attributes: &Attributes,
    ) -> Result<(), LobbyError> {
        let session = self.host_session(peer, id, epoch)?;
        session.attributes = attributes.clone();
        let recipients: Vec<PeerId> = session.members.keys().copied().collect();
        let epoch = session.epoch;
        self.events.push(LobbyEvent {
            recipients,
            body: ServerBody::AttributesChanged {
                session_id: id,
                epoch,
                attributes: attributes.clone(),
            },
        });
        Ok(())
    }

    /// Stores state from the current host (current epoch only) via the [`MigrationState`] hook.
    pub fn upload_state(
        &mut self,
        peer: PeerId,
        id: SessionId,
        epoch: u64,
        bytes: &[u8],
    ) -> Result<(), LobbyError> {
        if bytes.len() > self.limits.max_migration_state_bytes {
            return Err(LobbyError::Invalid("state size"));
        }
        self.host_session(peer, id, epoch)?;
        if self.state.accept(id, epoch, bytes) {
            Ok(())
        } else {
            Err(LobbyError::Invalid("state rejected"))
        }
    }

    /// Records the transport-measured RTT between `peer` and the service for election.
    pub fn set_rtt(&mut self, peer: PeerId, rtt_ms: u32) {
        let Some(ids) = self.memberships.get(&peer) else {
            return;
        };
        for id in ids {
            if let Some(member) = self
                .sessions
                .get_mut(id)
                .and_then(|s| s.members.get_mut(&peer))
            {
                member.rtt_ms = Some(rtt_ms);
            }
        }
    }

    /// Structural self-check used by tests and fuzzing: returns the first violated invariant.
    pub fn check_invariants(&self) -> Result<(), String> {
        if self.sessions.len() > self.limits.max_sessions {
            return Err("too many sessions".into());
        }
        let mut codes = BTreeSet::new();
        for (id, s) in &self.sessions {
            if s.members.is_empty() {
                return Err(format!("session {id} is empty"));
            }
            if s.members.len() > usize::from(s.capacity) {
                return Err(format!("session {id} over capacity"));
            }
            match (s.hosting, s.host) {
                (Hosting::Player, Some(h)) if s.members.contains_key(&h) => {}
                (Hosting::Dedicated, None) => {}
                _ => return Err(format!("session {id} has an invalid host")),
            }
            if let Some(code) = s.code
                && !codes.insert(code)
            {
                return Err(format!("duplicate code in session {id}"));
            }
            let mut subjects = BTreeSet::new();
            for (peer, m) in &s.members {
                if !subjects.insert(&m.subject) {
                    return Err(format!("duplicate subject in session {id}"));
                }
                if !self
                    .memberships
                    .get(peer)
                    .is_some_and(|set| set.contains(id))
                {
                    return Err(format!("membership index missing for peer {peer}"));
                }
            }
        }
        for (peer, set) in &self.memberships {
            if set.is_empty() || set.len() > self.limits.max_memberships_per_peer {
                return Err(format!("peer {peer} membership count"));
            }
            for id in set {
                if !self.is_member(*peer, *id) {
                    return Err(format!("stale membership {peer}/{id}"));
                }
            }
        }
        Ok(())
    }
}

/// Uppercases and validates a client-supplied join code.
fn normalize_code(text: &str) -> Option<[u8; CODE_LEN]> {
    let bytes = text.as_bytes();
    if bytes.len() != CODE_LEN {
        return None;
    }
    let mut code = [0u8; CODE_LEN];
    for (slot, &b) in code.iter_mut().zip(bytes) {
        let upper = b.to_ascii_uppercase();
        if !ALPHABET.contains(&upper) {
            return None;
        }
        *slot = upper;
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(peer: PeerId, name: &str) -> Actor {
        Actor {
            peer,
            subject: name.into(),
        }
    }
    fn spec(name: &str, private: bool, capacity: u16) -> CreateSession {
        CreateSession {
            name: name.into(),
            mode: "duel".into(),
            capacity,
            private,
            attributes: Attributes::from([("map".into(), "dust".into())]),
            candidates: Some(Blob(vec![peer_byte(name)])),
        }
    }
    fn peer_byte(name: &str) -> u8 {
        name.bytes().next().unwrap_or(0)
    }
    fn lobby() -> Lobby {
        Lobby::new(OnlineLimits::default())
    }
    fn by_id(id: SessionId) -> JoinTarget {
        JoinTarget::Id { session_id: id }
    }
    fn code_of(lobby: &Lobby, id: SessionId, host: PeerId) -> String {
        lobby.view(id, Some(host)).unwrap().code.unwrap()
    }

    #[test]
    fn create_list_filter_page_and_hide_private_sessions() {
        let mut l = lobby();
        let ids: Vec<SessionId> = (1..=6)
            .map(|p| {
                l.create(
                    &actor(p, &format!("p{p}")),
                    &spec(&format!("room{p}"), p % 3 == 0, 4),
                )
                .unwrap_or_else(|e| panic!("{e:?}"))
            })
            .collect();
        // Peers may only host one session each (default membership cap = 1).
        assert_eq!(
            l.create(&actor(1, "p1"), &spec("again", false, 2)),
            Err(LobbyError::LimitExceeded)
        );
        let (all, next) = l.list(&SessionFilter::default(), None, 25);
        assert_eq!(
            all.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2, 4, 5]
        );
        assert_eq!(next, None);
        // Paging by cursor.
        let (page, next) = l.list(&SessionFilter::default(), None, 3);
        assert_eq!((page.len(), next), (3, Some(4)));
        let (page, next) = l.list(&SessionFilter::default(), next, 3);
        assert_eq!(
            (page.iter().map(|s| s.id).collect::<Vec<_>>(), next),
            (vec![5], None)
        );
        let filter = SessionFilter {
            name_prefix: Some("room4".into()),
            ..SessionFilter::default()
        };
        assert_eq!(l.list(&filter, None, 5).0.len(), 1);
        let filter = SessionFilter {
            attributes: Attributes::from([("map".into(), "other".into())]),
            ..SessionFilter::default()
        };
        assert!(l.list(&filter, None, 5).0.is_empty());
        let filter = SessionFilter {
            mode: Some("ffa".into()),
            ..SessionFilter::default()
        };
        assert!(l.list(&filter, None, 5).0.is_empty());
        assert_eq!(ids.len(), 6);
        l.check_invariants().unwrap();
    }

    #[test]
    fn has_space_filter_and_capacity_are_enforced() {
        let mut l = lobby();
        let id = l.create(&actor(1, "a"), &spec("r", false, 2)).unwrap();
        l.join(&actor(2, "b"), &by_id(id), None).unwrap();
        let filter = SessionFilter {
            has_space: true,
            ..SessionFilter::default()
        };
        assert!(l.list(&filter, None, 5).0.is_empty());
        assert_eq!(
            l.join(&actor(3, "c"), &by_id(id), None),
            Err(LobbyError::Full)
        );
        l.leave(2, id).unwrap();
        assert_eq!(l.list(&filter, None, 5).0.len(), 1);
        assert_eq!(l.leave(2, id), Err(LobbyError::NotFound));
        l.check_invariants().unwrap();
    }

    #[test]
    fn private_sessions_join_only_by_code_and_do_not_leak_existence() {
        let mut l = lobby();
        let id = l
            .create(&actor(1, "host"), &spec("secret", true, 4))
            .unwrap();
        let code = code_of(&l, id, 1);
        assert_eq!(code.len(), CODE_LEN);
        // By id: indistinguishable from a session that does not exist.
        assert_eq!(
            l.join(&actor(2, "b"), &by_id(id), None),
            Err(LobbyError::NotFound)
        );
        assert_eq!(
            l.join(&actor(2, "b"), &by_id(999), None),
            Err(LobbyError::NotFound)
        );
        // Wrong, malformed and truncated codes.
        let bad_codes = [
            String::new(),
            "AAAA".to_owned(),
            "A".repeat(CODE_LEN),
            code[..CODE_LEN - 1].to_owned(),
            format!("{code}A"),
        ];
        for bad in bad_codes {
            let target = JoinTarget::Code { code: bad.clone() };
            assert_eq!(
                l.join(&actor(2, "b"), &target, None),
                Err(LobbyError::NotFound),
                "{bad}"
            );
        }
        // Only the host's view carries the code.
        assert!(l.view(id, Some(2)).unwrap().code.is_none());
        assert!(l.view(id, None).unwrap().code.is_none());
        // Lowercase input is accepted and normalised.
        let target = JoinTarget::Code {
            code: code.to_lowercase(),
        };
        assert_eq!(l.join(&actor(2, "b"), &target, None), Ok(id));
        l.check_invariants().unwrap();
    }

    #[test]
    fn join_codes_are_unique_high_entropy_and_use_the_alphabet() {
        let mut l = Lobby::new(OnlineLimits {
            max_sessions: 600,
            ..OnlineLimits::default()
        });
        let mut seen = BTreeSet::new();
        for p in 1..=500u64 {
            let id = l
                .create(&actor(p, &format!("p{p}")), &spec("r", true, 2))
                .unwrap();
            let code = code_of(&l, id, p);
            assert!(code.bytes().all(|b| ALPHABET.contains(&b)));
            assert!(seen.insert(code));
        }
    }

    #[test]
    fn join_rejects_duplicates_and_enforces_membership_limits() {
        let mut l = lobby();
        let a = l.create(&actor(1, "a"), &spec("r1", false, 4)).unwrap();
        let b = l.create(&actor(2, "b"), &spec("r2", false, 4)).unwrap();
        // Peer 3 can join one session, not two.
        l.join(&actor(3, "c"), &by_id(a), None).unwrap();
        assert_eq!(
            l.join(&actor(3, "c"), &by_id(b), None),
            Err(LobbyError::LimitExceeded)
        );
        // Joining the same session twice, or a second peer with the same subject, conflicts.
        assert_eq!(
            l.join(&actor(3, "c"), &by_id(a), None),
            Err(LobbyError::LimitExceeded)
        );
        assert_eq!(
            l.join(&actor(4, "c"), &by_id(a), None),
            Err(LobbyError::Conflict)
        );
        l.check_invariants().unwrap();
    }

    #[test]
    fn host_only_operations_reject_members_and_stale_epochs() {
        let mut l = lobby();
        let id = l.create(&actor(1, "host"), &spec("r", false, 4)).unwrap();
        l.join(&actor(2, "bob"), &by_id(id), None).unwrap();
        l.join(&actor(3, "cat"), &by_id(id), None).unwrap();
        let epoch = l.epoch(id).unwrap();
        // A member cannot kick, set attributes or upload migration state.
        assert_eq!(l.kick(2, id, epoch, "cat"), Err(LobbyError::Forbidden));
        assert_eq!(
            l.set_attributes(2, id, epoch, &Attributes::new()),
            Err(LobbyError::Forbidden)
        );
        assert_eq!(
            l.upload_state(2, id, epoch, b"x"),
            Err(LobbyError::Forbidden)
        );
        // Non-members learn nothing.
        assert_eq!(l.kick(9, id, epoch, "cat"), Err(LobbyError::NotFound));
        // Stale epoch beats the role check.
        assert_eq!(l.kick(1, id, epoch + 1, "cat"), Err(LobbyError::StaleEpoch));
        assert_eq!(l.kick(1, id, epoch, "nobody"), Err(LobbyError::NotFound));
        assert!(matches!(
            l.kick(1, id, epoch, "host"),
            Err(LobbyError::Invalid(_))
        ));
        l.drain_events();
        l.kick(1, id, epoch, "cat").unwrap();
        let events = l.drain_events();
        assert!(events.iter().any(|e| e.recipients == vec![3]
            && matches!(e.body, ServerBody::Kicked { session_id } if session_id == id)));
        assert!(events.iter().any(|e| matches!(
            &e.body,
            ServerBody::MemberLeft { reason: LeaveReason::Kicked, player, .. } if player == "cat"
        )));
        assert!(!l.is_member(3, id));
        l.set_attributes(
            1,
            id,
            epoch,
            &Attributes::from([("map".into(), "ice".into())]),
        )
        .unwrap();
        assert_eq!(l.view(id, None).unwrap().attributes["map"], "ice");
        l.check_invariants().unwrap();
    }

    #[test]
    fn candidates_and_ready_are_per_member_and_broadcast() {
        let mut l = lobby();
        let id = l.create(&actor(1, "host"), &spec("r", false, 4)).unwrap();
        l.join(&actor(2, "bob"), &by_id(id), Some(&Blob(vec![7, 7])))
            .unwrap();
        l.drain_events();
        l.set_candidates(2, id, &Blob(vec![9; 100])).unwrap();
        l.set_ready(2, id, true).unwrap();
        let events = l.drain_events();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.recipients == vec![1]));
        let view = l.view(id, Some(1)).unwrap();
        let bob = view.members.iter().find(|m| m.player == "bob").unwrap();
        assert!(bob.ready);
        assert_eq!(bob.candidates, Some(Blob(vec![9; 100])));
        assert_eq!(
            l.set_candidates(
                2,
                id,
                &Blob(vec![0; OnlineLimits::default().max_candidates_bytes + 1])
            ),
            Err(LobbyError::Invalid("candidates size"))
        );
    }

    #[test]
    fn host_loss_elects_lowest_rtt_then_earliest_join_and_fences_the_epoch() {
        let mut l = lobby();
        let id = l.create(&actor(1, "host"), &spec("r", false, 8)).unwrap();
        for (peer, name) in [(2, "slow"), (3, "fast"), (4, "also_fast")] {
            l.join(
                &actor(peer, name),
                &by_id(id),
                Some(&Blob(vec![peer as u8; 4])),
            )
            .unwrap();
        }
        l.set_rtt(2, 120);
        l.set_rtt(3, 25);
        l.set_rtt(4, 25);
        assert_eq!(l.upload_state(1, id, 1, b"world-state-v7"), Ok(()));
        l.drain_events();

        l.peer_gone(1);
        assert_eq!(l.host(id), Some(3), "tie on RTT broken by earlier join");
        assert_eq!(l.epoch(id), Some(2));
        let events = l.drain_events();
        let changed: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.body {
                ServerBody::HostChanged {
                    epoch,
                    host,
                    you_are_host,
                    candidates,
                    state,
                    ..
                } => Some((
                    e.recipients.clone(),
                    *epoch,
                    host.clone(),
                    *you_are_host,
                    candidates.clone(),
                    state.clone(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(changed.len(), 3, "every remaining member is told");
        for (recipients, epoch, host, you, candidates, state) in &changed {
            assert_eq!((*epoch, host.as_str()), (2, "fast"));
            assert_eq!(*you, recipients == &vec![3]);
            assert_eq!(*candidates, Some(Blob(vec![3; 4])));
            // Only the new host receives the migrated state.
            assert_eq!(state.is_some(), *you);
        }
        assert!(
            changed
                .iter()
                .any(|c| c.5 == Some(Blob(b"world-state-v7".to_vec())))
        );
        l.check_invariants().unwrap();
    }

    #[test]
    fn deposed_host_is_fenced_by_the_epoch() {
        let mut l = lobby();
        let id = l
            .create(&actor(1, "old_host"), &spec("r", true, 4))
            .unwrap();
        l.join(&actor(2, "next"), &by_id(id), None).unwrap_err(); // private: by id fails
        let code = code_of(&l, id, 1);
        l.join(&actor(2, "next"), &JoinTarget::Code { code }, None)
            .unwrap();
        l.set_rtt(1, 10);
        l.set_rtt(2, 50);
        l.force_migrate(id).unwrap();
        assert_eq!(l.host(id), Some(2));
        assert_eq!(l.epoch(id), Some(2));
        assert!(l.is_member(1, id), "deposed host is still a member");
        // Every host-only action by the old host with its old epoch is stale...
        assert_eq!(l.kick(1, id, 1, "next"), Err(LobbyError::StaleEpoch));
        assert_eq!(
            l.set_attributes(1, id, 1, &Attributes::new()),
            Err(LobbyError::StaleEpoch)
        );
        assert_eq!(
            l.upload_state(1, id, 1, b"stale"),
            Err(LobbyError::StaleEpoch)
        );
        // ...and even with the new epoch it is merely a member.
        assert_eq!(l.kick(1, id, 2, "next"), Err(LobbyError::Forbidden));
        assert_eq!(
            l.upload_state(1, id, 2, b"forged"),
            Err(LobbyError::Forbidden)
        );
        // The new host works, and sees the join code.
        assert_eq!(l.upload_state(2, id, 2, b"fresh"), Ok(()));
        assert!(l.view(id, Some(2)).unwrap().code.is_some());
        assert!(l.view(id, Some(1)).unwrap().code.is_none());
        // A sole member cannot be force-migrated away from itself.
        let solo = l.create(&actor(5, "solo"), &spec("s", false, 2)).unwrap();
        assert_eq!(l.force_migrate(solo), Err(LobbyError::Conflict));
        assert_eq!(l.epoch(solo), Some(1));
        l.check_invariants().unwrap();
    }

    #[test]
    fn last_member_leaving_closes_the_session_and_discards_state() {
        let mut l = lobby();
        let id = l.create(&actor(1, "a"), &spec("r", false, 2)).unwrap();
        l.upload_state(1, id, 1, b"x").unwrap();
        l.peer_gone(1);
        assert_eq!(l.session_count(), 0);
        let again = l.create(&actor(1, "a"), &spec("r", false, 2)).unwrap();
        assert!(again > id, "session ids are never reused");
        l.check_invariants().unwrap();
    }

    #[test]
    fn matched_sessions_assign_teams_and_pick_a_low_rtt_host_or_none() {
        let members = |hosting| MatchedSession {
            mode: "duel".into(),
            region: "eu".into(),
            hosting,
            members: vec![
                MatchedMember {
                    peer: 1,
                    subject: "a".into(),
                    team: 0,
                    candidates: vec![],
                    rtt_ms: Some(80),
                },
                MatchedMember {
                    peer: 2,
                    subject: "b".into(),
                    team: 1,
                    candidates: vec![1],
                    rtt_ms: Some(20),
                },
            ],
        };
        let mut l = lobby();
        let id = l.create_matched(&members(Hosting::Player)).unwrap();
        assert_eq!(l.host(id), Some(2));
        let view = l.view(id, None).unwrap();
        assert_eq!(
            view.members.iter().map(|m| m.team).collect::<Vec<_>>(),
            vec![Some(0), Some(1)]
        );
        assert!(view.private && view.code.is_none());
        let dedicated = l.create_matched(&members(Hosting::Dedicated));
        // Peers 1 and 2 already hold their one membership.
        assert_eq!(dedicated, Err(LobbyError::LimitExceeded));
        l.peer_gone(1);
        l.peer_gone(2);
        let id = l.create_matched(&members(Hosting::Dedicated)).unwrap();
        assert_eq!(l.host(id), None);
        // Dedicated sessions have no host to migrate to and cannot be force-migrated.
        assert!(l.force_migrate(id).is_err());
        assert_eq!(l.kick(1, id, 1, "b"), Err(LobbyError::Forbidden));
        l.peer_gone(1);
        assert_eq!(l.epoch(id), Some(1), "no migration for dedicated sessions");
        l.check_invariants().unwrap();
    }

    #[test]
    fn session_count_is_bounded() {
        let mut l = Lobby::new(OnlineLimits {
            max_sessions: 3,
            ..OnlineLimits::default()
        });
        for p in 1..=3 {
            l.create(&actor(p, &format!("p{p}")), &spec("r", false, 2))
                .unwrap();
        }
        assert_eq!(
            l.create(&actor(4, "p4"), &spec("r", false, 2)),
            Err(LobbyError::LimitExceeded)
        );
        assert!(!l.has_capacity_for_session());
    }
}
