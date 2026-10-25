//! 2D joint descriptors. The dimension-generic side table and `World` methods are in
//! `shared.rs`, compiled once per dimension.
//!
//! There is no spherical joint in 2D (only one rotation axis exists): use `Revolute`.
use super::{Position, rotation, valid_rotation, vector};
use crate::{AxisMode, Error, Result};
use rapier2d::math::Pose;
use rapier2d::prelude::*;
#[path = "shared.rs"]
mod shared;
pub use shared::JointState;

/// Relative joint angle: radians of frame 2 in frame 1.
pub type Angles = f32;

/// A joint frame: anchor position plus orientation (radians) in a body's local coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub position: Position,
    pub rotation: f32,
}
impl Frame {
    fn pose(self) -> Result<Pose> {
        if !crate::finite(&self.position) || !valid_rotation(self.rotation) {
            return Err(Error("invalid joint frame"));
        }
        Ok(Pose::from_parts(
            vector(self.position),
            rotation(self.rotation),
        ))
    }
}

/// One of the three joint degrees of freedom, in the joint frame of body one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dof {
    LinX,
    LinY,
    /// Rotation about Z.
    Ang,
}
impl Dof {
    pub const ALL: [Self; 3] = [Self::LinX, Self::LinY, Self::Ang];
    /// Number of linear degrees of freedom.
    pub const LINEAR: usize = 2;
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn is_angular(self) -> bool {
        self.index() >= Self::LINEAR
    }
    fn axis(self) -> JointAxis {
        match self {
            Self::LinX => JointAxis::LinX,
            Self::LinY => JointAxis::LinY,
            Self::Ang => JointAxis::AngX,
        }
    }
    /// Parses `lin_x`, `lin_y` and `ang`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "lin_x" => Self::LinX,
            "lin_y" => Self::LinY,
            "ang" => Self::Ang,
            _ => return None,
        })
    }
}

/// Anchors, axes and frames are in local body coordinates. Connected contacts are disabled
/// unless [`crate::JointOptions::contacts_enabled`] says otherwise.
#[derive(Clone, Copy, Debug)]
pub enum JointDesc {
    /// Locks relative translation and aligns the two bodies' local rotation frames.
    Fixed {
        anchor1: Position,
        anchor2: Position,
    },
    /// A hinge around the out-of-plane Z axis, with optional angle limits in radians.
    Revolute {
        anchor1: Position,
        anchor2: Position,
        limits: Option<[f32; 2]>,
    },
    /// Slider along `axis` (both local frames) with optional translation limits in meters.
    Prismatic {
        anchor1: Position,
        anchor2: Position,
        axis: Position,
        limits: Option<[f32; 2]>,
    },
    /// Spring-damper pulling the anchors to `rest_length` (force based: `k` in N/m).
    Spring {
        anchor1: Position,
        anchor2: Position,
        rest_length: f32,
        stiffness: f32,
        damping: f32,
    },
    /// Keeps the anchor distance within `min..=max`.
    Distance {
        anchor1: Position,
        anchor2: Position,
        min: f32,
        max: f32,
    },
    /// Keeps the anchor distance at most `max`; slack is allowed.
    Rope {
        anchor1: Position,
        anchor2: Position,
        max: f32,
    },
    /// Three degrees of freedom in arbitrary joint frames: `axes` is LinX, LinY, Ang.
    Generic {
        frame1: Frame,
        frame2: Frame,
        axes: [AxisMode; 3],
    },
}
impl JointDesc {
    /// The positive minimum anchor distance, which Rapier cannot express natively.
    pub(super) fn min_distance(&self) -> Option<f32> {
        match self {
            Self::Distance { min, .. } if *min > 0. => Some(*min),
            _ => None,
        }
    }
    pub(super) fn build(self) -> Result<GenericJoint> {
        let (a, b) = match self {
            Self::Generic {
                frame1,
                frame2,
                axes,
            } => return generic(frame1, frame2, axes),
            Self::Fixed { anchor1, anchor2 }
            | Self::Revolute {
                anchor1, anchor2, ..
            }
            | Self::Prismatic {
                anchor1, anchor2, ..
            }
            | Self::Spring {
                anchor1, anchor2, ..
            }
            | Self::Distance {
                anchor1, anchor2, ..
            }
            | Self::Rope {
                anchor1, anchor2, ..
            } => (anchor1, anchor2),
        };
        if !crate::finite(&a) || !crate::finite(&b) {
            return Err(Error("invalid joint anchors"));
        }
        let (a, b) = (vector(a), vector(b));
        match self {
            Self::Fixed { .. } => Ok(FixedJointBuilder::new()
                .local_anchor1(a)
                .local_anchor2(b)
                .contacts_enabled(false)
                .into()),
            Self::Revolute { limits, .. } => {
                if !crate::valid_joint_limits(limits) {
                    return Err(Error("invalid hinge angle limits"));
                }
                let mut joint = RevoluteJointBuilder::new()
                    .local_anchor1(a)
                    .local_anchor2(b)
                    .contacts_enabled(false);
                if let Some(limits) = limits {
                    joint = joint.limits(limits);
                }
                Ok(joint.into())
            }
            Self::Prismatic { axis, limits, .. } => {
                if !crate::finite(&axis) || vector(axis).length_squared() < 1e-12 {
                    return Err(Error("invalid joint axis"));
                }
                let mut joint = PrismaticJointBuilder::new(vector(axis).normalize())
                    .local_anchor1(a)
                    .local_anchor2(b)
                    .contacts_enabled(false);
                if let Some(limits) = limits {
                    if !crate::valid_linear_limits(limits) {
                        return Err(Error("invalid slider limits"));
                    }
                    joint = joint.limits(limits);
                }
                Ok(joint.into())
            }
            Self::Spring {
                rest_length,
                stiffness,
                damping,
                ..
            } => {
                if !crate::finite(&[rest_length, stiffness, damping])
                    || rest_length < 0.
                    || stiffness < 0.
                    || damping < 0.
                {
                    return Err(Error("invalid spring parameters"));
                }
                Ok(SpringJointBuilder::new(rest_length, stiffness, damping)
                    .local_anchor1(a)
                    .local_anchor2(b)
                    .contacts_enabled(false)
                    .into())
            }
            Self::Distance { min, max, .. } => distance(a, b, min, max),
            Self::Rope { max, .. } => distance(a, b, 0., max),
            Self::Generic { .. } => Err(Error("generic joint handled separately")),
        }
    }
}
fn generic(frame1: Frame, frame2: Frame, axes: [AxisMode; 3]) -> Result<GenericJoint> {
    let mut locked = JointAxesMask::empty();
    let mut joint = GenericJointBuilder::new(JointAxesMask::empty())
        .local_frame1(frame1.pose()?)
        .local_frame2(frame2.pose()?)
        .contacts_enabled(false);
    for (dof, mode) in Dof::ALL.into_iter().zip(axes) {
        mode.validate(dof.is_angular())?;
        match mode {
            AxisMode::Free => {}
            AxisMode::Locked => locked |= dof.axis().into(),
            AxisMode::Limited { min, max } => joint = joint.limits(dof.axis(), [min, max]),
        }
    }
    Ok(joint.locked_axes(locked).build())
}
fn distance(a: Vector, b: Vector, min: f32, max: f32) -> Result<GenericJoint> {
    if !crate::finite(&[min, max]) || min < 0. || max <= 0. || min > max {
        return Err(Error("invalid distance range"));
    }
    Ok(GenericJointBuilder::new(JointAxesMask::empty())
        .local_anchor1(a)
        .local_anchor2(b)
        .coupled_axes(JointAxesMask::LIN_AXES)
        .limits(JointAxis::LinX, [min, max])
        .contacts_enabled(false)
        .build())
}
fn angular_from(values: &[f32]) -> Angles {
    values[0]
}
fn angular_norm(angles: Angles) -> f32 {
    angles.abs()
}
/// `moment + arm x force`: the moment about a point `arm` away from the force's line.
fn lever(moment: Angles, arm: Position, force: Position) -> Angles {
    moment + arm[0] * force[1] - arm[1] * force[0]
}
/// Linear offset and angle of frame 2 relative to frame 1.
fn relative(frame1: Pose, frame2: Pose) -> (Position, Angles) {
    let rel = frame1.inverse() * frame2;
    (rel.translation.to_array(), rel.rotation.angle())
}
