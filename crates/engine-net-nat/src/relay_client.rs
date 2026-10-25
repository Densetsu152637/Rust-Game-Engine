//! Client side of the relay: a [`DatagramSocket`] that carries QUIC through allocations.
use crate::{
    Error, RelayRefusal, Result,
    agent::is_transient,
    wire::{self, Allocate, COOKIE_LEN, KEY_LEN, Refresh, TxId, kind},
};
use engine_net_quic::DatagramSocket;
use serde::Serialize;
use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    time::{Duration, Instant},
};

/// Timing and bounds of a [`RelaySocket`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySocketConfig {
    /// Retransmission interval of allocate/refresh requests.
    pub request_interval: Duration,
    /// Attempts per request before allocation fails (allocate) or the refresh is retried
    /// later (refresh, until the allocation expires).
    pub max_request_attempts: u32,
    /// Refresh cadence while the other party has not allocated yet (how the client learns
    /// the counterpart).
    pub peer_poll_interval: Duration,
    /// Lifetime asked for on refresh; the server caps it.
    pub requested_lifetime: Duration,
    pub max_allocations: usize,
    /// Largest QUIC datagram accepted by `send_to`; keep at or below the server's limit.
    pub max_payload_bytes: usize,
    /// Relayed datagrams received during [`RelaySocket::poll_control`] and held for
    /// `recv_from`.
    pub max_queued_datagrams: usize,
    pub max_reads_per_call: usize,
}
impl Default for RelaySocketConfig {
    fn default() -> Self {
        Self {
            request_interval: Duration::from_millis(200),
            max_request_attempts: 8,
            peer_poll_interval: Duration::from_millis(200),
            requested_lifetime: Duration::from_secs(120),
            max_allocations: 16,
            max_payload_bytes: 1472,
            max_queued_datagrams: 128,
            max_reads_per_call: 64,
        }
    }
}
impl RelaySocketConfig {
    pub fn validate(&self) -> Result<()> {
        if self.request_interval < Duration::from_millis(10)
            || self.request_interval > Duration::from_secs(10)
            || self.max_request_attempts == 0
            || self.max_request_attempts > 64
            || self.peer_poll_interval < Duration::from_millis(50)
            || self.requested_lifetime < Duration::from_secs(3)
            || self.requested_lifetime > Duration::from_secs(3600)
            || self.max_allocations == 0
            || self.max_allocations > 256
            || !(1200..=65_000).contains(&self.max_payload_bytes)
            || self.max_queued_datagrams == 0
            || self.max_reads_per_call == 0
        {
            return Err(Error::Invalid("invalid relay socket config"));
        }
        Ok(())
    }
}

/// Handle of one allocation inside a [`RelaySocket`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AllocationId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayFailure {
    Refused(RelayRefusal),
    /// No response to allocate after every attempt.
    TimedOut,
    /// The allocation lapsed because refreshes went unanswered.
    Expired,
    /// Released locally.
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationStatus {
    Requesting,
    /// Allocated; the other party has not allocated yet.
    WaitingForPeer,
    /// `peer` is the counterpart's virtual address: hand it to QUIC (for example
    /// `QuicClient::with_socket(relay_socket, peer, ..)`); a server receives from it.
    Ready {
        peer: SocketAddr,
    },
    Failed(RelayFailure),
}

/// Nonsecret counters of a [`RelaySocket`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RelaySocketStats {
    pub data_sent: u64,
    pub data_received: u64,
    pub control_sent: u64,
    pub control_received: u64,
    /// Datagrams not from the relay address (ignored).
    pub dropped_foreign: u64,
    /// Relayed data for no matching allocation/counterpart, or malformed relay datagrams.
    pub dropped_unmatched: u64,
    /// Relayed data dropped because the hold queue was full.
    pub dropped_queue: u64,
    /// `send_to` calls to an address that is no ready counterpart.
    pub send_refused: u64,
}

enum Phase {
    Allocating {
        cookie: [u8; COOKIE_LEN],
    },
    Active {
        vport: u16,
        key: [u8; KEY_LEN],
        expires: Instant,
        counterpart: Option<u16>,
        next_refresh: Instant,
    },
    Failed(RelayFailure),
}
struct Request {
    txid: TxId,
    attempts: u32,
    next_send: Instant,
    lifetime_secs: u16,
}
struct ClientAllocation {
    credential: Vec<u8>,
    phase: Phase,
    request: Option<Request>,
}

/// QUIC-over-relay socket. Each allocation joins one two-party channel on the relay; the
/// counterpart appears to QUIC as a virtual address `relay_ip:counterpart_vport`, so one
/// socket (and one `QuicServer`) can serve several relayed peers. `send_to` wraps datagrams
/// in 9-byte relay data headers addressed to the counterpart; `recv_from` accepts only
/// datagrams from the relay address for a live allocation and its known counterpart. QUIC
/// runs end to end inside, so the relay sees only ciphertext. Allocation, cookie challenge,
/// refresh and counterpart discovery happen inside `send_to`/`recv_from` (or
/// [`Self::poll_control`] before QUIC owns the socket), bounded by the configured rates.
pub struct RelaySocket<S> {
    inner: S,
    relay: SocketAddr,
    config: RelaySocketConfig,
    allocations: Vec<ClientAllocation>,
    held: VecDeque<(SocketAddr, Vec<u8>)>,
    scratch: Vec<u8>,
    out: Vec<u8>,
    stats: RelaySocketStats,
}
impl<S: DatagramSocket> RelaySocket<S> {
    pub fn new(inner: S, relay: SocketAddr, config: RelaySocketConfig) -> Result<Self> {
        config.validate()?;
        if !wire::valid_endpoint(&relay) {
            return Err(Error::Invalid("invalid relay address"));
        }
        Ok(Self {
            inner,
            relay,
            config,
            allocations: Vec::new(),
            held: VecDeque::new(),
            scratch: vec![0; 64 * 1024],
            out: Vec::with_capacity(2048),
            stats: RelaySocketStats::default(),
        })
    }
    pub fn relay_addr(&self) -> SocketAddr {
        self.relay
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }
    pub fn stats(&self) -> RelaySocketStats {
        self.stats
    }
    /// Starts an allocation with an opaque credential (validated by the relay's authorizer).
    pub fn allocate(&mut self, credential: &[u8]) -> Result<AllocationId> {
        if credential.is_empty() || credential.len() > wire::MAX_CREDENTIAL_BYTES {
            return Err(Error::Invalid("relay credential must be 1..=1024 bytes"));
        }
        if self.allocations.len() >= self.config.max_allocations {
            return Err(Error::Invalid("too many relay allocations"));
        }
        self.allocations.push(ClientAllocation {
            credential: credential.to_vec(),
            phase: Phase::Allocating {
                cookie: [0; COOKIE_LEN],
            },
            request: None,
        });
        Ok(AllocationId(self.allocations.len() - 1))
    }
    pub fn status(&self, id: AllocationId) -> Option<AllocationStatus> {
        Some(match &self.allocations.get(id.0)?.phase {
            Phase::Allocating { .. } => AllocationStatus::Requesting,
            Phase::Active {
                counterpart: None, ..
            } => AllocationStatus::WaitingForPeer,
            Phase::Active {
                counterpart: Some(port),
                ..
            } => AllocationStatus::Ready {
                peer: self.virtual_addr(*port),
            },
            Phase::Failed(failure) => AllocationStatus::Failed(*failure),
        })
    }
    /// Our own virtual port on the relay once allocated.
    pub fn local_vport(&self, id: AllocationId) -> Option<u16> {
        match self.allocations.get(id.0)?.phase {
            Phase::Active { vport, .. } => Some(vport),
            _ => None,
        }
    }
    /// Releases the allocation (one best-effort release request; the server also expires it).
    pub fn release(&mut self, id: AllocationId) {
        let Some(allocation) = self.allocations.get_mut(id.0) else {
            return;
        };
        if let Phase::Active { vport, key, .. } = allocation.phase {
            if let Ok(txid) = wire::random_bytes() {
                wire::encode_refresh(
                    &mut self.out,
                    &Refresh {
                        txid,
                        vport,
                        key,
                        lifetime_secs: 0,
                    },
                );
                let _ = self.inner.send_to(&self.out, self.relay);
                self.stats.control_sent += 1;
            }
        }
        allocation.phase = Phase::Failed(RelayFailure::Released);
        allocation.request = None;
    }
    /// Sends due control requests and reads replies; relayed data read here is held (bounded)
    /// for `recv_from`. Call while establishing allocations before QUIC owns the socket.
    pub fn poll_control(&mut self) -> io::Result<()> {
        let now = Instant::now();
        self.drive_timers(now);
        for _ in 0..self.config.max_reads_per_call {
            match self.read_one(now) {
                Ok(Some((from, len))) => {
                    if self.held.len() < self.config.max_queued_datagrams {
                        self.held.push_back((from, self.scratch[..len].to_vec()));
                    } else {
                        self.stats.dropped_queue += 1;
                    }
                }
                Ok(None) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if is_transient(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn virtual_addr(&self, vport: u16) -> SocketAddr {
        SocketAddr::new(self.relay.ip(), vport)
    }

    /// Reads one datagram. `Ok(Some)` is relayed data in `scratch` with its virtual source;
    /// `Ok(None)` means a control reply or ignored datagram was consumed.
    fn read_one(&mut self, now: Instant) -> io::Result<Option<(SocketAddr, usize)>> {
        let (len, from) = self.inner.recv_from(&mut self.scratch)?;
        if from != self.relay || !wire::is_control_datagram(&self.scratch[..len]) {
            self.stats.dropped_foreign += 1;
            return Ok(None);
        }
        if self.scratch[0] == kind::DATA {
            let Some((source, destination, payload)) = wire::decode_data(&self.scratch[..len])
            else {
                self.stats.dropped_unmatched += 1;
                return Ok(None);
            };
            let matched = self.allocations.iter().any(|a| {
                matches!(a.phase, Phase::Active { vport, counterpart: Some(peer), .. }
                    if vport == destination && peer == source)
            });
            if !matched {
                self.stats.dropped_unmatched += 1;
                return Ok(None);
            }
            let payload_len = payload.len();
            self.scratch.copy_within(wire::DATA_HEADER_BYTES..len, 0);
            self.stats.data_received += 1;
            return Ok(Some((self.virtual_addr(source), payload_len)));
        }
        self.stats.control_received += 1;
        self.on_control(now, len);
        Ok(None)
    }

    fn find_request(&mut self, txid: &TxId) -> Option<&mut ClientAllocation> {
        self.allocations.iter_mut().find(|a| {
            a.request
                .as_ref()
                .is_some_and(|r| wire::ct_eq(&r.txid, txid))
        })
    }

    fn on_control(&mut self, now: Instant, len: usize) {
        let datagram = self.scratch[..len].to_vec();
        let config = self.config.clone();
        let refresh_at = |lifetime: Duration, counterpart: Option<u16>| {
            now + if counterpart.is_some() {
                lifetime / 3
            } else {
                config.peer_poll_interval.min(lifetime / 3)
            }
        };
        if let Some((txid, cookie)) = wire::decode_challenge(&datagram) {
            if let Some(a) = self.find_request(&txid)
                && let Phase::Allocating { cookie: held } = &mut a.phase
            {
                *held = cookie;
                if let Some(r) = a.request.as_mut() {
                    r.next_send = now;
                }
            }
        } else if let Some(ok) = wire::decode_allocate_ok(&datagram) {
            if let Some(a) = self.find_request(&ok.txid)
                && matches!(a.phase, Phase::Allocating { .. })
            {
                let lifetime = Duration::from_secs(u64::from(ok.lifetime_secs));
                let counterpart = (ok.counterpart != 0).then_some(ok.counterpart);
                a.phase = Phase::Active {
                    vport: ok.vport,
                    key: ok.key,
                    expires: now + lifetime,
                    counterpart,
                    next_refresh: refresh_at(lifetime, counterpart),
                };
                a.request = None;
            }
        } else if let Some(ok) = wire::decode_refresh_ok(&datagram) {
            if let Some(a) = self.find_request(&ok.txid)
                && let Phase::Active {
                    expires,
                    counterpart,
                    next_refresh,
                    vport,
                    ..
                } = &mut a.phase
            {
                let lifetime = Duration::from_secs(u64::from(ok.lifetime_secs));
                *counterpart =
                    (ok.counterpart != 0 && ok.counterpart != *vport).then_some(ok.counterpart);
                *expires = now + lifetime;
                *next_refresh = refresh_at(lifetime, *counterpart);
                a.request = None;
            }
        } else if let Some((txid, code)) = wire::decode_error(&datagram)
            && let Some(a) = self.find_request(&txid)
        {
            let refusal = RelayRefusal::from_code(code).unwrap_or(RelayRefusal::Unauthorized);
            a.phase = Phase::Failed(RelayFailure::Refused(refusal));
            a.request = None;
        } else {
            self.stats.dropped_unmatched += 1;
        }
    }

    /// Starts, retransmits and expires requests; sends at most one datagram per allocation.
    fn drive_timers(&mut self, now: Instant) {
        let lifetime_secs = self.config.requested_lifetime.as_secs().min(3600) as u16;
        for index in 0..self.allocations.len() {
            let allocation = &mut self.allocations[index];
            match allocation.phase {
                Phase::Failed(_) => continue,
                Phase::Active { expires, .. } if now >= expires => {
                    allocation.phase = Phase::Failed(RelayFailure::Expired);
                    allocation.request = None;
                    continue;
                }
                Phase::Active { next_refresh, .. }
                    if allocation.request.is_none() && now >= next_refresh =>
                {
                    allocation.request = new_request(now, lifetime_secs);
                }
                Phase::Allocating { .. } if allocation.request.is_none() => {
                    allocation.request = new_request(now, lifetime_secs);
                }
                _ => {}
            }
            let Some(request) = allocation.request.as_mut() else {
                continue;
            };
            if now < request.next_send {
                continue;
            }
            if request.attempts >= self.config.max_request_attempts {
                match &mut allocation.phase {
                    Phase::Allocating { .. } => {
                        allocation.phase = Phase::Failed(RelayFailure::TimedOut)
                    }
                    Phase::Active { next_refresh, .. } => {
                        *next_refresh = now + self.config.request_interval
                    }
                    Phase::Failed(_) => {}
                }
                allocation.request = None;
                continue;
            }
            request.attempts += 1;
            request.next_send = now + self.config.request_interval;
            let encoded = match &allocation.phase {
                Phase::Allocating { cookie } => wire::encode_allocate(
                    &mut self.out,
                    &Allocate {
                        txid: request.txid,
                        cookie: *cookie,
                        credential: &allocation.credential,
                    },
                )
                .is_ok(),
                Phase::Active { vport, key, .. } => {
                    wire::encode_refresh(
                        &mut self.out,
                        &Refresh {
                            txid: request.txid,
                            vport: *vport,
                            key: *key,
                            lifetime_secs: request.lifetime_secs,
                        },
                    );
                    true
                }
                Phase::Failed(_) => false,
            };
            if encoded {
                let _ = self.inner.send_to(&self.out, self.relay);
                self.stats.control_sent += 1;
            }
        }
    }
}

fn new_request(now: Instant, lifetime_secs: u16) -> Option<Request> {
    Some(Request {
        txid: wire::random_bytes().ok()?,
        attempts: 0,
        next_send: now,
        lifetime_secs,
    })
}

impl<S: DatagramSocket> DatagramSocket for RelaySocket<S> {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        self.drive_timers(Instant::now());
        if datagram.is_empty() || datagram.len() > self.config.max_payload_bytes {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let route = (destination.ip() == self.relay.ip())
            .then(|| {
                self.allocations.iter().find_map(|a| match a.phase {
                    Phase::Active {
                        vport,
                        counterpart: Some(peer),
                        ..
                    } if peer == destination.port() => Some((vport, peer)),
                    _ => None,
                })
            })
            .flatten();
        let Some((source, peer)) = route else {
            self.stats.send_refused += 1;
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "destination is not a ready relay counterpart",
            ));
        };
        wire::encode_data_header(&mut self.out, source, peer);
        self.out.extend_from_slice(datagram);
        self.inner.send_to(&self.out, self.relay)?;
        self.stats.data_sent += 1;
        Ok(datagram.len())
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let now = Instant::now();
        self.drive_timers(now);
        if let Some((from, datagram)) = self.held.pop_front() {
            let len = datagram.len().min(buffer.len());
            buffer[..len].copy_from_slice(&datagram[..len]);
            return Ok((len, from));
        }
        for _ in 0..self.config.max_reads_per_call {
            if let Some((from, len)) = self.read_one(now)? {
                let len = len.min(buffer.len());
                buffer[..len].copy_from_slice(&self.scratch[..len]);
                return Ok((len, from));
            }
        }
        Err(io::ErrorKind::WouldBlock.into())
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}
