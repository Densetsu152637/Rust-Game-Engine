//! Rivers: a Catmull-Rom centreline with width, depth and a mass-conserving flow field.
//!
//! The centreline passes through the authored points (uniform Catmull-Rom, end points
//! repeated) and is sampled into `subdivisions` straight pieces per span. Width and depth are
//! interpolated linearly along each span. The discharge is fixed by the first section,
//! `Q = flow_speed * width_0 * depth_0`, and the depth-averaged speed at every section is
//! `u_mean(s) = Q / (width(s) depth(s))`, so narrowing or shallowing speeds the flow up and the
//! discharge is the same through every section (continuity). Across a section the speed has a
//! parabolic profile `u(n) = 1.5 u_mean (1 - (2 n / width)^2)` (zero at the banks, mean
//! `u_mean`) and points along the 3D centreline tangent.
use crate::{Error, Result};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Most centreline points.
pub const MAX_RIVER_POINTS: usize = 256;

/// Authored river geometry and flow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RiverSettings {
    /// Centreline points (local space; `y` is the surface height there), 2..=256, flowing
    /// from the first to the last.
    pub points: Vec<[f32; 3]>,
    /// One width for the whole river or one per point (m, 0.1..=10000).
    pub widths: Vec<f32>,
    /// One depth or one per point (m, 0.01..=1000).
    pub depths: Vec<f32>,
    /// Depth-averaged speed at the first point (m/s, 0..=50); sets the discharge.
    pub flow_speed: f32,
    /// Straight pieces per span (1..=64).
    pub subdivisions: u32,
}
impl Default for RiverSettings {
    fn default() -> Self {
        Self {
            points: vec![[0.0, 0.0, 0.0], [20.0, 0.0, 0.0]],
            widths: vec![4.0],
            depths: vec![1.0],
            flow_speed: 1.0,
            subdivisions: 8,
        }
    }
}
impl RiverSettings {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("water.river.invalid", m.to_string()));
        let n = self.points.len();
        if !(2..=MAX_RIVER_POINTS).contains(&n) {
            return bad("a river needs 2..=256 centreline points");
        }
        if self
            .points
            .iter()
            .any(|p| p.iter().any(|v| !v.is_finite() || v.abs() > 1.0e6))
        {
            return bad("river points must be finite and within 1e6 m");
        }
        for pair in self.points.windows(2) {
            let d = Vec2::new(pair[1][0] - pair[0][0], pair[1][2] - pair[0][2]);
            if d.length() < 1.0e-3 {
                return bad("consecutive river points must be at least 1 mm apart horizontally");
            }
        }
        let per_point = |values: &[f32], lo: f32, hi: f32| {
            (values.len() == 1 || values.len() == n)
                && values
                    .iter()
                    .all(|v| v.is_finite() && (lo..=hi).contains(v))
        };
        if !per_point(&self.widths, 0.1, 10_000.0) {
            return bad("river widths need 1 or one-per-point values within 0.1..=10000 m");
        }
        if !per_point(&self.depths, 0.01, 1000.0) {
            return bad("river depths need 1 or one-per-point values within 0.01..=1000 m");
        }
        if !(self.flow_speed.is_finite() && (0.0..=50.0).contains(&self.flow_speed)) {
            return bad("river flow_speed must be within 0..=50 m/s");
        }
        if !(1..=64).contains(&self.subdivisions) {
            return bad("river subdivisions must be within 1..=64");
        }
        Ok(())
    }
}

/// One sample of the sampled centreline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CentreSample {
    pub position: Vec3,
    /// Unit 3D tangent (flow direction).
    pub tangent: Vec3,
    /// Arc length from the source along the 3D centreline (m).
    pub distance: f32,
    pub width: f32,
    pub depth: f32,
}

/// Where a point lies relative to the river.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RiverPoint {
    /// Arc length of the closest centreline point.
    pub distance: f32,
    /// Signed horizontal offset from the centreline along `perp(tangent)` (`(-t.z, t.x)`).
    pub offset: f32,
    /// Surface height (the interpolated centreline height).
    pub height: f32,
    pub width: f32,
    pub depth: f32,
    /// Unit 3D flow direction.
    pub tangent: Vec3,
    /// Flow velocity at the point (zero outside the banks).
    pub velocity: Vec3,
    /// Whether `|offset| <= width / 2`.
    pub inside: bool,
}

/// A sampled river.
#[derive(Clone, Debug, PartialEq)]
pub struct RiverSpline {
    samples: Vec<CentreSample>,
    discharge: f32,
}

fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let (t2, t3) = (t * t, t * t * t);
    0.5 * ((2.0 * p1)
        + (p2 - p0) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t3)
}

impl RiverSpline {
    pub fn new(settings: &RiverSettings) -> Result<Self> {
        settings.validate()?;
        let points: Vec<Vec3> = settings.points.iter().map(|p| Vec3::from(*p)).collect();
        let n = points.len();
        let value = |values: &[f32], i: usize| values[if values.len() == 1 { 0 } else { i }];
        let steps = settings.subdivisions as usize;
        let mut raw: Vec<(Vec3, f32, f32)> = Vec::with_capacity((n - 1) * steps + 1);
        for i in 0..n - 1 {
            let p0 = points[i.saturating_sub(1)];
            let (p1, p2) = (points[i], points[i + 1]);
            let p3 = points[(i + 2).min(n - 1)];
            for s in 0..steps {
                let t = s as f32 / steps as f32;
                let width = value(&settings.widths, i)
                    + (value(&settings.widths, i + 1) - value(&settings.widths, i)) * t;
                let depth = value(&settings.depths, i)
                    + (value(&settings.depths, i + 1) - value(&settings.depths, i)) * t;
                raw.push((catmull_rom(p0, p1, p2, p3, t), width, depth));
            }
        }
        raw.push((
            points[n - 1],
            value(&settings.widths, n - 1),
            value(&settings.depths, n - 1),
        ));
        let mut samples = Vec::with_capacity(raw.len());
        let mut distance = 0.0;
        for i in 0..raw.len() {
            if i > 0 {
                distance += raw[i].0.distance(raw[i - 1].0);
            }
            let (a, b) = if i + 1 < raw.len() {
                (raw[i].0, raw[i + 1].0)
            } else {
                (raw[i - 1].0, raw[i].0)
            };
            samples.push(CentreSample {
                position: raw[i].0,
                tangent: (b - a).try_normalize().unwrap_or(Vec3::X),
                distance,
                width: raw[i].1,
                depth: raw[i].2,
            });
        }
        let first = samples[0];
        Ok(Self {
            samples,
            discharge: settings.flow_speed * first.width * first.depth,
        })
    }
    pub fn samples(&self) -> &[CentreSample] {
        &self.samples
    }
    /// Volume flow rate (m^3/s), the same through every section.
    pub fn discharge(&self) -> f32 {
        self.discharge
    }
    pub fn length(&self) -> f32 {
        self.samples.last().map_or(0.0, |s| s.distance)
    }
    /// Depth-averaged speed of a section of `width` and `depth`.
    pub fn mean_speed(&self, width: f32, depth: f32) -> f32 {
        self.discharge / (width * depth).max(1.0e-6)
    }
    /// Flow speed at lateral `offset` of a section.
    pub fn speed_at(&self, width: f32, depth: f32, offset: f32) -> f32 {
        let x = 2.0 * offset / width;
        (1.5 * self.mean_speed(width, depth) * (1.0 - x * x)).max(0.0)
    }
    /// Horizontal bounds `(min, max)` of the banks.
    pub fn bounds(&self) -> (Vec2, Vec2) {
        let mut lo = Vec2::splat(f32::INFINITY);
        let mut hi = Vec2::splat(f32::NEG_INFINITY);
        for s in &self.samples {
            let p = Vec2::new(s.position.x, s.position.z);
            lo = lo.min(p - s.width * 0.5);
            hi = hi.max(p + s.width * 0.5);
        }
        (lo, hi)
    }

    /// The river relative to horizontal point `p` (closest centreline piece).
    pub fn locate(&self, p: Vec2) -> RiverPoint {
        let mut best = (f32::INFINITY, 0usize, 0.0f32);
        for i in 0..self.samples.len() - 1 {
            let a = Vec2::new(self.samples[i].position.x, self.samples[i].position.z);
            let b = Vec2::new(
                self.samples[i + 1].position.x,
                self.samples[i + 1].position.z,
            );
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_squared().max(1.0e-12)).clamp(0.0, 1.0);
            let d = p.distance_squared(a + ab * t);
            if d < best.0 {
                best = (d, i, t);
            }
        }
        let (_, i, t) = best;
        let (a, b) = (self.samples[i], self.samples[i + 1]);
        let position = a.position.lerp(b.position, t);
        let tangent = (b.position - a.position)
            .try_normalize()
            .unwrap_or(a.tangent);
        let horizontal = Vec2::new(tangent.x, tangent.z)
            .try_normalize()
            .unwrap_or(Vec2::X);
        let offset = (p - Vec2::new(position.x, position.z)).dot(horizontal.perp());
        let width = a.width + (b.width - a.width) * t;
        let depth = a.depth + (b.depth - a.depth) * t;
        let inside = offset.abs() <= width * 0.5;
        let velocity = if inside {
            tangent * self.speed_at(width, depth, offset)
        } else {
            Vec3::ZERO
        };
        RiverPoint {
            distance: a.distance + (b.distance - a.distance) * t,
            offset,
            height: position.y,
            width,
            depth,
            tangent,
            velocity,
            inside,
        }
    }

    /// Ribbon surface mesh: `across + 1` vertices per centreline sample (bank to bank), with
    /// UVs `(arc length, 0..1 across)`, and triangle indices (counter-clockwise from above).
    pub fn ribbon(&self, across: u32) -> (Vec<Vec3>, Vec<[f32; 2]>, Vec<u32>) {
        let across = across.max(1);
        let row = across + 1;
        let mut positions = Vec::with_capacity(self.samples.len() * row as usize);
        let mut uvs = Vec::with_capacity(positions.capacity());
        for s in &self.samples {
            let horizontal = Vec2::new(s.tangent.x, s.tangent.z)
                .try_normalize()
                .unwrap_or(Vec2::X)
                .perp();
            for j in 0..row {
                let v = j as f32 / across as f32;
                let o = (v - 0.5) * s.width;
                positions.push(s.position + Vec3::new(horizontal.x * o, 0.0, horizontal.y * o));
                uvs.push([s.distance, v]);
            }
        }
        let mut indices = Vec::new();
        for i in 0..self.samples.len() as u32 - 1 {
            for j in 0..across {
                let a = i * row + j;
                let (b, c, d) = (a + 1, a + row, a + row + 1);
                indices.extend_from_slice(&[a, b, c, b, d, c]);
            }
        }
        (positions, uvs, indices)
    }
}
