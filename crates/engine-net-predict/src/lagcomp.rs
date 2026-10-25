//! Server-side lag compensation: bounded hitbox history and rewound queries.
//!
//! The history is independent of any physics engine. Integration code copies collider data
//! into [`Hitbox`] shapes (entity space) once and records entity poses every server tick.
//! Animated parts (limbs, bones) are recorded as separate entities with their own poses.
use crate::{PredictError, Result};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, collections::VecDeque, time::Duration};

/// Application entity identifier (stable across ticks).
pub type HitEntity = u64;

/// Hit volume in entity space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitShape {
    Sphere {
        center: Vec3,
        radius: f32,
    },
    /// Segment `a`-`b` swept by `radius`.
    Capsule {
        a: Vec3,
        b: Vec3,
        radius: f32,
    },
    OrientedBox {
        center: Vec3,
        half_extents: Vec3,
        rotation: Quat,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hitbox {
    pub shape: HitShape,
    /// Application part tag (head, torso, ...), reported with hits.
    pub part: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub translation: Vec3,
    pub rotation: Quat,
}
impl Pose {
    pub fn new(translation: Vec3, rotation: Quat) -> Self {
        Self {
            translation,
            rotation,
        }
    }
    fn valid(&self) -> bool {
        self.translation.is_finite()
            && self.translation.abs().max_element() <= 1.0e9
            && self.rotation.is_finite()
            && (self.rotation.length() - 1.0).abs() <= 1.0e-3
    }
    fn point(&self, local: Vec3) -> Vec3 {
        self.translation + self.rotation * local
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColliderHistoryConfig {
    /// Recorded ticks retained (the rewind window), 2..=4096.
    pub max_ticks: usize,
    pub max_entities: usize,
    pub max_hitboxes_per_entity: usize,
}
impl Default for ColliderHistoryConfig {
    fn default() -> Self {
        Self {
            max_ticks: 64,
            max_entities: 1024,
            max_hitboxes_per_entity: 32,
        }
    }
}

/// Query filter: entities whose layer bits intersect `layer_mask`, minus `exclude`
/// (typically the shooter).
#[derive(Clone, Copy, Debug)]
pub struct QueryFilter<'a> {
    pub layer_mask: u32,
    pub exclude: &'a [HitEntity],
}
impl Default for QueryFilter<'_> {
    fn default() -> Self {
        Self {
            layer_mask: u32::MAX,
            exclude: &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    /// Need not be normalised; must be finite and nonzero.
    pub direction: Vec3,
    pub max_distance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub entity: HitEntity,
    pub part: u16,
    pub distance: f32,
    pub point: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlapHit {
    pub entity: HitEntity,
    pub part: u16,
}

struct HitboxSet {
    layers: u32,
    hitboxes: Vec<Hitbox>,
}

struct Frame {
    tick: u64,
    /// Sorted by entity.
    poses: Vec<(HitEntity, Pose)>,
}

/// Per-entity hitboxes plus poses for a bounded window of recorded ticks.
pub struct ColliderHistory {
    config: ColliderHistoryConfig,
    shapes: BTreeMap<HitEntity, HitboxSet>,
    frames: VecDeque<Frame>,
}

fn valid_shape(shape: &HitShape) -> bool {
    let radius_ok = |r: f32| r.is_finite() && r > 0.0 && r <= 1.0e6;
    let point_ok = |p: Vec3| p.is_finite() && p.abs().max_element() <= 1.0e6;
    match *shape {
        HitShape::Sphere { center, radius } => point_ok(center) && radius_ok(radius),
        HitShape::Capsule { a, b, radius } => point_ok(a) && point_ok(b) && radius_ok(radius),
        HitShape::OrientedBox {
            center,
            half_extents,
            rotation,
        } => {
            point_ok(center)
                && half_extents.is_finite()
                && half_extents.min_element() > 0.0
                && half_extents.max_element() <= 1.0e6
                && rotation.is_finite()
                && (rotation.length() - 1.0).abs() <= 1.0e-3
        }
    }
}

impl ColliderHistory {
    pub fn new(config: ColliderHistoryConfig) -> Result<Self> {
        if config.max_ticks < 2 || config.max_ticks > 4096 {
            return Err(PredictError::Invalid("max_ticks must be 2..=4096"));
        }
        if config.max_entities == 0 || config.max_entities > 65536 {
            return Err(PredictError::Invalid("max_entities must be 1..=65536"));
        }
        if config.max_hitboxes_per_entity == 0 || config.max_hitboxes_per_entity > 256 {
            return Err(PredictError::Invalid(
                "max_hitboxes_per_entity must be 1..=256",
            ));
        }
        Ok(Self {
            frames: VecDeque::with_capacity(config.max_ticks),
            config,
            shapes: BTreeMap::new(),
        })
    }
    pub fn config(&self) -> &ColliderHistoryConfig {
        &self.config
    }
    /// Sets (or replaces) an entity's hitboxes and layer bits. Shapes apply to every retained
    /// tick, so record animated parts as separate entities.
    pub fn set_hitboxes(
        &mut self,
        entity: HitEntity,
        layers: u32,
        hitboxes: Vec<Hitbox>,
    ) -> Result<()> {
        if hitboxes.is_empty() || hitboxes.len() > self.config.max_hitboxes_per_entity {
            return Err(PredictError::Invalid("hitbox count outside limits"));
        }
        if !hitboxes.iter().all(|h| valid_shape(&h.shape)) {
            return Err(PredictError::Invalid(
                "hitbox shape is not finite and positive",
            ));
        }
        if !self.shapes.contains_key(&entity) && self.shapes.len() >= self.config.max_entities {
            return Err(PredictError::Full("too many hitbox entities"));
        }
        self.shapes.insert(entity, HitboxSet { layers, hitboxes });
        Ok(())
    }
    pub fn remove_entity(&mut self, entity: HitEntity) -> bool {
        self.shapes.remove(&entity).is_some()
    }
    pub fn entity_count(&self) -> usize {
        self.shapes.len()
    }
    /// Recorded ticks currently retained.
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    pub fn oldest_tick(&self) -> Option<u64> {
        self.frames.front().map(|f| f.tick)
    }
    pub fn newest_tick(&self) -> Option<u64> {
        self.frames.back().map(|f| f.tick)
    }
    /// Records every entity pose for a tick (strictly increasing ticks). Entities without
    /// hitboxes are ignored by queries but still recorded. Invalid input records nothing.
    pub fn record(
        &mut self,
        tick: u64,
        poses: impl IntoIterator<Item = (HitEntity, Pose)>,
    ) -> Result<()> {
        if self.newest_tick().is_some_and(|n| tick <= n) {
            return Err(PredictError::Invalid("recorded ticks must increase"));
        }
        let mut buf = Vec::with_capacity(self.shapes.len().min(self.config.max_entities));
        for (entity, pose) in poses {
            if buf.len() >= self.config.max_entities {
                return Err(PredictError::Full("too many posed entities in one tick"));
            }
            if !pose.valid() {
                return Err(PredictError::Invalid(
                    "pose is not finite with a unit rotation",
                ));
            }
            buf.push((entity, pose));
        }
        buf.sort_unstable_by_key(|(e, _)| *e);
        if buf.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(PredictError::Invalid("duplicate entity in one tick"));
        }
        if self.frames.len() >= self.config.max_ticks {
            self.frames.pop_front();
        }
        self.frames.push_back(Frame { tick, poses: buf });
        Ok(())
    }
    /// Brackets a fractional tick: (frame index, next frame index, blend factor).
    fn bracket(&self, tick: f64) -> Option<(usize, usize, f32)> {
        if !tick.is_finite() {
            return None;
        }
        let (oldest, newest) = (self.oldest_tick()? as f64, self.newest_tick()? as f64);
        if tick < oldest || tick > newest {
            return None;
        }
        let i = self.frames.partition_point(|f| (f.tick as f64) <= tick) - 1;
        let t0 = self.frames[i].tick as f64;
        if tick == t0 || i + 1 == self.frames.len() {
            return Some((i, i, 0.0));
        }
        let t1 = self.frames[i + 1].tick as f64;
        Some((i, i + 1, ((tick - t0) / (t1 - t0)) as f32))
    }
    fn lookup(frame: &Frame, entity: HitEntity) -> Option<Pose> {
        frame
            .poses
            .binary_search_by_key(&entity, |(e, _)| *e)
            .ok()
            .map(|i| frame.poses[i].1)
    }
    fn blend(a: Pose, b: Pose, alpha: f32) -> Pose {
        Pose {
            translation: a.translation.lerp(b.translation, alpha),
            rotation: a.rotation.slerp(b.rotation, alpha).normalize(),
        }
    }
    /// Interpolated pose at a fractional tick. `None` outside the retained window or when the
    /// entity is absent from either bracketing tick (spawned/despawned in between).
    pub fn pose_at(&self, entity: HitEntity, tick: f64) -> Option<Pose> {
        let (i0, i1, alpha) = self.bracket(tick)?;
        let a = Self::lookup(&self.frames[i0], entity)?;
        let b = Self::lookup(&self.frames[i1], entity)?;
        Some(Self::blend(a, b, alpha))
    }
    /// Visits every filtered entity with hitboxes and a pose at `tick`, in entity order.
    fn for_each_posed(
        &self,
        tick: f64,
        filter: &QueryFilter<'_>,
        mut visit: impl FnMut(HitEntity, Pose, &[Hitbox]),
    ) {
        let Some((i0, i1, alpha)) = self.bracket(tick) else {
            return;
        };
        let (f0, f1) = (&self.frames[i0], &self.frames[i1]);
        for &(entity, a) in &f0.poses {
            if filter.exclude.contains(&entity) {
                continue;
            }
            let Some(set) = self.shapes.get(&entity) else {
                continue;
            };
            if set.layers & filter.layer_mask == 0 {
                continue;
            }
            let Some(b) = Self::lookup(f1, entity) else {
                continue;
            };
            visit(entity, Self::blend(a, b, alpha), &set.hitboxes);
        }
    }
    /// Nearest hit along the ray at a (rewound) tick; ties resolve to the lowest entity ID.
    pub fn raycast(&self, tick: f64, ray: Ray, filter: &QueryFilter<'_>) -> Option<RayHit> {
        let len = ray.direction.length();
        if !ray.origin.is_finite()
            || !len.is_finite()
            || len <= f32::EPSILON
            || !ray.max_distance.is_finite()
            || ray.max_distance < 0.0
        {
            return None;
        }
        let dir = ray.direction / len;
        let mut best: Option<RayHit> = None;
        self.for_each_posed(tick, filter, |entity, pose, hitboxes| {
            for h in hitboxes {
                let Some(t) = ray_shape(ray.origin, dir, &h.shape, pose) else {
                    continue;
                };
                if t <= ray.max_distance && best.is_none_or(|b| t < b.distance) {
                    best = Some(RayHit {
                        entity,
                        part: h.part,
                        distance: t,
                        point: ray.origin + dir * t,
                    });
                }
            }
        });
        best
    }
    /// Every filtered hitbox overlapping the sphere at a (rewound) tick, in entity order.
    pub fn overlap_sphere(
        &self,
        tick: f64,
        center: Vec3,
        radius: f32,
        filter: &QueryFilter<'_>,
    ) -> Vec<OverlapHit> {
        let mut out = Vec::new();
        if !center.is_finite() || !radius.is_finite() || radius < 0.0 {
            return out;
        }
        self.for_each_posed(tick, filter, |entity, pose, hitboxes| {
            for h in hitboxes {
                if sphere_overlaps(center, radius, &h.shape, pose) {
                    out.push(OverlapHit {
                        entity,
                        part: h.part,
                    });
                }
            }
        });
        out
    }
}

fn ray_sphere(o: Vec3, d: Vec3, c: Vec3, r: f32) -> Option<f32> {
    let m = o - c;
    let b = m.dot(d);
    let cc = m.dot(m) - r * r;
    if cc <= 0.0 {
        return Some(0.0);
    }
    if b > 0.0 {
        return None;
    }
    let disc = b * b - cc;
    (disc >= 0.0).then(|| (-b - disc.sqrt()).max(0.0))
}

fn closest_on_segment(p: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= f32::EPSILON {
        return a;
    }
    a + ab * ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
}

fn ray_capsule(o: Vec3, d: Vec3, a: Vec3, b: Vec3, r: f32) -> Option<f32> {
    if o.distance_squared(closest_on_segment(o, a, b)) <= r * r {
        return Some(0.0);
    }
    let mut best = [a, b]
        .into_iter()
        .filter_map(|c| ray_sphere(o, d, c, r))
        .reduce(f32::min);
    let ba = b - a;
    let baba = ba.dot(ba);
    if baba > f32::EPSILON {
        let oa = o - a;
        let bard = ba.dot(d);
        let baoa = ba.dot(oa);
        let k2 = baba - bard * bard;
        if k2 > 1.0e-6 * baba {
            let k1 = baba * oa.dot(d) - baoa * bard;
            let k0 = baba * oa.dot(oa) - baoa * baoa - r * r * baba;
            let h = k1 * k1 - k2 * k0;
            if h >= 0.0 {
                let t = (-k1 - h.sqrt()) / k2;
                let y = baoa + t * bard;
                if t >= 0.0 && (0.0..=baba).contains(&y) {
                    best = Some(best.map_or(t, |b| b.min(t)));
                }
            }
        }
    }
    best
}

fn ray_box(o: Vec3, d: Vec3, center: Vec3, half: Vec3, rotation: Quat) -> Option<f32> {
    let inv = rotation.inverse();
    let lo = inv * (o - center);
    let ld = inv * d;
    let (mut tmin, mut tmax) = (0.0f32, f32::INFINITY);
    for axis in 0..3 {
        let (p, v, h) = (lo[axis], ld[axis], half[axis]);
        if v.abs() <= 1.0e-9 {
            if p.abs() > h {
                return None;
            }
            continue;
        }
        let (mut t1, mut t2) = ((-h - p) / v, (h - p) / v);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        tmin = tmin.max(t1);
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    Some(tmin)
}

fn ray_shape(o: Vec3, d: Vec3, shape: &HitShape, pose: Pose) -> Option<f32> {
    match *shape {
        HitShape::Sphere { center, radius } => ray_sphere(o, d, pose.point(center), radius),
        HitShape::Capsule { a, b, radius } => {
            ray_capsule(o, d, pose.point(a), pose.point(b), radius)
        }
        HitShape::OrientedBox {
            center,
            half_extents,
            rotation,
        } => ray_box(
            o,
            d,
            pose.point(center),
            half_extents,
            pose.rotation * rotation,
        ),
    }
}

fn sphere_overlaps(c: Vec3, r: f32, shape: &HitShape, pose: Pose) -> bool {
    match *shape {
        HitShape::Sphere { center, radius } => c.distance(pose.point(center)) <= r + radius,
        HitShape::Capsule { a, b, radius } => {
            let (a, b) = (pose.point(a), pose.point(b));
            c.distance(closest_on_segment(c, a, b)) <= r + radius
        }
        HitShape::OrientedBox {
            center,
            half_extents,
            rotation,
        } => {
            let local = (pose.rotation * rotation).inverse() * (c - pose.point(center));
            let closest = local.clamp(-half_extents, half_extents);
            local.distance(closest) <= r
        }
    }
}

/// Why a client-claimed view tick was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindError {
    NonFinite,
    /// The client claims to see a tick the server has not simulated.
    FutureTick,
    /// No history is recorded.
    NoHistory,
}

/// Accepted rewind tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rewind {
    pub tick: f64,
    /// The claim was older than allowed (or than retained history) and was clamped.
    pub clamped: bool,
}

/// Converts client-claimed view ticks into bounded rewind ticks.
///
/// Trust model: the claimed view tick is attacker-controlled. It is accepted only within
/// `[current_tick - max_rewind_ticks, current_tick]` (and the retained history); future claims
/// are rejected and older claims are clamped, so a hostile client can at most pick the most
/// favourable tick inside that window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LagCompensator {
    max_rewind_ticks: f64,
}
impl LagCompensator {
    pub fn new(max_rewind_ticks: u32) -> Result<Self> {
        if max_rewind_ticks == 0 {
            return Err(PredictError::Invalid("max_rewind_ticks must be positive"));
        }
        Ok(Self {
            max_rewind_ticks: f64::from(max_rewind_ticks),
        })
    }
    /// Max rewind as wall time (rounded down to whole ticks, at least one).
    pub fn from_duration(max_rewind: Duration, tick: Duration) -> Result<Self> {
        if tick.is_zero() {
            return Err(PredictError::Invalid("tick duration must be positive"));
        }
        let ticks = (max_rewind.as_nanos() / tick.as_nanos()).clamp(1, u128::from(u32::MAX));
        Self::new(ticks as u32)
    }
    pub fn max_rewind_ticks(&self) -> f64 {
        self.max_rewind_ticks
    }
    /// Clamps a claimed view tick against the current server tick.
    pub fn rewind_tick(
        &self,
        claimed_view_tick: f64,
        current_tick: u64,
    ) -> std::result::Result<Rewind, RewindError> {
        if !claimed_view_tick.is_finite() {
            return Err(RewindError::NonFinite);
        }
        let current = current_tick as f64;
        if claimed_view_tick > current {
            return Err(RewindError::FutureTick);
        }
        let earliest = (current - self.max_rewind_ticks).max(0.0);
        Ok(if claimed_view_tick < earliest {
            Rewind {
                tick: earliest,
                clamped: true,
            }
        } else {
            Rewind {
                tick: claimed_view_tick,
                clamped: false,
            }
        })
    }
    /// [`LagCompensator::rewind_tick`] additionally clamped to the retained history.
    pub fn rewind_in(
        &self,
        history: &ColliderHistory,
        claimed_view_tick: f64,
        current_tick: u64,
    ) -> std::result::Result<Rewind, RewindError> {
        let mut rewind = self.rewind_tick(claimed_view_tick, current_tick)?;
        let (Some(oldest), Some(newest)) = (history.oldest_tick(), history.newest_tick()) else {
            return Err(RewindError::NoHistory);
        };
        if rewind.tick > newest as f64 {
            return Err(RewindError::FutureTick);
        }
        if rewind.tick < oldest as f64 {
            rewind = Rewind {
                tick: oldest as f64,
                clamped: true,
            };
        }
        Ok(rewind)
    }
    /// Rewound ray cast for a client-claimed view tick.
    pub fn raycast(
        &self,
        history: &ColliderHistory,
        claimed_view_tick: f64,
        current_tick: u64,
        ray: Ray,
        filter: &QueryFilter<'_>,
    ) -> std::result::Result<(Rewind, Option<RayHit>), RewindError> {
        let rewind = self.rewind_in(history, claimed_view_tick, current_tick)?;
        Ok((rewind, history.raycast(rewind.tick, ray, filter)))
    }
    /// Rewound sphere overlap for a client-claimed view tick.
    pub fn overlap_sphere(
        &self,
        history: &ColliderHistory,
        claimed_view_tick: f64,
        current_tick: u64,
        center: Vec3,
        radius: f32,
        filter: &QueryFilter<'_>,
    ) -> std::result::Result<(Rewind, Vec<OverlapHit>), RewindError> {
        let rewind = self.rewind_in(history, claimed_view_tick, current_tick)?;
        Ok((
            rewind,
            history.overlap_sphere(rewind.tick, center, radius, filter),
        ))
    }
}
