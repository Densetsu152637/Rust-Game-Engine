use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    Fixed,
    #[default]
    Dynamic,
    Kinematic,
}
impl From<Motion> for engine_physics::BodyKind {
    fn from(value: Motion) -> Self {
        match value {
            Motion::Fixed => Self::Fixed,
            Motion::Dynamic => Self::Dynamic,
            Motion::Kinematic => Self::Kinematic,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Box,
    Ball,
    Capsule,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BodyMaterial {
    pub density: f32,
    pub friction: f32,
    pub restitution: f32,
    pub sensor: bool,
    pub layer: u32,
    pub mask: u32,
}
impl Default for BodyMaterial {
    fn default() -> Self {
        Self {
            density: 1.,
            friction: 0.5,
            restitution: 0.,
            sensor: false,
            layer: 1,
            mask: u32::MAX,
        }
    }
}
impl From<&BodyMaterial> for engine_physics::Material {
    fn from(v: &BodyMaterial) -> Self {
        Self {
            density: v.density,
            friction: v.friction,
            restitution: v.restitution,
            sensor: v.sensor,
            layer: v.layer,
            mask: v.mask,
        }
    }
}
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsBody2D {
    pub motion: Motion,
    pub shape: Shape,
    pub half_extents: [f32; 2],
    pub radius: f32,
    pub half_height: f32,
    pub velocity: [f32; 2],
    pub angular_velocity: f32,
    pub ccd: bool,
    pub material: BodyMaterial,
}
impl Default for PhysicsBody2D {
    fn default() -> Self {
        Self {
            motion: Motion::Dynamic,
            shape: Shape::Box,
            half_extents: [0.5; 2],
            radius: 0.5,
            half_height: 0.5,
            velocity: [0.; 2],
            angular_velocity: 0.,
            ccd: false,
            material: BodyMaterial::default(),
        }
    }
}
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsBody3D {
    pub motion: Motion,
    pub shape: Shape,
    pub half_extents: [f32; 3],
    pub radius: f32,
    pub half_height: f32,
    pub velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub ccd: bool,
    pub material: BodyMaterial,
}
impl Default for PhysicsBody3D {
    fn default() -> Self {
        Self {
            motion: Motion::Dynamic,
            shape: Shape::Box,
            half_extents: [0.5; 3],
            radius: 0.5,
            half_height: 0.5,
            velocity: [0.; 3],
            angular_velocity: [0.; 3],
            ccd: false,
            material: BodyMaterial::default(),
        }
    }
}
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnimationPlayer {
    pub clip: String,
    pub looping: bool,
    pub speed: f32,
    pub bindings: Vec<String>,
}
/// Mesh-local deformation binding. Node indices refer to the retained model document.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkinBinding {
    pub model: String,
    pub mesh_node: usize,
    pub primitive: usize,
    pub bindings: Vec<String>,
}
impl Default for AnimationPlayer {
    fn default() -> Self {
        Self {
            clip: String::new(),
            looping: true,
            speed: 1.,
            bindings: vec![],
        }
    }
}
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NavigationAgent {
    pub waypoints: Vec<[f32; 3]>,
    pub speed: f32,
}
impl Default for NavigationAgent {
    fn default() -> Self {
        Self {
            waypoints: vec![],
            speed: 1.,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimulationConfig {
    pub gravity_2d: [f32; 2],
    pub gravity_3d: [f32; 3],
    pub body_capacity: usize,
}
impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            gravity_2d: [0., -9.81],
            gravity_3d: [0., -9.81, 0.],
            body_capacity: 16384,
        }
    }
}
