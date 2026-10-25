use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navmesh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
    pub query: engine_navigation::QuerySettings,
    /// Increment from a native callback to request a new route from the current pose.
    pub repath: u64,
}
impl Default for NavigationAgent {
    fn default() -> Self {
        Self {
            waypoints: vec![],
            speed: 1.,
            navmesh: None,
            target: None,
            query: Default::default(),
            repath: 0,
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

/// Crowd-steered agent: ORCA avoidance over a baked NavMesh. Agents sharing a `navmesh` avoid
/// each other. Mutually exclusive with `engine.NavigationAgent`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CrowdAgent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navmesh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
    pub radius: f32,
    pub max_speed: f32,
    pub arrival_radius: f32,
    /// Authored id of an `engine.Formation` entity this agent takes a slot in (overrides `target`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formation: Option<String>,
    /// Increment from a native callback to request a new route from the current pose.
    pub repath: u64,
}
impl Default for CrowdAgent {
    fn default() -> Self {
        Self {
            navmesh: None,
            target: None,
            radius: 0.4,
            max_speed: 2.,
            arrival_radius: 0.1,
            formation: None,
            repath: 0,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObstacleShape {
    #[default]
    Box,
    Cylinder,
}
/// Runtime navmesh carving obstacle centred on the entity's world position (axis aligned; entity
/// rotation and scale are ignored).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NavigationObstacle {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navmesh: Option<String>,
    pub shape: ObstacleShape,
    /// Box half sizes (x, y, z).
    pub half_extents: [f32; 3],
    /// Cylinder radius.
    pub radius: f32,
    /// Cylinder half height.
    pub half_height: f32,
    pub enabled: bool,
}
impl Default for NavigationObstacle {
    fn default() -> Self {
        Self {
            navmesh: None,
            shape: ObstacleShape::Box,
            half_extents: [0.5; 3],
            radius: 0.5,
            half_height: 1.,
            enabled: true,
        }
    }
}
/// Off-mesh connection from the entity's world position to `end` (world space).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OffMeshLink {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navmesh: Option<String>,
    pub end: [f32; 3],
    /// Cost in polygon-hop units.
    pub cost: f32,
    pub bidirectional: bool,
    pub enabled: bool,
    pub snap_distance: f32,
}
impl Default for OffMeshLink {
    fn default() -> Self {
        Self {
            navmesh: None,
            end: [0.; 3],
            cost: 1.,
            bidirectional: true,
            enabled: true,
            snap_distance: 1.,
        }
    }
}
/// Slot layout for `engine.CrowdAgent`s whose `formation` names this entity. Anchored to the
/// entity's position, or to the crowd agent entity named by `leader`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Formation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navmesh: Option<String>,
    /// `[right, forward]` offsets in metres.
    pub slots: Vec<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    pub yaw: f32,
    pub follow_heading: bool,
}
impl Default for Formation {
    fn default() -> Self {
        Self {
            navmesh: None,
            slots: vec![],
            leader: None,
            yaw: 0.,
            follow_heading: false,
        }
    }
}
