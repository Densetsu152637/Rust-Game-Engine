//! Steam Networking Sockets as an [`engine_network::MessageEndpoint`].
//!
//! [`SteamServerEndpoint`] listens (P2P virtual port or UDP address) and
//! [`SteamClientEndpoint`] dials. Both run on a [`SteamSession`]: every Steam call goes
//! through the session's backend on the tick thread, and the session lock is held only
//! for the duration of one backend call. The identity and authentication design, the
//! wire format and the limits are documented in `docs/steam.md` (Networking Sockets
//! transport).

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use engine_network::{
    Delivery, Error, Lane, LinkEvent, LinkStats, MessageEndpoint, PeerId, PollBudget, Result,
    SERVER_PEER, TransportKind, TransportSecurity,
};

use crate::error::SteamError;
use crate::net::*;
use crate::session::{NetKey, SteamSession};
use crate::types::{AuthTicket, SteamId, SteamStatus};
use crate::wire::{self, Frame};

/// Where a server listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamListen {
    /// `ConnectP2P` peers by Steam identity, through the relay network.
    P2p { virtual_port: i32 },
    /// `ConnectByIPAddress` clients over UDP. The client address is not authenticated.
    Ip(SocketAddr),
}

/// How a server proves who a connecting client is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPolicy {
    /// Require a valid auth session ticket for the SteamID the client claims.
    Ticket,
    /// Trust the SteamID Steam authenticated for the P2P connection and skip the ticket.
    /// Refused for IP listeners. Intended for single-account loopback tests, where Steam
    /// allows one auth session per SteamID in a process.
    IdentityOnly,
}

/// Resource limits shared by both endpoints.
#[derive(Debug, Clone)]
pub struct SteamLimits {
    /// Admitted plus still-handshaking peers a server accepts.
    pub max_peers: usize,
    /// Time from connection start to admission.
    pub handshake_timeout: Duration,
    /// Time a client waits for the relay network before giving up.
    pub relay_timeout: Duration,
    /// Connection requests held while `max_accepts` is exhausted; further ones are rejected.
    pub max_pending_requests: usize,
}

impl Default for SteamLimits {
    fn default() -> Self {
        Self {
            max_peers: 16,
            handshake_timeout: Duration::from_secs(10),
            relay_timeout: Duration::from_secs(30),
            max_pending_requests: 64,
        }
    }
}

/// Server construction parameters.
#[derive(Debug, Clone)]
pub struct SteamServerConfig {
    pub listen: SteamListen,
    pub auth: AuthPolicy,
    pub limits: SteamLimits,
}

impl SteamServerConfig {
    /// A P2P server on `virtual_port` requiring auth tickets.
    pub fn p2p(virtual_port: i32) -> Self {
        Self {
            listen: SteamListen::P2p { virtual_port },
            auth: AuthPolicy::Ticket,
            limits: SteamLimits::default(),
        }
    }

    /// A UDP server on `addr` requiring auth tickets.
    pub fn ip(addr: SocketAddr) -> Self {
        Self {
            listen: SteamListen::Ip(addr),
            auth: AuthPolicy::Ticket,
            limits: SteamLimits::default(),
        }
    }

    pub fn with_auth(mut self, auth: AuthPolicy) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_limits(mut self, limits: SteamLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Client construction parameters.
#[derive(Debug, Clone, Default)]
pub struct SteamClientConfig {
    pub limits: SteamLimits,
    /// The server's SteamID, when known out of band. A P2P client always expects the
    /// SteamID it dialed; an IP client verifies the server only when this is set (the
    /// check compares a claim, it is not proof: see `docs/steam.md`).
    pub expected_server: Option<SteamId>,
}

fn steam_error(error: SteamError) -> Error {
    Error::Io(io::Error::other(error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Server: accepted, waiting for the connection to complete.
    Accepting,
    /// Server: connected, waiting for the client's Hello.
    AwaitHello,
    /// Server: ticket submitted, waiting for Steam's verdict.
    Authenticating(SteamId),
    /// Client: connecting.
    Dialing,
    /// Client: connected, waiting for an auth ticket to send Hello.
    NeedTicket,
    /// Client: Hello sent, waiting for Accept.
    AwaitAccept,
    Admitted,
}

struct Peer {
    conn: NetConnId,
    state: State,
    deadline: Instant,
    steam_id: Option<SteamId>,
    remote: NetRemote,
    send_seq: [u32; 16],
    recv_newest: [Option<u32>; 16],
    /// Received frames that did not fit the byte budget; processed first next poll.
    stash: VecDeque<Vec<u8>>,
    stats: LinkStats,
    dirty: bool,
    /// Why the transport ended; the peer is removed once its received data is drained.
    closed: Option<String>,
    auth_user: Option<SteamId>,
}

impl Peer {
    fn new(conn: NetConnId, state: State, remote: NetRemote, deadline: Instant) -> Self {
        Self {
            conn,
            state,
            deadline,
            steam_id: None,
            remote,
            send_seq: [0; 16],
            recv_newest: [None; 16],
            stash: VecDeque::new(),
            stats: LinkStats::default(),
            dirty: false,
            closed: None,
            auth_user: None,
        }
    }
}

enum Role {
    Server {
        listen: NetListenId,
        auth: AuthPolicy,
    },
    Client {
        dial: Option<Dial>,
        expected: Option<SteamId>,
        ticket: Option<AuthTicket>,
    },
}

#[derive(Clone, Copy)]
enum Target {
    P2p { host: SteamId, virtual_port: i32 },
    Ip(SocketAddr),
}

#[derive(Clone, Copy)]
struct Dial {
    target: Target,
    started: Instant,
}

/// Frames requested from the backend per call.
const RECEIVE_BATCH: usize = 8;

/// Remaining work in one `poll`.
struct Budget {
    messages: usize,
    bytes: usize,
    consumed: bool,
}

impl Budget {
    fn fits(&self, len: usize) -> bool {
        self.messages > 0 && (len <= self.bytes || !self.consumed)
    }
    fn take(&mut self, len: usize) {
        self.messages -= 1;
        self.bytes = self.bytes.saturating_sub(len);
        self.consumed = true;
    }
}

/// Everything the server and client endpoints share.
struct Core {
    session: SteamSession,
    /// This endpoint's id in the session's networking event router.
    owner: u32,
    role: Role,
    limits: SteamLimits,
    me: SteamId,
    max_message: usize,
    peers: BTreeMap<PeerId, Peer>,
    by_conn: BTreeMap<NetConnId, PeerId>,
    backlog: VecDeque<NetEvent>,
    next_peer: PeerId,
    rotate: usize,
    out: Vec<LinkEvent>,
}

impl Core {
    fn new(session: SteamSession, role: Role, limits: SteamLimits) -> Result<Self> {
        let me = match session.status() {
            SteamStatus::Ready { user, .. } => user.id,
            other => {
                let reason = other
                    .unavailable_reason()
                    .map_or_else(|| "not ready".into(), |r| r.to_string());
                return Err(Error::Io(io::Error::other(format!(
                    "Steam unavailable: {reason}"
                ))));
            }
        };
        let max_message = session
            .with_backend(|b| b.net_max_message_size())
            .map_err(steam_error)?;
        if max_message <= wire::overhead(Delivery::UnreliableSequenced) {
            return Err(Error::Invalid("Steam message limit is too small"));
        }
        let owner = session.net_register();
        Ok(Self {
            session,
            owner,
            role,
            limits,
            me,
            max_message,
            peers: BTreeMap::new(),
            by_conn: BTreeMap::new(),
            backlog: VecDeque::new(),
            next_peer: 1,
            rotate: 0,
            out: Vec::new(),
        })
    }

    fn is_server(&self) -> bool {
        matches!(self.role, Role::Server { .. })
    }

    fn relay_status(&self) -> RelayStatus {
        self.session
            .with_backend(|b| b.net_relay_status())
            .unwrap_or(RelayStatus::NotStarted)
    }

    fn admitted(&self, peer: PeerId) -> Option<&Peer> {
        self.peers.get(&peer).filter(|p| p.state == State::Admitted)
    }

    fn max_payload(&self, delivery: Delivery) -> usize {
        self.max_message - wire::overhead(delivery)
    }

    // ---- sending ----

    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        if !lane.is_valid() {
            return Err(Error::Invalid("lane out of range"));
        }
        if self.admitted(peer).is_none() {
            return Err(Error::Invalid("unknown peer"));
        }
        if payload.len() > self.max_payload(delivery) {
            return Err(Error::Invalid("payload exceeds the Steam message limit"));
        }
        let Some(p) = self.peers.get_mut(&peer) else {
            return Err(Error::Invalid("unknown peer"));
        };
        let sequence = p.send_seq[usize::from(lane.0)];
        p.send_seq[usize::from(lane.0)] = sequence.wrapping_add(1);
        let frame = wire::encode_data(lane, delivery, sequence, payload);
        let flags = NetSendFlags {
            reliable: delivery.is_reliable(),
            no_nagle: !delivery.is_reliable(),
        };
        let conn = p.conn;
        match self
            .session
            .with_backend(|b| b.net_send(conn, &frame, flags))
        {
            Ok(()) => {
                p.stats.sent_bytes += frame.len() as u64;
                p.stats.sent_packets += 1;
                p.dirty |= flags.reliable;
                Ok(())
            }
            Err(SteamError::SendQueueFull) if flags.reliable => Err(Error::QueueFull),
            Err(SteamError::SendQueueFull) => {
                p.stats.dropped_unreliable += 1;
                Ok(())
            }
            Err(SteamError::MessageTooLarge) => {
                Err(Error::Invalid("payload exceeds the Steam message limit"))
            }
            Err(SteamError::ConnectionClosed) => Err(Error::Disconnected),
            Err(other) => Err(steam_error(other)),
        }
    }

    fn send_control(&mut self, conn: NetConnId, frame: &[u8]) -> bool {
        let flags = NetSendFlags {
            reliable: true,
            no_nagle: true,
        };
        self.session
            .with_backend(|b| b.net_send(conn, frame, flags))
            .is_ok()
    }

    // ---- peers ----

    /// Removes a peer. `announce` emits `Disconnected`; the transport connection is closed
    /// unless it already ended.
    fn remove_peer(&mut self, id: PeerId, reason: &str, announce: bool, linger: bool) {
        let Some(peer) = self.peers.remove(&id) else {
            return;
        };
        self.by_conn.remove(&peer.conn);
        let user = peer.auth_user;
        let conn = peer.conn;
        self.session.net_release(NetKey::Conn(conn));
        if let Some(user) = user {
            self.session.net_release(NetKey::Auth(user));
        }
        self.session.with_backend(|b| {
            let _ = b.net_close(conn, reason, linger);
            if let Some(user) = user {
                let _ = b.net_end_auth(user);
            }
        });
        if announce {
            self.out.push(LinkEvent::Disconnected {
                peer: id,
                reason: reason.to_owned(),
            });
        }
    }

    /// Drops a peer because it broke the protocol or failed authentication.
    fn reject_peer(&mut self, id: PeerId, reason: &str) {
        let announce = self
            .peers
            .get(&id)
            .is_some_and(|p| p.state == State::Admitted || !self.is_server());
        self.remove_peer(id, reason, announce, false);
    }

    fn admit(&mut self, id: PeerId, user: SteamId) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        peer.state = State::Admitted;
        peer.steam_id = Some(user);
        peer.deadline = Instant::now();
        let conn = peer.conn;
        if self.is_server() {
            let accept = wire::encode_accept(self.me);
            if !self.send_control(conn, &accept) {
                self.reject_peer(id, "could not send accept");
                return;
            }
        }
        self.out.push(LinkEvent::Authenticated(id));
        self.out.push(LinkEvent::Connected(id));
    }

    // ---- connection setup ----

    fn advance_dial(&mut self) {
        let Role::Client { dial, .. } = &self.role else {
            return;
        };
        let Some(Dial { target, started }) = dial.as_ref().copied() else {
            return;
        };
        let fail = |core: &mut Self, reason: &str| {
            if let Role::Client { dial, .. } = &mut core.role {
                *dial = None;
            }
            core.out.push(LinkEvent::Disconnected {
                peer: SERVER_PEER,
                reason: reason.to_owned(),
            });
        };
        let result = match target {
            Target::Ip(addr) => self.session.with_backend(|b| b.net_connect_ip(addr)),
            Target::P2p { host, virtual_port } => {
                match self.relay_status() {
                    RelayStatus::Ready => {}
                    RelayStatus::Failed(why) => {
                        fail(self, &format!("relay network failed: {why}"));
                        return;
                    }
                    RelayStatus::NotStarted | RelayStatus::Pending => {
                        if started.elapsed() >= self.limits.relay_timeout {
                            fail(self, "relay network is not ready");
                        }
                        return;
                    }
                }
                self.session
                    .with_backend(|b| b.net_connect_p2p(host, virtual_port))
            }
        };
        match result {
            Ok(conn) => {
                if let Role::Client { dial, .. } = &mut self.role {
                    *dial = None;
                }
                let deadline = Instant::now() + self.limits.handshake_timeout;
                self.session.net_claim(self.owner, NetKey::Conn(conn));
                self.peers.insert(
                    SERVER_PEER,
                    Peer::new(conn, State::Dialing, NetRemote::Unknown, deadline),
                );
                self.by_conn.insert(conn, SERVER_PEER);
            }
            Err(error) => fail(self, &format!("connect failed: {error}")),
        }
    }

    /// Moves backend events into the backlog, rejecting connection requests beyond the cap.
    fn gather_events(&mut self) {
        let events = self.session.net_events(self.owner);
        let mut pending = self
            .backlog
            .iter()
            .filter(|e| matches!(e, NetEvent::ConnectionRequest { .. }))
            .count();
        for event in events {
            if let NetEvent::ConnectionRequest { conn, .. } = &event {
                if pending >= self.limits.max_pending_requests {
                    let conn = *conn;
                    self.refuse(conn, "too many pending connections");
                    continue;
                }
                pending += 1;
            }
            self.backlog.push_back(event);
        }
    }

    fn process_events(&mut self, mut accepts: usize) {
        // Requests beyond the accept budget wait (in order); everything else proceeds.
        let mut deferred = VecDeque::new();
        while let Some(event) = self.backlog.pop_front() {
            if matches!(event, NetEvent::ConnectionRequest { .. }) && accepts == 0 {
                deferred.push_back(event);
                continue;
            }
            match event {
                NetEvent::ConnectionRequest { conn, remote, .. } => {
                    accepts -= 1;
                    self.on_request(conn, remote);
                }
                NetEvent::Connected { conn, remote } => self.on_connected(conn, remote),
                NetEvent::Closed { conn, reason } => {
                    if let Some(peer) = self
                        .by_conn
                        .get(&conn)
                        .and_then(|id| self.peers.get_mut(id))
                    {
                        peer.closed = Some(reason);
                    }
                }
                NetEvent::AuthResult {
                    user,
                    accepted,
                    reason,
                } => self.on_auth_result(user, accepted, &reason),
            }
        }
        self.backlog = deferred;
    }

    /// Rejects a connection request and drops its router ownership.
    fn refuse(&mut self, conn: NetConnId, reason: &str) {
        self.session.net_release(NetKey::Conn(conn));
        self.session.with_backend(|b| {
            let _ = b.net_close(conn, reason, false);
        });
    }

    fn on_request(&mut self, conn: NetConnId, remote: NetRemote) {
        let Role::Server { auth, .. } = &self.role else {
            return;
        };
        let identity_ok = *auth == AuthPolicy::Ticket || matches!(remote, NetRemote::Steam(_));
        if self.peers.len() >= self.limits.max_peers || !identity_ok {
            let reason = if identity_ok {
                "server is full"
            } else {
                "no authenticated identity"
            };
            self.refuse(conn, reason);
            return;
        }
        if self.session.with_backend(|b| b.net_accept(conn)).is_err() {
            self.session.net_release(NetKey::Conn(conn));
            return;
        }
        let id = self.next_peer;
        self.next_peer += 1;
        let deadline = Instant::now() + self.limits.handshake_timeout;
        self.peers
            .insert(id, Peer::new(conn, State::Accepting, remote, deadline));
        self.by_conn.insert(conn, id);
    }

    fn on_connected(&mut self, conn: NetConnId, remote: NetRemote) {
        let Some(&id) = self.by_conn.get(&conn) else {
            return;
        };
        let expected = match &self.role {
            Role::Client { expected, .. } => *expected,
            Role::Server { .. } => None,
        };
        let is_server = self.is_server();
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        peer.remote = remote;
        let state = peer.state;
        match (state, is_server) {
            (State::Accepting, true) => peer.state = State::AwaitHello,
            (State::Dialing, false) => {
                // A P2P connection reports the authenticated identity of the server.
                if let (NetRemote::Steam(found), Some(want)) = (remote, expected)
                    && found != want
                {
                    self.reject_peer(id, "server identity mismatch");
                    return;
                }
                peer.state = State::NeedTicket;
            }
            _ => {}
        }
    }

    fn on_auth_result(&mut self, user: SteamId, accepted: bool, reason: &str) {
        let Some((&id, peer)) = self.peers.iter().find(|(_, p)| p.auth_user == Some(user)) else {
            return;
        };
        match (peer.state, accepted) {
            (State::Authenticating(_), true) => self.admit(id, user),
            (State::Authenticating(_), false) => {
                self.reject_peer(id, &format!("authentication failed: {reason}"));
            }
            (State::Admitted, false) => {
                self.reject_peer(id, &format!("authentication revoked: {reason}"));
            }
            _ => {}
        }
    }

    fn advance_client_handshake(&mut self) {
        let Some(peer) = self.peers.get(&SERVER_PEER) else {
            return;
        };
        if peer.state != State::NeedTicket {
            return;
        }
        let conn = peer.conn;
        let Ok(ticket) = self.session.with_backend(|b| b.auth_session_ticket()) else {
            return; // Steam issues tickets asynchronously; retry next poll until the deadline.
        };
        if ticket.data.len() > wire::MAX_TICKET {
            let handle = ticket.handle;
            self.session.with_backend(|b| {
                let _ = b.cancel_auth_ticket(handle);
            });
            self.reject_peer(SERVER_PEER, "auth ticket too large");
            return;
        }
        let hello = wire::encode_hello(self.me, &ticket.data);
        if let Role::Client { ticket: slot, .. } = &mut self.role {
            *slot = Some(ticket);
        }
        if !self.send_control(conn, &hello) {
            self.reject_peer(SERVER_PEER, "could not send hello");
            return;
        }
        if let Some(peer) = self.peers.get_mut(&SERVER_PEER) {
            peer.state = State::AwaitAccept;
        }
    }

    // ---- receiving ----

    fn receive(&mut self, budget: &mut Budget) {
        let ids: Vec<PeerId> = self.peers.keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        let start = self.rotate % ids.len();
        self.rotate = self.rotate.wrapping_add(1);
        for offset in 0..ids.len() {
            let id = ids[(start + offset) % ids.len()];
            self.receive_from(id, budget);
        }
    }

    fn receive_from(&mut self, id: PeerId, budget: &mut Budget) {
        loop {
            if budget.messages == 0 {
                return;
            }
            let Some(peer) = self.peers.get_mut(&id) else {
                return;
            };
            let conn = peer.conn;
            let connected = !matches!(
                peer.state,
                State::Accepting | State::Dialing | State::NeedTicket
            );
            let mut frames: VecDeque<Vec<u8>> = std::mem::take(&mut peer.stash);
            if connected && frames.is_empty() {
                let want = budget.messages.min(RECEIVE_BATCH);
                if let Ok(batch) = self.session.with_backend(|b| b.net_receive(conn, want)) {
                    frames.extend(batch);
                }
            }
            if frames.is_empty() {
                if let Some(reason) = self.peers.get(&id).and_then(|p| p.closed.clone()) {
                    let announce = self
                        .peers
                        .get(&id)
                        .is_some_and(|p| p.state == State::Admitted || !self.is_server());
                    self.remove_peer(id, &reason, announce, false);
                }
                return;
            }
            while let Some(frame) = frames.pop_front() {
                if !budget.fits(frame.len()) {
                    frames.push_front(frame);
                    if let Some(peer) = self.peers.get_mut(&id) {
                        peer.stash = frames;
                    }
                    return;
                }
                budget.take(frame.len());
                if !self.on_frame(id, &frame) {
                    return;
                }
            }
        }
    }

    /// Processes one received frame; false when the peer was removed.
    fn on_frame(&mut self, id: PeerId, frame: &[u8]) -> bool {
        let decoded = match wire::decode(frame) {
            Ok(frame) => frame,
            Err(reason) => {
                self.reject_peer(id, &format!("protocol violation: {reason}"));
                return false;
            }
        };
        let Some(state) = self.peers.get(&id).map(|p| p.state) else {
            return false;
        };
        match (state, decoded) {
            (
                State::Admitted,
                Frame::Data {
                    lane,
                    delivery,
                    sequence,
                    payload,
                },
            ) => {
                self.on_data(id, lane, delivery, sequence, payload);
                true
            }
            (State::AwaitHello, Frame::Hello { user, ticket }) if self.is_server() => {
                self.on_hello(id, user, ticket)
            }
            (State::AwaitAccept, Frame::Accept { server }) if !self.is_server() => {
                self.on_accept(id, server)
            }
            _ => {
                self.reject_peer(id, "protocol violation: unexpected frame");
                false
            }
        }
    }

    fn on_data(
        &mut self,
        id: PeerId,
        lane: Lane,
        delivery: Delivery,
        sequence: Option<u32>,
        payload: &[u8],
    ) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        if let Some(sequence) = sequence {
            let newest = &mut peer.recv_newest[usize::from(lane.0)];
            if newest.is_some_and(|n| !wire::newer(sequence, n)) {
                peer.stats.dropped_unreliable += 1;
                return;
            }
            *newest = Some(sequence);
        }
        peer.stats.received_bytes += payload.len() as u64;
        peer.stats.received_packets += 1;
        self.out.push(LinkEvent::Message {
            peer: id,
            lane,
            delivery,
            payload: payload.to_vec(),
        });
    }

    fn on_hello(&mut self, id: PeerId, user: SteamId, ticket: &[u8]) -> bool {
        let Role::Server { auth, .. } = &self.role else {
            return false;
        };
        let auth = *auth;
        let Some(remote) = self.peers.get(&id).map(|p| p.remote) else {
            return false;
        };
        if let NetRemote::Steam(authenticated) = remote
            && authenticated != user
        {
            self.reject_peer(
                id,
                "claimed SteamID differs from the authenticated identity",
            );
            return false;
        }
        if self.peers.iter().any(|(other, p)| {
            *other != id && p.state == State::Admitted && p.steam_id == Some(user)
        }) {
            self.reject_peer(id, "this SteamID is already connected");
            return false;
        }
        // A newer claim for a SteamID still being validated supersedes the older pending
        // one (an unauthenticated peer must not be able to hold a SteamID hostage).
        let superseded: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(other, p)| {
                **other != id && matches!(p.state, State::Authenticating(u) if u == user)
            })
            .map(|(other, _)| *other)
            .collect();
        for old in superseded {
            self.reject_peer(old, "superseded by a newer login for this SteamID");
        }
        match auth {
            AuthPolicy::IdentityOnly => {
                if !matches!(remote, NetRemote::Steam(_)) {
                    self.reject_peer(id, "no authenticated identity");
                    return false;
                }
                self.admit(id, user);
            }
            AuthPolicy::Ticket => {
                match self
                    .session
                    .with_backend(|b| b.net_begin_auth(user, ticket))
                {
                    Ok(()) => {
                        self.session.net_claim(self.owner, NetKey::Auth(user));
                        if let Some(peer) = self.peers.get_mut(&id) {
                            peer.state = State::Authenticating(user);
                            peer.auth_user = Some(user);
                            peer.steam_id = Some(user);
                        }
                    }
                    Err(error) => {
                        self.reject_peer(id, &format!("invalid auth ticket: {error}"));
                        return false;
                    }
                }
            }
        }
        true
    }

    fn on_accept(&mut self, id: PeerId, server: SteamId) -> bool {
        let want = match &self.role {
            Role::Client { expected, .. } => *expected,
            Role::Server { .. } => None,
        };
        let remote = self.peers.get(&id).map(|p| p.remote);
        let authenticated = match remote {
            Some(NetRemote::Steam(found)) => Some(found),
            _ => None,
        };
        if authenticated
            .or(want)
            .is_some_and(|expected| expected != server)
        {
            self.reject_peer(id, "server identity mismatch");
            return false;
        }
        self.admit(id, server);
        true
    }

    fn expire(&mut self) {
        let now = Instant::now();
        let stale: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, p)| p.state != State::Admitted && now >= p.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.reject_peer(id, "handshake timed out");
        }
    }

    fn flush(&mut self) {
        let dirty: Vec<NetConnId> = self
            .peers
            .values_mut()
            .filter_map(|p| std::mem::take(&mut p.dirty).then_some(p.conn))
            .collect();
        if dirty.is_empty() {
            return;
        }
        self.session.with_backend(|b| {
            for conn in dirty {
                let _ = b.net_flush(conn);
            }
        });
    }

    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        if budget.max_messages == 0 && budget.max_bytes == 0 && budget.max_accepts == 0 {
            return Ok(Vec::new());
        }
        self.advance_dial();
        self.gather_events();
        self.process_events(budget.max_accepts);
        self.advance_client_handshake();
        let mut work = Budget {
            messages: budget.max_messages,
            bytes: budget.max_bytes,
            consumed: false,
        };
        self.receive(&mut work);
        // Events raised while receiving (auth verdicts from instant backends).
        self.expire();
        self.flush();
        Ok(std::mem::take(&mut self.out))
    }

    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        let p = self.admitted(peer)?;
        let mut stats = p.stats;
        if let Some(net) = self.session.with_backend(|b| b.net_conn_stats(p.conn)) {
            stats.rtt_ms = net.ping_ms;
            stats.queued_bytes = net.queued_bytes;
        }
        Some(stats)
    }

    fn disconnect(&mut self, peer: PeerId) -> bool {
        if self.peers.contains_key(&peer) {
            self.remove_peer(peer, "disconnected locally", false, true);
            return true;
        }
        if peer == SERVER_PEER
            && let Role::Client { dial, .. } = &mut self.role
            && dial.take().is_some()
        {
            return true;
        }
        false
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let ids: Vec<PeerId> = self.peers.keys().copied().collect();
        for id in ids {
            self.remove_peer(id, "endpoint dropped", false, false);
        }
        match &mut self.role {
            Role::Server { listen, .. } => {
                let listen = *listen;
                self.session.with_backend(|b| {
                    let _ = b.net_close_listen(listen);
                });
            }
            Role::Client { ticket, .. } => {
                if let Some(ticket) = ticket.take() {
                    self.session.with_backend(|b| {
                        let _ = b.cancel_auth_ticket(ticket.handle);
                    });
                }
            }
        }
        self.session.net_unregister(self.owner);
    }
}

/// Server side of the Steam transport.
pub struct SteamServerEndpoint {
    core: Core,
}

impl SteamServerEndpoint {
    /// Starts listening. A P2P listener also starts the asynchronous relay initialisation;
    /// clients cannot reach it until [`Self::relay_status`] is `Ready`.
    pub fn listen(session: SteamSession, config: SteamServerConfig) -> Result<Self> {
        if config.auth == AuthPolicy::IdentityOnly && matches!(config.listen, SteamListen::Ip(_)) {
            return Err(Error::Invalid("IdentityOnly requires a P2P listener"));
        }
        if config.limits.max_peers == 0 || config.limits.max_pending_requests == 0 {
            return Err(Error::Invalid("limits must be positive"));
        }
        let listen = session
            .with_backend(|b| match config.listen {
                SteamListen::P2p { virtual_port } => {
                    let _ = b.net_init_relay();
                    b.net_listen_p2p(virtual_port)
                }
                SteamListen::Ip(addr) => b.net_listen_ip(addr),
            })
            .map_err(steam_error)?;
        let role = Role::Server {
            listen,
            auth: config.auth,
        };
        match Core::new(session.clone(), role, config.limits) {
            Ok(core) => {
                session.net_claim(core.owner, NetKey::Listen(listen));
                Ok(Self { core })
            }
            Err(error) => {
                session.with_backend(|b| {
                    let _ = b.net_close_listen(listen);
                });
                Err(error)
            }
        }
    }

    /// State of the relay network P2P clients need.
    pub fn relay_status(&self) -> RelayStatus {
        self.core.relay_status()
    }

    pub fn local_steam_id(&self) -> SteamId {
        self.core.me
    }

    /// The SteamID of an admitted peer (valid once `Authenticated` was delivered).
    pub fn peer_steam_id(&self, peer: PeerId) -> Option<SteamId> {
        self.core.admitted(peer).and_then(|p| p.steam_id)
    }

    /// Peers that connected but are not yet admitted.
    pub fn pending_peers(&self) -> usize {
        self.core
            .peers
            .values()
            .filter(|p| p.state != State::Admitted)
            .count()
    }
}

/// Client side of the Steam transport.
pub struct SteamClientEndpoint {
    core: Core,
}

impl SteamClientEndpoint {
    /// Dials a hosting player by SteamID through the relay network. The dial is deferred
    /// until the relay is `Ready`; failure or timeout is reported as `Disconnected`.
    pub fn connect_p2p(
        session: SteamSession,
        host: SteamId,
        virtual_port: i32,
        mut config: SteamClientConfig,
    ) -> Result<Self> {
        config.expected_server = Some(host);
        let _ = session.with_backend(|b| b.net_init_relay());
        Self::start(session, Target::P2p { host, virtual_port }, config)
    }

    /// Dials a UDP address. The server is unauthenticated unless
    /// `config.expected_server` is set (and even then only a claim is compared).
    pub fn connect_ip(
        session: SteamSession,
        addr: SocketAddr,
        config: SteamClientConfig,
    ) -> Result<Self> {
        Self::start(session, Target::Ip(addr), config)
    }

    fn start(session: SteamSession, target: Target, config: SteamClientConfig) -> Result<Self> {
        let role = Role::Client {
            dial: Some(Dial {
                target,
                started: Instant::now(),
            }),
            expected: config.expected_server,
            ticket: None,
        };
        let mut core = Core::new(session, role, config.limits)?;
        core.advance_dial();
        Ok(Self { core })
    }

    pub fn relay_status(&self) -> RelayStatus {
        self.core.relay_status()
    }

    pub fn local_steam_id(&self) -> SteamId {
        self.core.me
    }

    /// The server's SteamID once admitted.
    pub fn server_steam_id(&self) -> Option<SteamId> {
        self.core.admitted(SERVER_PEER).and_then(|p| p.steam_id)
    }

    /// True while the dial is waiting for the relay network.
    pub fn waiting_for_relay(&self) -> bool {
        matches!(&self.core.role, Role::Client { dial: Some(_), .. })
    }
}

macro_rules! endpoint_impl {
    ($ty:ty) => {
        impl MessageEndpoint for $ty {
            fn transport(&self) -> TransportKind {
                TransportKind::Steam
            }
            fn security(&self) -> TransportSecurity {
                TransportSecurity::PlatformEncrypted
            }
            fn connected_peers(&self) -> Vec<PeerId> {
                self.core
                    .peers
                    .iter()
                    .filter(|(_, p)| p.state == State::Admitted)
                    .map(|(id, _)| *id)
                    .collect()
            }
            fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
                self.core
                    .admitted(peer)
                    .map(|_| self.core.max_payload(delivery))
            }
            fn send(
                &mut self,
                peer: PeerId,
                lane: Lane,
                delivery: Delivery,
                payload: &[u8],
            ) -> Result<()> {
                self.core.send(peer, lane, delivery, payload)
            }
            fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
                self.core.poll(budget)
            }
            fn stats(&self, peer: PeerId) -> Option<LinkStats> {
                self.core.stats(peer)
            }
            fn disconnect(&mut self, peer: PeerId) -> bool {
                self.core.disconnect(peer)
            }
        }
    };
}

endpoint_impl!(SteamServerEndpoint);
endpoint_impl!(SteamClientEndpoint);
