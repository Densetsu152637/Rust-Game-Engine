//! 3D debug shape descriptions and outline geometry. The dimension-generic view, options and
//! wireframe builder are in `shared.rs`, compiled once per dimension.
use super::{Orientation, Position, rotation, vector};
use rapier3d::parry::shape::{Shape as ParryShape, ShapeType};
use std::f32::consts::{PI, TAU};

#[path = "shared.rs"]
mod shared;
pub use shared::*;

/// Circle subdivision of drawn balls, cylinders and capsules.
const CIRCLE_STEPS: usize = 24;

/// Collider geometry in body-local coordinates. Hulls, meshes and heightfields are described
/// only by their local bounds so a view stays small.
#[derive(Clone, Debug, PartialEq)]
pub enum DebugShape {
    Ball {
        radius: f32,
    },
    Box {
        half_extents: Position,
    },
    /// Y-axis capsule.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Y-axis cylinder.
    Cylinder {
        half_height: f32,
        radius: f32,
    },
    Compound {
        parts: Vec<DebugPart>,
    },
    /// `kind` is `convex_hull`, `trimesh`, `heightfield` or `other`.
    Bounded {
        kind: &'static str,
        min: Position,
        max: Position,
    },
}
/// One compound part in body-local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugPart {
    pub position: Position,
    pub rotation: Orientation,
    pub shape: DebugShape,
}
pub(super) fn describe(shape: &dyn ParryShape) -> DebugShape {
    if let Some(ball) = shape.as_ball() {
        DebugShape::Ball {
            radius: ball.radius,
        }
    } else if let Some(cuboid) = shape.as_cuboid() {
        DebugShape::Box {
            half_extents: cuboid.half_extents.to_array(),
        }
    } else if let Some(capsule) = shape.as_capsule() {
        DebugShape::Capsule {
            half_height: capsule.segment.b.y,
            radius: capsule.radius,
        }
    } else if let Some(cylinder) = shape.as_cylinder() {
        DebugShape::Cylinder {
            half_height: cylinder.half_height,
            radius: cylinder.radius,
        }
    } else if let Some(compound) = shape.as_compound() {
        DebugShape::Compound {
            parts: compound
                .shapes()
                .iter()
                .map(|(pose, part)| DebugPart {
                    position: pose.translation.to_array(),
                    rotation: pose.rotation.to_array(),
                    shape: describe(&**part),
                })
                .collect(),
        }
    } else {
        let bounds = shape.compute_local_aabb();
        DebugShape::Bounded {
            kind: match shape.shape_type() {
                ShapeType::ConvexPolyhedron => "convex_hull",
                ShapeType::TriMesh => "trimesh",
                ShapeType::HeightField => "heightfield",
                _ => "other",
            },
            min: bounds.mins.to_array(),
            max: bounds.maxs.to_array(),
        }
    }
}
/// Body-local point to world space.
pub(super) fn to_world(position: Position, orientation: Orientation, local: Position) -> Position {
    (vector(position) + rotation(orientation) * vector(local)).to_array()
}
/// Outline segments of `shape` in its own local frame.
pub(super) fn local_segments(shape: &DebugShape) -> Vec<[Position; 2]> {
    match shape {
        DebugShape::Ball { radius } => [(0, 1), (1, 2), (2, 0)]
            .into_iter()
            .flat_map(|plane| shared::arc([0.; 3], plane, *radius, (0., TAU), CIRCLE_STEPS))
            .collect(),
        DebugShape::Box { half_extents: h } => shared::box_edges([-h[0], -h[1], -h[2]], *h),
        DebugShape::Capsule {
            half_height: h,
            radius: r,
        } => {
            let mut lines = Vec::new();
            for y in [-h, *h] {
                lines.extend(shared::arc(
                    [0., y, 0.],
                    (0, 2),
                    *r,
                    (0., TAU),
                    CIRCLE_STEPS,
                ));
            }
            // Hemispherical caps in the XY and ZY planes.
            for plane in [(0, 1), (2, 1)] {
                lines.extend(shared::arc(
                    [0., *h, 0.],
                    plane,
                    *r,
                    (0., PI),
                    CIRCLE_STEPS / 2,
                ));
                lines.extend(shared::arc(
                    [0., -h, 0.],
                    plane,
                    *r,
                    (PI, TAU),
                    CIRCLE_STEPS / 2,
                ));
            }
            lines.extend(sides(*h, *r));
            lines
        }
        DebugShape::Cylinder {
            half_height: h,
            radius: r,
        } => {
            let mut lines = Vec::new();
            for y in [-h, *h] {
                lines.extend(shared::arc(
                    [0., y, 0.],
                    (0, 2),
                    *r,
                    (0., TAU),
                    CIRCLE_STEPS,
                ));
            }
            lines.extend(sides(*h, *r));
            lines
        }
        DebugShape::Compound { parts } => parts
            .iter()
            .flat_map(|part| {
                local_segments(&part.shape).into_iter().map(|[a, b]| {
                    [
                        to_world(part.position, part.rotation, a),
                        to_world(part.position, part.rotation, b),
                    ]
                })
            })
            .collect(),
        DebugShape::Bounded { min, max, .. } => shared::box_edges(*min, *max),
    }
}
/// Four vertical lines joining the end circles of a capsule or cylinder.
fn sides(half_height: f32, radius: f32) -> impl Iterator<Item = [Position; 2]> {
    [[radius, 0.], [-radius, 0.], [0., radius], [0., -radius]]
        .into_iter()
        .map(move |[x, z]| [[x, -half_height, z], [x, half_height, z]])
}
