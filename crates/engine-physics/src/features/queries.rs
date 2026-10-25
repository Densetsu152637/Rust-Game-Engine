//! Spatial queries: ray casts, shape casts, overlaps and point queries.
//!
//! Dimension-generic: `dim2.rs` and `dim3.rs` each include this file with
//! `#[path = "features/queries.rs"] mod queries;`, so `QueryFilter`, the hit types and the
//! `World` methods exist once per dimension.
//!
//! # Freshness
//!
//! Rapier's own query pipeline (the broad-phase BVH) is rebuilt by `step`, so a body inserted
//! or removed between steps would be invisible to it. These queries therefore scan the
//! collider set directly, using a cheap AABB rejection for shape casts and overlaps, and are
//! exact immediately after `insert`/`remove` (the guarantee `raycast` already gives). The cost
//! is linear in the body count, which the world caps at 100 000.
//!
//! # Ordering
//!
//! Every multi-hit result is sorted by `(distance, id)` with `f32::total_cmp` and only then
//! truncated to the caller's limit, so output is deterministic and truncation keeps the
//! nearest hits. `distance` is the ray or sweep distance for casts, the distance from the
//! query pose to the body origin for overlaps, and the distance to the surface for points.
use super::*;
use crate::{Error, Result};
use rapier::parry::query::{ShapeCastOptions, ShapeCastStatus, cast_shapes, intersection_test};

/// Most ids accepted in [`QueryFilter::exclude`].
pub const MAX_QUERY_EXCLUDES: usize = 1024;
/// Largest `limit` accepted by multi-hit queries.
pub const MAX_QUERY_HITS: usize = 65_536;

/// Which sensor colliders a query reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SensorFilter {
    /// Solid and sensor colliders (the default, matching `World::raycast`).
    #[default]
    Include,
    /// Only solid colliders.
    Exclude,
    /// Only sensor colliders.
    Only,
}
/// Which body kinds a query reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindFilter {
    pub fixed: bool,
    pub dynamic: bool,
    pub kinematic: bool,
}
impl KindFilter {
    pub const ALL: Self = Self {
        fixed: true,
        dynamic: true,
        kinematic: true,
    };
    fn allows(self, kind: crate::BodyKind) -> bool {
        match kind {
            crate::BodyKind::Fixed => self.fixed,
            crate::BodyKind::Dynamic => self.dynamic,
            crate::BodyKind::Kinematic => self.kinematic,
        }
    }
}
impl Default for KindFilter {
    fn default() -> Self {
        Self::ALL
    }
}
/// Which colliders a query may report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryFilter {
    /// A collider is eligible when its membership bits intersect `mask`.
    pub mask: u32,
    /// Body ids that are never reported (at most [`MAX_QUERY_EXCLUDES`]).
    pub exclude: Vec<u64>,
    pub sensors: SensorFilter,
    pub kinds: KindFilter,
}
impl Default for QueryFilter {
    fn default() -> Self {
        Self {
            mask: u32::MAX,
            exclude: Vec::new(),
            sensors: SensorFilter::Include,
            kinds: KindFilter::ALL,
        }
    }
}
impl QueryFilter {
    /// Everything on the layers selected by `mask`.
    pub fn with_mask(mask: u32) -> Self {
        Self {
            mask,
            ..Self::default()
        }
    }
    fn validate(&self) -> Result<()> {
        if self.exclude.len() > MAX_QUERY_EXCLUDES {
            return Err(Error("query excludes at most 1024 bodies"));
        }
        Ok(())
    }
}

/// Closest or sorted ray intersection. `normal` is the surface normal facing the ray origin
/// side of the hit shape; a ray starting inside a solid collider reports distance 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayCastHit {
    pub id: u64,
    pub distance: f32,
    pub point: Position,
    pub normal: Position,
}
/// A swept-shape impact. `distance` is the travel distance until first touch (the time of
/// impact along the unit direction), `point` the touching point on the hit collider, `normal`
/// that collider's outward surface normal there and `shape_point` the touching point on the
/// swept shape. `penetrating` marks shapes already overlapping at distance 0, whose
/// `point`/`normal` come from the closest features and are only approximate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeCastHit {
    pub id: u64,
    pub distance: f32,
    pub point: Position,
    pub normal: Position,
    pub shape_point: Position,
    pub penetrating: bool,
}
/// A collider intersecting a query shape. `distance` is the query-origin to body-origin distance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlapHit {
    pub id: u64,
    pub distance: f32,
}
/// Nearest point on a collider. `distance` is 0 and `point` is the query point when inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointHit {
    pub id: u64,
    pub point: Position,
    pub distance: f32,
    pub inside: bool,
}
/// Sorted multi-hit result; `truncated` is set when more hits than `limit` existed.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryHits<T> {
    pub hits: Vec<T>,
    pub truncated: bool,
}

fn check_limit(limit: usize) -> Result<()> {
    if !(1..=MAX_QUERY_HITS).contains(&limit) {
        return Err(Error("query limit must be 1..=65536"));
    }
    Ok(())
}
fn sort_and_limit<T>(
    mut hits: Vec<T>,
    limit: usize,
    key: impl Fn(&T) -> (f32, u64),
) -> QueryHits<T> {
    hits.sort_by(|a, b| {
        let (da, ia) = key(a);
        let (db, ib) = key(b);
        da.total_cmp(&db).then(ia.cmp(&ib))
    });
    let truncated = hits.len() > limit;
    hits.truncate(limit);
    QueryHits { hits, truncated }
}
fn unit(direction: Position, max_distance: f32) -> Result<Vector> {
    if !crate::finite(&direction) || !max_distance.is_finite() || max_distance < 0.0 {
        return Err(Error("invalid query direction or distance"));
    }
    let direction = vector(direction);
    if direction.length_squared() < 1e-12 {
        return Err(Error("zero query direction"));
    }
    Ok(direction.normalize())
}
fn query_pose(position: Position, orientation: Orientation) -> Result<Pose> {
    if !crate::finite(&position) || !valid_rotation(orientation) {
        return Err(Error("invalid query pose"));
    }
    Ok(Pose::from_parts(vector(position), rotation(orientation)))
}

impl World {
    /// Every eligible collider as `(body id, world pose, collider)`, in arena order. Callers
    /// sort their results, so this order never reaches the output.
    fn eligible<'a>(
        &'a self,
        filter: &'a QueryFilter,
    ) -> impl Iterator<Item = (u64, Pose, &'a Collider)> + 'a {
        let core = &self.core;
        core.rapier
            .colliders
            .iter()
            .filter_map(move |(handle, collider)| {
                let id = core.collider_body_id(handle)?;
                if collider.collision_groups().memberships.bits() & filter.mask == 0
                    || filter.exclude.contains(&id)
                {
                    return None;
                }
                match (filter.sensors, collider.is_sensor()) {
                    (SensorFilter::Exclude, true) | (SensorFilter::Only, false) => return None,
                    _ => {}
                }
                let body = core.rapier.bodies.get(collider.parent()?)?;
                let kind = if body.is_fixed() {
                    crate::BodyKind::Fixed
                } else if body.is_kinematic() {
                    crate::BodyKind::Kinematic
                } else {
                    crate::BodyKind::Dynamic
                };
                filter
                    .kinds
                    .allows(kind)
                    .then(|| (id, *body.position(), collider))
            })
    }
    /// Closest ray hit with point and normal. `direction` is normalized, so distances are
    /// world units. Equal distances resolve to the lowest id.
    pub fn raycast_hit(
        &self,
        origin: Position,
        direction: Position,
        max_distance: f32,
        filter: &QueryFilter,
    ) -> Result<Option<RayCastHit>> {
        Ok(self
            .ray_hits(origin, direction, max_distance, filter)?
            .into_iter()
            .min_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id))))
    }
    /// Every ray hit sorted by `(distance, id)`, nearest `limit` kept.
    pub fn raycast_all(
        &self,
        origin: Position,
        direction: Position,
        max_distance: f32,
        filter: &QueryFilter,
        limit: usize,
    ) -> Result<QueryHits<RayCastHit>> {
        check_limit(limit)?;
        let hits = self.ray_hits(origin, direction, max_distance, filter)?;
        Ok(sort_and_limit(hits, limit, |h| (h.distance, h.id)))
    }
    fn ray_hits(
        &self,
        origin: Position,
        direction: Position,
        max_distance: f32,
        filter: &QueryFilter,
    ) -> Result<Vec<RayCastHit>> {
        filter.validate()?;
        if !crate::finite(&origin) {
            return Err(Error("invalid ray"));
        }
        let direction = unit(direction, max_distance)?;
        let ray = Ray::new(vector(origin), direction);
        Ok(self
            .eligible(filter)
            .filter_map(|(id, pose, collider)| {
                let hit =
                    collider
                        .shape()
                        .cast_ray_and_get_normal(&pose, &ray, max_distance, true)?;
                Some(RayCastHit {
                    id,
                    distance: hit.time_of_impact,
                    point: ray.point_at(hit.time_of_impact).to_array(),
                    normal: hit.normal.to_array(),
                })
            })
            .collect())
    }
    /// Sweeps `shape` posed at `position`/`orientation` along `direction` (normalized) for up
    /// to `max_distance` and returns the first impact, equal distances resolving to the
    /// lowest id. The orientation stays fixed during the sweep.
    pub fn cast_shape(
        &self,
        shape: &Shape,
        position: Position,
        orientation: Orientation,
        direction: Position,
        max_distance: f32,
        filter: &QueryFilter,
    ) -> Result<Option<ShapeCastHit>> {
        Ok(self
            .shape_hits(
                shape,
                position,
                orientation,
                direction,
                max_distance,
                filter,
            )?
            .into_iter()
            .min_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id))))
    }
    /// Every collider the sweep touches, sorted by `(distance, id)`, nearest `limit` kept.
    #[allow(clippy::too_many_arguments)]
    pub fn cast_shape_all(
        &self,
        shape: &Shape,
        position: Position,
        orientation: Orientation,
        direction: Position,
        max_distance: f32,
        filter: &QueryFilter,
        limit: usize,
    ) -> Result<QueryHits<ShapeCastHit>> {
        check_limit(limit)?;
        let hits = self.shape_hits(
            shape,
            position,
            orientation,
            direction,
            max_distance,
            filter,
        )?;
        Ok(sort_and_limit(hits, limit, |h| (h.distance, h.id)))
    }
    fn shape_hits(
        &self,
        shape: &Shape,
        position: Position,
        orientation: Orientation,
        direction: Position,
        max_distance: f32,
        filter: &QueryFilter,
    ) -> Result<Vec<ShapeCastHit>> {
        filter.validate()?;
        let direction = unit(direction, max_distance)?;
        let pose = query_pose(position, orientation)?;
        let shared = shape.shared()?;
        let start = shared.compute_aabb(&pose);
        let end = shared.compute_aabb(&Pose::from_parts(
            pose.translation + direction * max_distance,
            pose.rotation,
        ));
        let swept = start.merged(&end);
        let options = ShapeCastOptions {
            max_time_of_impact: max_distance,
            target_distance: 0.0,
            stop_at_penetration: true,
            compute_impact_geometry_on_penetration: true,
        };
        let mut hits = Vec::new();
        for (id, body_pose, collider) in self.eligible(filter) {
            if !collider.shape().compute_aabb(&body_pose).intersects(&swept) {
                continue;
            }
            let hit = cast_shapes(
                &pose,
                direction,
                &*shared,
                &body_pose,
                Vector::ZERO,
                collider.shape(),
                options,
            )
            .map_err(|_| Error("shape cast is unsupported for this shape pair"))?;
            let Some(hit) = hit else { continue };
            hits.push(ShapeCastHit {
                id,
                distance: hit.time_of_impact,
                point: (body_pose * hit.witness2).to_array(),
                normal: (body_pose.rotation * hit.normal2).to_array(),
                shape_point: (pose * hit.witness1).to_array(),
                penetrating: hit.status == ShapeCastStatus::PenetratingOrWithinTargetDist,
            });
        }
        Ok(hits)
    }
    /// Colliders intersecting `shape` posed at `position`/`orientation`, sorted by the
    /// distance from the query position to each body origin, then id; nearest `limit` kept.
    pub fn overlap_shape(
        &self,
        shape: &Shape,
        position: Position,
        orientation: Orientation,
        filter: &QueryFilter,
        limit: usize,
    ) -> Result<QueryHits<OverlapHit>> {
        check_limit(limit)?;
        filter.validate()?;
        let pose = query_pose(position, orientation)?;
        let shared = shape.shared()?;
        let aabb = shared.compute_aabb(&pose);
        let mut hits = Vec::new();
        for (id, body_pose, collider) in self.eligible(filter) {
            if !collider.shape().compute_aabb(&body_pose).intersects(&aabb) {
                continue;
            }
            let touching = intersection_test(&pose, &*shared, &body_pose, collider.shape())
                .map_err(|_| Error("overlap is unsupported for this shape pair"))?;
            if touching.intersecting {
                hits.push(OverlapHit {
                    id,
                    distance: (body_pose.translation - pose.translation).length(),
                });
            }
        }
        Ok(sort_and_limit(hits, limit, |h| (h.distance, h.id)))
    }
    /// Nearest collider to `point` (distance 0 when inside), equal distances to the lowest id.
    pub fn project_point(&self, point: Position, filter: &QueryFilter) -> Result<Option<PointHit>> {
        Ok(self
            .point_hits(point, filter)?
            .into_iter()
            .min_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id))))
    }
    /// Colliders containing `point`, ordered by id, first `limit` kept.
    pub fn contains_point(
        &self,
        point: Position,
        filter: &QueryFilter,
        limit: usize,
    ) -> Result<QueryHits<PointHit>> {
        check_limit(limit)?;
        let hits = self
            .point_hits(point, filter)?
            .into_iter()
            .filter(|h| h.inside)
            .collect();
        Ok(sort_and_limit(hits, limit, |h| (h.distance, h.id)))
    }
    fn point_hits(&self, point: Position, filter: &QueryFilter) -> Result<Vec<PointHit>> {
        filter.validate()?;
        if !crate::finite(&point) {
            return Err(Error("invalid query point"));
        }
        let target = vector(point);
        Ok(self
            .eligible(filter)
            .map(|(id, pose, collider)| {
                let projection = collider.shape().project_point(&pose, target, true);
                PointHit {
                    id,
                    point: projection.point.to_array(),
                    distance: if projection.is_inside {
                        0.0
                    } else {
                        (projection.point - target).length()
                    },
                    inside: projection.is_inside,
                }
            })
            .collect())
    }
}
