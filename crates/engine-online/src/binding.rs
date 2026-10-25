//! Records which TLS leaf certificate each transport-admitted peer proved possession of, so
//! tokens can be bound to the live connection.
//!
//! The transports run a native `authorize(PeerId, &[CertificateDer]) -> bool` policy exactly
//! once per connection, after TLS 1.3 client-certificate verification and before `Connected`.
//! [`CertificateBinding::authorizer`] wraps your policy, remembers the SHA-256 of the leaf
//! certificate under the assigned peer ID, and the service claims it when `Connected` arrives.
use crate::token::{CertFingerprint, fingerprint};
use engine_network::{CertificateDer, PeerId};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct Entry {
    fingerprint: CertFingerprint,
    recorded: Instant,
}
struct Inner {
    entries: BTreeMap<PeerId, Entry>,
    max_entries: usize,
    ttl: Duration,
}

/// Cheap-to-clone handle shared between the transport's `authorize` callback and the server.
/// Bounded: unclaimed entries (handshakes that never reached `Connected`) expire after `ttl`,
/// and when `max_entries` live entries exist further peers are refused (fail closed).
#[derive(Clone)]
pub struct CertificateBinding {
    inner: Arc<Mutex<Inner>>,
}
impl CertificateBinding {
    pub fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: BTreeMap::new(),
                max_entries: max_entries.max(1),
                ttl,
            })),
        }
    }
    /// Wraps `policy` (called first; the leaf is recorded only when it admits the peer) in the
    /// callback type `QuicServerSecurity::new` / `ServerSecurity::new` expect. A chain without a
    /// leaf, a poisoned lock or a full table all reject the peer.
    pub fn authorizer(
        &self,
        policy: impl Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> impl Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static {
        let binding = self.clone();
        move |peer, chain| {
            let Some(leaf) = chain.first() else {
                return false;
            };
            policy(peer, chain) && binding.record(peer, fingerprint(leaf.as_ref()))
        }
    }
    /// Policy that admits every peer whose chain TLS already verified against the trust roots.
    pub fn authorize_verified(
        &self,
    ) -> impl Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static {
        self.authorizer(|_, _| true)
    }
    fn record(&self, peer: PeerId, fingerprint: CertFingerprint) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let now = Instant::now();
        let ttl = inner.ttl;
        inner
            .entries
            .retain(|_, entry| now.duration_since(entry.recorded) < ttl);
        if inner.entries.len() >= inner.max_entries {
            return false;
        }
        inner.entries.insert(
            peer,
            Entry {
                fingerprint,
                recorded: now,
            },
        );
        true
    }
    /// Claims (removes) the fingerprint recorded for `peer`; `None` if it never passed
    /// `authorize`, expired, or was already claimed.
    pub fn take(&self, peer: PeerId) -> Option<CertFingerprint> {
        self.inner
            .lock()
            .ok()?
            .entries
            .remove(&peer)
            .map(|entry| entry.fingerprint)
    }
    pub fn forget(&self, peer: PeerId) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.entries.remove(&peer);
        }
    }
    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.entries.len()).unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(bytes: &[u8]) -> CertificateDer<'static> {
        CertificateDer::from(bytes.to_vec())
    }

    #[test]
    fn records_the_leaf_only_after_policy_admits_and_is_claimed_once() {
        let binding = CertificateBinding::new(4, Duration::from_secs(30));
        let authorize = binding.authorizer(|peer, _| peer != 2);
        assert!(authorize(1, &[cert(b"leaf"), cert(b"root")]));
        assert!(!authorize(2, &[cert(b"leaf")]));
        assert!(!authorize(3, &[]));
        assert_eq!(binding.len(), 1);
        assert_eq!(binding.take(2), None);
        assert_eq!(binding.take(1), Some(fingerprint(b"leaf")));
        assert_eq!(binding.take(1), None);
    }

    #[test]
    fn table_is_bounded_and_stale_entries_expire() {
        let binding = CertificateBinding::new(2, Duration::from_millis(30));
        let authorize = binding.authorize_verified();
        assert!(authorize(1, &[cert(b"a")]) && authorize(2, &[cert(b"b")]));
        assert!(!authorize(3, &[cert(b"c")]), "full table fails closed");
        std::thread::sleep(Duration::from_millis(60));
        assert!(authorize(3, &[cert(b"c")]));
        assert_eq!(binding.len(), 1);
        binding.forget(3);
        assert!(binding.is_empty());
    }
}
