//! AI gameplay for the runtime: behavior tree and state tree agents, perception.
//!
//! * `engine.BehaviorTreeAgent` ([`BehaviorTreeAgent`]): runs a `behavior_tree`
//!   graph asset on an entity.
//! * [`register_bt_task`]: native tasks and services the graph names.
//! * `behavior_tree.finished` / `behavior_tree.aborted` events and the
//!   `behavior_trees` telemetry of the runtime snapshot.
//! * [`state_tree`]: `engine.StateTreeAgent`, its native API, events and the
//!   `state_trees` telemetry.
//! * [`perception`]: sight, hearing and damage senses, memory, events, blackboard
//!   keys and the `perception` telemetry.
//! * [`env_query`] and [`smart_object`]: environment queries, smart object slots and
//!   the built-in `engine.run_query` / `engine.use_smart_object` tree tasks.
//!
//! The services run immediately before `simulation::step` (perception, smart
//! objects and queries first, then behavior trees, then state trees),
//! so a `move_to` task that sets the entity's `engine.NavigationAgent` target is
//! routed in the same tick. See `docs/ai-behavior-trees.md` and
//! `docs/ai-state-trees.md`.
use crate::{Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_ai::{Blackboard, behavior_tree::TaskFactory};
use engine_core::EngineApp;

/// Catalog name of the navigation agent component `move_to` drives.
const NAVIGATION_AGENT: &str = "engine.NavigationAgent";

mod component;
pub mod env_query;
mod host;
pub mod perception;
mod runtime;
pub mod smart_object;
pub mod state_tree;
mod tasks;

pub use component::{BEHAVIOR_TREE_AGENT, BehaviorTreeAgent, MAX_OVERRIDES, descriptor};
// Native tasks implement `NativeTask<World>`: `cx.world` is the live world and
// `cx.subject` the agent entity's bits (`Entity::from_bits`).
pub use engine_ai::{
    BbValue, Blackboard as BtBlackboard,
    behavior_tree::{NativeTask, Status as TaskStatus, TaskContext},
};
pub use env_query::{ENVIRONMENT_QUERIES, EnvironmentQueries, status as env_query_status};
pub use perception::{
    PERCEPTION_NOISE, PERCEPTION_SENSOR, PERCEPTION_SETTINGS, PERCEPTION_STIMULUS, PerceptionNoise,
    PerceptionSensor, PerceptionSettings, PerceptionStatus, PerceptionStimulus, Perceptions,
    register_hearing_attenuation, report_damage, report_noise, status as perception_status,
};
pub use runtime::{
    AgentStatus, BehaviorTreeStatus, BehaviorTrees, EVENT_ABORTED, EVENT_FINISHED, MAX_AGENTS,
    MAX_EVENTS_PER_TICK, STATUS_MAX_AGENTS, status,
};
pub use smart_object::{SMART_OBJECT, SmartObject, status as smart_object_status};
pub use state_tree::{
    STATE_TREE_AGENT, StateTreeAgent, StateTreeStatus, StateTrees, register_state_tree_evaluator,
    register_state_tree_task, status as state_tree_status,
};

/// Catalog descriptors of this module's components.
pub fn descriptors() -> Vec<crate::ComponentDescriptor> {
    let mut all = vec![descriptor(), state_tree::descriptor()];
    all.extend(perception::descriptors());
    all.push(env_query::descriptor());
    all.push(smart_object::descriptor());
    all
}

/// Registers the component adapters and the runtime services.
pub fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<BehaviorTreeAgent>(app, BEHAVIOR_TREE_AGENT, 1)?;
    app.world_mut().insert_resource(BehaviorTrees::default());
    state_tree::register(app)?;
    perception::register(app)?;
    smart_object::register(app)?;
    env_query::register(app)?;
    // Built-in tree tasks under reserved names.
    for (name, factory) in [
        (
            tasks::RUN_QUERY,
            tasks::run_query_factory as TaskFactory<World>,
        ),
        (tasks::USE_SMART_OBJECT, tasks::use_smart_object_factory),
    ] {
        app.world_mut()
            .resource_mut::<BehaviorTrees>()
            .registry
            .register(name, factory)
            .map_err(|m| Error::new("behavior_tree.native.duplicate", m))?;
        state_tree::register_builtin(app, name, factory)?;
    }
    Ok(())
}

/// Brings every AI service in line with the world's agent components. Called at
/// runtime start and after scene transitions.
pub fn prepare(world: &mut World) -> Result<()> {
    perception::prepare(world)?;
    smart_object::prepare(world)?;
    runtime::prepare(world)?;
    state_tree::prepare(world)
}

/// Ticks every due agent: behavior trees, then state trees. Runs once per runtime
/// tick, before `simulation::step`.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    perception::step(world, dt)?;
    smart_object::step(world, dt)?;
    env_query::step(world)?;
    runtime::step(world, dt)?;
    state_tree::step(world, dt)
}

/// Stops every running tree without emitting events. Called when the runtime
/// stops.
pub fn shutdown(world: &mut World) {
    runtime::shutdown(world);
    state_tree::shutdown(world);
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

/// Scene-level checks of every AI component: see [`validate_behavior_trees`] and
/// [`state_tree::validate_scene`].
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    validate_behavior_trees(project, scene)?;
    state_tree::validate_scene(project, scene)?;
    perception::validate_scene(project, scene)?;
    validate_queries_and_objects(project, scene)
}

/// Query templates and smart objects: values within bounds, query names unique across
/// the scene, navmesh assets exist.
fn validate_queries_and_objects(project: &Project, scene: &Scene) -> Result<()> {
    let mut names = std::collections::BTreeSet::new();
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if let Some(value) = custom.get(env_query::ENVIRONMENT_QUERIES) {
            let queries: env_query::EnvironmentQueries =
                serde_json::from_value(value.data.clone())?;
            queries.check()?;
            for template in &queries.queries {
                if !names.insert(template.name.clone()) {
                    return Err(Error::new(
                        "env_query.duplicate",
                        format!("query name {:?} repeats across the scene", template.name),
                    ));
                }
                if let Some(navmesh) = &template.navmesh {
                    project.require_asset(navmesh, "navigation")?;
                }
            }
        }
        if let Some(value) = custom.get(smart_object::SMART_OBJECT) {
            let object: smart_object::SmartObject = serde_json::from_value(value.data.clone())?;
            object.check()?;
        }
    }
    Ok(())
}

/// Behavior tree checks: graph assets exist and compile as behavior trees,
/// blackboard overrides fit the graph's variables, and a `move_to` tree can
/// reach a navigation agent.
pub fn validate_behavior_trees(project: &Project, scene: &Scene) -> Result<()> {
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
