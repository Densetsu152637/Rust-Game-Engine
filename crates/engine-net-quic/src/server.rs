//! QUIC server endpoint driven synchronously inside `poll`.
use crate::{
    DatagramSocket, QuicServerConfig, QuicServerSecurity,
    link::{Budget, Fault, Link, Phase, close},
    security::QUIC_ALPN,
    socket::{Received, bind_nonblocking, recv, send_lossy},
};
use bytes::BytesMut;
use engine_network::{
    Delivery, Error, Lane, LinkEvent, LinkStats, MessageEndpoint, PeerId, PollBudget, Result,
    TransportKind, TransportSecurity,
};
use quinn_proto::{DatagramEvent, Endpoint, EndpointConfig, Incoming, ServerConfig};
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Instant};

const RECEIVE_BUFFER_BYTES: usize = 64 * 1024;

/// Mutually authenticated QUIC server. All socket, timer and handshake work happens inside
/// [`MessageEndpoint::poll`]; no method blocks, sleeps or spawns threads. Peers appear as
/// `Authenticated` then `Connected` only after TLS 1.3 verification, the ALPN check and the
/// native policy all pass; `Disconnected` is reported only for admitted peers.
pub struct QuicServer {
    socket: Box<dyn DatagramSocket>,
    endpoint: Endpoint,
    config: QuicServerConfig,
    authorize: Arc<crate::security::Authorize>,
    links: BTreeMap<usize, Link>,
    peers: BTreeMap<PeerId, usize>,
    next_peer: PeerId,
    receive_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
}
impl QuicServer {
    /// Binds a nonblocking UDP socket.
    pub fn bind(
        addr: SocketAddr,
        config: QuicServerConfig,
        security: QuicServerSecurity,
    ) -> Result<Self> {
        Self::with_socket(bind_nonblocking(addr)?, config, security)
    }
    /// Uses a caller-supplied nonblocking socket (for example a NAT-traversed or relayed path).
    pub fn with_socket(
        socket: impl DatagramSocket + 'static,
        config: QuicServerConfig,
        security: QuicServerSecurity,
    ) -> Result<Self> {
        config.limits.validate()?;
        let mut server_config = ServerConfig::with_crypto(security.config.clone());
        server_config
            .transport_config(config.limits.transport(config.congestion))
            .max_incoming(config.limits.max_pending_handshakes)
            .incoming_buffer_size(64 * 1024)
            .incoming_buffer_size_total(64 * 1024 * config.limits.max_pending_handshakes as u64);
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::new(server_config)),
            true,
            None,
        );
        Ok(Self {
            socket: Box::new(socket),
            endpoint,
            config,
            authorize: security.authorize,
            links: BTreeMap::new(),
            peers: BTreeMap::new(),
            next_peer: 1,
            receive_buffer: vec![0; RECEIVE_BUFFER_BYTES],
            send_buffer: Vec::new(),
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }
    /// Connections in the handshake or authorization phase.
    pub fn pending_peers(&self) -> usize {
        self.links.values().filter(|link| link.is_pending()).count()
    }
    fn admitted(&self, peer: PeerId) -> Option<&Link> {
        self.peers
            .get(&peer)
            .and_then(|key| self.links.get(key))
            .filter(|link| link.phase == Phase::Admitted)
    }

    fn intake(&mut self, now: Instant, max_accepts: usize) -> Result<()> {
        let mut accepted = 0;
        for _ in 0..self.config.limits.max_datagrams_per_poll {
            let (len, from) = match recv(self.socket.as_mut(), &mut self.receive_buffer)? {
                Received::Datagram(len, from) => (len, from),
                Received::Transient => continue,
                Received::Empty => break,
            };
            self.send_buffer.clear();
            let data = BytesMut::from(&self.receive_buffer[..len]);
            match self
                .endpoint
                .handle(now, from, None, None, data, &mut self.send_buffer)
            {
                None => {}
                Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                    if let Some(link) = self.links.get_mut(&handle.0) {
                        link.conn.handle_event(event);
                    }
                }
                Some(DatagramEvent::Response(transmit)) => send_lossy(
                    self.socket.as_mut(),
                    &self.send_buffer[..transmit.size],
                    transmit.destination,
                ),
                Some(DatagramEvent::NewConnection(incoming)) => {
                    self.admit(incoming, now, &mut accepted, max_accepts)?;
                }
            }
        }
        Ok(())
    }
    /// Retry first (stateless, so caps count only validated addresses), then the budgets/caps.
    fn admit(
        &mut self,
        incoming: Incoming,
        now: Instant,
        accepted: &mut usize,
        max_accepts: usize,
    ) -> Result<()> {
        self.send_buffer.clear();
        if self.config.require_address_validation && !incoming.remote_address_validated() {
            let transmit = if incoming.may_retry() {
                match self.endpoint.retry(incoming, &mut self.send_buffer) {
                    Ok(transmit) => transmit,
                    Err(error) => {
                        self.endpoint.ignore(error.into_incoming());
                        return Ok(());
                    }
                }
            } else {
                self.endpoint.refuse(incoming, &mut self.send_buffer)
            };
            return self.respond(transmit);
        }
        if *accepted >= max_accepts {
            // The client retransmits its Initial; a later poll may accept it.
            self.endpoint.ignore(incoming);
            return Ok(());
        }
        let live = self.links.values().filter(|link| link.is_live()).count();
        if live >= self.config.limits.max_peers
            || self.pending_peers() >= self.config.limits.max_pending_handshakes
        {
            let transmit = self.endpoint.refuse(incoming, &mut self.send_buffer);
            return self.respond(transmit);
        }
        let peer = self.next_peer;
        self.next_peer = peer
            .checked_add(1)
            .ok_or(Error::Invalid("peer IDs exhausted"))?;
        match self
            .endpoint
            .accept(incoming, now, &mut self.send_buffer, None)
        {
            Ok((handle, conn)) => {
                *accepted += 1;
                self.links
                    .insert(handle.0, Link::new(conn, handle, peer, now));
                Ok(())
            }
            Err(error) => match error.response {
                Some(transmit) => self.respond(transmit),
                None => Ok(()),
            },
        }
    }
    fn respond(&mut self, transmit: quinn_proto::Transmit) -> Result<()> {
        send_lossy(
            self.socket.as_mut(),
            &self.send_buffer[..transmit.size],
            transmit.destination,
        );
        Ok(())
    }

    /// Advances one connection: timers, authorization, bounded receive and send, transmit.
    fn drive(&mut self, key: usize, now: Instant, budget: PollBudget, events: &mut Vec<LinkEvent>) {
        let Some(link) = self.links.get_mut(&key) else {
            return;
        };
        let mut lost = link.drive_timers_and_events(now);
        if lost.is_none() && link.phase == Phase::Established {
            match authorize(link, self.authorize.as_ref()) {
                Ok(()) => {
                    self.peers.insert(link.peer, key);
                    events.push(LinkEvent::Authenticated(link.peer));
                    events.push(LinkEvent::Connected(link.peer));
                }
                Err(fault) => link.close(now, fault.code),
            }
        }
        if link.handshake_expired(now, &self.config.limits) {
            link.close(now, close::HANDSHAKE_TIMEOUT);
        }
        if link.phase == Phase::Admitted
            && let Err(fault) = exchange(link, false, now, budget, &self.config.limits, events)
        {
            link.close(now, fault.code);
            lost = Some(fault.reason.to_owned());
        }
        let max = self.config.limits.max_datagrams_per_poll;
        link.transmit(now, self.socket.as_mut(), &mut self.send_buffer, max);
        link.exchange_endpoint_events(&mut self.endpoint);
        // `peers` holds admitted peers only, so failed handshakes emit nothing.
        if link.phase == Phase::Closed && self.peers.remove(&link.peer).is_some() {
            events.push(LinkEvent::Disconnected {
                peer: link.peer,
                reason: lost.unwrap_or_else(|| "closed".into()),
            });
        }
        if link.conn.is_drained() {
            self.links.remove(&key);
        }
    }
}

/// Runs the TLS 1.3 + ALPN + certificate-presence checks and native policy exactly once.
fn authorize(
    link: &mut Link,
    policy: &crate::security::Authorize,
) -> std::result::Result<(), Fault> {
    if !link.negotiated_alpn(QUIC_ALPN) {
        return Err(Fault::protocol("ALPN negotiation failed"));
    }
    let chain = link.peer_chain().ok_or(Fault {
        code: close::UNAUTHORIZED,
        reason: "missing client identity",
    })?;
    if !policy(link.peer, &chain) {
        return Err(Fault {
            code: close::UNAUTHORIZED,
            reason: "client identity unauthorized",
        });
    }
    link.admit_and_welcome()
}

/// Bounded receive and send for an admitted connection, shared with the client.
pub(crate) fn exchange(
    link: &mut Link,
    client: bool,
    now: Instant,
    budget: PollBudget,
    limits: &crate::QuicLimits,
    events: &mut Vec<LinkEvent>,
) -> std::result::Result<(), Fault> {
    let mut budget = Budget {
        messages: budget.max_messages,
        bytes: budget.max_bytes,
    };
    for message in link.receive(&mut budget, client, limits, now)? {
        events.push(LinkEvent::Message {
            peer: link.peer,
            lane: message.lane,
            delivery: message.delivery,
            payload: message.payload,
        });
    }
    link.flush()?;
    link.refresh_datagram_limit();
    Ok(())
}

impl MessageEndpoint for QuicServer {
    fn transport(&self) -> TransportKind {
        TransportKind::Quic
    }
    fn security(&self) -> TransportSecurity {
        TransportSecurity::MutualTls13
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        self.peers
            .keys()
            .copied()
            .filter(|&peer| self.admitted(peer).is_some())
            .collect()
    }
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
        self.admitted(peer)?
            .max_payload(delivery, &self.config.limits)
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        if !lane.is_valid() {
            return Err(Error::Invalid("lane exceeds Lane::MAX"));
        }
        let link = self
            .peers
            .get(&peer)
            .and_then(|key| self.links.get_mut(key))
            .filter(|link| link.phase == Phase::Admitted)
            .ok_or(Error::Invalid("unknown or unadmitted peer"))?;
        link.send(lane, delivery, payload, &self.config.limits)
    }
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        let now = Instant::now();
        self.intake(
            now,
            budget
                .max_accepts
                .min(self.config.limits.max_pending_handshakes),
        )?;
        let mut events = Vec::new();
        let keys: Vec<usize> = self.links.keys().copied().collect();
        for key in keys {
            self.drive(key, now, budget, &mut events);
        }
        Ok(events)
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        self.admitted(peer).map(Link::stats)
    }
    /// Sends CONNECTION_CLOSE immediately; the connection drains during later polls.
    fn disconnect(&mut self, peer: PeerId) -> bool {
        let Some(key) = self.peers.remove(&peer) else {
            return false;
        };
        if let Some(link) = self.links.get_mut(&key) {
            let now = Instant::now();
            link.close(now, close::NORMAL);
            link.transmit(
                now,
                self.socket.as_mut(),
                &mut self.send_buffer,
                self.config.limits.max_datagrams_per_poll,
            );
        }
        true
    }
}
