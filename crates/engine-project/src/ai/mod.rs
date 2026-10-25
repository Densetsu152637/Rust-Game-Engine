//! AI gameplay for the runtime: behavior tree agents.
//!
//! * `engine.BehaviorTreeAgent` ([`BehaviorTreeAgent`]): runs a `behavior_tree`
//!   graph asset on an entity.
//! * [`register_bt_task`]: native tasks and services the graph names.
//! * `behavior_tree.finished` / `behavior_tree.aborted` events and the
//!   `behavior_trees` telemetry of the runtime snapshot.
//!
//! The service ticks immediately before `simulation::step`, so a `move_to` task
//! that sets the entity's `engine.NavigationAgent` target is routed in the same
//! tick. See `docs/ai-behavior-trees.md`.
use crate::{Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_ai::{Blackboard, behavior_tree::TaskFactory};
use engine_core::EngineApp;

/// Catalog name of the navigation agent component `move_to` drives.
const NAVIGATION_AGENT: &str = "engine.NavigationAgent";

mod component;
mod host;
mod runtime;

pub use component::{BEHAVIOR_TREE_AGENT, BehaviorTreeAgent, MAX_OVERRIDES, descriptor};
// Native tasks implement `NativeTask<World>`: `cx.world` is the live world and
// `cx.subject` the agent entity's bits (`Entity::from_bits`).
pub use engine_ai::{
    BbValue, Blackboard as BtBlackboard,
    behavior_tree::{NativeTask, Status as TaskStatus, TaskContext},
};
pub use runtime::{
    AgentStatus, BehaviorTreeStatus, BehaviorTrees, EVENT_ABORTED, EVENT_FINISHED, MAX_AGENTS,
    MAX_EVENTS_PER_TICK, STATUS_MAX_AGENTS, prepare, shutdown, status, step,
};

/// Catalog descriptors of this module's components.
pub fn descriptors() -> Vec<crate::ComponentDescriptor> {
    vec![descriptor()]
}

/// Registers the component adapter and the runtime service.
pub fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<BehaviorTreeAgent>(app, BEHAVIOR_TREE_AGENT, 1)?;
    app.world_mut().insert_resource(BehaviorTrees::default());
    Ok(())
}

/// Registers a native task or service under `name` for `native_task`,
/// `native_service` nodes. Register before the runtime starts; the factory runs
/// once per agent and graph node and its result is reused across activations.
///
/// # Errors
/// A repeated name, or an `app` that is not a project runtime.
pub fn register_bt_task(
    app: &mut EngineApp,
    name: &str,
    factory: TaskFactory<World>,
) -> Result<()> {
    let mut trees = app
        .world_mut()
        .get_resource_mut::<BehaviorTrees>()
        .ok_or_else(|| {
            Error::new(
                "behavior_tree.unavailable",
                "register_bt_task needs a project runtime",
            )
        })?;
    trees
        .registry
        .register(name, factory)
        .map_err(|message| Error::new("behavior_tree.native.duplicate", message))
}

/// Scene-level checks: graph assets exist and compile as behavior trees,
/// blackboard overrides fit the graph's variables, and a `move_to` tree can
/// reach a navigation agent.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(BEHAVIOR_TREE_AGENT) else {
            continue;
        };
        let agent: BehaviorTreeAgent = serde_json::from_value(value.data.clone())?;
        agent.check_shape()?;
        project.require_asset(&agent.graph, "graph")?;
        if let Some(navmesh) = &agent.navmesh {
            project.require_asset(navmesh, "navigation")?;
        }
        let tree = runtime::compile_graph(project, &agent.graph)?;
        let mut board = Blackboard::from_variables(tree.variables())
            .map_err(|e| Error::new("behavior_tree.blackboard.invalid", e.to_string()))?;
        for (key, value) in &agent.blackboard {
            board.set_json(key, value).map_err(|e| {
                Error::new(
                    "behavior_tree.blackboard.invalid",
                    format!("entity {}: {e}", entity.id),
                )
            })?;
        }
        if tree.uses_move_to() {
            let navigation = entity
                .components
                .custom
                .get(NAVIGATION_AGENT)
                .ok_or_else(|| {
                    Error::new(
                        "behavior_tree.navigation.missing",
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
                    "behavior_tree.navigation.missing",
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
