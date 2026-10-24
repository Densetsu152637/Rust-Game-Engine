use super::{Position, vector};
use crate::{Error, Result};
use rapier2d::prelude::*;
/// Anchors are in each body's local coordinates. Connected contacts are disabled.
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
}
impl JointDesc {
    pub(super) fn build(self) -> Result<GenericJoint> {
        let (a, b) = match self {
            Self::Fixed { anchor1, anchor2 }
            | Self::Revolute {
                anchor1, anchor2, ..
            } => (anchor1, anchor2),
        };
        if !crate::finite(&a) || !crate::finite(&b) {
            return Err(Error("invalid joint anchors"));
        }
        match self {
            Self::Fixed { .. } => Ok(FixedJointBuilder::new()
                .local_anchor1(vector(a))
                .local_anchor2(vector(b))
                .contacts_enabled(false)
                .into()),
            Self::Revolute { limits, .. } => {
                if !crate::valid_joint_limits(limits) {
                    return Err(Error("invalid hinge angle limits"));
                }
                let mut joint = RevoluteJointBuilder::new()
                    .local_anchor1(vector(a))
                    .local_anchor2(vector(b))
                    .contacts_enabled(false);
                if let Some(limits) = limits {
                    joint = joint.limits(limits);
                }
                Ok(joint.into())
            }
        }
    }
}
