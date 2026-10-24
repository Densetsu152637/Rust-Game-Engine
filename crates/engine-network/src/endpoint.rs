use crate::{
    ClientSecurity, Error, InputCommand, Limits, Message, PeerId, PollBudget, ReplicationState,
    Result, ServerSecurity, Snapshot, TransportSecurity, framing::Framed, transport::Transport,
};
use std::{
    collections::BTreeMap,
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

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
}
pub struct Server {
    security: Option<ServerSecurity>,
    listener: TcpListener,
    limits: Limits,
    peers: BTreeMap<PeerId, Peer>,
    owners: BTreeMap<String, PeerId>,
    next_peer: PeerId,
    last_snapshot_tick: Option<u64>,
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
    pub fn poll(&mut self, budget: PollBudget) -> Result<Vec<ServerEvent>> {
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
                    self.peers.insert(
                        peer_id,
                        Peer {
                            connection,
                            admitted,
                            last_message: Instant::now(),
                            last_input_tick: None,
                        },
                    );
                    if admitted {
                        events.push(ServerEvent::Connected(peer_id));
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
                        .ok_or(Error::Protocol("missing client identity"))?;
                    if !(security.authorize)(peer_id, chain) {
                        return Err(Error::Protocol("client identity unauthorized"));
                    }
                    peer.admitted = true;
                    peer.last_message = Instant::now();
                    queue_welcome(&mut peer.connection, peer_id, &self.limits)?;
                    events.push(ServerEvent::Authenticated(peer_id));
                    events.push(ServerEvent::Connected(peer_id));
                }
                receive_peer(peer, budget, &self.limits)
            })();
            match result {
                Err(error) => disconnected.push((peer_id, error.to_string())),
                Ok(messages) => {
                    for message in messages {
                        if matches!(message, Message::Heartbeat) {
                            continue;
                        }
                        let Message::Input(command) = message else {
                            disconnected.push((peer_id, "unexpected client message".into()));
                            break;
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
                        if let Some(reason) = reason {
                            events.push(ServerEvent::InputRejected { peer_id, reason });
                        } else {
                            peer.last_input_tick = Some(command.tick);
                            events.push(ServerEvent::Input { peer_id, command });
                        }
                    }
                }
            }
        }
        for (peer_id, reason) in disconnected {
            self.disconnect(peer_id);
            events.push(ServerEvent::Disconnected { peer_id, reason });
        }
        Ok(events)
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
                version: 1,
                peer_id,
            },
            limits,
        )?,
        limits,
    )
}
fn prepare_peer(peer: &mut Peer, budget: PollBudget, limits: &Limits) -> Result<()> {
    if peer.last_message.elapsed() >= limits.idle_timeout {
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

pub struct Client {
    security: TransportSecurity,
    peer: Option<Peer>,
    peer_id: Option<PeerId>,
    limits: Limits,
    state: ReplicationState,
    last_sent_tick: Option<u64>,
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
            peer: Some(Peer {
                connection: Framed::new(transport),
                admitted: false,
                last_message: Instant::now(),
                last_input_tick: None,
            }),
            peer_id: None,
            limits,
            state,
            last_sent_tick: None,
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
    pub fn poll(&mut self, budget: PollBudget) -> Result<Vec<ClientEvent>> {
        let Some(peer) = &mut self.peer else {
            return Ok(Vec::new());
        };
        let budget = budget.capped(&self.limits);
        let messages = match prepare_peer(peer, budget, &self.limits)
            .and_then(|()| receive_peer(peer, budget, &self.limits))
        {
            Ok(messages) => messages,
            Err(error) => {
                self.disconnect();
                return Ok(vec![ClientEvent::Disconnected(error.to_string())]);
            }
        };
        let mut events = Vec::new();
        for message in messages {
            let result = match message {
                Message::Welcome {
                    version: 1,
                    peer_id,
                } if self.peer_id.is_none() && peer_id != 0 => {
                    self.peer_id = Some(peer_id);
                    if self.security == TransportSecurity::MutualTls13 {
                        events.push(ClientEvent::Authenticated(peer_id));
                    }
                    events.push(ClientEvent::Connected(peer_id));
                    Ok(())
                }
                Message::Snapshot(snapshot) if self.peer_id.is_some() => {
                    let tick = snapshot.tick;
                    self.state
                        .apply(snapshot)
                        .map(|()| events.push(ClientEvent::Snapshot(tick)))
                }
                _ => Err(Error::Protocol("unexpected server message or version")),
            };
            if let Err(error) = result {
                self.disconnect();
                events.push(ClientEvent::Disconnected(error.to_string()));
                break;
            }
        }
        Ok(events)
    }
}
