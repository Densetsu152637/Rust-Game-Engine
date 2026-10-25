//! Gerstner (trochoidal) wave sets.
//!
//! Wave `i` with unit direction `d`, wavenumber `k = 2 pi / wavelength`, amplitude `A`,
//! angular frequency `w = sqrt(g k tanh(k h))` (deep water: `sqrt(g k)`) and phase `phi` moves
//! the rest point `x0` (horizontal, metres) at time `t` to
//!
//! ```text
//! theta = k (d . x0) - w t + phi
//! x = x0 + sum q_i A_i d_i cos(theta_i)
//! y =      sum A_i sin(theta_i)
//! ```
//!
//! The chop factor `q_i = c s_i / (k_i A_i)` comes from the authored steepness `s_i` (0..=1)
//! and a set-wide scale `c = min(1, MAX_STEEPNESS_SUM / sum s_i)`, so
//! `sum q_i k_i A_i <= MAX_STEEPNESS_SUM = 0.9`. That sum bounds the Lipschitz constant `L` of
//! the horizontal displacement, so the horizontal map has a Jacobian determinant of at least
//! `(1 - L)^2 > 0` (no loops or folds) and its inverse (height at a world position) converges
//! (see [`crate::displace`]).
use crate::displace::{HorizontalSample, Inversion, invert_horizontal};
use crate::{Error, GRAVITY, Result};
use glam::{Mat2, Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Most waves in one set.
pub const MAX_GERSTNER_WAVES: usize = 16;
/// Upper bound of `sum q_i k_i A_i` (the horizontal displacement's Lipschitz constant).
pub const MAX_STEEPNESS_SUM: f32 = 0.9;
/// Largest `amplitude / wavelength` (the Stokes limit `H / L = 1 / 7`).
pub const MAX_AMPLITUDE_RATIO: f32 = 1.0 / 14.0;

/// Angular frequency of a surface gravity wave of wavenumber `k` (rad/m) in water of `depth`
/// metres (`None`: deep water): `w^2 = g k tanh(k h)`.
pub fn angular_frequency(k: f32, depth: Option<f32>, gravity: f32) -> f32 {
    let deep = gravity * k;
    match depth {
        Some(h) => (deep * (k * h).tanh()).max(0.0).sqrt(),
        None => deep.max(0.0).sqrt(),
    }
}

/// One authored Gerstner wave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GerstnerWave {
    /// Travel direction in the XZ plane, degrees from +X toward +Z.
    pub direction_degrees: f32,
    /// Crest-to-crest distance in metres (0.05..=5000).
    pub wavelength: f32,
    /// Crest height above the mean level in metres (at most `wavelength / 14`).
    pub amplitude: f32,
    /// Crest sharpness 0..=1 (0: sine wave; the set scales the sum down to 0.9).
    pub steepness: f32,
    /// Phase offset in radians.
    pub phase: f32,
}
impl Default for GerstnerWave {
    fn default() -> Self {
        Self {
            direction_degrees: 0.0,
            wavelength: 10.0,
            amplitude: 0.2,
            steepness: 0.5,
            phase: 0.0,
        }
    }
}
impl GerstnerWave {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("water.waves.invalid", m.to_string()));
        if !self.direction_degrees.is_finite() || !self.phase.is_finite() {
            return bad("Gerstner direction and phase must be finite");
        }
        if !(self.wavelength.is_finite() && (0.05..=5000.0).contains(&self.wavelength)) {
            return bad("Gerstner wavelength must be within 0.05..=5000 m");
        }
        if !(self.amplitude.is_finite()
            && self.amplitude >= 0.0
            && self.amplitude <= self.wavelength * MAX_AMPLITUDE_RATIO)
        {
            return bad("Gerstner amplitude must be within 0..=wavelength/14");
        }
        if !(self.steepness.is_finite() && (0.0..=1.0).contains(&self.steepness)) {
            return bad("Gerstner steepness must be within 0..=1");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Compiled {
    direction: Vec2,
    k: f32,
    omega: f32,
    amplitude: f32,
    /// `q A`: horizontal excursion.
    chop: f32,
    phase: f32,
}

/// Displacement, derivatives and velocity of a Gerstner set at one rest point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WavePoint {
    /// `(dx, y, dz)`: horizontal displacement and height.
    pub offset: Vec3,
    /// `dD/dx0` of the horizontal displacement (columns: d/dx0, d/dz0).
    pub jacobian: Mat2,
    /// `(dy/dx0, dy/dz0)`.
    pub slope: Vec2,
    /// Velocity of the surface particle (orbital motion), m/s.
    pub velocity: Vec3,
}
impl WavePoint {
    /// Unit normal of the displaced surface.
    pub fn normal(&self) -> Vec3 {
        surface_normal(self.jacobian, self.slope)
    }
}

/// Unit normal of a displaced surface from the horizontal Jacobian and height slopes (both with
/// respect to the rest position): `normalize(dP/dz0 x dP/dx0)`.
pub fn surface_normal(jacobian: Mat2, slope: Vec2) -> Vec3 {
    let tx = Vec3::new(1.0 + jacobian.x_axis.x, slope.x, jacobian.x_axis.y);
    let tz = Vec3::new(jacobian.y_axis.x, slope.y, 1.0 + jacobian.y_axis.y);
    tz.cross(tx).try_normalize().unwrap_or(Vec3::Y)
}

/// A validated, compiled wave set.
#[derive(Clone, Debug, PartialEq)]
pub struct GerstnerSet {
    waves: Vec<Compiled>,
    steepness_scale: f32,
    lipschitz: f32,
}
impl GerstnerSet {
    /// Compiles `waves` for water `depth` (`None` = deep) under `gravity` (m/s^2).
    pub fn new(waves: &[GerstnerWave], depth: Option<f32>, gravity: f32) -> Result<Self> {
        if waves.len() > MAX_GERSTNER_WAVES {
            return Err(Error::new(
                "water.waves.invalid",
                format!("at most {MAX_GERSTNER_WAVES} Gerstner waves"),
            ));
        }
        if !(gravity.is_finite() && gravity > 0.0) {
            return Err(Error::new(
                "water.waves.invalid",
                "gravity must be positive",
            ));
        }
        if depth.is_some_and(|d| !(d.is_finite() && d > 0.0)) {
            return Err(Error::new("water.waves.invalid", "depth must be positive"));
        }
        for wave in waves {
            wave.validate()?;
        }
        let sum: f32 = waves
            .iter()
            .filter(|w| w.amplitude > 0.0)
            .map(|w| w.steepness)
            .sum();
        let steepness_scale = if sum > MAX_STEEPNESS_SUM {
            MAX_STEEPNESS_SUM / sum
        } else {
            1.0
        };
        let compiled: Vec<Compiled> = waves
            .iter()
            .map(|w| {
                let k = std::f32::consts::TAU / w.wavelength;
                let angle = w.direction_degrees.to_radians();
                let chop = if w.amplitude > 0.0 {
                    w.steepness * steepness_scale / k
                } else {
                    0.0
                };
                Compiled {
                    direction: Vec2::new(angle.cos(), angle.sin()),
                    k,
                    omega: angular_frequency(k, depth, gravity),
                    amplitude: w.amplitude,
                    chop,
                    phase: w.phase,
                }
            })
            .collect();
        let lipschitz = compiled.iter().map(|w| w.chop * w.k).sum();
        Ok(Self {
            waves: compiled,
            steepness_scale,
            lipschitz,
        })
    }
    /// Deep-water set under standard gravity.
    pub fn deep(waves: &[GerstnerWave]) -> Result<Self> {
        Self::new(waves, None, GRAVITY)
    }
    pub fn len(&self) -> usize {
        self.waves.len()
    }
    pub fn is_empty(&self) -> bool {
        self.waves.is_empty()
    }
    /// Factor applied to every authored steepness (1 unless the sum exceeded 0.9).
    pub fn steepness_scale(&self) -> f32 {
        self.steepness_scale
    }
    /// `sum q_i k_i A_i`: the Lipschitz constant of the horizontal displacement (<= 0.9).
    pub fn lipschitz(&self) -> f32 {
        self.lipschitz
    }
    /// Sum of amplitudes: the largest possible height.
    pub fn max_height(&self) -> f32 {
        self.waves.iter().map(|w| w.amplitude).sum()
    }
    /// `(wavenumber, angular frequency)` of wave `i`.
    pub fn dispersion(&self, i: usize) -> Option<(f32, f32)> {
        self.waves.get(i).map(|w| (w.k, w.omega))
    }
    /// Per-wave GPU parameters: `[dir.x, dir.z, k, omega]` and `[amplitude, chop, phase, 0]`.
    pub fn gpu_params(&self) -> Vec<([f32; 4], [f32; 4])> {
        self.waves
            .iter()
            .map(|w| {
                (
                    [w.direction.x, w.direction.y, w.k, w.omega],
                    [w.amplitude, w.chop, w.phase, 0.0],
                )
            })
            .collect()
    }

    /// Displacement of rest point `x0` at time `t`, with every amplitude multiplied by `scale`
    /// (shoreline attenuation; 1 = authored).
    pub fn evaluate(&self, x0: Vec2, t: f32, scale: f32) -> WavePoint {
        let mut point = WavePoint {
            offset: Vec3::ZERO,
            jacobian: Mat2::ZERO,
            slope: Vec2::ZERO,
            velocity: Vec3::ZERO,
        };
        for w in &self.waves {
            let theta = w.k * w.direction.dot(x0) - w.omega * t + w.phase;
            let (s, c) = theta.sin_cos();
            let (a, q) = (w.amplitude * scale, w.chop * scale);
            let d = w.direction;
            point.offset += Vec3::new(q * d.x * c, a * s, q * d.y * c);
            // d/dx0 of q d cos(theta) = -q k d d^T sin(theta).
            let j = -q * w.k * s;
            point.jacobian += Mat2::from_cols(
                Vec2::new(j * d.x * d.x, j * d.x * d.y),
                Vec2::new(j * d.x * d.y, j * d.y * d.y),
            );
            point.slope += a * w.k * c * d;
            point.velocity += Vec3::new(
                q * w.omega * d.x * s,
                -a * w.omega * c,
                q * w.omega * d.y * s,
            );
        }
        point
    }

    /// Height, normal and particle velocity of the surface above world-horizontal `p` at time
    /// `t` (amplitudes scaled by `scale`), by inverting the horizontal displacement.
    pub fn sample(&self, p: Vec2, t: f32, scale: f32) -> (WavePoint, Inversion) {
        let inversion = invert_horizontal(p, |x0| {
            let point = self.evaluate(x0, t, scale);
            HorizontalSample {
                offset: Vec2::new(point.offset.x, point.offset.z),
                jacobian: point.jacobian,
            }
        });
        (self.evaluate(inversion.rest, t, scale), inversion)
    }
}
