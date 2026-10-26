//! The state tree runtime service: one resource owning every agent's state tree,
//! ticked right after the behavior trees and immediately before
//! `simulation::step`.
use super::{component::StateTreeAgent, host::StateWorldHost};
use crate::{
    Error, Project, Result, ai::host::Motion, ai::host::WorldHost, events::DeliveredEvents,
    runtime::AuthoredId,
};
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue, Blackboard,
    behavior_tree::{NativeTaskInstances, NativeTaskRegistry, Status},
    state_tree::{LogRecord, Phase, StateChange, StateTree, StateTreeInstance, TickReport},
};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Most agents one runtime supports.
pub const MAX_AGENTS: usize = 512;
/// Most events all agents together may emit per runtime tick.
pub const MAX_EVENTS_PER_TICK: usize = 64;
/// Most `state_tree.state_entered` / `state_exited` events one agent may emit per
/// tick; further changes are counted as dropped.
pub const MAX_STATE_EVENTS_PER_AGENT: usize = 8;
/// Agents listed in telemetry.
pub const STATUS_MAX_AGENTS: usize = 32;
const STATUS_MAX_PATH: usize = 16;
const STATUS_MAX_KEYS: usize = 16;
const STATUS_MAX_STRING: usize = 64;
/// Recent transitions listed per agent in telemetry.
pub const STATUS_MAX_TRANSITIONS: usize = 8;
const EPS: f64 = 1e-6;

pub const EVENT_STATE_ENTERED: &str = "state_tree.state_entered";
pub const EVENT_STATE_EXITED: &str = "state_tree.state_exited";
pub const EVENT_FINISHED: &str = "state_tree.finished";

struct Agent {
    config: StateTreeAgent,
    /// `None` while disabled.
    instance: Option<StateTreeInstance>,
    tasks: NativeTaskInstances<World>,
    evaluators: NativeTaskInstances<World>,
    motion: Motion,
    /// Simulated seconds not yet consumed by a tree tick.
    accumulated: f64,
}

/// Every agent's state tree plus the shared native registries.
#[derive(Resource, Default)]
pub struct StateTrees {
    agents: BTreeMap<Entity, Agent>,
    /// Compiled trees by graph asset id, shared by agents of the same graph.
    trees: BTreeMap<String, Arc<StateTree>>,
    pub(super) tasks: NativeTaskRegistry<World>,
    pub(super) evaluators: NativeTaskRegistry<World>,
    /// Simulated seconds since the runtime started.
    time: f64,
    dropped_events: u64,
}

impl StateTrees {
    /// The live blackboard of the agent on `entity`; absent while the agent is
    /// disabled or before the runtime synchronised it.
    pub fn blackboard(&self, entity: Entity) -> Option<&Blackboard> {
        self.agents
            .get(&entity)?
            .instance
            .as_ref()
            .map(StateTreeInstance::blackboard)
    }
    /// Mutable blackboard, for perception and gameplay systems; transitions see
    /// the writes on the agent's next tick. Not callable from a native task: tasks
    /// receive their blackboard in the task context.
    pub fn blackboard_mut(&mut self, entity: Entity) -> Option<&mut Blackboard> {
        self.agents
            .get_mut(&entity)?
            .instance
            .as_mut()
            .map(StateTreeInstance::blackboard_mut)
    }
    /// State ids from the top-level state to the active leaf for `entity`.
    pub fn active_path(&self, entity: Entity) -> Vec<String> {
        self.agents
            .get(&entity)
            .and_then(|a| a.instance.as_ref())
            .map(StateTreeInstance::active_path)
            .unwrap_or_default()
    }
}

/// What one drive of an agent produced.
#[derive(Default)]
struct Output {
    report: TickReport,
    events: Vec<(String, serde_json::Value)>,
    dropped: usize,
}

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Compiles the `state_tree` graph asset `id`.
pub(super) fn compile_graph(project: &Project, id: &str) -> Result<Arc<StateTree>> {
    let document = crate::graphs::load_graph(project, id)?;
    if document.kind != engine_ai::state_tree::KIND {
        return Err(invalid(
            "state_tree.graph.kind",
            format!(
                "graph asset {id} is a {} graph, not a state_tree",
                document.kind
            ),
        ));
    }
    engine_ai::state_tree::compile(&document)
        .map(Arc::new)
        .map_err(|d| crate::graphs::into_error(d, &project.assets[id].path))
}

/// Brings the service in line with the world's agent components and checks the
/// native registries. Called at runtime start and after scene transitions.
pub fn prepare(world: &mut World) -> Result<()> {
    with_state(world, sync)
}

/// Ticks every due agent. Runs once per runtime tick, before `simulation::step`.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    with_state(world, |world, state| step_inner(world, state, dt))
}

/// Stops every running tree without emitting events, so native tasks can release
/// what they hold. Called when the runtime stops.
pub fn shutdown(world: &mut World) {
    let Some(mut state) = world.remove_resource::<StateTrees>() else {
        return;
    };
    for (entity, mut agent) in std::mem::take(&mut state.agents) {
        // A failing native factory cannot matter while stopping.
        let _ = drive(world, &state, entity, &mut agent, None);
    }
    world.insert_resource(state);
}

fn with_state(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut StateTrees) -> Result<()>,
) -> Result<()> {
    let Some(mut state) = world.remove_resource::<StateTrees>() else {
        return Ok(());
    };
    let result = run(world, &mut state);
    world.insert_resource(state);
    result
}

fn sync(world: &mut World, state: &mut StateTrees) -> Result<()> {
    let configs: BTreeMap<Entity, StateTreeAgent> = world
        .iter_entities()
        .filter_map(|e| e.get::<StateTreeAgent>().cloned().map(|c| (e.id(), c)))
        .collect();
    if configs.len() > MAX_AGENTS {
        return Err(invalid(
            "state_tree.capacity",
            format!("more than {MAX_AGENTS} state tree agents"),
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

fn build_agent(world: &World, state: &mut StateTrees, config: StateTreeAgent) -> Result<Agent> {
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
        for name in tree.native_task_names() {
            if !state.tasks.contains(name) {
                return Err(invalid(
                    "state_tree.native.unregistered",
                    format!(
                        "graph {} uses native task {name:?}; register it with register_state_tree_task before start",
                        config.graph
                    ),
                ));
            }
        }
        for name in tree.native_evaluator_names() {
            if !state.evaluators.contains(name) {
                return Err(invalid(
                    "state_tree.native.unregistered",
                    format!(
                        "graph {} uses native evaluator {name:?}; register it with register_state_tree_evaluator before start",
                        config.graph
                    ),
                ));
            }
        }
        Some(
            StateTreeInstance::new(tree, &config.blackboard, config.seed)
                .map_err(|e| invalid("state_tree.blackboard.invalid", e.to_string()))?,
        )
    } else {
        None
    };
    Ok(Agent {
        config,
        instance,
        tasks: NativeTaskInstances::default(),
        evaluators: NativeTaskInstances::default(),
        motion: Motion::default(),
        accumulated: 0.0,
    })
}

/// Stops a running tree (agent removed, disabled or rebuilt) and forgets it.
fn stop_agent(
    world: &mut World,
    state: &mut StateTrees,
    entity: Entity,
    budget: &mut usize,
    queue_full: &mut bool,
) -> Result<()> {
    let Some(mut agent) = state.agents.remove(&entity) else {
        return Ok(());
    };
    let output = drive(world, state, entity, &mut agent, None)?;
    if world.get_entity(entity).is_ok() {
        emit(world, state, entity, &agent, output, budget, queue_full)?;
    }
    Ok(())
}

/// Ticks (`Some(dt)`) or stops (`None`) one agent's tree.
fn drive(
    world: &mut World,
    state: &StateTrees,
    entity: Entity,
    agent: &mut Agent,
    dt: Option<(f32, &[String])>,
) -> Result<Output> {
    let Agent {
        instance,
        tasks,
        evaluators,
        motion,
        config,
        ..
    } = agent;
    let Some(instance) = instance.as_mut() else {
        return Ok(Output::default());
    };
    let mut host = StateWorldHost {
        base: WorldHost {
            world,
            entity,
            registry: &state.tasks,
            natives: tasks,
            motion,
            navmesh: config.navmesh.as_deref(),
            events: Vec::new(),
            dropped: 0,
        },
        evaluators: &state.evaluators,
        evaluator_natives: evaluators,
        time: state.time,
    };
    let report = match dt {
        Some((dt, events)) => {
            for name in events {
                instance.send_event(name);
            }
            instance.tick(dt, &mut host)
        }
        None => {
            let exited = instance.stop(&mut host);
            TickReport {
                changes: exited.into_iter().map(StateChange::Exited).collect(),
                ..TickReport::default()
            }
        }
    };
    let (events, dropped) = (host.base.events, host.base.dropped);
    if let Some(message) = tasks
        .take_errors()
        .into_iter()
        .chain(evaluators.take_errors())
        .next()
    {
        return Err(invalid("state_tree.native.failed", message));
    }
    Ok(Output {
        report,
        events,
        dropped,
    })
}

/// Events delivered this tick, by target entity (names only, in delivery order).
fn targeted_events(world: &World) -> BTreeMap<Entity, Vec<String>> {
    let mut events: BTreeMap<Entity, Vec<String>> = BTreeMap::new();
    if let Some(delivered) = world.get_resource::<DeliveredEvents>() {
        for event in &delivered.0 {
            if let Some(target) = event.target {
                events.entry(target).or_default().push(event.name.clone());
            }
        }
    }
    events
}

fn step_inner(world: &mut World, state: &mut StateTrees, dt: f32) -> Result<()> {
    state.time += f64::from(dt);
    sync(world, state)?;
    if state.agents.is_empty() {
        return Ok(());
    }
    let targeted = targeted_events(world);
    let mut budget = MAX_EVENTS_PER_TICK;
    let mut queue_full = false;
    let entities: Vec<Entity> = state.agents.keys().copied().collect();
    for entity in entities {
        let Some(mut agent) = state.agents.remove(&entity) else {
            continue;
        };
        let events = targeted.get(&entity).map_or(&[][..], Vec::as_slice);
        for name in events {
            agent.motion.apply(name);
        }
        let result = tick_agent(world, state, entity, &mut agent, dt, events);
        let result = result.and_then(|output| match output {
            Some(output) => emit(
                world,
                state,
                entity,
                &agent,
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
    state: &StateTrees,
    entity: Entity,
    agent: &mut Agent,
    dt: f32,
    events: &[String],
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
    drive(world, state, entity, agent, Some((elapsed, events))).map(Some)
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Success => "success",
        Status::Failure => "failure",
        Status::Running => "running",
    }
}

/// Turns one drive's output into bounded events for the agent's entity: state
/// changes first (in order, at most [`MAX_STATE_EVENTS_PER_AGENT`]), then the
/// events `emit_event` tasks produced, then `state_tree.finished`.
fn emit(
    world: &mut World,
    state: &mut StateTrees,
    entity: Entity,
    agent: &Agent,
    output: Output,
    budget: &mut usize,
    queue_full: &mut bool,
) -> Result<()> {
    let graph = &agent.config.graph;
    let event = |name: &str, payload: serde_json::Value| crate::EngineEvent {
        name: name.to_owned(),
        source: None,
        target: Some(entity),
        payload,
    };
    let mut dropped = output.dropped
        + output
            .report
            .changes
            .len()
            .saturating_sub(MAX_STATE_EVENTS_PER_AGENT);
    let mut events: Vec<crate::EngineEvent> = output
        .report
        .changes
        .iter()
        .take(MAX_STATE_EVENTS_PER_AGENT)
        .map(|change| match change {
            StateChange::Entered(id) => {
                event(EVENT_STATE_ENTERED, json!({"graph": graph, "state": id}))
            }
            StateChange::Exited(id) => {
                event(EVENT_STATE_EXITED, json!({"graph": graph, "state": id}))
            }
        })
        .collect();
    events.extend(
        output
            .events
            .into_iter()
            .map(|(name, payload)| event(&name, payload)),
    );
    if let Some(status) = output.report.finished {
        let ticks = agent.instance.as_ref().map_or(0, StateTreeInstance::ticks);
        events.push(event(
            EVENT_FINISHED,
            json!({"graph": graph, "status": status_name(status), "ticks": ticks}),
        ));
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
    /// `None` while the agent is disabled.
    pub phase: Option<Phase>,
    /// How the tree ended, once it has.
    pub last_finished: Option<Status>,
    /// State ids from the top-level state to the active leaf (capped).
    pub active_path: Vec<String>,
    /// The most recent fired transitions, oldest first (capped).
    pub transitions: Vec<LogRecord>,
    /// Set blackboard keys (capped; long strings cut).
    pub blackboard: BTreeMap<String, BbValue>,
}

/// The `state_trees` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct StateTreeStatus {
    /// Agents in the world, of which the first [`STATUS_MAX_AGENTS`] by authored id
    /// are listed.
    pub total: usize,
    pub agents: Vec<AgentStatus>,
    /// Events dropped by the per-tick bounds or a full event queue.
    pub dropped_events: u64,
}

/// Bounded telemetry; `None` when the world has no state tree agents.
pub fn status(world: &World) -> Option<StateTreeStatus> {
    let state = world.get_resource::<StateTrees>()?;
    if state.agents.is_empty() {
        return None;
    }
    let mut agents: Vec<AgentStatus> = state
        .agents
        .iter()
        .map(|(entity, agent)| {
            let instance = agent.instance.as_ref();
            AgentStatus {
                entity: world
                    .get::<AuthoredId>(*entity)
                    .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone()),
                graph: agent.config.graph.clone(),
                enabled: agent.config.enabled,
                ticks: instance.map_or(0, StateTreeInstance::ticks),
                phase: instance.map(StateTreeInstance::phase),
                last_finished: instance.and_then(StateTreeInstance::last_finished),
                active_path: instance
                    .map(|i| {
                        let mut path = i.active_path();
                        path.truncate(STATUS_MAX_PATH);
                        path
                    })
                    .unwrap_or_default(),
                transitions: instance
                    .map(|i| i.recent_transitions(STATUS_MAX_TRANSITIONS))
                    .unwrap_or_default(),
                blackboard: instance
                    .map(|i| i.blackboard().summary(STATUS_MAX_KEYS, STATUS_MAX_STRING))
                    .unwrap_or_default(),
            }
        })
        .collect();
    agents.sort_by(|a, b| a.entity.cmp(&b.entity));
    let total = agents.len();
    agents.truncate(STATUS_MAX_AGENTS);
    Some(StateTreeStatus {
        total,
        agents,
        dropped_events: state.dropped_events,
    })
}
