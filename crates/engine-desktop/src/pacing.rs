//! Frame pacing for the desktop runner: a fixed-step accumulator.
//!
//! The simulation always advances in whole [`FIXED_DELTA`](crate::FIXED_DELTA)
//! ticks. Presentation happens once per redraw at whatever rate the display
//! delivers; [`FramePacer`] turns the real time since the previous redraw into
//! "run this many ticks, then draw `alpha` of the way from the previous tick to
//! the current one". It is pure (no clock, no window), so it is tested with
//! synthetic frame durations.
//!
//! Two limits keep a slow or stalled frame from snowballing into a spiral of
//! ever more catch-up work:
//!
//! * a frame longer than [`MAX_FRAME_TIME`] is treated as that long (a window
//!   drag, a debugger pause or a suspended laptop must not fast-forward the
//!   game), and
//! * at most [`MAX_TICKS_PER_FRAME`] ticks run in one redraw; backlog beyond
//!   that is dropped, not carried (the game runs slower than real time rather
//!   than falling further behind).

/// Longest real time one redraw may add to the accumulator, in seconds.
pub const MAX_FRAME_TIME: f64 = 0.25;
/// Most simulation ticks one redraw may run.
pub const MAX_TICKS_PER_FRAME: u32 = 5;

/// Float slack, in seconds, so a frame of exactly N ticks runs N ticks rather
/// than N-1 plus an alpha of `1 - 1e-16`.
const EPSILON: f64 = 1e-9;

/// What one redraw should do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStep {
    /// Fixed ticks to run before drawing.
    pub ticks: u32,
    /// How far, in `[0, 1]`, the drawn instant lies from the previous tick (0)
    /// to the current tick (1) after running `ticks`.
    pub alpha: f64,
    /// Simulated seconds discarded because the tick cap was hit.
    pub dropped: f64,
}

/// Accumulates real time and hands out whole fixed ticks.
#[derive(Clone, Debug)]
pub struct FramePacer {
    dt: f64,
    max_ticks: u32,
    max_frame: f64,
    accumulator: f64,
}

impl FramePacer {
    /// A pacer for `dt`-second ticks with the default limits.
    pub fn new(dt: f64) -> Self {
        Self::with_limits(dt, MAX_TICKS_PER_FRAME, MAX_FRAME_TIME)
    }

    /// # Panics
    ///
    /// If `dt` or `max_frame` is not finite and positive, or `max_ticks` is zero.
    pub fn with_limits(dt: f64, max_ticks: u32, max_frame: f64) -> Self {
        assert!(dt.is_finite() && dt > 0.0, "tick length must be positive");
        assert!(
            max_frame.is_finite() && max_frame > 0.0,
            "frame time limit must be positive"
        );
        assert!(max_ticks > 0, "tick limit must be positive");
        Self {
            dt,
            max_ticks,
            max_frame,
            accumulator: 0.0,
        }
    }

    pub fn dt(&self) -> f64 {
        self.dt
    }

    /// Time banked toward the next tick, in `[0, dt)`.
    pub fn accumulator(&self) -> f64 {
        self.accumulator
    }

    /// Seconds until the next tick is due if no further time were banked.
    pub fn until_next_tick(&self) -> f64 {
        (self.dt - self.accumulator).max(0.0)
    }

    /// Forget banked time (after a pause the caller does not want simulated).
    pub fn reset(&mut self) {
        self.accumulator = 0.0;
    }

    /// Bank `frame_time` real seconds and decide this redraw's work. A
    /// non-finite or negative duration counts as zero.
    pub fn advance(&mut self, frame_time: f64) -> FrameStep {
        let frame_time = if frame_time.is_finite() && frame_time > 0.0 {
            frame_time.min(self.max_frame)
        } else {
            0.0
        };
        self.accumulator += frame_time;
        let mut ticks = 0;
        while ticks < self.max_ticks && self.accumulator + EPSILON >= self.dt {
            self.accumulator -= self.dt;
            ticks += 1;
        }
        self.accumulator = self.accumulator.max(0.0);
        let mut dropped = 0.0;
        if self.accumulator + EPSILON >= self.dt {
            // The cap left whole ticks of backlog: discard them and keep the
            // sub-tick phase.
            let whole = ((self.accumulator + EPSILON) / self.dt).floor();
            dropped = whole * self.dt;
            self.accumulator = (self.accumulator - dropped).max(0.0);
        }
        FrameStep {
            ticks,
            alpha: (self.accumulator / self.dt).clamp(0.0, 1.0),
            dropped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 60.0;

    fn pacer() -> FramePacer {
        FramePacer::new(DT)
    }

    fn assert_close(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "{left} != {right}");
    }

    #[test]
    fn frames_of_exactly_one_tick_run_one_tick_with_alpha_zero() {
        let mut pacer = pacer();
        for _ in 0..600 {
            let step = pacer.advance(DT);
            assert_eq!(step.ticks, 1);
            assert_close(step.alpha, 0.0);
            assert_eq!(step.dropped, 0.0);
        }
    }

    #[test]
    fn exact_multiples_run_that_many_ticks_with_nothing_left_over() {
        for multiple in 1..=MAX_TICKS_PER_FRAME {
            let mut pacer = pacer();
            let step = pacer.advance(DT * multiple as f64);
            assert_eq!(step.ticks, multiple, "{multiple} ticks");
            assert_close(step.alpha, 0.0);
            assert_eq!(step.dropped, 0.0);
            assert_close(pacer.accumulator(), 0.0);
        }
    }

    #[test]
    fn fractional_time_accumulates_across_frames() {
        let mut pacer = pacer();
        // A quarter tick per frame: a tick on every fourth frame, alpha ramps.
        let mut ticks = Vec::new();
        let mut alphas = Vec::new();
        for _ in 0..8 {
            let step = pacer.advance(DT / 4.0);
            ticks.push(step.ticks);
            alphas.push(step.alpha);
        }
        assert_eq!(ticks, [0, 0, 0, 1, 0, 0, 0, 1]);
        for (got, want) in alphas
            .iter()
            .zip([0.25, 0.5, 0.75, 0.0, 0.25, 0.5, 0.75, 0.0])
        {
            assert_close(*got, want);
        }
    }

    #[test]
    fn a_144_hz_display_runs_sixty_ticks_per_second_with_alpha_always_in_range() {
        let mut pacer = pacer();
        let frame = 1.0 / 144.0;
        let mut ticks = 0;
        for _ in 0..1440 {
            let step = pacer.advance(frame);
            assert!(step.ticks <= 1, "no catch-up at 144 Hz");
            assert!((0.0..1.0).contains(&step.alpha), "alpha {}", step.alpha);
            ticks += step.ticks;
        }
        // Ten seconds of frames: 600 ticks, give or take the banked remainder.
        assert!((599..=600).contains(&ticks), "{ticks} ticks");
    }

    #[test]
    fn a_slow_display_catches_up_with_multiple_ticks_per_frame() {
        let mut pacer = pacer();
        let frame = 1.0 / 30.0;
        let mut ticks = 0;
        for _ in 0..300 {
            let step = pacer.advance(frame);
            assert!((1..=3).contains(&step.ticks), "{} ticks", step.ticks);
            ticks += step.ticks;
        }
        assert!((599..=600).contains(&ticks), "{ticks} ticks");
    }

    #[test]
    fn zero_length_frames_run_no_ticks_and_keep_alpha() {
        let mut pacer = pacer();
        pacer.advance(DT * 0.4);
        let before = pacer.accumulator();
        for _ in 0..10 {
            let step = pacer.advance(0.0);
            assert_eq!(step.ticks, 0);
            assert_close(step.alpha, 0.4);
            assert_eq!(step.dropped, 0.0);
        }
        assert_eq!(pacer.accumulator(), before);
    }

    #[test]
    fn invalid_durations_count_as_zero() {
        for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut pacer = pacer();
            pacer.advance(DT * 0.5);
            let step = pacer.advance(bad);
            assert_eq!(step.ticks, 0, "{bad}");
            assert_close(step.alpha, 0.5);
        }
    }

    #[test]
    fn catch_up_is_capped_and_the_excess_is_dropped_not_banked() {
        let mut pacer = pacer();
        // 8 ticks' worth of time in one frame; only 5 may run.
        let step = pacer.advance(DT * 8.0);
        assert_eq!(step.ticks, MAX_TICKS_PER_FRAME);
        assert_close(step.dropped, DT * 3.0);
        assert_close(step.alpha, 0.0);
        assert_close(pacer.accumulator(), 0.0);
        // The next normal frame is normal: no debt is repaid.
        let next = pacer.advance(DT);
        assert_eq!(next.ticks, 1);
        assert_eq!(next.dropped, 0.0);
    }

    #[test]
    fn the_dropped_backlog_keeps_the_sub_tick_phase() {
        let mut pacer = pacer();
        let step = pacer.advance(DT * 7.25);
        assert_eq!(step.ticks, MAX_TICKS_PER_FRAME);
        assert_close(step.dropped, DT * 2.0);
        assert_close(step.alpha, 0.25);
    }

    #[test]
    fn sustained_overload_does_not_spiral() {
        // Every frame takes 100 ms (10 fps) while a tick is 16.7 ms: 6 ticks of
        // demand per frame, capped at 5. The accumulator never grows.
        let mut pacer = pacer();
        for _ in 0..1000 {
            let step = pacer.advance(0.1);
            assert!(step.ticks <= MAX_TICKS_PER_FRAME);
            assert!(pacer.accumulator() < DT);
        }
    }

    #[test]
    fn a_huge_stall_is_clamped_to_the_maximum_frame_time() {
        for stall in [10.0, 3600.0, f64::MAX] {
            let mut pacer = pacer();
            let step = pacer.advance(stall);
            assert_eq!(step.ticks, MAX_TICKS_PER_FRAME, "{stall}");
            // 250 ms is 15 ticks; 5 run, 10 are dropped.
            assert_close(
                step.dropped,
                MAX_FRAME_TIME - DT * 5.0 - pacer.accumulator(),
            );
            assert!(pacer.accumulator() < DT);
            assert!((0.0..1.0).contains(&step.alpha));
        }
    }

    #[test]
    fn a_stall_just_inside_the_cap_runs_every_tick() {
        let mut pacer = pacer();
        let step = pacer.advance(DT * 5.5);
        assert_eq!(step.ticks, 5);
        assert_eq!(step.dropped, 0.0);
        assert_close(step.alpha, 0.5);
    }

    #[test]
    fn custom_limits_apply() {
        let mut pacer = FramePacer::with_limits(0.1, 2, 0.3);
        let step = pacer.advance(10.0);
        assert_eq!(step.ticks, 2);
        assert_close(step.dropped, 0.3 - 0.2);
        assert_close(pacer.dt(), 0.1);
    }

    #[test]
    fn until_next_tick_and_reset_follow_the_accumulator() {
        let mut pacer = pacer();
        assert_close(pacer.until_next_tick(), DT);
        pacer.advance(DT * 0.75);
        assert_close(pacer.until_next_tick(), DT * 0.25);
        pacer.reset();
        assert_eq!(pacer.accumulator(), 0.0);
        assert_close(pacer.until_next_tick(), DT);
    }

    #[test]
    #[should_panic(expected = "tick length")]
    fn a_nonpositive_tick_length_is_rejected() {
        FramePacer::new(0.0);
    }
}
