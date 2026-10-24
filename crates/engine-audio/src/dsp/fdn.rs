//! Parametric late reverb: an 8-line feedback delay network (section 5.4.6).
//!
//! * Householder feedback `A = I - (2/N) 1 1^T` (orthogonal, lossless).
//! * Line lengths are distinct primes spread from 0.63x to 1.35x the mean free
//!   time (`mean_free_path_m / c`), so they are mutually prime.
//! * Each line carries a [`BandFilter`] whose centre gains are
//!   `g_b = 10^(-3 (L + tau_b) / (RT60_b fs))`, where `tau_b` is the filter's own
//!   group delay at the centre, so the loop time includes the filter.
//! * Input passes a predelay and an EQ that sets the per-band energy of the
//!   impulse response to `level_b` (relative to a unit impulse, i.e. the
//!   direct sound at 1 m): for unit-norm input and output vectors the output
//!   energy is about `fs RT60 / (13.82 sum L)`, which the EQ inverts.
//! * Two orthogonal Hadamard rows give the left and right outputs.
//! * Gains and the EQ ramp over about 50 ms. A changed line length switches to
//!   the second (idle) network: input cross-fades over 50 ms and the old
//!   network rings out on its own, so the tail is never cut. If the second
//!   network is still ringing, the length change waits.

use super::{
    band::{BandDesign, BandFilter},
    delay::DelayLine,
    ramp::{Ramp, Ramp4},
};
use crate::acoustic::{BANDS, ReverbParams};

pub(crate) const LINES: usize = 8;
const SPREAD: [f64; LINES] = [0.63, 0.70, 0.78, 0.87, 0.97, 1.08, 1.21, 1.35];
const MFP_MIN_M: f64 = 1.0;
const MFP_MAX_M: f64 = 30.0;
const DEFAULT_MFP_M: f64 = 8.0;
const RT60_MIN_S: f32 = 0.05;
const RT60_MAX_S: f32 = 30.0;
const MAX_PREDELAY_S: f64 = 0.5;
const LENGTH_HYSTERESIS: f64 = 0.1;
/// ln(10^6): energy decay exponent over one RT60.
const DECAY_60DB: f64 = 13.815_510_557_964_274;
const S: f32 = 0.353_553_39; // 1 / sqrt(8)
const IN: [f32; LINES] = [S, -S, -S, S, -S, S, S, -S];
const OUT: [[f32; LINES]; 2] = [[S, -S, S, -S, S, -S, S, -S], [S, S, -S, -S, S, S, -S, -S]];

fn prime_at_least(mut n: usize) -> usize {
    let is_prime = |n: usize| {
        n >= 2
            && (2..)
                .take_while(|d| d * d <= n)
                .all(|d| !n.is_multiple_of(d))
    };
    while !is_prime(n) {
        n += 1;
    }
    n
}

fn lengths_for(mfp_m: f64, speed: f64, rate: u32) -> [usize; LINES] {
    let base = mfp_m / speed * f64::from(rate);
    let mut out = [0usize; LINES];
    let mut last = 0;
    for i in 0..LINES {
        let l = prime_at_least(((base * SPREAD[i]).round() as usize).max(31).max(last + 1));
        out[i] = l;
        last = l;
    }
    out
}

struct Network {
    lines: [DelayLine; LINES],
    lengths: [usize; LINES],
    filters: [BandFilter; LINES],
    gains: [Ramp4; LINES],
    feed: Ramp,
    active: bool,
    quiet: u64,
    tail: u64,
}

impl Network {
    fn new(capacity: usize) -> Self {
        Self {
            lines: std::array::from_fn(|_| DelayLine::new(capacity)),
            lengths: [31; LINES],
            filters: [BandFilter::default(); LINES],
            gains: [Ramp4::new([0.0; BANDS]); LINES],
            feed: Ramp::new(0.0),
            active: false,
            quiet: 0,
            tail: 0,
        }
    }
    fn clear(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.filters = [BandFilter::default(); LINES];
        self.active = false;
        self.quiet = 0;
    }
    fn set_decay(&mut self, design: &BandDesign, rt60: &[f32; BANDS], rate: u32, frames: usize) {
        let gd = design.group_delay();
        for i in 0..LINES {
            let g: [f32; BANDS] = std::array::from_fn(|b| {
                let loop_len = self.lengths[i] as f64 + gd[b];
                10f64.powf(-3.0 * loop_len / (f64::from(rt60[b]) * f64::from(rate))) as f32
            });
            self.gains[i].set(design.lanes(g), frames);
        }
        let longest = rt60.iter().copied().fold(0.0f32, f32::max);
        self.tail =
            (f64::from(longest) * 1.5 * f64::from(rate)) as u64 + self.lengths[LINES - 1] as u64;
    }
    /// Sum of loop lengths per band, including the filter group delay.
    fn loop_sum(&self, design: &BandDesign) -> [f64; BANDS] {
        let gd = design.group_delay();
        std::array::from_fn(|b| self.lengths.iter().map(|l| *l as f64 + gd[b]).sum())
    }
    #[inline(always)]
    fn tick(&mut self, design: &BandDesign, input: f32) -> [f32; 2] {
        let x = input * self.feed.next();
        let mut s = [0.0f32; LINES];
        for i in 0..LINES {
            let raw = self.lines[i].read(self.lengths[i] - 1);
            s[i] = self.filters[i].tick(design, raw, self.gains[i].next());
        }
        let mut y = [0.0f32; 2];
        let mut sum = 0.0f32;
        for i in 0..LINES {
            y[0] += OUT[0][i] * s[i];
            y[1] += OUT[1][i] * s[i];
            sum += s[i];
        }
        let reflect = sum * (2.0 / LINES as f32);
        for i in 0..LINES {
            self.lines[i].push(s[i] - reflect + IN[i] * x);
        }
        if x != 0.0 {
            self.quiet = 0;
        }
        y
    }
}

/// Listener-centric late field renderer.
pub(crate) struct LateReverb {
    rate: u32,
    ramp: usize,
    capacity: usize,
    nets: [Network; 2],
    fed: usize,
    predelay_line: DelayLine,
    predelay: Ramp,
    eq: BandFilter,
    eq_gains: Ramp4,
    rt60: [f32; BANDS],
    level: [f32; BANDS],
    mfp_m: f64,
    on: bool,
    pending: bool,
}

impl LateReverb {
    pub(crate) fn new(rate: u32) -> Self {
        let max_line =
            (MFP_MAX_M / 300.0 * SPREAD[LINES - 1] * f64::from(rate)).ceil() as usize + 64;
        Self {
            rate,
            ramp: (0.05 * f64::from(rate)) as usize,
            capacity: max_line,
            nets: [Network::new(max_line), Network::new(max_line)],
            fed: 0,
            predelay_line: DelayLine::new((MAX_PREDELAY_S * f64::from(rate)) as usize + 2),
            predelay: Ramp::new(0.0),
            eq: BandFilter::default(),
            eq_gains: Ramp4::new([0.0; BANDS]),
            rt60: [1.0; BANDS],
            level: [0.0; BANDS],
            mfp_m: DEFAULT_MFP_M,
            on: false,
            pending: false,
        }
    }

    /// True while nothing rings and nothing is fed.
    pub(crate) fn idle(&self) -> bool {
        !self.nets.iter().any(|n| n.active)
    }

    /// Applies new parameters (`on` = reverb enabled and parametric rendering selected).
    pub(crate) fn set_params(
        &mut self,
        design: &BandDesign,
        params: &ReverbParams,
        on: bool,
        speed: f32,
    ) {
        self.on = on && params.enabled;
        if !self.on {
            for net in &mut self.nets {
                net.feed.set(0.0, self.ramp);
            }
            self.pending = false;
            return;
        }
        self.rt60 = params.rt60_s.map(|t| {
            if t.is_finite() {
                t.clamp(RT60_MIN_S, RT60_MAX_S)
            } else {
                1.0
            }
        });
        self.level = params.level.map(|l| {
            if l.is_finite() {
                l.clamp(0.0, 16.0)
            } else {
                0.0
            }
        });
        let mfp = f64::from(params.mean_free_path_m);
        self.mfp_m = if mfp.is_finite() && mfp > 0.0 {
            mfp.clamp(MFP_MIN_M, MFP_MAX_M)
        } else {
            DEFAULT_MFP_M
        };
        let pre = f64::from(params.predelay_s);
        let pre = if pre.is_finite() {
            pre.clamp(0.0, MAX_PREDELAY_S)
        } else {
            0.0
        };
        let target = (pre * f64::from(self.rate)) as f32;
        let slew = ((target - self.predelay.value()).abs() / 0.05).ceil() as usize;
        self.predelay.set(target, slew);
        let wanted = lengths_for(self.mfp_m, f64::from(speed), self.rate);
        let current = &self.nets[self.fed];
        let change =
            (wanted[0] as f64 - current.lengths[0] as f64).abs() / current.lengths[0] as f64;
        self.pending = !current.active || change > LENGTH_HYSTERESIS;
        let cold = self.idle();
        if self.pending {
            self.switch(design, speed);
        }
        self.update(design);
        if cold {
            // Nothing is audible yet, so parameters take effect immediately.
            let eq = self.eq_gains.target();
            self.eq_gains.jump(eq);
            self.predelay.jump(target);
        }
    }

    /// Moves input to the other network with new lengths if it is free.
    fn switch(&mut self, design: &BandDesign, speed: f32) {
        let fed_active = self.nets[self.fed].active;
        let other = if fed_active { 1 - self.fed } else { self.fed };
        if self.nets[other].active && other != self.fed {
            return; // still ringing; retried from `process`.
        }
        let lengths =
            lengths_for(self.mfp_m, f64::from(speed), self.rate).map(|l| l.min(self.capacity));
        // `other` is idle, and idle networks are already cleared.
        let net = &mut self.nets[other];
        net.lengths = lengths;
        net.set_decay(design, &self.rt60, self.rate, 0);
        // A cold start feeds at once; a hand-over cross-fades the input.
        net.feed.jump(if fed_active { 0.0 } else { 1.0 });
        if fed_active {
            self.nets[self.fed].feed.set(0.0, self.ramp);
        }
        self.fed = other;
        self.pending = false;
    }

    fn update(&mut self, design: &BandDesign) {
        let ramp = self.ramp;
        for (i, net) in self.nets.iter_mut().enumerate() {
            if net.active || i == self.fed {
                // Idle networks take new values at once (nothing audible).
                let frames = if net.active { ramp } else { 0 };
                net.set_decay(design, &self.rt60, self.rate, frames);
            }
        }
        let fed = &mut self.nets[self.fed];
        fed.feed.set(1.0, ramp);
        let sums = fed.loop_sum(design);
        let eq: [f32; BANDS] = std::array::from_fn(|b| {
            let energy = f64::from(self.rate) * f64::from(self.rt60[b]) / (DECAY_60DB * sums[b]);
            (f64::from(self.level[b]) / energy).sqrt() as f32
        });
        self.eq_gains.set(design.lanes(eq), ramp);
    }

    /// Adds the late field for `input` (mono send) to `out`.
    pub(crate) fn process(
        &mut self,
        design: &BandDesign,
        speed: f32,
        input: &[f32],
        out: &mut [[f32; 2]],
    ) {
        let fed_input = self.on && input.iter().any(|x| *x != 0.0);
        if self.idle() && !fed_input {
            return;
        }
        if self.pending && self.on {
            self.switch(design, speed);
            self.update(design);
        }
        if fed_input {
            self.nets[self.fed].active = true;
        }
        for (x, y) in input.iter().zip(out.iter_mut()) {
            self.predelay_line.push(if self.on { *x } else { 0.0 });
            let d = self.predelay.next();
            let delayed = self.predelay_line.read_linear(f64::from(d));
            let shaped = self.eq.tick(design, delayed, self.eq_gains.next());
            for net in &mut self.nets {
                if net.active {
                    let o = net.tick(design, shaped);
                    y[0] += o[0];
                    y[1] += o[1];
                }
            }
        }
        self.eq.flush();
        for net in &mut self.nets {
            if net.active {
                net.quiet += input.len() as u64;
                net.filters.iter_mut().for_each(BandFilter::flush);
                if net.quiet > net.tail {
                    net.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_lengths_are_distinct_primes_around_the_mean_free_time() {
        let l = lengths_for(5.0, 343.0, 48_000);
        let base = 5.0 / 343.0 * 48_000.0;
        for w in l.windows(2) {
            assert!(w[0] < w[1]);
        }
        assert!(
            (l[0] as f64) >= base * 0.6 && (l[7] as f64) <= base * 1.4,
            "{l:?}"
        );
    }

    #[test]
    fn disabled_reverb_goes_idle() {
        let design = BandDesign::new(48_000);
        let mut r = LateReverb::new(48_000);
        let params = ReverbParams {
            enabled: true,
            rt60_s: [0.2; 4],
            level: [1.0; 4],
            predelay_s: 0.0,
            mean_free_path_m: 3.0,
        };
        r.set_params(&design, &params, true, 343.0);
        let mut out = vec![[0.0; 2]; 256];
        let mut input = vec![0.0; 256];
        input[0] = 1.0;
        r.process(&design, 343.0, &input, &mut out);
        assert!(!r.idle());
        input[0] = 0.0;
        for _ in 0..400 {
            r.process(&design, 343.0, &input, &mut out);
        }
        assert!(r.idle());
    }
}
