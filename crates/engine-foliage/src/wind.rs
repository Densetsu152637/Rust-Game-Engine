//! Wind: a parameter model and the CPU evaluation of the sway offset the GPU shader repeats.
//!
//! The model is a horizontal displacement of a vertex given the time, the instance's world
//! position and phase, and a bend factor (0 at the plant's base, growing toward the top):
//!
//! ```text
//! dir      = (cos a, sin a)                      // wind direction on the XZ plane
//! perp     = (-dir.y, dir.x)
//! along    = dot(dir, position.xz) / wavelength
//! gust     = sin(2 pi (gust_frequency * t - 0.25 * along))
//! amp      = strength * (1 + gust_strength * gust)
//! w1       = sin(2 pi (sway_frequency * t + phase + along))
//! w2       = sin(2 pi (2 * sway_frequency * t + 2 * phase + along))
//! offset   = bend * amp * (dir * (0.6 + 0.4 * w1) + perp * (0.5 * flutter * w2))
//! ```
//!
//! The offset is horizontal (y = 0), at most [`WindParams::max_offset`] long, and periodic in
//! `t` with period `1 / sway_frequency` when `gust_strength` is 0 (with gusts, the least
//! common period of both frequencies). `foliage.wgsl` in `engine-world-render` implements the
//! same expression from [`WindParams::gpu_block`]; both use `f32`.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Wind settings of a foliage set.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindParams {
    /// Direction the wind blows toward, degrees on the XZ plane: 0 is +X, 90 is +Z.
    pub direction_degrees: f32,
    /// Lean in metres of a vertex with bend factor 1 (0 disables the wind).
    pub strength: f32,
    /// Sway frequency in Hz (0..=20).
    pub sway_frequency: f32,
    /// Gust frequency in Hz (0..=5).
    pub gust_frequency: f32,
    /// 0..=1: how much gusts scale the strength.
    pub gust_strength: f32,
    /// 0..=1: sideways flutter relative to the lean.
    pub flutter: f32,
    /// Metres per wave: how fast the phase changes across the ground (1..=1000).
    pub wavelength: f32,
}

impl Default for WindParams {
    fn default() -> Self {
        Self {
            direction_degrees: 0.0,
            strength: 0.0,
            sway_frequency: 0.5,
            gust_frequency: 0.1,
            gust_strength: 0.5,
            flutter: 0.5,
            wavelength: 24.0,
        }
    }
}

/// Uniform block the shader reads: two `vec4`s.
/// `[0]` = (direction x, direction z, strength, sway frequency),
/// `[1]` = (gust frequency, gust strength, flutter, 1 / wavelength); `time` goes separately.
pub type WindBlock = [[f32; 4]; 2];

const TAU: f32 = std::f32::consts::TAU;

impl WindParams {
    /// Checks every field.
    pub fn validate(&self) -> Result<()> {
        let ok = [
            (self.direction_degrees, -3600.0, 3600.0),
            (self.strength, 0.0, 100.0),
            (self.sway_frequency, 0.0, 20.0),
            (self.gust_frequency, 0.0, 5.0),
            (self.gust_strength, 0.0, 1.0),
            (self.flutter, 0.0, 1.0),
            (self.wavelength, 1.0, 1000.0),
        ]
        .iter()
        .all(|(v, lo, hi)| v.is_finite() && (lo..=hi).contains(&v));
        if ok {
            Ok(())
        } else {
            Err(Error::new(
                "foliage.wind.invalid",
                "wind needs direction within +-3600, strength 0..=100, sway 0..=20 Hz, gust 0..=5 Hz, gust_strength and flutter 0..=1, wavelength 1..=1000",
            ))
        }
    }

    /// Whether the wind moves anything.
    pub fn active(&self) -> bool {
        self.strength > 0.0
    }

    /// Unit wind direction `(x, z)`.
    pub fn direction(&self) -> [f32; 2] {
        let a = self.direction_degrees.to_radians();
        [a.cos(), a.sin()]
    }

    /// Upper bound of the offset length for bend factor 1:
    /// `strength * (1 + gust_strength) * sqrt(1 + (0.5 * flutter)^2)`.
    pub fn max_offset(&self) -> f32 {
        self.strength * (1.0 + self.gust_strength) * (1.0 + (0.5 * self.flutter).powi(2)).sqrt()
    }

    /// The horizontal sway offset `[x, 0, z]` of a vertex.
    ///
    /// `position` is the instance's world position, `phase` its stored phase (0..1) and
    /// `bend` the vertex bend factor already multiplied by the type's wind weight.
    pub fn sway_offset(&self, time: f32, position: [f32; 3], phase: f32, bend: f32) -> [f32; 3] {
        let [dx, dz] = self.direction();
        let (px, pz) = (-dz, dx);
        let along = (dx * position[0] + dz * position[2]) / self.wavelength;
        let gust = (TAU * (self.gust_frequency * time - 0.25 * along)).sin();
        let amp = self.strength * (1.0 + self.gust_strength * gust);
        let w1 = (TAU * (self.sway_frequency * time + phase + along)).sin();
        let w2 = (TAU * (2.0 * self.sway_frequency * time + 2.0 * phase + along)).sin();
        let lean = bend * amp * (0.6 + 0.4 * w1);
        let side = bend * amp * 0.5 * self.flutter * w2;
        [lean * dx + side * px, 0.0, lean * dz + side * pz]
    }

    /// The uniform block for the shader.
    pub fn gpu_block(&self) -> WindBlock {
        let [dx, dz] = self.direction();
        [
            [dx, dz, self.strength, self.sway_frequency],
            [
                self.gust_frequency,
                self.gust_strength,
                self.flutter,
                1.0 / self.wavelength,
            ],
        ]
    }
}

/// Bend factor of a vertex at `height` in a mesh spanning `base..base + extent` along Y:
/// 0 at the base, 1 at the top, quadratic so the base stays planted.
pub fn bend_factor(height: f32, base: f32, extent: f32) -> f32 {
    if extent <= 0.0 {
        return 0.0;
    }
    let h = ((height - base) / extent).clamp(0.0, 1.0);
    h * h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wind() -> WindParams {
        WindParams {
            direction_degrees: 30.0,
            strength: 0.4,
            sway_frequency: 0.8,
            gust_frequency: 0.2,
            gust_strength: 0.5,
            flutter: 0.6,
            wavelength: 16.0,
        }
    }

    fn len(v: [f32; 3]) -> f32 {
        (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
    }

    #[test]
    fn offset_is_horizontal_zero_at_the_base_and_bounded_everywhere() {
        let w = wind();
        let bound = w.max_offset();
        let mut peak = 0.0f32;
        for i in 0..4000 {
            let t = i as f32 * 0.037;
            let p = [(i % 17) as f32 * 3.1, 5.0, (i % 29) as f32 * 1.7];
            let phase = (i % 13) as f32 / 13.0;
            let o = w.sway_offset(t, p, phase, 1.0);
            assert_eq!(o[1], 0.0);
            assert!(len(o) <= bound + 1e-5, "{} > {bound}", len(o));
            peak = peak.max(len(o));
            assert_eq!(w.sway_offset(t, p, phase, 0.0), [0.0; 3]);
        }
        // The bound is tight to within 25 %: the model reaches most of it.
        assert!(peak > 0.75 * bound, "peak {peak} of bound {bound}");
    }

    #[test]
    fn offset_scales_linearly_with_the_bend_factor() {
        let w = wind();
        let a = w.sway_offset(1.3, [4.0, 0.0, 9.0], 0.2, 1.0);
        let b = w.sway_offset(1.3, [4.0, 0.0, 9.0], 0.2, 0.25);
        for k in 0..3 {
            assert!((a[k] * 0.25 - b[k]).abs() < 1e-6);
        }
    }

    #[test]
    fn sway_is_periodic_in_time() {
        // No gusts: the period is 1 / sway_frequency.
        let mut w = wind();
        w.gust_strength = 0.0;
        let period = 1.0 / w.sway_frequency;
        for i in 0..50 {
            let t = i as f32 * 0.31;
            let p = [i as f32, 0.0, 2.0 * i as f32];
            let a = w.sway_offset(t, p, 0.37, 1.0);
            let b = w.sway_offset(t + period, p, 0.37, 1.0);
            let c = w.sway_offset(t + 3.0 * period, p, 0.37, 1.0);
            for k in 0..3 {
                assert!((a[k] - b[k]).abs() < 2e-5, "{a:?} {b:?}");
                assert!((a[k] - c[k]).abs() < 1e-4, "{a:?} {c:?}");
            }
        }
        // Gusts at a fifth of the sway frequency: the combined period is 5 / f = 6.25 s.
        let mut g = wind();
        g.gust_frequency = g.sway_frequency / 5.0;
        let combined = 5.0 / g.sway_frequency;
        let a = g.sway_offset(2.2, [3.0, 0.0, 1.0], 0.5, 1.0);
        let b = g.sway_offset(2.2 + combined, [3.0, 0.0, 1.0], 0.5, 1.0);
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() < 1e-4, "{a:?} {b:?}");
        }
        // And it is not constant: a quarter period later it has moved.
        let c = w.sway_offset(period * 0.25, [3.0, 0.0, 1.0], 0.5, 1.0);
        let d = w.sway_offset(0.0, [3.0, 0.0, 1.0], 0.5, 1.0);
        assert!(len([c[0] - d[0], 0.0, c[2] - d[2]]) > 0.01);
    }

    #[test]
    fn neighbours_with_different_phases_move_differently() {
        let w = wind();
        let a = w.sway_offset(0.7, [0.0, 0.0, 0.0], 0.0, 1.0);
        let b = w.sway_offset(0.7, [0.0, 0.0, 0.0], 0.5, 1.0);
        assert!(len([a[0] - b[0], 0.0, a[2] - b[2]]) > 0.05);
    }

    #[test]
    fn zero_strength_never_moves_and_validation_rejects_bad_values() {
        let mut w = wind();
        w.strength = 0.0;
        assert!(!w.active());
        assert_eq!(w.sway_offset(5.0, [1.0, 2.0, 3.0], 0.3, 1.0), [0.0; 3]);
        assert!(WindParams::default().validate().is_ok());
        for bad in [
            WindParams {
                strength: -1.0,
                ..wind()
            },
            WindParams {
                wavelength: 0.0,
                ..wind()
            },
            WindParams {
                gust_strength: 1.5,
                ..wind()
            },
            WindParams {
                sway_frequency: f32::NAN,
                ..wind()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn bend_is_zero_at_the_base_and_one_at_the_top() {
        assert_eq!(bend_factor(0.0, 0.0, 2.0), 0.0);
        assert_eq!(bend_factor(2.0, 0.0, 2.0), 1.0);
        assert_eq!(bend_factor(1.0, 0.0, 2.0), 0.25);
        assert_eq!(bend_factor(1.0, 0.0, 0.0), 0.0);
        assert_eq!(bend_factor(9.0, 0.0, 2.0), 1.0);
    }

    #[test]
    fn gpu_block_carries_the_parameters() {
        let b = wind().gpu_block();
        assert!((b[0][0].hypot(b[0][1]) - 1.0).abs() < 1e-6);
        assert_eq!(b[0][2], 0.4);
        assert_eq!(b[1][3], 1.0 / 16.0);
    }
}
