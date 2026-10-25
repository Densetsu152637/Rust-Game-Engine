//! Transport-neutral message-delivery contract.
//!
//! Every engine endpoint (mutual-TLS TCP in this crate, QUIC in `engine-net-quic`) implements
//! [`MessageEndpoint`]. Replication, RPC, prediction, voice and session layers are sans-IO: they
//! produce and consume opaque byte payloads addressed by `(peer, lane, delivery)` and never own
//! sockets. Payload bytes are untrusted input for every consumer.
use crate::{PeerId, PollBudget, Result, TransportSecurity};
use serde::{Deserialize, Serialize};

/// Peer ID a client endpoint uses to address its server. Servers never assign 0 to a client.
pub const SERVER_PEER: PeerId = 0;

/// Delivery guarantee requested for one payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Retransmitted until acknowledged; delivered in send order within `(peer, lane)`.
    ReliableOrdered,
    /// Retransmitted until acknowledged; may be delivered in any order.
    ReliableUnordered,
    /// Never retransmitted; a payload older than the newest one already delivered on
    /// `(peer, lane)` is discarded by the receiving endpoint.
    UnreliableSequenced,
    /// Never retransmitted; may be lost or reordered.
    Unreliable,
}
impl Delivery {
    pub fn is_reliable(self) -> bool {
        matches!(self, Self::ReliableOrdered | Self::ReliableUnordered)
    }
}

/// Logical lane. Ordering and sequencing guarantees apply per `(peer, lane)`; lanes are
/// independent, so loss or blocking on one lane never stalls another on transports that
/// support it (QUIC). TCP serializes every lane on one ordered stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Lane(pub u8);
impl Lane {
    /// Highest valid lane number; endpoints reject larger lanes on send and receive.
    pub const MAX: u8 = 15;
    /// Application default lane.
    pub const DEFAULT: Self = Self(0);
    /// Engine replication (state deltas, baselines, acknowledgements).
    pub const REPLICATION: Self = Self(1);
    /// Engine remote procedure calls.
    pub const RPC: Self = Self(2);
    /// Client input commands and prediction/rollback traffic.
    pub const INPUT: Self = Self(3);
    /// Voice frames.
    pub const VOICE: Self = Self(4);
    /// Session/control traffic (tokens, lobby, migration).
    pub const SESSION: Self = Self(5);
    /// First lane reserved for applications; lanes `8..=MAX` are never used by the engine.
    pub const FIRST_APPLICATION: u8 = 8;

    pub fn is_valid(self) -> bool {
        self.0 <= Self::MAX
    }
}

/// Wire transport behind an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Tcp,
    Quic,
}

/// Event produced by [`MessageEndpoint::poll`].
#[derive(Clone, Debug, PartialEq)]
pub enum LinkEvent {
    /// Transport authentication and native authorization passed (secure endpoints only);
    /// always precedes `Connected` for the same peer.
    Authenticated(PeerId),
    /// The peer is admitted; payloads may now be sent to and received from it.
    Connected(PeerId),
    Disconnected {
        peer: PeerId,
        reason: String,
    },
    /// A complete payload. Bounded by the endpoint's limits but otherwise untrusted.
    Message {
        peer: PeerId,
        lane: Lane,
        delivery: Delivery,
        payload: Vec<u8>,
    },
}

/// Nonsecret per-peer link telemetry (safe for reports, logs and MCP output).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LinkStats {
    /// Smoothed round-trip time; 0 when not yet measured.
    pub rtt_ms: f32,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub sent_packets: u64,
    pub received_packets: u64,
    /// Packets the transport declared lost (QUIC loss detection; 0 on TCP).
    pub lost_packets: u64,
    /// Unreliable payloads discarded locally (congestion/backpressure) or as stale on receive.
    pub dropped_unreliable: u64,
    /// Current congestion window in bytes; 0 when the transport does not expose one.
    pub congestion_window_bytes: u64,
    /// Bytes queued for this peer and not yet written to the socket.
    pub queued_bytes: u64,
}

/// Uniform nonblocking endpoint API. Implementations never sleep or block in any method,
/// bound every queue, and report backpressure explicitly.
pub trait MessageEndpoint {
    fn transport(&self) -> TransportKind;
    fn security(&self) -> TransportSecurity;
    /// Admitted peers only: a server's admitted clients, or `[SERVER_PEER]` on a connected client.
    fn connected_peers(&self) -> Vec<PeerId>;
    /// Largest payload `send` accepts for this peer and delivery right now; `None` for an
    /// unknown/unadmitted peer.
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize>;
    /// Queues one payload without blocking. Invalid lanes, oversized payloads and unknown peers
    /// return `Error::Invalid`; a full reliable queue returns `Error::QueueFull` and queues
    /// nothing. Unreliable payloads may be discarded under congestion and are then counted in
    /// `LinkStats::dropped_unreliable`; reliable payloads are never silently dropped.
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()>;
    /// Performs bounded socket/timer work and returns the resulting events.
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>>;
    fn stats(&self, peer: PeerId) -> Option<LinkStats>;
    /// Closes the peer (a client's only peer is `SERVER_PEER`). Returns whether it existed.
    fn disconnect(&mut self, peer: PeerId) -> bool;
}
