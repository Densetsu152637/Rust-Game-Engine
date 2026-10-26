//! Tessendorf FFT ocean: a seeded, tileable patch evolved in time with the dispersion relation.
//!
//! Mode `k = 2 pi (i, j) / L` (signed FFT frequencies, Nyquist and zero modes left empty) starts
//! with `h0(k) = (xi_r + i xi_i) / 2 sqrt(P(k)) dk` (`xi` standard normal from the seed,
//! `dk = 2 pi / L`) and evolves as `h(k, t) = h0(k) e^(-i w t) + conj(h0(-k)) e^(i w t)`, which is
//! Hermitian (a real height field) and travels along `+k`. The expected height variance is
//! `sum P(k) dk^2`, the Riemann sum of the continuum variance.
//!
//! One frame takes four complex inverse FFTs, each packing two real fields (`A + i B`): height
//! `h`, horizontal displacement `D = lambda IFFT(i k_hat h)`, slopes `IFFT(i k h)` and the
//! displacement Jacobian `lambda IFFT(-k k^T / |k| h)`. The patch tiles with period
//! `patch_length` in X and Z.
use crate::displace::{HorizontalSample, Inversion, invert_horizontal};
use crate::fft::{Complex, fft2};
use crate::gerstner::{angular_frequency, surface_normal};
use crate::rng::SplitMix64;
use crate::spectrum::OceanSettings;
use crate::{Error, Result};
use glam::{Mat2, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;

/// The seeded initial spectrum of one ocean patch.
#[derive(Clone, Debug, PartialEq)]
pub struct OceanSpectrum {
    settings: OceanSettings,
    gravity: f32,
    n: usize,
    h0: Vec<Complex>,
    /// `conj(h0(-k))` per mode.
    h0_mirror: Vec<Complex>,
    omega: Vec<f32>,
    k: Vec<Vec2>,
}

/// Signed frequency of FFT index `i` on an `n` grid.
fn signed(i: usize, n: usize) -> i64 {
    if i < n / 2 {
        i as i64
    } else {
        i as i64 - n as i64
    }
}

impl OceanSpectrum {
    /// Draws the initial amplitudes for `settings` (deterministic for a seed).
    pub fn new(settings: &OceanSettings, gravity: f32) -> Result<Self> {
        settings.validate()?;
        if !(gravity.is_finite() && gravity > 0.0) {
            return Err(Error::new(
                "water.ocean.invalid",
                "gravity must be positive",
            ));
        }
        let n = settings.resolution as usize;
        let dk = TAU / settings.patch_length;
        let depth = settings.dispersion_depth();
        let quantum = (settings.loop_period > 0.0).then(|| TAU / settings.loop_period);
        let mut rng = SplitMix64::new(settings.seed);
        let mut h0 = vec![Complex::ZERO; n * n];
        let mut omega = vec![0.0; n * n];
        let mut k = vec![Vec2::ZERO; n * n];
        for r in 0..n {
            for c in 0..n {
                let index = r * n + c;
                let (g1, g2) = rng.gaussian_pair();
                let (fx, fz) = (signed(c, n), signed(r, n));
                let wave = Vec2::new(fx as f32, fz as f32) * dk;
                k[index] = wave;
                let w = angular_frequency(wave.length(), depth, gravity);
                omega[index] = match quantum {
                    Some(q) => (w / q).floor() * q,
                    None => w,
                };
                let nyquist = c == n / 2 || r == n / 2;
                if nyquist || (fx == 0 && fz == 0) {
                    continue;
                }
                let amplitude = 0.5 * settings.density(wave, gravity).max(0.0).sqrt() * dk;
                h0[index] = Complex::new(g1 * amplitude, g2 * amplitude);
            }
        }
        let mirror = |r: usize, c: usize| ((n - r) % n) * n + (n - c) % n;
        let h0_mirror = (0..n * n)
            .map(|i| h0[mirror(i / n, i % n)].conj())
            .collect();
        Ok(Self {
            settings: settings.clone(),
            gravity,
            n,
            h0,
            h0_mirror,
            omega,
            k,
        })
    }

    pub fn settings(&self) -> &OceanSettings {
        &self.settings
    }
    pub fn resolution(&self) -> usize {
        self.n
    }
    /// Wavenumber and angular frequency of FFT mode `(column, row)`.
    pub fn mode(&self, column: usize, row: usize) -> Option<(Vec2, f32)> {
        let i = row.checked_mul(self.n)?.checked_add(column)?;
        (column < self.n && row < self.n).then(|| (self.k[i], self.omega[i]))
    }
    /// Initial amplitude `h0` of mode `(column, row)`.
    pub fn initial(&self, column: usize, row: usize) -> Complex {
        self.h0[row * self.n + column]
    }

    /// Fourier amplitudes `h(k, t)` in FFT order.
    pub fn amplitudes(&self, t: f32) -> Vec<Complex> {
        self.amplitudes_f64(f64::from(t))
    }

    /// [`Self::amplitudes`] for an f64 clock: each mode's phase `omega * t` is computed in f64
    /// and wrapped to one turn before it becomes an f32, so hours of simulated time keep full
    /// phase accuracy.
    pub fn amplitudes_f64(&self, t: f64) -> Vec<Complex> {
        (0..self.n * self.n)
            .map(|i| {
                let turns = (f64::from(self.omega[i]) * t).rem_euclid(std::f64::consts::TAU);
                let phase = Complex::from_angle(turns as f32);
                self.h0[i] * phase.conj() + self.h0_mirror[i] * phase
            })
            .collect()
    }

    /// Expected height variance of the discrete patch, `sum P(k) dk^2` (m^2).
    pub fn discrete_variance(&self) -> f32 {
        let dk = TAU / self.settings.patch_length;
        let n = self.n;
        let mut sum = 0.0f64;
        for r in 0..n {
            for c in 0..n {
                if c == n / 2 || r == n / 2 {
                    continue;
                }
                sum += self.settings.density(self.k[r * n + c], self.gravity) as f64;
            }
        }
        (sum * (dk * dk) as f64) as f32
    }

    /// Height of the (unchopped) surface at rest position `x` by direct Fourier summation.
    /// O(N^2) per point: for verification and tests.
    pub fn height_direct(&self, x: Vec2, t: f32) -> f32 {
        let amplitudes = self.amplitudes(t);
        let mut sum = 0.0f64;
        for (a, k) in amplitudes.iter().zip(&self.k) {
            let (s, c) = (k.dot(x) as f64).sin_cos();
            sum += a.re as f64 * c - a.im as f64 * s;
        }
        sum as f32
    }

    /// Evaluates the patch at time `t`.
    pub fn frame(&self, t: f32) -> Result<OceanFrame> {
        self.frame_f64(f64::from(t))
    }

    /// Evaluates the patch at the f64 time `t` (see [`Self::amplitudes_f64`]). A looping
    /// spectrum (`loop_period > 0`) is evaluated at `t` modulo the period, which is exact because
    /// every mode frequency is a multiple of `2 pi / loop_period`; the frame's
    /// [`OceanFrame::time`] is that reduced time.
    pub fn frame_f64(&self, t: f64) -> Result<OceanFrame> {
        let t = if self.settings.loop_period > 0.0 {
            t.rem_euclid(f64::from(self.settings.loop_period))
        } else {
            t
        };
        let n = self.n;
        let lambda = self.settings.choppiness;
        let h = self.amplitudes_f64(t);
        let mut pairs = [
            vec![Complex::ZERO; n * n],
            vec![Complex::ZERO; n * n],
            vec![Complex::ZERO; n * n],
            vec![Complex::ZERO; n * n],
        ];
        for i in 0..n * n {
            let (k, a) = (self.k[i], h[i]);
            let length = k.length();
            if length < 1.0e-9 {
                continue;
            }
            let unit = k / length;
            // i k_hat h, i k h and -k k^T / |k| h.
            let dx = a.times_i().scale(unit.x * lambda);
            let dz = a.times_i().scale(unit.y * lambda);
            let hx = a.times_i().scale(k.x);
            let hz = a.times_i().scale(k.y);
            let dxx = a.scale(-k.x * unit.x * lambda);
            let dzz = a.scale(-k.y * unit.y * lambda);
            let dxz = a.scale(-k.x * unit.y * lambda);
            pairs[0][i] = a + dx.times_i();
            pairs[1][i] = dz + hx.times_i();
            pairs[2][i] = hz + dxx.times_i();
            pairs[3][i] = dzz + dxz.times_i();
        }
        for field in &mut pairs {
            fft2(field, n, true)?;
        }
        let texels = (0..n * n)
            .map(|i| {
                let [a, b, c, d] = [pairs[0][i], pairs[1][i], pairs[2][i], pairs[3][i]];
                OceanTexel {
                    dx: a.im,
                    h: a.re,
                    dz: b.re,
                    hx: b.im,
                    hz: c.re,
                    dxx: c.im,
                    dzz: d.re,
                    dxz: d.im,
                }
            })
            .collect();
        Ok(OceanFrame {
            n,
            length: self.settings.patch_length,
            time: t as f32,
            texels,
        })
    }

    /// Spectrum summary: significant wave height `4 sqrt(variance)`, peak wavelength and
    /// period, mean-square slope and a 16-bin energy histogram over log wavelength.
    pub fn stats(&self) -> WaveStats {
        let variance = self.discrete_variance();
        let k_peak = self.settings.peak_wavenumber(self.gravity);
        let omega_peak = angular_frequency(k_peak, self.settings.dispersion_depth(), self.gravity);
        let dk = TAU / self.settings.patch_length;
        let n = self.n;
        let shortest = 2.0 * self.settings.patch_length / n as f32;
        let longest = self.settings.patch_length;
        let bins = 16usize;
        let mut histogram = vec![0.0f64; bins];
        let mut slope = 0.0f64;
        let ratio = (longest / shortest).ln().max(1.0e-6);
        for r in 0..n {
            for c in 0..n {
                if c == n / 2 || r == n / 2 {
                    continue;
                }
                let k = self.k[r * n + c];
                let length = k.length();
                if length < 1.0e-9 {
                    continue;
                }
                let e = (self.settings.density(k, self.gravity) * dk * dk) as f64;
                slope += e * (length * length) as f64;
                let wavelength = TAU / length;
                let bin = ((wavelength / shortest).ln() / ratio * bins as f32).floor();
                let bin = (bin.max(0.0) as usize).min(bins - 1);
                histogram[bin] += e;
            }
        }
        let total: f64 = histogram.iter().sum::<f64>().max(1.0e-30);
        WaveStats {
            variance,
            significant_height: 4.0 * variance.sqrt(),
            continuum_variance: self.settings.continuum_variance(self.gravity),
            peak_wavelength: TAU / k_peak,
            peak_period: if omega_peak > 0.0 {
                TAU / omega_peak
            } else {
                0.0
            },
            mean_square_slope: slope as f32,
            shortest_wavelength: shortest,
            longest_wavelength: longest,
            energy_by_wavelength: histogram.iter().map(|e| (e / total) as f32).collect(),
        }
    }
}

/// Summary statistics of an ocean spectrum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveStats {
    /// Expected height variance of the patch (m^2).
    pub variance: f32,
    /// `4 sqrt(variance)` (m).
    pub significant_height: f32,
    /// Variance of the continuous spectrum (m^2); the patch misses waves longer than the patch
    /// and shorter than two cells.
    pub continuum_variance: f32,
    pub peak_wavelength: f32,
    pub peak_period: f32,
    pub mean_square_slope: f32,
    /// Shortest (two cells) and longest (the patch) resolved wavelength in metres.
    pub shortest_wavelength: f32,
    pub longest_wavelength: f32,
    /// Fraction of the patch energy in 16 log-spaced wavelength bins, shortest first.
    pub energy_by_wavelength: Vec<f32>,
}

/// One grid sample of an ocean frame (choppiness applied to the displacement terms).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OceanTexel {
    /// Horizontal displacement.
    pub dx: f32,
    /// Height.
    pub h: f32,
    pub dz: f32,
    /// Height slopes `dh/dx0`, `dh/dz0`.
    pub hx: f32,
    pub hz: f32,
    /// Displacement Jacobian terms `dDx/dx0`, `dDz/dz0`, `dDx/dz0 (= dDz/dx0)`.
    pub dxx: f32,
    pub dzz: f32,
    pub dxz: f32,
}
impl OceanTexel {
    fn jacobian(&self) -> Mat2 {
        Mat2::from_cols(Vec2::new(self.dxx, self.dxz), Vec2::new(self.dxz, self.dzz))
    }
    /// Determinant of the horizontal map's Jacobian (below 0: folded, foam).
    pub fn determinant(&self) -> f32 {
        (1.0 + self.dxx) * (1.0 + self.dzz) - self.dxz * self.dxz
    }
    pub fn normal(&self) -> Vec3 {
        surface_normal(self.jacobian(), Vec2::new(self.hx, self.hz))
    }
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Self {
            dx: l(a.dx, b.dx),
            h: l(a.h, b.h),
            dz: l(a.dz, b.dz),
            hx: l(a.hx, b.hx),
            hz: l(a.hz, b.hz),
            dxx: l(a.dxx, b.dxx),
            dzz: l(a.dzz, b.dzz),
            dxz: l(a.dxz, b.dxz),
        }
    }
}

/// Displacement, height and derivatives of an ocean point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OceanPoint {
    /// `(dx, h, dz)`.
    pub offset: Vec3,
    pub normal: Vec3,
    /// Jacobian determinant of the horizontal map.
    pub determinant: f32,
}

/// The surface of one patch at one time, sampled on the `n` x `n` grid (row-major, rows along
/// Z, sample `(c, r)` at rest position `(c, r) * patch_length / n`).
#[derive(Clone, Debug, PartialEq)]
pub struct OceanFrame {
    n: usize,
    length: f32,
    time: f32,
    texels: Vec<OceanTexel>,
}
impl OceanFrame {
    pub fn resolution(&self) -> usize {
        self.n
    }
    pub fn patch_length(&self) -> f32 {
        self.length
    }
    pub fn time(&self) -> f32 {
        self.time
    }
    pub fn texels(&self) -> &[OceanTexel] {
        &self.texels
    }
    /// Texel at integer grid coordinates (wrapped).
    pub fn texel(&self, column: i64, row: i64) -> &OceanTexel {
        let n = self.n as i64;
        &self.texels[(row.rem_euclid(n) * n + column.rem_euclid(n)) as usize]
    }
    /// Bilinearly interpolated, periodic texel at rest position `x0` (metres).
    pub fn texel_at(&self, x0: Vec2) -> OceanTexel {
        let g = x0 * (self.n as f32 / self.length);
        let (c, r) = (g.x.floor(), g.y.floor());
        let (fx, fz) = (g.x - c, g.y - r);
        let (c, r) = (c as i64, r as i64);
        let top = OceanTexel::lerp(self.texel(c, r), self.texel(c + 1, r), fx);
        let bottom = OceanTexel::lerp(self.texel(c, r + 1), self.texel(c + 1, r + 1), fx);
        OceanTexel::lerp(&top, &bottom, fz)
    }
    /// The displaced point of rest position `x0`.
    pub fn at_rest(&self, x0: Vec2) -> OceanPoint {
        let t = self.texel_at(x0);
        OceanPoint {
            offset: Vec3::new(t.dx, t.h, t.dz),
            normal: t.normal(),
            determinant: t.determinant(),
        }
    }
    /// Surface above world-horizontal `p` (displacement inverted; see [`crate::displace`]).
    /// Where the choppy displacement folds (determinant <= 0) the inverse is not unique and
    /// the iterate with the smallest residual is returned.
    pub fn sample(&self, p: Vec2) -> (OceanPoint, Inversion) {
        let inversion = invert_horizontal(p, |x0| {
            let t = self.texel_at(x0);
            HorizontalSample {
                offset: Vec2::new(t.dx, t.dz),
                jacobian: t.jacobian(),
            }
        });
        (self.at_rest(inversion.rest), inversion)
    }
    /// GPU textures: `(dx, h, dz, determinant)` and `(normal, 0)` per texel, row-major.
    pub fn gpu_textures(&self) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        self.texels
            .iter()
            .map(|t| {
                let n = t.normal();
                ([t.dx, t.h, t.dz, t.determinant()], [n.x, n.y, n.z, 0.0])
            })
            .unzip()
    }
    /// Height statistics: `(min, max, mean square, folded fraction)`.
    pub fn height_stats(&self) -> (f32, f32, f32, f32) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        let mut square = 0.0f64;
        let mut folded = 0usize;
        for t in &self.texels {
            lo = lo.min(t.h);
            hi = hi.max(t.h);
            square += (t.h * t.h) as f64;
            folded += usize::from(t.determinant() <= 0.0);
        }
        let count = self.texels.len().max(1) as f64;
        (
            lo,
            hi,
            (square / count) as f32,
            (folded as f64 / count) as f32,
        )
    }
}
