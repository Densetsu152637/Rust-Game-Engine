//! Token buckets and a bounded per-IP limiter.
use std::{
    collections::HashMap,
    net::IpAddr,
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

/// Per-source-IP request limiter with a hard bound on tracked sources. When the table is
/// full it forgets idle sources at most once per second; if none are idle, new sources are
/// refused (fail closed) rather than evicting active ones.
pub(crate) struct SourceLimiter {
    buckets: HashMap<IpAddr, TokenBucket>,
    max_sources: usize,
    rate: f64,
    burst: f64,
    next_sweep: Instant,
}
impl SourceLimiter {
    pub(crate) fn new(now: Instant, max_sources: usize, rate: f64, burst: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            max_sources,
            rate,
            burst,
            next_sweep: now,
        }
    }
    pub(crate) fn allow(&mut self, now: Instant, ip: IpAddr) -> bool {
        if let Some(bucket) = self.buckets.get_mut(&ip) {
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
        self.buckets.insert(ip, bucket);
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
