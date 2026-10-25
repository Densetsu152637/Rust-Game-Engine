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
