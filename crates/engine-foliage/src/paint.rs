//! Brush painting: add or erase instances inside a circle with deterministic seeds.
//!
//! A stroke adds instances of one type exactly as [`crate::scatter`] would place them
//! (same candidate grid, same limits, same minimum spacing against what is already there),
//! with the type's density scaled by the brush weight; the same stroke with the same seed is
//! therefore idempotent. Erasing removes the instances inside the circle whose brush weight
//! exceeds a per-instance hash, so strength 1 clears the inner disc.
use crate::field::FoliageField;
use crate::scatter::{ExclusionZone, HoleMask, Placement, TypeReport, place_type};
use crate::types::{FoliageType, validate_types};
use crate::{Error, Result};
use engine_pcg::{Aabb, Surface, mix64};
use serde::{Deserialize, Serialize};

/// A circular brush on the XZ plane.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brush {
    pub center: [f32; 2],
    /// Metres (0 < radius <= 10000).
    pub radius: f32,
    /// 0..=1: scales density when adding and the removal chance when erasing.
    #[serde(default = "one")]
    pub strength: f32,
    /// 0..=1: weight 1 inside `radius * (1 - falloff)`, smoothstep to 0 at `radius`.
    #[serde(default = "half")]
    pub falloff: f32,
}

fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}

impl Brush {
    /// A full-strength brush with half falloff.
    pub fn new(center: [f32; 2], radius: f32) -> Self {
        Self {
            center,
            radius,
            strength: 1.0,
            falloff: 0.5,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let ok = self.center.iter().all(|v| v.is_finite())
            && self.radius.is_finite()
            && self.radius > 0.0
            && self.radius <= 10_000.0
            && (0.0..=1.0).contains(&self.strength)
            && (0.0..=1.0).contains(&self.falloff);
        if ok {
            Ok(())
        } else {
            Err(Error::new(
                "foliage.brush.invalid",
                "brush needs a finite center, radius within (0, 10000], strength and falloff within 0..=1",
            ))
        }
    }
    /// Brush weight at `(x, z)`: 0 outside the circle, 1 inside the inner radius.
    pub fn weight(&self, x: f32, z: f32) -> f32 {
        let d = ((x - self.center[0]).powi(2) + (z - self.center[1]).powi(2)).sqrt();
        if d >= self.radius {
            return 0.0;
        }
        let inner = self.radius * (1.0 - self.falloff);
        if d <= inner {
            return 1.0;
        }
        let t = ((d - inner) / (self.radius - inner)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }
    fn rect(&self) -> [[f32; 2]; 2] {
        [
            [self.center[0] - self.radius, self.center[1] - self.radius],
            [self.center[0] + self.radius, self.center[1] + self.radius],
        ]
    }
}

/// What a stroke does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PaintOp {
    /// Add instances of the type at this index.
    Add { type_index: usize },
    /// Remove instances of the type (all types with `None`).
    Erase { type_index: Option<usize> },
}

/// The ground and limits a stroke places against.
pub struct PaintContext<'a> {
    pub surface: &'a dyn Surface,
    /// Placement box (see [`crate::ScatterParams::bounds`]).
    pub bounds: Aabb,
    pub zones: &'a [ExclusionZone],
    pub holes: Option<&'a dyn HoleMask>,
    pub max_instances: usize,
}

/// What one stroke changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaintReport {
    pub added: usize,
    pub removed: usize,
    /// Rectangle `[[min x, min z], [max x, max z]]` that may have changed, if anything did.
    pub region: Option<[[f32; 2]; 2]>,
    /// Why candidates were rejected (adding only).
    pub details: TypeReport,
}

/// Applies one stroke to `field`. `types` must be the set `field`'s type indices refer to.
pub fn paint(
    field: &mut FoliageField,
    types: &[FoliageType],
    ctx: &PaintContext<'_>,
    brush: &Brush,
    op: PaintOp,
    seed: u64,
) -> Result<PaintReport> {
    brush.validate()?;
    validate_types(types)?;
    let mut report = PaintReport::default();
    match op {
        PaintOp::Add { type_index } => {
            let Some(t) = types.get(type_index) else {
                return Err(Error::new(
                    "foliage.paint.type",
                    format!("type index {type_index} out of range (0..{})", types.len()),
                ));
            };
            report.details.name = t.name.clone();
            let place = Placement::new(ctx.surface, &ctx.bounds, ctx.zones, ctx.holes);
            if place.is_empty() {
                return Ok(report);
            }
            let rect = brush.rect();
            let clipped = [
                [
                    rect[0][0].max(place.region[0][0]),
                    rect[0][1].max(place.region[0][1]),
                ],
                [
                    rect[1][0].min(place.region[1][0]),
                    rect[1][1].min(place.region[1][1]),
                ],
            ];
            let before = field.len();
            place_type(
                types,
                type_index,
                seed,
                clipped,
                &place,
                |x, z| brush.weight(x, z) * brush.strength,
                field,
                ctx.max_instances,
                &mut report.details,
            )?;
            report.added = field.len() - before;
            if report.added > 0 {
                report.region = Some(clipped);
            }
        }
        PaintOp::Erase { type_index } => {
            let rect = brush.rect();
            report.removed = field.remove_where(Some(rect), |i| {
                if type_index.is_some_and(|t| i.type_index as usize != t) {
                    return false;
                }
                let w = brush.weight(i.position[0], i.position[2]) * brush.strength;
                if w <= 0.0 {
                    return false;
                }
                let h = mix64(
                    seed ^ (u64::from(i.position[0].to_bits()) << 32)
                        ^ u64::from(i.position[2].to_bits()),
                );
                let u = (h >> 40) as f32 / 16_777_216.0;
                u < w
            });
            if report.removed > 0 {
                report.region = Some(rect);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_pcg::FlatSurface;

    fn setup() -> (FlatSurface, Vec<FoliageType>) {
        let mut t = FoliageType::new("grass", "g");
        t.density = 2.0;
        (FlatSurface::new(0.0), vec![t])
    }
    fn ctx<'a>(s: &'a FlatSurface) -> PaintContext<'a> {
        PaintContext {
            surface: s,
            bounds: Aabb::new([-100.0, 0.0, -100.0], [100.0, 50.0, 100.0]),
            zones: &[],
            holes: None,
            max_instances: crate::field::MAX_INSTANCES,
        }
    }

    #[test]
    fn brush_weight_has_a_flat_core_and_smooth_edge() {
        let b = Brush::new([0.0, 0.0], 10.0);
        assert_eq!(b.weight(0.0, 0.0), 1.0);
        assert_eq!(b.weight(5.0, 0.0), 1.0);
        assert_eq!(b.weight(10.0, 0.0), 0.0);
        let mid = b.weight(7.5, 0.0);
        assert!((mid - 0.5).abs() < 1e-5, "{mid}");
        assert!(Brush::new([0.0, f32::NAN], 1.0).validate().is_err());
    }

    #[test]
    fn adding_places_instances_only_inside_the_circle_at_about_the_authored_density() {
        let (s, types) = setup();
        let mut field = FoliageField::new(16.0).unwrap();
        let brush = Brush {
            center: [10.0, -5.0],
            radius: 8.0,
            strength: 1.0,
            falloff: 0.0,
        };
        let r = paint(
            &mut field,
            &types,
            &ctx(&s),
            &brush,
            PaintOp::Add { type_index: 0 },
            7,
        )
        .unwrap();
        // Expected: density 2 / m2 over a disc of area pi r^2 (a square candidate box is
        // clipped by the exact weight 1 inside, 0 outside).
        let expected = 2.0 * std::f32::consts::PI * 64.0;
        assert_eq!(r.added, field.len());
        assert!(
            (r.added as f32 - expected).abs() < 0.08 * expected,
            "{} vs {expected}",
            r.added
        );
        assert!(field.iter().all(|i| {
            ((i.position[0] - 10.0).powi(2) + (i.position[2] + 5.0).powi(2)).sqrt() <= 8.0
        }));
    }

    #[test]
    fn the_same_stroke_with_the_same_seed_is_idempotent_and_other_seeds_add_more() {
        let (s, types) = setup();
        let mut field = FoliageField::new(16.0).unwrap();
        let brush = Brush::new([0.0, 0.0], 6.0);
        let op = PaintOp::Add { type_index: 0 };
        let first = paint(&mut field, &types, &ctx(&s), &brush, op, 5)
            .unwrap()
            .added;
        assert!(first > 100);
        let again = paint(&mut field, &types, &ctx(&s), &brush, op, 5)
            .unwrap()
            .added;
        assert_eq!(again, 0);
        let other = paint(&mut field, &types, &ctx(&s), &brush, op, 6)
            .unwrap()
            .added;
        assert!(other > 100);
        // Determinism: a second field painted the same way is identical.
        let mut twin = FoliageField::new(16.0).unwrap();
        paint(&mut twin, &types, &ctx(&s), &brush, op, 5).unwrap();
        paint(&mut twin, &types, &ctx(&s), &brush, op, 5).unwrap();
        paint(&mut twin, &types, &ctx(&s), &brush, op, 6).unwrap();
        assert_eq!(
            twin.iter().copied().collect::<Vec<_>>(),
            field.iter().copied().collect::<Vec<_>>()
        );
    }

    #[test]
    fn erasing_clears_the_inner_disc_and_leaves_the_outside() {
        let (s, types) = setup();
        let mut field = FoliageField::new(16.0).unwrap();
        paint(
            &mut field,
            &types,
            &ctx(&s),
            &Brush::new([0.0, 0.0], 20.0),
            PaintOp::Add { type_index: 0 },
            1,
        )
        .unwrap();
        let total = field.len();
        let eraser = Brush {
            center: [0.0, 0.0],
            radius: 10.0,
            strength: 1.0,
            falloff: 0.5,
        };
        let r = paint(
            &mut field,
            &types,
            &ctx(&s),
            &eraser,
            PaintOp::Erase { type_index: None },
            2,
        )
        .unwrap();
        assert_eq!(field.len(), total - r.removed);
        // Nothing remains inside the inner radius (5 m), everything outside 10 m stays.
        assert!(
            field
                .iter()
                .all(|i| i.position[0].hypot(i.position[2]) > 5.0)
        );
        assert!(
            field
                .iter()
                .filter(|i| i.position[0].hypot(i.position[2]) > 10.0)
                .count()
                > 0
        );
        let outside_before = total - r.removed;
        assert!(outside_before > 0);
        // Half strength removes roughly half of the inner core.
        let mut again = FoliageField::new(16.0).unwrap();
        paint(
            &mut again,
            &types,
            &ctx(&s),
            &Brush::new([0.0, 0.0], 12.0),
            PaintOp::Add { type_index: 0 },
            1,
        )
        .unwrap();
        let n = again.len() as f32;
        let half = Brush {
            center: [0.0, 0.0],
            radius: 12.0,
            strength: 0.5,
            falloff: 0.0,
        };
        let r = paint(
            &mut again,
            &types,
            &ctx(&s),
            &half,
            PaintOp::Erase {
                type_index: Some(0),
            },
            3,
        )
        .unwrap();
        assert!(
            (r.removed as f32 / n - 0.5).abs() < 0.06,
            "{} of {n}",
            r.removed
        );
    }

    #[test]
    fn add_validates_the_type_index_and_brush() {
        let (s, types) = setup();
        let mut field = FoliageField::new(16.0).unwrap();
        let brush = Brush::new([0.0, 0.0], 3.0);
        assert_eq!(
            paint(
                &mut field,
                &types,
                &ctx(&s),
                &brush,
                PaintOp::Add { type_index: 3 },
                1
            )
            .unwrap_err()
            .code,
            "foliage.paint.type"
        );
        let bad = Brush {
            strength: 2.0,
            ..brush
        };
        assert!(
            paint(
                &mut field,
                &types,
                &ctx(&s),
                &bad,
                PaintOp::Erase { type_index: None },
                1
            )
            .is_err()
        );
    }
}
