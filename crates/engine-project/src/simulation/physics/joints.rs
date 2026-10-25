//! Authored joints (`engine.PhysicsJoint2D`, `engine.PhysicsJoint3D`): fixed, revolute,
//! prismatic, spherical (3D), spring, distance, rope and generic 6-DoF constraints between
//! two physics bodies, with limits, motors and break thresholds.
//!
//! The component lives on the entity that owns the first body (`body_a` omitted) or on a
//! standalone entity with explicit `body_a` and `body_b` entity references. `body_b` is
//! always required. Both references are descriptor `entity_references`, so prefab
//! instantiation remaps them. See `docs/physics-joints.md`.
use super::{Context, Dimension, Feature, Resolved, emit, service};
use crate::Result;
use crate::simulation::{invalid, mapped};
use bevy_ecs::prelude::*;
use engine_physics::{
    AxisMode, BreakLimits, JointBroken, JointOptions, MotorDesc, World2D, World3D, dim2, dim3,
};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_AXES: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JointKind {
    /// Locks relative translation and orientation.
    #[default]
    Fixed,
    /// Hinge: around `axis` (3D) or Z (2D), with optional `limits` in radians.
    Revolute,
    /// Slider along `axis`, with optional `limits` in meters.
    Prismatic,
    /// Ball-and-socket (3D only) with optional `swing_limit` cone and `twist_limits`.
    Spherical,
    /// Spring-damper holding the anchors at `rest_length`.
    Spring,
    /// Keeps the anchor distance within `min_distance..=max_distance`.
    Distance,
    /// Keeps the anchor distance at most `max_distance`; slack is allowed.
    Rope,
    /// Per-axis free/locked/limited degrees of freedom in arbitrary joint frames.
    Generic,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisKind {
    Free,
    #[default]
    Locked,
    Limited,
}
/// One non-default degree of freedom of a `generic` joint. Axes not listed are locked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AxisSetting {
    /// 3D: `lin_x`, `lin_y`, `lin_z`, `ang_x`, `ang_y`, `ang_z`; 2D: `lin_x`, `lin_y`, `ang`.
    pub axis: String,
    pub mode: AxisKind,
    pub min: f32,
    pub max: f32,
}
impl Default for AxisSetting {
    fn default() -> Self {
        Self {
            axis: "lin_x".into(),
            mode: AxisKind::Free,
            min: 0.,
            max: 0.,
        }
    }
}
/// Motor on one joint axis (same axis names as [`AxisSetting`]). `max_force` is in N for
/// linear axes and N*m for angular ones; omitted means unlimited.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JointMotor {
    pub axis: String,
    pub target_position: f32,
    pub target_velocity: f32,
    pub stiffness: f32,
    pub damping: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_force: Option<f32>,
    pub force_based: bool,
}
impl Default for JointMotor {
    fn default() -> Self {
        Self {
            axis: "lin_x".into(),
            target_position: 0.,
            target_velocity: 0.,
            stiffness: 0.,
            damping: 0.,
            max_force: None,
            force_based: false,
        }
    }
}
impl JointMotor {
    fn desc(&self) -> MotorDesc {
        MotorDesc {
            target_position: self.target_position,
            target_velocity: self.target_velocity,
            stiffness: self.stiffness,
            damping: self.damping,
            max_force: self.max_force.unwrap_or(f32::MAX),
            force_based: self.force_based,
        }
    }
}

/// Authored 3D joint. Anchors are body-local and not affected by `Transform` scale.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsJoint3D {
    pub kind: JointKind,
    /// Entity id of the first body; omitted means the entity carrying this component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_a: Option<String>,
    /// Entity id of the second body (required).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_b: Option<String>,
    pub anchor_a: [f32; 3],
    pub anchor_b: [f32; 3],
    /// Joint frame orientations (XYZW) for `generic` joints.
    pub rotation_a: [f32; 4],
    pub rotation_b: [f32; 4],
    /// Hinge, slider or twist axis in both bodies' local frames.
    pub axis: [f32; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swing_limit: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twist_limits: Option<[f32; 2]>,
    pub rest_length: f32,
    pub stiffness: f32,
    pub damping: f32,
    pub min_distance: f32,
    pub max_distance: f32,
    pub axes: Vec<AxisSetting>,
    pub motors: Vec<JointMotor>,
    /// Constraint force magnitude (N) above which the joint breaks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_force: Option<f32>,
    /// Constraint torque magnitude (N*m) above which the joint breaks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_torque: Option<f32>,
    /// Whether the two connected bodies still collide.
    pub collide_connected: bool,
}
impl Default for PhysicsJoint3D {
    fn default() -> Self {
        Self {
            kind: JointKind::Fixed,
            body_a: None,
            body_b: None,
            anchor_a: [0.; 3],
            anchor_b: [0.; 3],
            rotation_a: [0., 0., 0., 1.],
            rotation_b: [0., 0., 0., 1.],
            axis: [1., 0., 0.],
            limits: None,
            swing_limit: None,
            twist_limits: None,
            rest_length: 1.,
            stiffness: 100.,
            damping: 1.,
            min_distance: 0.,
            max_distance: 1.,
            axes: vec![],
            motors: vec![],
            break_force: None,
            break_torque: None,
            collide_connected: false,
        }
    }
}
/// Authored 2D joint (rotation about Z). There is no spherical joint in 2D: use `revolute`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsJoint2D {
    pub kind: JointKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_b: Option<String>,
    pub anchor_a: [f32; 2],
    pub anchor_b: [f32; 2],
    /// Joint frame angles (radians) for `generic` joints.
    pub rotation_a: f32,
    pub rotation_b: f32,
    pub axis: [f32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f32; 2]>,
    pub rest_length: f32,
    pub stiffness: f32,
    pub damping: f32,
    pub min_distance: f32,
    pub max_distance: f32,
    pub axes: Vec<AxisSetting>,
    pub motors: Vec<JointMotor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_force: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_torque: Option<f32>,
    pub collide_connected: bool,
}
impl Default for PhysicsJoint2D {
    fn default() -> Self {
        Self {
            kind: JointKind::Fixed,
            body_a: None,
            body_b: None,
            anchor_a: [0.; 2],
            anchor_b: [0.; 2],
            rotation_a: 0.,
            rotation_b: 0.,
            axis: [1., 0.],
            limits: None,
            rest_length: 1.,
            stiffness: 100.,
            damping: 1.,
            min_distance: 0.,
            max_distance: 1.,
            axes: vec![],
            motors: vec![],
            break_force: None,
            break_torque: None,
            collide_connected: false,
        }
    }
}

/// Lifecycle of an authored joint in the simulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JointStatus {
    /// The constraint is live in engine-physics.
    Active,
    /// The joint exceeded a break threshold; edit the component to re-create it.
    Broken { force: f32, torque: f32 },
    /// Not simulated: no synced bodies yet, a body was despawned, or the data is unsynced.
    Inactive,
}
/// A broken joint, from [`broken_joints`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrokenJoint {
    pub entity: Entity,
    pub dimension: Dimension,
    pub force: f32,
    pub torque: f32,
}
/// Relative pose and load of a live joint in the joint frame of body one. 2D joints report
/// X/Y in `position`/`force` and the rotation/torque about Z in `angles.z`/`torque.z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointReadout {
    pub dimension: Dimension,
    pub position: Vec3,
    pub angles: Vec3,
    pub force: Vec3,
    pub torque: Vec3,
}

// ---- Validation ----------------------------------------------------------------------

fn finite(values: &[f32]) -> bool {
    values.iter().all(|v| v.is_finite() && v.abs() <= 1e8)
}
fn bad(message: &str) -> crate::Error {
    invalid(format!("Joint {message}"))
}
fn ordered(range: [f32; 2], angular: bool) -> bool {
    finite(&range)
        && range[0] <= range[1]
        && (!angular || range.iter().all(|v| v.abs() <= std::f32::consts::PI))
}

/// Behaviour shared by the 2D and 3D authored components.
trait Config: Component<Mutability = bevy_ecs::component::Mutable> + Clone + PartialEq {
    const DIMENSION: Dimension;
    fn kind(&self) -> JointKind;
    fn ends(&self) -> (&Option<String>, &Option<String>);
    fn axes(&self) -> &[AxisSetting];
    fn motors(&self) -> &[JointMotor];
    fn motors_mut(&mut self) -> &mut Vec<JointMotor>;
    fn break_limits(&self) -> BreakLimits;
    fn collide_connected(&self) -> bool;
    /// Clears the fields that are updated in place (motors, break thresholds).
    fn strip_tunables(&mut self);
    /// Axis names this dimension accepts.
    fn parse_axis(name: &str) -> Option<usize>;
    /// Degrees of freedom (as `parse_axis` indices) a motor may drive.
    fn drivable(&self) -> Vec<usize>;
    /// Dimension-specific numeric checks.
    fn check_values(&self) -> Result<()>;
    fn check(&self) -> Result<()> {
        self.check_values()?;
        let mut seen = Vec::<usize>::new();
        if self.axes().len() > MAX_AXES || self.motors().len() > MAX_AXES {
            return Err(bad("axes and motors are limited to six entries"));
        }
        if !self.axes().is_empty() && self.kind() != JointKind::Generic {
            return Err(bad("`axes` only applies to generic joints"));
        }
        for axis in self.axes() {
            let index = Self::parse_axis(&axis.axis).ok_or_else(|| bad("has an unknown axis"))?;
            if seen.contains(&index) {
                return Err(bad("lists an axis twice"));
            }
            seen.push(index);
            let angular = index >= Self::LINEAR;
            if axis.mode == AxisKind::Limited && !ordered([axis.min, axis.max], angular) {
                return Err(bad("has unordered or out-of-range axis limits"));
            }
        }
        let drivable = self.drivable();
        let mut driven = Vec::<usize>::new();
        for motor in self.motors() {
            let index =
                Self::parse_axis(&motor.axis).ok_or_else(|| bad("motor has an unknown axis"))?;
            if driven.contains(&index) {
                return Err(bad("has two motors on one axis"));
            }
            driven.push(index);
            if !drivable.contains(&index) {
                return Err(bad("motor axis is locked for this joint kind"));
            }
            motor
                .desc()
                .validate()
                .map_err(|e| bad(&format!("motor: {e}")))?;
        }
        if self.ends().1.as_deref().is_none_or(str::is_empty) {
            return Err(bad("requires body_b"));
        }
        self.break_limits()
            .validate()
            .map_err(|e| bad(&e.to_string()))?;
        Ok(())
    }
    const LINEAR: usize;
}
fn distance_ok(kind: JointKind, min: f32, max: f32, rest: f32, k: f32, d: f32) -> Result<()> {
    match kind {
        JointKind::Distance if !(finite(&[min, max]) && min >= 0. && min <= max && max > 0.) => {
            Err(bad("distance range must satisfy 0 <= min <= max, max > 0"))
        }
        JointKind::Rope if !(finite(&[max]) && max > 0.) => {
            Err(bad("rope max_distance must be positive"))
        }
        JointKind::Spring if !(finite(&[rest, k, d]) && rest >= 0. && k >= 0. && d >= 0.) => {
            Err(bad("spring needs finite non-negative length and gains"))
        }
        _ => Ok(()),
    }
}
impl Config for PhysicsJoint3D {
    const DIMENSION: Dimension = Dimension::Three;
    const LINEAR: usize = dim3::Dof::LINEAR;
    fn kind(&self) -> JointKind {
        self.kind
    }
    fn ends(&self) -> (&Option<String>, &Option<String>) {
        (&self.body_a, &self.body_b)
    }
    fn axes(&self) -> &[AxisSetting] {
        &self.axes
    }
    fn motors(&self) -> &[JointMotor] {
        &self.motors
    }
    fn motors_mut(&mut self) -> &mut Vec<JointMotor> {
        &mut self.motors
    }
    fn break_limits(&self) -> BreakLimits {
        BreakLimits {
            force: self.break_force,
            torque: self.break_torque,
        }
    }
    fn collide_connected(&self) -> bool {
        self.collide_connected
    }
    fn strip_tunables(&mut self) {
        self.motors.clear();
        self.break_force = None;
        self.break_torque = None;
    }
    fn parse_axis(name: &str) -> Option<usize> {
        dim3::Dof::parse(name).map(dim3::Dof::index)
    }
    fn drivable(&self) -> Vec<usize> {
        use JointKind::*;
        let dofs: Vec<dim3::Dof> = match self.kind {
            Fixed => vec![],
            Revolute => vec![dim3::Dof::AngX],
            Prismatic | Spring | Rope => vec![dim3::Dof::LinX],
            Distance if self.min_distance <= 0. => vec![dim3::Dof::LinX],
            Distance => vec![],
            Spherical => vec![dim3::Dof::AngX, dim3::Dof::AngY, dim3::Dof::AngZ],
            Generic => self
                .axes
                .iter()
                .filter(|a| a.mode != AxisKind::Locked)
                .filter_map(|a| dim3::Dof::parse(&a.axis))
                .collect(),
        };
        dofs.into_iter().map(dim3::Dof::index).collect()
    }
    fn check_values(&self) -> Result<()> {
        let unit =
            |q: [f32; 4]| finite(&q) && (q.iter().map(|v| v * v).sum::<f32>() - 1.).abs() < 0.01;
        if !finite(&self.anchor_a)
            || !finite(&self.anchor_b)
            || !unit(self.rotation_a)
            || !unit(self.rotation_b)
        {
            return Err(bad("anchors must be finite and rotations unit quaternions"));
        }
        let needs_axis = matches!(
            self.kind,
            JointKind::Revolute | JointKind::Prismatic | JointKind::Spherical
        );
        if needs_axis
            && !(finite(&self.axis) && self.axis.iter().map(|v| v * v).sum::<f32>() > 1e-12)
        {
            return Err(bad("axis must be finite and non-zero"));
        }
        if let Some(limits) = self.limits
            && !ordered(limits, self.kind == JointKind::Revolute)
        {
            return Err(bad("limits must be ordered (angles within -pi..=pi)"));
        }
        if self.twist_limits.is_some_and(|l| !ordered(l, true))
            || self
                .swing_limit
                .is_some_and(|s| !(s.is_finite() && (0. ..=std::f32::consts::PI).contains(&s)))
        {
            return Err(bad("swing_limit must be 0..=pi and twist_limits ordered"));
        }
        distance_ok(
            self.kind,
            self.min_distance,
            self.max_distance,
            self.rest_length,
            self.stiffness,
            self.damping,
        )
    }
}
impl Config for PhysicsJoint2D {
    const DIMENSION: Dimension = Dimension::Two;
    const LINEAR: usize = dim2::Dof::LINEAR;
    fn kind(&self) -> JointKind {
        self.kind
    }
    fn ends(&self) -> (&Option<String>, &Option<String>) {
        (&self.body_a, &self.body_b)
    }
    fn axes(&self) -> &[AxisSetting] {
        &self.axes
    }
    fn motors(&self) -> &[JointMotor] {
        &self.motors
    }
    fn motors_mut(&mut self) -> &mut Vec<JointMotor> {
        &mut self.motors
    }
    fn break_limits(&self) -> BreakLimits {
        BreakLimits {
            force: self.break_force,
            torque: self.break_torque,
        }
    }
    fn collide_connected(&self) -> bool {
        self.collide_connected
    }
    fn strip_tunables(&mut self) {
        self.motors.clear();
        self.break_force = None;
        self.break_torque = None;
    }
    fn parse_axis(name: &str) -> Option<usize> {
        dim2::Dof::parse(name).map(dim2::Dof::index)
    }
    fn drivable(&self) -> Vec<usize> {
        use JointKind::*;
        let dofs: Vec<dim2::Dof> = match self.kind {
            Fixed | Spherical => vec![],
            Revolute => vec![dim2::Dof::Ang],
            Prismatic | Spring | Rope => vec![dim2::Dof::LinX],
            Distance if self.min_distance <= 0. => vec![dim2::Dof::LinX],
            Distance => vec![],
            Generic => self
                .axes
                .iter()
                .filter(|a| a.mode != AxisKind::Locked)
                .filter_map(|a| dim2::Dof::parse(&a.axis))
                .collect(),
        };
        dofs.into_iter().map(dim2::Dof::index).collect()
    }
    fn check_values(&self) -> Result<()> {
        if self.kind == JointKind::Spherical {
            return Err(bad(
                "spherical joints are 3D only; use revolute in 2D (one rotation axis)",
            ));
        }
        if !finite(&self.anchor_a)
            || !finite(&self.anchor_b)
            || !finite(&[self.rotation_a, self.rotation_b])
        {
            return Err(bad("anchors and rotations must be finite"));
        }
        if self.kind == JointKind::Prismatic
            && !(finite(&self.axis) && self.axis.iter().map(|v| v * v).sum::<f32>() > 1e-12)
        {
            return Err(bad("axis must be finite and non-zero"));
        }
        if let Some(limits) = self.limits
            && !ordered(limits, self.kind == JointKind::Revolute)
        {
            return Err(bad("limits must be ordered (angles within -pi..=pi)"));
        }
        distance_ok(
            self.kind,
            self.min_distance,
            self.max_distance,
            self.rest_length,
            self.stiffness,
            self.damping,
        )
    }
}

// ---- Authored data -> engine-physics -------------------------------------------------

fn desc3(c: &PhysicsJoint3D) -> Result<dim3::JointDesc> {
    let (a, b) = (c.anchor_a, c.anchor_b);
    Ok(match c.kind {
        JointKind::Fixed => dim3::JointDesc::Fixed {
            anchor1: a,
            anchor2: b,
        },
        JointKind::Revolute => dim3::JointDesc::Revolute {
            anchor1: a,
            anchor2: b,
            axis: c.axis,
            limits: c.limits,
        },
        JointKind::Prismatic => dim3::JointDesc::Prismatic {
            anchor1: a,
            anchor2: b,
            axis: c.axis,
            limits: c.limits,
        },
        JointKind::Spherical => dim3::JointDesc::Spherical {
            anchor1: a,
            anchor2: b,
            axis: c.axis,
            swing_limit: c.swing_limit,
            twist_limits: c.twist_limits,
        },
        JointKind::Spring => dim3::JointDesc::Spring {
            anchor1: a,
            anchor2: b,
            rest_length: c.rest_length,
            stiffness: c.stiffness,
            damping: c.damping,
        },
        JointKind::Distance => dim3::JointDesc::Distance {
            anchor1: a,
            anchor2: b,
            min: c.min_distance,
            max: c.max_distance,
        },
        JointKind::Rope => dim3::JointDesc::Rope {
            anchor1: a,
            anchor2: b,
            max: c.max_distance,
        },
        JointKind::Generic => {
            let mut axes = [AxisMode::Locked; 6];
            for setting in &c.axes {
                if let Some(dof) = dim3::Dof::parse(&setting.axis) {
                    axes[dof.index()] = axis_mode(setting);
                }
            }
            dim3::JointDesc::Generic {
                frame1: dim3::Frame {
                    position: a,
                    rotation: c.rotation_a,
                },
                frame2: dim3::Frame {
                    position: b,
                    rotation: c.rotation_b,
                },
                axes,
            }
        }
    })
}
fn desc2(c: &PhysicsJoint2D) -> Result<dim2::JointDesc> {
    let (a, b) = (c.anchor_a, c.anchor_b);
    Ok(match c.kind {
        JointKind::Fixed => dim2::JointDesc::Fixed {
            anchor1: a,
            anchor2: b,
        },
        JointKind::Revolute => dim2::JointDesc::Revolute {
            anchor1: a,
            anchor2: b,
            limits: c.limits,
        },
        JointKind::Prismatic => dim2::JointDesc::Prismatic {
            anchor1: a,
            anchor2: b,
            axis: c.axis,
            limits: c.limits,
        },
        JointKind::Spherical => {
            return Err(bad("spherical joints are 3D only; use revolute in 2D"));
        }
        JointKind::Spring => dim2::JointDesc::Spring {
            anchor1: a,
            anchor2: b,
            rest_length: c.rest_length,
            stiffness: c.stiffness,
            damping: c.damping,
        },
        JointKind::Distance => dim2::JointDesc::Distance {
            anchor1: a,
            anchor2: b,
            min: c.min_distance,
            max: c.max_distance,
        },
        JointKind::Rope => dim2::JointDesc::Rope {
            anchor1: a,
            anchor2: b,
            max: c.max_distance,
        },
        JointKind::Generic => {
            let mut axes = [AxisMode::Locked; 3];
            for setting in &c.axes {
                if let Some(dof) = dim2::Dof::parse(&setting.axis) {
                    axes[dof.index()] = axis_mode(setting);
                }
            }
            dim2::JointDesc::Generic {
                frame1: dim2::Frame {
                    position: a,
                    rotation: c.rotation_a,
                },
                frame2: dim2::Frame {
                    position: b,
                    rotation: c.rotation_b,
                },
                axes,
            }
        }
    })
}
fn axis_mode(setting: &AxisSetting) -> AxisMode {
    match setting.mode {
        AxisKind::Free => AxisMode::Free,
        AxisKind::Locked => AxisMode::Locked,
        AxisKind::Limited => AxisMode::Limited {
            min: setting.min,
            max: setting.max,
        },
    }
}
fn options<C: Config>(c: &C) -> JointOptions {
    JointOptions {
        break_limits: c.break_limits(),
        contacts_enabled: c.collide_connected(),
    }
}

/// An engine-physics world that can host authored joints.
trait Sim {
    type Cfg: Config;
    fn create(&mut self, id: u64, a: u64, b: u64, config: &Self::Cfg) -> Result<()>;
    fn apply_motors(&mut self, id: u64, old: &[JointMotor], new: &[JointMotor]) -> Result<()>;
    fn set_break(&mut self, id: u64, limits: BreakLimits) -> Result<()>;
    fn delete(&mut self, id: u64) -> bool;
    fn live(&self, id: u64) -> bool;
    /// No room for another joint.
    fn full(&self) -> bool;
    fn broken(&self, id: u64) -> Option<JointBroken>;
    fn drain(&mut self) -> Vec<JointBroken>;
    fn has_body(bodies: &super::Bodies, entity: Entity) -> bool;
}
macro_rules! sim {
    ($world:ty, $dim:ident, $cfg:ty, $desc:expr, $bodies:ident) => {
        impl Sim for $world {
            type Cfg = $cfg;
            fn create(&mut self, id: u64, a: u64, b: u64, config: &$cfg) -> Result<()> {
                let desc = $desc(config)?;
                self.insert_joint_with(id, a, b, desc, options(config))
                    .map_err(mapped)?;
                if let Err(error) = self.apply_motors(id, &[], &config.motors) {
                    self.remove_joint(id);
                    return Err(error);
                }
                Ok(())
            }
            fn apply_motors(
                &mut self,
                id: u64,
                old: &[JointMotor],
                new: &[JointMotor],
            ) -> Result<()> {
                for motor in old.iter().filter(|m| new.iter().all(|n| n.axis != m.axis)) {
                    if let Some(dof) = $dim::Dof::parse(&motor.axis) {
                        self.clear_joint_motor(id, dof).map_err(mapped)?;
                    }
                }
                for motor in new.iter().filter(|m| !old.contains(m)) {
                    let dof = $dim::Dof::parse(&motor.axis)
                        .ok_or_else(|| bad("motor has an unknown axis"))?;
                    self.set_joint_motor(id, dof, motor.desc())
                        .map_err(mapped)?;
                }
                Ok(())
            }
            fn set_break(&mut self, id: u64, limits: BreakLimits) -> Result<()> {
                self.set_joint_break_limits(id, limits).map_err(mapped)
            }
            fn delete(&mut self, id: u64) -> bool {
                self.remove_joint(id)
            }
            fn live(&self, id: u64) -> bool {
                self.has_joint(id)
            }
            fn full(&self) -> bool {
                self.joint_count() >= self.capacity().saturating_mul(4)
            }
            fn broken(&self, id: u64) -> Option<JointBroken> {
                self.broken_joint(id)
            }
            fn drain(&mut self) -> Vec<JointBroken> {
                self.drain_joint_broken()
            }
            fn has_body(bodies: &super::Bodies, entity: Entity) -> bool {
                bodies.$bodies.contains_key(&entity)
            }
        }
    };
}
sim!(World2D, dim2, PhysicsJoint2D, desc2, two);
sim!(World3D, dim3, PhysicsJoint3D, desc3, three);

/// Joint entity as the engine-physics joint ID.
fn joint_id(entity: Entity) -> u64 {
    entity.to_bits()
}
fn endpoint(world: &World, own: Entity, id: &Option<String>) -> Option<Entity> {
    match id.as_deref() {
        None => Some(own),
        Some(id) => crate::services::find_entity(world, id),
    }
}
/// Resolved body entities of a joint, if both exist.
fn ends<C: Config>(world: &World, entity: Entity, config: &C) -> Option<(Entity, Entity)> {
    let (a, b) = config.ends();
    Some((
        endpoint(world, entity, a)?,
        endpoint(world, entity, &Some(b.clone()?))?,
    ))
}

/// A joint currently mirrored into engine-physics.
struct Live<C> {
    config: C,
    first: Entity,
    second: Entity,
    /// The joint broke. engine-physics keeps only the last 1024 break records, so the project
    /// remembers the break itself: a broken joint stays broken until its component is edited,
    /// even after its record was evicted.
    broken: bool,
}
fn structure<C: Config>(config: &C) -> C {
    let mut copy = config.clone();
    copy.strip_tunables();
    copy
}

/// Mirrors every authored joint of one dimension into its engine-physics world. A joint whose
/// endpoint has no body (a body that is not simulated, or that fractured) is removed; a joint
/// that does not fit in the joint table is "starved" until room appears.
fn sync_dimension<S: Sim>(
    sim: &mut S,
    bodies: &super::Bodies,
    live: &mut BTreeMap<Entity, Live<S::Cfg>>,
    starved: (&mut BTreeSet<Entity>, &mut usize),
    world: &World,
) -> Result<()> {
    let gone: Vec<Entity> = live
        .keys()
        .filter(|e| world.get::<S::Cfg>(**e).is_none())
        .copied()
        .collect();
    for entity in gone {
        sim.delete(joint_id(entity));
        live.remove(&entity);
    }
    for entity_ref in world.iter_entities() {
        let entity = entity_ref.id();
        let Some(config) = entity_ref.get::<S::Cfg>() else {
            continue;
        };
        let id = joint_id(entity);
        let ready = ends(world, entity, config)
            .filter(|(a, b)| a != b && S::has_body(bodies, *a) && S::has_body(bodies, *b));
        let Some((first, second)) = ready else {
            // A broken joint outlives a missing endpoint (a starved or removed body): the
            // break is remembered, so the joint is not re-created intact when the body comes
            // back. Only an edit of the component repairs it.
            if let Some(current) = live.get_mut(&entity)
                && (current.broken || sim.broken(id).is_some())
            {
                current.broken = true;
                continue;
            }
            if live.remove(&entity).is_some() {
                sim.delete(id);
            }
            continue;
        };
        if let Some(current) = live.get_mut(&entity) {
            let unchanged = current.first == first
                && current.second == second
                && structure(&current.config) == structure(config);
            if unchanged {
                if sim.live(id) {
                    if current.config.motors() != config.motors() {
                        sim.apply_motors(id, current.config.motors(), config.motors())?;
                    }
                    if current.config.break_limits() != config.break_limits() {
                        sim.set_break(id, config.break_limits())?;
                    }
                    current.config = config.clone();
                    continue;
                }
                if current.broken || sim.broken(id).is_some() {
                    // Stays broken until the component is edited: structure or thresholds.
                    if current.config.break_limits() == config.break_limits() {
                        current.broken = true;
                        current.config = config.clone();
                        continue;
                    }
                    sim.delete(id);
                }
                // Otherwise a body was rebuilt and took its joint with it: re-create.
            } else {
                sim.delete(id);
            }
        }
        if sim.full() {
            live.remove(&entity);
            if starved.0.insert(entity) {
                *starved.1 += 1;
            }
            continue;
        }
        starved.0.remove(&entity);
        sim.create(id, first.to_bits(), second.to_bits(), config)?;
        live.insert(
            entity,
            Live {
                config: config.clone(),
                first,
                second,
                broken: false,
            },
        );
    }
    Ok(())
}
fn check_scene<C: Config>(world: &World, has_body: impl Fn(&World, Entity) -> bool) -> Result<()> {
    for entity_ref in world.iter_entities() {
        let Some(config) = entity_ref.get::<C>() else {
            continue;
        };
        config.check()?;
        let (a, b) = config.ends();
        let own = entity_ref.id();
        let named = [
            endpoint(world, own, a),
            b.as_deref()
                .and_then(|id| crate::services::find_entity(world, id)),
        ];
        if let (Some(first), Some(second)) = (named[0], named[1])
            && first == second
        {
            return Err(bad("must connect two different bodies"));
        }
        for body in named.into_iter().flatten() {
            // A fractured body lost its engine.PhysicsBody on purpose: the joint is simply
            // inactive (the sync removes it) and the scene stays valid.
            if !has_body(world, body) && world.get::<super::Fractured>(body).is_none() {
                return Err(bad(&format!(
                    "body entity has no engine.PhysicsBody{}D of the joint's dimension",
                    if C::DIMENSION == Dimension::Two { 2 } else { 3 }
                )));
            }
        }
    }
    Ok(())
}

pub(super) struct Joints {
    two: BTreeMap<Entity, Live<PhysicsJoint2D>>,
    three: BTreeMap<Entity, Live<PhysicsJoint3D>>,
}
impl Joints {
    pub(super) fn new() -> Self {
        Self {
            two: BTreeMap::new(),
            three: BTreeMap::new(),
        }
    }
}
fn authored(world: &World, entity: Entity) -> Value {
    world
        .get::<crate::AuthoredId>(entity)
        .map_or(Value::Null, |id| json!(id.id))
}
impl Feature for Joints {
    fn name(&self) -> &'static str {
        "joints"
    }
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        joint_descriptors()
    }
    fn register_components(&self, app: &mut engine_core::EngineApp) -> Result<()> {
        crate::register_component::<PhysicsJoint2D>(app, "engine.PhysicsJoint2D", 1)?;
        crate::register_component::<PhysicsJoint3D>(app, "engine.PhysicsJoint3D", 1)
    }
    fn validate(&self, world: &World, _settings: &Resolved) -> Result<()> {
        check_scene::<PhysicsJoint2D>(world, |w, e| w.get::<super::PhysicsBody2D>(e).is_some())?;
        check_scene::<PhysicsJoint3D>(world, |w, e| w.get::<super::PhysicsBody3D>(e).is_some())
    }
    fn sync(&mut self, cx: Context<'_>, world: &World) -> Result<()> {
        let worlds = cx.worlds;
        sync_dimension(
            &mut worlds.two,
            cx.bodies,
            &mut self.two,
            (&mut worlds.starved, &mut worlds.starved_new),
            world,
        )?;
        sync_dimension(
            &mut worlds.three,
            cx.bodies,
            &mut self.three,
            (&mut worlds.starved, &mut worlds.starved_new),
            world,
        )
    }
    fn after_substep(&mut self, cx: Context<'_>, world: &mut World, _dt: f32) -> Result<()> {
        let events = [(2, cx.worlds.two.drain()), (3, cx.worlds.three.drain())];
        for (dimension, events) in events {
            for event in events {
                let Some(joint) = Entity::try_from_bits(event.id) else {
                    continue;
                };
                let ends = if dimension == 2 {
                    self.two.get(&joint).map(|l| (l.first, l.second))
                } else {
                    self.three.get(&joint).map(|l| (l.first, l.second))
                };
                match dimension {
                    2 => {
                        if let Some(live) = self.two.get_mut(&joint) {
                            live.broken = true;
                        }
                    }
                    _ => {
                        if let Some(live) = self.three.get_mut(&joint) {
                            live.broken = true;
                        }
                    }
                }
                let (first, second) = ends.unzip();
                let payload = json!({
                    "dimension": dimension,
                    "joint": authored(world, joint),
                    "force": event.force,
                    "torque": event.torque,
                    "body_a": first.map_or(Value::Null, |e| authored(world, e)),
                    "body_b": second.map_or(Value::Null, |e| authored(world, e)),
                });
                let mut targets = vec![joint];
                targets.extend([first, second].into_iter().flatten());
                targets.dedup();
                for target in targets {
                    if world.get_entity(target).is_ok() {
                        emit(world, self, "broken", target, Some(joint), payload.clone())?;
                    }
                }
            }
        }
        Ok(())
    }
}

// ---- Native API ----------------------------------------------------------------------

/// Adds (or replaces) an `engine.PhysicsJoint2D` on `entity` after validating it. The
/// constraint is created on the next simulation sync.
pub fn add_joint_2d(world: &mut World, entity: Entity, joint: PhysicsJoint2D) -> Result<()> {
    joint.check()?;
    world
        .get_entity_mut(entity)
        .map_err(|_| invalid("Entity does not exist"))?
        .insert(joint);
    Ok(())
}
/// Adds (or replaces) an `engine.PhysicsJoint3D` on `entity` after validating it.
pub fn add_joint_3d(world: &mut World, entity: Entity, joint: PhysicsJoint3D) -> Result<()> {
    joint.check()?;
    world
        .get_entity_mut(entity)
        .map_err(|_| invalid("Entity does not exist"))?
        .insert(joint);
    Ok(())
}
/// Removes the joint component of `dimension`; returns whether one existed. The constraint
/// disappears on the next sync.
pub fn remove_joint(world: &mut World, entity: Entity, dimension: Dimension) -> bool {
    let Ok(mut entity) = world.get_entity_mut(entity) else {
        return false;
    };
    match dimension {
        Dimension::Two => entity.take::<PhysicsJoint2D>().is_some(),
        Dimension::Three => entity.take::<PhysicsJoint3D>().is_some(),
    }
}
fn edit_motors<C: Config>(
    world: &mut World,
    entity: Entity,
    edit: impl FnOnce(&mut Vec<JointMotor>),
) -> Result<()> {
    let mut component = world
        .get_mut::<C>(entity)
        .ok_or_else(|| invalid("Entity has no joint component of that dimension"))?;
    let mut next = component.clone();
    edit(next.motors_mut());
    next.check()?;
    *component = next;
    Ok(())
}
/// Adds or replaces the motor on `motor.axis` of an authored joint. The change is applied
/// in place (no rebuild) on the next simulation sync.
pub fn set_joint_motor(
    world: &mut World,
    entity: Entity,
    dimension: Dimension,
    motor: JointMotor,
) -> Result<()> {
    let edit = |motors: &mut Vec<JointMotor>| match motors.iter_mut().find(|m| m.axis == motor.axis)
    {
        Some(existing) => *existing = motor.clone(),
        None => motors.push(motor.clone()),
    };
    match dimension {
        Dimension::Two => edit_motors::<PhysicsJoint2D>(world, entity, edit),
        Dimension::Three => edit_motors::<PhysicsJoint3D>(world, entity, edit),
    }
}
/// Removes the motor on `axis`, if any.
pub fn clear_joint_motor(
    world: &mut World,
    entity: Entity,
    dimension: Dimension,
    axis: &str,
) -> Result<()> {
    let edit = |motors: &mut Vec<JointMotor>| motors.retain(|m| m.axis != axis);
    match dimension {
        Dimension::Two => edit_motors::<PhysicsJoint2D>(world, entity, edit),
        Dimension::Three => edit_motors::<PhysicsJoint3D>(world, entity, edit),
    }
}
/// Whether the authored joint is live, broken or not simulated.
pub fn joint_status(world: &World, entity: Entity, dimension: Dimension) -> Result<JointStatus> {
    let physics = service(world)?;
    let id = joint_id(entity);
    let (live, broken) = match dimension {
        Dimension::Two => (
            physics.worlds.two.has_joint(id),
            physics.worlds.two.broken_joint(id),
        ),
        Dimension::Three => (
            physics.worlds.three.has_joint(id),
            physics.worlds.three.broken_joint(id),
        ),
    };
    Ok(if live {
        JointStatus::Active
    } else if let Some(event) = broken {
        JointStatus::Broken {
            force: event.force,
            torque: event.torque,
        }
    } else {
        JointStatus::Inactive
    })
}
/// Relative pose and load of a live joint (see [`JointReadout`]).
pub fn joint_state(world: &World, entity: Entity, dimension: Dimension) -> Result<JointReadout> {
    let physics = service(world)?;
    let id = joint_id(entity);
    let missing = || invalid("Joint is not live in the simulation");
    Ok(match dimension {
        Dimension::Two => {
            let s = physics.worlds.two.joint_state(id).ok_or_else(missing)?;
            JointReadout {
                dimension,
                position: Vec3::new(s.position[0], s.position[1], 0.),
                angles: Vec3::new(0., 0., s.angles),
                force: Vec3::new(s.force[0], s.force[1], 0.),
                torque: Vec3::new(0., 0., s.torque),
            }
        }
        Dimension::Three => {
            let s = physics.worlds.three.joint_state(id).ok_or_else(missing)?;
            JointReadout {
                dimension,
                position: Vec3::from(s.position),
                angles: Vec3::from(s.angles),
                force: Vec3::from(s.force),
                torque: Vec3::from(s.torque),
            }
        }
    })
}
/// Every joint that broke and has not been re-created, by entity (2D before 3D, ascending).
pub fn broken_joints(world: &World) -> Result<Vec<BrokenJoint>> {
    let physics = service(world)?;
    let convert = |dimension, events: Vec<JointBroken>| {
        events
            .into_iter()
            .filter_map(move |event| {
                Some(BrokenJoint {
                    entity: Entity::try_from_bits(event.id)?,
                    dimension,
                    force: event.force,
                    torque: event.torque,
                })
            })
            .collect::<Vec<_>>()
    };
    let mut result = convert(Dimension::Two, physics.worlds.two.broken_joints());
    result.extend(convert(
        Dimension::Three,
        physics.worlds.three.broken_joints(),
    ));
    Ok(result)
}
/// Replaces the break thresholds of an authored joint (`None` disables a threshold).
pub fn set_joint_break_limits(
    world: &mut World,
    entity: Entity,
    dimension: Dimension,
    force: Option<f32>,
    torque: Option<f32>,
) -> Result<()> {
    fn edit<C: Config>(
        world: &mut World,
        entity: Entity,
        apply: impl FnOnce(&mut C),
    ) -> Result<()> {
        let mut component = world
            .get_mut::<C>(entity)
            .ok_or_else(|| invalid("Entity has no joint component of that dimension"))?;
        let mut next = component.clone();
        apply(&mut next);
        next.check()?;
        *component = next;
        Ok(())
    }
    match dimension {
        Dimension::Two => edit::<PhysicsJoint2D>(world, entity, |c| {
            c.break_force = force;
            c.break_torque = torque;
        }),
        Dimension::Three => edit::<PhysicsJoint3D>(world, entity, |c| {
            c.break_force = force;
            c.break_torque = torque;
        }),
    }
}

// ---- Schema ----------------------------------------------------------------------------

fn vector(n: usize) -> Value {
    json!({"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n})
}
fn number(min: Option<f64>, description: &str) -> Value {
    let mut value = json!({"type":"number","description":description});
    if let Some(min) = min {
        value["minimum"] = json!(min);
    }
    value
}
fn schema(n: usize) -> Value {
    let axes: &[&str] = if n == 2 {
        &["lin_x", "lin_y", "ang"]
    } else {
        &["lin_x", "lin_y", "lin_z", "ang_x", "ang_y", "ang_z"]
    };
    let kinds: Vec<&str> = if n == 2 {
        vec![
            "fixed",
            "revolute",
            "prismatic",
            "spring",
            "distance",
            "rope",
            "generic",
        ]
    } else {
        vec![
            "fixed",
            "revolute",
            "prismatic",
            "spherical",
            "spring",
            "distance",
            "rope",
            "generic",
        ]
    };
    let id = json!({"type":"string","minLength":1,"maxLength":256});
    let rotation = if n == 2 {
        json!({"type":"number"})
    } else {
        vector(4)
    };
    let axis_setting = json!({"type":"object","additionalProperties":false,"properties":{
        "axis":{"type":"string","enum":axes},
        "mode":{"type":"string","enum":["free","locked","limited"]},
        "min":{"type":"number"},"max":{"type":"number"}}});
    let motor = json!({"type":"object","additionalProperties":false,"properties":{
        "axis":{"type":"string","enum":axes},
        "target_position":{"type":"number"},"target_velocity":{"type":"number"},
        "stiffness":{"type":"number","minimum":0},"damping":{"type":"number","minimum":0},
        "max_force":{"type":"number","minimum":0},"force_based":{"type":"boolean"}}});
    let mut properties = json!({
        "kind":{"type":"string","enum":kinds},
        "body_a":{"type":"string","minLength":1,"maxLength":256,
            "description":"Entity id of the first body; omitted means this entity."},
        "body_b":id,
        "anchor_a":vector(n),"anchor_b":vector(n),
        "rotation_a":rotation,"rotation_b":rotation,
        "axis":vector(n),
        "limits":vector(2),
        "rest_length":number(Some(0.), "Spring rest length in meters."),
        "stiffness":number(Some(0.), "Spring stiffness in N/m."),
        "damping":number(Some(0.), "Spring damping in N*s/m."),
        "min_distance":number(Some(0.), "Distance joint minimum in meters."),
        "max_distance":number(Some(0.), "Distance or rope joint maximum in meters."),
        "axes":{"type":"array","maxItems":MAX_AXES,"items":axis_setting},
        "motors":{"type":"array","maxItems":MAX_AXES,"items":motor},
        "break_force":number(Some(0.), "Constraint force magnitude (N) above which the joint breaks."),
        "break_torque":number(Some(0.), "Constraint torque magnitude (N*m) above which the joint breaks."),
        "collide_connected":{"type":"boolean"}
    });
    if n == 3 {
        properties["swing_limit"] =
            number(Some(0.), "Spherical cone half-angle in radians, 0..=pi.");
        properties["twist_limits"] = vector(2);
    }
    json!({"type":"object","properties":properties,"additionalProperties":false})
}
fn joint_descriptors() -> Vec<crate::ComponentDescriptor> {
    let make = |name: &str, defaults: Value, n| crate::ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema: schema(n),
        defaults,
        requires: vec![],
        entity_references: vec!["/body_a".into(), "/body_b".into()],
        data_only: false,
    };
    vec![
        make(
            "engine.PhysicsJoint2D",
            serde_json::to_value(PhysicsJoint2D::default()).expect("finite defaults"),
            2,
        ),
        make(
            "engine.PhysicsJoint3D",
            serde_json::to_value(PhysicsJoint3D::default()).expect("finite defaults"),
            3,
        ),
    ]
}
