//! 3D joint descriptors. The dimension-generic side table and `World` methods are in
//! `shared.rs`, compiled once per dimension.
use super::{Orientation, Position, rotation, valid_rotation, vector};
use crate::{AxisMode, Error, Result};
use rapier3d::math::Pose;
use rapier3d::prelude::*;
#[path = "shared.rs"]
mod shared;
pub use shared::JointState;

/// Relative joint angles: XYZ intrinsic Euler angles (radians) of frame 2 in frame 1.
pub type Angles = [f32; 3];

/// A joint frame: anchor position plus orientation in a body's local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub position: Position,
    pub rotation: Orientation,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
        }
    }
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

/// One of the six joint degrees of freedom, in the joint frame of body one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dof {
    LinX,
    LinY,
    LinZ,
    AngX,
    AngY,
    AngZ,
}
impl Dof {
    pub const ALL: [Self; 6] = [
        Self::LinX,
        Self::LinY,
        Self::LinZ,
        Self::AngX,
        Self::AngY,
        Self::AngZ,
    ];
    /// Number of linear degrees of freedom.
    pub const LINEAR: usize = 3;
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
            Self::LinZ => JointAxis::LinZ,
            Self::AngX => JointAxis::AngX,
            Self::AngY => JointAxis::AngY,
            Self::AngZ => JointAxis::AngZ,
        }
    }
    /// Parses `lin_x`..`lin_z` and `ang_x`..`ang_z`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "lin_x" => Self::LinX,
            "lin_y" => Self::LinY,
            "lin_z" => Self::LinZ,
            "ang_x" => Self::AngX,
            "ang_y" => Self::AngY,
            "ang_z" => Self::AngZ,
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
    /// Axis is normalized and interpreted in both bodies' local frames.
    Revolute {
        anchor1: Position,
        anchor2: Position,
        axis: Position,
        limits: Option<[f32; 2]>,
    },
    /// Slider along `axis` (both local frames) with optional translation limits in meters.
    Prismatic {
        anchor1: Position,
        anchor2: Position,
        axis: Position,
        limits: Option<[f32; 2]>,
    },
    /// Ball-and-socket. `axis` is the twist axis in both local frames; `swing_limit` is the
    /// largest angle (0..=pi) between the two twist axes (cone) and `twist_limits` bounds the
    /// rotation about them.
    Spherical {
        anchor1: Position,
        anchor2: Position,
        axis: Position,
        swing_limit: Option<f32>,
        twist_limits: Option<[f32; 2]>,
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
    /// Six degrees of freedom in arbitrary joint frames: `axes` is LinX, LinY, LinZ, AngX,
    /// AngY, AngZ (see [`Dof::ALL`]).
    Generic {
        frame1: Frame,
        frame2: Frame,
        axes: [AxisMode; 6],
    },
}
fn unit_axis(axis: Position) -> Result<Vector> {
    if !crate::finite(&axis) || vector(axis).length_squared() < 1e-12 {
        return Err(Error("invalid joint axis"));
    }
    Ok(vector(axis).normalize())
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
            | Self::Spherical {
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
            Self::Revolute { axis, limits, .. } => {
                if !crate::finite(&axis)
                    || vector(axis).length_squared() < 1e-12
                    || !crate::valid_joint_limits(limits)
                {
                    return Err(Error("invalid hinge axis or limits"));
                }
                let mut joint = RevoluteJointBuilder::new(vector(axis).normalize())
                    .local_anchor1(a)
                    .local_anchor2(b)
                    .contacts_enabled(false);
                if let Some(limits) = limits {
                    joint = joint.limits(limits);
                }
                Ok(joint.into())
            }
            Self::Prismatic { axis, limits, .. } => {
                let mut joint = PrismaticJointBuilder::new(unit_axis(axis)?)
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
            Self::Spherical {
                axis,
                swing_limit,
                twist_limits,
                ..
            } => {
                let axis = unit_axis(axis)?;
                if swing_limit
                    .is_some_and(|s| !s.is_finite() || !(0.0..=std::f32::consts::PI).contains(&s))
                    || !crate::valid_joint_limits(twist_limits)
                {
                    return Err(Error("invalid cone or twist limits"));
                }
                let mut joint = GenericJointBuilder::new(JointAxesMask::LOCKED_SPHERICAL_AXES)
                    .local_anchor1(a)
                    .local_anchor2(b)
                    .local_axis1(axis)
                    .local_axis2(axis)
                    .contacts_enabled(false);
                if let Some(twist) = twist_limits {
                    joint = joint.limits(JointAxis::AngX, twist);
                }
                if let Some(swing) = swing_limit {
                    // Coupled swing axes limit the angle between the two twist axes; a
                    // negative minimum keeps the lower bound inactive.
                    joint = joint
                        .coupled_axes(JointAxesMask::ANG_Y | JointAxesMask::ANG_Z)
                        .limits(JointAxis::AngY, [-1., swing]);
                }
                Ok(joint.build())
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
fn generic(frame1: Frame, frame2: Frame, axes: [AxisMode; 6]) -> Result<GenericJoint> {
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
    [values[0], values[1], values[2]]
}
fn angular_norm(angles: Angles) -> f32 {
    angles.iter().map(|v| v * v).sum::<f32>().sqrt()
}
/// `moment + arm x force`: the moment about a point `arm` away from the force's line.
fn lever(moment: Angles, arm: Position, force: Position) -> Angles {
    [
        moment[0] + arm[1] * force[2] - arm[2] * force[1],
        moment[1] + arm[2] * force[0] - arm[0] * force[2],
        moment[2] + arm[0] * force[1] - arm[1] * force[0],
    ]
}
/// Linear offset and XYZ Euler angles of frame 2 relative to frame 1.
fn relative(frame1: Pose, frame2: Pose) -> (Position, Angles) {
    let rel = frame1.inverse() * frame2;
    let [x, y, z, w] = rel.rotation.to_array();
    let sin_b = (2. * (x * z + y * w)).clamp(-1., 1.);
    let a = (-2. * (y * z - x * w)).atan2(1. - 2. * (x * x + y * y));
    let c = (-2. * (x * y - z * w)).atan2(1. - 2. * (y * y + z * z));
    (rel.translation.to_array(), [a, sin_b.asin(), c])
}
