//! Stable binding tokens are independent of native window or gamepad integer codes.
use crate::{Error, Result};
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod bindings;
mod devices;
mod mapper;
mod sources;

pub use bindings::*;
pub use devices::*;
pub use mapper::*;
pub use sources::*;

pub type BindingMap = BTreeMap<String, Vec<String>>;
#[derive(Clone, Debug, Serialize, Deserialize, Resource, PartialEq)]
#[serde(default)]
pub struct ActionFrame {
    /// Device or binding lifecycle cancellation, distinct from a normal release.
    pub cancelled: BTreeSet<String>,
    pub pressed: BTreeSet<String>,
    pub held: BTreeSet<String>,
    pub released: BTreeSet<String>,
    pub values: BTreeMap<String, f32>,
    pub pointer: [f32; 2],
    pub wheel: [f32; 2],
    pub focused: bool,
}
impl Default for ActionFrame {
    fn default() -> Self {
        Self {
            cancelled: BTreeSet::new(),
            pressed: BTreeSet::new(),
            held: BTreeSet::new(),
            released: BTreeSet::new(),
            values: BTreeMap::new(),
            pointer: [0.0; 2],
            wheel: [0.0; 2],
            focused: true,
        }
    }
}
impl ActionFrame {
    pub fn pressed(&self, action: &str) -> bool {
        self.pressed.contains(action)
    }
    pub fn held(&self, action: &str) -> bool {
        self.held.contains(action)
    }
    pub fn released(&self, action: &str) -> bool {
        self.released.contains(action)
    }
    pub fn value(&self, action: &str) -> f32 {
        self.values.get(action).copied().unwrap_or(0.0)
    }
}
pub type InputSnapshot = ActionFrame;
#[derive(Clone, Debug)]
pub struct RawInput {
    pub down: BTreeSet<String>,
    pub axes: BTreeMap<String, f32>,
    pub pointer: [f32; 2],
    pub wheel: [f32; 2],
    pub focused: bool,
    pub gamepad_connected: bool,
}
impl Default for RawInput {
    fn default() -> Self {
        Self {
            down: BTreeSet::new(),
            axes: BTreeMap::new(),
            pointer: [0.0; 2],
            wheel: [0.0; 2],
            focused: true,
            gamepad_connected: false,
        }
    }
}
/// Injected actions are one-tick pulses. Physical holds remain held until release.
pub fn merge_input(
    physical: &ActionFrame,
    injected: &BTreeSet<String>,
    previous: &mut BTreeSet<String>,
) -> ActionFrame {
    let mut result = physical.clone();
    result.held.extend(injected.iter().cloned());
    result.pressed = result.held.difference(previous).cloned().collect();
    result.released = previous.difference(&result.held).cloned().collect();
    for name in injected {
        result.values.insert(name.clone(), 1.0);
    }
    *previous = result.held.clone();
    result
}
