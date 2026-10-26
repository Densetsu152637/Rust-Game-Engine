//! The sans-IO online service: admission gate + lobby + matchmaking + token issuance.
//!
//! Feed it transport events (`on_connected`, `on_message`, `on_disconnected`, `tick`) with explicit
//! time and drain [`Action`]s to send or disconnect. It never touches a socket or a clock, so the
//! whole control plane is deterministic under test; [`crate::OnlineServer`] drives it over any
//! [`engine_network::MessageEndpoint`].
use crate::{
    OnlineError,
    gate::{GateConfig, GateTimeout, Now, SessionGate},
    limiter::{FailureWindow, TokenBucket},
    limits::OnlineLimits,
    lobby::{Actor, Lobby, LobbyError, MatchedMember, MatchedSession},
    matchmaking::{MatchError, Matchmaker, ModeRules},
    migration::{InMemoryMigrationState, MigrationState},
    protocol::{
        Blob, ClientBody, ClientEnvelope, ErrorCode, JoinTarget, PROTOCOL_VERSION, ProtocolError,
        ServerBody, ServerEnvelope, SessionId, TicketId, TokenKind,
    },
    relay::{relay_channel, relay_party},
    token::{CertFingerprint, Scope, TokenIssuer, TokenRequest, TokenVerifier},
};
use engine_network::PeerId;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Player data the service consults. The shipped implementation is in memory; production
/// deployments back it with their own account system. Implementations must be fast.
pub trait PlayerDirectory: Send {
    /// Authoritative skill rating; overrides the (untrusted) rating a client claims.
    fn rating(&self, subject: &str) -> Option<i32>;
    /// Banned subjects are refused at authentication even with a valid token.
    fn is_banned(&self, subject: &str) -> bool;
}

/// Bounded in-memory [`PlayerDirectory`].
#[derive(Default)]
pub struct InMemoryDirectory {
    max_entries: usize,
    ratings: BTreeMap<String, i32>,
    banned: BTreeSet<String>,
}
impl InMemoryDirectory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            ..Self::default()
        }
    }
    /// False when the table is full.
    pub fn set_rating(&mut self, subject: &str, rating: i32) -> bool {
        if !self.ratings.contains_key(subject) && self.ratings.len() >= self.max_entries {
            return false;
        }
        self.ratings.insert(subject.to_owned(), rating);
        true
    }
    pub fn ban(&mut self, subject: &str) -> bool {
        if !self.banned.contains(subject) && self.banned.len() >= self.max_entries {
            return false;
        }
        self.banned.insert(subject.to_owned());
        true
    }
    pub fn unban(&mut self, subject: &str) {
        self.banned.remove(subject);
    }
}
impl PlayerDirectory for InMemoryDirectory {
    fn rating(&self, subject: &str) -> Option<i32> {
        self.ratings.get(subject).copied()
    }
    fn is_banned(&self, subject: &str) -> bool {
        self.banned.contains(subject)
    }
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub limits: OnlineLimits,
    /// Audience of the tokens that authenticate to this service.
    pub online_audience: String,
    /// Audience of the join tokens this service mints for game servers.
    pub game_audience: String,
    /// Audience of relay tokens; `None` disables `RequestRelayToken`.
    pub relay_audience: Option<String>,
    pub join_token_ttl_secs: u64,
    /// Lifetime of relay credentials. It is also the hard limit of the relay allocation made
    /// with the credential (refreshes cannot extend past it), so it should cover a match;
    /// clients request a new credential before it ends.
    pub relay_token_ttl_secs: u64,
    /// Consume connect-token nonces so a captured token cannot be replayed.
    pub connect_token_single_use: bool,
    pub modes: Vec<ModeRules>,
}
impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            limits: OnlineLimits::default(),
            online_audience: "engine-online".into(),
            game_audience: "engine-game".into(),
            relay_audience: None,
            join_token_ttl_secs: 120,
            relay_token_ttl_secs: 3600,
            connect_token_single_use: true,
            modes: crate::matchmaking::default_modes(),
        }
    }
}

/// Instruction for the transport driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Send { peer: PeerId, payload: Vec<u8> },
    Disconnect { peer: PeerId, reason: &'static str },
}

/// Nonsecret counters (safe to log).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceStats {
    pub requests: u64,
    pub auth_success: u64,
    pub auth_failures: u64,
    pub malformed: u64,
    pub rate_limited: u64,
    pub join_code_failures: u64,
    /// Windows in which failed guesses across the server reached
    /// `OnlineLimits::join_code_failures_global` (an alert signal; it blocks nobody).
    pub join_code_alarms: u64,
    pub sessions_created: u64,
    pub matches_formed: u64,
    pub tickets_expired: u64,
    pub forced_disconnects: u64,
}

struct PeerState {
    subject: String,
    fingerprint: CertFingerprint,
    bucket: TokenBucket,
    rate_violations: u32,
    malformed: u32,
    rtt_ms: Option<u32>,
}

pub struct OnlineService {
    cfg: ServiceConfig,
    gate: SessionGate,
    issuer: TokenIssuer,
    lobby: Lobby,
    matchmaker: Matchmaker,
    directory: Box<dyn PlayerDirectory>,
    peers: BTreeMap<PeerId, PeerState>,
    by_subject: BTreeMap<String, PeerId>,
    /// Failed join-code guesses per authenticated subject. Survives reconnects, so dropping and
    /// re-establishing the connection does not reset the allowance. Bounded.
    guessers: BTreeMap<String, FailureWindow>,
    /// Server-wide failed-guess count; telemetry only (see `join_code_failures_global`).
    global_code_failures: FailureWindow,
    next_match_ms: u64,
    actions: Vec<Action>,
    stats: ServiceStats,
}
impl OnlineService {
    /// `verifier` must trust the key that signs connect tokens (often `issuer`'s own key or an
    /// external identity provider's). Uses an empty in-memory directory and migration store.
    pub fn new(
        cfg: ServiceConfig,
        issuer: TokenIssuer,
        verifier: Arc<TokenVerifier>,
    ) -> Result<Self, OnlineError> {
        cfg.limits.validate()?;
        if cfg.join_token_ttl_secs == 0 || cfg.join_token_ttl_secs > issuer.max_lifetime_secs() {
            return Err(OnlineError::Invalid("join token lifetime"));
        }
        if cfg.relay_token_ttl_secs == 0 || cfg.relay_token_ttl_secs > issuer.max_lifetime_secs() {
            return Err(OnlineError::Invalid("relay token lifetime"));
        }
        let gate = SessionGate::new(
            GateConfig {
                audience: cfg.online_audience.clone(),
                auth_timeout_ms: cfg.limits.auth_timeout_ms,
                max_peers: cfg.limits.max_peers,
                single_use: cfg.connect_token_single_use,
                enforce_expiry: true,
            },
            verifier,
        );
        let matchmaker = Matchmaker::new(cfg.limits.clone(), cfg.modes.clone())
            .map_err(|_| OnlineError::Invalid("invalid matchmaking modes"))?;
        let migration = InMemoryMigrationState::new(
            cfg.limits.max_migration_state_bytes,
            cfg.limits.max_sessions,
        );
        Ok(Self {
            gate,
            lobby: Lobby::with_migration_state(cfg.limits.clone(), Box::new(migration)),
            matchmaker,
            directory: Box::new(InMemoryDirectory::new(
                cfg.limits.max_peers.saturating_mul(4),
            )),
            global_code_failures: FailureWindow::new(
                cfg.limits.join_code_failures_global,
                cfg.limits.join_code_window_ms,
            ),
            cfg,
            issuer,
            peers: BTreeMap::new(),
            by_subject: BTreeMap::new(),
            guessers: BTreeMap::new(),
            next_match_ms: 0,
            actions: Vec::new(),
            stats: ServiceStats::default(),
        })
    }
    pub fn with_directory(mut self, directory: Box<dyn PlayerDirectory>) -> Self {
        self.directory = directory;
        self
    }
    /// Replaces the host-migration state hook (call before any session exists).
    pub fn with_migration_state(mut self, state: Box<dyn MigrationState>) -> Self {
        self.lobby = Lobby::with_migration_state(self.cfg.limits.clone(), state);
        self
    }

    pub fn stats(&self) -> &ServiceStats {
        &self.stats
    }
    pub fn lobby(&self) -> &Lobby {
        &self.lobby
    }
    pub fn matchmaker(&self) -> &Matchmaker {
        &self.matchmaker
    }
    pub fn authenticated_peers(&self) -> usize {
        self.peers.len()
    }
    /// Subjects with a remembered failed-guess window (bounded by `max_tracked_guessers`).
    pub fn tracked_code_guessers(&self) -> usize {
        self.guessers.len()
    }
    pub fn tracked_peers(&self) -> usize {
        self.gate.len()
    }
    pub fn subject_of(&self, peer: PeerId) -> Option<&str> {
        self.peers.get(&peer).map(|p| p.subject.as_str())
    }
    pub fn authenticated_peer_ids(&self) -> Vec<PeerId> {
        self.peers.keys().copied().collect()
    }
    pub fn drain_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    // ----- transport events -------------------------------------------------------------

    /// The transport admitted `peer`. `fingerprint` is the SHA-256 of its TLS leaf certificate
    /// from [`crate::CertificateBinding::take`]; without it the peer is disconnected.
    pub fn on_connected(&mut self, peer: PeerId, fingerprint: Option<CertFingerprint>, now: Now) {
        if self
            .gate
            .peer_connected(peer, fingerprint, now.mono_ms)
            .is_err()
        {
            self.force_disconnect(peer, "certificate binding unavailable or service full");
        }
    }

    pub fn on_disconnected(&mut self, peer: PeerId, now: Now) {
        self.drop_peer(peer, now);
    }

    /// Handles one `Lane::SESSION` payload.
    pub fn on_message(&mut self, peer: PeerId, payload: &[u8], now: Now) {
        if let Some(why) = self.gate.timeout(peer, now) {
            self.expire_peer(peer, why, now);
            return;
        }
        let admitted = self.peers.contains_key(&peer);
        if !admitted && !self.gate.contains(peer) {
            // Already disconnected or never admitted: nothing to answer.
            return;
        }
        if admitted && !self.rate_allows(peer, now) {
            return;
        }
        let envelope = match ClientEnvelope::decode_keeping_id(payload, &self.cfg.limits) {
            Ok(envelope) => envelope,
            Err((error, id)) => return self.on_bad_message(peer, id, error, admitted, now),
        };
        self.stats.requests = self.stats.requests.saturating_add(1);
        let id = envelope.id;
        if !admitted {
            match envelope.body {
                ClientBody::Hello { token } => self.handle_hello(peer, id, &token, now),
                _ => self.reject(peer, id, ErrorCode::Unauthorized, "authenticate first", now),
            }
            return;
        }
        self.dispatch(peer, id, envelope.body, now);
        self.flush_lobby_events(now);
    }

    /// Periodic work: deadlines, token expiry, matchmaking. Call at least every few hundred ms.
    pub fn tick(&mut self, now: Now) {
        for (peer, why) in self.gate.expired(now) {
            self.expire_peer(peer, why, now);
        }
        if now.mono_ms >= self.next_match_ms {
            self.next_match_ms = now
                .mono_ms
                .saturating_add(self.cfg.limits.match_interval_ms);
            self.run_matchmaking(now);
        }
        self.flush_lobby_events(now);
    }

    /// Feeds the transport-measured RTT between the service and `peer` (used for host election).
    pub fn set_rtt(&mut self, peer: PeerId, rtt_ms: u32) {
        if let Some(state) = self.peers.get_mut(&peer) {
            state.rtt_ms = Some(rtt_ms);
            self.lobby.set_rtt(peer, rtt_ms);
        }
    }

    /// Operator hook: depose a session's host and elect another member.
    pub fn force_migrate(&mut self, session: SessionId, now: Now) -> Result<(), LobbyError> {
        let result = self.lobby.force_migrate(session);
        self.flush_lobby_events(now);
        result
    }

    // ----- plumbing ---------------------------------------------------------------------

    fn push_send(&mut self, peer: PeerId, id: u32, body: ServerBody) {
        let envelope = ServerEnvelope {
            v: PROTOCOL_VERSION,
            id,
            body,
        };
        match envelope.encode(&self.cfg.limits) {
            Ok(payload) => self.actions.push(Action::Send { peer, payload }),
            Err(_) => {
                // A response that cannot be encoded within the limits is an internal fault; the
                // request still gets a definite answer.
                let fallback = ServerEnvelope {
                    v: PROTOCOL_VERSION,
                    id,
                    body: ServerBody::Error {
                        code: ErrorCode::Internal,
                        message: "response too large".into(),
                    },
                };
                if let Ok(payload) = fallback.encode(&self.cfg.limits) {
                    self.actions.push(Action::Send { peer, payload });
                }
            }
        }
    }
    fn error(&mut self, peer: PeerId, id: u32, code: ErrorCode, message: &'static str) {
        self.push_send(
            peer,
            id,
            ServerBody::Error {
                code,
                message: message.into(),
            },
        );
    }
    fn expire_peer(&mut self, peer: PeerId, why: GateTimeout, now: Now) {
        let reason = match why {
            GateTimeout::NoToken => "no valid token before the deadline",
            GateTimeout::TokenExpired => "connect token expired",
        };
        self.drop_peer(peer, now);
        self.force_disconnect(peer, reason);
    }
    fn force_disconnect(&mut self, peer: PeerId, reason: &'static str) {
        self.stats.forced_disconnects = self.stats.forced_disconnects.saturating_add(1);
        self.actions.push(Action::Disconnect { peer, reason });
    }
    /// Sends an error, then removes the peer everywhere and asks the transport to close it.
    fn reject(&mut self, peer: PeerId, id: u32, code: ErrorCode, message: &'static str, now: Now) {
        self.error(peer, id, code, message);
        self.drop_peer(peer, now);
        self.force_disconnect(peer, message);
    }

    fn drop_peer(&mut self, peer: PeerId, now: Now) {
        self.gate.peer_gone(peer);
        if let Some(state) = self.peers.remove(&peer)
            && self.by_subject.get(&state.subject) == Some(&peer)
        {
            self.by_subject.remove(&state.subject);
        }
        self.matchmaker.peer_gone(peer);
        self.lobby.peer_gone(peer);
        self.flush_lobby_events(now);
    }

    fn rate_allows(&mut self, peer: PeerId, now: Now) -> bool {
        let limit = self.cfg.limits.max_rate_violations;
        let Some(state) = self.peers.get_mut(&peer) else {
            return false;
        };
        if state.bucket.allow(now.mono_ms) {
            return true;
        }
        state.rate_violations = state.rate_violations.saturating_add(1);
        let over = state.rate_violations >= limit;
        self.stats.rate_limited = self.stats.rate_limited.saturating_add(1);
        if over {
            self.reject(
                peer,
                0,
                ErrorCode::RateLimited,
                "request rate exceeded",
                now,
            );
        } else {
            self.error(peer, 0, ErrorCode::RateLimited, "slow down");
        }
        false
    }

    fn on_bad_message(
        &mut self,
        peer: PeerId,
        id: u32,
        error: ProtocolError,
        admitted: bool,
        now: Now,
    ) {
        self.stats.malformed = self.stats.malformed.saturating_add(1);
        match error {
            ProtocolError::Version(_) => {
                self.reject(
                    peer,
                    id,
                    ErrorCode::Unsupported,
                    "unsupported protocol version",
                    now,
                );
            }
            ProtocolError::Oversized => {
                self.reject(peer, id, ErrorCode::BadRequest, "message too large", now);
            }
            ProtocolError::Malformed | ProtocolError::Invalid(_) if !admitted => {
                self.reject(peer, id, ErrorCode::BadRequest, "malformed message", now);
            }
            ProtocolError::Malformed | ProtocolError::Invalid(_) => {
                let limit = self.cfg.limits.max_malformed_messages;
                let strikes = self.peers.get_mut(&peer).map_or(limit, |state| {
                    state.malformed = state.malformed.saturating_add(1);
                    state.malformed
                });
                let message = match error {
                    ProtocolError::Invalid(why) => why,
                    _ => "malformed message",
                };
                if strikes >= limit {
                    self.reject(
                        peer,
                        id,
                        ErrorCode::BadRequest,
                        "too many malformed messages",
                        now,
                    );
                } else {
                    self.error(peer, id, ErrorCode::BadRequest, message);
                }
            }
        }
    }

    // ----- authentication ---------------------------------------------------------------

    fn handle_hello(&mut self, peer: PeerId, id: u32, token: &Blob, now: Now) {
        let verified = self
            .gate
            .authenticate(peer, &token.0, &Scope::Connect, now)
            .map(|identity| {
                (
                    identity.subject.clone(),
                    identity.fingerprint,
                    identity.expires_at,
                )
            });
        let Ok((subject, fingerprint, expires_at)) = verified else {
            // One generic answer: the reason (expired, wrong certificate, replay...) is not an oracle.
            self.stats.auth_failures = self.stats.auth_failures.saturating_add(1);
            return self.reject(
                peer,
                id,
                ErrorCode::Unauthorized,
                "authentication failed",
                now,
            );
        };
        if self.directory.is_banned(&subject) {
            self.stats.auth_failures = self.stats.auth_failures.saturating_add(1);
            return self.reject(peer, id, ErrorCode::Forbidden, "account not permitted", now);
        }
        // The newest authenticated connection for a subject wins.
        if let Some(&old) = self.by_subject.get(&subject)
            && old != peer
        {
            self.drop_peer(old, now);
            self.force_disconnect(old, "replaced by a newer connection");
        }
        self.by_subject.insert(subject.clone(), peer);
        let limits = &self.cfg.limits;
        self.peers.entry(peer).or_insert_with(|| PeerState {
            subject: subject.clone(),
            fingerprint,
            bucket: TokenBucket::new(
                limits.requests_per_second,
                limits.request_burst,
                now.mono_ms,
            ),
            rate_violations: 0,
            malformed: 0,
            rtt_ms: None,
        });
        self.stats.auth_success = self.stats.auth_success.saturating_add(1);
        self.push_send(
            peer,
            id,
            ServerBody::HelloOk {
                subject,
                expires_at,
            },
        );
    }

    // ----- tokens -----------------------------------------------------------------------

    fn mint(
        &self,
        peer: PeerId,
        audience: &str,
        session: SessionId,
        scope: Scope,
        lifetime_secs: u64,
        now: Now,
    ) -> Result<Blob, ErrorCode> {
        let state = self.peers.get(&peer).ok_or(ErrorCode::Unauthorized)?;
        self.issuer
            .issue(
                &TokenRequest {
                    audience,
                    subject: &state.subject,
                    session: Some(session),
                    cert_sha256: state.fingerprint,
                    lifetime_secs,
                    scopes: vec![scope],
                },
                now.unix_secs,
            )
            .map(Blob)
            .map_err(|_| ErrorCode::Internal)
    }
    fn join_token(&self, peer: PeerId, session: SessionId, now: Now) -> Result<Blob, ErrorCode> {
        let epoch = self.lobby.epoch(session).ok_or(ErrorCode::NotFound)?;
        self.mint(
            peer,
            &self.cfg.game_audience,
            session,
            Scope::JoinSession { session, epoch },
            self.cfg.join_token_ttl_secs,
            now,
        )
    }

    // ----- request dispatch -------------------------------------------------------------

    fn actor(&self, peer: PeerId) -> Option<Actor> {
        self.peers.get(&peer).map(|p| Actor {
            peer,
            subject: p.subject.clone(),
        })
    }

    fn lobby_error(&mut self, peer: PeerId, id: u32, error: LobbyError) {
        let (code, message) = match error {
            LobbyError::NotFound => (ErrorCode::NotFound, "not found"),
            LobbyError::Forbidden => (ErrorCode::Forbidden, "host only"),
            LobbyError::Full => (ErrorCode::Full, "session is full"),
            LobbyError::Conflict => (ErrorCode::Conflict, "conflict"),
            LobbyError::LimitExceeded => (ErrorCode::LimitExceeded, "limit exceeded"),
            LobbyError::StaleEpoch => (ErrorCode::StaleEpoch, "stale host epoch"),
            LobbyError::Kicked => (ErrorCode::Forbidden, "removed from this session"),
            LobbyError::Invalid(why) => (ErrorCode::BadRequest, why),
            LobbyError::Internal => (ErrorCode::Internal, "internal error"),
        };
        self.error(peer, id, code, message);
    }

    /// Answers with the session view plus a join token, or with the error.
    fn session_reply(
        &mut self,
        peer: PeerId,
        id: u32,
        session: SessionId,
        created: bool,
        now: Now,
    ) {
        let token = self.join_token(peer, session, now);
        let view = self.lobby.view(session, Some(peer));
        match (view, token) {
            (Some(session), Ok(join_token)) if created => {
                self.push_send(
                    peer,
                    id,
                    ServerBody::SessionCreated {
                        session,
                        join_token,
                    },
                );
            }
            (Some(session), Ok(join_token)) => {
                self.push_send(
                    peer,
                    id,
                    ServerBody::Joined {
                        session,
                        join_token,
                    },
                );
            }
            (_, Err(code)) => self.error(peer, id, code, "could not issue a join token"),
            (None, _) => self.error(peer, id, ErrorCode::NotFound, "not found"),
        }
    }

    fn dispatch(&mut self, peer: PeerId, id: u32, body: ClientBody, now: Now) {
        let Some(actor) = self.actor(peer) else {
            return;
        };
        match body {
            ClientBody::Hello { token } => self.handle_hello(peer, id, &token, now),
            ClientBody::CreateSession(spec) => match self.lobby.create(&actor, &spec) {
                Ok(session) => {
                    self.stats.sessions_created = self.stats.sessions_created.saturating_add(1);
                    self.drop_ticket_for_session(peer);
                    self.session_reply(peer, id, session, true, now);
                }
                Err(error) => self.lobby_error(peer, id, error),
            },
            ClientBody::ListSessions {
                filter,
                after,
                limit,
            } => {
                let (sessions, next) = self.lobby.list(&filter, after, usize::from(limit));
                self.push_send(peer, id, ServerBody::SessionList { sessions, next });
            }
            ClientBody::JoinSession { target, candidates } => {
                self.handle_join(peer, id, &actor, &target, candidates.as_ref(), now);
            }
            ClientBody::LeaveSession { session_id } => match self.lobby.leave(peer, session_id) {
                Ok(()) => self.push_send(peer, id, ServerBody::Left { session_id }),
                Err(error) => self.lobby_error(peer, id, error),
            },
            ClientBody::SetReady { session_id, ready } => {
                let result = self.lobby.set_ready(peer, session_id, ready);
                self.ack(peer, id, result);
            }
            ClientBody::Kick {
                session_id,
                epoch,
                player,
            } => {
                let result = self.lobby.kick(peer, session_id, epoch, &player);
                self.ack(peer, id, result);
            }
            ClientBody::SetAttributes {
                session_id,
                epoch,
                attributes,
            } => {
                let result = self
                    .lobby
                    .set_attributes(peer, session_id, epoch, &attributes);
                self.ack(peer, id, result);
            }
            ClientBody::SetCandidates {
                session_id,
                candidates,
            } => {
                let result = self.lobby.set_candidates(peer, session_id, &candidates);
                self.ack(peer, id, result);
            }
            ClientBody::RequestJoinToken { session_id } => {
                self.handle_token_request(peer, id, session_id, TokenKind::Join, now);
            }
            ClientBody::RequestRelayToken {
                session_id,
                peer: other,
            } => self.handle_relay_token_request(peer, id, session_id, &other, now),
            ClientBody::RotateCode { session_id, epoch } => {
                match self.lobby.rotate_code(peer, session_id, epoch) {
                    Ok(code) => self.push_send(
                        peer,
                        id,
                        ServerBody::CodeRotated {
                            session_id,
                            epoch,
                            code,
                        },
                    ),
                    Err(error) => self.lobby_error(peer, id, error),
                }
            }
            ClientBody::SetGameIdentity {
                session_id,
                cert_sha256,
            } => {
                let result = <[u8; 32]>::try_from(cert_sha256.0.as_slice())
                    .map_err(|_| LobbyError::Invalid("certificate fingerprint"))
                    .and_then(|fingerprint| {
                        self.lobby.set_game_identity(peer, session_id, fingerprint)
                    });
                self.ack(peer, id, result);
            }
            ClientBody::RequestPeerIntro {
                session_id,
                peer: other,
            } => match self.lobby.pair_intro(peer, session_id, &other) {
                Ok(intro) => self.push_send(
                    peer,
                    id,
                    ServerBody::PeerIntro {
                        session_id,
                        peer: intro.peer,
                        candidates: intro.candidates,
                        punch_secret: Blob(intro.punch_secret.to_vec()),
                        controlling: intro.controlling,
                    },
                ),
                Err(error) => self.lobby_error(peer, id, error),
            },
            ClientBody::UploadMigrationState {
                session_id,
                epoch,
                state,
            } => {
                let result = self.lobby.upload_state(peer, session_id, epoch, &state.0);
                self.ack(peer, id, result);
            }
            ClientBody::EnqueueTicket(spec) => self.handle_enqueue(peer, id, &actor, spec, now),
            ClientBody::CancelTicket { ticket } => {
                if self.matchmaker.cancel(peer, ticket) {
                    self.push_send(peer, id, ServerBody::TicketCancelled { ticket });
                } else {
                    self.error(peer, id, ErrorCode::NotFound, "not found");
                }
            }
        }
    }

    fn ack(&mut self, peer: PeerId, id: u32, result: Result<(), LobbyError>) {
        match result {
            Ok(()) => self.push_send(peer, id, ServerBody::Ack {}),
            Err(error) => self.lobby_error(peer, id, error),
        }
    }

    fn handle_join(
        &mut self,
        peer: PeerId,
        id: u32,
        actor: &Actor,
        target: &JoinTarget,
        candidates: Option<&Blob>,
        now: Now,
    ) {
        let by_code = matches!(target, JoinTarget::Code { .. });
        if by_code && !self.guess_window_open(&actor.subject, now.mono_ms) {
            // Lock out guessing before the code is even compared. The allowance belongs to the
            // subject, not the connection, so reconnecting does not reset it.
            self.stats.rate_limited = self.stats.rate_limited.saturating_add(1);
            // Attempts made while locked out count too, so persistent guessing escalates
            // to a disconnect.
            self.record_guess_failure(&actor.subject, now.mono_ms);
            let abusive = self.guess_failures(&actor.subject)
                >= self
                    .cfg
                    .limits
                    .join_code_failures_per_peer
                    .saturating_mul(2);
            if abusive {
                return self.reject(
                    peer,
                    id,
                    ErrorCode::RateLimited,
                    "too many join attempts",
                    now,
                );
            }
            return self.error(peer, id, ErrorCode::RateLimited, "too many join attempts");
        }
        match self.lobby.join(actor, target, candidates) {
            Ok(session) => {
                self.drop_ticket_for_session(peer);
                self.session_reply(peer, id, session, false, now);
            }
            Err(error) => {
                if by_code && error == LobbyError::NotFound {
                    self.stats.join_code_failures = self.stats.join_code_failures.saturating_add(1);
                    self.record_guess_failure(&actor.subject, now.mono_ms);
                    self.note_global_code_failure(now.mono_ms);
                }
                self.lobby_error(peer, id, error);
            }
        }
    }

    /// Whether `subject` may still try a join code in the current window. Subjects with no
    /// recorded failures are always allowed; no state is created for them.
    fn guess_window_open(&mut self, subject: &str, now_ms: u64) -> bool {
        self.guessers
            .get_mut(subject)
            .is_none_or(|window| window.open(now_ms))
    }
    fn guess_failures(&self, subject: &str) -> u32 {
        self.guessers
            .get(subject)
            .map_or(0, FailureWindow::failures)
    }
    fn record_guess_failure(&mut self, subject: &str, now_ms: u64) {
        let limits = &self.cfg.limits;
        if !self.guessers.contains_key(subject)
            && self.guessers.len() >= limits.max_tracked_guessers
        {
            // Forget windows that no longer restrict anybody, then, if the table is still full
            // of active ones, the oldest. An attacker cycling through accounts can only push out
            // other guessers' history at the cost of one account per eviction.
            self.guessers.retain(|_, window| !window.is_stale(now_ms));
            if self.guessers.len() >= limits.max_tracked_guessers
                && let Some(oldest) = self
                    .guessers
                    .iter()
                    .min_by_key(|(_, window)| window.started_ms())
                    .map(|(subject, _)| subject.clone())
            {
                self.guessers.remove(&oldest);
            }
        }
        let (max, window_ms) = (
            limits.join_code_failures_per_peer,
            limits.join_code_window_ms,
        );
        self.guessers
            .entry(subject.to_owned())
            .or_insert_with(|| FailureWindow::starting_at(max, window_ms, now_ms))
            .record_failure(now_ms);
    }
    fn note_global_code_failure(&mut self, now_ms: u64) {
        self.global_code_failures.record_failure(now_ms);
        if self.global_code_failures.failures() == self.cfg.limits.join_code_failures_global {
            self.stats.join_code_alarms = self.stats.join_code_alarms.saturating_add(1);
        }
    }

    fn handle_token_request(
        &mut self,
        peer: PeerId,
        id: u32,
        session: SessionId,
        kind: TokenKind,
        now: Now,
    ) {
        if !self.lobby.is_member(peer, session) {
            return self.error(peer, id, ErrorCode::NotFound, "not found");
        }
        let Some(epoch) = self.lobby.epoch(session) else {
            return self.error(peer, id, ErrorCode::NotFound, "not found");
        };
        match self.join_token(peer, session, now) {
            Ok(token) => self.push_send(
                peer,
                id,
                ServerBody::Token {
                    session_id: session,
                    kind,
                    epoch,
                    token,
                },
            ),
            Err(code) => self.error(peer, id, code, "could not issue a token"),
        }
    }

    /// Mints a relay credential for the pair (requester, `other`). Both must be members of the
    /// session; the credential carries the channel derived from the session and the two subjects
    /// and the slot fixed by subject order, so a member can never obtain a credential that
    /// reaches a channel it is not party to, or the other member's slot.
    fn handle_relay_token_request(
        &mut self,
        peer: PeerId,
        id: u32,
        session: SessionId,
        other: &str,
        now: Now,
    ) {
        if !self.lobby.is_member(peer, session) {
            return self.error(peer, id, ErrorCode::NotFound, "not found");
        }
        let Some(epoch) = self.lobby.epoch(session) else {
            return self.error(peer, id, ErrorCode::NotFound, "not found");
        };
        let Some(audience) = self.cfg.relay_audience.clone() else {
            return self.error(peer, id, ErrorCode::Unsupported, "relay is not configured");
        };
        let Some(me) = self.subject_of(peer).map(str::to_owned) else {
            return self.error(peer, id, ErrorCode::Unauthorized, "authenticate first");
        };
        if me == other {
            return self.error(
                peer,
                id,
                ErrorCode::BadRequest,
                "name another member of the session",
            );
        }
        if !self
            .lobby
            .members(session)
            .iter()
            .any(|(_, subject)| subject == other)
        {
            return self.error(peer, id, ErrorCode::NotFound, "not found");
        }
        let scope = Scope::Relay {
            session,
            channel: relay_channel(session, &me, other),
            party: relay_party(&me, other),
        };
        match self.mint(
            peer,
            &audience,
            session,
            scope,
            self.cfg.relay_token_ttl_secs,
            now,
        ) {
            Ok(token) => self.push_send(
                peer,
                id,
                ServerBody::Token {
                    session_id: session,
                    kind: TokenKind::Relay,
                    epoch,
                    token,
                },
            ),
            Err(code) => self.error(peer, id, code, "could not issue a token"),
        }
    }

    fn handle_enqueue(
        &mut self,
        peer: PeerId,
        id: u32,
        actor: &Actor,
        mut spec: crate::protocol::TicketSpec,
        now: Now,
    ) {
        if self.lobby.membership_count(peer) >= self.cfg.limits.max_memberships_per_peer {
            return self.error(
                peer,
                id,
                ErrorCode::LimitExceeded,
                "leave your session first",
            );
        }
        if let Some(rating) = self.directory.rating(&actor.subject) {
            spec.rating = rating.clamp(0, crate::protocol::MAX_RATING);
        }
        let rtt = self.peers.get(&peer).and_then(|p| p.rtt_ms);
        match self
            .matchmaker
            .enqueue(peer, &actor.subject, &spec, rtt, now.mono_ms)
        {
            Ok(ticket) => self.push_send(peer, id, ServerBody::TicketQueued { ticket }),
            Err(MatchError::UnknownMode) => {
                self.error(peer, id, ErrorCode::BadRequest, "unknown mode")
            }
            Err(MatchError::QueueFull) => {
                self.error(
                    peer,
                    id,
                    ErrorCode::LimitExceeded,
                    "matchmaking queue is full",
                );
            }
            Err(MatchError::AlreadyQueued) => {
                self.error(peer, id, ErrorCode::Conflict, "already in the queue");
            }
            Err(MatchError::Invalid(why)) => self.error(peer, id, ErrorCode::BadRequest, why),
        }
    }

    // ----- matchmaking + lobby notifications ---------------------------------------------

    fn run_matchmaking(&mut self, now: Now) {
        if self.matchmaker.is_empty() {
            return;
        }
        let output = self.matchmaker.tick(now.mono_ms);
        for ticket in output.expired {
            self.stats.tickets_expired = self.stats.tickets_expired.saturating_add(1);
            self.push_send(
                ticket.peer,
                0,
                ServerBody::TicketExpired {
                    ticket: ticket.id,
                    reason: "timeout".into(),
                },
            );
        }
        for found in output.matches {
            let mut members = Vec::new();
            let mut tickets: Vec<(PeerId, TicketId)> = Vec::new();
            for (team, group) in found.teams.iter().enumerate() {
                for ticket in group {
                    members.push(MatchedMember {
                        peer: ticket.peer,
                        subject: ticket.subject.clone(),
                        team: team as u8,
                        candidates: ticket.candidates.clone(),
                        // Prefer the freshest transport measurement over the one at enqueue time.
                        rtt_ms: self
                            .peers
                            .get(&ticket.peer)
                            .and_then(|p| p.rtt_ms)
                            .or(ticket.rtt_ms),
                    });
                    tickets.push((ticket.peer, ticket.id));
                }
            }
            let created = self.lobby.create_matched(&MatchedSession {
                mode: found.mode.clone(),
                region: found.region.clone(),
                hosting: found.hosting,
                members,
            });
            match created {
                Ok(session) => {
                    self.stats.matches_formed = self.stats.matches_formed.saturating_add(1);
                    self.stats.sessions_created = self.stats.sessions_created.saturating_add(1);
                    for (peer, ticket) in tickets {
                        let token = self.join_token(peer, session, now);
                        let view = self.lobby.view(session, Some(peer));
                        if let (Some(session), Ok(join_token)) = (view, token) {
                            self.push_send(
                                peer,
                                0,
                                ServerBody::MatchFound {
                                    ticket,
                                    session,
                                    join_token,
                                },
                            );
                        } else {
                            self.push_send(
                                peer,
                                0,
                                ServerBody::TicketExpired {
                                    ticket,
                                    reason: "token unavailable".into(),
                                },
                            );
                        }
                    }
                }
                Err(_) => {
                    for (peer, ticket) in tickets {
                        self.push_send(
                            peer,
                            0,
                            ServerBody::TicketExpired {
                                ticket,
                                reason: "match could not be created".into(),
                            },
                        );
                    }
                }
            }
        }
    }

    /// A peer that enters a lobby session leaves the matchmaking queue (it could not be placed
    /// in a matched session anyway while it holds the maximum number of memberships).
    fn drop_ticket_for_session(&mut self, peer: PeerId) {
        if let Some(ticket) = self.matchmaker.peer_gone(peer) {
            self.push_send(
                peer,
                0,
                ServerBody::TicketExpired {
                    ticket: ticket.id,
                    reason: "joined a session".into(),
                },
            );
        }
    }

    fn flush_lobby_events(&mut self, now: Now) {
        for event in self.lobby.drain_events() {
            for recipient in event.recipients {
                if !self.peers.contains_key(&recipient) {
                    continue;
                }
                let mut body = event.body.clone();
                if let ServerBody::HostChanged {
                    session_id,
                    epoch,
                    join_token,
                    ..
                } = &mut body
                {
                    let scope = Scope::JoinSession {
                        session: *session_id,
                        epoch: *epoch,
                    };
                    *join_token = self
                        .mint(
                            recipient,
                            &self.cfg.game_audience,
                            *session_id,
                            scope,
                            self.cfg.join_token_ttl_secs,
                            now,
                        )
                        .ok();
                }
                self.push_send(recipient, 0, body);
            }
        }
    }
}
