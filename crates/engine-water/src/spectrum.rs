//! Ocean wave spectra on the wavenumber plane.
//!
//! `P(k)` is a directional wavenumber spectrum in m^4: the height variance is
//! `integral P(k) d^2k`.
//!
//! * **Phillips** (Tessendorf 2001): `P(k) = a (alpha / 2 pi) k^-4 exp(-1 / (k L)^2)
//!   (k_hat . w_hat)^2 exp(-k^2 l^2)` with `alpha = 0.0081`, `L = V^2 / g` (largest wave from
//!   wind speed `V`) and an optional small-wave cutoff `l`. Its continuum variance is
//!   `a alpha L^2 / 4` (for `l = 0`).
//! * **JONSWAP** (Hasselmann et al. 1973) over angular frequency with fetch `F` and peak
//!   enhancement `gamma`: `S(w) = alpha g^2 w^-5 exp(-5/4 (w_p / w)^4) gamma^r`,
//!   `alpha = 0.076 (V^2 / (F g))^0.22`, `w_p = 22 (g^2 / (V F))^(1/3)`,
//!   `r = exp(-(w - w_p)^2 / (2 sigma^2 w_p^2))`, `sigma = 0.07` below and `0.09` above the
//!   peak, spread as `D(theta) = (2 / pi) cos^2(theta - theta_wind)` over the downwind half
//!   plane and mapped to wavenumbers with the deep-water relation `w = sqrt(g k)`:
//!   `P(k) = a S(w) (dw/dk) D(theta) / k`, `dw/dk = g / (2 w)`.
use crate::{Error, Result};
use glam::Vec2;
use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};

/// The Phillips constant.
pub const PHILLIPS_ALPHA: f32 = 0.0081;
/// Grid sizes accepted by [`OceanSettings::resolution`].
pub const OCEAN_RESOLUTIONS: [u32; 6] = [16, 32, 64, 128, 256, 512];

/// Which spectrum seeds the ocean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpectrumKind {
    /// Fully developed wind sea (Tessendorf's Phillips spectrum); waves in both directions
    /// along the wind axis.
    #[default]
    Phillips,
    /// Fetch-limited JONSWAP; waves travel downwind only.
    Jonswap,
}

/// Parameters of a Tessendorf FFT ocean patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OceanSettings {
    /// Grid side (samples per patch side): 16, 32, 64, 128, 256 or 512.
    pub resolution: u32,
    /// Patch side in metres (1..=10000); the surface tiles with this period.
    pub patch_length: f32,
    pub spectrum: SpectrumKind,
    /// Wind speed at 10 m in m/s (0.1..=60).
    pub wind_speed: f32,
    /// Wind direction in the XZ plane, degrees from +X toward +Z.
    pub wind_direction_degrees: f32,
    /// Multiplier on the spectrum (0..=100); heights scale with its square root.
    pub amplitude: f32,
    /// JONSWAP fetch in metres (1000..=10000000).
    pub fetch: f32,
    /// JONSWAP peak enhancement gamma (1..=10).
    pub peak_enhancement: f32,
    /// Horizontal displacement factor lambda (0..=2): `x = x0 + lambda D(x0)`.
    pub choppiness: f32,
    /// Waves shorter than this (m) are damped by `exp(-k^2 l^2)` (0..=100; 0 = off).
    pub small_wave_cutoff: f32,
    /// Water depth for the dispersion relation in metres (0 = deep water).
    pub depth: f32,
    /// Quantizes frequencies so the surface repeats every `loop_period` seconds (0 = never;
    /// otherwise 1..=3600).
    pub loop_period: f32,
    pub seed: u64,
}
impl Default for OceanSettings {
    fn default() -> Self {
        Self {
            resolution: 64,
            patch_length: 128.0,
            spectrum: SpectrumKind::Phillips,
            wind_speed: 10.0,
            wind_direction_degrees: 0.0,
            amplitude: 1.0,
            fetch: 100_000.0,
            peak_enhancement: 3.3,
            choppiness: 1.0,
            small_wave_cutoff: 0.0,
            depth: 0.0,
            loop_period: 0.0,
            seed: 1,
        }
    }
}

fn within(v: f32, lo: f32, hi: f32) -> bool {
    v.is_finite() && (lo..=hi).contains(&v)
}

impl OceanSettings {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("water.ocean.invalid", m.to_string()));
        if !OCEAN_RESOLUTIONS.contains(&self.resolution) {
            return bad("ocean resolution must be 16, 32, 64, 128, 256 or 512");
        }
        if !within(self.patch_length, 1.0, 10_000.0) {
            return bad("ocean patch_length must be within 1..=10000 m");
        }
        if !within(self.wind_speed, 0.1, 60.0) {
            return bad("ocean wind_speed must be within 0.1..=60 m/s");
        }
        if !self.wind_direction_degrees.is_finite() {
            return bad("ocean wind_direction_degrees must be finite");
        }
        if !within(self.amplitude, 0.0, 100.0) {
            return bad("ocean amplitude must be within 0..=100");
        }
        if !within(self.fetch, 1000.0, 1.0e7) {
            return bad("ocean fetch must be within 1000..=10000000 m");
        }
        if !within(self.peak_enhancement, 1.0, 10.0) {
            return bad("ocean peak_enhancement must be within 1..=10");
        }
        if !within(self.choppiness, 0.0, 2.0) {
            return bad("ocean choppiness must be within 0..=2");
        }
        if !within(self.small_wave_cutoff, 0.0, 100.0) {
            return bad("ocean small_wave_cutoff must be within 0..=100 m");
        }
        if !(self.depth == 0.0 || within(self.depth, 0.1, 10_000.0)) {
            return bad("ocean depth must be 0 (deep) or within 0.1..=10000 m");
        }
        if !(self.loop_period == 0.0 || within(self.loop_period, 1.0, 3600.0)) {
            return bad("ocean loop_period must be 0 or within 1..=3600 s");
        }
        Ok(())
    }
    /// Unit wind direction.
    pub fn wind(&self) -> Vec2 {
        let a = self.wind_direction_degrees.to_radians();
        Vec2::new(a.cos(), a.sin())
    }
    /// Depth for the dispersion relation (`None` for deep water).
    pub fn dispersion_depth(&self) -> Option<f32> {
        (self.depth > 0.0).then_some(self.depth)
    }

    /// JONSWAP `(alpha, peak angular frequency)` for this wind and fetch.
    pub fn jonswap_parameters(&self, gravity: f32) -> (f32, f32) {
        let v = self.wind_speed;
        let alpha = 0.076 * (v * v / (self.fetch * gravity)).powf(0.22);
        let peak = 22.0 * (gravity * gravity / (v * self.fetch)).powf(1.0 / 3.0);
        (alpha, peak)
    }

    /// JONSWAP frequency spectrum `S(w)` in m^2 s (without the `amplitude` factor).
    pub fn jonswap_frequency(&self, omega: f32, gravity: f32) -> f32 {
        if omega <= 0.0 {
            return 0.0;
        }
        let (alpha, peak) = self.jonswap_parameters(gravity);
        let sigma = if omega <= peak { 0.07 } else { 0.09 };
        let r = (-(omega - peak).powi(2) / (2.0 * sigma * sigma * peak * peak)).exp();
        alpha * gravity * gravity / omega.powi(5)
            * (-1.25 * (peak / omega).powi(4)).exp()
            * self.peak_enhancement.powf(r)
    }

    /// Directional wavenumber spectrum `P(k)` in m^4.
    pub fn density(&self, k: Vec2, gravity: f32) -> f32 {
        let length = k.length();
        if length < 1.0e-6 {
            return 0.0;
        }
        let dir = k / length;
        let cutoff = (-length * length * self.small_wave_cutoff * self.small_wave_cutoff).exp();
        let value = match self.spectrum {
            SpectrumKind::Phillips => {
                let big = self.wind_speed * self.wind_speed / gravity;
                let align = dir.dot(self.wind());
                PHILLIPS_ALPHA / TAU
                    * length.powi(-4)
                    * (-1.0 / (length * big).powi(2)).exp()
                    * align
                    * align
            }
            SpectrumKind::Jonswap => {
                let cos = dir.dot(self.wind());
                if cos <= 0.0 {
                    return 0.0;
                }
                let omega = (gravity * length).sqrt();
                let spread = 2.0 / PI * cos * cos;
                self.jonswap_frequency(omega, gravity) * (gravity / (2.0 * omega)) * spread / length
            }
        };
        self.amplitude * value * cutoff
    }

    /// Height variance of the continuous spectrum in m^2 (analytic for Phillips, Simpson
    /// integration of `S(w)` for JONSWAP; the small-wave cutoff is ignored).
    pub fn continuum_variance(&self, gravity: f32) -> f32 {
        match self.spectrum {
            SpectrumKind::Phillips => {
                let big = self.wind_speed * self.wind_speed / gravity;
                self.amplitude * PHILLIPS_ALPHA * big * big / 4.0
            }
            SpectrumKind::Jonswap => {
                let (_, peak) = self.jonswap_parameters(gravity);
                let (lo, hi, steps) = (0.2 * peak, 20.0 * peak, 20_000usize);
                let h = (hi - lo) / steps as f32;
                let mut sum =
                    self.jonswap_frequency(lo, gravity) + self.jonswap_frequency(hi, gravity);
                for i in 1..steps {
                    let w = if i % 2 == 1 { 4.0 } else { 2.0 };
                    sum += w * self.jonswap_frequency(lo + i as f32 * h, gravity);
                }
                self.amplitude * sum * h / 3.0
            }
        }
    }

    /// Wavenumber of the spectral peak of the omnidirectional spectrum (rad/m).
    pub fn peak_wavenumber(&self, gravity: f32) -> f32 {
        match self.spectrum {
            // d/dk of k^-3 exp(-1/(kL)^2) vanishes at k = sqrt(2/3) / L.
            SpectrumKind::Phillips => {
                (2.0f32 / 3.0).sqrt() * gravity / (self.wind_speed * self.wind_speed)
            }
            SpectrumKind::Jonswap => {
                let (_, peak) = self.jonswap_parameters(gravity);
                peak * peak / gravity
            }
        }
    }
}
