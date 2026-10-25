//! 2D debug shape descriptions and outline geometry. The dimension-generic view, options and
//! wireframe builder are in `shared.rs`, compiled once per dimension.
use super::{Orientation, Position, rotation, vector};
use rapier2d::parry::shape::{Shape as ParryShape, ShapeType};
use std::f32::consts::{PI, TAU};

#[path = "shared.rs"]
mod shared;
pub use shared::*;

/// Circle subdivision of drawn balls and capsules.
const CIRCLE_STEPS: usize = 24;

/// Collider geometry in body-local coordinates. Hulls, polylines and heightfields are
/// described only by their local bounds so a view stays small.
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
    } else if let Some(compound) = shape.as_compound() {
        DebugShape::Compound {
            parts: compound
                .shapes()
                .iter()
                .map(|(pose, part)| DebugPart {
                    position: pose.translation.to_array(),
                    rotation: pose.rotation.angle(),
                    shape: describe(&**part),
                })
                .collect(),
        }
    } else {
        let bounds = shape.compute_local_aabb();
        DebugShape::Bounded {
            kind: match shape.shape_type() {
                ShapeType::ConvexPolygon => "convex_hull",
                ShapeType::TriMesh | ShapeType::Polyline => "trimesh",
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
        DebugShape::Ball { radius } => {
            shared::arc([0.; 2], (0, 1), *radius, (0., TAU), CIRCLE_STEPS)
        }
        DebugShape::Box { half_extents: h } => shared::box_edges([-h[0], -h[1]], *h),
        DebugShape::Capsule {
            half_height: h,
            radius: r,
        } => {
            let mut lines = shared::arc([0., *h], (0, 1), *r, (0., PI), CIRCLE_STEPS / 2);
            lines.extend(shared::arc(
                [0., -h],
                (0, 1),
                *r,
                (PI, TAU),
                CIRCLE_STEPS / 2,
            ));
            lines.push([[*r, -h], [*r, *h]]);
            lines.push([[-r, -h], [-r, *h]]);
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
