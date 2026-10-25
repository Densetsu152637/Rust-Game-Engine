//! Authored `engine.Ragdoll` data, validation and the component descriptor.
use super::MAX_BONES;
use crate::Result;
use crate::simulation::invalid;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::f32::consts::PI;

/// How the skeleton is simulated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RagdollMode {
    /// Kinematic bones that follow the animation pose.
    #[default]
    Animated,
    /// Fully dynamic, limp bones.
    Ragdoll,
    /// Dynamic bones whose joint motors drive toward the animation pose.
    Powered,
}
impl RagdollMode {
    /// The authored name, also used in event payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Animated => "animated",
            Self::Ragdoll => "ragdoll",
            Self::Powered => "powered",
        }
    }
}
/// Collider shape of one bone, in a frame whose Y axis runs along the bone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoneShape {
    #[default]
    Capsule,
    Box,
    Ball,
}
/// Joint between a bone and its parent bone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoneJoint {
    /// Ball-and-socket with `swing_limit` swing bounds and `twist_limits`.
    #[default]
    Spherical,
    /// Hinge about `axis` (bone space) with `hinge_limits`.
    Hinge,
    /// Free angular 6-DoF joint with per-axis `angular_limits` about the bone's local axes.
    Generic,
    /// Rigid attachment.
    Fixed,
}
/// One simulated bone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RagdollBone {
    /// Authored id of the bone entity (needs `Transform`). Its position is the joint position.
    pub bone: String,
    pub shape: BoneShape,
    /// Collider length along the bone in meters; default is the distance to the child joint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f32>,
    /// Capsule/ball radius in meters; default is 0.2 of the length, within 0.02..=0.12.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// Box half extents in the bone-aligned frame (X, Y along the bone, Z).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub half_extents: Option<[f32; 3]>,
    /// Bone direction in bone-local space; default points at the farthest child bone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f32; 3]>,
    /// Total mass in kg; takes precedence over `density`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mass: Option<f32>,
    /// Density in kg/m^3; default is the ragdoll `density`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    pub joint: BoneJoint,
    /// Hinge axis in bone-local space.
    pub axis: [f32; 3],
    /// Spherical swing limit in radians about each of the two axes perpendicular to the bone.
    pub swing_limit: f32,
    /// Spherical twist range about the bone direction, radians.
    pub twist_limits: [f32; 2],
    /// Hinge range about `axis`, radians.
    pub hinge_limits: [f32; 2],
    /// Generic limits about the bone-local X, Y and Z axes, radians (`[-pi, pi]` is free).
    pub angular_limits: [[f32; 2]; 3],
    /// Per-bone motor strength multiplier for `powered` mode.
    pub strength: f32,
}
impl Default for RagdollBone {
    fn default() -> Self {
        Self {
            bone: String::new(),
            shape: BoneShape::Capsule,
            length: None,
            radius: None,
            half_extents: None,
            direction: None,
            mass: None,
            density: None,
            joint: BoneJoint::Spherical,
            axis: [1., 0., 0.],
            swing_limit: 1.,
            twist_limits: [-0.5, 0.5],
            hinge_limits: [-1.5, 1.5],
            angular_limits: [[-0.5, 0.5]; 3],
            strength: 1.,
        }
    }
}
/// The authored ragdoll, on the skeleton root or any other entity.
#[derive(bevy_ecs::prelude::Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ragdoll {
    pub mode: RagdollMode,
    /// Weight of the physics pose written to the joint transforms, 0..=1.
    pub blend: f32,
    /// Overall motor strength multiplier for `powered` mode, 0..=10.
    pub strength: f32,
    /// Seconds over which leaving `ragdoll`/`powered` for `animated` fades the physics pose out.
    pub blend_out_seconds: f32,
    /// Keeps bones without a parent bone kinematic (following the animation) while the rest
    /// is dynamic.
    pub pin_root: bool,
    /// Whether non-adjacent bones collide with each other.
    pub self_collision: bool,
    /// Whether switching from `animated` hands the animation velocity to the bodies.
    pub inherit_velocity: bool,
    /// Default bone density in kg/m^3.
    pub density: f32,
    pub friction: f32,
    pub restitution: f32,
    /// Optional named physics layer for the bone colliders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_name: Option<String>,
    /// Joint stiffness per kg m^2 of the bone subtree (1/s^2) at strength 1.
    pub motor_stiffness: f32,
    /// Joint damping per kg m^2 of the bone subtree (1/s) at strength 1.
    pub motor_damping: f32,
    /// Bone speed (m/s and rad/s) below which the ragdoll counts as at rest.
    pub settle_speed: f32,
    /// Seconds all bones must stay slow before `physics.ragdoll.settled`.
    pub settle_seconds: f32,
    pub bones: Vec<RagdollBone>,
}
impl Default for Ragdoll {
    fn default() -> Self {
        Self {
            mode: RagdollMode::Animated,
            blend: 1.,
            strength: 1.,
            blend_out_seconds: 0.,
            pin_root: false,
            self_collision: false,
            inherit_velocity: true,
            density: 1000.,
            friction: 0.8,
            restitution: 0.,
            layer_name: None,
            motor_stiffness: 1000.,
            motor_damping: 80.,
            settle_speed: 0.1,
            settle_seconds: 0.5,
            bones: vec![],
        }
    }
}
fn finite_in(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}
fn positive(value: Option<f32>, high: f32) -> bool {
    value.is_none_or(|v| v.is_finite() && v > 0. && v <= high)
}
fn limits_ok(limits: [f32; 2]) -> bool {
    limits.iter().all(|v| finite_in(*v, -PI, PI)) && limits[0] <= limits[1]
}
fn vector_ok(v: [f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite() && c.abs() <= 1e6) && v.iter().any(|c| c.abs() > 1e-6)
}
impl RagdollBone {
    fn validate(&self) -> Result<()> {
        let name = &self.bone;
        if name.is_empty() || name.len() > 256 {
            return Err(invalid(
                "Ragdoll bone requires an entity id of 1..=256 chars",
            ));
        }
        let message = |what: &str| invalid(format!("Ragdoll bone {name}: invalid {what}"));
        if !positive(self.length, 100.) {
            return Err(message("length"));
        }
        if !positive(self.radius, 10.) {
            return Err(message("radius"));
        }
        if !positive(self.mass, 1e6) || !positive(self.density, 1e5) {
            return Err(message("mass or density"));
        }
        if self
            .half_extents
            .is_some_and(|h| h.iter().any(|v| !v.is_finite() || *v <= 0. || *v > 100.))
        {
            return Err(message("half_extents"));
        }
        if self.direction.is_some_and(|d| !vector_ok(d)) || !vector_ok(self.axis) {
            return Err(message("direction or axis"));
        }
        if !finite_in(self.swing_limit, 0., PI) {
            return Err(message("swing_limit"));
        }
        if !limits_ok(self.twist_limits)
            || !limits_ok(self.hinge_limits)
            || !self.angular_limits.iter().all(|l| limits_ok(*l))
        {
            return Err(message("joint limits (ordered, within -pi..=pi)"));
        }
        if !finite_in(self.strength, 0., 10.) {
            return Err(message("strength"));
        }
        Ok(())
    }
}
impl Ragdoll {
    /// Bounds and finiteness of every authored value; no world access.
    pub fn validate(&self) -> Result<()> {
        let checks = [
            (finite_in(self.blend, 0., 1.), "blend (0..=1)"),
            (finite_in(self.strength, 0., 10.), "strength (0..=10)"),
            (
                finite_in(self.blend_out_seconds, 0., 60.),
                "blend_out_seconds (0..=60)",
            ),
            (finite_in(self.density, 1e-3, 1e5), "density"),
            (finite_in(self.friction, 0., 100.), "friction"),
            (finite_in(self.restitution, 0., 1.), "restitution"),
            (finite_in(self.motor_stiffness, 0., 1e6), "motor_stiffness"),
            (finite_in(self.motor_damping, 0., 1e6), "motor_damping"),
            (finite_in(self.settle_speed, 1e-4, 100.), "settle_speed"),
            (finite_in(self.settle_seconds, 0., 60.), "settle_seconds"),
        ];
        if let Some((_, what)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(invalid(format!("Ragdoll has invalid {what}")));
        }
        if self.bones.len() > MAX_BONES {
            return Err(invalid(format!("A ragdoll has at most {MAX_BONES} bones")));
        }
        let mut seen = BTreeSet::new();
        for bone in &self.bones {
            bone.validate()?;
            if !seen.insert(bone.bone.as_str()) {
                return Err(invalid(format!("Ragdoll lists bone {} twice", bone.bone)));
            }
        }
        Ok(())
    }
    /// Everything except the live controls (mode, blend, strength, fade): a change here
    /// rebuilds the bodies and joints.
    pub(super) fn structure(&self) -> Self {
        let mut copy = Self {
            mode: RagdollMode::Animated,
            blend: 1.,
            strength: 1.,
            blend_out_seconds: 0.,
            inherit_velocity: true,
            motor_stiffness: 0.,
            motor_damping: 0.,
            settle_speed: 0.1,
            settle_seconds: 0.,
            ..self.clone()
        };
        for bone in &mut copy.bones {
            bone.strength = 1.;
        }
        copy
    }
}

fn limits_schema() -> Value {
    json!({"type":"array","items":{"type":"number","minimum":-PI,"maximum":PI},"minItems":2,"maxItems":2})
}
fn vector3(description: &str) -> Value {
    json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3,"description":description})
}
fn bone_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["bone"],"properties":{
        "bone":{"type":"string","minLength":1,"maxLength":256,
            "description":"Authored id of the bone entity; its position is the joint position."},
        "shape":{"type":"string","enum":["capsule","box","ball"]},
        "length":{"type":"number","minimum":0.0001,"maximum":100,
            "description":"Collider length along the bone; default is the distance to the child joint."},
        "radius":{"type":"number","minimum":0.0001,"maximum":10},
        "half_extents":vector3("Box half extents in the bone-aligned frame (Y along the bone)."),
        "direction":vector3("Bone direction in bone-local space; default points at the farthest child bone."),
        "mass":{"type":"number","minimum":0.0001,"maximum":1000000,"description":"Kilograms; overrides density."},
        "density":{"type":"number","minimum":0.0001,"maximum":100000},
        "joint":{"type":"string","enum":["spherical","hinge","generic","fixed"]},
        "axis":vector3("Hinge axis in bone-local space."),
        "swing_limit":{"type":"number","minimum":0,"maximum":PI,"description":"Spherical swing limit about each axis perpendicular to the bone, radians."},
        "twist_limits":limits_schema(),
        "hinge_limits":limits_schema(),
        "angular_limits":{"type":"array","items":limits_schema(),"minItems":3,"maxItems":3,
            "description":"Generic joint limits about bone-local X, Y, Z."},
        "strength":{"type":"number","minimum":0,"maximum":10}
    }})
}
fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "mode":{"type":"string","enum":["animated","ragdoll","powered"]},
        "blend":{"type":"number","minimum":0,"maximum":1,
            "description":"Weight of the physics pose written to the joint transforms."},
        "strength":{"type":"number","minimum":0,"maximum":10,"description":"Overall powered-mode strength."},
        "blend_out_seconds":{"type":"number","minimum":0,"maximum":60,
            "description":"Fade-out when returning to animated mode (get-up)."},
        "pin_root":{"type":"boolean"},
        "self_collision":{"type":"boolean"},
        "inherit_velocity":{"type":"boolean"},
        "density":{"type":"number","minimum":0.001,"maximum":100000},
        "friction":{"type":"number","minimum":0,"maximum":100},
        "restitution":{"type":"number","minimum":0,"maximum":1},
        "layer_name":{"type":"string","minLength":1,"maxLength":64},
        "motor_stiffness":{"type":"number","minimum":0,"maximum":1000000},
        "motor_damping":{"type":"number","minimum":0,"maximum":1000000},
        "settle_speed":{"type":"number","minimum":0.0001,"maximum":100},
        "settle_seconds":{"type":"number","minimum":0,"maximum":60},
        "bones":{"type":"array","items":bone_schema(),"maxItems":MAX_BONES}
    }})
}
pub(super) fn descriptors() -> Vec<crate::ComponentDescriptor> {
    vec![crate::ComponentDescriptor {
        name: "engine.Ragdoll".into(),
        version: 1,
        schema: schema(),
        defaults: serde_json::to_value(Ragdoll::default()).expect("finite defaults"),
        requires: vec!["Transform".into()],
        // The catalog has no wildcard pointers, so each possible bone slot is declared.
        entity_references: (0..MAX_BONES).map(|i| format!("/bones/{i}/bone")).collect(),
        data_only: false,
    }]
}
