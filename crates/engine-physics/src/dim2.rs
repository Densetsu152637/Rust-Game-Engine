//! Two-dimensional world: XY meters and radians about Z.
use rapier::math::Pose;
use rapier::prelude::*;
use rapier2d as rapier;
pub type Position = [f32; 2];
pub type Orientation = f32;
pub type AngularImpulse = f32;
pub type AngularVelocity = f32;
#[path = "world.rs"]
mod world;
pub use world::{BodyVelocity, World};
use world::{Core, Extension};
// Feature registrations: one `#[path] mod` item each (docs/physics.md#extension-seams).
#[path = "features/joints/dim2.rs"]
mod joints;
pub use joints::{Angles, Dof, Frame, JointDesc, JointState};
#[path = "features/queries.rs"]
mod queries;
pub use queries::{
    KindFilter, MAX_QUERY_EXCLUDES, MAX_QUERY_HITS, OverlapHit, PointHit, QueryFilter, QueryHits,
    RayCastHit, SensorFilter, ShapeCastHit,
};
#[path = "features/debug/dim2.rs"]
mod debug;
pub use debug::{
    DebugAnchor, DebugCollider, DebugContact, DebugContributor, DebugLine, DebugLines,
    DebugOptions, DebugPart, DebugShape, DebugSink, DebugState, DebugTotals, DebugView,
    MAX_DEBUG_ITEMS,
};
#[derive(Clone, Copy, Debug)]
pub struct BodyDesc {
    pub kind: crate::BodyKind,
    pub position: Position,
    pub rotation: f32,
    pub velocity: Position,
    pub angular_velocity: f32,
    pub ccd: bool,
}
impl Default for BodyDesc {
    fn default() -> Self {
        Self {
            kind: crate::BodyKind::Dynamic,
            position: [0.; 2],
            rotation: 0.,
            velocity: [0.; 2],
            angular_velocity: 0.,
            ccd: false,
        }
    }
}
impl BodyDesc {
    fn validate(self) -> crate::Result<()> {
        if !crate::finite(&self.position)
            || !crate::finite(&self.velocity)
            || !valid_rotation(self.rotation)
            || !crate::finite(&[self.angular_velocity])
        {
            return Err(crate::Error("invalid body"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyPose {
    pub position: Position,
    pub rotation: f32,
    pub velocity: Position,
}
/// Collider geometry in body-local coordinates.
#[derive(Clone, Debug)]
pub enum Shape {
    Box {
        half_extents: Position,
    },
    Ball {
        radius: f32,
    },
    /// Y-axis capsule: half segment height plus radius.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Convex polygon hull of 3..=4096 points; collinear or degenerate sets fail.
    ConvexHull {
        points: Vec<Position>,
    },
    /// Fixed or kinematic bodies only. Each triangle indexes `vertices`.
    TriMesh {
        vertices: Vec<Position>,
        indices: Vec<[u32; 3]>,
    },
    /// Fixed or kinematic bodies only. Samples are evenly spaced along X over `scale[0]`
    /// meters centred on the body origin; every height is multiplied by `scale[1]`.
    HeightField {
        heights: Vec<f32>,
        scale: Position,
    },
    /// Rigid convex parts (no meshes, heightfields or nested compounds), 1..=64.
    Compound {
        parts: Vec<CompoundPart>,
    },
}
/// One part of [`Shape::Compound`], posed in body-local coordinates.
#[derive(Clone, Debug)]
pub struct CompoundPart {
    pub position: Position,
    pub rotation: Orientation,
    pub shape: Shape,
}
impl Shape {
    fn static_only(&self) -> bool {
        matches!(self, Self::TriMesh { .. } | Self::HeightField { .. })
    }
    fn shared(&self) -> crate::Result<SharedShape> {
        use crate::{Error, dimension};
        match self {
            Self::Box { half_extents: h } if h.iter().copied().all(dimension) => {
                Ok(SharedShape::cuboid(h[0], h[1]))
            }
            Self::Ball { radius: r } if dimension(*r) => Ok(SharedShape::ball(*r)),
            Self::Capsule {
                half_height: h,
                radius: r,
            } if h.is_finite() && (0.0..=10000.).contains(h) && dimension(*r) => {
                Ok(SharedShape::capsule_y(*h, *r))
            }
            Self::ConvexHull { points } => {
                crate::valid_hull(points)?;
                let points: Vec<_> = points.iter().map(|p| vector(*p)).collect();
                SharedShape::convex_hull(&points)
                    // Parry accepts flat hulls; reject them so dynamic hulls always have mass.
                    .filter(|hull| hull.mass_properties(1.).mass() > 1e-9)
                    .ok_or(Error("degenerate convex hull"))
            }
            Self::TriMesh { vertices, indices } => {
                crate::valid_mesh(vertices, indices)?;
                let vertices = vertices.iter().map(|v| vector(*v)).collect();
                SharedShape::trimesh(vertices, indices.clone())
                    .map_err(|_| Error("invalid triangle mesh"))
            }
            Self::HeightField { heights, scale } => {
                if heights.len() < 2 || heights.len() > crate::MAX_HEIGHTFIELD_SAMPLES {
                    return Err(Error("heightfield needs 2..=1048576 samples"));
                }
                crate::valid_heights(heights, scale)?;
                Ok(SharedShape::heightfield(heights.clone(), vector(*scale)))
            }
            Self::Compound { parts } => {
                if parts.is_empty() || parts.len() > crate::MAX_COMPOUND_PARTS {
                    return Err(Error("compound needs 1..=64 parts"));
                }
                let mut shapes = Vec::with_capacity(parts.len());
                for part in parts {
                    if part.shape.static_only() || matches!(part.shape, Self::Compound { .. }) {
                        return Err(Error("compound parts must be convex shapes"));
                    }
                    if !crate::finite(&part.position) || !valid_rotation(part.rotation) {
                        return Err(Error("invalid compound part pose"));
                    }
                    let pose = Pose::from_parts(vector(part.position), rotation(part.rotation));
                    shapes.push((pose, part.shape.shared()?));
                }
                Ok(SharedShape::compound(shapes))
            }
            _ => Err(Error("invalid collider dimensions")),
        }
    }
}
fn vector(v: Position) -> Vector {
    Vector::new(v[0], v[1])
}
fn rotation(v: f32) -> Rotation {
    Rotation::new(v)
}
fn angular(v: f32) -> f32 {
    v
}
fn valid_angular(v: AngularImpulse) -> bool {
    crate::finite(&[v])
}
fn valid_rotation(v: f32) -> bool {
    crate::finite(&[v])
}
fn body_pose(b: &RigidBody) -> BodyPose {
    BodyPose {
        position: b.translation().to_array(),
        rotation: b.rotation().angle(),
        velocity: b.linvel().to_array(),
    }
}
fn angular_velocity(b: &RigidBody) -> AngularVelocity {
    b.angvel()
}
