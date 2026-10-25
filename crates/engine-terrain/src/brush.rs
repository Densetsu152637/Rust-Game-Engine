//! Deterministic sculpt and paint brushes. Every operation visits samples (or cells) in row-major
//! order with plain `f32` arithmetic, so the same inputs always give the same field, and
//! returns the rectangle it touched so callers rebuild only affected chunks and colliders.
use crate::heightfield::{Heightfield, MAX_ABS_HEIGHT, SampleRect, WEIGHT_SUM};
use crate::noise::fbm;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Largest brush radius in local units.
pub const MAX_RADIUS: f32 = 100_000.0;

fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}

/// A circular brush in terrain-local units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brush {
    /// Centre `[x, z]` in terrain-local coordinates.
    pub center: [f32; 2],
    /// Radius in local units, (0, 100000].
    pub radius: f32,
    /// Operation strength (see [`SculptOp`]); 0..=1 for blends, metres for raise/lower/noise.
    #[serde(default = "one")]
    pub strength: f32,
    /// Fraction of the radius over which the weight falls from 1 to 0 (smoothstep); 0..=1.
    #[serde(default = "half")]
    pub falloff: f32,
}
impl Brush {
    /// A brush with strength 1 and falloff 0.5.
    pub fn new(center: [f32; 2], radius: f32) -> Self {
        Self {
            center,
            radius,
            strength: 1.0,
            falloff: 0.5,
        }
    }
    /// Validates centre, radius, strength (0..=10000) and falloff.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("terrain.brush.invalid", m.to_string()));
        if !self
            .center
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1.0e7)
        {
            return bad("brush centre must be finite");
        }
        if !(self.radius.is_finite() && self.radius > 0.0 && self.radius <= MAX_RADIUS) {
            return bad("brush radius must be within (0, 100000]");
        }
        if !(self.strength.is_finite() && (0.0..=10_000.0).contains(&self.strength)) {
            return bad("brush strength must be within 0..=10000");
        }
        if !(self.falloff.is_finite() && (0.0..=1.0).contains(&self.falloff)) {
            return bad("brush falloff must be within 0..=1");
        }
        Ok(())
    }
    /// Brush weight (0..=1) at local `(x, z)`: 1 inside `radius * (1 - falloff)`, smoothstep
    /// down to 0 at `radius`.
    pub fn weight(&self, x: f32, z: f32) -> f32 {
        let d = ((x - self.center[0]).powi(2) + (z - self.center[1]).powi(2)).sqrt();
        if d >= self.radius {
            return 0.0;
        }
        let inner = self.radius * (1.0 - self.falloff);
        if d <= inner {
            return 1.0;
        }
        let t = (self.radius - d) / (self.radius - inner);
        t * t * (3.0 - 2.0 * t)
    }
    /// Samples whose position may lie inside the brush, clamped to the grid.
    pub fn sample_rect(&self, field: &Heightfield) -> SampleRect {
        let s = field.spacing();
        let lo = |v: f32| ((v - self.radius) / s).floor().max(0.0);
        let hi = |v: f32, n: u32| (((v + self.radius) / s).ceil() + 1.0).clamp(0.0, n as f32);
        SampleRect::new(
            [lo(self.center[0]) as u32, lo(self.center[1]) as u32],
            [
                hi(self.center[0], field.columns()) as u32,
                hi(self.center[1], field.rows()) as u32,
            ],
        )
    }
}

/// A sculpt operation applied with a [`Brush`] of weight `w` at each sample.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SculptOp {
    /// `h += strength * w` (strength in local units).
    Raise,
    /// `h -= strength * w`.
    Lower,
    /// Blend toward the 3x3 mean of the original heights by `clamp(strength * w, 0, 1)`.
    Smooth,
    /// Blend toward `height` (default: the surface height at the brush centre) by
    /// `clamp(strength * w, 0, 1)`.
    Flatten {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f32>,
    },
    /// Blend toward `height` by `w` (strength ignored): a hard set inside the falloff.
    SetHeight { height: f32 },
    /// `h += strength * w * fbm(x * frequency, z * frequency, seed)`; frequency in (0, 100].
    Noise { seed: u32, frequency: f32 },
}

/// What an edit changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditReport {
    /// Bounding rectangle of the changed samples (cells for hole edits), `None` if nothing
    /// changed.
    pub region: Option<SampleRect>,
    /// Number of samples (or cells) whose value changed.
    pub changed: usize,
}
impl EditReport {
    /// Combines two reports of edits to the same field.
    pub fn merge(self, other: EditReport) -> EditReport {
        let region = match (self.region, other.region) {
            (Some(a), Some(b)) => Some(a.union(b)),
            (a, b) => a.or(b),
        };
        EditReport {
            region,
            changed: self.changed + other.changed,
        }
    }
}

struct Tracker {
    min: [u32; 2],
    max: [u32; 2],
    changed: usize,
}
impl Tracker {
    fn new() -> Self {
        Self {
            min: [u32::MAX; 2],
            max: [0; 2],
            changed: 0,
        }
    }
    fn hit(&mut self, c: u32, r: u32) {
        self.changed += 1;
        self.min = [self.min[0].min(c), self.min[1].min(r)];
        self.max = [self.max[0].max(c + 1), self.max[1].max(r + 1)];
    }
    fn report(self) -> EditReport {
        EditReport {
            region: (self.changed > 0).then(|| SampleRect::new(self.min, self.max)),
            changed: self.changed,
        }
    }
}

/// `h` moved toward `target` by `t`, landing exactly on `target` at `t = 1`.
fn toward(h: f32, target: f32, t: f32) -> f32 {
    if t >= 1.0 {
        target
    } else {
        h + t * (target - h)
    }
}

/// Applies `op` with `brush`; returns the changed samples.
pub fn sculpt(field: &mut Heightfield, brush: &Brush, op: SculptOp) -> Result<EditReport> {
    brush.validate()?;
    let blend = matches!(op, SculptOp::Smooth | SculptOp::Flatten { .. });
    if blend && brush.strength > 1.0 {
        return Err(Error::new(
            "terrain.brush.invalid",
            "smooth and flatten strength must be within 0..=1",
        ));
    }
    let target = match op {
        SculptOp::Flatten { height } => {
            Some(height.unwrap_or_else(|| field.height_at(brush.center[0], brush.center[1])))
        }
        SculptOp::SetHeight { height } => Some(height),
        _ => None,
    };
    if target.is_some_and(|t| !(t.is_finite() && t.abs() <= MAX_ABS_HEIGHT)) {
        return Err(Error::new(
            "terrain.height",
            "target height must be finite within the height limit",
        ));
    }
    if let SculptOp::Noise { frequency, .. } = op
        && !(frequency.is_finite() && frequency > 0.0 && frequency <= 100.0)
    {
        return Err(Error::new(
            "terrain.brush.invalid",
            "noise frequency must be within (0, 100]",
        ));
    }
    let rect = brush.sample_rect(field);
    let s = field.spacing();
    let columns = field.columns() as usize;
    // Smoothing reads the original neighbourhood, not partially smoothed values.
    let original = matches!(op, SculptOp::Smooth).then(|| field.heights().to_vec());
    let (rows, cols) = (field.rows(), field.columns());
    let heights = field.heights_mut();
    let mut tracker = Tracker::new();
    for r in rect.min[1]..rect.max[1] {
        for c in rect.min[0]..rect.max[0] {
            let (x, z) = (c as f32 * s, r as f32 * s);
            let w = brush.weight(x, z);
            if w <= 0.0 {
                continue;
            }
            let index = r as usize * columns + c as usize;
            let h = heights[index];
            let next = match op {
                SculptOp::Raise => h + brush.strength * w,
                SculptOp::Lower => h - brush.strength * w,
                SculptOp::Smooth => {
                    let source = original.as_ref().expect("copied for smooth");
                    let mut sum = 0.0;
                    for dr in -1i64..=1 {
                        for dc in -1i64..=1 {
                            let cc = (c as i64 + dc).clamp(0, cols as i64 - 1) as usize;
                            let rr = (r as i64 + dr).clamp(0, rows as i64 - 1) as usize;
                            sum += source[rr * columns + cc];
                        }
                    }
                    h + (brush.strength * w).clamp(0.0, 1.0) * (sum / 9.0 - h)
                }
                SculptOp::Flatten { .. } => toward(
                    h,
                    target.expect("set"),
                    (brush.strength * w).clamp(0.0, 1.0),
                ),
                SculptOp::SetHeight { .. } => toward(h, target.expect("set"), w),
                SculptOp::Noise { seed, frequency } => {
                    h + brush.strength * w * fbm(x * frequency, z * frequency, seed)
                }
            }
            .clamp(-MAX_ABS_HEIGHT, MAX_ABS_HEIGHT);
            if next != h {
                heights[index] = next;
                tracker.hit(c, r);
            }
        }
    }
    let report = tracker.report();
    if report.changed > 0 {
        field.touch();
    }
    Ok(report)
}

/// Quantizes normalized weights to bytes summing exactly to 255 (largest remainders first,
/// ties to the lower layer index).
pub fn quantize_weights(weights: &[f32], out: &mut [u8]) {
    let scaled: Vec<f32> = weights
        .iter()
        .map(|w| (w.clamp(0.0, 1.0) * WEIGHT_SUM as f32).min(WEIGHT_SUM as f32))
        .collect();
    let mut q: Vec<u32> = scaled.iter().map(|v| v.floor() as u32).collect();
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| {
        let fa = scaled[a] - q[a] as f32;
        let fb = scaled[b] - q[b] as f32;
        fb.total_cmp(&fa).then(a.cmp(&b))
    });
    let mut sum: u32 = q.iter().sum();
    let mut i = 0;
    while sum < WEIGHT_SUM {
        let k = order[i % order.len()];
        if q[k] < WEIGHT_SUM {
            q[k] += 1;
            sum += 1;
        }
        i += 1;
    }
    i = order.len();
    while sum > WEIGHT_SUM {
        i -= 1;
        let k = order[i % order.len()];
        if q[k] > 0 {
            q[k] -= 1;
            sum -= 1;
        }
        if i == 0 {
            i = order.len();
        }
    }
    for (o, v) in out.iter_mut().zip(q) {
        *o = v as u8;
    }
}

/// Paints splat layer `layer`: at each sample its weight moves toward 1 by
/// `clamp(strength * w, 0, 1)` and the other layers shrink proportionally; weights stay
/// normalized (bytes summing to 255).
pub fn paint_layer(field: &mut Heightfield, brush: &Brush, layer: usize) -> Result<EditReport> {
    brush.validate()?;
    let n = field.layers().len();
    if layer >= n {
        return Err(Error::new(
            "terrain.layer.unknown",
            format!("layer index {layer} out of 0..{n}"),
        ));
    }
    if brush.strength > 1.0 {
        return Err(Error::new(
            "terrain.brush.invalid",
            "paint strength must be within 0..=1",
        ));
    }
    let rect = brush.sample_rect(field);
    let s = field.spacing();
    let columns = field.columns() as usize;
    let splat = field.splat_mut();
    let mut tracker = Tracker::new();
    let mut weights = vec![0.0f32; n];
    let mut bytes = vec![0u8; n];
    for r in rect.min[1]..rect.max[1] {
        for c in rect.min[0]..rect.max[0] {
            let w = brush.weight(c as f32 * s, r as f32 * s);
            let a = (brush.strength * w).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let at = (r as usize * columns + c as usize) * n;
            let current = &splat[at..at + n];
            let old = current[layer] as f32 / WEIGHT_SUM as f32;
            let new = old + a * (1.0 - old);
            let scale = if old < 1.0 {
                (1.0 - new) / (1.0 - old)
            } else {
                0.0
            };
            for (i, value) in weights.iter_mut().enumerate() {
                *value = if i == layer {
                    new
                } else {
                    current[i] as f32 / WEIGHT_SUM as f32 * scale
                };
            }
            quantize_weights(&weights, &mut bytes);
            if bytes[..] != splat[at..at + n] {
                splat[at..at + n].copy_from_slice(&bytes);
                tracker.hit(c, r);
            }
        }
    }
    let report = tracker.report();
    if report.changed > 0 {
        field.bump();
    }
    Ok(report)
}

/// Marks (`hole = true`) or clears the cells whose centre lies within the brush radius;
/// strength and falloff are ignored. The report's region is in cell coordinates.
pub fn paint_holes(field: &mut Heightfield, brush: &Brush, hole: bool) -> Result<EditReport> {
    brush.validate()?;
    let rect = brush.sample_rect(field);
    let s = field.spacing();
    let mut tracker = Tracker::new();
    let max_c = rect.max[0].min(field.columns() - 1);
    let max_r = rect.max[1].min(field.rows() - 1);
    for r in rect.min[1]..max_r {
        for c in rect.min[0]..max_c {
            let (x, z) = ((c as f32 + 0.5) * s, (r as f32 + 0.5) * s);
            let d2 = (x - brush.center[0]).powi(2) + (z - brush.center[1]).powi(2);
            if d2 <= brush.radius * brush.radius && field.set_hole_unchecked(c, r, hole) {
                tracker.hit(c, r);
            }
        }
    }
    let report = tracker.report();
    if report.changed > 0 {
        field.bump();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightfield::TerrainLayer;

    fn field() -> Heightfield {
        Heightfield::new(33, 33, 1.0).unwrap()
    }

    #[test]
    fn raise_follows_the_falloff_profile_and_reports_its_region() {
        let mut f = field();
        let brush = Brush {
            center: [16.0, 16.0],
            radius: 8.0,
            strength: 2.0,
            falloff: 0.5,
        };
        let report = sculpt(&mut f, &brush, SculptOp::Raise).unwrap();
        assert_eq!(f.height(16, 16), 2.0); // inner disc: full strength
        assert_eq!(f.height(16, 24), 0.0); // on the rim: zero
        let mid = f.height(16, 22); // d = 6: t = 0.5, smoothstep 0.5
        assert!((mid - 1.0).abs() < 1e-6, "{mid}");
        let region = report.region.unwrap();
        assert_eq!(region, SampleRect::new([9, 9], [24, 24]));
        assert_eq!(f.revision(), 1);
        // Deterministic.
        let mut g = field();
        sculpt(&mut g, &brush, SculptOp::Raise).unwrap();
        assert_eq!(f.heights(), g.heights());
    }

    #[test]
    fn flatten_set_smooth_and_noise_behave() {
        let mut f = field();
        sculpt(
            &mut f,
            &Brush::new([16.0, 16.0], 6.0),
            SculptOp::SetHeight { height: 5.0 },
        )
        .unwrap();
        assert_eq!(f.height(16, 16), 5.0);
        let peak = f.height(16, 16);
        let flatten = Brush {
            strength: 1.0,
            falloff: 0.0,
            ..Brush::new([16.0, 16.0], 3.0)
        };
        sculpt(&mut f, &flatten, SculptOp::Flatten { height: Some(1.0) }).unwrap();
        assert_eq!(f.height(16, 16), 1.0);
        assert!(peak > f.height(16, 16));
        let before = f.clone();
        sculpt(&mut f, &Brush::new([16.0, 16.0], 10.0), SculptOp::Smooth).unwrap();
        // Smoothing reduces the total variation of the bump.
        let variation = |f: &Heightfield| -> f32 {
            (0..32)
                .map(|c| (f.height(c + 1, 16) - f.height(c, 16)).abs())
                .sum()
        };
        assert!(variation(&f) < variation(&before));
        let mut g = field();
        let noise = Brush {
            strength: 3.0,
            ..Brush::new([16.0, 16.0], 12.0)
        };
        sculpt(
            &mut g,
            &noise,
            SculptOp::Noise {
                seed: 4,
                frequency: 0.2,
            },
        )
        .unwrap();
        let [lo, hi] = g.height_range();
        assert!(lo < 0.0 && hi > 0.0 && hi <= 3.0 && lo >= -3.0);
        assert!(sculpt(&mut g, &Brush::new([0.0, 0.0], 0.0), SculptOp::Raise).is_err());
        let strong = Brush {
            strength: 2.0,
            ..Brush::new([0.0, 0.0], 1.0)
        };
        assert!(sculpt(&mut g, &strong, SculptOp::Smooth).is_err());
    }

    #[test]
    fn painting_keeps_weights_normalized_and_full_strength_replaces() {
        let mut f = field();
        let rock = f.add_layer(TerrainLayer::new("rock", [0.5; 4])).unwrap();
        let snow = f.add_layer(TerrainLayer::new("snow", [1.0; 4])).unwrap();
        let soft = Brush {
            strength: 0.5,
            ..Brush::new([10.0, 10.0], 6.0)
        };
        paint_layer(&mut f, &soft, rock).unwrap();
        paint_layer(&mut f, &Brush::new([12.0, 10.0], 4.0), snow).unwrap();
        for r in 0..33 {
            for c in 0..33 {
                let sum: u32 = f.sample_weights(c, r).iter().map(|&w| w as u32).sum();
                assert_eq!(sum, 255);
            }
        }
        assert_eq!(f.sample_weights(12, 10), &[0, 0, 255]);
        let w = f.layer_weight_at(rock, 10.0, 13.0).unwrap();
        assert!(w > 0.0 && w < 1.0);
        assert!(paint_layer(&mut f, &soft, 7).is_err());
    }

    #[test]
    fn holes_are_painted_and_cleared_by_cell_centre() {
        let mut f = field();
        let brush = Brush::new([8.0, 8.0], 1.0);
        let report = paint_holes(&mut f, &brush, true).unwrap();
        // Cells (7,7), (8,7), (7,8), (8,8) have centres within 0.71 of (8, 8).
        assert_eq!(report.changed, 4);
        assert_eq!(report.region, Some(SampleRect::new([7, 7], [9, 9])));
        assert!(f.is_hole_at(7.5, 8.5));
        let cleared = paint_holes(&mut f, &brush, false).unwrap();
        assert_eq!(cleared.changed, 4);
        assert_eq!(f.hole_count(), 0);
    }

    #[test]
    fn quantization_sums_to_255() {
        let mut out = [0u8; 3];
        quantize_weights(&[1.0 / 3.0; 3], &mut out);
        assert_eq!(out, [85, 85, 85]);
        quantize_weights(&[0.5, 0.25, 0.25], &mut out);
        assert_eq!(out.iter().map(|&v| v as u32).sum::<u32>(), 255);
        quantize_weights(&[0.999_999, 0.0, 0.0], &mut out);
        assert_eq!(out, [255, 0, 0]);
    }
}
