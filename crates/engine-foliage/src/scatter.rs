//! Deterministic procedural placement over a [`Surface`].
//!
//! Every type is scattered independently, in list order, from candidates drawn per square
//! cell of a fixed world grid with a generator seeded by `(seed, cell, type)`. Two properties
//! follow: the candidate set of a region does not depend on any other region (painting a
//! brush draws the same candidates the full scatter would have), and a candidate's own
//! attributes (scale, yaw, phase, priority) do not depend on whether other candidates were
//! rejected, so changing a limit never reshuffles the survivors.
//!
//! Pipeline per candidate: bounds, exclusion zones, density map, holes, ground limits
//! (height, altitude, slope), exclusion of earlier types; survivors are then thinned to the
//! type's `min_spacing` greedily in a seeded priority order.
use crate::field::{FoliageField, Instance, MAX_INSTANCES};
use crate::types::{DensityMap, FoliageType, validate_types};
use crate::{Error, Result};
use engine_pcg::{Aabb, Rng, Surface, hash_cell};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Most candidate cells one scatter or stroke visits.
pub const MAX_CANDIDATE_CELLS: u64 = 4_000_000;

/// Marks ground that must stay empty although the surface has height there (terrain holes).
pub trait HoleMask: Send + Sync {
    fn is_hole(&self, x: f32, z: f32) -> bool;
}
impl<F: Fn(f32, f32) -> bool + Send + Sync> HoleMask for F {
    fn is_hole(&self, x: f32, z: f32) -> bool {
        self(x, z)
    }
}

/// An area where placement is forbidden (roads, buildings, camps).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExclusionZone {
    Circle {
        center: [f32; 2],
        radius: f32,
        /// Type names the zone applies to; empty = all.
        #[serde(default)]
        types: Vec<String>,
    },
    Rect {
        min: [f32; 2],
        max: [f32; 2],
        #[serde(default)]
        types: Vec<String>,
    },
}

impl ExclusionZone {
    pub fn validate(&self) -> Result<()> {
        let ok = match self {
            Self::Circle { center, radius, .. } => {
                center.iter().all(|v| v.is_finite()) && radius.is_finite() && *radius > 0.0
            }
            Self::Rect { min, max, .. } => {
                min.iter().chain(max).all(|v| v.is_finite()) && min[0] <= max[0] && min[1] <= max[1]
            }
        };
        if ok {
            Ok(())
        } else {
            Err(Error::new(
                "foliage.zone.invalid",
                "zones need finite coordinates, a positive circle radius or an ordered rectangle",
            ))
        }
    }
    fn applies_to(&self, name: &str) -> bool {
        let (Self::Circle { types, .. } | Self::Rect { types, .. }) = self;
        types.is_empty() || types.iter().any(|t| t == name)
    }
    /// Whether `(x, z)` lies inside the zone.
    pub fn contains(&self, x: f32, z: f32) -> bool {
        match self {
            Self::Circle { center, radius, .. } => {
                let (dx, dz) = (x - center[0], z - center[1]);
                dx * dx + dz * dz <= radius * radius
            }
            Self::Rect { min, max, .. } => x >= min[0] && x <= max[0] && z >= min[1] && z <= max[1],
        }
    }
}

/// Inputs of [`scatter`].
pub struct ScatterParams<'a> {
    /// Seed of the whole scatter.
    pub seed: u64,
    /// Placement box; x and z clip candidates, `min.y` is the floor `altitude` is relative to.
    pub bounds: Aabb,
    /// Edge of the storage cells of the resulting field.
    pub cell_size: f32,
    pub zones: &'a [ExclusionZone],
    pub holes: Option<&'a dyn HoleMask>,
    /// Instance cap (at most [`MAX_INSTANCES`]); exceeding it is an error, not a truncation.
    pub max_instances: usize,
}

impl<'a> ScatterParams<'a> {
    pub fn new(seed: u64, bounds: Aabb) -> Self {
        Self {
            seed,
            bounds,
            cell_size: 32.0,
            zones: &[],
            holes: None,
            max_instances: MAX_INSTANCES,
        }
    }
}

/// What happened to the candidates of one type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeReport {
    pub name: String,
    pub candidates: usize,
    pub rejected_bounds: usize,
    pub rejected_zone: usize,
    pub rejected_density: usize,
    pub rejected_hole: usize,
    pub rejected_slope: usize,
    pub rejected_height: usize,
    pub rejected_altitude: usize,
    pub rejected_exclusion: usize,
    pub rejected_spacing: usize,
    pub accepted: usize,
}

/// The result of [`scatter`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScatterResult {
    pub field: FoliageField,
    pub types: Vec<TypeReport>,
}

/// One drawn candidate: a position and the random rolls of its attributes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    pub x: f32,
    pub z: f32,
    pub density: f32,
    pub scale: f32,
    pub yaw: f32,
    pub tilt: f32,
    pub tilt_dir: f32,
    pub priority: f32,
    pub phase: f32,
}

/// Candidate cell edge for a density: a power of two giving about two candidates per cell.
pub(crate) fn candidate_cell(density: f32) -> f32 {
    let ideal = (2.0 / density.max(1e-6)).sqrt();
    2f32.powf(ideal.log2().round()).clamp(0.25, 32.0)
}

/// Draws the candidates of `type_index` inside `rect` (`[[min x, min z], [max x, max z]]`).
pub(crate) fn candidates(
    seed: u64,
    type_index: usize,
    density: f32,
    rect: [[f32; 2]; 2],
    out: &mut Vec<Candidate>,
) -> Result<()> {
    if density <= 0.0 || rect[1][0] <= rect[0][0] || rect[1][1] <= rect[0][1] {
        return Ok(());
    }
    let c = candidate_cell(density);
    let (x0, x1) = (
        (rect[0][0] / c).floor() as i64,
        (rect[1][0] / c).floor() as i64,
    );
    let (z0, z1) = (
        (rect[0][1] / c).floor() as i64,
        (rect[1][1] / c).floor() as i64,
    );
    let cells = (x1 - x0 + 1) as u64 * (z1 - z0 + 1) as u64;
    if cells > MAX_CANDIDATE_CELLS {
        return Err(Error::new(
            "foliage.limit.cells",
            format!("{cells} candidate cells exceed {MAX_CANDIDATE_CELLS}; use a smaller area"),
        ));
    }
    let expected = density * c * c;
    for iz in z0..=z1 {
        for ix in x0..=x1 {
            let mut rng = Rng::new(hash_cell(seed, ix, type_index as i64, iz));
            let whole = expected.floor();
            let n = whole as u32 + u32::from(rng.next_f32() < expected - whole);
            for _ in 0..n {
                let candidate = Candidate {
                    x: (ix as f32 + rng.next_f32()) * c,
                    z: (iz as f32 + rng.next_f32()) * c,
                    density: rng.next_f32(),
                    scale: rng.next_f32(),
                    yaw: rng.next_f32(),
                    tilt: rng.next_f32(),
                    tilt_dir: rng.next_f32(),
                    priority: rng.next_f32(),
                    phase: rng.next_f32(),
                };
                if candidate.x >= rect[0][0]
                    && candidate.x < rect[1][0]
                    && candidate.z >= rect[0][1]
                    && candidate.z < rect[1][1]
                {
                    out.push(candidate);
                }
            }
        }
    }
    Ok(())
}

/// Per-run placement context shared by scatter and painting.
pub(crate) struct Placement<'a> {
    pub surface: &'a dyn Surface,
    /// Horizontal clip (`[[min x, min z], [max x, max z]]`): bounds intersected with the surface.
    pub region: [[f32; 2]; 2],
    pub floor_y: f32,
    pub zones: &'a [ExclusionZone],
    pub holes: Option<&'a dyn HoleMask>,
}

impl<'a> Placement<'a> {
    pub fn new(
        surface: &'a dyn Surface,
        bounds: &Aabb,
        zones: &'a [ExclusionZone],
        holes: Option<&'a dyn HoleMask>,
    ) -> Self {
        let clipped = bounds.intersect_xz(&surface.bounds());
        Self {
            surface,
            region: [
                [clipped.min.x, clipped.min.z],
                [clipped.max.x, clipped.max.z],
            ],
            floor_y: bounds.min.y,
            zones,
            holes,
        }
    }
    pub fn is_empty(&self) -> bool {
        !(self.region[1][0] > self.region[0][0] && self.region[1][1] > self.region[0][1])
            || !self.region.iter().flatten().all(|v| v.is_finite())
    }
}

enum Rejection {
    Bounds,
    Zone,
    Density,
    Hole,
    Slope,
    Height,
    Altitude,
    Exclusion,
}

fn count(report: &mut TypeReport, why: &Rejection) {
    match why {
        Rejection::Bounds => report.rejected_bounds += 1,
        Rejection::Zone => report.rejected_zone += 1,
        Rejection::Density => report.rejected_density += 1,
        Rejection::Hole => report.rejected_hole += 1,
        Rejection::Slope => report.rejected_slope += 1,
        Rejection::Height => report.rejected_height += 1,
        Rejection::Altitude => report.rejected_altitude += 1,
        Rejection::Exclusion => report.rejected_exclusion += 1,
    }
}

/// Resolved `excludes` of a type: (earlier type index, radius).
pub(crate) fn resolve_exclusions(types: &[FoliageType], index: usize) -> Vec<(u16, f32)> {
    types[index]
        .excludes
        .iter()
        .filter_map(|e| {
            types[..index]
                .iter()
                .position(|t| t.name == e.name)
                .map(|i| (i as u16, e.radius))
        })
        .collect()
}

/// Tests one candidate against every limit and builds the instance.
#[allow(clippy::too_many_arguments)]
fn evaluate(
    t: &FoliageType,
    type_index: u16,
    c: &Candidate,
    place: &Placement<'_>,
    exclusions: &[(u16, f32)],
    existing: &FoliageField,
    accept_probability: f32,
) -> std::result::Result<Instance, Rejection> {
    let (x, z) = (c.x, c.z);
    if x < place.region[0][0]
        || x >= place.region[1][0]
        || z < place.region[0][1]
        || z >= place.region[1][1]
    {
        return Err(Rejection::Bounds);
    }
    if place
        .zones
        .iter()
        .any(|zone| zone.applies_to(&t.name) && zone.contains(x, z))
    {
        return Err(Rejection::Zone);
    }
    let weight = t.density_map.sample(place.surface, x, z) * accept_probability;
    if c.density >= weight {
        return Err(Rejection::Density);
    }
    if place.holes.is_some_and(|h| h.is_hole(x, z)) {
        return Err(Rejection::Hole);
    }
    let ground = place.surface.height(x, z);
    if !ground.is_finite() {
        return Err(Rejection::Hole);
    }
    if ground < t.height[0] || ground > t.height[1] {
        return Err(Rejection::Height);
    }
    let altitude = ground - place.floor_y;
    if altitude < t.altitude[0] || altitude > t.altitude[1] {
        return Err(Rejection::Altitude);
    }
    let normal = place.surface.normal(x, z);
    let slope = normal.y.clamp(-1.0, 1.0).acos().to_degrees();
    if slope < t.slope_degrees[0] || slope > t.slope_degrees[1] {
        return Err(Rejection::Slope);
    }
    if exclusions
        .iter()
        .any(|(other, radius)| existing.any_within(x, z, *radius, Some(*other)))
    {
        return Err(Rejection::Exclusion);
    }
    let lerp = |r: [f32; 2], u: f32| r[0] + (r[1] - r[0]) * u;
    let yaw = Quat::from_rotation_y(lerp(t.yaw_degrees, c.yaw).to_radians());
    let tilt_axis = Vec3::new(
        (c.tilt_dir * std::f32::consts::TAU).cos(),
        0.0,
        (c.tilt_dir * std::f32::consts::TAU).sin(),
    );
    let tilt = Quat::from_axis_angle(tilt_axis, (t.tilt_degrees * c.tilt).to_radians());
    let align = if t.align_to_normal > 0.0 {
        Quat::IDENTITY.slerp(
            Quat::from_rotation_arc(Vec3::Y, normal.normalize_or(Vec3::Y)),
            t.align_to_normal,
        )
    } else {
        Quat::IDENTITY
    };
    let rotation = (align * tilt * yaw).normalize();
    let scale = lerp(t.scale, c.scale);
    Ok(Instance {
        type_index,
        position: [x, ground + t.ground_offset, z],
        rotation: rotation.to_array(),
        scale: [scale; 3],
        phase: c.phase,
    })
}

/// Greedy minimum-distance thinning in priority order. `existing` instances of the same type
/// also block. Returns the indices (into `items`) that survive, in their original order.
fn thin(
    items: &[(Candidate, Instance)],
    radius: f32,
    existing: &FoliageField,
    type_index: u16,
) -> Vec<usize> {
    if radius <= 0.0 {
        return (0..items.len()).collect();
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        items[a]
            .0
            .priority
            .total_cmp(&items[b].0.priority)
            .then(a.cmp(&b))
    });
    let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    let key = |x: f32, z: f32| ((x / radius).floor() as i32, (z / radius).floor() as i32);
    let r2 = radius * radius;
    let mut kept = Vec::new();
    for i in order {
        let (x, z) = (items[i].1.position[0], items[i].1.position[2]);
        if existing.any_within(x, z, radius, Some(type_index)) {
            continue;
        }
        let (kx, kz) = key(x, z);
        let blocked = (kx - 1..=kx + 1).any(|cx| {
            (kz - 1..=kz + 1).any(|cz| {
                grid.get(&(cx, cz)).is_some_and(|members| {
                    members.iter().any(|&m| {
                        let (dx, dz) = (items[m].1.position[0] - x, items[m].1.position[2] - z);
                        dx * dx + dz * dz < r2
                    })
                })
            })
        });
        if !blocked {
            grid.entry((kx, kz)).or_default().push(i);
            kept.push(i);
        }
    }
    kept.sort_unstable();
    kept
}

/// Smallest separation of two instances of a type (stops brush strokes from stacking
/// duplicates when `min_spacing` is 0).
pub(crate) const MIN_SEPARATION: f32 = 1e-3;

/// Places `type_index` into `field`: candidates in `rect`, accepted with an extra probability
/// `accept_probability` (1 for a full scatter, the brush weight when painting).
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_type(
    types: &[FoliageType],
    type_index: usize,
    seed: u64,
    rect: [[f32; 2]; 2],
    place: &Placement<'_>,
    accept_probability: impl Fn(f32, f32) -> f32,
    field: &mut FoliageField,
    max_instances: usize,
    report: &mut TypeReport,
) -> Result<()> {
    let t = &types[type_index];
    let exclusions = resolve_exclusions(types, type_index);
    let mut drawn = Vec::new();
    candidates(seed, type_index, t.density, rect, &mut drawn)?;
    report.candidates += drawn.len();
    let mut survivors: Vec<(Candidate, Instance)> = Vec::with_capacity(drawn.len() / 2);
    for c in &drawn {
        match evaluate(
            t,
            type_index as u16,
            c,
            place,
            &exclusions,
            field,
            accept_probability(c.x, c.z),
        ) {
            Ok(instance) => survivors.push((*c, instance)),
            Err(why) => count(report, &why),
        }
    }
    let radius = t.min_spacing.max(MIN_SEPARATION);
    let kept = thin(&survivors, radius, field, type_index as u16);
    report.rejected_spacing += survivors.len() - kept.len();
    if field.len() + kept.len() > max_instances.min(MAX_INSTANCES) {
        return Err(Error::new(
            "foliage.limit.instances",
            format!(
                "placing `{}` would exceed {} instances; lower the density or the area",
                t.name,
                max_instances.min(MAX_INSTANCES)
            ),
        ));
    }
    for i in kept {
        field.insert(survivors[i].1)?;
        report.accepted += 1;
    }
    Ok(())
}

/// Scatters every type over `surface`. Deterministic: the result is a pure function of the
/// types, the surface's answers and `params`.
pub fn scatter(
    types: &[FoliageType],
    surface: &dyn Surface,
    params: &ScatterParams<'_>,
) -> Result<ScatterResult> {
    validate_types(types)?;
    for zone in params.zones {
        zone.validate()?;
    }
    if !params.bounds.min.is_finite() || !params.bounds.max.is_finite() {
        return Err(Error::new(
            "foliage.bounds.invalid",
            "bounds must be finite",
        ));
    }
    for t in types {
        match &t.density_map {
            DensityMap::Texture { .. } => {
                return Err(Error::new(
                    "foliage.density.unresolved",
                    format!(
                        "type `{}`: texture density maps must be resolved into a grid first",
                        t.name
                    ),
                ));
            }
            DensityMap::Layer { layer, .. } if t.density > 0.0 => {
                let probe = params.bounds.intersect_xz(&surface.bounds());
                let (cx, cz) = (
                    (probe.min.x + probe.max.x) * 0.5,
                    (probe.min.z + probe.max.z) * 0.5,
                );
                if surface.layer_weight(layer, cx, cz).is_none() {
                    return Err(Error::new(
                        "foliage.density.layer",
                        format!(
                            "type `{}`: the surface has no paint layer `{layer}`",
                            t.name
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    let mut field = FoliageField::new(params.cell_size)?;
    let mut reports: Vec<TypeReport> = types
        .iter()
        .map(|t| TypeReport {
            name: t.name.clone(),
            ..TypeReport::default()
        })
        .collect();
    let place = Placement::new(surface, &params.bounds, params.zones, params.holes);
    if !place.is_empty() {
        for (index, report) in reports.iter_mut().enumerate() {
            place_type(
                types,
                index,
                params.seed,
                place.region,
                &place,
                |_, _| 1.0,
                &mut field,
                params.max_instances,
                report,
            )?;
        }
    }
    Ok(ScatterResult {
        field,
        types: reports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_pcg::{FlatSurface, FnSurface};

    fn bounds(side: f32) -> Aabb {
        Aabb::new([0.0, 0.0, 0.0], [side, 100.0, side])
    }

    #[test]
    fn candidate_cells_are_powers_of_two_with_about_two_candidates() {
        for (density, cell) in [
            (1.0, 1.0),
            (0.125, 4.0),
            (8.0, 0.5),
            (0.0001, 32.0),
            (64.0, 0.25),
        ] {
            let c = candidate_cell(density);
            assert_eq!(c, cell, "density {density}");
        }
    }

    #[test]
    fn empty_types_and_empty_bounds_give_empty_fields() {
        let surface = FlatSurface::new(0.0);
        let r = scatter(&[], &surface, &ScatterParams::new(1, bounds(10.0))).unwrap();
        assert!(r.field.is_empty());
        let t = [FoliageType::new("a", "m")];
        let r = scatter(
            &t,
            &surface,
            &ScatterParams::new(1, Aabb::new([0.0; 3], [0.0, 1.0, 0.0])),
        )
        .unwrap();
        assert!(r.field.is_empty());
    }

    #[test]
    fn texture_density_maps_must_be_resolved_and_unknown_layers_fail() {
        let surface = FlatSurface::new(0.0);
        let mut t = FoliageType::new("a", "m");
        t.density_map = DensityMap::Texture {
            asset: "d".into(),
            origin: [0.0; 2],
            size: [1.0; 2],
        };
        let e = scatter(&[t.clone()], &surface, &ScatterParams::new(1, bounds(10.0))).unwrap_err();
        assert_eq!(e.code, "foliage.density.unresolved");
        t.density_map = DensityMap::Layer {
            layer: "nope".into(),
            invert: false,
            threshold: 0.0,
        };
        let e = scatter(&[t], &surface, &ScatterParams::new(1, bounds(10.0))).unwrap_err();
        assert_eq!(e.code, "foliage.density.layer");
    }

    #[test]
    fn instance_cap_is_an_error_not_a_truncation() {
        let surface = FlatSurface::new(0.0);
        let mut t = FoliageType::new("a", "m");
        t.density = 4.0;
        let mut p = ScatterParams::new(1, bounds(20.0));
        p.max_instances = 100;
        assert_eq!(
            scatter(&[t], &surface, &p).unwrap_err().code,
            "foliage.limit.instances"
        );
    }

    #[test]
    fn layer_density_follows_the_painted_weight() {
        // Weight 1 on the left half and 0 on the right half.
        let surface = FnSurface::new(Aabb::new([0.0; 3], [40.0, 10.0, 40.0]), |_, _| 0.0)
            .with_layer("grass", |x, _| if x < 20.0 { 1.0 } else { 0.0 });
        let mut t = FoliageType::new("g", "m");
        t.density = 1.0;
        t.density_map = DensityMap::Layer {
            layer: "grass".into(),
            invert: false,
            threshold: 0.0,
        };
        let r = scatter(&[t], &surface, &ScatterParams::new(3, bounds(40.0))).unwrap();
        assert!(r.field.len() > 600);
        assert!(r.field.iter().all(|i| i.position[0] < 20.0));
    }
}
