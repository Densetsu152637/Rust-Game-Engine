//! Thin driver that pumps a [`MessageEndpoint`] through an [`OnlineService`].
//!
//! `poll` is nonblocking: it reads transport events, feeds `Lane::SESSION` payloads to the
//! service, runs its timers, then sends the service's replies. Disconnects requested by the
//! service are applied after a short grace period so the peer can read the farewell error
//! (QUIC discards unread stream data when a CONNECTION_CLOSE arrives). The service has already
//! forgotten such a peer, so it receives no service during the grace period.
use crate::{
    CertificateBinding, OnlineError,
    gate::Now,
    service::{Action, OnlineService},
};
use engine_network::{Delivery, Lane, LinkEvent, MessageEndpoint, PeerId, PollBudget};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// How often transport RTT is copied into the service for host election.
const RTT_REFRESH_MS: u64 = 500;
/// Time a peer may keep its connection after the service rejected it, to read the reason.
pub const DISCONNECT_GRACE_MS: u64 = 250;

/// Driver counters (nonsecret).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriverStats {
    /// Payloads on other lanes or deliveries, discarded unread.
    pub ignored_messages: u64,
    /// Replies the transport refused (peer dropped for being unable to keep up).
    pub send_failures: u64,
}

pub struct OnlineServer<E: MessageEndpoint> {
    endpoint: E,
    service: OnlineService,
    binding: CertificateBinding,
    start: Instant,
    /// Peers to close and the monotonic time at which to close them.
    pending_disconnects: Vec<(PeerId, u64)>,
    next_rtt_ms: u64,
    stats: DriverStats,
}
impl<E: MessageEndpoint> OnlineServer<E> {
    /// `binding` must be the one whose `authorizer` the endpoint's security was built with;
    /// without it every peer is disconnected for lack of a certificate fingerprint.
    pub fn new(endpoint: E, service: OnlineService, binding: CertificateBinding) -> Self {
        Self {
            endpoint,
            service,
            binding,
            start: Instant::now(),
            pending_disconnects: Vec::new(),
            next_rtt_ms: 0,
            stats: DriverStats::default(),
        }
    }
    pub fn endpoint(&self) -> &E {
        &self.endpoint
    }
    pub fn endpoint_mut(&mut self) -> &mut E {
        &mut self.endpoint
    }
    pub fn service(&self) -> &OnlineService {
        &self.service
    }
    pub fn service_mut(&mut self) -> &mut OnlineService {
        &mut self.service
    }
    pub fn stats(&self) -> &DriverStats {
        &self.stats
    }
    pub fn now(&self) -> Now {
        Now {
            mono_ms: u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX),
            // A clock before 1970 yields 0, which makes every token "not yet valid" (fail closed).
            unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    /// One bounded pass; returns the number of transport events handled.
    pub fn poll(&mut self) -> Result<usize, OnlineError> {
        let now = self.now();
        self.poll_at(now)
    }

    pub fn poll_at(&mut self, now: Now) -> Result<usize, OnlineError> {
        let events = self.endpoint.poll(PollBudget::default())?;
        let mut waiting = Vec::new();
        for (peer, due_ms) in std::mem::take(&mut self.pending_disconnects) {
            if now.mono_ms >= due_ms {
                self.endpoint.disconnect(peer);
            } else {
                waiting.push((peer, due_ms));
            }
        }
        self.pending_disconnects = waiting;
        let handled = events.len();
        for event in events {
            match event {
                LinkEvent::Authenticated(_) => {}
                LinkEvent::Connected(peer) => {
                    let fingerprint = self.binding.take(peer);
                    self.service.on_connected(peer, fingerprint, now);
                }
                LinkEvent::Disconnected { peer, .. } => {
                    self.binding.forget(peer);
                    self.service.on_disconnected(peer, now);
                }
                LinkEvent::Message {
                    peer,
                    lane,
                    delivery,
                    payload,
                } => {
                    if lane == Lane::SESSION && delivery == Delivery::ReliableOrdered {
                        self.service.on_message(peer, &payload, now);
                    } else {
                        self.stats.ignored_messages = self.stats.ignored_messages.saturating_add(1);
                    }
                }
            }
        }
        self.service.tick(now);
        if now.mono_ms >= self.next_rtt_ms {
            self.next_rtt_ms = now.mono_ms.saturating_add(RTT_REFRESH_MS);
            for peer in self.service.authenticated_peer_ids() {
                if let Some(stats) = self.endpoint.stats(peer)
                    && stats.rtt_ms > 0.0
                {
                    self.service.set_rtt(peer, stats.rtt_ms.round() as u32);
                }
            }
        }
        self.flush(now);
        Ok(handled)
    }

    fn flush(&mut self, now: Now) {
        for action in self.service.drain_actions() {
            match action {
                Action::Send { peer, payload } => {
                    if self
                        .endpoint
                        .send(peer, Lane::SESSION, Delivery::ReliableOrdered, &payload)
                        .is_err()
                    {
                        // Full queue or vanished peer: a peer that cannot keep up is dropped
                        // rather than letting replies pile up without bound.
                        self.stats.send_failures = self.stats.send_failures.saturating_add(1);
                        self.service.on_disconnected(peer, now);
                        self.pending_disconnects.push((peer, now.mono_ms));
                    }
                }
                Action::Disconnect { peer, .. } => self
                    .pending_disconnects
                    .push((peer, now.mono_ms.saturating_add(DISCONNECT_GRACE_MS))),
            }
        }
    }

    /// Closes every connection immediately (CONNECTION_CLOSE / TCP close).
    pub fn shutdown(&mut self) {
        for (peer, _) in std::mem::take(&mut self.pending_disconnects) {
            self.endpoint.disconnect(peer);
        }
        for peer in self.endpoint.connected_peers() {
            self.endpoint.disconnect(peer);
        }
    }
}
