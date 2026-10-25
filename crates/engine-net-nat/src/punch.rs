//! Authenticated UDP hole punching between two peers that exchanged candidate sets and a
//! per-session secret out of band.
use crate::{
    CandidateKind, CandidateSet, ControlAgent, Error, Result, Transmit,
    limiter::TokenBucket,
    wire::{self, ADDR_LEN, HEADER_LEN, TXID_LEN, TxId, kind},
};
use ring::hmac;
use serde::Serialize;
use std::{
    cmp::Reverse,
    collections::VecDeque,
    fmt,
    net::SocketAddr,
    time::{Duration, Instant},
};

/// Per-session secret shared by both peers through the authenticated lobby channel. It keys
/// HMAC-SHA256 over every probe and acknowledgement. Never logged; `Debug` is redacted.
#[derive(Clone)]
pub struct PunchSecret([u8; 32]);
impl PunchSecret {
    pub const LEN: usize = 32;
    pub fn generate() -> Result<Self> {
        Ok(Self(wire::random_bytes()?))
    }
    pub fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }
    /// Raw bytes for transport over an authenticated, encrypted channel only.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        self.0
    }
}
impl fmt::Debug for PunchSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PunchSecret(..)")
    }
}

/// ICE-style roles: the controlling side nominates the selected pair. Both peers must take
/// opposite roles (for example the joining client controls, the host is controlled).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PunchRole {
    Controlling,
    Controlled,
}

/// Timing and rate bounds of a [`PunchSession`]. Defaults: probe every 40 ms per candidate,
/// at most 60 probes per candidate, 4 s overall, 100 ms nomination wait for a better pair,
/// 16 remote candidates, 400 datagrams/s total and 100 acknowledgements/s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PunchConfig {
    pub probe_interval: Duration,
    pub max_probes_per_candidate: u32,
    pub timeout: Duration,
    pub nomination_wait: Duration,
    /// Advertised plus peer-reflexive remote candidates kept.
    pub max_remote_candidates: usize,
    pub max_datagrams_per_second: u32,
    pub max_acks_per_second: u32,
}
impl Default for PunchConfig {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_millis(40),
            max_probes_per_candidate: 60,
            timeout: Duration::from_secs(4),
            nomination_wait: Duration::from_millis(100),
            max_remote_candidates: 16,
            max_datagrams_per_second: 400,
            max_acks_per_second: 100,
        }
    }
}
impl PunchConfig {
    pub fn validate(&self) -> Result<()> {
        if self.probe_interval < Duration::from_millis(5)
            || self.probe_interval > Duration::from_secs(5)
            || self.max_probes_per_candidate == 0
            || self.max_probes_per_candidate > 1000
            || self.timeout < self.probe_interval
            || self.timeout > Duration::from_secs(120)
            || self.nomination_wait > self.timeout
            || self.max_remote_candidates < CandidateSet::MAX_CANDIDATES
            || self.max_remote_candidates > 64
            || self.max_datagrams_per_second == 0
            || self.max_datagrams_per_second > 10_000
            || self.max_acks_per_second == 0
            || self.max_acks_per_second > 10_000
        {
            return Err(Error::Invalid("invalid punch config"));
        }
        Ok(())
    }
}

/// The selected direct path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PunchPath {
    /// Remote address to hand to QUIC (`QuicClient::with_socket(.., remote, ..)`).
    pub remote: SocketAddr,
    pub kind: CandidateKind,
    /// Probe round-trip time on this pair.
    pub rtt: Duration,
    /// Time from session start to selection.
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PunchOutcome {
    Pending,
    Connected(PunchPath),
    /// No pair was selected before the deadline: fall back to the relay.
    TimedOut,
}

/// Nonsecret counters of a [`PunchSession`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PunchStats {
    pub probes_sent: u64,
    pub acks_sent: u64,
    pub probes_received: u64,
    pub acks_received: u64,
    /// Datagrams of this session whose HMAC failed (spoofed, tampered or foreign key).
    pub rejected_auth: u64,
    /// Authenticated datagrams claiming our own role (reflected or misconfigured).
    pub rejected_role: u64,
    /// Authenticated acknowledgements matching no outstanding probe on that address
    /// (replays, late duplicates, wrong source).
    pub rejected_unmatched: u64,
    /// Probes left unanswered by the acknowledgement rate limit.
    pub rate_limited: u64,
}

const FLAG_CONTROLLING: u8 = 0x01;
const FLAG_NOMINATE: u8 = 0x02;
const SESSION_LEN: usize = 8;
const SIGNED_LEN: usize = HEADER_LEN + SESSION_LEN + 1 + TXID_LEN + ADDR_LEN;
const TAG_LEN: usize = 32;
/// Probes and acknowledgements share this fixed size, so answering never amplifies.
pub(crate) const PUNCH_LEN: usize = SIGNED_LEN + TAG_LEN;
const OUTSTANDING_PER_CANDIDATE: usize = 4;
const MAX_PENDING_ACKS: usize = 16;

/// Structural view of a probe or acknowledgement (before authentication).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PunchPacket {
    pub kind: u8,
    pub session: [u8; SESSION_LEN],
    pub flags: u8,
    pub txid: TxId,
    pub observed: Option<SocketAddr>,
}
pub(crate) fn decode_punch(datagram: &[u8]) -> Option<PunchPacket> {
    if datagram.len() != PUNCH_LEN {
        return None;
    }
    let packet_kind = wire::control_kind(datagram)?;
    let mut r = wire::Reader::new(datagram, packet_kind)?;
    let session = r.array()?;
    let flags = r.u8()?;
    let txid: TxId = r.array()?;
    let addr = r.take(ADDR_LEN)?;
    let observed = match packet_kind {
        kind::PUNCH_PROBE if addr.iter().all(|&b| b == 0) => None,
        kind::PUNCH_ACK => Some(wire::read_addr(addr, Some(&txid))?),
        _ => return None,
    };
    (flags & !(FLAG_CONTROLLING | FLAG_NOMINATE) == 0).then_some(PunchPacket {
        kind: packet_kind,
        session,
        flags,
        txid,
        observed,
    })
}

struct Outstanding {
    txid: TxId,
    sent: Instant,
    nominate: bool,
}
struct Remote {
    addr: SocketAddr,
    kind: CandidateKind,
    probes: u32,
    next_probe: Instant,
    outstanding: [Option<Outstanding>; OUTSTANDING_PER_CANDIDATE],
    next_slot: usize,
    /// Set once one of our probes to this address was acknowledged from it.
    rtt: Option<Duration>,
}
impl Remote {
    fn new(addr: SocketAddr, kind: CandidateKind, now: Instant) -> Self {
        Self {
            addr,
            kind,
            probes: 0,
            next_probe: now,
            outstanding: Default::default(),
            next_slot: 0,
            rtt: None,
        }
    }
}

enum State {
    Checking,
    Nominating {
        index: usize,
        probes: u32,
        next: Instant,
    },
    Connected(PunchPath),
    TimedOut,
}

/// Sans-IO hole-punching session with one peer (simplified ICE): probes every advertised
/// host/server-reflexive candidate, answers authenticated probes, learns peer-reflexive
/// addresses, validates acknowledgements against outstanding transactions per address and
/// selects the best working pair. The controlling side nominates; the controlled side
/// completes once the nominated pair also passed its own check. Unauthenticated, spoofed,
/// reflected and replayed datagrams are ignored and counted. All sending is bounded by
/// per-candidate probe caps, a datagram rate and an acknowledgement rate.
pub struct PunchSession {
    role: PunchRole,
    key: hmac::Key,
    session: [u8; SESSION_LEN],
    config: PunchConfig,
    started: Instant,
    deadline: Instant,
    remotes: Vec<Remote>,
    state: State,
    nominated: Option<SocketAddr>,
    first_success: Option<Instant>,
    observed_by_peer: Option<SocketAddr>,
    pending_acks: VecDeque<Transmit>,
    datagrams: TokenBucket,
    acks: TokenBucket,
    stats: PunchStats,
}
impl PunchSession {
    pub fn new(
        now: Instant,
        role: PunchRole,
        secret: &PunchSecret,
        remote: &CandidateSet,
        config: PunchConfig,
    ) -> Result<Self> {
        config.validate()?;
        let master = hmac::Key::new(hmac::HMAC_SHA256, &secret.0);
        let key_bytes = hmac::sign(&master, b"engine-net-nat/punch/key/v1");
        let session_tag = hmac::sign(&master, b"engine-net-nat/punch/session/v1");
        let mut session = [0; SESSION_LEN];
        session.copy_from_slice(&session_tag.as_ref()[..SESSION_LEN]);
        let mut remotes: Vec<Remote> = remote
            .candidates()
            .iter()
            .filter(|c| c.kind != CandidateKind::Relay)
            .map(|c| Remote::new(c.addr, c.kind, now))
            .collect();
        remotes.sort_by_key(|r| Reverse(r.kind.preference()));
        let pps = f64::from(config.max_datagrams_per_second);
        let aps = f64::from(config.max_acks_per_second);
        Ok(Self {
            role,
            key: hmac::Key::new(hmac::HMAC_SHA256, key_bytes.as_ref()),
            session,
            started: now,
            deadline: now + config.timeout,
            remotes,
            state: State::Checking,
            nominated: None,
            first_success: None,
            observed_by_peer: None,
            pending_acks: VecDeque::new(),
            datagrams: TokenBucket::new(now, pps, (pps / 10.0).max(8.0)),
            acks: TokenBucket::new(now, aps, (aps / 4.0).max(4.0)),
            stats: PunchStats::default(),
            config,
        })
    }
    pub fn role(&self) -> PunchRole {
        self.role
    }
    pub fn outcome(&self) -> PunchOutcome {
        match &self.state {
            State::Connected(path) => PunchOutcome::Connected(*path),
            State::TimedOut => PunchOutcome::TimedOut,
            State::Checking | State::Nominating { .. } => PunchOutcome::Pending,
        }
    }
    pub fn stats(&self) -> PunchStats {
        self.stats
    }
    /// Our address as the peer observed it on an authenticated acknowledgement.
    pub fn observed_by_peer(&self) -> Option<SocketAddr> {
        self.observed_by_peer
    }
    /// Advances the deadline; `handle` and `poll_transmit` call this too.
    pub fn update(&mut self, now: Instant) {
        if matches!(self.state, State::Checking | State::Nominating { .. }) && now >= self.deadline
        {
            self.state = State::TimedOut;
        }
    }

    fn encode(
        &self,
        packet_kind: u8,
        flags: u8,
        txid: &TxId,
        observed: Option<SocketAddr>,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(PUNCH_LEN);
        wire::header(&mut out, packet_kind);
        out.extend_from_slice(&self.session);
        out.push(flags);
        out.extend_from_slice(txid);
        match observed {
            Some(addr) => wire::put_addr(&mut out, addr, Some(txid)),
            None => out.resize(SIGNED_LEN, 0),
        }
        let tag = hmac::sign(&self.key, &out);
        out.extend_from_slice(tag.as_ref());
        out
    }
    fn role_flag(&self) -> u8 {
        match self.role {
            PunchRole::Controlling => FLAG_CONTROLLING,
            PunchRole::Controlled => 0,
        }
    }
    fn remote_index(&self, addr: SocketAddr) -> Option<usize> {
        self.remotes.iter().position(|r| r.addr == addr)
    }
    fn is_active(&self) -> bool {
        matches!(self.state, State::Checking | State::Nominating { .. })
    }

    fn on_probe(&mut self, now: Instant, from: SocketAddr, packet: &PunchPacket) {
        self.stats.probes_received += 1;
        if self.pending_acks.len() >= MAX_PENDING_ACKS || !self.acks.try_take(now, 1.0) {
            self.stats.rate_limited += 1;
            return;
        }
        let nominate = packet.flags & FLAG_NOMINATE;
        let payload = self.encode(
            kind::PUNCH_ACK,
            self.role_flag() | nominate,
            &packet.txid,
            Some(from),
        );
        self.pending_acks.push_back(Transmit {
            destination: from,
            payload,
        });
        if !self.is_active() {
            return;
        }
        // Triggered check: probe the source back right away (adding it as peer-reflexive).
        let index = match self.remote_index(from) {
            Some(index) => Some(index),
            None if self.remotes.len() < self.config.max_remote_candidates => {
                self.remotes
                    .push(Remote::new(from, CandidateKind::PeerReflexive, now));
                Some(self.remotes.len() - 1)
            }
            None => None,
        };
        if let Some(remote) = index.map(|i| &mut self.remotes[i])
            && remote.rtt.is_none()
        {
            remote.next_probe = remote.next_probe.min(now);
        }
        if nominate != 0 && self.role == PunchRole::Controlled {
            self.nominated = Some(from);
        }
        self.complete_controlled(now);
    }

    fn on_ack(&mut self, now: Instant, from: SocketAddr, packet: &PunchPacket) {
        let matched = self.remote_index(from).and_then(|index| {
            let slot = self.remotes[index].outstanding.iter_mut().find(|o| {
                o.as_ref()
                    .is_some_and(|o| wire::ct_eq(&o.txid, &packet.txid))
            })?;
            slot.take().map(|o| (index, o))
        });
        let Some((index, outstanding)) = matched else {
            self.stats.rejected_unmatched += 1;
            return;
        };
        self.stats.acks_received += 1;
        self.observed_by_peer = packet.observed;
        let rtt = now.saturating_duration_since(outstanding.sent);
        let remote = &mut self.remotes[index];
        if remote.rtt.is_none() {
            remote.rtt = Some(rtt);
            self.first_success.get_or_insert(now);
        }
        if let State::Nominating {
            index: nominated, ..
        } = self.state
            && nominated == index
            && outstanding.nominate
            && packet.flags & FLAG_NOMINATE != 0
        {
            self.state = State::Connected(self.path(index, now));
            return;
        }
        self.complete_controlled(now);
    }

    fn path(&self, index: usize, now: Instant) -> PunchPath {
        let remote = &self.remotes[index];
        PunchPath {
            remote: remote.addr,
            kind: remote.kind,
            rtt: remote.rtt.unwrap_or_default(),
            elapsed: now.saturating_duration_since(self.started),
        }
    }

    /// Controlled side: the nominated pair must also have passed our own check.
    fn complete_controlled(&mut self, now: Instant) {
        if self.role != PunchRole::Controlled || !matches!(self.state, State::Checking) {
            return;
        }
        let Some(index) = self.nominated.and_then(|addr| self.remote_index(addr)) else {
            return;
        };
        if self.remotes[index].rtt.is_some() {
            self.state = State::Connected(self.path(index, now));
        }
    }

    /// Controlling side: nominate the best succeeded pair once no better pair can still win
    /// or the nomination wait after the first success has elapsed.
    fn maybe_nominate(&mut self, now: Instant) {
        if self.role != PunchRole::Controlling || !matches!(self.state, State::Checking) {
            return;
        }
        let Some((index, best)) = self
            .remotes
            .iter()
            .enumerate()
            .filter(|(_, r)| r.rtt.is_some())
            .max_by_key(|(_, r)| (r.kind.preference(), Reverse(r.rtt)))
        else {
            return;
        };
        let best_possible = self
            .remotes
            .iter()
            .filter(|r| r.rtt.is_some() || r.probes < self.config.max_probes_per_candidate)
            .map(|r| r.kind.preference())
            .max()
            .unwrap_or(0);
        let waited = self
            .first_success
            .is_some_and(|first| now >= first + self.config.nomination_wait);
        if best.kind.preference() >= best_possible || waited {
            self.state = State::Nominating {
                index,
                probes: 0,
                next: now,
            };
        }
    }

    /// The next (candidate index, nominate) probe that is due.
    fn due_probe(&self, now: Instant) -> Option<(usize, bool)> {
        match self.state {
            State::Nominating {
                index,
                probes,
                next,
            } => (now >= next && probes < self.config.max_probes_per_candidate)
                .then_some((index, true)),
            State::Checking => self
                .remotes
                .iter()
                .position(|r| {
                    r.rtt.is_none()
                        && r.probes < self.config.max_probes_per_candidate
                        && r.next_probe <= now
                })
                .map(|index| (index, false)),
            State::Connected(_) | State::TimedOut => None,
        }
    }

    fn probe(&mut self, now: Instant, index: usize, nominate: bool) -> Result<Transmit> {
        let txid: TxId = wire::random_bytes()?;
        let flags = self.role_flag() | if nominate { FLAG_NOMINATE } else { 0 };
        let payload = self.encode(kind::PUNCH_PROBE, flags, &txid, None);
        let interval = self.config.probe_interval;
        let remote = &mut self.remotes[index];
        remote.outstanding[remote.next_slot] = Some(Outstanding {
            txid,
            sent: now,
            nominate,
        });
        remote.next_slot = (remote.next_slot + 1) % OUTSTANDING_PER_CANDIDATE;
        if nominate {
            if let State::Nominating { probes, next, .. } = &mut self.state {
                *probes += 1;
                *next = now + interval;
            }
        } else {
            remote.probes += 1;
            remote.next_probe = now + interval;
        }
        self.stats.probes_sent += 1;
        Ok(Transmit {
            destination: remote.addr,
            payload,
        })
    }
}

impl ControlAgent for PunchSession {
    fn handle(&mut self, now: Instant, from: SocketAddr, datagram: &[u8]) -> bool {
        let Some(packet) = decode_punch(datagram) else {
            return false;
        };
        if !wire::ct_eq(&packet.session, &self.session) {
            return false;
        }
        self.update(now);
        let (signed, tag) = datagram.split_at(SIGNED_LEN);
        if hmac::verify(&self.key, signed, tag).is_err() {
            self.stats.rejected_auth += 1;
            return true;
        }
        let sender_controlling = packet.flags & FLAG_CONTROLLING != 0;
        if sender_controlling == (self.role == PunchRole::Controlling) {
            self.stats.rejected_role += 1;
            return true;
        }
        match packet.kind {
            kind::PUNCH_PROBE => self.on_probe(now, from, &packet),
            _ => self.on_ack(now, from, &packet),
        }
        true
    }

    fn poll_transmit(&mut self, now: Instant) -> Option<Transmit> {
        self.update(now);
        if !self.pending_acks.is_empty() {
            if !self.datagrams.try_take(now, 1.0) {
                return None;
            }
            self.stats.acks_sent += 1;
            return self.pending_acks.pop_front();
        }
        self.maybe_nominate(now);
        let (index, nominate) = self.due_probe(now)?;
        if !self.datagrams.try_take(now, 1.0) {
            return None;
        }
        // A failing system RNG ends probing for this poll; the deadline still applies.
        self.probe(now, index, nominate).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Candidate;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }
    fn set(ports: &[u16]) -> CandidateSet {
        let mut set = CandidateSet::new();
        for &port in ports {
            set.push(Candidate {
                kind: CandidateKind::ServerReflexive,
                addr: addr(port),
            })
            .unwrap();
        }
        set
    }
    /// Delivers every due datagram between two in-memory sessions at `a_addr`/`b_addr`.
    fn exchange(
        a: &mut PunchSession,
        a_addr: SocketAddr,
        b: &mut PunchSession,
        b_addr: SocketAddr,
        now: Instant,
    ) {
        while let Some(t) = a.poll_transmit(now) {
            if t.destination == b_addr {
                assert!(b.handle(now, a_addr, &t.payload));
            }
        }
        while let Some(t) = b.poll_transmit(now) {
            if t.destination == a_addr {
                assert!(a.handle(now, b_addr, &t.payload));
            }
        }
    }

    #[test]
    fn sessions_select_the_working_pair_on_both_sides() {
        let t0 = Instant::now();
        let secret = PunchSecret::generate().unwrap();
        let (a_addr, b_addr) = (addr(1000), addr(2000));
        let config = PunchConfig::default();
        let mut a = PunchSession::new(
            t0,
            PunchRole::Controlling,
            &secret,
            &set(&[2000, 2999]),
            config.clone(),
        )
        .unwrap();
        let mut b =
            PunchSession::new(t0, PunchRole::Controlled, &secret, &set(&[1000]), config).unwrap();
        for step in 0..50 {
            exchange(
                &mut a,
                a_addr,
                &mut b,
                b_addr,
                t0 + Duration::from_millis(10 * step),
            );
        }
        let PunchOutcome::Connected(path) = a.outcome() else {
            panic!("controlling not connected: {:?}", a.outcome())
        };
        assert_eq!(path.remote, b_addr);
        assert!(matches!(b.outcome(), PunchOutcome::Connected(p) if p.remote == a_addr));
        assert_eq!(a.observed_by_peer(), Some(a_addr));
        assert_eq!(a.stats().rejected_auth + b.stats().rejected_auth, 0);
    }

    #[test]
    fn forged_reflected_and_replayed_datagrams_are_ignored() {
        let t0 = Instant::now();
        let secret = PunchSecret::generate().unwrap();
        let config = PunchConfig::default();
        let mut a = PunchSession::new(
            t0,
            PunchRole::Controlling,
            &secret,
            &set(&[2000]),
            config.clone(),
        )
        .unwrap();
        let mut b = PunchSession::new(
            t0,
            PunchRole::Controlled,
            &secret,
            &set(&[1000]),
            config.clone(),
        )
        .unwrap();
        let probe = a.poll_transmit(t0).unwrap();

        // Same session ID, different key: authentication fails, nothing is answered.
        let attacker_secret = PunchSecret::generate().unwrap();
        let mut forger = PunchSession::new(
            t0,
            PunchRole::Controlling,
            &attacker_secret,
            &set(&[2000]),
            config,
        )
        .unwrap();
        forger.session = b.session;
        let forged = forger.poll_transmit(t0).unwrap();
        assert!(b.handle(t0, addr(6666), &forged.payload));
        let mut tampered = probe.payload.clone();
        tampered[20] ^= 1;
        assert!(b.handle(t0, addr(1000), &tampered));
        assert_eq!(b.stats().rejected_auth, 2);
        assert!(
            b.poll_transmit(t0)
                .is_some_and(|t| t.destination == addr(1000)),
            "own probe only"
        );
        assert!(b.pending_acks.is_empty());

        // A's own probe reflected back at A is refused by the role check.
        assert!(a.handle(t0, addr(2000), &probe.payload));
        assert_eq!(a.stats().rejected_role, 1);

        // A genuine ack is accepted once; its replay matches no outstanding transaction.
        assert!(b.handle(t0, addr(1000), &probe.payload));
        let ack = b.poll_transmit(t0).unwrap();
        assert!(a.handle(t0, addr(2000), &ack.payload));
        assert!(a.handle(t0, addr(2000), &ack.payload));
        assert_eq!(a.stats().acks_received, 1);
        assert_eq!(a.stats().rejected_unmatched, 1);
        // An ack from a different address than the probed one is not matched either.
        let mut c = PunchSession::new(
            t0,
            PunchRole::Controlled,
            &secret,
            &set(&[1000]),
            PunchConfig::default(),
        )
        .unwrap();
        let probe = a.poll_transmit(t0 + Duration::from_millis(50));
        if let Some(probe) = probe {
            assert!(c.handle(t0, addr(1000), &probe.payload));
            let ack = c.poll_transmit(t0).unwrap();
            assert!(a.handle(t0, addr(7777), &ack.payload));
            assert_eq!(a.stats().rejected_unmatched, 2);
        }
        // Foreign sessions are not consumed at all.
        let other = PunchSecret::generate().unwrap();
        let mut d = PunchSession::new(
            t0,
            PunchRole::Controlled,
            &other,
            &set(&[1000]),
            PunchConfig::default(),
        )
        .unwrap();
        assert!(!d.handle(t0, addr(1000), &ack.payload));
    }

    #[test]
    fn acknowledgement_rate_and_size_are_bounded() {
        let t0 = Instant::now();
        let secret = PunchSecret::generate().unwrap();
        let config = PunchConfig {
            max_acks_per_second: 8,
            ..PunchConfig::default()
        };
        let mut a = PunchSession::new(
            t0,
            PunchRole::Controlling,
            &secret,
            &set(&[2000]),
            PunchConfig::default(),
        )
        .unwrap();
        let mut b = PunchSession::new(
            t0,
            PunchRole::Controlled,
            &secret,
            &CandidateSet::new(),
            config,
        )
        .unwrap();
        let probe = a.poll_transmit(t0).unwrap();
        for _ in 0..100 {
            b.handle(t0, addr(1000), &probe.payload);
        }
        let mut acks = 0;
        while let Some(t) = b.poll_transmit(t0) {
            assert!(t.payload.len() <= probe.payload.len(), "no amplification");
            acks += usize::from(decode_punch(&t.payload).unwrap().kind == kind::PUNCH_ACK);
        }
        assert!(acks <= 4, "burst bounded, got {acks}");
        assert!(b.stats().rate_limited >= 96);
    }

    #[test]
    fn unreachable_peers_time_out_after_bounded_probes() {
        let t0 = Instant::now();
        let secret = PunchSecret::generate().unwrap();
        let config = PunchConfig {
            max_probes_per_candidate: 10,
            timeout: Duration::from_secs(1),
            ..PunchConfig::default()
        };
        let mut a = PunchSession::new(
            t0,
            PunchRole::Controlling,
            &secret,
            &set(&[2000, 2001]),
            config,
        )
        .unwrap();
        let mut sent = 0;
        for step in 0..200 {
            while a
                .poll_transmit(t0 + Duration::from_millis(10 * step))
                .is_some()
            {
                sent += 1;
            }
        }
        assert_eq!(sent, 20);
        assert_eq!(a.outcome(), PunchOutcome::TimedOut);
    }
}
