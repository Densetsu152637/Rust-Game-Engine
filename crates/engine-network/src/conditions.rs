//! Deterministic network impairment for tests, soak runs and development builds.
//!
//! [`NetworkConditions`] is authored data; [`Conditioner`] turns it into per-item fates with a
//! seeded PRNG; [`ConditionedEndpoint`] applies it to received `MessageEndpoint` events.
//! `engine-net-quic` applies the same [`Conditioner`] to UDP datagrams.
use crate::{
    Delivery, Error, Lane, LinkEvent, LinkStats, MessageEndpoint, PeerId, PollBudget, Result,
    TransportKind, TransportSecurity,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

/// Upper bound for `latency_ms` and `jitter_ms`.
pub const MAX_CONDITION_DELAY_MS: u32 = 10_000;
/// Minimum extra hold applied to a reordered item.
const MIN_REORDER_HOLD_MS: u32 = 10;

/// Simulated link impairment for one direction. Every field is optional in authored data;
/// unknown fields are rejected. `loss` and `duplicate` apply only to droppable items
/// (UDP datagrams, or unreliable deliveries at message level).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConditions {
    /// Probability in `[0, 1]` that a droppable item is discarded.
    pub loss: f64,
    /// Fixed one-way delay added to every item, in milliseconds (`0..=10_000`).
    pub latency_ms: u32,
    /// Additional uniformly distributed delay in `[0, jitter_ms]` (`0..=10_000`).
    pub jitter_ms: u32,
    /// Probability in `[0, 1]` that a droppable item is delivered twice.
    pub duplicate: f64,
    /// Probability in `[0, 1]` that an item is held back by an extra `max(jitter_ms, 10)` ms,
    /// letting later items overtake it.
    pub reorder: f64,
    /// Serialization rate of the simulated link; `None` is unlimited. Must not be zero.
    pub bandwidth_bytes_per_second: Option<u64>,
    /// PRNG seed. Equal seeds and equal inputs produce equal fates.
    pub seed: u64,
}
impl NetworkConditions {
    pub fn validate(&self) -> Result<()> {
        let probability = |p: f64| (0.0..=1.0).contains(&p);
        if !probability(self.loss)
            || !probability(self.duplicate)
            || !probability(self.reorder)
            || self.latency_ms > MAX_CONDITION_DELAY_MS
            || self.jitter_ms > MAX_CONDITION_DELAY_MS
            || self.bandwidth_bytes_per_second == Some(0)
        {
            return Err(Error::Invalid("invalid network conditions"));
        }
        Ok(())
    }
}

/// What a [`Conditioner`] decided for one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    Dropped,
    Deliver(Instant),
    Duplicate(Instant, Instant),
}

/// Seeded scheduler shared by message-level and datagram-level simulation. Items are serialized
/// through the configured bandwidth, then delayed by latency, jitter and reordering.
#[derive(Clone, Debug)]
pub struct Conditioner {
    conditions: NetworkConditions,
    state: u64,
    link_free_at: Option<Instant>,
}
impl Conditioner {
    pub fn new(conditions: NetworkConditions) -> Result<Self> {
        conditions.validate()?;
        Ok(Self {
            state: conditions.seed,
            conditions,
            link_free_at: None,
        })
    }
    pub fn conditions(&self) -> &NetworkConditions {
        &self.conditions
    }
    /// Decides the fate of one `bytes`-long item offered at `now`.
    pub fn schedule(&mut self, now: Instant, bytes: usize, droppable: bool) -> Fate {
        if droppable && self.chance(self.conditions.loss) {
            return Fate::Dropped;
        }
        let sent = match self.conditions.bandwidth_bytes_per_second {
            Some(rate) => {
                let start = self.link_free_at.map_or(now, |free| free.max(now));
                let done = start + Duration::from_secs_f64(bytes as f64 / rate as f64);
                self.link_free_at = Some(done);
                done
            }
            None => now,
        };
        let first = sent + self.delay();
        if droppable && self.chance(self.conditions.duplicate) {
            Fate::Duplicate(first, sent + self.delay())
        } else {
            Fate::Deliver(first)
        }
    }
    fn delay(&mut self) -> Duration {
        let jitter = self.conditions.jitter_ms;
        let mut ms = f64::from(self.conditions.latency_ms) + self.unit() * f64::from(jitter);
        if self.chance(self.conditions.reorder) {
            ms += f64::from(jitter.max(MIN_REORDER_HOLD_MS));
        }
        Duration::from_secs_f64(ms / 1000.0)
    }
    fn chance(&mut self, probability: f64) -> bool {
        probability > 0.0 && self.unit() < probability
    }
    /// SplitMix64 mapped to `[0, 1)`.
    fn unit(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// Default bound on events held by a [`ConditionedEndpoint`].
pub const DEFAULT_MAX_HELD_EVENTS: usize = 4096;
/// Default bound on payload bytes held by a [`ConditionedEndpoint`].
pub const DEFAULT_MAX_HELD_BYTES: usize = 8 * 1024 * 1024;

/// Message-level simulation around any [`MessageEndpoint`]. Every event received from the inner
/// endpoint is delayed (latency, jitter, reorder, bandwidth); only unreliable deliveries are
/// lost or duplicated. Reliable-ordered payloads keep their `(peer, lane)` order, stale
/// sequenced payloads are discarded on release, and lifecycle events never overtake earlier
/// payloads of the same peer. Sends are not delayed: wrap both endpoints to impair both
/// directions. While the held queue is at capacity the inner endpoint is polled with zero
/// message/byte budgets (timers and lifecycle only), so held memory is bounded by the capacity
/// plus one inner poll; unreliable payloads arriving over capacity are discarded and counted.
pub struct ConditionedEndpoint<E> {
    inner: E,
    conditioner: Conditioner,
    held: BTreeMap<(Instant, u64), Held>,
    held_bytes: usize,
    max_held_events: usize,
    max_held_bytes: usize,
    next_seq: u64,
    next_origin: u64,
    peer_horizon: BTreeMap<PeerId, Instant>,
    ordered_horizon: BTreeMap<(PeerId, Lane), Instant>,
    newest_sequenced: BTreeMap<(PeerId, Lane), u64>,
    dropped: BTreeMap<PeerId, u64>,
    ready: VecDeque<LinkEvent>,
}
struct Held {
    /// Index of the original inner event; duplicates share it.
    origin: u64,
    event: LinkEvent,
}
impl<E: MessageEndpoint> ConditionedEndpoint<E> {
    pub fn new(inner: E, conditions: NetworkConditions) -> Result<Self> {
        Self::with_capacity(
            inner,
            conditions,
            DEFAULT_MAX_HELD_EVENTS,
            DEFAULT_MAX_HELD_BYTES,
        )
    }
    pub fn with_capacity(
        inner: E,
        conditions: NetworkConditions,
        max_held_events: usize,
        max_held_bytes: usize,
    ) -> Result<Self> {
        if max_held_events == 0 || max_held_bytes == 0 {
            return Err(Error::Invalid(
                "conditioned endpoint capacity must be nonzero",
            ));
        }
        Ok(Self {
            inner,
            conditioner: Conditioner::new(conditions)?,
            held: BTreeMap::new(),
            held_bytes: 0,
            max_held_events,
            max_held_bytes,
            next_seq: 0,
            next_origin: 0,
            peer_horizon: BTreeMap::new(),
            ordered_horizon: BTreeMap::new(),
            newest_sequenced: BTreeMap::new(),
            dropped: BTreeMap::new(),
            ready: VecDeque::new(),
        })
    }
    pub fn inner(&self) -> &E {
        &self.inner
    }
    pub fn inner_mut(&mut self) -> &mut E {
        &mut self.inner
    }
    pub fn into_inner(self) -> E {
        self.inner
    }
    /// Events currently delayed.
    pub fn held_events(&self) -> usize {
        self.held.len()
    }
    fn full(&self) -> bool {
        self.held.len() >= self.max_held_events || self.held_bytes >= self.max_held_bytes
    }
    fn count_drop(&mut self, peer: PeerId) {
        *self.dropped.entry(peer).or_default() += 1;
    }
    fn hold(&mut self, release: Instant, origin: u64, event: LinkEvent) {
        let seq = self.next_seq;
        self.next_seq += 1;
        if let LinkEvent::Message { payload, .. } = &event {
            self.held_bytes += payload.len();
        }
        let peer = event_peer(&event);
        let horizon = self.peer_horizon.entry(peer).or_insert(release);
        *horizon = (*horizon).max(release);
        self.held.insert((release, seq), Held { origin, event });
    }
    fn schedule(&mut self, now: Instant, origin: u64, event: LinkEvent) {
        let LinkEvent::Message {
            peer,
            lane,
            delivery,
            payload,
        } = event
        else {
            // Lifecycle events wait for every earlier item of the same peer.
            let peer = event_peer(&event);
            let release = self.peer_horizon.get(&peer).map_or(now, |&h| h.max(now));
            self.hold(release, origin, event);
            return;
        };
        let droppable = !delivery.is_reliable();
        if droppable && self.full() {
            self.count_drop(peer);
            return;
        }
        let fate = self.conditioner.schedule(now, payload.len(), droppable);
        let (mut first, second) = match fate {
            Fate::Dropped => {
                self.count_drop(peer);
                return;
            }
            Fate::Deliver(at) => (at, None),
            Fate::Duplicate(a, b) => (a, Some(b)),
        };
        if delivery == Delivery::ReliableOrdered {
            let horizon = self.ordered_horizon.entry((peer, lane)).or_insert(first);
            first = first.max(*horizon);
            *horizon = first;
        }
        let message = |payload| LinkEvent::Message {
            peer,
            lane,
            delivery,
            payload,
        };
        if let Some(second) = second {
            self.hold(second, origin, message(payload.clone()));
        }
        self.hold(first, origin, message(payload));
    }
    fn release_due(&mut self, now: Instant) {
        while let Some(entry) = self.held.first_entry() {
            if entry.key().0 > now {
                break;
            }
            let Held { origin, event } = entry.remove();
            if let LinkEvent::Message {
                peer,
                lane,
                delivery,
                payload,
            } = &event
            {
                self.held_bytes -= payload.len();
                if *delivery == Delivery::UnreliableSequenced {
                    // Older payloads and duplicates of the newest one are stale.
                    let key = (*peer, *lane);
                    if self
                        .newest_sequenced
                        .get(&key)
                        .is_some_and(|&n| origin <= n)
                    {
                        let peer = *peer;
                        self.count_drop(peer);
                        continue;
                    }
                    self.newest_sequenced.insert(key, origin);
                }
            }
            self.ready.push_back(event);
        }
    }
}
fn event_peer(event: &LinkEvent) -> PeerId {
    match event {
        LinkEvent::Authenticated(peer) | LinkEvent::Connected(peer) => *peer,
        LinkEvent::Disconnected { peer, .. } | LinkEvent::Message { peer, .. } => *peer,
    }
}
impl<E: MessageEndpoint> MessageEndpoint for ConditionedEndpoint<E> {
    fn transport(&self) -> TransportKind {
        self.inner.transport()
    }
    fn security(&self) -> TransportSecurity {
        self.inner.security()
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        self.inner.connected_peers()
    }
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
        self.inner.max_payload(peer, delivery)
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        self.inner.send(peer, lane, delivery, payload)
    }
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        let budget = if self.full() {
            PollBudget {
                max_messages: 0,
                max_bytes: 0,
                ..budget
            }
        } else {
            budget
        };
        let now = Instant::now();
        for event in self.inner.poll(budget)? {
            let origin = self.next_origin;
            self.next_origin += 1;
            self.schedule(now, origin, event);
        }
        self.release_due(Instant::now());
        Ok(self.ready.drain(..).collect())
    }
    /// Inner statistics plus payloads this wrapper discarded (in `dropped_unreliable`).
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        let mut stats = self.inner.stats(peer)?;
        stats.dropped_unreliable += self.dropped.get(&peer).copied().unwrap_or(0);
        Some(stats)
    }
    fn disconnect(&mut self, peer: PeerId) -> bool {
        self.inner.disconnect(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_validate_and_reject_unknown_fields() {
        let parsed: NetworkConditions =
            serde_json::from_str(r#"{"loss":0.2,"latency_ms":40,"seed":7}"#).unwrap();
        assert_eq!(parsed.loss, 0.2);
        assert_eq!(parsed.jitter_ms, 0);
        assert!(parsed.validate().is_ok());
        assert!(serde_json::from_str::<NetworkConditions>(r#"{"los":0.2}"#).is_err());
        for bad in [
            NetworkConditions {
                loss: 1.5,
                ..Default::default()
            },
            NetworkConditions {
                duplicate: f64::NAN,
                ..Default::default()
            },
            NetworkConditions {
                reorder: -0.1,
                ..Default::default()
            },
            NetworkConditions {
                latency_ms: MAX_CONDITION_DELAY_MS + 1,
                ..Default::default()
            },
            NetworkConditions {
                bandwidth_bytes_per_second: Some(0),
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err());
            assert!(Conditioner::new(bad).is_err());
        }
    }

    #[test]
    fn conditioner_is_deterministic_and_matches_configured_rates() {
        let conditions = NetworkConditions {
            loss: 0.2,
            duplicate: 0.1,
            latency_ms: 30,
            jitter_ms: 10,
            seed: 42,
            ..Default::default()
        };
        let now = Instant::now();
        let run = || {
            let mut conditioner = Conditioner::new(conditions.clone()).unwrap();
            (0..10_000)
                .map(|_| conditioner.schedule(now, 100, true))
                .collect::<Vec<_>>()
        };
        let fates = run();
        assert_eq!(fates, run());
        let dropped = fates.iter().filter(|f| **f == Fate::Dropped).count();
        let duplicated = fates
            .iter()
            .filter(|f| matches!(f, Fate::Duplicate(..)))
            .count();
        assert!((1_800..=2_200).contains(&dropped), "dropped {dropped}");
        assert!(
            (600..=1_000).contains(&duplicated),
            "duplicated {duplicated}"
        );
        for fate in fates {
            if let Fate::Deliver(at) | Fate::Duplicate(at, _) = fate {
                let delay = at - now;
                assert!(delay >= Duration::from_millis(30) && delay <= Duration::from_millis(40));
            }
        }
        let mut reliable = Conditioner::new(conditions).unwrap();
        assert!((0..1000).all(|_| reliable.schedule(now, 1, false) != Fate::Dropped));
    }

    #[test]
    fn bandwidth_serializes_items() {
        let mut conditioner = Conditioner::new(NetworkConditions {
            bandwidth_bytes_per_second: Some(10_000),
            ..Default::default()
        })
        .unwrap();
        let now = Instant::now();
        let mut last = now;
        for _ in 0..10 {
            let Fate::Deliver(at) = conditioner.schedule(now, 1_000, true) else {
                panic!("no loss configured");
            };
            last = at;
        }
        let total = last - now;
        assert!(total >= Duration::from_millis(999) && total <= Duration::from_millis(1001));
    }
}
