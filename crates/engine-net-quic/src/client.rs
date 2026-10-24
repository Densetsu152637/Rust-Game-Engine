//! QUIC client endpoint driven synchronously inside `poll`.
use crate::{
    DatagramSocket, QuicClientConfig, QuicClientSecurity,
    link::{Fault, Link, Phase, close},
    security::QUIC_ALPN,
    server::exchange,
    socket::{Received, bind_nonblocking, recv, send_lossy},
};
use bytes::BytesMut;
use engine_network::{
    Delivery, Error, Lane, LinkEvent, LinkStats, MessageEndpoint, PeerId, PollBudget, Result,
    SERVER_PEER, TransportKind, TransportSecurity,
};
use quinn_proto::{ClientConfig, DatagramEvent, Endpoint, EndpointConfig};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Instant,
};

const RECEIVE_BUFFER_BYTES: usize = 64 * 1024;

/// Mutually authenticated QUIC client. Its only peer is [`SERVER_PEER`]. `Authenticated` and
/// `Connected` follow the server's welcome, which the server sends only after its own
/// verification and policy passed; a failure at any stage yields `Disconnected` (also before
/// `Connected`). All work happens inside [`MessageEndpoint::poll`].
pub struct QuicClient {
    socket: Box<dyn DatagramSocket>,
    endpoint: Endpoint,
    limits: crate::QuicLimits,
    pub(crate) link: Option<Link>,
    reported: bool,
    receive_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
}
impl QuicClient {
    /// Binds an ephemeral nonblocking UDP socket (loopback when the server is loopback,
    /// otherwise the unspecified address of the server's family) and starts the handshake.
    pub fn connect(
        server_addr: SocketAddr,
        config: QuicClientConfig,
        security: QuicClientSecurity,
    ) -> Result<Self> {
        let bind: SocketAddr = match server_addr {
            SocketAddr::V4(v4) if v4.ip().is_loopback() => (Ipv4Addr::LOCALHOST, 0).into(),
            SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            SocketAddr::V6(v6) if v6.ip().is_loopback() => (Ipv6Addr::LOCALHOST, 0).into(),
            SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        Self::with_socket(bind_nonblocking(bind)?, server_addr, config, security)
    }
    /// Uses a caller-supplied nonblocking socket (for example a NAT-traversed or relayed path).
    pub fn with_socket(
        socket: impl DatagramSocket + 'static,
        server_addr: SocketAddr,
        config: QuicClientConfig,
        security: QuicClientSecurity,
    ) -> Result<Self> {
        config.limits.validate()?;
        let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None, true, None);
        let mut client_config = ClientConfig::new(security.config.clone());
        client_config.transport_config(config.limits.transport(config.congestion));
        let now = Instant::now();
        let (handle, conn) = endpoint
            .connect(now, client_config, server_addr, &security.server_name)
            .map_err(|_| Error::Invalid("QUIC connect setup failed"))?;
        Ok(Self {
            socket: Box::new(socket),
            endpoint,
            limits: config.limits,
            link: Some(Link::new(conn, handle, SERVER_PEER, now)),
            reported: false,
            receive_buffer: vec![0; RECEIVE_BUFFER_BYTES],
            send_buffer: Vec::new(),
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }
    pub fn is_connected(&self) -> bool {
        self.admitted().is_some()
    }
    fn admitted(&self) -> Option<&Link> {
        self.link
            .as_ref()
            .filter(|link| link.phase == Phase::Admitted)
    }
    fn intake(&mut self, now: Instant) -> Result<()> {
        for _ in 0..self.limits.max_datagrams_per_poll {
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
                Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                    if let Some(link) = self.link.as_mut().filter(|link| link.handle == handle) {
                        link.conn.handle_event(event);
                    }
                }
                Some(DatagramEvent::Response(transmit)) => send_lossy(
                    self.socket.as_mut(),
                    &self.send_buffer[..transmit.size],
                    transmit.destination,
                ),
                // A client endpoint has no server configuration and accepts nothing.
                Some(DatagramEvent::NewConnection(incoming)) => self.endpoint.ignore(incoming),
                None => {}
            }
        }
        Ok(())
    }
}

/// The TLS layer has verified the server chain and name; this adds the ALPN check and waits
/// for the welcome that proves the server admitted us.
fn advance(link: &mut Link, limits: &crate::QuicLimits) -> std::result::Result<bool, Fault> {
    if link.phase != Phase::Established {
        return Ok(false);
    }
    if !link.negotiated_alpn(QUIC_ALPN) {
        return Err(Fault::protocol("ALPN negotiation failed"));
    }
    link.receive_welcome(limits)
}

impl MessageEndpoint for QuicClient {
    fn transport(&self) -> TransportKind {
        TransportKind::Quic
    }
    fn security(&self) -> TransportSecurity {
        TransportSecurity::MutualTls13
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        if self.is_connected() {
            vec![SERVER_PEER]
        } else {
            Vec::new()
        }
    }
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
        if peer != SERVER_PEER {
            return None;
        }
        self.admitted()?.max_payload(delivery, &self.limits)
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        if !lane.is_valid() {
            return Err(Error::Invalid("lane exceeds Lane::MAX"));
        }
        let link = self
            .link
            .as_mut()
            .filter(|link| peer == SERVER_PEER && link.phase == Phase::Admitted)
            .ok_or(Error::Invalid("unknown or unadmitted peer"))?;
        link.send(lane, delivery, payload, &self.limits)
    }
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        let now = Instant::now();
        self.intake(now)?;
        let mut events = Vec::new();
        let Some(link) = self.link.as_mut() else {
            return Ok(events);
        };
        let mut lost = link.drive_timers_and_events(now);
        if lost.is_none() {
            match advance(link, &self.limits) {
                Ok(true) => {
                    events.push(LinkEvent::Authenticated(SERVER_PEER));
                    events.push(LinkEvent::Connected(SERVER_PEER));
                }
                Ok(false) => {}
                Err(fault) => {
                    link.close(now, fault.code);
                    lost = Some(fault.reason.to_owned());
                }
            }
        }
        if link.handshake_expired(now, &self.limits) {
            link.close(now, close::HANDSHAKE_TIMEOUT);
            lost = Some("handshake timeout".into());
        }
        if link.phase == Phase::Admitted
            && let Err(fault) = exchange(link, true, now, budget, &self.limits, &mut events)
        {
            link.close(now, fault.code);
            lost = Some(fault.reason.to_owned());
        }
        link.transmit(
            now,
            self.socket.as_mut(),
            &mut self.send_buffer,
            self.limits.max_datagrams_per_poll,
        );
        link.exchange_endpoint_events(&mut self.endpoint);
        if link.phase == Phase::Closed && !self.reported {
            self.reported = true;
            events.push(LinkEvent::Disconnected {
                peer: SERVER_PEER,
                reason: lost.unwrap_or_else(|| "closed".into()),
            });
        }
        if link.conn.is_drained() {
            self.link = None;
        }
        Ok(events)
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        if peer != SERVER_PEER {
            return None;
        }
        self.admitted().map(Link::stats)
    }
    /// Sends CONNECTION_CLOSE immediately; no `Disconnected` event follows a local disconnect.
    fn disconnect(&mut self, peer: PeerId) -> bool {
        let Some(link) = self
            .link
            .as_mut()
            .filter(|link| peer == SERVER_PEER && link.is_live())
        else {
            return false;
        };
        let now = Instant::now();
        link.close(now, close::NORMAL);
        self.reported = true;
        link.transmit(
            now,
            self.socket.as_mut(),
            &mut self.send_buffer,
            self.limits.max_datagrams_per_poll,
        );
        true
    }
}
