use rapier::prelude::*;
use rapier2d as rapier;
pub type Position = [f32; 2];
pub type Orientation = f32;
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
                Ok(ColliderBuilder::cuboid(h[0], h[1]))
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
    Vector::new(v[0], v[1])
}
fn rotation(v: f32) -> Rotation {
    Rotation::new(v)
}
fn angular(v: f32) -> f32 {
    v
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
crate::world::world!();
