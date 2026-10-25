//! Native spatial query API: ray casts, shape casts, overlaps and point queries over the
//! synced physics bodies, with `Entity` results.
//!
//! Every function takes the [`Dimension`] to search, because 2D and 3D bodies live in
//! separate engine-physics worlds. 2D queries use the XY plane: the Z of positions and
//! directions is ignored and reported hit Z is 0. Bodies exist once the simulation synced
//! them (scene start, then every tick), and queries are exact against the current engine
//! state, so a body is queryable right after the sync that inserted it and gone right after
//! the sync that removed it. Results sort by `(distance, entity bits)`.
//! `docs/physics-queries.md` describes the approach.
use super::super::{invalid, mapped};
use super::{Dimension, body_entity, body_id, service};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::{dim2, dim3};
use glam::{Quat, Vec3};

/// Which sensor colliders a query reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SensorFilter {
    /// Solid and sensor colliders (the default).
    #[default]
    Include,
    Exclude,
    Only,
}
/// Which body kinds a query reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyKinds {
    pub fixed: bool,
    pub dynamic: bool,
    pub kinematic: bool,
}
impl BodyKinds {
    pub const ALL: Self = Self {
        fixed: true,
        dynamic: true,
        kinematic: true,
    };
}
impl Default for BodyKinds {
    fn default() -> Self {
        Self::ALL
    }
}
/// Which colliders a query may report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryFilter {
    /// Collider layer membership bits must intersect this mask (see `layer_mask`).
    pub mask: u32,
    /// Entities that are never reported (at most 1024).
    pub exclude: Vec<Entity>,
    pub sensors: SensorFilter,
    pub kinds: BodyKinds,
}
impl Default for QueryFilter {
    fn default() -> Self {
        Self {
            mask: u32::MAX,
            exclude: Vec::new(),
            sensors: SensorFilter::Include,
            kinds: BodyKinds::ALL,
        }
    }
}
impl QueryFilter {
    pub fn with_mask(mask: u32) -> Self {
        Self {
            mask,
            ..Self::default()
        }
    }
    fn ids(&self) -> Vec<u64> {
        self.exclude.iter().map(|e| body_id(*e)).collect()
    }
    fn two(&self) -> dim2::QueryFilter {
        dim2::QueryFilter {
            mask: self.mask,
            exclude: self.ids(),
            sensors: match self.sensors {
                SensorFilter::Include => dim2::SensorFilter::Include,
                SensorFilter::Exclude => dim2::SensorFilter::Exclude,
                SensorFilter::Only => dim2::SensorFilter::Only,
            },
            kinds: dim2::KindFilter {
                fixed: self.kinds.fixed,
                dynamic: self.kinds.dynamic,
                kinematic: self.kinds.kinematic,
            },
        }
    }
    fn three(&self) -> dim3::QueryFilter {
        dim3::QueryFilter {
            mask: self.mask,
            exclude: self.ids(),
            sensors: match self.sensors {
                SensorFilter::Include => dim3::SensorFilter::Include,
                SensorFilter::Exclude => dim3::SensorFilter::Exclude,
                SensorFilter::Only => dim3::SensorFilter::Only,
            },
            kinds: dim3::KindFilter {
                fixed: self.kinds.fixed,
                dynamic: self.kinds.dynamic,
                kinematic: self.kinds.kinematic,
            },
        }
    }
}

/// Geometry swept or tested by shape queries, in its own local frame.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryShape {
    Ball {
        radius: f32,
    },
    Box {
        half_extents: Vec3,
    },
    /// Y-axis capsule.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Y-axis cylinder (3D only).
    Cylinder {
        half_height: f32,
        radius: f32,
    },
    ConvexHull {
        points: Vec<Vec3>,
    },
}
/// World pose of a query shape. 2D queries use `position.xy` and the rotation about Z.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueryPose {
    pub position: Vec3,
    pub rotation: Quat,
}
impl QueryPose {
    pub fn at(position: Vec3) -> Self {
        Self {
            position,
            rotation: Quat::IDENTITY,
        }
    }
}

/// A ray hit. `normal` faces the ray side of the surface; a ray starting inside a solid
/// collider reports distance 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub entity: Entity,
    pub distance: f32,
    pub point: Vec3,
    pub normal: Vec3,
}
/// A swept-shape impact: `distance` is the travel until first touch, `point` and `normal`
/// the touching point and outward surface normal on the hit collider, `shape_point` the
/// touching point on the swept shape. `penetrating` marks shapes already overlapping at
/// distance 0 (point and normal are then approximate).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeHit {
    pub entity: Entity,
    pub distance: f32,
    pub point: Vec3,
    pub normal: Vec3,
    pub shape_point: Vec3,
    pub penetrating: bool,
}
/// A collider intersecting a query shape; `distance` runs from the query position to the
/// body origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlapHit {
    pub entity: Entity,
    pub distance: f32,
}
/// Nearest point on a collider; `distance` is 0 and `point` the query point when inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointHit {
    pub entity: Entity,
    pub point: Vec3,
    pub distance: f32,
    pub inside: bool,
}
/// Sorted multi-hit result; `truncated` marks that more than `limit` hits existed.
#[derive(Clone, Debug, PartialEq)]
pub struct Hits<T> {
    pub hits: Vec<T>,
    pub truncated: bool,
}
impl<T> Hits<T> {
    fn from<U>(source: engine_physics::dim3::QueryHits<U>, map: impl Fn(U) -> Option<T>) -> Self {
        Self {
            hits: source.hits.into_iter().filter_map(map).collect(),
            truncated: source.truncated,
        }
    }
}

fn lift(p: [f32; 2]) -> Vec3 {
    Vec3::new(p[0], p[1], 0.)
}
fn planar_angle(rotation: Quat) -> Result<f32> {
    if !rotation.is_finite() || rotation.x.abs() > 0.0001 || rotation.y.abs() > 0.0001 {
        return Err(invalid("2D query rotation must be about Z only"));
    }
    Ok(2. * rotation.z.atan2(rotation.w))
}
fn shape2(shape: &QueryShape) -> Result<dim2::Shape> {
    Ok(match shape {
        QueryShape::Ball { radius } => dim2::Shape::Ball { radius: *radius },
        QueryShape::Box { half_extents } => dim2::Shape::Box {
            half_extents: [half_extents.x, half_extents.y],
        },
        QueryShape::Capsule {
            half_height,
            radius,
        } => dim2::Shape::Capsule {
            half_height: *half_height,
            radius: *radius,
        },
        QueryShape::Cylinder { .. } => return Err(invalid("Cylinder query shapes are 3D only")),
        QueryShape::ConvexHull { points } => dim2::Shape::ConvexHull {
            points: points.iter().map(|p| [p.x, p.y]).collect(),
        },
    })
}
fn shape3(shape: &QueryShape) -> dim3::Shape {
    match shape {
        QueryShape::Ball { radius } => dim3::Shape::Ball { radius: *radius },
        QueryShape::Box { half_extents } => dim3::Shape::Box {
            half_extents: half_extents.to_array(),
        },
        QueryShape::Capsule {
            half_height,
            radius,
        } => dim3::Shape::Capsule {
            half_height: *half_height,
            radius: *radius,
        },
        QueryShape::Cylinder {
            half_height,
            radius,
        } => dim3::Shape::Cylinder {
            half_height: *half_height,
            radius: *radius,
        },
        QueryShape::ConvexHull { points } => dim3::Shape::ConvexHull {
            points: points.iter().map(|p| p.to_array()).collect(),
        },
    }
}

/// Closest ray hit. `direction` is normalized, so distances are world units.
pub fn ray_cast(
    world: &World,
    dimension: Dimension,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
    filter: &QueryFilter,
) -> Result<Option<RayHit>> {
    let physics = service(world)?;
    Ok(match dimension {
        Dimension::Two => physics
            .worlds
            .two
            .raycast_hit(
                [origin.x, origin.y],
                [direction.x, direction.y],
                max_distance,
                &filter.two(),
            )
            .map_err(mapped)?
            .and_then(ray2),
        Dimension::Three => physics
            .worlds
            .three
            .raycast_hit(
                origin.to_array(),
                direction.to_array(),
                max_distance,
                &filter.three(),
            )
            .map_err(mapped)?
            .and_then(ray3),
    })
}
/// Every ray hit sorted by `(distance, entity)`; the nearest `limit` (1..=65536) are kept.
pub fn ray_cast_all(
    world: &World,
    dimension: Dimension,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
    filter: &QueryFilter,
    limit: usize,
) -> Result<Hits<RayHit>> {
    let physics = service(world)?;
    Ok(match dimension {
        Dimension::Two => {
            let hits = physics
                .worlds
                .two
                .raycast_all(
                    [origin.x, origin.y],
                    [direction.x, direction.y],
                    max_distance,
                    &filter.two(),
                    limit,
                )
                .map_err(mapped)?;
            Hits {
                hits: hits.hits.into_iter().filter_map(ray2).collect(),
                truncated: hits.truncated,
            }
        }
        Dimension::Three => Hits::from(
            physics
                .worlds
                .three
                .raycast_all(
                    origin.to_array(),
                    direction.to_array(),
                    max_distance,
                    &filter.three(),
                    limit,
                )
                .map_err(mapped)?,
            ray3,
        ),
    })
}
fn ray2(hit: dim2::RayCastHit) -> Option<RayHit> {
    Some(RayHit {
        entity: body_entity(hit.id)?,
        distance: hit.distance,
        point: lift(hit.point),
        normal: lift(hit.normal),
    })
}
fn ray3(hit: dim3::RayCastHit) -> Option<RayHit> {
    Some(RayHit {
        entity: body_entity(hit.id)?,
        distance: hit.distance,
        point: Vec3::from(hit.point),
        normal: Vec3::from(hit.normal),
    })
}

/// Sweeps `shape` from `pose` along `direction` (normalized) for up to `max_distance`; the
/// orientation stays fixed. Returns the first impact.
pub fn shape_cast(
    world: &World,
    dimension: Dimension,
    shape: &QueryShape,
    pose: QueryPose,
    direction: Vec3,
    max_distance: f32,
    filter: &QueryFilter,
) -> Result<Option<ShapeHit>> {
    Ok(shape_cast_all(
        world,
        dimension,
        shape,
        pose,
        direction,
        max_distance,
        filter,
        1,
    )?
    .hits
    .into_iter()
    .next())
}
/// Every collider the sweep touches, sorted by `(distance, entity)`, nearest `limit` kept.
#[allow(clippy::too_many_arguments)]
pub fn shape_cast_all(
    world: &World,
    dimension: Dimension,
    shape: &QueryShape,
    pose: QueryPose,
    direction: Vec3,
    max_distance: f32,
    filter: &QueryFilter,
    limit: usize,
) -> Result<Hits<ShapeHit>> {
    let physics = service(world)?;
    Ok(match dimension {
        Dimension::Two => {
            let hits = physics
                .worlds
                .two
                .cast_shape_all(
                    &shape2(shape)?,
                    [pose.position.x, pose.position.y],
                    planar_angle(pose.rotation)?,
                    [direction.x, direction.y],
                    max_distance,
                    &filter.two(),
                    limit,
                )
                .map_err(mapped)?;
            Hits {
                hits: hits
                    .hits
                    .into_iter()
                    .filter_map(|h| {
                        Some(ShapeHit {
                            entity: body_entity(h.id)?,
                            distance: h.distance,
                            point: lift(h.point),
                            normal: lift(h.normal),
                            shape_point: lift(h.shape_point),
                            penetrating: h.penetrating,
                        })
                    })
                    .collect(),
                truncated: hits.truncated,
            }
        }
        Dimension::Three => Hits::from(
            physics
                .worlds
                .three
                .cast_shape_all(
                    &shape3(shape),
                    pose.position.to_array(),
                    pose.rotation.to_array(),
                    direction.to_array(),
                    max_distance,
                    &filter.three(),
                    limit,
                )
                .map_err(mapped)?,
            |h| {
                Some(ShapeHit {
                    entity: body_entity(h.id)?,
                    distance: h.distance,
                    point: Vec3::from(h.point),
                    normal: Vec3::from(h.normal),
                    shape_point: Vec3::from(h.shape_point),
                    penetrating: h.penetrating,
                })
            },
        ),
    })
}
/// Colliders intersecting `shape` at `pose`, sorted by the distance from the query position
/// to each body origin then entity; the nearest `limit` are kept.
pub fn overlap(
    world: &World,
    dimension: Dimension,
    shape: &QueryShape,
    pose: QueryPose,
    filter: &QueryFilter,
    limit: usize,
) -> Result<Hits<OverlapHit>> {
    let physics = service(world)?;
    Ok(match dimension {
        Dimension::Two => {
            let hits = physics
                .worlds
                .two
                .overlap_shape(
                    &shape2(shape)?,
                    [pose.position.x, pose.position.y],
                    planar_angle(pose.rotation)?,
                    &filter.two(),
                    limit,
                )
                .map_err(mapped)?;
            Hits {
                hits: hits
                    .hits
                    .into_iter()
                    .filter_map(|h| {
                        Some(OverlapHit {
                            entity: body_entity(h.id)?,
                            distance: h.distance,
                        })
                    })
                    .collect(),
                truncated: hits.truncated,
            }
        }
        Dimension::Three => Hits::from(
            physics
                .worlds
                .three
                .overlap_shape(
                    &shape3(shape),
                    pose.position.to_array(),
                    pose.rotation.to_array(),
                    &filter.three(),
                    limit,
                )
                .map_err(mapped)?,
            |h| {
                Some(OverlapHit {
                    entity: body_entity(h.id)?,
                    distance: h.distance,
                })
            },
        ),
    })
}
/// Nearest collider to `point`; distance 0 and `inside` when the point is within a collider.
pub fn project_point(
    world: &World,
    dimension: Dimension,
    point: Vec3,
    filter: &QueryFilter,
) -> Result<Option<PointHit>> {
    let physics = service(world)?;
    Ok(match dimension {
        Dimension::Two => physics
            .worlds
            .two
            .project_point([point.x, point.y], &filter.two())
            .map_err(mapped)?
            .and_then(|h| {
                Some(PointHit {
                    entity: body_entity(h.id)?,
                    point: lift(h.point),
                    distance: h.distance,
                    inside: h.inside,
                })
            }),
        Dimension::Three => physics
            .worlds
            .three
            .project_point(point.to_array(), &filter.three())
            .map_err(mapped)?
            .and_then(point3),
    })
}
fn point3(h: dim3::PointHit) -> Option<PointHit> {
    Some(PointHit {
        entity: body_entity(h.id)?,
        point: Vec3::from(h.point),
        distance: h.distance,
        inside: h.inside,
    })
}
/// Colliders containing `point`, ordered by entity; the first `limit` are kept.
pub fn contains_point(
    world: &World,
    dimension: Dimension,
    point: Vec3,
    filter: &QueryFilter,
    limit: usize,
) -> Result<Hits<PointHit>> {
    let physics = service(world)?;
    Ok(match dimension {
        Dimension::Two => {
            let hits = physics
                .worlds
                .two
                .contains_point([point.x, point.y], &filter.two(), limit)
                .map_err(mapped)?;
            Hits {
                hits: hits
                    .hits
                    .into_iter()
                    .filter_map(|h| {
                        Some(PointHit {
                            entity: body_entity(h.id)?,
                            point: lift(h.point),
                            distance: h.distance,
                            inside: h.inside,
                        })
                    })
                    .collect(),
                truncated: hits.truncated,
            }
        }
        Dimension::Three => Hits::from(
            physics
                .worlds
                .three
                .contains_point(point.to_array(), &filter.three(), limit)
                .map_err(mapped)?,
            point3,
        ),
    })
}
