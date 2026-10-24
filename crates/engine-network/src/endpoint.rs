use crate::{
    ClientSecurity, DataFrame, Delivery, Error, InputCommand, Lane, Limits, LinkEvent, LinkStats,
    Message, MessageEndpoint, PeerId, PollBudget, ReplicationState, Result, SERVER_PEER,
    ServerSecurity, Snapshot, TransportKind, TransportSecurity, framing::Framed,
    transport::Transport,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

/// TCP wire version announced by the server welcome. Version 2 added binary data frames for
/// [`MessageEndpoint`] payloads; endpoints accept only their own version, so a version-1 peer
/// fails closed against a version-2 peer instead of misparsing data frames.
pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
    pub limits: Limits,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            limits: Limits::default(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub server_addr: SocketAddr,
    pub connect_timeout: Duration,
    pub limits: Limits,
}
impl ClientConfig {
    pub fn new(server_addr: SocketAddr) -> Self {
        Self {
            server_addr,
            connect_timeout: Duration::from_secs(3),
            limits: Limits::default(),
        }
    }
}

#[derive(Debug)]
pub enum ServerEvent {
    Authenticated(PeerId),
    Connected(PeerId),
    Disconnected {
        peer_id: PeerId,
        reason: String,
    },
    Input {
        peer_id: PeerId,
        command: InputCommand,
    },
    InputRejected {
        peer_id: PeerId,
        reason: &'static str,
    },
}
#[derive(Debug)]
pub enum ClientEvent {
    Authenticated(PeerId),
    Connected(PeerId),
    Snapshot(u64),
    Disconnected(String),
}

struct Peer {
    connection: Framed<Transport>,
    admitted: bool,
    last_message: Instant,
    last_input_tick: Option<u64>,
    /// Received items retained for the other event API. At `max_queued_frames` the connection is
    /// not read, so retention applies TCP backpressure instead of dropping data.
    held: usize,
    dropped_unreliable: u64,
}
impl Peer {
    fn new(connection: Framed<Transport>, admitted: bool) -> Self {
        Self {
            connection,
            admitted,
            last_message: Instant::now(),
            last_input_tick: None,
            held: 0,
            dropped_unreliable: 0,
        }
    }
    /// Caps per-poll message parsing so retained items never exceed `max_queued_frames`.
    fn receive_budget(&self, budget: PollBudget, limits: &Limits) -> PollBudget {
        PollBudget {
            max_messages: budget
                .max_messages
                .min(limits.max_queued_frames.saturating_sub(self.held)),
            ..budget
        }
    }
    fn queue_data(&mut self, frame: Vec<u8>, delivery: Delivery, limits: &Limits) -> Result<()> {
        match self.connection.queue(frame, limits) {
            Err(Error::QueueFull) if !delivery.is_reliable() => {
                self.dropped_unreliable += 1;
                Ok(())
            }
            result => result,
        }
    }
    fn link_stats(&self) -> LinkStats {
        let stats = self.connection.stats();
        LinkStats {
            sent_bytes: stats.sent_bytes,
            received_bytes: stats.received_bytes,
            sent_packets: stats.sent_frames,
            received_packets: stats.received_frames,
            dropped_unreliable: self.dropped_unreliable,
            queued_bytes: self.connection.queued_bytes() as u64,
            ..LinkStats::default()
        }
    }
}

/// Output of the single socket drain shared by [`Server::poll`] and [`MessageEndpoint::poll`].
enum ServerItem {
    Authenticated(PeerId),
    Connected(PeerId),
    /// `Input`/`InputRejected`, which only the legacy event API can represent.
    Legacy(PeerId, ServerEvent),
    Data(PeerId, DataFrame),
    Lost {
        peer_id: PeerId,
        reason: String,
        admitted: bool,
    },
}

/// TCP server. Drive it through one event API: [`Server::poll`] (replication events) or
/// [`MessageEndpoint::poll`] (lane payloads). Both share one socket drain; items the chosen API
/// cannot represent are retained (bounded per peer) and returned by the other API's next poll.
pub struct Server {
    security: Option<ServerSecurity>,
    listener: TcpListener,
    limits: Limits,
    peers: BTreeMap<PeerId, Peer>,
    owners: BTreeMap<String, PeerId>,
    next_peer: PeerId,
    last_snapshot_tick: Option<u64>,
    inbox: VecDeque<(PeerId, DataFrame)>,
    deferred: VecDeque<(PeerId, ServerEvent)>,
}
impl Server {
    pub fn bind(config: ServerConfig) -> Result<Self> {
        config.limits.validate()?;
        let listener = TcpListener::bind(config.bind_addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            security: None,
            listener,
            limits: config.limits,
            peers: BTreeMap::new(),
            owners: BTreeMap::new(),
            next_peer: 1,
            last_snapshot_tick: None,
            inbox: VecDeque::new(),
            deferred: VecDeque::new(),
        })
    }
    /// TLS 1.3 + mandatory client certificate and application authorization. No plaintext fallback.
    pub fn bind_secure(config: ServerConfig, security: ServerSecurity) -> Result<Self> {
        security.limits.validate()?;
        let mut server = Self::bind(config)?;
        server.security = Some(security);
        Ok(server)
    }
    pub fn security(&self) -> TransportSecurity {
        if self.security.is_some() {
            TransportSecurity::MutualTls13
        } else {
            TransportSecurity::TrustedLanPlaintext
        }
    }
    pub fn pending_peers(&self) -> usize {
        self.peers.values().filter(|peer| !peer.admitted).count()
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }
    pub fn peer_ids(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.peers
            .iter()
            .filter(|(_, peer)| peer.admitted)
            .map(|(&id, _)| id)
    }
    /// Server-only authorization map; assigning ownership does not spawn an entity.
    pub fn assign_owner(&mut self, entity_id: impl Into<String>, peer_id: PeerId) -> Result<()> {
        let entity_id = entity_id.into();
        if !self.limits.valid_id(&entity_id)
            || !self.peers.get(&peer_id).is_some_and(|peer| peer.admitted)
        {
            return Err(Error::Invalid("unknown peer or invalid entity ID"));
        }
        if !self.owners.contains_key(&entity_id) && self.owners.len() >= self.limits.max_entities {
            return Err(Error::Invalid("ownership cap exceeded"));
        }
        self.owners.insert(entity_id, peer_id);
        Ok(())
    }
    pub fn revoke_owner(&mut self, entity_id: &str) {
        self.owners.remove(entity_id);
    }
    /// Explicitly remove a slow/unwanted peer and all ownership grants. Returns whether it existed.
    /// Items already received from the peer stay retained for the next poll.
    pub fn disconnect(&mut self, peer_id: PeerId) -> bool {
        self.owners.retain(|_, owner| *owner != peer_id);
        self.peers.remove(&peer_id).is_some()
    }
    /// Queues atomically for every current peer. QueueFull changes neither queues nor tick.
    /// The caller may poll to drain queues or disconnect a slow peer before retrying.
    pub fn publish_snapshot(&mut self, snapshot: Snapshot) -> Result<()> {
        snapshot.validate(&self.limits)?;
        if self
            .last_snapshot_tick
            .is_some_and(|tick| snapshot.tick <= tick)
        {
            return Err(Error::Invalid("snapshot tick must increase"));
        }
        let tick = snapshot.tick;
        let frame = Framed::<Transport>::encode(&Message::Snapshot(snapshot), &self.limits)?;
        if self
            .peers
            .values()
            .any(|peer| peer.admitted && !peer.connection.can_queue(&frame, &self.limits))
        {
            return Err(Error::QueueFull);
        }
        for peer in self.peers.values_mut().filter(|peer| peer.admitted) {
            peer.connection.queue(frame.clone(), &self.limits)?;
        }
        self.last_snapshot_tick = Some(tick);
        Ok(())
    }
    /// Replication events. Data payloads received here are retained for
    /// [`MessageEndpoint::poll`]; `Input` events retained by that API are returned first.
    pub fn poll(&mut self, budget: PollBudget) -> Result<Vec<ServerEvent>> {
        let mut events = Vec::new();
        while let Some((peer_id, event)) = self.deferred.pop_front() {
            self.release(peer_id);
            events.push(event);
        }
        for item in self.drain(budget)? {
            match item {
                ServerItem::Authenticated(peer_id) => {
                    events.push(ServerEvent::Authenticated(peer_id));
                }
                ServerItem::Connected(peer_id) => events.push(ServerEvent::Connected(peer_id)),
                ServerItem::Legacy(_, event) => events.push(event),
                ServerItem::Data(peer_id, frame) => {
                    self.hold(peer_id);
                    self.inbox.push_back((peer_id, frame));
                }
                ServerItem::Lost {
                    peer_id, reason, ..
                } => events.push(ServerEvent::Disconnected { peer_id, reason }),
            }
        }
        Ok(events)
    }
    fn hold(&mut self, peer_id: PeerId) {
        if let Some(peer) = self.peers.get_mut(&peer_id) {
            peer.held += 1;
        }
    }
    fn release(&mut self, peer_id: PeerId) {
        if let Some(peer) = self.peers.get_mut(&peer_id) {
            peer.held = peer.held.saturating_sub(1);
        }
    }
    fn drain(&mut self, budget: PollBudget) -> Result<Vec<ServerItem>> {
        let budget = budget.capped(&self.limits);
        let mut events = Vec::new();
        for _ in 0..budget.max_accepts {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.peers.len() >= self.limits.max_peers {
                        continue;
                    }
                    stream.set_nonblocking(true)?;
                    stream.set_nodelay(true)?;
                    let peer_id = self.next_peer;
                    self.next_peer = self
                        .next_peer
                        .checked_add(1)
                        .ok_or(Error::Invalid("peer IDs exhausted"))?;
                    let transport = if let Some(security) = &self.security {
                        Transport::tls(
                            stream,
                            rustls::ServerConnection::new(security.config.clone())
                                .map_err(|_| Error::Invalid("TLS server setup failed"))?
                                .into(),
                            security.limits.clone(),
                        )?
                    } else {
                        Transport::Plain(stream)
                    };
                    let mut connection = Framed::new(transport);
                    let admitted = self.security.is_none();
                    if admitted {
                        queue_welcome(&mut connection, peer_id, &self.limits)?;
                    }
                    self.peers.insert(peer_id, Peer::new(connection, admitted));
                    if admitted {
                        events.push(ServerItem::Connected(peer_id));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e.into()),
            }
        }
        let mut disconnected = Vec::new();
        for (&peer_id, peer) in &mut self.peers {
            let result = (|| {
                prepare_peer(peer, budget, &self.limits)?;
                if !peer.admitted {
                    if !peer.connection.transport_mut().ready() {
                        return Ok(Vec::new());
                    }
                    let security = self
                        .security
                        .as_ref()
                        .ok_or(Error::Protocol("missing authorization policy"))?;
                    let chain = peer
                        .connection
                        .transport_mut()
                        .certificates()
                        .filter(|chain| !chain.is_empty())
                        .ok_or(Error::Protocol("missing client identity"))?;
                    if !(security.authorize)(peer_id, chain) {
                        return Err(Error::Protocol("client identity unauthorized"));
                    }
                    peer.admitted = true;
                    peer.last_message = Instant::now();
                    queue_welcome(&mut peer.connection, peer_id, &self.limits)?;
                    events.push(ServerItem::Authenticated(peer_id));
                    events.push(ServerItem::Connected(peer_id));
                }
                let budget = peer.receive_budget(budget, &self.limits);
                receive_peer(peer, budget, &self.limits)
            })();
            match result {
                Err(error) => disconnected.push((peer_id, error.to_string())),
                Ok(messages) => {
                    for message in messages {
                        let command = match message {
                            Message::Heartbeat => continue,
                            Message::Data(frame) => {
                                events.push(ServerItem::Data(peer_id, frame));
                                continue;
                            }
                            Message::Input(command) => command,
                            _ => {
                                disconnected.push((peer_id, "unexpected client message".into()));
                                break;
                            }
                        };
                        if command.validate(&self.limits).is_err() {
                            disconnected.push((peer_id, "invalid input command".into()));
                            break;
                        }
                        let reason = if self.owners.get(&command.entity_id) != Some(&peer_id) {
                            Some("entity is not owned by peer")
                        } else if peer
                            .last_input_tick
                            .is_some_and(|tick| command.tick <= tick)
                        {
                            Some("input tick must increase")
                        } else if command.tick
                            > self
                                .last_snapshot_tick
                                .unwrap_or(0)
                                .saturating_add(self.limits.max_input_tick_lead)
                        {
                            Some("input tick exceeds allowed lead")
                        } else {
                            None
                        };
                        let event = if let Some(reason) = reason {
                            ServerEvent::InputRejected { peer_id, reason }
                        } else {
                            peer.last_input_tick = Some(command.tick);
                            ServerEvent::Input { peer_id, command }
                        };
                        events.push(ServerItem::Legacy(peer_id, event));
                    }
                }
            }
        }
        for (peer_id, reason) in disconnected {
            let admitted = self.peers.get(&peer_id).is_some_and(|peer| peer.admitted);
            self.disconnect(peer_id);
            events.push(ServerItem::Lost {
                peer_id,
                reason,
                admitted,
            });
        }
        Ok(events)
    }
}

/// TCP carries every lane and delivery reliably and in send order on one stream, so lanes share
/// head-of-line blocking. Unreliable payloads that meet a full queue are discarded and counted;
/// reliable ones return `QueueFull`. `Disconnected` is reported only for admitted peers.
impl MessageEndpoint for Server {
    fn transport(&self) -> TransportKind {
        TransportKind::Tcp
    }
    fn security(&self) -> TransportSecurity {
        Server::security(self)
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        self.peer_ids().collect()
    }
    fn max_payload(&self, peer: PeerId, _: Delivery) -> Option<usize> {
        self.peers
            .get(&peer)
            .filter(|peer| peer.admitted)
            .map(|_| Framed::<Transport>::max_data_payload(&self.limits))
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        let frame = Framed::<Transport>::encode_data(lane, delivery, payload, &self.limits)?;
        self.peers
            .get_mut(&peer)
            .filter(|peer| peer.admitted)
            .ok_or(Error::Invalid("unknown or unadmitted peer"))?
            .queue_data(frame, delivery, &self.limits)
    }
    /// Lane payloads and lifecycle events. `Input`/`InputRejected` events received here are
    /// retained for the next [`Server::poll`]; payloads that poll retained are returned first.
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        let mut events = Vec::new();
        while let Some((peer_id, frame)) = self.inbox.pop_front() {
            self.release(peer_id);
            events.push(link_message(peer_id, frame));
        }
        for item in self.drain(budget)? {
            match item {
                ServerItem::Authenticated(peer_id) => {
                    events.push(LinkEvent::Authenticated(peer_id));
                }
                ServerItem::Connected(peer_id) => events.push(LinkEvent::Connected(peer_id)),
                ServerItem::Legacy(peer_id, event) => {
                    self.hold(peer_id);
                    self.deferred.push_back((peer_id, event));
                }
                ServerItem::Data(peer_id, frame) => events.push(link_message(peer_id, frame)),
                ServerItem::Lost {
                    peer_id,
                    reason,
                    admitted: true,
                } => events.push(LinkEvent::Disconnected {
                    peer: peer_id,
                    reason,
                }),
                ServerItem::Lost { .. } => {}
            }
        }
        Ok(events)
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        self.peers
            .get(&peer)
            .filter(|peer| peer.admitted)
            .map(Peer::link_stats)
    }
    fn disconnect(&mut self, peer: PeerId) -> bool {
        Server::disconnect(self, peer)
    }
}
fn link_message(peer: PeerId, frame: DataFrame) -> LinkEvent {
    LinkEvent::Message {
        peer,
        lane: frame.lane,
        delivery: frame.delivery,
        payload: frame.payload,
    }
}
fn queue_welcome(
    connection: &mut Framed<Transport>,
    peer_id: PeerId,
    limits: &Limits,
) -> Result<()> {
    connection.queue(
        Framed::<Transport>::encode(
            &Message::Welcome {
                version: PROTOCOL_VERSION,
                peer_id,
            },
            limits,
        )?,
        limits,
    )
}
fn prepare_peer(peer: &mut Peer, budget: PollBudget, limits: &Limits) -> Result<()> {
    // A peer whose retained items fill its capacity is not read: that is local backpressure,
    // not peer idleness.
    if peer.held == 0 && peer.last_message.elapsed() >= limits.idle_timeout {
        return Err(Error::Protocol("idle timeout"));
    }
    peer.connection.transport_mut().pump(budget.max_bytes)?;
    Ok(())
}
fn receive_peer(peer: &mut Peer, budget: PollBudget, limits: &Limits) -> Result<Vec<Message>> {
    if !peer.connection.transport_mut().ready() {
        return Ok(Vec::new());
    }
    peer.connection.flush(budget.max_bytes)?;
    let messages = peer.connection.receive(budget, limits)?;
    peer.connection
        .transport_mut()
        .charge_messages(messages.len())?;
    if !messages.is_empty() {
        peer.last_message = Instant::now();
    }
    Ok(messages)
}

enum ClientItem {
    Event(ClientEvent),
    Data(DataFrame),
}
/// TCP client. Like [`Server`], drive it through one event API; `Snapshot` notifications and
/// data payloads the chosen API cannot represent are retained (bounded) for the other API.
/// The inherent `poll`/`disconnect` methods shadow the trait's: call the trait through a
/// generic `E: MessageEndpoint` or as `MessageEndpoint::poll(&mut client, budget)`.
pub struct Client {
    security: TransportSecurity,
    peer: Option<Peer>,
    peer_id: Option<PeerId>,
    limits: Limits,
    state: ReplicationState,
    last_sent_tick: Option<u64>,
    inbox: VecDeque<DataFrame>,
    deferred: VecDeque<ClientEvent>,
}
impl Client {
    /// Connection setup is blocking up to connect_timeout; subsequent work is nonblocking.
    pub fn connect(config: ClientConfig) -> Result<Self> {
        config.limits.validate()?;
        if config.connect_timeout.is_zero() {
            return Err(Error::Invalid("zero connect timeout"));
        }
        let stream = TcpStream::connect_timeout(&config.server_addr, config.connect_timeout)?;
        Self::from_stream(stream, config.limits)
    }
    /// Inject an already connected TCP stream (for host-managed connection establishment).
    /// This takes ownership, sets nonblocking mode, and expects this protocol's welcome frame.
    pub fn from_stream(stream: TcpStream, limits: Limits) -> Result<Self> {
        Self::from_transport(
            Transport::Plain(stream),
            limits,
            TransportSecurity::TrustedLanPlaintext,
        )
    }
    /// TCP connect has a timeout; TLS authentication progresses only through bounded poll calls.
    pub fn connect_secure(config: ClientConfig, security: ClientSecurity) -> Result<Self> {
        config.limits.validate()?;
        security.limits.validate()?;
        if config.connect_timeout.is_zero() {
            return Err(Error::Invalid("zero connect timeout"));
        }
        let stream = TcpStream::connect_timeout(&config.server_addr, config.connect_timeout)?;
        Self::from_secure_stream(stream, config.limits, security)
    }
    pub fn from_secure_stream(
        stream: TcpStream,
        limits: Limits,
        security: ClientSecurity,
    ) -> Result<Self> {
        let connection = rustls::ClientConnection::new(security.config, security.server_name)
            .map_err(|_| Error::Invalid("TLS client setup failed"))?;
        Self::from_transport(
            Transport::tls(stream, connection.into(), security.limits)?,
            limits,
            TransportSecurity::MutualTls13,
        )
    }
    fn from_transport(
        transport: Transport,
        limits: Limits,
        security: TransportSecurity,
    ) -> Result<Self> {
        limits.validate()?;
        transport.configure_socket()?;
        let state = ReplicationState::new(limits.clone())?;
        Ok(Self {
            security,
            peer: Some(Peer::new(Framed::new(transport), false)),
            peer_id: None,
            limits,
            state,
            last_sent_tick: None,
            inbox: VecDeque::new(),
            deferred: VecDeque::new(),
        })
    }
    pub fn security(&self) -> TransportSecurity {
        self.security
    }
    pub fn peer_id(&self) -> Option<PeerId> {
        self.peer_id
    }
    pub fn state(&self) -> &ReplicationState {
        &self.state
    }
    pub fn is_connected(&self) -> bool {
        self.peer.is_some() && self.peer_id.is_some()
    }
    pub fn disconnect(&mut self) {
        self.peer = None;
        self.peer_id = None;
    }
    pub fn send_heartbeat(&mut self) -> Result<()> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }
        let frame = Framed::<Transport>::encode(&Message::Heartbeat, &self.limits)?;
        self.peer
            .as_mut()
            .ok_or(Error::Disconnected)?
            .connection
            .queue(frame, &self.limits)
    }
    pub fn send_input(&mut self, command: InputCommand) -> Result<()> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }
        command.validate(&self.limits)?;
        if self.last_sent_tick.is_some_and(|tick| command.tick <= tick) {
            return Err(Error::Invalid("input tick must increase"));
        }
        let tick = command.tick;
        let frame = Framed::<Transport>::encode(&Message::Input(command), &self.limits)?;
        self.peer
            .as_mut()
            .ok_or(Error::Disconnected)?
            .connection
            .queue(frame, &self.limits)?;
        self.last_sent_tick = Some(tick);
        Ok(())
    }
    /// Replication events. Data payloads received here are retained for
    /// [`MessageEndpoint::poll`]; snapshot notifications retained by that API come first.
    pub fn poll(&mut self, budget: PollBudget) -> Result<Vec<ClientEvent>> {
        let mut events: Vec<ClientEvent> = self.deferred.drain(..).collect();
        for item in self.drain(budget) {
            match item {
                ClientItem::Event(event) => events.push(event),
                ClientItem::Data(frame) => self.inbox.push_back(frame),
            }
        }
        Ok(events)
    }
    fn drain(&mut self, budget: PollBudget) -> Vec<ClientItem> {
        let held = self.inbox.len() + self.deferred.len();
        let Some(peer) = &mut self.peer else {
            return Vec::new();
        };
        peer.held = held;
        let budget = peer.receive_budget(budget.capped(&self.limits), &self.limits);
        let messages = match prepare_peer(peer, budget, &self.limits)
            .and_then(|()| receive_peer(peer, budget, &self.limits))
        {
            Ok(messages) => messages,
            Err(error) => {
                self.disconnect();
                return vec![ClientItem::Event(ClientEvent::Disconnected(
                    error.to_string(),
                ))];
            }
        };
        let mut events = Vec::new();
        for message in messages {
            let result = match message {
                Message::Welcome {
                    version: PROTOCOL_VERSION,
                    peer_id,
                } if self.peer_id.is_none() && peer_id != SERVER_PEER => {
                    self.peer_id = Some(peer_id);
                    if self.security == TransportSecurity::MutualTls13 {
                        events.push(ClientItem::Event(ClientEvent::Authenticated(peer_id)));
                    }
                    events.push(ClientItem::Event(ClientEvent::Connected(peer_id)));
                    Ok(())
                }
                Message::Snapshot(snapshot) if self.peer_id.is_some() => {
                    let tick = snapshot.tick;
                    self.state
                        .apply(snapshot)
                        .map(|()| events.push(ClientItem::Event(ClientEvent::Snapshot(tick))))
                }
                Message::Data(frame) if self.peer_id.is_some() => {
                    events.push(ClientItem::Data(frame));
                    Ok(())
                }
                _ => Err(Error::Protocol("unexpected server message or version")),
            };
            if let Err(error) = result {
                self.disconnect();
                events.push(ClientItem::Event(ClientEvent::Disconnected(
                    error.to_string(),
                )));
                break;
            }
        }
        events
    }
}

/// The client's only peer is [`SERVER_PEER`]. Unlike the server, a client reports
/// `Disconnected` even when the connection fails before `Connected`.
impl MessageEndpoint for Client {
    fn transport(&self) -> TransportKind {
        TransportKind::Tcp
    }
    fn security(&self) -> TransportSecurity {
        self.security
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        if self.is_connected() {
            vec![SERVER_PEER]
        } else {
            Vec::new()
        }
    }
    fn max_payload(&self, peer: PeerId, _: Delivery) -> Option<usize> {
        (peer == SERVER_PEER && self.is_connected())
            .then(|| Framed::<Transport>::max_data_payload(&self.limits))
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        let frame = Framed::<Transport>::encode_data(lane, delivery, payload, &self.limits)?;
        if peer != SERVER_PEER || !self.is_connected() {
            return Err(Error::Invalid("unknown or unadmitted peer"));
        }
        self.peer
            .as_mut()
            .ok_or(Error::Disconnected)?
            .queue_data(frame, delivery, &self.limits)
    }
    /// Lane payloads and lifecycle events. Snapshots are still applied to [`Client::state`];
    /// their `Snapshot` notifications are retained for the next [`Client::poll`].
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        let mut events: Vec<LinkEvent> = self
            .inbox
            .drain(..)
            .map(|frame| link_message(SERVER_PEER, frame))
            .collect();
        for item in self.drain(budget) {
            match item {
                ClientItem::Event(ClientEvent::Authenticated(_)) => {
                    events.push(LinkEvent::Authenticated(SERVER_PEER));
                }
                ClientItem::Event(ClientEvent::Connected(_)) => {
                    events.push(LinkEvent::Connected(SERVER_PEER));
                }
                ClientItem::Event(ClientEvent::Disconnected(reason)) => {
                    events.push(LinkEvent::Disconnected {
                        peer: SERVER_PEER,
                        reason,
                    });
                }
                ClientItem::Event(event @ ClientEvent::Snapshot(_)) => {
                    self.deferred.push_back(event);
                }
                ClientItem::Data(frame) => events.push(link_message(SERVER_PEER, frame)),
            }
        }
        Ok(events)
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        if peer != SERVER_PEER || !self.is_connected() {
            return None;
        }
        self.peer.as_ref().map(Peer::link_stats)
    }
    fn disconnect(&mut self, peer: PeerId) -> bool {
        let existed = peer == SERVER_PEER && self.peer.is_some();
        if existed {
            Client::disconnect(self);
        }
        existed
    }
}
