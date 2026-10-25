//! The authored `engine.BehaviorTreeAgent` component.
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Catalog name of the component.
pub const BEHAVIOR_TREE_AGENT: &str = "engine.BehaviorTreeAgent";
/// Most blackboard overrides one agent may carry.
pub const MAX_OVERRIDES: usize = 64;

/// Runs a `behavior_tree` graph asset on an entity.
///
/// Authored data only: the live tree state, blackboard and debug log live in the
/// runtime service, so snapshots of this component never change while the game
/// runs.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BehaviorTreeAgent {
    /// Asset id of a `graph` asset of kind `behavior_tree`.
    pub graph: String,
    /// Initial blackboard values by key, overriding the graph variable defaults.
    pub blackboard: BTreeMap<String, Value>,
    /// Seconds between tree ticks; 0 ticks every runtime tick.
    pub tick_interval: f32,
    /// A disabled agent runs nothing; enabling it starts the tree afresh.
    pub enabled: bool,
    /// Mixed into every random selector, so agents sharing a graph differ.
    pub seed: u64,
    /// Navigation mesh asset `move_to` uses when the entity's
    /// `engine.NavigationAgent` does not name one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navmesh: Option<String>,
}

impl Default for BehaviorTreeAgent {
    fn default() -> Self {
        Self {
            graph: String::new(),
            blackboard: BTreeMap::new(),
            tick_interval: 0.0,
            enabled: true,
            seed: 0,
            navmesh: None,
        }
    }
}

impl BehaviorTreeAgent {
    /// Whether a change from `self` to `next` needs a fresh tree (as opposed to
    /// only a different tick interval).
    pub(crate) fn needs_restart(&self, next: &Self) -> bool {
        self.graph != next.graph
            || self.blackboard != next.blackboard
            || self.seed != next.seed
            || self.navmesh != next.navmesh
            || self.enabled != next.enabled
    }

    /// Checks the fields that need no project context.
    pub(crate) fn check_shape(&self) -> Result<()> {
        if self.graph.is_empty() {
            return Err(Error::new(
                "behavior_tree.graph.missing",
                "engine.BehaviorTreeAgent needs a graph asset id",
            ));
        }
        if self.blackboard.len() > MAX_OVERRIDES {
            return Err(Error::new(
                "behavior_tree.blackboard.invalid",
                format!("more than {MAX_OVERRIDES} blackboard overrides"),
            ));
        }
        if !self.tick_interval.is_finite() || !(0.0..=3600.0).contains(&self.tick_interval) {
            return Err(Error::new(
                "behavior_tree.tick_interval.invalid",
                "tick_interval must be 0 to 3600 seconds",
            ));
        }
        Ok(())
    }
}

/// Catalog descriptor: JSON schema and defaults.
pub fn descriptor() -> crate::ComponentDescriptor {
    crate::ComponentDescriptor {
        name: BEHAVIOR_TREE_AGENT.into(),
        version: 1,
        schema: json!({
            "type": "object",
            "required": ["graph"],
            "additionalProperties": false,
            "properties": {
                "graph": {"type": "string", "maxLength": 128,
                    "description": "Asset id of a graph asset of kind behavior_tree."},
                "blackboard": {"type": "object",
                    "description": "Initial blackboard values by key, typed by the graph variables."},
                "tick_interval": {"type": "number", "minimum": 0, "maximum": 3600,
                    "description": "Seconds between tree ticks; 0 ticks every runtime tick."},
                "enabled": {"type": "boolean"},
                "seed": {"type": "integer", "minimum": 0,
                    "description": "Mixed into every random selector."},
                "navmesh": {"type": "string", "maxLength": 128,
                    "description": "Navigation asset for move_to when the NavigationAgent has none."}
            }
        }),
        defaults: serde_json::to_value(BehaviorTreeAgent::default())
            .expect("finite builtin defaults"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}
