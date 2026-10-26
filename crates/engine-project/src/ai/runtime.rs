//! The behavior tree runtime service: one resource owning every agent's tree,
//! ticked immediately before `simulation::step`.
use super::{
    component::BehaviorTreeAgent,
    host::{Motion, WorldHost},
};
use crate::{Error, Project, Result, events::DeliveredEvents, runtime::AuthoredId};
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue, Blackboard,
    behavior_tree::{
        AbortRecord, BehaviorTree, NativeTaskInstances, NativeTaskRegistry, Status, TreeInstance,
    },
};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Most agents one runtime supports.
pub const MAX_AGENTS: usize = 512;
/// Most events all agents together may emit per runtime tick.
pub const MAX_EVENTS_PER_TICK: usize = 64;
/// Most `behavior_tree.aborted` events one agent may emit per tick.
const MAX_ABORT_EVENTS: usize = 4;
/// Least simulated seconds between `behavior_tree.finished` events of one agent.
const FINISHED_MIN_INTERVAL: f64 = 0.25;
/// Agents listed in telemetry.
pub const STATUS_MAX_AGENTS: usize = 32;
const STATUS_MAX_PATH: usize = 16;
const STATUS_MAX_KEYS: usize = 16;
const STATUS_MAX_STRING: usize = 64;
const EPS: f64 = 1e-6;

pub const EVENT_FINISHED: &str = "behavior_tree.finished";
pub const EVENT_ABORTED: &str = "behavior_tree.aborted";

pub(super) struct Agent {
    config: BehaviorTreeAgent,
    /// `None` while disabled.
    instance: Option<TreeInstance>,
    natives: NativeTaskInstances<World>,
    motion: Motion,
    /// Simulated seconds not yet consumed by a tree tick.
    accumulated: f64,
    last_finished_event: Option<f64>,
}

/// Every agent's tree plus the shared native task registry.
#[derive(Resource, Default)]
pub struct BehaviorTrees {
    agents: BTreeMap<Entity, Agent>,
    /// Compiled trees by graph asset id, shared by agents of the same graph.
    trees: BTreeMap<String, Arc<BehaviorTree>>,
    pub(super) registry: NativeTaskRegistry<World>,
    /// Simulated seconds since the runtime started.
    time: f64,
    dropped_events: u64,
}

impl BehaviorTrees {
    /// The live blackboard of the agent on `entity`; absent while the agent is
    /// disabled or before the runtime synchronised it.
    pub fn blackboard(&self, entity: Entity) -> Option<&Blackboard> {
        self.agents
            .get(&entity)?
            .instance
            .as_ref()
            .map(TreeInstance::blackboard)
    }
    /// Mutable blackboard, for perception and gameplay systems. Writes notify
    /// observers at the start of the next tree tick. Not callable from a native
    /// task: tasks receive their blackboard in the task context.
    pub fn blackboard_mut(&mut self, entity: Entity) -> Option<&mut Blackboard> {
        self.agents
            .get_mut(&entity)?
            .instance
            .as_mut()
            .map(TreeInstance::blackboard_mut)
    }
}

/// What one drive of an agent produced.
#[derive(Default)]
struct Output {
    finished: Option<Status>,
    aborts: Vec<AbortRecord>,
    events: Vec<(String, serde_json::Value)>,
    dropped: usize,
}

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Compiles the `behavior_tree` graph asset `id`.
pub(super) fn compile_graph(project: &Project, id: &str) -> Result<Arc<BehaviorTree>> {
    let document = crate::graphs::load_graph(project, id)?;
    if document.kind != engine_ai::behavior_tree::KIND {
        return Err(invalid(
            "behavior_tree.graph.kind",
            format!(
                "graph asset {id} is a {} graph, not a behavior_tree",
                document.kind
            ),
        ));
    }
    engine_ai::behavior_tree::compile(&document)
        .map(Arc::new)
        .map_err(|d| crate::graphs::into_error(d, &project.assets[id].path))
}

/// Brings the service in line with the world's agent components and checks the
/// native task registry. Called at runtime start and after scene transitions.
pub fn prepare(world: &mut World) -> Result<()> {
    with_state(world, sync)
}

/// Ticks every due agent. Runs once per runtime tick, before `simulation::step`.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    with_state(world, |world, state| step_inner(world, state, dt))
}

/// Aborts every running tree without emitting events, so native tasks can release
/// what they hold. Called when the runtime stops.
pub fn shutdown(world: &mut World) {
    let Some(mut state) = world.remove_resource::<BehaviorTrees>() else {
        return;
    };
    for (entity, mut agent) in std::mem::take(&mut state.agents) {
        // A failing native factory cannot matter while stopping.
        let _ = drive(world, &state.registry, entity, &mut agent, None);
    }
    world.insert_resource(state);
}

fn with_state(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut BehaviorTrees) -> Result<()>,
) -> Result<()> {
    let Some(mut state) = world.remove_resource::<BehaviorTrees>() else {
        return Ok(());
    };
    let result = run(world, &mut state);
    world.insert_resource(state);
    result
}

fn sync(world: &mut World, state: &mut BehaviorTrees) -> Result<()> {
    let configs: BTreeMap<Entity, BehaviorTreeAgent> = world
        .iter_entities()
        .filter_map(|e| e.get::<BehaviorTreeAgent>().cloned().map(|c| (e.id(), c)))
        .collect();
    if configs.len() > MAX_AGENTS {
        return Err(invalid(
            "behavior_tree.capacity",
            format!("more than {MAX_AGENTS} behavior tree agents"),
        ));
    }
    let removed: Vec<Entity> = state
        .agents
        .keys()
        .filter(|e| !configs.contains_key(e))
        .copied()
        .collect();
    let mut budget = MAX_EVENTS_PER_TICK;
    let mut queue_full = false;
    for entity in removed {
        stop_agent(world, state, entity, &mut budget, &mut queue_full)?;
    }
    for (entity, config) in configs {
        match state.agents.get_mut(&entity) {
            Some(agent) if agent.config == config => continue,
            Some(agent) if !agent.config.needs_restart(&config) => {
                agent.config.tick_interval = config.tick_interval;
                continue;
            }
            Some(_) => stop_agent(world, state, entity, &mut budget, &mut queue_full)?,
            None => {}
        }
        let agent = build_agent(world, state, config)?;
        state.agents.insert(entity, agent);
    }
    let used: Vec<&str> = state
        .agents
        .values()
        .map(|a| a.config.graph.as_str())
        .collect();
    state.trees.retain(|id, _| used.contains(&id.as_str()));
    Ok(())
}

fn build_agent(
    world: &World,
    state: &mut BehaviorTrees,
    config: BehaviorTreeAgent,
) -> Result<Agent> {
    config.check_shape()?;
    let instance = if config.enabled {
        let tree = match state.trees.get(&config.graph) {
            Some(tree) => Arc::clone(tree),
            None => {
                let project = &world.resource::<crate::runtime::ProjectResource>().0;
                let tree = compile_graph(project, &config.graph)?;
                state.trees.insert(config.graph.clone(), Arc::clone(&tree));
                tree
            }
        };
        for name in tree.native_names() {
            if !state.registry.contains(name) {
                return Err(invalid(
                    "behavior_tree.native.unregistered",
                    format!(
                        "graph {} uses native task {name:?}; register it with register_bt_task before start",
                        config.graph
                    ),
                ));
            }
        }
        Some(
            TreeInstance::new(tree, &config.blackboard, config.seed)
                .map_err(|e| invalid("behavior_tree.blackboard.invalid", e.to_string()))?,
        )
    } else {
        None
    };
    Ok(Agent {
        config,
        instance,
        natives: NativeTaskInstances::default(),
        motion: Motion::default(),
        accumulated: 0.0,
        last_finished_event: None,
    })
}

/// Aborts a running tree (agent removed, disabled or rebuilt) and forgets it.
fn stop_agent(
    world: &mut World,
    state: &mut BehaviorTrees,
    entity: Entity,
    budget: &mut usize,
    queue_full: &mut bool,
) -> Result<()> {
    let Some(mut agent) = state.agents.remove(&entity) else {
        return Ok(());
    };
    let output = drive(world, &state.registry, entity, &mut agent, None)?;
    if world.get_entity(entity).is_ok() {
        emit(world, state, entity, &mut agent, output, budget, queue_full)?;
    }
    Ok(())
}

fn drive(
    world: &mut World,
    registry: &NativeTaskRegistry<World>,
    entity: Entity,
    agent: &mut Agent,
    dt: Option<f32>,
) -> Result<Output> {
    let Agent {
        instance,
        natives,
        motion,
        config,
        ..
    } = agent;
    let Some(instance) = instance.as_mut() else {
        return Ok(Output::default());
    };
    let mut host = WorldHost {
        world,
        entity,
        registry,
        natives,
        motion,
        navmesh: config.navmesh.as_deref(),
        events: Vec::new(),
        dropped: 0,
    };
    let (finished, aborts) = match dt {
        Some(dt) => {
            let report = instance.tick(dt, &mut host);
            (report.finished, report.aborts)
        }
        None => (None, instance.stop(&mut host)),
    };
    let (events, dropped) = (host.events, host.dropped);
    if let Some(message) = natives.take_errors().into_iter().next() {
        return Err(invalid("behavior_tree.native.failed", message));
    }
    Ok(Output {
        finished,
        aborts,
        events,
        dropped,
    })
}

fn navigation_events(world: &World) -> BTreeMap<Entity, Vec<String>> {
    let mut events: BTreeMap<Entity, Vec<String>> = BTreeMap::new();
    if let Some(delivered) = world.get_resource::<DeliveredEvents>() {
        for event in &delivered.0 {
            if event.name.starts_with("navigation.")
                && let Some(target) = event.target
            {
                events.entry(target).or_default().push(event.name.clone());
            }
        }
    }
    events
}

fn step_inner(world: &mut World, state: &mut BehaviorTrees, dt: f32) -> Result<()> {
    state.time += f64::from(dt);
    sync(world, state)?;
    if state.agents.is_empty() {
        return Ok(());
    }
    let navigation = navigation_events(world);
    let mut budget = MAX_EVENTS_PER_TICK;
    let mut queue_full = false;
    let entities: Vec<Entity> = state.agents.keys().copied().collect();
    for entity in entities {
        let Some(mut agent) = state.agents.remove(&entity) else {
            continue;
        };
        for name in navigation.get(&entity).into_iter().flatten() {
            agent.motion.apply(name);
        }
        let result = tick_agent(world, &state.registry, entity, &mut agent, dt);
        let result = result.and_then(|output| match output {
            Some(output) => emit(
                world,
                state,
                entity,
                &mut agent,
                output,
                &mut budget,
                &mut queue_full,
            ),
            None => Ok(()),
        });
        state.agents.insert(entity, agent);
        result?;
    }
    Ok(())
}

fn tick_agent(
    world: &mut World,
    registry: &NativeTaskRegistry<World>,
    entity: Entity,
    agent: &mut Agent,
    dt: f32,
) -> Result<Option<Output>> {
    if agent.instance.is_none() {
        return Ok(None);
    }
    let interval = f64::from(agent.config.tick_interval);
    agent.accumulated += f64::from(dt);
    if interval > 0.0 && agent.accumulated + EPS < interval {
        return Ok(None);
    }
    let elapsed = agent.accumulated as f32;
    agent.accumulated = 0.0;
    drive(world, registry, entity, agent, Some(elapsed)).map(Some)
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Success => "success",
        Status::Failure => "failure",
        Status::Running => "running",
    }
}

/// Turns one drive's output into bounded events for the agent's entity.
fn emit(
    world: &mut World,
    state: &mut BehaviorTrees,
    entity: Entity,
    agent: &mut Agent,
    output: Output,
    budget: &mut usize,
    queue_full: &mut bool,
) -> Result<()> {
    let graph = agent.config.graph.clone();
    let event = |name: &str, payload: serde_json::Value| crate::EngineEvent {
        name: name.to_owned(),
        source: None,
        target: Some(entity),
        payload,
    };
    let mut dropped = output.dropped + output.aborts.len().saturating_sub(MAX_ABORT_EVENTS);
    let mut events: Vec<crate::EngineEvent> = output
        .events
        .into_iter()
        .map(|(name, payload)| event(&name, payload))
        .collect();
    for abort in output.aborts.iter().take(MAX_ABORT_EVENTS) {
        let mut payload =
            json!({"graph": graph, "node": abort.node, "reason": abort.reason.as_str()});
        if let Some(key) = &abort.key {
            payload["key"] = json!(key);
        }
        events.push(event(EVENT_ABORTED, payload));
    }
    if let Some(status) = output.finished {
        let due = agent
            .last_finished_event
            .is_none_or(|at| state.time - at + EPS >= FINISHED_MIN_INTERVAL);
        if due {
            agent.last_finished_event = Some(state.time);
            let ticks = agent.instance.as_ref().map_or(0, TreeInstance::ticks);
            events.push(event(
                EVENT_FINISHED,
                json!({"graph": graph, "status": status_name(status), "ticks": ticks}),
            ));
        } else {
            dropped += 1;
        }
    }
    if events.len() > *budget {
        dropped += events.len() - *budget;
        events.truncate(*budget);
    }
    if *queue_full {
        dropped += events.len();
    } else if !events.is_empty() {
        let count = events.len();
        match crate::emit_events(world, events) {
            Ok(()) => *budget -= count,
            Err(error)
                if error
                    .diagnostics
                    .first()
                    .is_some_and(|d| d.code == "event.capacity") =>
            {
                dropped += count;
                *queue_full = true;
            }
            Err(error) => return Err(error),
        }
    }
    state.dropped_events += dropped as u64;
    Ok(())
}

/// Telemetry for one agent.
#[derive(Clone, Debug, Serialize)]
pub struct AgentStatus {
    /// Authored entity id.
    pub entity: String,
    pub graph: String,
    pub enabled: bool,
    pub ticks: u64,
    /// Outcome of the most recent complete run of the tree.
    pub last_finished: Option<Status>,
    /// Node ids from the root to the deepest running node (capped).
    pub active_path: Vec<String>,
    /// Set blackboard keys (capped; long strings cut).
    pub blackboard: BTreeMap<String, BbValue>,
}

/// The `behavior_trees` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct BehaviorTreeStatus {
    /// Agents in the world, of which the first [`STATUS_MAX_AGENTS`] by authored id
    /// are listed.
    pub total: usize,
    pub agents: Vec<AgentStatus>,
    /// Events dropped by the per-tick bounds, rate limits or a full event queue.
    pub dropped_events: u64,
}

/// Bounded telemetry; `None` when the world has no behavior tree agents.
pub fn status(world: &World) -> Option<BehaviorTreeStatus> {
    let state = world.get_resource::<BehaviorTrees>()?;
    if state.agents.is_empty() {
        return None;
    }
    let mut agents: Vec<AgentStatus> = state
        .agents
        .iter()
        .map(|(entity, agent)| AgentStatus {
            entity: world
                .get::<AuthoredId>(*entity)
                .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone()),
            graph: agent.config.graph.clone(),
            enabled: agent.config.enabled,
            ticks: agent.instance.as_ref().map_or(0, TreeInstance::ticks),
            last_finished: agent
                .instance
                .as_ref()
                .and_then(TreeInstance::last_finished),
            active_path: agent
                .instance
                .as_ref()
                .map(|i| {
                    let mut path = i.active_path();
                    path.truncate(STATUS_MAX_PATH);
                    path
                })
                .unwrap_or_default(),
            blackboard: agent
                .instance
                .as_ref()
                .map(|i| i.blackboard().summary(STATUS_MAX_KEYS, STATUS_MAX_STRING))
                .unwrap_or_default(),
        })
        .collect();
    agents.sort_by(|a, b| a.entity.cmp(&b.entity));
    let total = agents.len();
    agents.truncate(STATUS_MAX_AGENTS);
    Some(BehaviorTreeStatus {
        total,
        agents,
        dropped_events: state.dropped_events,
    })
}
