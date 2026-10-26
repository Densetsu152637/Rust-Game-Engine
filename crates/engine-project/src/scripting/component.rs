//! The authored `engine.ScriptGraph` component.
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use engine_script::{
    Capabilities, EmitScope,
    trap::{DEFAULT_FUEL, FUEL_RANGE},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Catalog name of the component.
pub const SCRIPT_GRAPH: &str = "engine.ScriptGraph";
/// Most variable overrides one component may carry.
pub const MAX_OVERRIDES: usize = 64;
/// Most component names in one capability list.
pub const MAX_CAPABILITY_NAMES: usize = 16;

/// What happens to an instance that traps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultPolicy {
    /// The instance stops running and a `script.faulted` event is emitted.
    #[default]
    Disable,
    /// The runtime tick returns the error, stopping the game loop.
    StopRuntime,
}

/// What an instance may touch. Everything is denied unless listed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptCapabilities {
    /// Component names `engine.component_get` may read on the entity.
    pub read: Vec<String>,
    /// Component names `engine.component_set` may write on the entity.
    pub write: Vec<String>,
    /// Entities the script may spawn. Runtime spawn is not shipped: must be 0.
    pub spawn: u16,
    /// Whom `engine.emit` and `emit_event` may address.
    pub emit: EmitScope,
}

impl ScriptCapabilities {
    pub(crate) fn to_runtime(&self) -> Capabilities {
        Capabilities {
            read: self.read.iter().cloned().collect(),
            write: self.write.iter().cloned().collect(),
            spawn: self.spawn,
            emit: self.emit,
        }
    }
}

/// Runs a `script_graph` graph asset on an entity.
///
/// Authored data only: variables, pending waits and fault state live in the
/// runtime service, so snapshots of this component never change while the game
/// runs.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptGraph {
    /// Asset id of a `graph` asset of kind `script_graph`.
    pub graph: String,
    /// Initial variable values by name, overriding the graph's defaults.
    pub variables: BTreeMap<String, Value>,
    /// Seeds `engine.random_int` and `engine.random_float` for this instance.
    pub seed: u64,
    /// A disabled instance runs nothing; enabling it starts the script afresh.
    pub enabled: bool,
    /// Fuel per tick (one op costs one), within 100..=1,000,000.
    pub fuel_per_tick: u64,
    pub capabilities: ScriptCapabilities,
    pub fault_policy: FaultPolicy,
}

impl Default for ScriptGraph {
    fn default() -> Self {
        Self {
            graph: String::new(),
            variables: BTreeMap::new(),
            seed: 0,
            enabled: true,
            fuel_per_tick: DEFAULT_FUEL,
            capabilities: ScriptCapabilities::default(),
            fault_policy: FaultPolicy::default(),
        }
    }
}

impl ScriptGraph {
    /// Checks the fields that need no project context.
    pub(crate) fn check_shape(&self) -> Result<()> {
        let invalid = |message: String| Error::new("script.component.invalid", message);
        if self.graph.is_empty() {
            return Err(invalid("engine.ScriptGraph needs a graph asset id".into()));
        }
        if self.variables.len() > MAX_OVERRIDES {
            return Err(invalid(format!(
                "more than {MAX_OVERRIDES} variable overrides"
            )));
        }
        if !(FUEL_RANGE.0..=FUEL_RANGE.1).contains(&self.fuel_per_tick) {
            return Err(invalid(format!(
                "fuel_per_tick must be {}..={}",
                FUEL_RANGE.0, FUEL_RANGE.1
            )));
        }
        if self.capabilities.spawn != 0 {
            return Err(Error::new(
                engine_script::codes::SPAWN_LIMIT,
                "runtime spawn is not shipped: capabilities.spawn must be 0",
            ));
        }
        for list in [&self.capabilities.read, &self.capabilities.write] {
            if list.len() > MAX_CAPABILITY_NAMES
                || list
                    .iter()
                    .any(|n| n.is_empty() || n.len() > 128 || n.chars().any(char::is_control))
            {
                return Err(invalid(format!(
                    "capability lists hold at most {MAX_CAPABILITY_NAMES} component names"
                )));
            }
        }
        Ok(())
    }
}

/// Catalog descriptor: JSON schema and defaults.
pub fn descriptor() -> crate::ComponentDescriptor {
    crate::ComponentDescriptor {
        name: SCRIPT_GRAPH.into(),
        version: 1,
        schema: json!({
            "type": "object",
            "required": ["graph"],
            "additionalProperties": false,
            "properties": {
                "graph": {"type": "string", "maxLength": 128,
                    "description": "Asset id of a graph asset of kind script_graph."},
                "variables": {"type": "object",
                    "description": "Initial variable values by name, typed by the graph variables."},
                "seed": {"type": "integer", "minimum": 0,
                    "description": "Seeds engine.random_int and engine.random_float."},
                "enabled": {"type": "boolean"},
                "fuel_per_tick": {"type": "integer", "minimum": 100, "maximum": 1_000_000,
                    "description": "Fuel per tick; one op costs one."},
                "capabilities": {"type": "object", "additionalProperties": false,
                    "properties": {
                        "read": {"type": "array", "maxItems": 16, "items": {"type": "string", "maxLength": 128},
                            "description": "Component names engine.component_get may read."},
                        "write": {"type": "array", "maxItems": 16, "items": {"type": "string", "maxLength": 128},
                            "description": "Component names engine.component_set may write."},
                        "spawn": {"type": "integer", "minimum": 0, "maximum": 0,
                            "description": "Reserved: runtime spawn is not shipped, so the maximum is 0."},
                        "emit": {"type": "string", "enum": ["self_only", "any"],
                            "description": "Whom emitted events may address."}
                    }},
                "fault_policy": {"type": "string", "enum": ["disable", "stop_runtime"],
                    "description": "disable stops the instance and emits script.faulted; stop_runtime fails the tick."}
            }
        }),
        defaults: serde_json::to_value(ScriptGraph::default()).expect("finite builtin defaults"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}
