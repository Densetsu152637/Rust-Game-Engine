use rapier::prelude::*;
use rapier3d as rapier;
pub type Position = [f32; 3];
/// Quaternion in x,y,z,w order.
pub type Orientation = [f32; 4];
pub type AngularImpulse = [f32; 3];
#[path = "joints3.rs"]
mod joints;
pub use joints::JointDesc;
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
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Box { half_extents: Position },
    Ball { radius: f32 },
    Capsule { half_height: f32, radius: f32 },
}
impl Shape {
    fn collider(self) -> crate::Result<ColliderBuilder> {
        match self {
            Self::Box { half_extents: h } if h.into_iter().all(crate::dimension) => {
                Ok(ColliderBuilder::cuboid(h[0], h[1], h[2]))
            }
            Self::Ball { radius: r } if crate::dimension(r) => Ok(ColliderBuilder::ball(r)),
            Self::Capsule {
                half_height: h,
                radius: r,
            } if h.is_finite() && (0.0..=10000.).contains(&h) && crate::dimension(r) => {
                Ok(ColliderBuilder::capsule_y(h, r))
            }
            _ => Err(crate::Error("invalid collider dimensions")),
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
crate::world::world!();
