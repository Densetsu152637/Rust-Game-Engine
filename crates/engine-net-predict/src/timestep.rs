//! Fixed simulation step accumulator with bounded catch-up.
use crate::{PredictError, Result};
use std::{ops::Range, time::Duration};

/// Ticks due after one [`FixedTimestep::advance`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimestepTicks {
    pub first_tick: u64,
    pub count: u32,
    /// Ticks discarded because more than `max_ticks_per_update` were due (spiral-of-death cap).
    pub dropped: u64,
}
impl TimestepTicks {
    pub fn ticks(&self) -> Range<u64> {
        self.first_tick..self.first_tick + u64::from(self.count)
    }
}

/// Accumulates wall time in integer nanoseconds so tick boundaries are exact and reproducible.
#[derive(Clone, Debug)]
pub struct FixedTimestep {
    tick_nanos: u64,
    accumulator: u64,
    max_ticks_per_update: u32,
    next_tick: u64,
    dropped_ticks: u64,
}
impl FixedTimestep {
    /// `tick` must be within 100 µs..=1 s and `max_ticks_per_update` within 1..=1024.
    pub fn new(tick: Duration, max_ticks_per_update: u32, first_tick: u64) -> Result<Self> {
        if tick < Duration::from_micros(100) || tick > Duration::from_secs(1) {
            return Err(PredictError::Invalid("tick duration must be 100 us..=1 s"));
        }
        if max_ticks_per_update == 0 || max_ticks_per_update > 1024 {
            return Err(PredictError::Invalid(
                "max_ticks_per_update must be 1..=1024",
            ));
        }
        Ok(Self {
            tick_nanos: tick.as_nanos() as u64,
            accumulator: 0,
            max_ticks_per_update,
            next_tick: first_tick,
            dropped_ticks: 0,
        })
    }
    /// Convenience constructor; the tick is rounded down to whole nanoseconds.
    pub fn from_hz(hz: u32, max_ticks_per_update: u32, first_tick: u64) -> Result<Self> {
        if hz == 0 {
            return Err(PredictError::Invalid("tick rate must be positive"));
        }
        Self::new(
            Duration::from_nanos(1_000_000_000 / u64::from(hz)),
            max_ticks_per_update,
            first_tick,
        )
    }
    pub fn tick_duration(&self) -> Duration {
        Duration::from_nanos(self.tick_nanos)
    }
    /// Tick the next due step will simulate.
    pub fn next_tick(&self) -> u64 {
        self.next_tick
    }
    pub fn dropped_ticks(&self) -> u64 {
        self.dropped_ticks
    }
    /// Adds elapsed wall time and returns the ticks to simulate now, oldest first.
    pub fn advance(&mut self, elapsed: Duration) -> TimestepTicks {
        let add = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.accumulator = self.accumulator.saturating_add(add);
        let due = self.accumulator / self.tick_nanos;
        self.accumulator -= due * self.tick_nanos;
        let count = due.min(u64::from(self.max_ticks_per_update));
        let dropped = due - count;
        let first_tick = self.next_tick;
        self.next_tick = self.next_tick.saturating_add(count);
        self.dropped_ticks = self.dropped_ticks.saturating_add(dropped);
        TimestepTicks {
            first_tick,
            count: count as u32,
            dropped,
        }
    }
    /// Removes up to `ticks` whole ticks of accumulated time without simulating them, e.g. to
    /// follow a rollback [`crate::TimeSync`] wait recommendation. Returns the time removed.
    pub fn delay(&mut self, ticks: u32) -> Duration {
        let want = self.tick_nanos.saturating_mul(u64::from(ticks));
        let removed = want.min(self.accumulator);
        self.accumulator -= removed;
        Duration::from_nanos(removed)
    }
    /// Fraction of the next tick already elapsed, in `[0, 1)`; use it to interpolate rendering
    /// between the last two simulated states.
    pub fn alpha(&self) -> f32 {
        (self.accumulator as f64 / self.tick_nanos as f64) as f32
    }
}
