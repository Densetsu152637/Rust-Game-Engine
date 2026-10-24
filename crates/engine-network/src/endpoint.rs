use crate::{
    Error, InputCommand, Limits, Message, PeerId, PollBudget, ReplicationState, Result, Snapshot,
    framing::Framed,
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
    Connected(PeerId),
    Snapshot(u64),
    Disconnected(String),
}

struct Peer {
    connection: Framed<TcpStream>,
    last_message: Instant,
    last_input_tick: Option<u64>,
}
pub struct Server {
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
            listener,
            limits: config.limits,
            peers: BTreeMap::new(),
            owners: BTreeMap::new(),
            next_peer: 1,
            last_snapshot_tick: None,
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }
    pub fn peer_ids(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.peers.keys().copied()
    }
    /// Server-only authorization map; assigning ownership does not spawn an entity.
    pub fn assign_owner(&mut self, entity_id: impl Into<String>, peer_id: PeerId) -> Result<()> {
        let entity_id = entity_id.into();
        if !self.limits.valid_id(&entity_id) || !self.peers.contains_key(&peer_id) {
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
        let frame = Framed::<TcpStream>::encode(&Message::Snapshot(snapshot), &self.limits)?;
        if self
            .peers
            .values()
            .any(|peer| !peer.connection.can_queue(&frame, &self.limits))
        {
            return Err(Error::QueueFull);
        }
        for peer in self.peers.values_mut() {
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
                    let mut connection = Framed::new(stream);
                    connection.queue(
                        Framed::<TcpStream>::encode(
                            &Message::Welcome {
                                version: 1,
                                peer_id,
                            },
                            &self.limits,
                        )?,
                        &self.limits,
                    )?;
                    self.peers.insert(
                        peer_id,
                        Peer {
                            connection,
                            last_message: Instant::now(),
                            last_input_tick: None,
                        },
                    );
                    events.push(ServerEvent::Connected(peer_id));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e.into()),
            }
        }
        let mut disconnected = Vec::new();
        for (&peer_id, peer) in &mut self.peers {
            let result = poll_peer(peer, budget, &self.limits);
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
fn poll_peer(peer: &mut Peer, budget: PollBudget, limits: &Limits) -> Result<Vec<Message>> {
    if peer.last_message.elapsed() >= limits.idle_timeout {
        return Err(Error::Protocol("idle timeout"));
    }
    peer.connection.flush(budget.max_bytes)?;
    let messages = peer.connection.receive(budget, limits)?;
    if !messages.is_empty() {
        peer.last_message = Instant::now();
    }
    Ok(messages)
}

pub struct Client {
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
        limits.validate()?;
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let state = ReplicationState::new(limits.clone())?;
        Ok(Self {
            peer: Some(Peer {
                connection: Framed::new(stream),
                last_message: Instant::now(),
                last_input_tick: None,
            }),
            peer_id: None,
            limits,
            state,
            last_sent_tick: None,
        })
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
        let frame = Framed::<TcpStream>::encode(&Message::Heartbeat, &self.limits)?;
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
        let frame = Framed::<TcpStream>::encode(&Message::Input(command), &self.limits)?;
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
        let messages = match poll_peer(peer, budget.capped(&self.limits), &self.limits) {
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
