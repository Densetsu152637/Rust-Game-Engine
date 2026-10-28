//! The script runtime service: one resource owning every `engine.ScriptGraph`
//! instance, stepped immediately before the AI services.
use super::{
    builtins::{self, ScriptRun, authored_id, fault_payload},
    component::{FaultPolicy, ScriptGraph},
};
use crate::{
    EngineEvent, Error, Project, Result, events::DeliveredEvents, runtime::ProjectResource,
};
use bevy_ecs::prelude::*;
use engine_ai::SplitMix64;
use engine_script::{
    Budget, Capabilities, EntityRef, Fault, HostRegistry, NativeCx, ScriptError, ScriptHost,
    ScriptInstance, Trap, Trigger, Value, codes,
    graph::{KIND, Program},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Most instances one runtime supports.
pub const MAX_INSTANCES: usize = 1024;
/// Fuel all instances together may spend per tick.
pub const GLOBAL_FUEL_PER_TICK: u64 = 2_000_000;
/// Most delivered events one instance handles per tick.
pub const MAX_EVENTS_PER_INSTANCE: usize = 64;
/// Instances listed in telemetry.
pub const STATUS_MAX_INSTANCES: usize = 32;
const STATUS_MAX_VARIABLES: usize = 16;

/// Name of the event emitted when an instance faults.
pub const EVENT_FAULTED: &str = "script.faulted";

struct FaultRecord {
    code: &'static str,
    site: Option<String>,
    message: String,
    tick: u64,
}

struct Instance {
    config: ScriptGraph,
    caps: Capabilities,
    /// `None` while disabled or after a fault.
    instance: Option<Box<dyn ScriptInstance>>,
    rng: SplitMix64,
    started: bool,
    fault: Option<FaultRecord>,
    ticks: u64,
    last_fuel_used: u64,
}

/// Every instance, the compiled programs and the host function registry.
#[derive(Resource)]
pub struct ScriptState {
    pub(super) registry: HostRegistry<World>,
    programs: BTreeMap<String, Arc<Program>>,
    instances: BTreeMap<(String, Entity), Instance>,
    reload: BTreeMap<String, Arc<Program>>,
    ticks: u64,
    faults: u64,
    skipped: u64,
}

impl ScriptState {
    pub(super) fn new() -> Result<Self> {
        let mut registry = HostRegistry::new();
        builtins::register_builtins(&mut registry)?;
        Ok(Self {
            registry,
            programs: BTreeMap::new(),
            instances: BTreeMap::new(),
            reload: BTreeMap::new(),
            ticks: 0,
            faults: 0,
            skipped: 0,
        })
    }
}

/// The host a graph instance runs against: the live world plus the instance's
/// capabilities. Effects of built-ins are staged (see [`builtins`]).
struct WorldHost<'a> {
    world: &'a mut World,
    registry: &'a HostRegistry<World>,
    caps: &'a Capabilities,
    entity: Entity,
}

fn trap_of(error: ScriptError) -> Trap {
    match error.code {
        codes::CAPABILITY_DENIED => Trap::Capability(error.message),
        codes::EVENT_LIMIT => Trap::EventLimit,
        codes::VALUE_NONFINITE => Trap::NonFinite,
        _ => Trap::Host(error),
    }
}

impl ScriptHost for WorldHost<'_> {
    fn call(
        &mut self,
        function: engine_script::FnId,
        args: &[Value],
    ) -> std::result::Result<Value, Trap> {
        if let Some(sig) = self.registry.sig(function)
            && sig.effects.contains(engine_script::Effects::SPAWN)
            && self.caps.spawn == 0
        {
            return Err(Trap::Capability("spawn is not granted".to_owned()));
        }
        let subject = self.subject();
        let mut cx = NativeCx {
            world: &mut *self.world,
            subject,
            caps: self.caps,
        };
        self.registry
            .invoke(function, &mut cx, args)
            .map_err(trap_of)
    }
    fn emit(
        &mut self,
        name: &str,
        payload: Value,
        target: Option<EntityRef>,
    ) -> std::result::Result<(), Trap> {
        let target = target.and_then(|t| Entity::try_from_bits(t.0));
        builtins::stage_emit(
            self.world,
            self.caps,
            self.entity,
            name,
            payload.to_json(),
            target,
        )
        .map_err(trap_of)
    }
    fn log(&mut self, message: &str) {
        builtins::stage_log(self.world, message);
    }
    fn subject(&self) -> EntityRef {
        EntityRef(self.entity.to_bits())
    }
}

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Compiles the `script_graph` asset `id` against `signatures`.
pub(super) fn compile_program(
    project: &Project,
    signatures: &dyn engine_script::Signatures,
    id: &str,
) -> Result<Arc<Program>> {
    let document = crate::graphs::load_graph(project, id)?;
    if document.kind != KIND {
        return Err(invalid(
            "script.graph.kind",
            format!(
                "graph asset {id} is a {} graph, not a script_graph",
                document.kind
            ),
        ));
    }
    engine_script::graph::compile(&document, signatures)
        .map(Arc::new)
        .map_err(|d| crate::graphs::into_error(d, &project.assets[id].path))
}

/// Brings the service in line with the world's components and checks that every
/// declared function has a matching native implementation.
pub fn prepare(world: &mut World) -> Result<()> {
    with_state(world, |world, state| {
        verify_functions(world, state)?;
        let mut fuel = GLOBAL_FUEL_PER_TICK;
        sync(world, state, &mut fuel)
    })
}

/// Runs every instance once. Called once per runtime tick, before `ai::step`.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    with_state(world, |world, state| step_inner(world, state, dt))
}

/// Stops every instance (running `on_stop`). Called when the runtime stops.
pub fn shutdown(world: &mut World) {
    let Some(mut state) = world.remove_resource::<ScriptState>() else {
        return;
    };
    let mut fuel = GLOBAL_FUEL_PER_TICK;
    for ((_, entity), mut instance) in std::mem::take(&mut state.instances) {
        let _ = stop_instance(world, &state.registry, entity, &mut instance, &mut fuel);
    }
    world.insert_resource(state);
}

fn with_state(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut ScriptState) -> Result<()>,
) -> Result<()> {
    let Some(mut state) = world.remove_resource::<ScriptState>() else {
        return Ok(());
    };
    let result = run(world, &mut state);
    world.insert_resource(state);
    result
}

fn verify_functions(world: &World, state: &ScriptState) -> Result<()> {
    let Some(project) = world.get_resource::<ProjectResource>() else {
        return Ok(());
    };
    for declaration in &project.0.script_functions {
        match state.registry.resolve(&declaration.name) {
            None => {
                return Err(invalid(
                    codes::FUNCTION_UNREGISTERED,
                    format!(
                        "script function {} is declared in scriptFunctions but not registered; call register_script_fn before start",
                        declaration.name
                    ),
                ));
            }
            Some((_, sig)) if *sig != declaration.sig() => {
                return Err(invalid(
                    codes::FUNCTION_SIGNATURE,
                    format!(
                        "script function {} is registered with a signature that differs from its declaration",
                        declaration.name
                    ),
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn authored_key(world: &World, entity: Entity) -> (String, Entity) {
    (authored_id(world, entity), entity)
}

fn sync(world: &mut World, state: &mut ScriptState, fuel: &mut u64) -> Result<()> {
    let configs: BTreeMap<(String, Entity), ScriptGraph> = world
        .iter_entities()
        .filter_map(|e| {
            e.get::<ScriptGraph>()
                .cloned()
                .map(|c| (authored_key(world, e.id()), c))
        })
        .collect();
    if configs.len() > MAX_INSTANCES {
        return Err(invalid(
            "script.capacity",
            format!("more than {MAX_INSTANCES} script instances"),
        ));
    }
    // A component may change after reload_asset checked its overrides. Recheck
    // every pending candidate against the current world before any Stop handler
    // runs. An incompatible candidate is cancelled; the old program remains.
    let mut rejected = Vec::new();
    for (id, program) in &state.reload {
        for config in configs
            .values()
            .filter(|config| config.graph == id.as_str() && config.enabled)
        {
            if let Err(error) = program.instantiate(&config.variables) {
                rejected.push((id.clone(), error));
                break;
            }
        }
    }
    for (id, error) in rejected {
        state.reload.remove(&id);
        if let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>() {
            if log.0.len() < builtins::MAX_RUNTIME_LOG {
                log.0.push(format!(
                    "script: pending reload {id} rejected after component change: {error}"
                ));
            }
        }
    }
    // Validate every new/changed instance before stopping any last-good one.
    let mut replacements = Vec::new();
    for (key, config) in &configs {
        if state
            .instances
            .get(key)
            .is_some_and(|existing| existing.config == *config)
        {
            continue;
        }
        let instance = match state.reload.get(&config.graph) {
            Some(program) => build_instance_with_program(config.clone(), Some(program))?,
            None => build_instance(world, state, config.clone())?,
        };
        replacements.push((key.clone(), instance));
    }
    let removed: Vec<(String, Entity)> = state
        .instances
        .keys()
        .filter(|k| !configs.contains_key(*k))
        .cloned()
        .collect();
    for key in removed {
        if let Some(mut instance) = state.instances.remove(&key) {
            // No handler runs for a despawned entity.
            if world.get_entity(key.1).is_ok() {
                let _ = stop_instance(world, &state.registry, key.1, &mut instance, fuel);
            }
        }
    }
    for (key, instance) in replacements {
        if let Some(mut old) = state.instances.remove(&key) {
            let _ = stop_instance(world, &state.registry, key.1, &mut old, fuel);
        }
        state.instances.insert(key, instance);
    }
    prune(state);
    Ok(())
}

fn prune(state: &mut ScriptState) {
    let used: BTreeSet<&str> = state
        .instances
        .values()
        .map(|i| i.config.graph.as_str())
        .collect();
    state.programs.retain(|id, _| used.contains(id.as_str()));
}

fn program_for(world: &World, state: &mut ScriptState, id: &str) -> Result<Arc<Program>> {
    if let Some(program) = state.programs.get(id) {
        return Ok(Arc::clone(program));
    }
    let project = &world.resource::<ProjectResource>().0;
    let program = compile_program(project, &state.registry, id)?;
    state.programs.insert(id.to_owned(), Arc::clone(&program));
    Ok(program)
}

fn build_instance(world: &World, state: &mut ScriptState, config: ScriptGraph) -> Result<Instance> {
    config.check_shape()?;
    let program = if config.enabled {
        Some(program_for(world, state, &config.graph)?)
    } else {
        None
    };
    build_instance_with_program(config, program.as_ref())
}

fn build_instance_with_program(
    config: ScriptGraph,
    program: Option<&Arc<Program>>,
) -> Result<Instance> {
    config.check_shape()?;
    let instance: Option<Box<dyn ScriptInstance>> = if config.enabled {
        let program =
            program.ok_or_else(|| invalid("script.graph.missing", "no compiled program"))?;
        Some(Box::new(
            program
                .instantiate(&config.variables)
                .map_err(|e| invalid(e.code, e.message))?,
        ))
    } else {
        None
    };
    Ok(Instance {
        caps: config.capabilities.to_runtime(),
        rng: SplitMix64::new(config.seed),
        config,
        instance,
        started: false,
        fault: None,
        ticks: 0,
        last_fuel_used: 0,
    })
}

/// Runs `on_stop` for a live instance and discards it. Faults while stopping are
/// logged, never raised.
fn stop_instance(
    world: &mut World,
    registry: &HostRegistry<World>,
    entity: Entity,
    instance: &mut Instance,
    global_fuel: &mut u64,
) -> Result<()> {
    let Some(mut running) = instance.instance.take() else {
        return Ok(());
    };
    if !instance.started {
        return Ok(());
    }
    if *global_fuel == 0 {
        if let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>() {
            if log.0.len() < builtins::MAX_RUNTIME_LOG {
                log.0
                    .push("script: on_stop skipped (global fuel exhausted)".into());
            }
        }
        return Ok(());
    }
    let mut budget = Budget::with_fuel(instance.config.fuel_per_tick);
    budget.fuel = budget.fuel.min(*global_fuel);
    let start_fuel = budget.fuel;
    begin_run(world, instance, 0.0);
    let result = {
        let mut host = WorldHost {
            world,
            registry,
            caps: &instance.caps,
            entity,
        };
        running.handle(&Trigger::Stop, &mut host, &mut budget)
    };
    *global_fuel = global_fuel.saturating_sub(start_fuel.saturating_sub(budget.fuel));
    end_run(world, instance, result.is_ok())?;
    if let Err(fault) = result
        && let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>()
    {
        log.0.push(format!("script: on_stop faulted: {fault}"));
    }
    Ok(())
}

fn begin_run(world: &mut World, instance: &mut Instance, time: f64) {
    let mut run = world.resource_mut::<ScriptRun>();
    run.clear_run();
    run.rng = std::mem::replace(&mut instance.rng, SplitMix64::new(0));
    run.time = time;
}

/// Commits or discards the run and takes the instance's RNG state back.
fn end_run(world: &mut World, instance: &mut Instance, ok: bool) -> Result<()> {
    instance.rng = std::mem::replace(
        &mut world.resource_mut::<ScriptRun>().rng,
        SplitMix64::new(0),
    );
    if ok {
        let result = builtins::commit(world);
        if result.is_err() {
            world.resource_mut::<ScriptRun>().clear_run();
        }
        result
    } else {
        world.resource_mut::<ScriptRun>().clear_run();
        Ok(())
    }
}

fn apply_reloads(world: &mut World, state: &mut ScriptState, fuel: &mut u64) -> Result<()> {
    if state.reload.is_empty() {
        return Ok(());
    }
    let configs: Vec<((String, Entity), ScriptGraph)> = state
        .instances
        .iter()
        .filter(|(_, i)| state.reload.contains_key(&i.config.graph))
        .map(|(key, instance)| (key.clone(), instance.config.clone()))
        .collect();
    let mut replacements = Vec::with_capacity(configs.len());
    for (key, config) in configs {
        let program = &state.reload[&config.graph];
        replacements.push((key, build_instance_with_program(config, Some(program))?));
    }
    for (key, fresh) in replacements {
        let Some(mut old) = state.instances.remove(&key) else {
            continue;
        };
        let _ = stop_instance(world, &state.registry, key.1, &mut old, fuel);
        state.instances.insert(key, fresh);
    }
    for (id, program) in std::mem::take(&mut state.reload) {
        state.programs.insert(id, program);
    }
    Ok(())
}

/// Compiles `id` as a candidate and, on success, makes it the program of every
/// affected instance at the next tick boundary. On failure the last good program
/// stays and the error is returned.
pub fn reload_asset(world: &mut World, id: &str) -> Result<()> {
    let Some(mut state) = world.remove_resource::<ScriptState>() else {
        return Err(invalid(
            "script.unavailable",
            "the runtime has no script service",
        ));
    };
    let result = (|| {
        let project = Arc::clone(&world.resource::<ProjectResource>().0);
        let program = compile_program(&project, &state.registry, id)?;
        for instance in state.instances.values().filter(|i| i.config.graph == id) {
            instance.config.check_shape()?;
            if instance.config.enabled {
                program
                    .instantiate(&instance.config.variables)
                    .map_err(|e| invalid(e.code, e.message))?;
            }
        }
        state.reload.insert(id.to_owned(), program);
        Ok(())
    })();
    world.insert_resource(state);
    result
}

fn step_inner(world: &mut World, state: &mut ScriptState, dt: f32) -> Result<()> {
    state.ticks += 1;
    {
        let mut run = world.resource_mut::<ScriptRun>();
        run.global_emits = 0;
    }
    let mut global_fuel = GLOBAL_FUEL_PER_TICK;
    sync(world, state, &mut global_fuel)?;
    apply_reloads(world, state, &mut global_fuel)?;
    prune(state);
    if state.instances.is_empty() {
        return Ok(());
    }
    let delivered: Vec<EngineEvent> = world
        .get_resource::<DeliveredEvents>()
        .map(|d| d.0.clone())
        .unwrap_or_default();
    let time = state.ticks as f64 * f64::from(dt);
    let keys: Vec<(String, Entity)> = state.instances.keys().cloned().collect();
    for key in keys {
        let Some(mut instance) = state.instances.remove(&key) else {
            continue;
        };
        let result = tick_instance(
            world,
            state,
            key.1,
            &mut instance,
            dt,
            time,
            &delivered,
            &mut global_fuel,
        );
        state.instances.insert(key, instance);
        result?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn tick_instance(
    world: &mut World,
    state: &mut ScriptState,
    entity: Entity,
    instance: &mut Instance,
    dt: f32,
    time: f64,
    delivered: &[EngineEvent],
    global_fuel: &mut u64,
) -> Result<()> {
    let Some(mut running) = instance.instance.take() else {
        return Ok(());
    };
    if *global_fuel == 0 {
        state.skipped += 1;
        instance.instance = Some(running);
        return Ok(());
    }
    let mut budget = Budget::with_fuel(instance.config.fuel_per_tick);
    budget.fuel = budget.fuel.min(*global_fuel);
    let start_fuel = budget.fuel;
    instance.ticks += 1;
    world.resource_mut::<ScriptRun>().instance_emits = 0;

    let subscriptions: BTreeSet<String> = running.subscriptions().iter().cloned().collect();
    let mut triggers: Vec<Option<&EngineEvent>> = Vec::new();
    if !instance.started {
        instance.started = true;
        triggers.push(None);
    }
    let mine = |e: &&EngineEvent| {
        (e.target == Some(entity) || e.target.is_none()) && subscriptions.contains(&e.name)
    };
    triggers.extend(
        delivered
            .iter()
            .filter(mine)
            .take(MAX_EVENTS_PER_INSTANCE)
            .map(Some),
    );

    let mut outcome: Result<()> = Ok(());
    let mut fault: Option<Fault> = None;
    let mut sequence: Vec<Trigger<'_>> = Vec::new();
    for entry in &triggers {
        match entry {
            None => sequence.push(Trigger::Start),
            Some(event) => sequence.push(Trigger::Event {
                name: &event.name,
                payload: &event.payload,
                source: event.source.map(|e| EntityRef(e.to_bits())),
            }),
        }
    }
    sequence.push(Trigger::Update { dt });
    for trigger in &sequence {
        begin_run(world, instance, time);
        let result = {
            let mut host = WorldHost {
                world,
                registry: &state.registry,
                caps: &instance.caps,
                entity,
            };
            running.handle(trigger, &mut host, &mut budget)
        };
        let ok = result.is_ok();
        if let Err(error) = end_run(world, instance, ok) {
            outcome = Err(error);
            break;
        }
        if let Err(f) = result {
            fault = Some(f);
            break;
        }
    }
    let used = start_fuel.saturating_sub(budget.fuel);
    instance.last_fuel_used = used;
    *global_fuel = global_fuel.saturating_sub(used);
    match fault {
        None => instance.instance = Some(running),
        Some(fault) => {
            // The faulted instance is dropped: its state is undefined after a trap.
            outcome = outcome.and(record_fault(world, state, entity, instance, &fault));
        }
    }
    outcome
}

fn record_fault(
    world: &mut World,
    state: &mut ScriptState,
    entity: Entity,
    instance: &mut Instance,
    fault: &Fault,
) -> Result<()> {
    let site = fault.site.as_ref().map(ToString::to_string);
    let message = fault.trap.message();
    state.faults += 1;
    instance.fault = Some(FaultRecord {
        code: fault.code(),
        site: site.clone(),
        message: message.clone(),
        tick: state.ticks,
    });
    let id = authored_id(world, entity);
    if let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>() {
        log.0.push(format!(
            "script: {id} faulted: {} ({})",
            fault.code(),
            site.as_deref().unwrap_or("unknown site")
        ));
    }
    let event = EngineEvent {
        name: EVENT_FAULTED.to_owned(),
        source: None,
        target: Some(entity),
        payload: fault_payload(&instance.config.graph, fault.code(), site, &message),
    };
    // A full event queue must not hide the fault: it is in telemetry and the log.
    let _ = crate::emit_events(world, vec![event]);
    match instance.config.fault_policy {
        FaultPolicy::Disable => Ok(()),
        FaultPolicy::StopRuntime => Err(invalid(
            fault.code(),
            format!("script {} faulted: {fault}", instance.config.graph),
        )),
    }
}

/// Telemetry for one instance.
#[derive(Clone, Debug, Serialize)]
pub struct InstanceStatus {
    /// Authored entity id.
    pub entity: String,
    pub graph: String,
    pub enabled: bool,
    pub ticks: u64,
    /// Set once the instance trapped and was disabled.
    pub fault: Option<FaultStatus>,
    pub fuel_used: u64,
    pub state_bytes: usize,
    pub pending: usize,
    /// First variables, rendered (capped, long values cut).
    pub variables: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FaultStatus {
    pub code: String,
    pub site: Option<String>,
    pub message: String,
    pub tick: u64,
}

/// The `scripts` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct ScriptsStatus {
    /// Instances in the world, of which the first [`STATUS_MAX_INSTANCES`] by
    /// authored id are listed.
    pub total: usize,
    pub instances: Vec<InstanceStatus>,
    /// Traps since the runtime started.
    pub faults: u64,
    /// Instance ticks skipped because the global fuel budget ran out.
    pub skipped: u64,
}

/// Bounded telemetry; `None` when the world has no script instances.
pub fn status(world: &World) -> Option<ScriptsStatus> {
    let state = world.get_resource::<ScriptState>()?;
    if state.instances.is_empty() {
        return None;
    }
    let mut instances: Vec<InstanceStatus> = state
        .instances
        .iter()
        .take(STATUS_MAX_INSTANCES)
        .map(|((id, _), instance)| {
            let debug = instance.instance.as_ref().map(|i| i.debug());
            InstanceStatus {
                entity: id.clone(),
                graph: instance.config.graph.clone(),
                enabled: instance.config.enabled,
                ticks: instance.ticks,
                fault: instance.fault.as_ref().map(|f| FaultStatus {
                    code: f.code.to_owned(),
                    site: f.site.clone(),
                    message: f.message.clone(),
                    tick: f.tick,
                }),
                fuel_used: instance.last_fuel_used,
                state_bytes: debug.as_ref().map_or(0, |d| d.state_bytes),
                pending: debug.as_ref().map_or(0, |d| d.pending),
                variables: debug
                    .map(|d| d.variables.into_iter().take(STATUS_MAX_VARIABLES).collect())
                    .unwrap_or_default(),
            }
        })
        .collect();
    instances.sort_by(|a, b| a.entity.cmp(&b.entity));
    Some(ScriptsStatus {
        total: state.instances.len(),
        instances,
        faults: state.faults,
        skipped: state.skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_script::InstanceDebug;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct StopBurner(Arc<AtomicUsize>);

    impl ScriptInstance for StopBurner {
        fn handle(
            &mut self,
            trigger: &Trigger<'_>,
            _host: &mut dyn ScriptHost,
            budget: &mut Budget,
        ) -> std::result::Result<(), Fault> {
            assert!(matches!(trigger, Trigger::Stop));
            self.0.fetch_add(1, Ordering::Relaxed);
            budget
                .charge(1_000_000)
                .map_err(|trap| Fault { trap, site: None })
        }
        fn subscriptions(&self) -> &[String] {
            &[]
        }
        fn debug(&self) -> InstanceDebug {
            InstanceDebug::default()
        }
    }

    #[test]
    fn stop_handlers_share_one_global_fuel_budget() {
        let mut world = World::new();
        world.insert_resource(ScriptRun::default());
        let entity = world.spawn_empty().id();
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = HostRegistry::new();
        let mut fuel = GLOBAL_FUEL_PER_TICK;
        for _ in 0..3 {
            let mut instance = Instance {
                config: ScriptGraph {
                    fuel_per_tick: 1_000_000,
                    ..ScriptGraph::default()
                },
                caps: Capabilities::default(),
                instance: Some(Box::new(StopBurner(Arc::clone(&calls)))),
                rng: SplitMix64::new(0),
                started: true,
                fault: None,
                ticks: 0,
                last_fuel_used: 0,
            };
            stop_instance(&mut world, &registry, entity, &mut instance, &mut fuel).unwrap();
            assert!(instance.instance.is_none());
        }
        assert_eq!(fuel, 0);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
