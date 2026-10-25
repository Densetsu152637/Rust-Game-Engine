//! Self-hostable discovery + relay server: a sans-IO core ([`RelayServer`]) and a socket
//! driver ([`RelayService`]).
use crate::{
    Error, Result,
    agent::is_transient,
    limiter::{SourceLimiter, TokenBucket},
    wire::{self, AllocateOk, COOKIE_LEN, KEY_LEN, RefreshOk, TxId, kind},
};
use engine_net_quic::DatagramSocket;
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt, io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

/// Opaque identifier of one two-party relay channel, chosen by the credential issuer (for
/// example derived from a lobby session and the pair of players).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(pub [u8; 16]);
impl fmt::Debug for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChannelId(")?;
        for b in &self.0[..4] {
            write!(f, "{b:02x}")?;
        }
        write!(f, "..)")
    }
}

/// Which of the channel's two parties a credential authorizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Party {
    A,
    B,
}
impl Party {
    fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
    pub(crate) fn code(self) -> u8 {
        self.index() as u8
    }
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::A),
            1 => Some(Self::B),
            _ => None,
        }
    }
}

/// What a validated credential allows: one party slot of one channel, optionally until a
/// hard deadline (for example the credential's own expiry) that refreshes cannot extend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub channel: ChannelId,
    pub party: Party,
    /// Absolute allocation lifetime from the allocate request; `None` lets refreshes extend
    /// the allocation indefinitely.
    pub max_lifetime: Option<Duration>,
}

/// Native credential policy: `(credential, source address) -> Grant`. It runs only after
/// the source passed the stateless return-routability cookie, must be fast and nonblocking,
/// and fails closed by returning `None`. Integration passes relay-scoped online tokens here.
pub type Authorizer = dyn Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync;

/// Trusted server limits, validated by [`RelayServer::new`]. JSON field names match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayLimits {
    /// Live allocations (two per channel); 0 disables relaying (discovery only).
    pub max_allocations: usize,
    /// Lifetime granted per allocate/refresh; clients refresh before it ends.
    pub allocation_lifetime_secs: u32,
    /// Forwarded payload bytes per second per allocation (one-second burst).
    pub allocation_bytes_per_second: u64,
    pub allocation_packets_per_second: u32,
    /// Forwarded bytes and packets per second across all allocations.
    pub global_bytes_per_second: u64,
    pub global_packets_per_second: u32,
    /// Control requests per second per source. IPv4 sources are keyed by address and IPv6
    /// sources by /64. The rate applies separately to unauthenticated binding requests,
    /// unauthenticated allocate challenges, verified (cookie-bearing) allocates per source
    /// address and port, and refreshes per allocation. Spoofed traffic can only drain the
    /// unauthenticated buckets of the address it forges.
    pub control_requests_per_second_per_ip: u32,
    pub control_burst_per_ip: u32,
    /// Sources tracked by each control limiter table; new sources are refused when full of
    /// active ones.
    pub max_tracked_ips: usize,
    /// Credential checks ([`Authorizer`] calls) per second across all sources, and the burst.
    /// Only requests that proved return routability and passed their own source bucket can
    /// consume it.
    pub global_authorizations_per_second: u32,
    pub global_authorization_burst: u32,
    /// Largest relayed payload (QUIC datagram) in bytes.
    pub max_payload_bytes: usize,
    pub max_credential_bytes: usize,
    /// Validity window of return-routability cookies (accepted for one to two windows).
    pub cookie_lifetime_secs: u32,
}
impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            max_allocations: 1024,
            allocation_lifetime_secs: 120,
            allocation_bytes_per_second: 1024 * 1024,
            allocation_packets_per_second: 2000,
            global_bytes_per_second: 64 * 1024 * 1024,
            global_packets_per_second: 100_000,
            control_requests_per_second_per_ip: 20,
            control_burst_per_ip: 40,
            max_tracked_ips: 4096,
            global_authorizations_per_second: 200,
            global_authorization_burst: 400,
            max_payload_bytes: 1472,
            max_credential_bytes: 512,
            cookie_lifetime_secs: 10,
        }
    }
}
impl RelayLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_allocations > 16_384
            || !(1..=3600).contains(&self.allocation_lifetime_secs)
            || self.allocation_bytes_per_second < self.max_payload_bytes as u64
            || self.allocation_packets_per_second == 0
            || self.global_bytes_per_second < self.allocation_bytes_per_second
            || self.global_packets_per_second < self.allocation_packets_per_second
            || self.control_requests_per_second_per_ip == 0
            || self.control_burst_per_ip == 0
            || !(1..=1_000_000).contains(&self.max_tracked_ips)
            || self.global_authorizations_per_second == 0
            || self.global_authorization_burst == 0
            || !(1200..=65_000).contains(&self.max_payload_bytes)
            || !(1..=wire::MAX_CREDENTIAL_BYTES).contains(&self.max_credential_bytes)
            || !(1..=300).contains(&self.cookie_lifetime_secs)
        {
            return Err(Error::Invalid("invalid relay limits"));
        }
        Ok(())
    }
}

/// Reasons the relay refuses a request (wire error codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayRefusal {
    /// The credential failed the authorizer, or a refresh key/source did not match.
    Unauthorized,
    /// The channel's party slot is held by another address.
    PartyTaken,
    /// `max_allocations` reached, or relaying disabled.
    Capacity,
}
impl RelayRefusal {
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Unauthorized => 1,
            Self::PartyTaken => 2,
            Self::Capacity => 3,
        }
    }
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Unauthorized,
            2 => Self::PartyTaken,
            3 => Self::Capacity,
            _ => return None,
        })
    }
}

/// Nonsecret counters. `control_bytes_out <= control_bytes_in` always holds (no response is
/// larger than its request) and forwarded datagrams keep their size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RelayStats {
    pub datagrams_in: u64,
    pub bytes_in: u64,
    pub datagrams_out: u64,
    pub bytes_out: u64,
    /// Bytes of every non-data datagram received (requests, garbage, foreign kinds).
    pub control_bytes_in: u64,
    /// Bytes of every response sent back to a request's source.
    pub control_bytes_out: u64,
    pub binding_responses: u64,
    pub challenges: u64,
    pub allocations_created: u64,
    pub allocations_active: u64,
    pub allocations_expired: u64,
    pub refreshes: u64,
    pub releases: u64,
    pub refused_unauthorized: u64,
    pub refused_party_taken: u64,
    pub refused_capacity: u64,
    pub rate_limited_control: u64,
    pub malformed: u64,
    pub forwarded_packets: u64,
    pub forwarded_bytes: u64,
    pub data_refused_unknown: u64,
    pub data_refused_source: u64,
    pub data_refused_permission: u64,
    pub data_refused_rate: u64,
    /// Responses withheld because they would have exceeded the request size (never expected).
    pub suppressed_amplification: u64,
}

struct Allocation {
    channel: ChannelId,
    party: Party,
    /// Return-routable source that allocated; the only address that may use or receive it.
    bound: SocketAddr,
    key: [u8; KEY_LEN],
    expires: Instant,
    /// Hard end from the grant; refreshes never extend past it.
    deadline: Option<Instant>,
    bytes: TokenBucket,
    packets: TokenBucket,
    /// Refresh requests, charged only after the vport, bound address and key matched.
    control: TokenBucket,
}

/// What `handle` should do after a control request was processed.
enum Handled {
    /// A reply is in the output buffer.
    Reply,
    /// The request did not decode.
    Malformed,
    /// A rate bucket refused it; nothing is sent.
    Limited,
}

/// Sans-IO relay and reflexive discovery server.
///
/// - Binding requests are answered with the observed source address (response 36 bytes for
///   a 40-byte request).
/// - Allocation needs a stateless return-routability cookie (an HMAC of the source address
///   and time window), then a credential the native [`Authorizer`] accepts. Each channel has
///   exactly two party slots; the relay forwards data only from a slot's bound address to the
///   other slot's bound address (its whole permission list), never to arbitrary addresses.
/// - Allocations expire unless refreshed with their 128-bit key from the bound address.
/// - Every request is answered by at most one datagram no larger than the request; forwarded
///   data keeps its exact size. Control requests are rate limited per source, and charged
///   only for what the source has proven: unauthenticated binding and challenge requests per
///   source IP (a spoofer can drain only the forged address's unauthenticated buckets),
///   cookie-bearing allocates per source address and port, refreshes per allocation after
///   the key matched, and credential checks globally. Data is limited per allocation and
///   globally; state is bounded by `max_allocations` and `max_tracked_ips`.
pub struct RelayServer {
    limits: RelayLimits,
    authorize: Arc<Authorizer>,
    cookie_key: hmac::Key,
    epoch: Instant,
    allocations: HashMap<u16, Allocation>,
    channels: HashMap<ChannelId, [Option<u16>; 2]>,
    /// Unauthenticated binding requests, per source IP.
    binding_sources: SourceLimiter<IpAddr>,
    /// Unauthenticated allocate challenges and refused refreshes, per source IP.
    challenge_sources: SourceLimiter<IpAddr>,
    /// Allocates that proved return routability, per source address and port.
    verified_sources: SourceLimiter<SocketAddr>,
    /// Bounds [`Authorizer`] calls across all sources.
    authorizations: TokenBucket,
    global_bytes: TokenBucket,
    global_packets: TokenBucket,
    next_sweep: Instant,
    stats: RelayStats,
}
impl RelayServer {
    pub fn new(
        now: Instant,
        limits: RelayLimits,
        authorize: impl Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync + 'static,
    ) -> Result<Self> {
        limits.validate()?;
        let cookie_secret: [u8; 32] = wire::random_bytes()?;
        Ok(Self {
            authorize: Arc::new(authorize),
            cookie_key: hmac::Key::new(hmac::HMAC_SHA256, &cookie_secret),
            epoch: now,
            allocations: HashMap::new(),
            channels: HashMap::new(),
            binding_sources: Self::source_limiter(now, &limits),
            challenge_sources: Self::source_limiter(now, &limits),
            verified_sources: Self::source_limiter(now, &limits),
            authorizations: TokenBucket::new(
                now,
                f64::from(limits.global_authorizations_per_second),
                f64::from(limits.global_authorization_burst),
            ),
            global_bytes: TokenBucket::new(
                now,
                limits.global_bytes_per_second as f64,
                limits.global_bytes_per_second as f64,
            ),
            global_packets: TokenBucket::new(
                now,
                f64::from(limits.global_packets_per_second),
                f64::from(limits.global_packets_per_second),
            ),
            next_sweep: now,
            stats: RelayStats::default(),
            limits,
        })
    }
    fn source_limiter<K: crate::limiter::SourceKey>(
        now: Instant,
        limits: &RelayLimits,
    ) -> SourceLimiter<K> {
        SourceLimiter::new(
            now,
            limits.max_tracked_ips,
            f64::from(limits.control_requests_per_second_per_ip),
            f64::from(limits.control_burst_per_ip),
        )
    }
    pub fn limits(&self) -> &RelayLimits {
        &self.limits
    }
    pub fn stats(&self) -> RelayStats {
        RelayStats {
            allocations_active: self.allocations.len() as u64,
            ..self.stats
        }
    }

    /// Processes one received datagram. Returns the destination of the single reply or
    /// forwarded datagram written to `out`, or `None` when nothing is sent.
    pub fn handle(
        &mut self,
        now: Instant,
        from: SocketAddr,
        datagram: &[u8],
        out: &mut Vec<u8>,
    ) -> Option<SocketAddr> {
        out.clear();
        self.stats.datagrams_in += 1;
        self.stats.bytes_in += datagram.len() as u64;
        self.sweep(now);
        let packet_kind = wire::control_kind(datagram);
        if packet_kind == Some(kind::DATA) {
            let destination = self.forward(now, from, datagram, out)?;
            self.stats.datagrams_out += 1;
            self.stats.bytes_out += out.len() as u64;
            return Some(destination);
        }
        self.stats.control_bytes_in += datagram.len() as u64;
        let request = matches!(
            packet_kind,
            Some(kind::BINDING_REQUEST | kind::ALLOCATE | kind::REFRESH)
        );
        if !request {
            self.stats.malformed += 1;
            return None;
        }
        // Each handler charges the bucket matching what the source has proven so far, so
        // traffic from a forged source address cannot spend an honest owner's budget for
        // authenticated requests.
        let handled = match packet_kind {
            Some(kind::BINDING_REQUEST) => self.binding(now, from, datagram, out),
            Some(kind::ALLOCATE) => self.allocate(now, from, datagram, out),
            _ => self.refresh(now, from, datagram, out),
        };
        match handled {
            Handled::Reply => {}
            Handled::Malformed => {
                self.stats.malformed += 1;
                out.clear();
                return None;
            }
            Handled::Limited => {
                self.stats.rate_limited_control += 1;
                out.clear();
                return None;
            }
        }
        if out.len() > datagram.len() {
            self.stats.suppressed_amplification += 1;
            out.clear();
            return None;
        }
        self.stats.datagrams_out += 1;
        self.stats.bytes_out += out.len() as u64;
        self.stats.control_bytes_out += out.len() as u64;
        Some(from)
    }

    /// Removes expired allocations; runs at most once per second from `handle`.
    pub fn sweep(&mut self, now: Instant) {
        if now < self.next_sweep {
            return;
        }
        self.next_sweep = now + Duration::from_secs(1);
        let expired: Vec<u16> = self
            .allocations
            .iter()
            .filter(|(_, a)| a.expires <= now)
            .map(|(&vport, _)| vport)
            .collect();
        for vport in expired {
            self.remove(vport);
            self.stats.allocations_expired += 1;
        }
    }

    fn remove(&mut self, vport: u16) {
        let Some(allocation) = self.allocations.remove(&vport) else {
            return;
        };
        if let Some(slots) = self.channels.get_mut(&allocation.channel) {
            slots[allocation.party.index()] = None;
            if slots.iter().all(Option::is_none) {
                self.channels.remove(&allocation.channel);
            }
        }
    }

    /// The live allocation at `vport`, removing it first if it expired.
    fn live(&mut self, now: Instant, vport: u16) -> Option<&mut Allocation> {
        if self.allocations.get(&vport)?.expires <= now {
            self.remove(vport);
            self.stats.allocations_expired += 1;
            return None;
        }
        self.allocations.get_mut(&vport)
    }

    fn counterpart(&mut self, now: Instant, vport: u16) -> Option<u16> {
        let allocation = self.allocations.get(&vport)?;
        let other = self.channels.get(&allocation.channel)?[1 - allocation.party.index()]?;
        self.live(now, other).map(|_| other)
    }

    fn binding(
        &mut self,
        now: Instant,
        from: SocketAddr,
        datagram: &[u8],
        out: &mut Vec<u8>,
    ) -> Handled {
        let Some(txid) = wire::decode_binding_request(datagram) else {
            return Handled::Malformed;
        };
        if !self.binding_sources.allow(now, from.ip()) {
            return Handled::Limited;
        }
        wire::encode_binding_response(out, &txid, from);
        self.stats.binding_responses += 1;
        Handled::Reply
    }

    fn cookie(&self, from: SocketAddr, window: u64) -> [u8; COOKIE_LEN] {
        let mut input = Vec::with_capacity(8 + wire::ADDR_LEN);
        input.extend_from_slice(&window.to_be_bytes());
        wire::put_addr(&mut input, from, None);
        let tag = hmac::sign(&self.cookie_key, &input);
        let mut cookie = [0; COOKIE_LEN];
        cookie.copy_from_slice(&tag.as_ref()[..COOKIE_LEN]);
        cookie
    }

    fn allocate(
        &mut self,
        now: Instant,
        from: SocketAddr,
        datagram: &[u8],
        out: &mut Vec<u8>,
    ) -> Handled {
        let Some(request) = wire::decode_allocate(datagram) else {
            return Handled::Malformed;
        };
        let window = now.saturating_duration_since(self.epoch).as_secs()
            / u64::from(self.limits.cookie_lifetime_secs);
        let current = self.cookie(from, window);
        let routable = wire::ct_eq(&request.cookie, &current)
            || (window > 0 && wire::ct_eq(&request.cookie, &self.cookie(from, window - 1)));
        if !routable {
            // Cookie-less (or stale-cookie) request: the source may be forged. Only the
            // forged IP's challenge bucket is charged, never the verified buckets.
            if !self.challenge_sources.allow(now, from.ip()) {
                return Handled::Limited;
            }
            wire::encode_challenge(out, &request.txid, &current);
            self.stats.challenges += 1;
            return Handled::Reply;
        }
        // The source received our cookie, so the address is genuine: charge it, then the
        // global credential-check budget, before running the Authorizer.
        if !self.verified_sources.allow(now, from) || !self.authorizations.try_take(now, 1.0) {
            return Handled::Limited;
        }
        let txid = request.txid;
        let grant = (request.credential.len() <= self.limits.max_credential_bytes)
            .then(|| (self.authorize)(request.credential, from))
            .flatten();
        let Some(grant) = grant else {
            return self.refuse(out, &txid, RelayRefusal::Unauthorized);
        };
        if grant
            .max_lifetime
            .is_some_and(|lifetime| lifetime < Duration::from_secs(1))
        {
            return self.refuse(out, &txid, RelayRefusal::Unauthorized);
        }
        let existing = self
            .channels
            .get(&grant.channel)
            .and_then(|slots| slots[grant.party.index()]);
        let vport = match existing {
            // A retransmitted request from the same address gets the same allocation.
            Some(vport) if self.live(now, vport).is_some_and(|a| a.bound == from) => vport,
            Some(vport) if self.allocations.contains_key(&vport) => {
                return self.refuse(out, &txid, RelayRefusal::PartyTaken);
            }
            _ => match self.create(now, from, grant) {
                Some(vport) => vport,
                None => return self.refuse(out, &txid, RelayRefusal::Capacity),
            },
        };
        let counterpart = self.counterpart(now, vport).unwrap_or(0);
        let allocation = &self.allocations[&vport];
        let lifetime = allocation.expires.saturating_duration_since(now);
        wire::encode_allocate_ok(
            out,
            &AllocateOk {
                txid,
                vport,
                key: allocation.key,
                lifetime_secs: lifetime.as_secs().clamp(1, u64::from(u16::MAX)) as u16,
                counterpart,
                observed: from,
            },
        );
        Handled::Reply
    }

    /// `now + min(requested, server lifetime)`, never past the grant deadline.
    fn expiry(&self, now: Instant, requested: Duration, deadline: Option<Instant>) -> Instant {
        let lifetime = requested.min(Duration::from_secs(u64::from(
            self.limits.allocation_lifetime_secs,
        )));
        let expires = now + lifetime;
        deadline.map_or(expires, |deadline| expires.min(deadline))
    }

    fn create(&mut self, now: Instant, from: SocketAddr, grant: Grant) -> Option<u16> {
        if self.allocations.len() >= self.limits.max_allocations {
            return None;
        }
        let key: [u8; KEY_LEN] = wire::random_bytes().ok()?;
        let vport = (0..32)
            .filter_map(|_| wire::random_bytes::<2>().ok())
            .map(u16::from_be_bytes)
            .find(|&p| p != 0 && !self.allocations.contains_key(&p))?;
        let deadline = grant.max_lifetime.map(|lifetime| now + lifetime);
        let expires = self.expiry(now, Duration::MAX, deadline);
        let bytes_rate = self.limits.allocation_bytes_per_second as f64;
        let packet_rate = f64::from(self.limits.allocation_packets_per_second);
        self.allocations.insert(
            vport,
            Allocation {
                channel: grant.channel,
                party: grant.party,
                bound: from,
                key,
                expires,
                deadline,
                bytes: TokenBucket::new(now, bytes_rate, bytes_rate),
                packets: TokenBucket::new(now, packet_rate, packet_rate),
                control: TokenBucket::new(
                    now,
                    f64::from(self.limits.control_requests_per_second_per_ip),
                    f64::from(self.limits.control_burst_per_ip),
                ),
            },
        );
        self.channels.entry(grant.channel).or_default()[grant.party.index()] = Some(vport);
        self.stats.allocations_created += 1;
        Some(vport)
    }

    fn refuse(&mut self, out: &mut Vec<u8>, txid: &TxId, refusal: RelayRefusal) -> Handled {
        match refusal {
            RelayRefusal::Unauthorized => self.stats.refused_unauthorized += 1,
            RelayRefusal::PartyTaken => self.stats.refused_party_taken += 1,
            RelayRefusal::Capacity => self.stats.refused_capacity += 1,
        }
        wire::encode_error(out, txid, refusal.code());
        Handled::Reply
    }

    fn refresh(
        &mut self,
        now: Instant,
        from: SocketAddr,
        datagram: &[u8],
        out: &mut Vec<u8>,
    ) -> Handled {
        let Some(request) = wire::decode_refresh(datagram) else {
            return Handled::Malformed;
        };
        // Verify the vport, the bound address and the key first; only then charge the
        // allocation's own control bucket, so nobody can drain it without holding the key.
        let authorized = self
            .live(now, request.vport)
            .is_some_and(|a| a.bound == from && wire::ct_eq(&a.key, &request.key));
        if !authorized {
            // Refused with an error, charged like any other unauthenticated request.
            if !self.challenge_sources.allow(now, from.ip()) {
                return Handled::Limited;
            }
            return self.refuse(out, &request.txid, RelayRefusal::Unauthorized);
        }
        let within_budget = self
            .allocations
            .get_mut(&request.vport)
            .is_some_and(|a| a.control.try_take(now, 1.0));
        if !within_budget {
            return Handled::Limited;
        }
        let (lifetime, counterpart) = if request.lifetime_secs == 0 {
            self.remove(request.vport);
            self.stats.releases += 1;
            (0, 0)
        } else {
            let counterpart = self.counterpart(now, request.vport).unwrap_or(0);
            let requested = Duration::from_secs(u64::from(request.lifetime_secs));
            let deadline = self
                .allocations
                .get(&request.vport)
                .and_then(|a| a.deadline);
            let expires = self.expiry(now, requested, deadline);
            let Some(allocation) = self.allocations.get_mut(&request.vport) else {
                return Handled::Malformed;
            };
            allocation.expires = expires;
            self.stats.refreshes += 1;
            let lifetime = expires.saturating_duration_since(now).as_secs();
            (lifetime.clamp(1, u64::from(u16::MAX)) as u16, counterpart)
        };
        wire::encode_refresh_ok(
            out,
            &RefreshOk {
                txid: request.txid,
                lifetime_secs: lifetime,
                counterpart,
            },
        );
        Handled::Reply
    }

    fn forward(
        &mut self,
        now: Instant,
        from: SocketAddr,
        datagram: &[u8],
        out: &mut Vec<u8>,
    ) -> Option<SocketAddr> {
        let Some((source, destination, payload)) = wire::decode_data(datagram) else {
            self.stats.malformed += 1;
            return None;
        };
        if payload.len() > self.limits.max_payload_bytes {
            self.stats.malformed += 1;
            return None;
        }
        let Some(sender) = self.live(now, source) else {
            self.stats.data_refused_unknown += 1;
            return None;
        };
        if sender.bound != from {
            self.stats.data_refused_source += 1;
            return None;
        }
        if self.counterpart(now, source) != Some(destination) {
            self.stats.data_refused_permission += 1;
            return None;
        }
        let len = datagram.len() as f64;
        let sender = self.allocations.get_mut(&source)?;
        let allowed = sender.packets.try_take(now, 1.0)
            && sender.bytes.try_take(now, len)
            && self.global_packets.try_take(now, 1.0)
            && self.global_bytes.try_take(now, len);
        if !allowed {
            self.stats.data_refused_rate += 1;
            return None;
        }
        let target = self.allocations.get(&destination)?.bound;
        out.extend_from_slice(datagram);
        self.stats.forwarded_packets += 1;
        self.stats.forwarded_bytes += datagram.len() as u64;
        Some(target)
    }
}

/// Drives a [`RelayServer`] over a nonblocking [`DatagramSocket`] with bounded work per poll
/// and two fixed buffers (no unbounded queues). Send failures are packet loss.
pub struct RelayService<S> {
    socket: S,
    server: RelayServer,
    buffer: Vec<u8>,
    out: Vec<u8>,
}
impl<S: DatagramSocket> RelayService<S> {
    pub fn new(socket: S, server: RelayServer) -> Self {
        Self {
            socket,
            server,
            buffer: vec![0; 64 * 1024],
            out: Vec::with_capacity(2048),
        }
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    pub fn server(&self) -> &RelayServer {
        &self.server
    }
    pub fn server_mut(&mut self) -> &mut RelayServer {
        &mut self.server
    }
    /// Handles up to `max_datagrams` queued datagrams; returns how many were read.
    pub fn poll(&mut self, max_datagrams: usize) -> io::Result<usize> {
        let mut read = 0;
        for _ in 0..max_datagrams {
            let (len, from) = match self.socket.recv_from(&mut self.buffer) {
                Ok(received) => received,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if is_transient(&e) => continue,
                Err(e) => return Err(e),
            };
            read += 1;
            let now = Instant::now();
            if let Some(to) = self
                .server
                .handle(now, from, &self.buffer[..len], &mut self.out)
            {
                let _ = self.socket.send_to(&self.out, to);
            }
        }
        self.server.sweep(Instant::now());
        Ok(read)
    }
}
