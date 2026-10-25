//! Four-band gain filter (`BandFilter` in the design, section 5.4.2).
//!
//! The signal is split by a Linkwitz-Riley (LR4) crossover tree at the band
//! edges 250 / 1000 / 4000 Hz: first at 1 kHz, then each half again, with the
//! other half's crossover allpass applied so all four band outputs share one
//! phase response. At every frequency each band output is therefore the common
//! allpass times a real, non-negative weight `w_j(f)`, and the weights sum to
//! one. The output is `sum_j g'_j * band_j`, so at a band centre `c_b` the
//! magnitude is exactly `sum_j W[b][j] g'_j`. The lane gains `g'` are obtained
//! from the requested gains through the precomputed inverse of `W`, which makes
//! the requested gain exact at the centres (up to rounding) even when adjacent
//! bands differ strongly. Gains are applied after all filtering, so a linear
//! gain ramp reaches the output unfiltered (click-free steps).
//!
//! Bands whose centre lies above 0.45 x the sample rate are not solved for, and
//! a crossover at or above 0.45 x the sample rate is removed (only possible for
//! the 4 kHz edge at 8 kHz output), leaving its upper band empty.

use super::{flush, ramp::Ramp4};
use crate::acoustic::{BAND_CENTERS_HZ, BAND_EDGES_HZ, BANDS};
use std::f64::consts::{FRAC_1_SQRT_2, PI};

#[derive(Clone, Copy, Debug)]
struct Coef {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coef {
    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
    const ZERO: Self = Self {
        b0: 0.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
    /// RBJ cookbook section with Q = 1/sqrt(2) (Butterworth) and a0 normalised.
    fn rbj(kind: Kind, fc: f64, fs: f64) -> Self {
        let w0 = 2.0 * PI * fc / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = match kind {
            Kind::Low => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0),
            Kind::High => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0),
            Kind::All => (1.0 - alpha, -2.0 * cos, 1.0 + alpha),
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }
    /// Complex response at `w` radians per sample.
    fn response(&self, w: f64) -> C64 {
        let z1 = C64::polar(-w);
        let z2 = C64::polar(-2.0 * w);
        let num = C64::real(self.b0) + z1.scale(self.b1) + z2.scale(self.b2);
        let den = C64::real(1.0) + z1.scale(self.a1) + z2.scale(self.a2);
        num.div(den)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Low,
    High,
    All,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct C64 {
    pub(crate) re: f64,
    pub(crate) im: f64,
}
impl C64 {
    fn real(re: f64) -> Self {
        Self { re, im: 0.0 }
    }
    fn polar(angle: f64) -> Self {
        Self {
            re: angle.cos(),
            im: angle.sin(),
        }
    }
    fn scale(self, s: f64) -> Self {
        Self {
            re: self.re * s,
            im: self.im * s,
        }
    }
    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn div(self, o: Self) -> Self {
        let d = o.re * o.re + o.im * o.im;
        Self {
            re: (self.re * o.re + self.im * o.im) / d,
            im: (self.im * o.re - self.re * o.im) / d,
        }
    }
    fn norm(self) -> f64 {
        self.re.hypot(self.im)
    }
    fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
}
impl std::ops::Add for C64 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}

/// `L` parallel biquad sections (structure-of-arrays coefficients).
#[derive(Clone, Copy, Debug)]
struct Lanes<const L: usize> {
    b0: [f32; L],
    b1: [f32; L],
    b2: [f32; L],
    a1: [f32; L],
    a2: [f32; L],
}
impl<const L: usize> Lanes<L> {
    fn new(coefs: [Coef; L]) -> Self {
        Self {
            b0: coefs.map(|c| c.b0 as f32),
            b1: coefs.map(|c| c.b1 as f32),
            b2: coefs.map(|c| c.b2 as f32),
            a1: coefs.map(|c| c.a1 as f32),
            a2: coefs.map(|c| c.a2 as f32),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct State<const L: usize> {
    s1: [f32; L],
    s2: [f32; L],
}
impl<const L: usize> Default for State<L> {
    fn default() -> Self {
        Self {
            s1: [0.0; L],
            s2: [0.0; L],
        }
    }
}
impl<const L: usize> State<L> {
    /// Transposed direct form II, one sample per lane.
    #[inline(always)]
    fn tick(&mut self, c: &Lanes<L>, x: [f32; L]) -> [f32; L] {
        let mut y = [0.0; L];
        for i in 0..L {
            y[i] = c.b0[i] * x[i] + self.s1[i];
            self.s1[i] = c.b1[i] * x[i] - c.a1[i] * y[i] + self.s2[i];
            self.s2[i] = c.b2[i] * x[i] - c.a2[i] * y[i];
        }
        y
    }
    fn flush(&mut self) {
        self.s1.iter_mut().chain(self.s2.iter_mut()).for_each(flush);
    }
}

/// Sample-rate dependent coefficients and gain compensation, shared by all
/// filter instances of one mixer.
#[derive(Clone, Debug)]
pub(crate) struct BandDesign {
    a: Lanes<2>,
    b: Lanes<4>,
    c: Lanes<4>,
    /// `lane = comp * requested`.
    comp: [[f32; BANDS]; BANDS],
    /// Group delay of the common allpass at each band centre, in samples.
    group_delay: [f64; BANDS],
    paths: [[Coef; 5]; BANDS],
}

impl BandDesign {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let fs = f64::from(sample_rate);
        let edge = |i: usize| f64::from(BAND_EDGES_HZ[i]);
        let usable = |f: f64| f < 0.45 * fs;
        let lp = |f| Coef::rbj(Kind::Low, f, fs);
        let hp = |f| Coef::rbj(Kind::High, f, fs);
        let ap = |f| Coef::rbj(Kind::All, f, fs);
        let (lp_hi, hp_hi, ap_hi) = if usable(edge(2)) {
            (lp(edge(2)), hp(edge(2)), ap(edge(2)))
        } else {
            (Coef::IDENTITY, Coef::ZERO, Coef::IDENTITY)
        };
        let a = [lp(edge(1)), hp(edge(1))];
        let b = [lp(edge(0)), hp(edge(0)), lp_hi, hp_hi];
        let c = [ap_hi, ap_hi, ap(edge(0)), ap(edge(0))];
        // Each band path: stage A twice, stage B twice, stage C once.
        let paths: [[Coef; 5]; BANDS] =
            std::array::from_fn(|j| [a[j / 2], a[j / 2], b[j], b[j], c[j]]);
        let mut design = Self {
            a: Lanes::new(a),
            b: Lanes::new(b),
            c: Lanes::new(c),
            comp: [[0.0; BANDS]; BANDS],
            group_delay: [0.0; BANDS],
            paths,
        };
        let w = |f: f64| 2.0 * PI * f / fs;
        let centres = BAND_CENTERS_HZ.map(f64::from);
        let weights: [[f64; BANDS]; BANDS] =
            std::array::from_fn(|b| std::array::from_fn(|j| design.path(j, w(centres[b])).norm()));
        let included: [bool; BANDS] = centres.map(usable);
        design.comp = compensation(&weights, &included);
        design.group_delay = std::array::from_fn(|b| {
            let d = 1e-4;
            let phase = |x: f64| design.total(x).arg();
            let mut diff = phase(w(centres[b]) + d) - phase(w(centres[b]) - d);
            while diff > PI {
                diff -= 2.0 * PI;
            }
            while diff < -PI {
                diff += 2.0 * PI;
            }
            if usable(centres[b]) {
                -diff / (2.0 * d)
            } else {
                0.0
            }
        });
        design
    }

    fn path(&self, band: usize, w: f64) -> C64 {
        self.paths[band]
            .iter()
            .fold(C64::real(1.0), |acc, c| acc.mul(c.response(w)))
    }
    /// Response with unit lane gains (the shared allpass).
    fn total(&self, w: f64) -> C64 {
        (0..BANDS).fold(C64::real(0.0), |acc, j| acc + self.path(j, w))
    }

    /// Lane gains that realise `requested` at the band centres.
    pub(crate) fn lanes(&self, requested: [f32; BANDS]) -> [f32; BANDS] {
        std::array::from_fn(|i| (0..BANDS).map(|k| self.comp[i][k] * requested[k]).sum())
    }

    /// Group delay of the filter at each band centre, in samples.
    pub(crate) fn group_delay(&self) -> [f64; BANDS] {
        self.group_delay
    }

    /// Magnitude response for `lanes` at `freq` Hz.
    #[cfg(test)]
    pub(crate) fn magnitude(&self, lanes: [f32; BANDS], freq: f64, sample_rate: u32) -> f64 {
        let w = 2.0 * PI * freq / f64::from(sample_rate);
        (0..BANDS)
            .fold(C64::real(0.0), |acc, j| {
                acc + self.path(j, w).scale(f64::from(lanes[j]))
            })
            .norm()
    }
}

/// Solves `W g' = g` for the included bands; excluded bands pass their
/// requested gain through and are moved to the right-hand side.
fn compensation(
    weights: &[[f64; BANDS]; BANDS],
    included: &[bool; BANDS],
) -> [[f32; BANDS]; BANDS] {
    let idx: Vec<usize> = (0..BANDS).filter(|&b| included[b]).collect();
    let mut comp = [[0.0f32; BANDS]; BANDS];
    for k in 0..BANDS {
        let requested: [f64; BANDS] = std::array::from_fn(|b| f64::from(u8::from(b == k)));
        let mut lanes = [0.0f64; BANDS];
        for j in 0..BANDS {
            if !included[j] {
                lanes[j] = requested[j];
            }
        }
        let n = idx.len();
        let mut m = vec![vec![0.0f64; n + 1]; n];
        for (r, &b) in idx.iter().enumerate() {
            for (c, &j) in idx.iter().enumerate() {
                m[r][c] = weights[b][j];
            }
            let excluded: f64 = (0..BANDS)
                .filter(|j| !included[*j])
                .map(|j| weights[b][j] * lanes[j])
                .sum();
            m[r][n] = requested[b] - excluded;
        }
        let solved = solve(&mut m);
        for (c, &j) in idx.iter().enumerate() {
            lanes[j] = solved[c];
        }
        for i in 0..BANDS {
            comp[i][k] = lanes[i] as f32;
        }
    }
    comp
}

/// Gaussian elimination with partial pivoting on an augmented matrix.
fn solve(m: &mut [Vec<f64>]) -> Vec<f64> {
    let n = m.len();
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|a, b| m[*a][col].abs().total_cmp(&m[*b][col].abs()))
            .unwrap_or(col);
        m.swap(col, pivot);
        for row in col + 1..n {
            let f = m[row][col] / m[col][col];
            for c in col..=n {
                m[row][c] -= f * m[col][c];
            }
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|c| m[row][c] * x[c]).sum();
        x[row] = (m[row][n] - s) / m[row][row];
    }
    x
}

/// Per-instance filter state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BandFilter {
    a: [State<2>; 2],
    b: [State<4>; 2],
    c: State<4>,
}

impl BandFilter {
    /// Filters one sample with fixed lane gains.
    #[inline(always)]
    pub(crate) fn tick(&mut self, d: &BandDesign, x: f32, lanes: [f32; BANDS]) -> f32 {
        let s = self.a[0].tick(&d.a, [x, x]);
        let s = self.a[1].tick(&d.a, s);
        let t = self.b[0].tick(&d.b, [s[0], s[0], s[1], s[1]]);
        let t = self.b[1].tick(&d.b, t);
        let u = self.c.tick(&d.c, t);
        u[0] * lanes[0] + u[1] * lanes[1] + u[2] * lanes[2] + u[3] * lanes[3]
    }
    /// Filters a block in place with ramped lane gains.
    pub(crate) fn process(&mut self, d: &BandDesign, io: &mut [f32], lanes: &mut Ramp4) {
        for x in io.iter_mut() {
            let g = lanes.next();
            *x = self.tick(d, *x, g);
        }
        self.flush();
    }
    pub(crate) fn flush(&mut self) {
        self.a.iter_mut().for_each(State::flush);
        self.b.iter_mut().for_each(State::flush);
        self.c.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    #[test]
    fn centre_gains_are_exact_for_strong_steps() {
        for rate in [44_100, 48_000, 96_000] {
            let d = BandDesign::new(rate);
            for g in [
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 0.01, 1.0, 0.01],
                [0.55, 0.316, 0.174, 0.1],
                [0.001, 1.0, 0.5, 0.25],
            ] {
                let lanes = d.lanes(g);
                for b in 0..BANDS {
                    let m = d.magnitude(lanes, f64::from(BAND_CENTERS_HZ[b]), rate);
                    assert!(
                        (db(m) - db(f64::from(g[b]))).abs() < 0.01,
                        "{rate} {g:?} band {b}: {}",
                        db(m)
                    );
                }
            }
        }
    }

    #[test]
    fn unity_is_an_allpass() {
        let d = BandDesign::new(48_000);
        let lanes = d.lanes([1.0; 4]);
        for f in [20.0, 100.0, 777.0, 3000.0, 12_000.0, 20_000.0] {
            assert!(db(d.magnitude(lanes, f, 48_000)).abs() < 0.001, "{f}");
        }
    }

    #[test]
    fn low_rates_drop_the_unrepresentable_band() {
        let d = BandDesign::new(8_000);
        let lanes = d.lanes([1.0, 0.5, 0.25, 0.9]);
        for b in 0..3 {
            let m = d.magnitude(lanes, f64::from(BAND_CENTERS_HZ[b]), 8_000);
            assert!((m - [1.0, 0.5, 0.25][b]).abs() < 1e-3, "band {b}: {m}");
        }
    }

    #[test]
    fn group_delay_is_positive_and_largest_at_low_frequency() {
        let gd = BandDesign::new(48_000).group_delay();
        assert!(gd.iter().all(|g| *g > 0.0), "{gd:?}");
        assert!(gd[0] > gd[3], "{gd:?}");
    }
}
