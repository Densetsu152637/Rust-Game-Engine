//! Token buckets and a bounded per-source limiter.
use std::{
    collections::HashMap,
    hash::Hash,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};

/// Classic token bucket; refills continuously at `rate` per second up to `capacity`.
#[derive(Clone, Debug)]
pub(crate) struct TokenBucket {
    tokens: f64,
    capacity: f64,
    rate: f64,
    last: Instant,
}
impl TokenBucket {
    pub(crate) fn new(now: Instant, rate: f64, capacity: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            rate,
            last: now,
        }
    }
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last = self.last.max(now);
    }
    pub(crate) fn try_take(&mut self, now: Instant, amount: f64) -> bool {
        self.refill(now);
        if self.tokens >= amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }
    /// True when the bucket would be full by `now`, so forgetting it changes nothing.
    fn is_idle(&self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens + elapsed * self.rate >= self.capacity
    }
}

/// A source identity a [`SourceLimiter`] can key on. `canonical` collapses addresses one
/// subscriber controls into one identity, so rotating through them buys nothing.
pub(crate) trait SourceKey: Copy + Eq + Hash {
    fn canonical(self) -> Self;
}
impl SourceKey for IpAddr {
    /// IPv4 keys on the address (/32). IPv6 keys on the /64, because an end site owns at least
    /// a whole /64 and can mint any address inside it; an IPv4-mapped IPv6 address is the IPv4
    /// address it wraps.
    fn canonical(self) -> Self {
        match self {
            Self::V4(_) => self,
            Self::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(v4),
                None => {
                    let mut octets = v6.octets();
                    octets[8..].fill(0);
                    Self::V6(Ipv6Addr::from(octets))
                }
            },
        }
    }
}
impl SourceKey for SocketAddr {
    /// The canonical IP plus the port (flow info and scope are not part of the identity).
    fn canonical(self) -> Self {
        Self::new(self.ip().canonical(), self.port())
    }
}

/// Per-source request limiter with a hard bound on tracked sources. When the table is
/// full it forgets idle sources at most once per second; if none are idle, new sources are
/// refused (fail closed) rather than evicting active ones.
pub(crate) struct SourceLimiter<K: SourceKey> {
    buckets: HashMap<K, TokenBucket>,
    max_sources: usize,
    rate: f64,
    burst: f64,
    next_sweep: Instant,
}
impl<K: SourceKey> SourceLimiter<K> {
    pub(crate) fn new(now: Instant, max_sources: usize, rate: f64, burst: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            max_sources,
            rate,
            burst,
            next_sweep: now,
        }
    }
    pub(crate) fn allow(&mut self, now: Instant, source: K) -> bool {
        let key = source.canonical();
        if let Some(bucket) = self.buckets.get_mut(&key) {
            return bucket.try_take(now, 1.0);
        }
        if self.buckets.len() >= self.max_sources {
            if now < self.next_sweep {
                return false;
            }
            self.next_sweep = now + Duration::from_secs(1);
            self.buckets.retain(|_, bucket| !bucket.is_idle(now));
            if self.buckets.len() >= self.max_sources {
                return false;
            }
        }
        let mut bucket = TokenBucket::new(now, self.rate, self.burst);
        let allowed = bucket.try_take(now, 1.0);
        self.buckets.insert(key, bucket);
        allowed
    }
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_refills_at_its_rate() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::new(t0, 10.0, 5.0);
        assert!((0..5).all(|_| bucket.try_take(t0, 1.0)));
        assert!(!bucket.try_take(t0, 1.0));
        assert!(bucket.try_take(t0 + Duration::from_millis(100), 1.0));
        assert!(!bucket.try_take(t0 + Duration::from_millis(100), 1.0));
    }

    /// Security finding L5: an IPv6 end user owns a whole /64, so keying on the /128 lets one
    /// host mint unlimited limiter identities.
    #[test]
    fn limiter_keys_ipv6_on_the_slash_64_and_ipv4_on_the_address() {
        let t0 = Instant::now();
        let mut limiter = SourceLimiter::new(t0, 1000, 1.0, 3.0);
        let v6 = |suffix: u16| IpAddr::from([0x2001, 0xdb8, 1, 2, 0, 0, 0, suffix]);
        let allowed = (0..50).filter(|&n| limiter.allow(t0, v6(n))).count();
        assert_eq!(allowed, 3, "one /64 shares one bucket");
        assert_eq!(limiter.tracked(), 1);
        let other_64 = IpAddr::from([0x2001, 0xdb8, 1, 3, 0, 0, 0, 1]);
        assert!(
            limiter.allow(t0, other_64),
            "a different /64 has its own bucket"
        );
        let v4 = |n| IpAddr::from([192, 0, 2, n]);
        assert!(limiter.allow(t0, v4(1)) && limiter.allow(t0, v4(2)));
        assert_eq!(limiter.tracked(), 4, "IPv4 stays per address");
        // An IPv4-mapped IPv6 address is the IPv4 address it wraps.
        let mapped = IpAddr::from(std::net::Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped());
        assert!(limiter.allow(t0, mapped));
        assert_eq!(limiter.tracked(), 4);
    }

    #[test]
    fn socket_address_keys_share_a_bucket_per_slash_64_and_port() {
        let t0 = Instant::now();
        let mut limiter: SourceLimiter<SocketAddr> = SourceLimiter::new(t0, 1000, 1.0, 2.0);
        let at = |suffix: u16, port: u16| {
            SocketAddr::new(IpAddr::from([0x2001, 0xdb8, 1, 2, 0, 0, 0, suffix]), port)
        };
        let allowed = (0..20).filter(|&n| limiter.allow(t0, at(n, 4000))).count();
        assert_eq!(allowed, 2, "rotating the interface id does not help");
        assert!(
            limiter.allow(t0, at(1, 4001)),
            "another port is another flow"
        );
        assert_eq!(limiter.tracked(), 2);
    }

    #[test]
    fn limiter_bounds_sources_and_fails_closed() {
        let t0 = Instant::now();
        let mut limiter = SourceLimiter::new(t0, 2, 1.0, 1.0);
        let ip = |n| IpAddr::from([10, 0, 0, n]);
        assert!(limiter.allow(t0, ip(1)));
        assert!(limiter.allow(t0, ip(2)));
        assert!(!limiter.allow(t0, ip(1)), "burst exhausted");
        assert!(!limiter.allow(t0, ip(3)), "table full of active sources");
        assert_eq!(limiter.tracked(), 2);
        let later = t0 + Duration::from_secs(5);
        assert!(limiter.allow(later, ip(3)), "idle sources forgotten");
        assert!(limiter.tracked() <= 2);
    }
}
