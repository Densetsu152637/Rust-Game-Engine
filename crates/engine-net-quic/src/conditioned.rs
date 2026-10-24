//! Datagram-level network simulation.
use crate::DatagramSocket;
use engine_network::{Conditioner, Error, Fate, NetworkConditions, Result};
use serde::Serialize;
use std::{collections::BTreeMap, io, net::SocketAddr, time::Instant};

/// Default bound on datagrams delayed by a [`ConditionedSocket`].
pub const DEFAULT_MAX_DELAYED_DATAGRAMS: usize = 4096;
/// Default bound on bytes delayed by a [`ConditionedSocket`].
pub const DEFAULT_MAX_DELAYED_BYTES: usize = 8 * 1024 * 1024;

/// Nonsecret counters of a [`ConditionedSocket`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ConditionStats {
    /// Datagrams the endpoint handed to `send_to`.
    pub offered: u64,
    /// Discarded by simulated loss.
    pub dropped_loss: u64,
    /// Discarded because the delay queue was full.
    pub dropped_overflow: u64,
    /// Extra copies scheduled by simulated duplication.
    pub duplicated: u64,
    /// Datagrams written to the inner socket.
    pub delivered: u64,
}

/// Applies [`NetworkConditions`] (loss, latency, jitter, duplication, reorder, bandwidth) to
/// the datagrams this socket sends; received datagrams pass through unchanged, so wrap both
/// endpoints' sockets to impair both directions. Delayed datagrams sit in a bounded queue
/// (overflow is counted as loss) and are written to the inner socket by later `send_to` or
/// `recv_from` calls, which endpoints make on every poll; timing resolution is therefore the
/// caller's poll interval. The PRNG is seeded from the conditions, so runs are reproducible
/// for identical send sequences.
pub struct ConditionedSocket<S> {
    inner: S,
    conditioner: Conditioner,
    delayed: BTreeMap<(Instant, u64), (SocketAddr, Vec<u8>)>,
    delayed_bytes: usize,
    max_datagrams: usize,
    max_bytes: usize,
    next_seq: u64,
    stats: ConditionStats,
}
impl<S: DatagramSocket> ConditionedSocket<S> {
    pub fn new(inner: S, conditions: NetworkConditions) -> Result<Self> {
        Self::with_capacity(
            inner,
            conditions,
            DEFAULT_MAX_DELAYED_DATAGRAMS,
            DEFAULT_MAX_DELAYED_BYTES,
        )
    }
    pub fn with_capacity(
        inner: S,
        conditions: NetworkConditions,
        max_datagrams: usize,
        max_bytes: usize,
    ) -> Result<Self> {
        if max_datagrams == 0 || max_bytes == 0 {
            return Err(Error::Invalid(
                "conditioned socket capacity must be nonzero",
            ));
        }
        Ok(Self {
            inner,
            conditioner: Conditioner::new(conditions)?,
            delayed: BTreeMap::new(),
            delayed_bytes: 0,
            max_datagrams,
            max_bytes,
            next_seq: 0,
            stats: ConditionStats::default(),
        })
    }
    pub fn stats(&self) -> ConditionStats {
        self.stats
    }
    pub fn conditions(&self) -> &NetworkConditions {
        self.conditioner.conditions()
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }
    fn delay(&mut self, at: Instant, destination: SocketAddr, datagram: &[u8]) {
        if self.delayed.len() >= self.max_datagrams
            || datagram.len() > self.max_bytes.saturating_sub(self.delayed_bytes)
        {
            self.stats.dropped_overflow += 1;
            return;
        }
        self.delayed_bytes += datagram.len();
        self.delayed
            .insert((at, self.next_seq), (destination, datagram.to_vec()));
        self.next_seq += 1;
    }
    /// Writes every due datagram; one the inner socket refuses with `WouldBlock` stays queued.
    fn release_due(&mut self) {
        let now = Instant::now();
        while let Some(entry) = self.delayed.first_entry() {
            if entry.key().0 > now {
                break;
            }
            let (destination, datagram) = entry.get();
            match self.inner.send_to(datagram, *destination) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                _ => {
                    let (_, datagram) = entry.remove();
                    self.delayed_bytes -= datagram.len();
                    self.stats.delivered += 1;
                }
            }
        }
    }
}
impl<S: DatagramSocket> DatagramSocket for ConditionedSocket<S> {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        self.stats.offered += 1;
        match self
            .conditioner
            .schedule(Instant::now(), datagram.len(), true)
        {
            Fate::Dropped => self.stats.dropped_loss += 1,
            Fate::Deliver(at) => self.delay(at, destination, datagram),
            Fate::Duplicate(first, second) => {
                self.stats.duplicated += 1;
                self.delay(first, destination, datagram);
                self.delay(second, destination, datagram);
            }
        }
        self.release_due();
        Ok(datagram.len())
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.release_due();
        self.inner.recv_from(buffer)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}
