//! 3D joint descriptors. The dimension-generic side table and `World` methods are in
//! `shared.rs`, compiled once per dimension.
use super::{Position, vector};
use crate::{Error, Result};
use rapier3d::prelude::*;
#[path = "shared.rs"]
mod shared;
/// Anchors and hinge axis are in local body coordinates. Connected contacts are disabled.
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
            Self::Revolute { axis, limits, .. } => {
                if !crate::finite(&axis)
                    || vector(axis).length_squared() < 1e-12
                    || !crate::valid_joint_limits(limits)
                {
                    return Err(Error("invalid hinge axis or limits"));
                }
                let mut joint = RevoluteJointBuilder::new(vector(axis).normalize())
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
