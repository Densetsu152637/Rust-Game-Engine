//! Dimension-independent joint value types shared by `dim2` and `dim3`.
use crate::{Error, Result};

/// How one degree of freedom of a generic joint is constrained.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AxisMode {
    /// Unconstrained (a motor may still drive it).
    Free,
    /// Rigidly locked at the frame offset.
    Locked,
    /// Free between `min` and `max` (meters for linear axes, radians for angular axes).
    Limited { min: f32, max: f32 },
}
impl AxisMode {
    pub(crate) fn validate(self, angular: bool) -> Result<()> {
        match self {
            Self::Limited { min, max } => {
                let range = if angular {
                    crate::valid_joint_limits(Some([min, max]))
                } else {
                    crate::valid_linear_limits([min, max])
                };
                if range {
                    Ok(())
                } else {
                    Err(Error("invalid joint axis limits"))
                }
            }
            _ => Ok(()),
        }
    }
}

/// Motor on one joint degree of freedom. Position motors use `stiffness`/`damping` to reach
/// `target_position`; velocity motors use `target_velocity` with `damping` as the gain.
/// `max_force` caps the motor force (N for linear axes, N*m for angular axes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotorDesc {
    pub target_position: f32,
    pub target_velocity: f32,
    pub stiffness: f32,
    pub damping: f32,
    pub max_force: f32,
    /// `true`: constants produce absolute forces; `false`: constants scale with mass.
    pub force_based: bool,
}
impl Default for MotorDesc {
    fn default() -> Self {
        Self {
            target_position: 0.,
            target_velocity: 0.,
            stiffness: 0.,
            damping: 0.,
            max_force: f32::MAX,
            force_based: false,
        }
    }
}
impl MotorDesc {
    /// Drives toward `target_velocity` with gain `damping`.
    pub fn velocity(target_velocity: f32, damping: f32) -> Self {
        Self {
            target_velocity,
            damping,
            ..Self::default()
        }
    }
    /// Drives toward `target_position` as a spring-damper.
    pub fn position(target_position: f32, stiffness: f32, damping: f32) -> Self {
        Self {
            target_position,
            stiffness,
            damping,
            ..Self::default()
        }
    }
    /// All values finite, non-negative gains and force cap.
    pub fn validate(self) -> Result<()> {
        let ok = crate::finite(&[self.target_position, self.target_velocity])
            && [self.stiffness, self.damping, self.max_force]
                .iter()
                .all(|v| !v.is_nan() && *v >= 0.)
            && self.stiffness <= 1e12
            && self.damping <= 1e12;
        if ok {
            Ok(())
        } else {
            Err(Error("invalid joint motor"))
        }
    }
}

/// Force and torque magnitudes above which a joint breaks. `None` disables that threshold.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BreakLimits {
    pub force: Option<f32>,
    pub torque: Option<f32>,
}
impl BreakLimits {
    pub fn validate(self) -> Result<()> {
        if [self.force, self.torque]
            .into_iter()
            .flatten()
            .all(|v| v.is_finite() && v > 0.)
        {
            Ok(())
        } else {
            Err(Error("break thresholds must be finite and positive"))
        }
    }
}

/// Per-joint options applied at creation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JointOptions {
    pub break_limits: BreakLimits,
    /// Whether the two connected bodies still collide with each other.
    pub contacts_enabled: bool,
}
/// Record of a joint that exceeded its break thresholds and was removed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointBroken {
    pub id: u64,
    pub first: u64,
    pub second: u64,
    /// Constraint force magnitude (N) at the break.
    pub force: f32,
    /// Constraint torque magnitude (N*m) at the break.
    pub torque: f32,
}
