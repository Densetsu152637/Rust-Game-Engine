//! Three-dimensional world: XYZ meters and normalized XYZW quaternions.
use rapier::math::Pose;
use rapier::parry::shape::HeightFieldFlags;
use rapier::parry::utils::Array2;
use rapier::prelude::*;
use rapier3d as rapier;
pub type Position = [f32; 3];
/// Quaternion in x,y,z,w order.
pub type Orientation = [f32; 4];
pub type AngularImpulse = [f32; 3];
pub type AngularVelocity = [f32; 3];
#[path = "world.rs"]
mod world;
pub use world::{BodyVelocity, World};
use world::{Core, Extension};
// Feature registrations: one `#[path] mod` item each (docs/physics.md#extension-seams).
#[path = "features/joints/dim3.rs"]
mod joints;
pub use joints::{Angles, Dof, Frame, JointDesc, JointState};
#[path = "features/queries.rs"]
mod queries;
pub use queries::{
    KindFilter, MAX_QUERY_EXCLUDES, MAX_QUERY_HITS, OverlapHit, PointHit, QueryFilter, QueryHits,
    RayCastHit, SensorFilter, ShapeCastHit,
};
#[path = "features/destruction.rs"]
mod destruction;
pub use destruction::ContactImpulse;
#[path = "features/debug/dim3.rs"]
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
    pub rotation: Orientation,
    pub velocity: Position,
    pub angular_velocity: Position,
    pub ccd: bool,
}
impl Default for BodyDesc {
    fn default() -> Self {
        Self {
            kind: crate::BodyKind::Dynamic,
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            velocity: [0.; 3],
            angular_velocity: [0.; 3],
            ccd: false,
        }
    }
}
impl BodyDesc {
    fn validate(self) -> crate::Result<()> {
        if !crate::finite(&self.position)
            || !crate::finite(&self.velocity)
            || !valid_rotation(self.rotation)
            || !crate::finite(&self.angular_velocity)
        {
            return Err(crate::Error("invalid body"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyPose {
    pub position: Position,
    pub rotation: Orientation,
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
    /// Y-axis cylinder: half height plus radius.
    Cylinder {
        half_height: f32,
        radius: f32,
    },
    /// Convex hull of 4..=4096 points; coplanar or otherwise degenerate sets fail.
    ConvexHull {
        points: Vec<Position>,
    },
    /// Fixed or kinematic bodies only. Each triangle indexes `vertices`.
    TriMesh {
        vertices: Vec<Position>,
        indices: Vec<[u32; 3]>,
    },
    /// Fixed or kinematic bodies only. `heights` is row-major: `rows` samples along Z, each
    /// with `columns` samples along X. The grid spans `scale[0]` x `scale[2]` meters centred on
    /// the body origin and every height is multiplied by `scale[1]`.
    HeightField {
        rows: usize,
        columns: usize,
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
                Ok(SharedShape::cuboid(h[0], h[1], h[2]))
            }
            Self::Ball { radius: r } if dimension(*r) => Ok(SharedShape::ball(*r)),
            Self::Capsule {
                half_height: h,
                radius: r,
            } if h.is_finite() && (0.0..=10000.).contains(h) && dimension(*r) => {
                Ok(SharedShape::capsule_y(*h, *r))
            }
            Self::Cylinder {
                half_height: h,
                radius: r,
            } if dimension(*h) && dimension(*r) => Ok(SharedShape::cylinder(*h, *r)),
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
            Self::HeightField {
                rows,
                columns,
                heights,
                scale,
            } => {
                if *rows < 2
                    || *columns < 2
                    || rows.checked_mul(*columns) != Some(heights.len())
                    || heights.len() > crate::MAX_HEIGHTFIELD_SAMPLES
                {
                    return Err(Error(
                        "heightfield needs at least 2x2 samples matching rows x columns",
                    ));
                }
                crate::valid_heights(heights, scale)?;
                // Parry stores heights column-major with rows along Z.
                let mut data = vec![0.; heights.len()];
                for (index, height) in heights.iter().enumerate() {
                    data[index / columns + index % columns * rows] = *height;
                }
                Ok(SharedShape::heightfield_with_flags(
                    Array2::new(*rows, *columns, data),
                    vector(*scale),
                    HeightFieldFlags::FIX_INTERNAL_EDGES,
                ))
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
    Vector::new(v[0], v[1], v[2])
}
fn rotation(v: Orientation) -> Rotation {
    Rotation::from_xyzw(v[0], v[1], v[2], v[3]).normalize()
}
fn angular(v: Position) -> Vector {
    vector(v)
}
fn valid_angular(v: AngularImpulse) -> bool {
    crate::finite(&v)
}
fn valid_rotation(v: Orientation) -> bool {
    crate::finite(&v) && (v.iter().map(|n| n * n).sum::<f32>() - 1.).abs() < 0.01
}
fn body_pose(b: &RigidBody) -> BodyPose {
    BodyPose {
        position: b.translation().to_array(),
        rotation: b.rotation().to_array(),
        velocity: b.linvel().to_array(),
    }
}
fn angular_velocity(b: &RigidBody) -> AngularVelocity {
    b.angvel().to_array()
}
