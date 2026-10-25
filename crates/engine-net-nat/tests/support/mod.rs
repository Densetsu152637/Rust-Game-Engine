//! Shared test fixtures: an in-process NAT simulator over loopback sockets, synthetic TLS
//! identities and polling helpers.
#![allow(dead_code)]
use engine_net_nat::*;
use engine_net_quic::{
    DatagramSocket, QuicClient, QuicClientConfig, QuicClientSecurity, QuicServer, QuicServerConfig,
    QuicServerSecurity,
};
use engine_network::{
    CertificateDer, Delivery, Lane, LinkEvent, MessageEndpoint, PeerId, PollBudget, PrivateKeyDer,
};
use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapping {
    /// One public port per private socket (all cone NATs).
    EndpointIndependent,
    /// A new public port per destination (symmetric NAT).
    AddressAndPortDependent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filtering {
    EndpointIndependent,
    /// Inbound only from IPs we sent to. With a single loopback IP this degenerates to
    /// endpoint-independent filtering (documented limitation of the simulator).
    AddressDependent,
    /// Inbound only from exact (IP, port) pairs we sent to.
    AddressAndPortDependent,
}
#[derive(Clone, Copy, Debug)]
pub struct NatType {
    pub name: &'static str,
    pub mapping: Mapping,
    pub filtering: Filtering,
}
pub const FULL_CONE: NatType = NatType {
    name: "full cone",
    mapping: Mapping::EndpointIndependent,
    filtering: Filtering::EndpointIndependent,
};
pub const RESTRICTED_CONE: NatType = NatType {
    name: "restricted cone",
    mapping: Mapping::EndpointIndependent,
    filtering: Filtering::AddressDependent,
};
pub const PORT_RESTRICTED: NatType = NatType {
    name: "port-restricted cone",
    mapping: Mapping::EndpointIndependent,
    filtering: Filtering::AddressAndPortDependent,
};
pub const SYMMETRIC: NatType = NatType {
    name: "symmetric",
    mapping: Mapping::AddressAndPortDependent,
    filtering: Filtering::AddressAndPortDependent,
};

const MAX_MAPPINGS: usize = 64;
const MAX_PERMITS: usize = 256;

struct MappingEntry {
    destination: Option<SocketAddr>,
    socket: UdpSocket,
    permits: Vec<SocketAddr>,
}

/// A private host behind a NAT, as a [`DatagramSocket`]. Each public mapping is a real
/// nonblocking socket on `127.0.0.1:0`; filtering drops unsolicited inbound datagrams.
/// Destinations outside loopback (for example private host candidates) are black-holed, so
/// nothing ever leaves the machine.
pub struct SimulatedNat {
    nat: NatType,
    private: SocketAddr,
    mappings: Vec<MappingEntry>,
    cursor: usize,
    filtered: Arc<AtomicU64>,
}
static NEXT_HOST: AtomicU16 = AtomicU16::new(1);
impl SimulatedNat {
    pub fn new(nat: NatType) -> Self {
        let id = NEXT_HOST.fetch_add(1, Ordering::Relaxed);
        Self {
            nat,
            private: SocketAddr::from(([192, 168, (id % 250) as u8, 2], 40_000 + id)),
            mappings: Vec::new(),
            cursor: 0,
            filtered: Arc::default(),
        }
    }
    /// Inbound datagrams dropped by filtering (shared, readable after the socket moved).
    pub fn filtered(&self) -> Arc<AtomicU64> {
        self.filtered.clone()
    }
    pub fn public_addrs(&self) -> Vec<SocketAddr> {
        self.mappings
            .iter()
            .map(|m| m.socket.local_addr().unwrap())
            .collect()
    }
    fn mapping_for(&mut self, destination: SocketAddr) -> io::Result<Option<usize>> {
        let key = match self.nat.mapping {
            Mapping::EndpointIndependent => None,
            Mapping::AddressAndPortDependent => Some(destination),
        };
        if let Some(index) = self.mappings.iter().position(|m| m.destination == key) {
            return Ok(Some(index));
        }
        if self.mappings.len() >= MAX_MAPPINGS {
            return Ok(None);
        }
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.set_nonblocking(true)?;
        self.mappings.push(MappingEntry {
            destination: key,
            socket,
            permits: Vec::new(),
        });
        Ok(Some(self.mappings.len() - 1))
    }
    fn permitted(&self, index: usize, from: SocketAddr) -> bool {
        let permits = &self.mappings[index].permits;
        match self.nat.filtering {
            Filtering::EndpointIndependent => true,
            Filtering::AddressDependent => permits.iter().any(|p| p.ip() == from.ip()),
            Filtering::AddressAndPortDependent => permits.contains(&from),
        }
    }
}
impl DatagramSocket for SimulatedNat {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        if !destination.ip().is_loopback() {
            return Ok(datagram.len());
        }
        let Some(index) = self.mapping_for(destination)? else {
            return Ok(datagram.len());
        };
        let entry = &mut self.mappings[index];
        if !entry.permits.contains(&destination) && entry.permits.len() < MAX_PERMITS {
            entry.permits.push(destination);
        }
        entry.socket.send_to(datagram, destination)
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let count = self.mappings.len();
        let mut empty = 0;
        let mut reads = 0;
        while count > 0 && empty < count && reads < 1024 {
            let index = self.cursor % count;
            match self.mappings[index].socket.recv_from(buffer) {
                Ok((len, from)) => {
                    reads += 1;
                    if self.permitted(index, from) {
                        return Ok((len, from));
                    }
                    self.filtered.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    empty += 1;
                    self.cursor += 1;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::Interrupted
                    ) =>
                {
                    reads += 1;
                }
                Err(e) => return Err(e),
            }
        }
        Err(io::ErrorKind::WouldBlock.into())
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.private)
    }
}

pub fn udp() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
}

/// Polls `test` with short sleeps until true; panics after `limit`. Returns the time taken.
pub fn until(limit: Duration, what: &str, mut test: impl FnMut() -> bool) -> Duration {
    let start = Instant::now();
    while !test() {
        assert!(start.elapsed() < limit, "timed out: {what}");
        thread::sleep(Duration::from_millis(1));
    }
    start.elapsed()
}

pub const CHANNEL: ChannelId = ChannelId([0x42; 16]);
pub const RELAY_SECRET: [u8; 32] = [0x5A; 32];

/// A discovery + relay service on loopback using HMAC credentials keyed by `RELAY_SECRET`.
pub fn relay_service(limits: RelayLimits) -> RelayService<UdpSocket> {
    let credentials = HmacCredentials::new(&RELAY_SECRET).unwrap();
    let server = RelayServer::new(Instant::now(), limits, credentials.into_authorizer()).unwrap();
    RelayService::new(udp(), server)
}
pub fn relay_credential(party: Party) -> Vec<u8> {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    HmacCredentials::new(&RELAY_SECRET)
        .unwrap()
        .mint(CHANNEL, party, expiry)
}

/// Server-reflexive address of `socket` as seen by `service`.
pub fn discover(
    socket: &mut dyn DatagramSocket,
    service: &mut RelayService<UdpSocket>,
) -> SocketAddr {
    let server = service.local_addr().unwrap();
    let mut client =
        ReflexiveClient::new(Instant::now(), server, ReflexiveConfig::default()).unwrap();
    until(Duration::from_secs(5), "reflexive discovery", || {
        drive_agents(socket, Instant::now(), &mut [&mut client], 64).unwrap();
        service.poll(64).unwrap();
        drive_agents(socket, Instant::now(), &mut [&mut client], 64).unwrap();
        matches!(client.outcome(), ReflexiveOutcome::Discovered(_))
    });
    match client.outcome() {
        ReflexiveOutcome::Discovered(addr) => addr,
        other => panic!("{other:?}"),
    }
}

// --- Synthetic identities (as in engine-net-quic tests) -----------------------------------

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}
impl Identity {
    pub fn new(name: &str) -> Self {
        let CertifiedKey { cert, signing_key }: CertifiedKey<KeyPair> =
            generate_simple_self_signed(vec![name.into()]).unwrap();
        Self {
            cert: cert.der().clone(),
            key: PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
        }
    }
}
/// A host identity plus client identities it admits (mutual TLS, allowlist policy).
pub struct Pki {
    pub host: Identity,
    pub clients: Vec<Identity>,
}
impl Pki {
    pub fn new(clients: usize) -> Self {
        Self {
            host: Identity::new("localhost"),
            clients: (0..clients)
                .map(|i| Identity::new(&format!("player{i}")))
                .collect(),
        }
    }
    pub fn server(&self, socket: impl DatagramSocket + 'static) -> QuicServer {
        let allowed: Vec<_> = self.clients.iter().map(|c| c.cert.clone()).collect();
        let security = QuicServerSecurity::new(
            vec![self.host.cert.clone()],
            self.host.key.clone_key(),
            allowed.clone(),
            move |_, chain| chain.first().is_some_and(|c| allowed.contains(c)),
        )
        .unwrap();
        QuicServer::with_socket(socket, QuicServerConfig::default(), security).unwrap()
    }
    pub fn client(
        &self,
        index: usize,
        socket: impl DatagramSocket + 'static,
        server: SocketAddr,
    ) -> QuicClient {
        let me = &self.clients[index];
        let security = QuicClientSecurity::new(
            "localhost",
            [self.host.cert.clone()],
            vec![me.cert.clone()],
            me.key.clone_key(),
        )
        .unwrap();
        QuicClient::with_socket(socket, server, QuicClientConfig::default(), security).unwrap()
    }
}

pub fn poll(endpoint: &mut dyn MessageEndpoint) -> Vec<LinkEvent> {
    endpoint.poll(PollBudget::default()).unwrap()
}
pub fn payloads(events: Vec<LinkEvent>) -> Vec<(PeerId, Vec<u8>)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            LinkEvent::Message { peer, payload, .. } => Some((peer, payload)),
            _ => None,
        })
        .collect()
}
pub fn send(endpoint: &mut dyn MessageEndpoint, peer: PeerId, payload: &[u8]) {
    endpoint
        .send(peer, Lane::RPC, Delivery::ReliableOrdered, payload)
        .unwrap();
}

/// Mean application round trip of `rounds` reliable echo exchanges. `pump` polls every
/// participant once (server first); the server echoes each message back.
pub fn mean_echo_rtt(
    rounds: u32,
    server: &mut dyn MessageEndpoint,
    client: &mut dyn MessageEndpoint,
    server_peer: PeerId,
    mut pump: impl FnMut(),
) -> Duration {
    let mut total = Duration::ZERO;
    for round in 0..rounds {
        let start = Instant::now();
        send(client, engine_network::SERVER_PEER, &round.to_be_bytes());
        let mut echoed = false;
        while !echoed {
            assert!(start.elapsed() < Duration::from_secs(10), "echo timed out");
            pump();
            for (peer, payload) in payloads(poll(server)) {
                assert_eq!(peer, server_peer);
                send(server, peer, &payload);
            }
            echoed = payloads(poll(client))
                .iter()
                .any(|(_, p)| p[..] == round.to_be_bytes());
            thread::yield_now();
        }
        total += start.elapsed();
    }
    total / rounds
}
