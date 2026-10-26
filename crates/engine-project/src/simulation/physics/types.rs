//! Authored rigid-body components (`engine.PhysicsBody2D`, `engine.PhysicsBody3D`).
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
/// Collider geometry. `cylinder` is 3D only; `trimesh` and `heightfield` need fixed or
/// kinematic motion; `compound` parts may be box, ball, capsule, cylinder or convex hull.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Box,
    Ball,
    Capsule,
    Cylinder,
    ConvexHull,
    #[serde(rename = "trimesh")]
    TriMesh,
    #[serde(rename = "heightfield")]
    HeightField,
    Compound,
}
/// How touching colliders combine friction or restitution. When both sides disagree the
/// higher-priority rule wins: `max > multiply > min > average`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineRule {
    #[default]
    Average,
    Min,
    Max,
    Multiply,
}
impl From<CombineRule> for engine_physics::CombineRule {
    fn from(value: CombineRule) -> Self {
        match value {
            CombineRule::Average => Self::Average,
            CombineRule::Min => Self::Min,
            CombineRule::Max => Self::Max,
            CombineRule::Multiply => Self::Multiply,
        }
    }
}
/// Inline material. `preset` replaces density, friction, restitution and both combine rules
/// with a named `engine.PhysicsSettings` material; `layer_name` replaces `layer`/`mask` with
/// the named layer's collision-matrix groups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BodyMaterial {
    pub density: f32,
    pub friction: f32,
    pub restitution: f32,
    pub friction_combine: CombineRule,
    pub restitution_combine: CombineRule,
    pub sensor: bool,
    pub layer: u32,
    pub mask: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_name: Option<String>,
}
impl Default for BodyMaterial {
    fn default() -> Self {
        Self {
            density: 1.,
            friction: 0.5,
            restitution: 0.,
            friction_combine: CombineRule::Average,
            restitution_combine: CombineRule::Average,
            sensor: false,
            layer: 1,
            mask: u32::MAX,
            preset: None,
            layer_name: None,
        }
    }
}
impl From<&BodyMaterial> for engine_physics::Material {
    fn from(v: &BodyMaterial) -> Self {
        Self {
            density: v.density,
            friction: v.friction,
            restitution: v.restitution,
            friction_combine: v.friction_combine.into(),
            restitution_combine: v.restitution_combine.into(),
            sensor: v.sensor,
            layer: v.layer,
            mask: v.mask,
        }
    }
}
/// One rigid part of a `compound` 2D body, in body-local coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompoundPart2D {
    pub shape: Shape,
    pub position: [f32; 2],
    pub rotation: f32,
    pub half_extents: [f32; 2],
    pub radius: f32,
    pub half_height: f32,
    pub points: Vec<[f32; 2]>,
}
impl Default for CompoundPart2D {
    fn default() -> Self {
        Self {
            shape: Shape::Box,
            position: [0.; 2],
            rotation: 0.,
            half_extents: [0.5; 2],
            radius: 0.5,
            half_height: 0.5,
            points: vec![],
        }
    }
}
/// One rigid part of a `compound` 3D body, in body-local coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompoundPart3D {
    pub shape: Shape,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub half_extents: [f32; 3],
    pub radius: f32,
    pub half_height: f32,
    pub points: Vec<[f32; 3]>,
}
impl Default for CompoundPart3D {
    fn default() -> Self {
        Self {
            shape: Shape::Box,
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            half_extents: [0.5; 3],
            radius: 0.5,
            half_height: 0.5,
            points: vec![],
        }
    }
}
/// Shape parameters by shape: box `half_extents`; ball `radius`; capsule `radius` and
/// `half_height`; convex hull `points`; trimesh `vertices` and `indices`; heightfield
/// `heights` sampled along X over `heightfield_scale[0]` meters, multiplied by
/// `heightfield_scale[1]`; compound `parts`. All lengths scale with the world transform.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsBody2D {
    pub motion: Motion,
    pub shape: Shape,
    pub half_extents: [f32; 2],
    pub radius: f32,
    pub half_height: f32,
    pub points: Vec<[f32; 2]>,
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<[u32; 3]>,
    pub heights: Vec<f32>,
    pub heightfield_scale: [f32; 2],
    pub parts: Vec<CompoundPart2D>,
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
            points: vec![],
            vertices: vec![],
            indices: vec![],
            heights: vec![],
            heightfield_scale: [1.; 2],
            parts: vec![],
            velocity: [0.; 2],
            angular_velocity: 0.,
            ccd: false,
            material: BodyMaterial::default(),
        }
    }
}
/// As [`PhysicsBody2D`], plus `cylinder` (`radius`, `half_height`, Y axis) and a 3D
/// heightfield of `heightfield_rows` x `heightfield_columns` row-major `heights` (rows along
/// Z, columns along X) spanning `heightfield_scale[0]` x `heightfield_scale[2]` meters with
/// heights multiplied by `heightfield_scale[1]`. `heightfield_holes` lists removed cells,
/// row-major over `(heightfield_rows - 1) x (heightfield_columns - 1)`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsBody3D {
    pub motion: Motion,
    pub shape: Shape,
    pub half_extents: [f32; 3],
    pub radius: f32,
    pub half_height: f32,
    pub points: Vec<[f32; 3]>,
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<[u32; 3]>,
    pub heights: Vec<f32>,
    pub heightfield_rows: u32,
    pub heightfield_columns: u32,
    pub heightfield_scale: [f32; 3],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub heightfield_holes: Vec<u32>,
    pub parts: Vec<CompoundPart3D>,
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
            points: vec![],
            vertices: vec![],
            indices: vec![],
            heights: vec![],
            heightfield_rows: 0,
            heightfield_columns: 0,
            heightfield_scale: [1.; 3],
            heightfield_holes: vec![],
            parts: vec![],
            velocity: [0.; 3],
            angular_velocity: [0.; 3],
            ccd: false,
            material: BodyMaterial::default(),
        }
    }
}
