//! Deterministic in-process Networking Sockets model for [`crate::FakeBackend`].
//!
//! One backend plays both ends: a connection to a listen socket creates a connected
//! pair of endpoints inside the same model, so server and client endpoints built on
//! clones of one `SteamSession` talk to each other with no Steam. Delivery is
//! immediate and in order; tests inject relay delay, a full send queue, unreliable
//! reordering and auth rejection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;

use crate::error::{SteamError, SteamResult};
use crate::net::*;
use crate::types::SteamId;

/// The largest message the fake accepts, matching Steam's 512 KiB send limit.
pub(crate) const FAKE_MAX_MESSAGE: usize = 512 * 1024;

#[derive(Debug, Clone)]
enum ListenKind {
    P2p(i32),
    Ip(SocketAddr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    /// Created by a remote connect; waiting for `net_accept` or `net_close`.
    Requested,
    /// Outgoing, waiting for the remote to accept.
    Connecting,
    Connected,
    /// The remote ended it; received data can still be read.
    ClosedByPeer,
}

#[derive(Debug, Clone)]
struct Conn {
    peer: Option<u32>,
    state: ConnState,
    inbox: VecDeque<Vec<u8>>,
    inbox_bytes: usize,
    /// The remote as seen from this end.
    remote: NetRemote,
}

#[derive(Debug, Clone)]
pub(crate) struct FakeNet {
    relay_started: bool,
    relay_delay: u32,
    relay_polls: u32,
    relay_failure: Option<String>,
    max_message: usize,
    queue_limit: usize,
    reorder: bool,
    reject_auth: bool,
    next_id: u32,
    listens: BTreeMap<u32, ListenKind>,
    conns: BTreeMap<u32, Conn>,
    events: VecDeque<NetEvent>,
    auth_sessions: BTreeSet<SteamId>,
}

impl Default for FakeNet {
    fn default() -> Self {
        Self {
            relay_started: false,
            relay_delay: 0,
            relay_polls: 0,
            relay_failure: None,
            max_message: FAKE_MAX_MESSAGE,
            queue_limit: 1024 * 1024,
            reorder: false,
            reject_auth: false,
            next_id: 1,
            listens: BTreeMap::new(),
            conns: BTreeMap::new(),
            events: VecDeque::new(),
            auth_sessions: BTreeSet::new(),
        }
    }
}

impl FakeNet {
    pub(crate) fn set_relay_delay(&mut self, polls: u32) {
        self.relay_delay = polls;
    }
    pub(crate) fn set_relay_failure(&mut self, message: Option<String>) {
        self.relay_failure = message;
    }
    pub(crate) fn set_queue_limit(&mut self, bytes: usize) {
        self.queue_limit = bytes;
    }
    pub(crate) fn set_max_message(&mut self, bytes: usize) {
        self.max_message = bytes;
    }
    pub(crate) fn set_reorder(&mut self, on: bool) {
        self.reorder = on;
    }
    pub(crate) fn set_reject_auth(&mut self, on: bool) {
        self.reject_auth = on;
    }
    pub(crate) fn open_connections(&self) -> usize {
        self.conns.len()
    }
    pub(crate) fn active_auth_sessions(&self) -> usize {
        self.auth_sessions.len()
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn init_relay(&mut self) {
        self.relay_started = true;
    }

    pub(crate) fn relay_status(&self) -> RelayStatus {
        if !self.relay_started {
            RelayStatus::NotStarted
        } else if let Some(why) = &self.relay_failure {
            RelayStatus::Failed(why.clone())
        } else if self.relay_polls < self.relay_delay {
            RelayStatus::Pending
        } else {
            RelayStatus::Ready
        }
    }

    pub(crate) fn max_message(&self) -> usize {
        self.max_message
    }

    pub(crate) fn listen_p2p(&mut self, port: i32) -> NetListenId {
        let id = self.id();
        self.listens.insert(id, ListenKind::P2p(port));
        NetListenId(id)
    }

    pub(crate) fn listen_ip(&mut self, addr: SocketAddr) -> SteamResult<NetListenId> {
        if self
            .listens
            .values()
            .any(|l| matches!(l, ListenKind::Ip(a) if *a == addr))
        {
            return Err(SteamError::Rejected(format!("{addr} is already in use")));
        }
        let id = self.id();
        self.listens.insert(id, ListenKind::Ip(addr));
        Ok(NetListenId(id))
    }

    pub(crate) fn close_listen(&mut self, listen: NetListenId) -> SteamResult<()> {
        self.listens
            .remove(&listen.0)
            .map(|_| ())
            .ok_or(SteamError::ConnectionClosed)
    }

    fn connect(&mut self, target: Option<u32>, local: NetRemote, remote: NetRemote) -> NetConnId {
        let client = self.id();
        let blank = |state, remote| Conn {
            peer: None,
            state,
            inbox: VecDeque::new(),
            inbox_bytes: 0,
            remote,
        };
        self.conns
            .insert(client, blank(ConnState::Connecting, remote));
        match target {
            Some(listen) => {
                let server = self.id();
                let mut conn = blank(ConnState::Requested, local);
                conn.peer = Some(client);
                self.conns.insert(server, conn);
                if let Some(c) = self.conns.get_mut(&client) {
                    c.peer = Some(server);
                }
                self.events.push_back(NetEvent::ConnectionRequest {
                    listen: NetListenId(listen),
                    conn: NetConnId(server),
                    remote: local,
                });
            }
            None => {
                self.events.push_back(NetEvent::Closed {
                    conn: NetConnId(client),
                    reason: "no listener at the target".into(),
                });
                if let Some(c) = self.conns.get_mut(&client) {
                    c.state = ConnState::ClosedByPeer;
                }
            }
        }
        NetConnId(client)
    }

    pub(crate) fn connect_p2p(
        &mut self,
        me: SteamId,
        peer: SteamId,
        port: i32,
    ) -> SteamResult<NetConnId> {
        if self.relay_status() != RelayStatus::Ready {
            return Err(SteamError::Rejected(
                "the relay network is not ready".into(),
            ));
        }
        let target = (peer == me)
            .then(|| {
                self.listens
                    .iter()
                    .find(|(_, l)| matches!(l, ListenKind::P2p(p) if *p == port))
                    .map(|(id, _)| *id)
            })
            .flatten();
        Ok(self.connect(target, NetRemote::Steam(me), NetRemote::Steam(peer)))
    }

    pub(crate) fn connect_ip(&mut self, addr: SocketAddr) -> NetConnId {
        let target = self
            .listens
            .iter()
            .find(|(_, l)| matches!(l, ListenKind::Ip(a) if a.port() == addr.port()))
            .map(|(id, _)| *id);
        let client_addr = SocketAddr::from(([127, 0, 0, 1], 40_000 + self.next_id as u16));
        self.connect(target, NetRemote::Ip(client_addr), NetRemote::Ip(addr))
    }

    pub(crate) fn accept(&mut self, conn: NetConnId) -> SteamResult<()> {
        let c = self
            .conns
            .get_mut(&conn.0)
            .filter(|c| c.state == ConnState::Requested)
            .ok_or(SteamError::ConnectionClosed)?;
        c.state = ConnState::Connected;
        let (peer, remote) = (c.peer, c.remote);
        self.events.push_back(NetEvent::Connected { conn, remote });
        if let Some(p) = peer.and_then(|p| self.conns.get_mut(&p).map(|c| (p, c))) {
            p.1.state = ConnState::Connected;
            let remote = p.1.remote;
            self.events.push_back(NetEvent::Connected {
                conn: NetConnId(p.0),
                remote,
            });
        }
        Ok(())
    }

    pub(crate) fn close(&mut self, conn: NetConnId, reason: &str) -> SteamResult<()> {
        let c = self
            .conns
            .remove(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        if let Some(p) = c.peer.and_then(|p| self.conns.get_mut(&p).map(|x| (p, x)))
            && p.1.state != ConnState::ClosedByPeer
        {
            p.1.state = ConnState::ClosedByPeer;
            self.events.push_back(NetEvent::Closed {
                conn: NetConnId(p.0),
                reason: reason.to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn send(
        &mut self,
        conn: NetConnId,
        data: &[u8],
        flags: NetSendFlags,
    ) -> SteamResult<()> {
        if data.len() > self.max_message {
            return Err(SteamError::MessageTooLarge);
        }
        let peer = self
            .conns
            .get(&conn.0)
            .filter(|c| c.state == ConnState::Connected)
            .and_then(|c| c.peer)
            .ok_or(SteamError::ConnectionClosed)?;
        let (limit, reorder) = (self.queue_limit, self.reorder);
        let target = self
            .conns
            .get_mut(&peer)
            .ok_or(SteamError::ConnectionClosed)?;
        if target.inbox_bytes + data.len() > limit {
            return Err(SteamError::SendQueueFull);
        }
        target.inbox_bytes += data.len();
        if reorder && !flags.reliable {
            target.inbox.push_front(data.to_vec());
        } else {
            target.inbox.push_back(data.to_vec());
        }
        Ok(())
    }

    pub(crate) fn receive(&mut self, conn: NetConnId, max: usize) -> SteamResult<Vec<Vec<u8>>> {
        let c = self
            .conns
            .get_mut(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        let mut out = Vec::new();
        while out.len() < max {
            let Some(m) = c.inbox.pop_front() else { break };
            c.inbox_bytes -= m.len();
            out.push(m);
        }
        Ok(out)
    }

    pub(crate) fn conn_stats(&self, conn: NetConnId) -> Option<NetConnStats> {
        let c = self.conns.get(&conn.0)?;
        let queued = c
            .peer
            .and_then(|p| self.conns.get(&p))
            .map_or(0, |p| p.inbox_bytes as u64);
        Some(NetConnStats {
            ping_ms: 1.0,
            queued_bytes: queued,
        })
    }

    pub(crate) fn poll_events(&mut self) -> Vec<NetEvent> {
        self.relay_polls = self.relay_polls.saturating_add(1);
        self.events.drain(..).collect()
    }

    pub(crate) fn begin_auth(&mut self, user: SteamId, valid_ticket: bool) -> SteamResult<()> {
        if !valid_ticket {
            return Err(SteamError::Rejected("invalid ticket".into()));
        }
        if !self.auth_sessions.insert(user) {
            return Err(SteamError::Rejected("duplicate ticket request".into()));
        }
        let accepted = !self.reject_auth;
        self.events.push_back(NetEvent::AuthResult {
            user,
            accepted,
            reason: if accepted {
                String::new()
            } else {
                "rejected by the fake".into()
            },
        });
        Ok(())
    }

    pub(crate) fn end_auth(&mut self, user: SteamId) {
        self.auth_sessions.remove(&user);
    }

    /// Simulates Steam revoking an active auth session.
    pub(crate) fn revoke_auth(&mut self, user: SteamId) {
        self.events.push_back(NetEvent::AuthResult {
            user,
            accepted: false,
            reason: "revoked".into(),
        });
    }
}
