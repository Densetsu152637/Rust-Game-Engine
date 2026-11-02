//! Networking Sockets half of the real backend. Everything runs on the tick thread;
//! Steam delivers connection events while `Client::run_callbacks` runs, and
//! [`RealNet::poll_events`] turns the per-socket event queues into [`NetEvent`]s.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use steamworks::networking_sockets::{ListenSocket, NetConnection};
use steamworks::networking_types::{
    AppNetConnectionEnd, ConnectionRequest, ListenSocketEvent, NetConnectionEnd,
    NetworkingAvailability, NetworkingConnectionState, NetworkingIdentity, SendFlags,
};
use steamworks::{Client, SteamId as RawSteamId};

use crate::error::{SteamError, SteamResult};
use crate::net::*;
use crate::types::SteamId;

/// `k_cbMaxSteamNetworkingSocketsMessageSizeSend`.
const MAX_MESSAGE: usize = 512 * 1024;

pub(crate) type AuthEvents = Arc<Mutex<Vec<NetEvent>>>;

struct Accepted {
    conn: u32,
    listen: u32,
    remote: NetRemote,
}

#[derive(Default)]
pub(crate) struct RealNet {
    next_id: u32,
    listens: BTreeMap<u32, ListenSocket>,
    conns: BTreeMap<u32, NetConnection>,
    /// Outgoing connections and who they dial; their events come from the connection itself.
    outgoing: BTreeMap<u32, NetRemote>,
    requests: BTreeMap<u32, (u32, ConnectionRequest)>,
    accepted: Vec<Accepted>,
    events: VecDeque<NetEvent>,
    relay_requested: bool,
    pub(crate) auth_events: AuthEvents,
}

fn remote_of(identity: &NetworkingIdentity) -> NetRemote {
    identity
        .steam_id()
        .map_or(NetRemote::Unknown, |id| NetRemote::Steam(SteamId(id.raw())))
}

fn nul_free(text: &str) -> String {
    text.replace('\0', "")
}

fn closed(code: impl std::fmt::Debug) -> String {
    format!("connection ended: {code:?}")
}

impl RealNet {
    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub(crate) fn init_relay(&mut self, client: &Client) {
        self.relay_requested = true;
        client.networking_utils().init_relay_network_access();
    }

    pub(crate) fn relay_status(&self, client: &Client) -> RelayStatus {
        if !self.relay_requested {
            return RelayStatus::NotStarted;
        }
        match client.networking_utils().relay_network_status() {
            Ok(NetworkingAvailability::Current) => RelayStatus::Ready,
            Ok(_) => RelayStatus::Pending,
            Err(error) => RelayStatus::Failed(error.to_string()),
        }
    }

    pub(crate) fn max_message(&self) -> usize {
        MAX_MESSAGE
    }

    pub(crate) fn listen_p2p(&mut self, client: &Client, port: i32) -> SteamResult<NetListenId> {
        let socket = client
            .networking_sockets()
            .create_listen_socket_p2p(port, [])
            .map_err(|_| SteamError::Rejected("could not create the P2P listen socket".into()))?;
        let id = self.id();
        self.listens.insert(id, socket);
        Ok(NetListenId(id))
    }

    pub(crate) fn listen_ip(
        &mut self,
        client: &Client,
        addr: SocketAddr,
    ) -> SteamResult<NetListenId> {
        let socket = client
            .networking_sockets()
            .create_listen_socket_ip(addr, [])
            .map_err(|_| SteamError::Rejected(format!("could not listen on {addr}")))?;
        let id = self.id();
        self.listens.insert(id, socket);
        Ok(NetListenId(id))
    }

    pub(crate) fn close_listen(&mut self, listen: NetListenId) -> SteamResult<()> {
        // Dropping the socket rejects further connection requests.
        self.listens
            .remove(&listen.0)
            .map(|_| ())
            .ok_or(SteamError::ConnectionClosed)
    }

    pub(crate) fn connect_p2p(
        &mut self,
        client: &Client,
        peer: SteamId,
        port: i32,
    ) -> SteamResult<NetConnId> {
        let identity = NetworkingIdentity::new_steam_id(RawSteamId::from_raw(peer.0));
        let conn = client
            .networking_sockets()
            .connect_p2p(identity, port, [])
            .map_err(|_| SteamError::Rejected("could not start the P2P connection".into()))?;
        let id = self.id();
        self.conns.insert(id, conn);
        self.outgoing.insert(id, NetRemote::Steam(peer));
        Ok(NetConnId(id))
    }

    pub(crate) fn connect_ip(
        &mut self,
        client: &Client,
        addr: SocketAddr,
    ) -> SteamResult<NetConnId> {
        let conn = client
            .networking_sockets()
            .connect_by_ip_address(addr, [])
            .map_err(|_| SteamError::Rejected(format!("could not connect to {addr}")))?;
        let id = self.id();
        self.conns.insert(id, conn);
        self.outgoing.insert(id, NetRemote::Ip(addr));
        Ok(NetConnId(id))
    }

    pub(crate) fn accept(&mut self, conn: NetConnId) -> SteamResult<()> {
        let (listen, request) = self
            .requests
            .remove(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        let remote = remote_of(&request.remote());
        request
            .accept()
            .map_err(|e| SteamError::Rejected(format!("accept failed: {e}")))?;
        self.accepted.push(Accepted {
            conn: conn.0,
            listen,
            remote,
        });
        Ok(())
    }

    pub(crate) fn close(&mut self, conn: NetConnId, reason: &str, linger: bool) -> SteamResult<()> {
        let end = || NetConnectionEnd::App(AppNetConnectionEnd::generic_normal());
        let reason = nul_free(reason);
        if let Some((_, request)) = self.requests.remove(&conn.0) {
            request.reject(end(), Some(&reason));
            return Ok(());
        }
        self.accepted.retain(|a| a.conn != conn.0);
        self.outgoing.remove(&conn.0);
        let connection = self
            .conns
            .remove(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        connection.close(end(), Some(&reason), linger);
        Ok(())
    }

    pub(crate) fn send(
        &mut self,
        conn: NetConnId,
        data: &[u8],
        flags: NetSendFlags,
    ) -> SteamResult<()> {
        if data.len() > MAX_MESSAGE {
            return Err(SteamError::MessageTooLarge);
        }
        let connection = self
            .conns
            .get(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        let flags = match (flags.reliable, flags.no_nagle) {
            (true, true) => SendFlags::RELIABLE_NO_NAGLE,
            (true, false) => SendFlags::RELIABLE,
            (false, true) => SendFlags::UNRELIABLE_NO_NAGLE,
            (false, false) => SendFlags::UNRELIABLE,
        };
        connection
            .send_message(data, flags)
            .map(|_| ())
            .map_err(|error| match error {
                steamworks::SteamError::LimitExceeded => SteamError::SendQueueFull,
                steamworks::SteamError::NoConnection | steamworks::SteamError::InvalidState => {
                    SteamError::ConnectionClosed
                }
                steamworks::SteamError::InvalidParameter => SteamError::MessageTooLarge,
                other => SteamError::Rejected(other.to_string()),
            })
    }

    pub(crate) fn flush(&mut self, conn: NetConnId) -> SteamResult<()> {
        let connection = self
            .conns
            .get(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        connection
            .flush_messages()
            .map_err(|e| SteamError::Rejected(e.to_string()))
    }

    pub(crate) fn receive(&mut self, conn: NetConnId, max: usize) -> SteamResult<Vec<Vec<u8>>> {
        let connection = self
            .conns
            .get_mut(&conn.0)
            .ok_or(SteamError::ConnectionClosed)?;
        let messages = connection
            .receive_messages(max)
            .map_err(|_| SteamError::ConnectionClosed)?;
        Ok(messages.iter().map(|m| m.data().to_vec()).collect())
    }

    pub(crate) fn conn_stats(&self, client: &Client, conn: NetConnId) -> Option<NetConnStats> {
        let connection = self.conns.get(&conn.0)?;
        let (info, _) = client
            .networking_sockets()
            .get_realtime_connection_status(connection, 0)
            .ok()?;
        Some(NetConnStats {
            ping_ms: info.ping().max(0) as f32,
            queued_bytes: (info.pending_unreliable().max(0) as u64)
                .saturating_add(info.pending_reliable().max(0) as u64),
        })
    }

    /// Drains every listen socket and outgoing connection, then queued auth verdicts.
    pub(crate) fn poll_events(&mut self) -> Vec<NetEvent> {
        let listen_ids: Vec<u32> = self.listens.keys().copied().collect();
        for listen_id in listen_ids {
            while let Some(event) = self
                .listens
                .get(&listen_id)
                .and_then(|socket| socket.try_receive_event())
            {
                self.on_listen_event(listen_id, event);
            }
        }
        let outgoing: Vec<(u32, NetRemote)> = self.outgoing.iter().map(|(k, v)| (*k, *v)).collect();
        for (id, remote) in outgoing {
            let mut states = Vec::new();
            if let Some(conn) = self.conns.get(&id) {
                while let Some(event) = conn.try_receive_event() {
                    states.push(event.new_state);
                }
            }
            for state in states {
                match state {
                    NetworkingConnectionState::Connected => {
                        self.events.push_back(NetEvent::Connected {
                            conn: NetConnId(id),
                            remote,
                        });
                    }
                    NetworkingConnectionState::ClosedByPeer
                    | NetworkingConnectionState::ProblemDetectedLocally => {
                        let reason = self
                            .conns
                            .get(&id)
                            .and_then(|c| c.info().ok())
                            .and_then(|info| info.end_reason())
                            .map_or_else(|| "connection ended".to_owned(), closed);
                        self.events.push_back(NetEvent::Closed {
                            conn: NetConnId(id),
                            reason,
                        });
                        self.outgoing.remove(&id);
                    }
                    _ => {}
                }
            }
        }
        let mut auth = self
            .auth_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.events.extend(auth.drain(..));
        drop(auth);
        self.events.drain(..).collect()
    }

    fn on_listen_event(&mut self, listen_id: u32, event: ListenSocketEvent) {
        match event {
            ListenSocketEvent::Connecting(request) => {
                let id = self.id();
                let remote = remote_of(&request.remote());
                self.requests.insert(id, (listen_id, request));
                self.events.push_back(NetEvent::ConnectionRequest {
                    listen: NetListenId(listen_id),
                    conn: NetConnId(id),
                    remote,
                });
            }
            ListenSocketEvent::Connected(connected) => {
                let remote = remote_of(&connected.remote());
                let position = self
                    .accepted
                    .iter()
                    .position(|a| a.listen == listen_id && a.remote == remote);
                let Some(position) = position else {
                    // Not one we accepted (should not happen): close it.
                    connected.take_connection().close(
                        NetConnectionEnd::App(AppNetConnectionEnd::generic_normal()),
                        Some("unexpected connection"),
                        false,
                    );
                    return;
                };
                let id = self.accepted.remove(position).conn;
                let conn = connected.take_connection();
                // The listen socket reports disconnects by user data, not by handle.
                let _ = conn.set_connection_user_data(i64::from(id));
                self.conns.insert(id, conn);
                self.events.push_back(NetEvent::Connected {
                    conn: NetConnId(id),
                    remote,
                });
            }
            ListenSocketEvent::Disconnected(ended) => {
                let reason = closed(ended.end_reason());
                let remote = remote_of(&ended.remote());
                let id = match u32::try_from(ended.user_data()) {
                    Ok(id) if id > 0 => id,
                    _ => {
                        // Ended before it became Connected: find the accepted entry.
                        let Some(position) = self
                            .accepted
                            .iter()
                            .position(|a| a.listen == listen_id && a.remote == remote)
                        else {
                            return;
                        };
                        self.accepted.remove(position).conn
                    }
                };
                self.events.push_back(NetEvent::Closed {
                    conn: NetConnId(id),
                    reason,
                });
            }
        }
    }
}
