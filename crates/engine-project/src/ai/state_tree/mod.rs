//! AI gameplay for the runtime: state tree agents.
//!
//! * `engine.StateTreeAgent` ([`StateTreeAgent`]): runs a `state_tree` graph asset
//!   on an entity.
//! * [`register_state_tree_task`] and [`register_state_tree_evaluator`]: native
//!   tasks and evaluators the graph names.
//! * `state_tree.state_entered` / `state_tree.state_exited` / `state_tree.finished`
//!   events and the `state_trees` telemetry of the runtime snapshot.
//!
//! The service ticks right after the behavior trees and immediately before
//! `simulation::step`, so a `move_to` task that sets the entity's
//! `engine.NavigationAgent` target is routed in the same tick. Events targeted at
//! the entity (including `navigation.*`) are delivered to the tree as the events
//! `on_event` transitions listen for. See `docs/ai-state-trees.md`.
use crate::{Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_ai::{Blackboard, behavior_tree::TaskFactory};
use engine_core::EngineApp;

mod component;
mod host;
mod runtime;

pub use component::{MAX_OVERRIDES, STATE_TREE_AGENT, StateTreeAgent, descriptor};
pub use host::INPUTS;
pub use runtime::{
    AgentStatus, EVENT_FINISHED, EVENT_STATE_ENTERED, EVENT_STATE_EXITED, MAX_AGENTS,
    MAX_EVENTS_PER_TICK, MAX_STATE_EVENTS_PER_AGENT, STATUS_MAX_AGENTS, STATUS_MAX_TRANSITIONS,
    StateTreeStatus, StateTrees, prepare, shutdown, status, step,
};

/// Catalog name of the navigation agent component `move_to` drives.
const NAVIGATION_AGENT: &str = "engine.NavigationAgent";

/// Registers the component adapter and the runtime service.
pub fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<StateTreeAgent>(app, STATE_TREE_AGENT, 1)?;
    app.world_mut().insert_resource(StateTrees::default());
    Ok(())
}

fn trees<'a>(app: &'a mut EngineApp, what: &str) -> Result<Mut<'a, StateTrees>> {
    app.world_mut()
        .get_resource_mut::<StateTrees>()
        .ok_or_else(|| {
            Error::new(
                "state_tree.unavailable",
                format!("{what} needs a project runtime"),
            )
        })
}

/// Registers a native task under `name` for `native_task` nodes. Register before
/// the runtime starts; the factory runs once per agent and graph node and its
/// result is reused across activations. `start` runs when the owning state is
/// entered, `tick` on every later tree tick, `abort` when the state is exited
/// while the task still runs.
///
/// # Errors
/// A repeated name, or an `app` that is not a project runtime.
pub fn register_state_tree_task(
    app: &mut EngineApp,
    name: &str,
    factory: TaskFactory<World>,
) -> Result<()> {
    trees(app, "register_state_tree_task")?
        .tasks
        .register(name, factory)
        .map_err(|message| Error::new("state_tree.native.duplicate", message))
}

/// Registers a native evaluator under `name` for `native_evaluator` nodes. Its
/// `start` runs on the tree's first tick (in place of a tick), `tick` on every
/// later tick before transitions are evaluated, and `abort` when the tree ends.
/// The returned status is ignored. Names are separate from task names.
///
/// # Errors
/// A repeated name, or an `app` that is not a project runtime.
pub fn register_state_tree_evaluator(
    app: &mut EngineApp,
    name: &str,
    factory: TaskFactory<World>,
) -> Result<()> {
    trees(app, "register_state_tree_evaluator")?
        .evaluators
        .register(name, factory)
        .map_err(|message| Error::new("state_tree.native.duplicate", message))
}

/// Scene-level checks: graph assets exist and compile as state trees, blackboard
/// overrides fit the graph's variables, host inputs exist, and a `move_to` tree
/// can reach a navigation agent.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(STATE_TREE_AGENT) else {
            continue;
        };
        let agent: StateTreeAgent = serde_json::from_value(value.data.clone())?;
        agent.check_shape()?;
        project.require_asset(&agent.graph, "graph")?;
        if let Some(navmesh) = &agent.navmesh {
            project.require_asset(navmesh, "navigation")?;
        }
        let tree = runtime::compile_graph(project, &agent.graph)?;
        let mut board = Blackboard::from_variables(tree.variables())
            .map_err(|e| Error::new("state_tree.blackboard.invalid", e.to_string()))?;
        for (key, value) in &agent.blackboard {
            board.set_json(key, value).map_err(|e| {
                Error::new(
                    "state_tree.blackboard.invalid",
                    format!("entity {}: {e}", entity.id),
                )
            })?;
        }
        if let Some(input) = tree
            .input_names()
            .iter()
            .find(|i| !INPUTS.contains(&i.as_str()))
        {
            return Err(Error::new(
                "state_tree.input.unknown",
                format!(
                    "graph {} reads host input {input:?}; the runtime provides {}",
                    agent.graph,
                    INPUTS.join(", ")
                ),
            ));
        }
        if tree.uses_move_to() {
            let navigation = entity
                .components
                .custom
                .get(NAVIGATION_AGENT)
                .ok_or_else(|| {
                    Error::new(
                        "state_tree.navigation.missing",
                        format!(
                            "entity {}: graph {} uses move_to, which needs an engine.NavigationAgent",
                            entity.id, agent.graph
                        ),
                    )
                })?;
            let has_navmesh = agent.navmesh.is_some()
                || navigation
                    .data
                    .get("navmesh")
                    .is_some_and(|v| v.is_string());
            if !has_navmesh {
                return Err(Error::new(
                    "state_tree.navigation.missing",
                    format!(
                        "entity {}: move_to needs a navmesh on the agent or on its engine.NavigationAgent",
                        entity.id
                    ),
                ));
            }
        }
    }
    Ok(())
}
