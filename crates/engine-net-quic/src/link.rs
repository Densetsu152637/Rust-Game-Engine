//! Per-connection state shared by the server and the client: delivery mapping, bounded send
//! queues, bounded receive parsing, rate caps, statistics and packet transmission.
//!
//! Wire mapping (all streams are unidirectional and opened by the sender):
//! - stream header `[kind, arg]`: `[1, version]` control (server welcome, client only),
//!   `[2, lane]` the lane's single reliable-ordered stream carrying `u32` big-endian
//!   length-prefixed messages, `[3, lane]` one reliable-unordered message ending at FIN;
//! - DATAGRAM frames: `[lane]` + payload (unreliable) or `[0x10 | lane, seq u16 BE]` +
//!   payload (unreliable-sequenced; serial-number comparison, stale ones are dropped).
use crate::{DatagramSocket, QuicLimits, socket::send_lossy};
use bytes::Bytes;
use engine_network::{Delivery, Error, Lane, LinkStats, PeerId, Result};
use quinn_proto::{
    Connection, ConnectionError, ConnectionHandle, Dir, Endpoint, ReadError, SendDatagramError,
    StreamId, TransportErrorCode, VarInt, WriteError,
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

/// Version announced by the server's control stream; clients accept only this version.
pub const QUIC_PROTOCOL_VERSION: u8 = 1;
const KIND_CONTROL: u8 = 1;
const KIND_ORDERED: u8 = 2;
const KIND_UNORDERED: u8 = 3;
const DATAGRAM_SEQUENCED: u8 = 0x10;
const LANES: usize = Lane::MAX as usize + 1;

/// Application close codes. Peers only ever see the number; reasons are mapped locally.
pub(crate) mod close {
    pub(crate) const NORMAL: u32 = 0;
    pub(crate) const UNAUTHORIZED: u32 = 1;
    pub(crate) const PROTOCOL: u32 = 2;
    pub(crate) const HANDSHAKE_TIMEOUT: u32 = 3;
    pub(crate) const RATE: u32 = 4;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// TLS handshake in progress.
    Handshaking,
    /// TLS finished; awaiting authorization (server) or the welcome (client).
    Established,
    Admitted,
    /// Closed locally or lost; driven only until QUIC drains it.
    Closed,
}

/// A connection-fatal condition, mapped to a close code and a nonsecret reason.
#[derive(Debug)]
pub(crate) struct Fault {
    pub(crate) code: u32,
    pub(crate) reason: &'static str,
}
impl Fault {
    pub(crate) fn protocol(reason: &'static str) -> Self {
        Self {
            code: close::PROTOCOL,
            reason,
        }
    }
}

/// Bounded per-poll receive work for one connection.
pub(crate) struct Budget {
    pub(crate) messages: usize,
    pub(crate) bytes: usize,
}

pub(crate) struct Received {
    pub(crate) lane: Lane,
    pub(crate) delivery: Delivery,
    pub(crate) payload: Vec<u8>,
}

struct OrderedOut {
    stream: Option<StreamId>,
    pending: Vec<u8>,
    offset: usize,
}
struct UnorderedOut {
    stream: StreamId,
    data: Vec<u8>,
    offset: usize,
}
enum Inbound {
    Header {
        bytes: [u8; 2],
        have: usize,
    },
    Ordered(OrderedIn),
    Unordered {
        lane: Lane,
        body: Vec<u8>,
    },
    /// Control stream after its header: no further bytes are allowed before FIN.
    Control,
}
struct OrderedIn {
    lane: Lane,
    length: [u8; 4],
    have: usize,
    body: Option<Vec<u8>>,
    expected: usize,
}

pub(crate) struct Link {
    pub(crate) conn: Connection,
    pub(crate) handle: ConnectionHandle,
    pub(crate) peer: PeerId,
    pub(crate) phase: Phase,
    started: Instant,
    ordered_out: [Option<OrderedOut>; LANES],
    unordered_queue: VecDeque<Vec<u8>>,
    unordered_writing: Vec<UnorderedOut>,
    queued_bytes: usize,
    send_sequence: [u16; LANES],
    inbound: BTreeMap<StreamId, Inbound>,
    /// Lanes whose ordered stream has been seen; a second one is a protocol violation.
    ordered_lanes: u16,
    /// Client only: the first server stream must be the control stream.
    control_stream: Option<StreamId>,
    welcomed: bool,
    receive_sequence: [Option<u16>; LANES],
    rate_started: Instant,
    rate_bytes: usize,
    rate_messages: usize,
    dropped_unreliable: u64,
    /// Cached `datagrams().max_size()`, refreshed each poll (`max_payload` takes `&self`).
    datagram_limit: Option<usize>,
}
impl Link {
    pub(crate) fn new(
        conn: Connection,
        handle: ConnectionHandle,
        peer: PeerId,
        now: Instant,
    ) -> Self {
        Self {
            conn,
            handle,
            peer,
            phase: Phase::Handshaking,
            started: now,
            ordered_out: std::array::from_fn(|_| None),
            unordered_queue: VecDeque::new(),
            unordered_writing: Vec::new(),
            queued_bytes: 0,
            send_sequence: [0; LANES],
            inbound: BTreeMap::new(),
            ordered_lanes: 0,
            control_stream: None,
            welcomed: false,
            receive_sequence: [None; LANES],
            rate_started: now,
            rate_bytes: 0,
            rate_messages: 0,
            dropped_unreliable: 0,
            datagram_limit: None,
        }
    }
    pub(crate) fn is_live(&self) -> bool {
        self.phase != Phase::Closed
    }
    pub(crate) fn is_pending(&self) -> bool {
        matches!(self.phase, Phase::Handshaking | Phase::Established)
    }
    pub(crate) fn handshake_expired(&self, now: Instant, limits: &QuicLimits) -> bool {
        self.is_pending() && now.duration_since(self.started) >= limits.handshake_timeout
    }
    pub(crate) fn close(&mut self, now: Instant, code: u32) {
        if self.phase != Phase::Closed {
            self.conn.close(now, VarInt::from_u32(code), Bytes::new());
            self.phase = Phase::Closed;
        }
    }

    /// Runs QUIC timers and returns `Some(reason)` when the connection was lost.
    pub(crate) fn drive_timers_and_events(&mut self, now: Instant) -> Option<String> {
        if self.conn.poll_timeout().is_some_and(|t| t <= now) {
            self.conn.handle_timeout(now);
        }
        let mut lost = None;
        while let Some(event) = self.conn.poll() {
            match event {
                quinn_proto::Event::Connected if self.phase == Phase::Handshaking => {
                    self.phase = Phase::Established;
                }
                quinn_proto::Event::ConnectionLost { reason } => {
                    lost = Some(lost_reason(&reason).to_owned());
                }
                _ => {}
            }
        }
        if lost.is_some() {
            self.phase = Phase::Closed;
        }
        lost
    }

    /// Negotiated ALPN check, run once the TLS handshake finished on either side.
    pub(crate) fn negotiated_alpn(&self, alpn: &[u8]) -> bool {
        self.conn
            .crypto_session()
            .handshake_data()
            .and_then(|data| {
                data.downcast::<quinn_proto::crypto::rustls::HandshakeData>()
                    .ok()
            })
            .and_then(|data| data.protocol)
            .is_some_and(|protocol| protocol == alpn)
    }
    /// The TLS-verified, nonempty peer certificate chain.
    pub(crate) fn peer_chain(&self) -> Option<Vec<engine_network::CertificateDer<'static>>> {
        self.conn
            .crypto_session()
            .peer_identity()
            .and_then(|identity| {
                identity
                    .downcast::<Vec<engine_network::CertificateDer<'static>>>()
                    .ok()
            })
            .map(|chain| *chain)
            .filter(|chain| !chain.is_empty())
    }
    /// Server: admits the peer and opens the control stream carrying the welcome.
    pub(crate) fn admit_and_welcome(&mut self) -> std::result::Result<(), Fault> {
        let stream = self
            .conn
            .streams()
            .open(Dir::Uni)
            .ok_or(Fault::protocol("peer refused the control stream"))?;
        let mut send = self.conn.send_stream(stream);
        let welcome = [KIND_CONTROL, QUIC_PROTOCOL_VERSION];
        if send.write(&welcome) != Ok(welcome.len()) || send.finish().is_err() {
            return Err(Fault::protocol("control stream write failed"));
        }
        self.phase = Phase::Admitted;
        Ok(())
    }
    /// Client: reads only the first server stream until the welcome arrives.
    pub(crate) fn receive_welcome(
        &mut self,
        limits: &QuicLimits,
    ) -> std::result::Result<bool, Fault> {
        let mut budget = Budget {
            messages: 1,
            bytes: 2,
        };
        self.receive_streams(&mut budget, true, limits, &mut Vec::new())?;
        if self.welcomed {
            self.phase = Phase::Admitted;
        }
        Ok(self.welcomed)
    }

    pub(crate) fn refresh_datagram_limit(&mut self) {
        self.datagram_limit = self.conn.datagrams().max_size();
    }
    pub(crate) fn max_payload(&self, delivery: Delivery, limits: &QuicLimits) -> Option<usize> {
        if self.phase != Phase::Admitted {
            return None;
        }
        Some(match delivery {
            Delivery::ReliableOrdered | Delivery::ReliableUnordered => limits.max_message_bytes,
            Delivery::Unreliable => self.datagram_limit?.saturating_sub(1),
            Delivery::UnreliableSequenced => self.datagram_limit?.saturating_sub(3),
        })
    }

    pub(crate) fn send(
        &mut self,
        lane: Lane,
        delivery: Delivery,
        payload: &[u8],
        limits: &QuicLimits,
    ) -> Result<()> {
        match delivery {
            Delivery::ReliableOrdered => self.queue_ordered(lane, payload, limits),
            Delivery::ReliableUnordered => self.queue_unordered(lane, payload, limits),
            Delivery::Unreliable => self.send_datagram(&[lane.0], payload),
            Delivery::UnreliableSequenced => {
                let sequence = self.send_sequence[usize::from(lane.0)];
                self.send_sequence[usize::from(lane.0)] = sequence.wrapping_add(1);
                let [high, low] = sequence.to_be_bytes();
                self.send_datagram(&[DATAGRAM_SEQUENCED | lane.0, high, low], payload)
            }
        }
    }
    fn reserve(&mut self, bytes: usize, payload: usize, limits: &QuicLimits) -> Result<()> {
        if payload > limits.max_message_bytes {
            return Err(Error::Invalid("payload exceeds max_payload"));
        }
        if bytes > limits.max_queued_bytes.saturating_sub(self.queued_bytes) {
            return Err(Error::QueueFull);
        }
        self.queued_bytes += bytes;
        Ok(())
    }
    fn queue_ordered(&mut self, lane: Lane, payload: &[u8], limits: &QuicLimits) -> Result<()> {
        let fresh = self.ordered_out[usize::from(lane.0)].is_none();
        self.reserve(
            4 + payload.len() + if fresh { 2 } else { 0 },
            payload.len(),
            limits,
        )?;
        let out = self.ordered_out[usize::from(lane.0)].get_or_insert_with(|| OrderedOut {
            stream: None,
            pending: vec![KIND_ORDERED, lane.0],
            offset: 0,
        });
        out.pending
            .extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.pending.extend_from_slice(payload);
        Ok(())
    }
    fn queue_unordered(&mut self, lane: Lane, payload: &[u8], limits: &QuicLimits) -> Result<()> {
        self.reserve(2 + payload.len(), payload.len(), limits)?;
        let mut data = Vec::with_capacity(2 + payload.len());
        data.extend_from_slice(&[KIND_UNORDERED, lane.0]);
        data.extend_from_slice(payload);
        self.unordered_queue.push_back(data);
        Ok(())
    }
    fn send_datagram(&mut self, header: &[u8], payload: &[u8]) -> Result<()> {
        let limit = self
            .conn
            .datagrams()
            .max_size()
            .ok_or(Error::Invalid("peer does not accept datagrams"))?;
        if header.len() + payload.len() > limit {
            return Err(Error::Invalid("payload exceeds the current datagram limit"));
        }
        let mut datagram = Vec::with_capacity(header.len() + payload.len());
        datagram.extend_from_slice(header);
        datagram.extend_from_slice(payload);
        match self.conn.datagrams().send(Bytes::from(datagram), false) {
            Ok(()) => Ok(()),
            Err(SendDatagramError::Blocked(_)) => {
                self.dropped_unreliable += 1;
                Ok(())
            }
            Err(SendDatagramError::TooLarge) => {
                Err(Error::Invalid("payload exceeds the current datagram limit"))
            }
            Err(_) => Err(Error::Invalid("peer does not accept datagrams")),
        }
    }

    /// Moves queued reliable bytes into QUIC as flow control allows.
    pub(crate) fn flush(&mut self) -> std::result::Result<(), Fault> {
        let Self {
            conn,
            ordered_out,
            unordered_queue,
            unordered_writing,
            queued_bytes,
            ..
        } = self;
        for out in ordered_out.iter_mut().flatten() {
            if out.offset == out.pending.len() {
                continue;
            }
            let stream = match out.stream {
                Some(stream) => stream,
                None => match conn.streams().open(Dir::Uni) {
                    Some(stream) => *out.stream.insert(stream),
                    None => continue,
                },
            };
            let written = write_some(conn, stream, &out.pending[out.offset..])?;
            out.offset += written;
            *queued_bytes -= written;
            if out.offset == out.pending.len() {
                out.pending.clear();
                out.offset = 0;
            } else if out.offset * 2 >= out.pending.len() {
                // Compact so the buffer holds at most twice the unwritten bytes.
                out.pending.drain(..out.offset);
                out.offset = 0;
            }
        }
        let mut index = 0;
        while index < unordered_writing.len() {
            let out = &mut unordered_writing[index];
            let written = write_some(conn, out.stream, &out.data[out.offset..])?;
            out.offset += written;
            *queued_bytes -= written;
            if out.offset == out.data.len() {
                let _ = conn.send_stream(out.stream).finish();
                unordered_writing.swap_remove(index);
            } else {
                index += 1;
            }
        }
        while !unordered_queue.is_empty() {
            let Some(stream) = conn.streams().open(Dir::Uni) else {
                break;
            };
            let data = unordered_queue.pop_front().unwrap_or_default();
            let written = write_some(conn, stream, &data)?;
            *queued_bytes -= written;
            if written == data.len() {
                let _ = conn.send_stream(stream).finish();
            } else {
                unordered_writing.push(UnorderedOut {
                    stream,
                    data,
                    offset: written,
                });
            }
        }
        Ok(())
    }

    /// Reads admitted-peer streams and datagrams within `budget`.
    pub(crate) fn receive(
        &mut self,
        budget: &mut Budget,
        client: bool,
        limits: &QuicLimits,
        now: Instant,
    ) -> std::result::Result<Vec<Received>, Fault> {
        if now.duration_since(self.rate_started).as_secs() >= 1 {
            self.rate_started = now;
            self.rate_bytes = 0;
            self.rate_messages = 0;
        }
        let mut received = Vec::new();
        self.receive_streams(budget, client, limits, &mut received)?;
        self.receive_datagrams(budget, &mut received)?;
        for message in &received {
            self.rate_messages += 1;
            self.rate_bytes = self.rate_bytes.saturating_add(message.payload.len());
        }
        if self.rate_messages > limits.max_messages_per_second
            || self.rate_bytes > limits.max_received_bytes_per_second
        {
            return Err(Fault {
                code: close::RATE,
                reason: "receive rate exceeded",
            });
        }
        Ok(received)
    }

    fn receive_streams(
        &mut self,
        budget: &mut Budget,
        client: bool,
        limits: &QuicLimits,
        received: &mut Vec<Received>,
    ) -> std::result::Result<(), Fault> {
        while let Some(stream) = self.conn.streams().accept(Dir::Uni) {
            if client && self.control_stream.is_none() {
                self.control_stream = Some(stream);
            }
            self.inbound.insert(
                stream,
                Inbound::Header {
                    bytes: [0; 2],
                    have: 0,
                },
            );
        }
        let streams: Vec<StreamId> = self.inbound.keys().copied().collect();
        for stream in streams {
            if budget.messages == 0 || budget.bytes == 0 {
                break;
            }
            // Before the welcome a client reads nothing but the control stream.
            if client && !self.welcomed && Some(stream) != self.control_stream {
                continue;
            }
            self.read_stream(stream, budget, client, limits, received)?;
        }
        Ok(())
    }

    fn read_stream(
        &mut self,
        stream: StreamId,
        budget: &mut Budget,
        client: bool,
        limits: &QuicLimits,
        received: &mut Vec<Received>,
    ) -> std::result::Result<(), Fault> {
        let Self {
            conn,
            inbound,
            ordered_lanes,
            welcomed,
            control_stream,
            ..
        } = self;
        let mut recv = conn.recv_stream(stream);
        let Ok(mut chunks) = recv.read(true) else {
            inbound.remove(&stream);
            return Ok(());
        };
        let Some(state) = inbound.get_mut(&stream) else {
            return Ok(());
        };
        let mut finished = false;
        let mut outcome = Ok(());
        while budget.messages > 0 && budget.bytes > 0 {
            let want = match state {
                Inbound::Header { have, .. } => 2 - *have,
                Inbound::Ordered(ordered) if ordered.body.is_none() => 4 - ordered.have,
                Inbound::Ordered(ordered) => {
                    ordered.expected - ordered.body.as_ref().map_or(0, Vec::len)
                }
                Inbound::Unordered { body, .. } => limits.max_message_bytes + 1 - body.len(),
                Inbound::Control => 1,
            };
            let chunk = match chunks.next(want.min(budget.bytes)) {
                Ok(Some(chunk)) => chunk.bytes,
                Ok(None) => {
                    finished = true;
                    break;
                }
                Err(ReadError::Blocked) => break,
                Err(ReadError::Reset(_)) => {
                    outcome = match state {
                        Inbound::Ordered(_) | Inbound::Control => {
                            Err(Fault::protocol("peer reset a reliable-ordered stream"))
                        }
                        _ => Ok(()),
                    };
                    finished = true;
                    break;
                }
            };
            budget.bytes = budget.bytes.saturating_sub(chunk.len());
            match state {
                Inbound::Header { bytes, have } => {
                    bytes[*have..*have + chunk.len()].copy_from_slice(&chunk);
                    *have += chunk.len();
                    if *have < 2 {
                        continue;
                    }
                    let [kind, arg] = *bytes;
                    let lane = Lane(arg);
                    *state = match kind {
                        KIND_CONTROL
                            if client
                                && !*welcomed
                                && Some(stream) == *control_stream
                                && arg == QUIC_PROTOCOL_VERSION =>
                        {
                            *welcomed = true;
                            Inbound::Control
                        }
                        KIND_ORDERED
                            if lane.is_valid()
                                && (!client || *welcomed)
                                && *ordered_lanes & (1 << arg) == 0 =>
                        {
                            *ordered_lanes |= 1 << arg;
                            Inbound::Ordered(OrderedIn {
                                lane,
                                length: [0; 4],
                                have: 0,
                                body: None,
                                expected: 0,
                            })
                        }
                        KIND_UNORDERED if lane.is_valid() && (!client || *welcomed) => {
                            Inbound::Unordered {
                                lane,
                                body: Vec::new(),
                            }
                        }
                        _ => {
                            outcome = Err(Fault::protocol("invalid stream header"));
                            break;
                        }
                    };
                }
                Inbound::Ordered(ordered) => {
                    if let Some(body) = &mut ordered.body {
                        body.extend_from_slice(&chunk);
                    } else {
                        ordered.length[ordered.have..ordered.have + chunk.len()]
                            .copy_from_slice(&chunk);
                        ordered.have += chunk.len();
                        if ordered.have < 4 {
                            continue;
                        }
                        ordered.expected = u32::from_be_bytes(ordered.length) as usize;
                        if ordered.expected > limits.max_message_bytes {
                            outcome = Err(Fault::protocol("message exceeds max_message_bytes"));
                            break;
                        }
                        ordered.body = Some(Vec::with_capacity(ordered.expected.min(64 * 1024)));
                    }
                    if ordered
                        .body
                        .as_ref()
                        .is_some_and(|b| b.len() == ordered.expected)
                    {
                        received.push(Received {
                            lane: ordered.lane,
                            delivery: Delivery::ReliableOrdered,
                            payload: ordered.body.take().unwrap_or_default(),
                        });
                        ordered.have = 0;
                        budget.messages -= 1;
                    }
                }
                Inbound::Unordered { body, .. } => {
                    body.extend_from_slice(&chunk);
                    if body.len() > limits.max_message_bytes {
                        outcome = Err(Fault::protocol("message exceeds max_message_bytes"));
                        break;
                    }
                }
                Inbound::Control => {
                    outcome = Err(Fault::protocol("unexpected control stream data"));
                    break;
                }
            }
        }
        let _ = chunks.finalize();
        outcome?;
        if finished {
            match inbound.remove(&stream) {
                Some(Inbound::Unordered { lane, body }) => {
                    budget.messages = budget.messages.saturating_sub(1);
                    received.push(Received {
                        lane,
                        delivery: Delivery::ReliableUnordered,
                        payload: body,
                    });
                }
                Some(Inbound::Ordered(ordered)) if ordered.have != 0 || ordered.body.is_some() => {
                    return Err(Fault::protocol("truncated reliable-ordered stream"));
                }
                Some(Inbound::Header { .. }) => {
                    return Err(Fault::protocol("stream ended before its header"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn receive_datagrams(
        &mut self,
        budget: &mut Budget,
        received: &mut Vec<Received>,
    ) -> std::result::Result<(), Fault> {
        while budget.messages > 0 && budget.bytes > 0 {
            let Some(datagram) = self.conn.datagrams().recv() else {
                break;
            };
            budget.bytes = budget.bytes.saturating_sub(datagram.len());
            let (delivery, lane, payload) = match datagram.as_ref() {
                [byte, payload @ ..] if *byte <= Lane::MAX => {
                    (Delivery::Unreliable, Lane(*byte), payload)
                }
                [byte, high, low, payload @ ..] if byte & 0xF0 == DATAGRAM_SEQUENCED => {
                    let lane = Lane(byte & 0x0F);
                    let sequence = u16::from_be_bytes([*high, *low]);
                    let last = &mut self.receive_sequence[usize::from(lane.0)];
                    // Serial-number arithmetic (RFC 1982) tolerates u16 wrap-around.
                    if last.is_some_and(|last| (sequence.wrapping_sub(last) as i16) <= 0) {
                        self.dropped_unreliable += 1;
                        continue;
                    }
                    *last = Some(sequence);
                    (Delivery::UnreliableSequenced, lane, payload)
                }
                _ => return Err(Fault::protocol("malformed datagram header")),
            };
            budget.messages -= 1;
            received.push(Received {
                lane,
                delivery,
                payload: payload.to_vec(),
            });
        }
        Ok(())
    }

    /// Writes up to `max` packets to the socket.
    pub(crate) fn transmit(
        &mut self,
        now: Instant,
        socket: &mut dyn DatagramSocket,
        buffer: &mut Vec<u8>,
        max: usize,
    ) {
        for _ in 0..max {
            buffer.clear();
            let Some(transmit) = self.conn.poll_transmit(now, 1, buffer) else {
                break;
            };
            send_lossy(socket, &buffer[..transmit.size], transmit.destination);
        }
    }
    /// Exchanges connection-ID and drain bookkeeping with the endpoint.
    pub(crate) fn exchange_endpoint_events(&mut self, endpoint: &mut Endpoint) {
        while let Some(event) = self.conn.poll_endpoint_events() {
            if let Some(event) = endpoint.handle_event(self.handle, event) {
                self.conn.handle_event(event);
            }
        }
    }

    pub(crate) fn stats(&self) -> LinkStats {
        let stats = self.conn.stats();
        LinkStats {
            rtt_ms: stats.path.rtt.as_secs_f32() * 1000.0,
            sent_bytes: stats.udp_tx.bytes,
            received_bytes: stats.udp_rx.bytes,
            sent_packets: stats.path.sent_packets,
            received_packets: stats.udp_rx.datagrams,
            lost_packets: stats.path.lost_packets,
            dropped_unreliable: self.dropped_unreliable,
            congestion_window_bytes: stats.path.cwnd,
            queued_bytes: self.queued_bytes as u64,
        }
    }
}

/// Writes as much as flow control accepts; a stopped stream loses reliable data, so it is fatal.
fn write_some(
    conn: &mut Connection,
    stream: StreamId,
    data: &[u8],
) -> std::result::Result<usize, Fault> {
    match conn.send_stream(stream).write(data) {
        Ok(written) => Ok(written),
        Err(WriteError::Blocked) => Ok(0),
        Err(_) => Err(Fault::protocol("peer stopped a reliable stream")),
    }
}

/// Maps a QUIC loss reason to a fixed nonsecret string; peer-supplied reason text is never used.
pub(crate) fn lost_reason(error: &ConnectionError) -> &'static str {
    match error {
        ConnectionError::ApplicationClosed(close) => {
            match u32::try_from(close.error_code.into_inner()) {
                Ok(close::UNAUTHORIZED) => "unauthorized",
                Ok(close::PROTOCOL) => "protocol violation",
                Ok(close::HANDSHAKE_TIMEOUT) => "handshake timeout",
                Ok(close::RATE) => "receive rate exceeded",
                _ => "closed by peer",
            }
        }
        ConnectionError::ConnectionClosed(close) => transport_reason(close.error_code),
        ConnectionError::TransportError(error) => transport_reason(error.code),
        ConnectionError::TimedOut => "idle timeout",
        ConnectionError::Reset => "reset by peer",
        ConnectionError::LocallyClosed => "closed",
        ConnectionError::VersionMismatch => "QUIC version mismatch",
        ConnectionError::CidsExhausted => "connection IDs exhausted",
    }
}
fn transport_reason(code: TransportErrorCode) -> &'static str {
    let code = u64::from(code);
    if code == u64::from(TransportErrorCode::CONNECTION_REFUSED) {
        "connection refused"
    } else if code == u64::from(TransportErrorCode::APPLICATION_ERROR) {
        // An application close sent before the handshake completed (RFC 9000 10.2.3).
        "closed by peer"
    } else if (0x100..=0x1ff).contains(&code) {
        "TLS authentication failed"
    } else {
        "transport error"
    }
}
