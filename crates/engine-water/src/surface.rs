//! Water bodies as queryable surfaces: shape (ocean, lake, river), waves, optics, underwater
//! state and shoreline attenuation.
//!
//! All positions are in the water body's local frame: `y = 0` is the mean surface (rivers:
//! the centreline height), `+Y` is up. Queries take the local horizontal position `p = (x, z)`.
use crate::gerstner::GerstnerSet;
use crate::ocean::OceanFrame;
use crate::river::RiverSpline;
use crate::{Error, Result};
use glam::{Mat2, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Colours, absorption, fog and foam of a water body. Colours are linear RGB.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaterOptics {
    /// Scattered colour seen through thin water.
    pub shallow_color: [f32; 3],
    /// Scattered colour of deep water.
    pub deep_color: [f32; 3],
    /// Beer-Lambert extinction per metre for red, green and blue (0..=100).
    pub absorption: [f32; 3],
    /// Underwater fog colour (linear RGB).
    pub fog_color: [f32; 3],
    /// Underwater fog density per metre (0..=10): `fog = 1 - exp(-density * distance)`.
    pub fog_density: f32,
    pub foam_color: [f32; 3],
    /// Crest foam starts where the horizontal Jacobian determinant drops below this (0..=1).
    pub foam_threshold: f32,
    /// Foam brightness multiplier (0..=4).
    pub foam_intensity: f32,
    /// Surface roughness for the sun highlight (0.01..=1).
    pub roughness: f32,
    /// Screen-space refraction offset scale (0..=1).
    pub refraction: f32,
}
impl Default for WaterOptics {
    fn default() -> Self {
        Self {
            shallow_color: [0.10, 0.45, 0.45],
            deep_color: [0.01, 0.06, 0.12],
            absorption: [0.45, 0.09, 0.06],
            fog_color: [0.02, 0.12, 0.16],
            fog_density: 0.15,
            foam_color: [0.9, 0.95, 1.0],
            foam_threshold: 0.4,
            foam_intensity: 1.0,
            roughness: 0.08,
            refraction: 0.3,
        }
    }
}
fn unit_color(c: &[f32; 3]) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
impl WaterOptics {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("water.optics.invalid", m.to_string()));
        if ![
            &self.shallow_color,
            &self.deep_color,
            &self.fog_color,
            &self.foam_color,
        ]
        .into_iter()
        .all(unit_color)
        {
            return bad("water colours must have components within 0..=1");
        }
        if !self
            .absorption
            .iter()
            .all(|v| v.is_finite() && (0.0..=100.0).contains(v))
        {
            return bad("water absorption must be within 0..=100 per metre");
        }
        let checks = [
            (self.fog_density, 0.0, 10.0, "fog_density within 0..=10"),
            (self.foam_threshold, 0.0, 1.0, "foam_threshold within 0..=1"),
            (self.foam_intensity, 0.0, 4.0, "foam_intensity within 0..=4"),
            (self.roughness, 0.01, 1.0, "roughness within 0.01..=1"),
            (self.refraction, 0.0, 1.0, "refraction within 0..=1"),
        ];
        for (v, lo, hi, what) in checks {
            if !(v.is_finite() && (lo..=hi).contains(&v)) {
                return bad(&format!("water {what}"));
            }
        }
        Ok(())
    }
    /// Crest foam from the horizontal Jacobian determinant `det`:
    /// `clamp((threshold - det) / threshold, 0, 1)`.
    pub fn crest_foam(&self, det: f32) -> f32 {
        if self.foam_threshold <= 0.0 {
            return f32::from(u8::from(det <= 0.0));
        }
        ((self.foam_threshold - det) / self.foam_threshold).clamp(0.0, 1.0)
    }
    /// Light transmitted through `distance` metres of water, per channel.
    pub fn transmittance(&self, distance: f32) -> [f32; 3] {
        self.absorption.map(|a| (-a * distance.max(0.0)).exp())
    }
}

/// Shoreline attenuation from the water depth over the ground.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShorelineSettings {
    pub enabled: bool,
    /// Depth (m) at which waves reach full amplitude (0.01..=1000).
    pub fade_depth: f32,
    /// Depth (m) below which shoreline foam appears (0.01..=100).
    pub foam_depth: f32,
    /// Amplitude factor at zero depth (0..=1).
    pub min_amplitude: f32,
}
impl Default for ShorelineSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            fade_depth: 4.0,
            foam_depth: 0.75,
            min_amplitude: 0.0,
        }
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Wave amplitude factor and shoreline foam at one point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attenuation {
    /// Factor on every wave amplitude (and horizontal displacement), 0..=1.
    pub amplitude: f32,
    /// Shoreline foam coverage, 0..=1.
    pub foam: f32,
}
impl Attenuation {
    pub const NONE: Attenuation = Attenuation {
        amplitude: 1.0,
        foam: 0.0,
    };
}

impl ShorelineSettings {
    pub fn validate(&self) -> Result<()> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !ok(self.fade_depth, 0.01, 1000.0)
            || !ok(self.foam_depth, 0.01, 100.0)
            || !ok(self.min_amplitude, 0.0, 1.0)
        {
            return Err(Error::new(
                "water.shoreline.invalid",
                "shoreline fade_depth 0.01..=1000, foam_depth 0.01..=100, min_amplitude 0..=1",
            ));
        }
        Ok(())
    }
    /// Attenuation at `water_depth` metres of water above the ground (`None`: no ground there,
    /// open water). `amplitude = min + (1 - min) smoothstep(0, fade_depth, depth)` and
    /// `foam = 1 - smoothstep(0, foam_depth, depth)`.
    pub fn attenuation(&self, water_depth: Option<f32>) -> Attenuation {
        let Some(depth) = water_depth.filter(|_| self.enabled) else {
            return Attenuation::NONE;
        };
        Attenuation {
            amplitude: self.min_amplitude
                + (1.0 - self.min_amplitude) * smoothstep(0.0, self.fade_depth, depth),
            foam: 1.0 - smoothstep(0.0, self.foam_depth, depth),
        }
    }
}

/// Ground height under a horizontal position, used for shoreline attenuation. Implemented for
/// closures `Fn(f32, f32) -> Option<f32>` (`None`: no ground known there).
pub trait GroundHeight {
    fn ground_height(&self, x: f32, z: f32) -> Option<f32>;
}
impl<F: Fn(f32, f32) -> Option<f32>> GroundHeight for F {
    fn ground_height(&self, x: f32, z: f32) -> Option<f32> {
        self(x, z)
    }
}
/// No ground anywhere (open water).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGround;
impl GroundHeight for NoGround {
    fn ground_height(&self, _x: f32, _z: f32) -> Option<f32> {
        None
    }
}

/// How the surface moves.
#[derive(Clone, Debug, PartialEq)]
pub enum Waves {
    /// A flat surface.
    Flat,
    Gerstner(Arc<GerstnerSet>),
    /// The current frame of an FFT ocean patch (tiled).
    Ocean(Arc<OceanFrame>),
}

/// Where the water is.
#[derive(Clone, Debug, PartialEq)]
pub enum WaterShape {
    /// Unbounded for queries; `half_extent` only sizes the rendered grid.
    Ocean {
        half_extent: Vec2,
    },
    /// `|x| <= half_extent.x`, `|z| <= half_extent.y`.
    Lake {
        half_extent: Vec2,
    },
    River(Arc<RiverSpline>),
}

/// The surface above one horizontal position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceSample {
    /// Surface height (local y).
    pub height: f32,
    pub normal: Vec3,
    /// Water velocity at the surface: river flow plus wave orbital velocity (Gerstner only).
    pub velocity: Vec3,
    /// Crest foam (0..=1) from the displacement Jacobian.
    pub crest_foam: f32,
    /// Horizontal Jacobian determinant at the rest point.
    pub determinant: f32,
    /// Residual of the displacement inversion (m) and whether it converged.
    pub residual: f32,
    pub converged: bool,
    /// The shoreline attenuation applied.
    pub attenuation: Attenuation,
}

/// Underwater state of a point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Underwater {
    /// Metres below the surface (positive).
    pub depth: f32,
    pub fog_color: [f32; 3],
    pub fog_density: f32,
    /// Sunlight transmitted from the surface down to the point, per channel.
    pub transmittance: [f32; 3],
}
impl Underwater {
    /// Fog coverage over a view distance: `1 - exp(-density * distance)`.
    pub fn fog_factor(&self, distance: f32) -> f32 {
        1.0 - (-self.fog_density * distance.max(0.0)).exp()
    }
}

/// A water body's surface at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct WaterSurface {
    pub shape: WaterShape,
    pub waves: Waves,
    pub optics: WaterOptics,
    pub shoreline: ShorelineSettings,
    /// Seconds of wave time.
    pub time: f32,
}
impl WaterSurface {
    /// Whether horizontal `p` lies inside the body (rivers: within the banks).
    pub fn contains(&self, p: Vec2) -> bool {
        match &self.shape {
            WaterShape::Ocean { .. } => true,
            WaterShape::Lake { half_extent } => {
                p.x.abs() <= half_extent.x && p.y.abs() <= half_extent.y
            }
            WaterShape::River(river) => river.locate(p).inside,
        }
    }

    /// The surface above `p`, or `None` outside the body. `ground` is the ground height in
    /// the same local frame (shoreline attenuation uses `depth = mean level - ground`).
    pub fn sample(&self, p: Vec2, ground: &impl GroundHeight) -> Option<SurfaceSample> {
        let (base, flow) = match &self.shape {
            WaterShape::River(river) => {
                let at = river.locate(p);
                if !at.inside {
                    return None;
                }
                (at.height, at.velocity)
            }
            _ if !self.contains(p) => return None,
            _ => (0.0, Vec3::ZERO),
        };
        let attenuation = self
            .shoreline
            .attenuation(ground.ground_height(p.x, p.y).map(|g| base - g));
        let scale = attenuation.amplitude;
        let mut sample = SurfaceSample {
            height: base,
            normal: Vec3::Y,
            velocity: flow,
            crest_foam: 0.0,
            determinant: 1.0,
            residual: 0.0,
            converged: true,
            attenuation,
        };
        match &self.waves {
            Waves::Flat => {}
            Waves::Gerstner(set) => {
                let (point, inversion) = set.sample(p, self.time, scale);
                let j = Mat2::IDENTITY + point.jacobian;
                sample.height += point.offset.y;
                sample.normal = point.normal();
                sample.velocity += point.velocity;
                sample.determinant = j.determinant();
                sample.residual = inversion.residual;
                sample.converged = inversion.converged;
            }
            Waves::Ocean(frame) => {
                // The patch is evaluated at full amplitude; attenuation scales the result.
                let (point, inversion) = frame.sample(p);
                sample.height += point.offset.y * scale;
                sample.normal = Vec3::Y.lerp(point.normal, scale).normalize_or(Vec3::Y);
                sample.determinant = 1.0 + (point.determinant - 1.0) * scale;
                sample.residual = inversion.residual;
                sample.converged = inversion.converged;
            }
        }
        sample.crest_foam = self.optics.crest_foam(sample.determinant);
        Some(sample)
    }

    /// Underwater state of local point `point`, or `None` when it is above the surface or
    /// outside the body.
    pub fn underwater(&self, point: Vec3, ground: &impl GroundHeight) -> Option<Underwater> {
        let sample = self.sample(Vec2::new(point.x, point.z), ground)?;
        let depth = sample.height - point.y;
        (depth > 0.0).then(|| Underwater {
            depth,
            fog_color: self.optics.fog_color,
            fog_density: self.optics.fog_density,
            transmittance: self.optics.transmittance(depth),
        })
    }
}
