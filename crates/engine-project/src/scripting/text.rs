//! Project loading and live service for `engine.TextScript` (feature `text-scripting`).

use super::{
    builtins::{self, ScriptRun, authored_id, fault_payload},
    component::{FaultPolicy, TextScript},
    runtime::{
        EVENT_FAULTED, FaultStatus, InstanceStatus, MAX_EVENTS_PER_INSTANCE, MAX_INSTANCES,
        WorldHost,
    },
};
use crate::{EngineEvent, Error, Project, Result, runtime::ProjectResource};
use bevy_ecs::prelude::*;
use engine_ai::SplitMix64;
use engine_script::{
    Budget, Capabilities, Fault, HostRegistry, ScriptInstance, Trigger,
    text::{HostBinding, MAX_SOURCE_BYTES, TextInstance, TextProgram},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Read and structurally compile a `.rhai` asset. No script code runs here.
pub(super) fn compile_text_program(project: &Project, id: &str) -> Result<Arc<TextProgram>> {
    project.require_asset(id, "text_script")?;
    let path = project.resolve_asset(id)?;
    let bytes = crate::schema::read_bounded(&path, MAX_SOURCE_BYTES as u64)?;
    TextProgram::compile(&bytes)
        .map(Arc::new)
        .map_err(|error| Error::new(error.code, format!("{}: {}", path.display(), error.message)))
}

struct FaultRecord {
    code: &'static str,
    site: Option<String>,
    message: String,
    tick: u64,
}

struct Running {
    config: TextScript,
    caps: Capabilities,
    instance: Option<TextInstance>,
    rng: SplitMix64,
    started: bool,
    fault: Option<FaultRecord>,
    ticks: u64,
    last_fuel_used: u64,
}

/// Text-side runtime data, owned by the existing ScriptState resource.
pub(super) struct TextState {
    programs: BTreeMap<String, Arc<TextProgram>>,
    instances: BTreeMap<(String, Entity), Running>,
    reload: BTreeMap<String, Arc<TextProgram>>,
    faults: u64,
    skipped: u64,
}

impl TextState {
    pub(super) fn new() -> Self {
        Self {
            programs: BTreeMap::new(),
            instances: BTreeMap::new(),
            reload: BTreeMap::new(),
            faults: 0,
            skipped: 0,
        }
    }

    fn bindings(registry: &HostRegistry<World>) -> Vec<HostBinding> {
        registry
            .names()
            .filter_map(|name| {
                registry.resolve(name).map(|(id, sig)| HostBinding {
                    name: name.to_owned(),
                    id,
                    sig: sig.clone(),
                })
            })
            .collect()
    }

    fn program_for(&mut self, world: &World, id: &str) -> Result<Arc<TextProgram>> {
        if let Some(program) = self.programs.get(id) {
            return Ok(Arc::clone(program));
        }
        let project = &world.resource::<ProjectResource>().0;
        let program = compile_text_program(project, id)?;
        self.programs.insert(id.to_owned(), Arc::clone(&program));
        Ok(program)
    }

    fn make_running(
        config: TextScript,
        program: Option<Arc<TextProgram>>,
        bindings: &[HostBinding],
    ) -> Result<Running> {
        config.check_shape()?;
        let instance = if config.enabled {
            let program = program
                .ok_or_else(|| Error::new("script.text.missing", "no compiled text program"))?;
            Some(
                TextInstance::new(program, bindings.to_vec())
                    .map_err(|error| Error::new(error.code, error.message))?,
            )
        } else {
            None
        };
        Ok(Running {
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

    /// Align instances with live components; validate replacements before stopping old instances.
    pub(super) fn sync(
        &mut self,
        world: &mut World,
        registry: &HostRegistry<World>,
        graph_count: usize,
        fuel: &mut u64,
    ) -> Result<()> {
        let configs: BTreeMap<(String, Entity), TextScript> = world
            .iter_entities()
            .filter_map(|entity| {
                entity
                    .get::<TextScript>()
                    .cloned()
                    .map(|config| ((authored_id(world, entity.id()), entity.id()), config))
            })
            .collect();
        if configs.len() + graph_count > MAX_INSTANCES {
            return Err(Error::new(
                "script.capacity",
                format!("more than {MAX_INSTANCES} script instances"),
            ));
        }
        let bindings = Self::bindings(registry);
        let mut replacements = Vec::new();
        for (key, config) in &configs {
            if self
                .instances
                .get(key)
                .is_some_and(|old| old.config == *config)
            {
                continue;
            }
            let program = if config.enabled {
                Some(match self.reload.get(&config.script) {
                    Some(candidate) => Arc::clone(candidate),
                    None => self.program_for(world, &config.script)?,
                })
            } else {
                None
            };
            replacements.push((
                key.clone(),
                Self::make_running(config.clone(), program, &bindings)?,
            ));
        }
        let removed: Vec<_> = self
            .instances
            .keys()
            .filter(|key| !configs.contains_key(*key))
            .cloned()
            .collect();
        for key in removed {
            if let Some(mut old) = self.instances.remove(&key)
                && world.get_entity(key.1).is_ok()
            {
                stop(world, registry, key.1, &mut old, fuel);
            }
        }
        for (key, fresh) in replacements {
            if let Some(mut old) = self.instances.remove(&key) {
                stop(world, registry, key.1, &mut old, fuel);
            }
            self.instances.insert(key, fresh);
        }
        let used: BTreeSet<_> = self
            .instances
            .values()
            .map(|running| running.config.script.as_str())
            .collect();
        self.programs.retain(|id, _| used.contains(id.as_str()));
        Ok(())
    }

    fn apply_reloads(
        &mut self,
        world: &mut World,
        registry: &HostRegistry<World>,
        fuel: &mut u64,
    ) -> Result<()> {
        if self.reload.is_empty() {
            return Ok(());
        }
        let bindings = Self::bindings(registry);
        let mut replacements = Vec::new();
        for (key, running) in &self.instances {
            if let Some(program) = self.reload.get(&running.config.script) {
                replacements.push((
                    key.clone(),
                    Self::make_running(
                        running.config.clone(),
                        Some(Arc::clone(program)),
                        &bindings,
                    )?,
                ));
            }
        }
        for (key, fresh) in replacements {
            if let Some(mut old) = self.instances.remove(&key) {
                stop(world, registry, key.1, &mut old, fuel);
            }
            self.instances.insert(key, fresh);
        }
        self.programs.append(&mut self.reload);
        Ok(())
    }

    /// Stage a compiled candidate; parse/shape failures preserve the last good program.
    pub(super) fn reload(
        &mut self,
        world: &World,
        registry: &HostRegistry<World>,
        id: &str,
    ) -> Result<()> {
        let project = &world.resource::<ProjectResource>().0;
        let candidate = compile_text_program(project, id)?;
        let bindings = Self::bindings(registry);
        for running in self
            .instances
            .values()
            .filter(|running| running.config.script == id)
        {
            Self::make_running(
                running.config.clone(),
                Some(Arc::clone(&candidate)),
                &bindings,
            )?;
        }
        self.reload.insert(id.to_owned(), candidate);
        Ok(())
    }

    // Mirrors the graph service's `tick_instance`: the per-tick context is passed
    // through as plain values instead of a one-use struct.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn step(
        &mut self,
        world: &mut World,
        registry: &HostRegistry<World>,
        graph_count: usize,
        dt: f32,
        time: f64,
        delivered: &[EngineEvent],
        fuel: &mut u64,
        tick: u64,
    ) -> Result<()> {
        self.sync(world, registry, graph_count, fuel)?;
        self.apply_reloads(world, registry, fuel)?;
        let keys: Vec<_> = self.instances.keys().cloned().collect();
        for key in keys {
            let Some(mut running) = self.instances.remove(&key) else {
                continue;
            };
            let result = self.tick_one(
                world,
                registry,
                key.1,
                &mut running,
                dt,
                time,
                delivered,
                fuel,
                tick,
            );
            self.instances.insert(key, running);
            result?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn tick_one(
        &mut self,
        world: &mut World,
        registry: &HostRegistry<World>,
        entity: Entity,
        running: &mut Running,
        dt: f32,
        time: f64,
        delivered: &[EngineEvent],
        fuel: &mut u64,
        tick: u64,
    ) -> Result<()> {
        let Some(mut instance) = running.instance.take() else {
            return Ok(());
        };
        if *fuel == 0 {
            self.skipped += 1;
            running.instance = Some(instance);
            return Ok(());
        }
        let mut budget = Budget::with_fuel(running.config.fuel_per_tick);
        budget.fuel = budget.fuel.min(*fuel);
        let initial_fuel = budget.fuel;
        running.ticks += 1;
        world.resource_mut::<ScriptRun>().instance_emits = 0;
        let subscriptions: BTreeSet<_> = instance
            .subscriptions()
            .iter()
            .map(String::as_str)
            .collect();
        let mut sequence = Vec::new();
        if !running.started {
            running.started = true;
            sequence.push(Trigger::Start);
        }
        sequence.extend(
            delivered
                .iter()
                .filter(|event| {
                    (event.target == Some(entity) || event.target.is_none())
                        && subscriptions.contains(event.name.as_str())
                })
                .take(MAX_EVENTS_PER_INSTANCE)
                .map(|event| Trigger::Event {
                    name: &event.name,
                    payload: &event.payload,
                    source: event
                        .source
                        .map(|entity| engine_script::EntityRef(entity.to_bits())),
                }),
        );
        sequence.push(Trigger::Update { dt });
        let mut fault = None;
        let mut outcome = Ok(());
        for trigger in sequence {
            begin(world, running, time);
            let result = {
                let mut host = WorldHost {
                    world,
                    registry,
                    caps: &running.caps,
                    entity,
                };
                instance.handle(&trigger, &mut host, &mut budget)
            };
            if let Err(error) = end(world, running, result.is_ok()) {
                outcome = Err(error);
                break;
            }
            if let Err(error) = result {
                fault = Some(error);
                break;
            }
        }
        let used = initial_fuel.saturating_sub(budget.fuel);
        running.last_fuel_used = used;
        *fuel = fuel.saturating_sub(used);
        if let Some(fault) = fault {
            self.record_fault(world, entity, running, &fault, tick)?;
        } else {
            running.instance = Some(instance);
        }
        outcome
    }

    fn record_fault(
        &mut self,
        world: &mut World,
        entity: Entity,
        running: &mut Running,
        fault: &Fault,
        tick: u64,
    ) -> Result<()> {
        self.faults += 1;
        let site = fault.site.as_ref().map(ToString::to_string);
        let message = fault.trap.message();
        running.fault = Some(FaultRecord {
            code: fault.code(),
            site: site.clone(),
            message: message.clone(),
            tick,
        });
        let id = authored_id(world, entity);
        if let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>()
            && log.0.len() < builtins::MAX_RUNTIME_LOG
        {
            log.0
                .push(format!("script: {id} faulted: {}", fault.code()));
        }
        let _ = crate::emit_events(
            world,
            vec![EngineEvent {
                name: EVENT_FAULTED.to_owned(),
                source: None,
                target: Some(entity),
                payload: fault_payload(&running.config.script, fault.code(), site, &message),
            }],
        );
        match running.config.fault_policy {
            FaultPolicy::Disable => Ok(()),
            FaultPolicy::StopRuntime => Err(Error::new(
                fault.code(),
                format!("script {} faulted: {fault}", running.config.script),
            )),
        }
    }

    pub(super) fn shutdown(
        &mut self,
        world: &mut World,
        registry: &HostRegistry<World>,
        fuel: &mut u64,
    ) {
        for ((_, entity), mut running) in std::mem::take(&mut self.instances) {
            if world.get_entity(entity).is_ok() {
                stop(world, registry, entity, &mut running, fuel);
            }
        }
    }

    pub(super) fn len(&self) -> usize {
        self.instances.len()
    }
    pub(super) fn faults(&self) -> u64 {
        self.faults
    }
    pub(super) fn skipped(&self) -> u64 {
        self.skipped
    }
    pub(super) fn status(&self) -> Vec<InstanceStatus> {
        self.instances
            .iter()
            .map(|((id, _), running)| {
                let debug = running.instance.as_ref().map(ScriptInstance::debug);
                InstanceStatus {
                    entity: id.clone(),
                    graph: running.config.script.clone(),
                    enabled: running.config.enabled,
                    ticks: running.ticks,
                    fault: running.fault.as_ref().map(|fault| FaultStatus {
                        code: fault.code.to_owned(),
                        site: fault.site.clone(),
                        message: fault.message.clone(),
                        tick: fault.tick,
                    }),
                    fuel_used: running.last_fuel_used,
                    state_bytes: debug.as_ref().map_or(0, |debug| debug.state_bytes),
                    pending: debug.as_ref().map_or(0, |debug| debug.pending),
                    variables: BTreeMap::new(),
                }
            })
            .collect()
    }
}

fn begin(world: &mut World, running: &mut Running, time: f64) {
    let mut run = world.resource_mut::<ScriptRun>();
    run.clear_run();
    run.rng = std::mem::replace(&mut running.rng, SplitMix64::new(0));
    run.time = time;
}

fn end(world: &mut World, running: &mut Running, ok: bool) -> Result<()> {
    running.rng = std::mem::replace(
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

fn stop(
    world: &mut World,
    registry: &HostRegistry<World>,
    entity: Entity,
    running: &mut Running,
    fuel: &mut u64,
) {
    let Some(mut instance) = running.instance.take() else {
        return;
    };
    if !running.started || *fuel == 0 {
        return;
    }
    let mut budget = Budget::with_fuel(running.config.fuel_per_tick);
    budget.fuel = budget.fuel.min(*fuel);
    let initial_fuel = budget.fuel;
    begin(world, running, 0.0);
    let result = {
        let mut host = WorldHost {
            world,
            registry,
            caps: &running.caps,
            entity,
        };
        instance.handle(&Trigger::Stop, &mut host, &mut budget)
    };
    *fuel = fuel.saturating_sub(initial_fuel.saturating_sub(budget.fuel));
    let _ = end(world, running, result.is_ok());
}
