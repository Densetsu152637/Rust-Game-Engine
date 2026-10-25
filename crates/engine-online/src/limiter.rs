//! Integer-only rate limiters driven by a caller-supplied monotonic clock.

/// Token bucket measured in milli-tokens so refill is exact integer arithmetic.
#[derive(Clone, Debug)]
pub(crate) struct TokenBucket {
    capacity: u64,
    per_second: u64,
    tokens: u64,
    last_ms: u64,
}
impl TokenBucket {
    pub(crate) fn new(per_second: u32, burst: u32, now_ms: u64) -> Self {
        let capacity = u64::from(burst.max(1)) * 1000;
        Self {
            capacity,
            per_second: u64::from(per_second.max(1)),
            tokens: capacity,
            last_ms: now_ms,
        }
    }
    /// Takes one token; false means the caller is over its rate.
    pub(crate) fn allow(&mut self, now_ms: u64) -> bool {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        self.tokens = self.capacity.min(
            self.tokens
                .saturating_add(elapsed.saturating_mul(self.per_second)),
        );
        if self.tokens >= 1000 {
            self.tokens -= 1000;
            true
        } else {
            false
        }
    }
}

/// Counts failures inside a fixed window; used to throttle guessing attacks.
#[derive(Clone, Debug)]
pub(crate) struct FailureWindow {
    window_ms: u64,
    max: u32,
    start_ms: u64,
    count: u32,
}
impl FailureWindow {
    pub(crate) fn new(max: u32, window_ms: u64) -> Self {
        Self {
            window_ms: window_ms.max(1),
            max,
            start_ms: 0,
            count: 0,
        }
    }
    fn roll(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.start_ms) >= self.window_ms {
            self.start_ms = now_ms;
            self.count = 0;
        }
    }
    /// True while further attempts are still allowed in the current window.
    pub(crate) fn open(&mut self, now_ms: u64) -> bool {
        self.roll(now_ms);
        self.count < self.max
    }
    pub(crate) fn record_failure(&mut self, now_ms: u64) {
        self.roll(now_ms);
        self.count = self.count.saturating_add(1);
    }
    pub(crate) fn failures(&self) -> u32 {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_refills_exactly_and_bursts() {
        let mut bucket = TokenBucket::new(10, 3, 0);
        assert!(bucket.allow(0) && bucket.allow(0) && bucket.allow(0));
        assert!(!bucket.allow(0));
        assert!(!bucket.allow(99));
        assert!(bucket.allow(100));
        assert!(!bucket.allow(100));
        // A clock that goes backwards never refunds tokens.
        assert!(!bucket.allow(50));
    }

    #[test]
    fn failure_window_blocks_then_rolls() {
        let mut window = FailureWindow::new(2, 1000);
        assert!(window.open(10));
        window.record_failure(10);
        window.record_failure(20);
        assert!(!window.open(30));
        assert!(window.open(2000));
        assert_eq!(window.failures(), 0);
    }
}
