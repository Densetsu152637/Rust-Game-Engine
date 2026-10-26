//! The built-in `engine.*` host functions and the staging they write through.
//!
//! Built-ins never change the world directly. Writes (`engine.set_position`,
//! `engine.component_set`) and emitted events are staged in [`ScriptRun`] and
//! committed by the runtime only when the whole run returns `Ok`; a trap discards
//! them. Reads see the world as of the start of the run.
use super::functions::builtin_signatures;
use crate::{
    EngineEvent, Error, Result, Transform,
    catalog::CustomValue,
    runtime::{AuthoredId, ProjectResource},
};
use bevy_ecs::prelude::*;
use engine_ai::SplitMix64;
use engine_script::{
    Capabilities, EmitScope, EntityRef, HostRegistry, NativeCx, NativeFn, ScriptError, Value,
    codes, graph::valid_event_name,
};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// Most events one instance may emit per tick.
pub const MAX_EMITS_PER_INSTANCE: u16 = 16;
/// Most events all scripts together may emit per tick.
pub const MAX_EMITS_GLOBAL: u32 = 256;
/// Most log lines one run may produce.
pub const MAX_LOG_LINES_PER_RUN: usize = 32;
/// Longest accepted log line, in bytes.
pub const MAX_LOG_BYTES: usize = 1024;

/// Components that own an entity's pose. `engine.set_position` is denied on an
/// entity carrying one (keep in step with the networking pose writers).
pub(crate) const POSE_WRITERS: &[&str] = &[
    "engine.PhysicsBody2D",
    "engine.PhysicsBody3D",
    "engine.AnimationPlayer",
    "engine.NavigationAgent",
    "engine.CrowdAgent",
];

pub(crate) enum Staged {
    SetPosition {
        entity: Entity,
        position: [f32; 3],
    },
    ComponentSet {
        entity: Entity,
        name: String,
        field: String,
        value: Json,
    },
}

/// Per-run scratch state shared with the built-ins through the world.
#[derive(Resource)]
pub(crate) struct ScriptRun {
    /// Simulated seconds since the runtime started, as `engine.time` reports.
    pub time: f64,
    pub rng: SplitMix64,
    pub staged: Vec<Staged>,
    pub events: Vec<EngineEvent>,
    pub logs: Vec<String>,
    pub dropped_logs: u64,
    /// Emits by the current instance this tick, and by all instances this tick.
    pub instance_emits: u16,
    pub global_emits: u32,
}

impl Default for ScriptRun {
    fn default() -> Self {
        Self {
            time: 0.0,
            rng: SplitMix64::new(0),
            staged: Vec::new(),
            events: Vec::new(),
            logs: Vec::new(),
            dropped_logs: 0,
            instance_emits: 0,
            global_emits: 0,
        }
    }
}

impl ScriptRun {
    /// Forgets the staged effects of a run (after commit or a trap).
    pub fn clear_run(&mut self) {
        self.staged.clear();
        self.events.clear();
        self.logs.clear();
    }
}

fn argument(message: impl Into<String>) -> ScriptError {
    ScriptError::new(codes::HOST_ARGUMENT, message)
}
fn denied(message: impl Into<String>) -> ScriptError {
    ScriptError::new(codes::CAPABILITY_DENIED, message)
}

fn entity_arg(value: &Value) -> std::result::Result<Entity, ScriptError> {
    match value {
        Value::Entity(EntityRef(bits)) => {
            Entity::try_from_bits(*bits).ok_or_else(|| argument("not an entity handle"))
        }
        _ => Err(argument("expected an entity")),
    }
}

fn live(world: &World, entity: Entity) -> std::result::Result<Entity, ScriptError> {
    if world.get_entity(entity).is_ok() {
        Ok(entity)
    } else {
        Err(argument("unknown entity"))
    }
}

fn string_arg(value: &Value) -> &str {
    match value {
        Value::Str(s) => s,
        _ => "",
    }
}

fn subject(cx: &NativeCx<'_, World>) -> std::result::Result<Entity, ScriptError> {
    Entity::try_from_bits(cx.subject.0).ok_or_else(|| argument("invalid subject"))
}

fn run(world: &mut World) -> Mut<'_, ScriptRun> {
    world.resource_mut::<ScriptRun>()
}

fn f_self(
    cx: &mut NativeCx<'_, World>,
    _args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    Ok(Value::Entity(cx.subject))
}

fn f_find_entity(
    cx: &mut NativeCx<'_, World>,
    args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    crate::services::find_entity(cx.world, string_arg(&args[0]))
        .map(|e| Value::Entity(EntityRef(e.to_bits())))
        .ok_or_else(|| argument(format!("no entity with id {:?}", string_arg(&args[0]))))
}

fn f_position(
    cx: &mut NativeCx<'_, World>,
    args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    let entity = live(cx.world, entity_arg(&args[0])?)?;
    cx.world
        .get::<Transform>(entity)
        .map(|t| Value::Vec3(t.position))
        .ok_or_else(|| argument("the entity has no transform"))
}

fn has_pose_writer(world: &World, entity: Entity) -> bool {
    crate::catalog::snapshot(world, entity)
        .map(|components| POSE_WRITERS.iter().any(|n| components.contains_key(*n)))
        .unwrap_or(true)
}

fn f_set_position(
    cx: &mut NativeCx<'_, World>,
    args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    let target = entity_arg(&args[0])?;
    let me = subject(cx)?;
    if target != me {
        return Err(denied("set_position is limited to the script's own entity"));
    }
    let Value::Vec3(position) = args[1] else {
        return Err(argument("expected a vec3"));
    };
    if has_pose_writer(cx.world, me) {
        return Err(denied(
            "the entity's pose is owned by physics, animation or navigation",
        ));
    }
    if cx.world.get::<Transform>(me).is_none() {
        return Err(argument("the entity has no transform"));
    }
    run(cx.world).staged.push(Staged::SetPosition {
        entity: me,
        position,
    });
    Ok(Value::Unit)
}

fn project(world: &World) -> std::result::Result<std::sync::Arc<crate::Project>, ScriptError> {
    world
        .get_resource::<ProjectResource>()
        .map(|p| std::sync::Arc::clone(&p.0))
        .ok_or_else(|| argument("no project"))
}

fn granted(caps: &Capabilities, name: &str, write: bool) -> bool {
    caps.write.contains(name) || (!write && caps.read.contains(name))
}

fn f_component_get(
    cx: &mut NativeCx<'_, World>,
    args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    let (name, field) = (string_arg(&args[0]), string_arg(&args[1]));
    if !granted(cx.caps, name, false) {
        return Err(denied(format!(
            "component {name:?} is not in capabilities.read"
        )));
    }
    if !project(cx.world)?.component_catalog.0.contains_key(name) {
        return Err(argument(format!("no component {name:?} in the catalog")));
    }
    let me = subject(cx)?;
    let components = crate::catalog::snapshot(cx.world, me)
        .map_err(|e| ScriptError::new(codes::HOST_FAILED, e.to_string()))?;
    let value = components
        .get(name)
        .ok_or_else(|| argument(format!("the entity has no {name}")))?;
    let found = if field.is_empty() {
        Some(&value.data)
    } else {
        value.data.get(field)
    };
    found
        .map(|v| Value::json(v.clone()))
        .ok_or_else(|| argument(format!("{name} has no field {field:?}")))
}

fn f_component_set(
    cx: &mut NativeCx<'_, World>,
    args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    let (name, field) = (string_arg(&args[0]), string_arg(&args[1]));
    let Value::Json(value) = &args[2] else {
        return Err(argument("expected json"));
    };
    if !granted(cx.caps, name, true) {
        return Err(denied(format!(
            "component {name:?} is not in capabilities.write"
        )));
    }
    if field.is_empty() {
        return Err(argument("component_set needs a field name"));
    }
    let project = project(cx.world)?;
    let descriptor = project
        .component_catalog
        .0
        .get(name)
        .ok_or_else(|| argument(format!("no component {name:?} in the catalog")))?;
    let me = subject(cx)?;
    let components = crate::catalog::snapshot(cx.world, me)
        .map_err(|e| ScriptError::new(codes::HOST_FAILED, e.to_string()))?;
    let mut data = components
        .get(name)
        .ok_or_else(|| argument(format!("the entity has no {name}")))?
        .data
        .clone();
    // Apply earlier staged writes of this component, then this one.
    let earlier: Vec<(String, Json)> = cx
        .world
        .resource::<ScriptRun>()
        .staged
        .iter()
        .filter_map(|s| match s {
            Staged::ComponentSet {
                entity,
                name: n,
                field,
                value,
            } if *entity == me && n == name => Some((field.clone(), value.clone())),
            _ => None,
        })
        .collect();
    let object = data
        .as_object_mut()
        .ok_or_else(|| argument("the component is not an object"))?;
    for (f, v) in earlier {
        object.insert(f, v);
    }
    object.insert(field.to_owned(), (**value).clone());
    project
        .component_catalog
        .validate(
            name,
            &CustomValue {
                version: descriptor.version,
                data,
            },
        )
        .map_err(|e| denied(format!("the write violates the component schema: {e}")))?;
    run(cx.world).staged.push(Staged::ComponentSet {
        entity: me,
        name: name.to_owned(),
        field: field.to_owned(),
        value: (**value).clone(),
    });
    Ok(Value::Unit)
}

/// Stages one emitted event, enforcing name, scope and the per-tick limits.
pub(crate) fn stage_emit(
    world: &mut World,
    caps: &Capabilities,
    me: Entity,
    name: &str,
    payload: Json,
    target: Option<Entity>,
) -> std::result::Result<(), ScriptError> {
    if !valid_event_name(name) {
        return Err(argument(format!("{name:?} is not a valid event name")));
    }
    if caps.emit == EmitScope::SelfOnly && target != Some(me) {
        return Err(denied(
            "capabilities.emit is self_only: events must target the script's own entity",
        ));
    }
    if let Some(target) = target {
        live(world, target)?;
    }
    let mut state = run(world);
    if state.instance_emits >= MAX_EMITS_PER_INSTANCE || state.global_emits >= MAX_EMITS_GLOBAL {
        return Err(ScriptError::new(
            codes::EVENT_LIMIT,
            "too many events emitted this tick",
        ));
    }
    state.instance_emits += 1;
    state.global_emits += 1;
    state.events.push(EngineEvent {
        name: name.to_owned(),
        source: Some(me),
        target,
        payload,
    });
    Ok(())
}

fn f_emit(cx: &mut NativeCx<'_, World>, args: &[Value]) -> std::result::Result<Value, ScriptError> {
    let me = subject(cx)?;
    let Value::Json(payload) = &args[1] else {
        return Err(argument("expected json"));
    };
    let target = match &args[2] {
        Value::Entity(EntityRef(0)) => None,
        other => Some(entity_arg(other)?),
    };
    let payload = (**payload).clone();
    stage_emit(cx.world, cx.caps, me, string_arg(&args[0]), payload, target)?;
    Ok(Value::Unit)
}

pub(crate) fn stage_log(world: &mut World, message: &str) {
    let mut state = run(world);
    if state.logs.len() >= MAX_LOG_LINES_PER_RUN {
        state.dropped_logs += 1;
        return;
    }
    let mut line = message.to_owned();
    if line.len() > MAX_LOG_BYTES {
        let mut cut = MAX_LOG_BYTES;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
    }
    state.logs.push(format!("script: {line}"));
}

fn f_log(cx: &mut NativeCx<'_, World>, args: &[Value]) -> std::result::Result<Value, ScriptError> {
    stage_log(cx.world, string_arg(&args[0]));
    Ok(Value::Unit)
}

fn f_random_int(
    cx: &mut NativeCx<'_, World>,
    args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    let (Value::Int(min), Value::Int(max)) = (&args[0], &args[1]) else {
        return Err(argument("expected two ints"));
    };
    if min > max {
        return Err(argument("random_int needs min <= max"));
    }
    let size = (i128::from(*max) - i128::from(*min) + 1) as u128;
    let draw = u128::from(run(cx.world).rng.next_u64());
    let offset = ((draw * size) >> 64) as i128;
    Ok(Value::Int((i128::from(*min) + offset) as i64))
}

fn f_random_float(
    cx: &mut NativeCx<'_, World>,
    _args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    let draw = run(cx.world).rng.next_u64() >> 11;
    Ok(Value::Float(draw as f64 / (1u64 << 53) as f64))
}

fn f_time(
    cx: &mut NativeCx<'_, World>,
    _args: &[Value],
) -> std::result::Result<Value, ScriptError> {
    Ok(Value::Float(cx.world.resource::<ScriptRun>().time))
}

/// Registers every built-in under its reserved name.
pub(crate) fn register_builtins(registry: &mut HostRegistry<World>) -> Result<()> {
    let table: [(&str, NativeFn<World>); 11] = [
        ("engine.self", f_self),
        ("engine.find_entity", f_find_entity),
        ("engine.position", f_position),
        ("engine.set_position", f_set_position),
        ("engine.component_get", f_component_get),
        ("engine.component_set", f_component_set),
        ("engine.emit", f_emit),
        ("engine.log", f_log),
        ("engine.random_int", f_random_int),
        ("engine.random_float", f_random_float),
        ("engine.time", f_time),
    ];
    let signatures: BTreeMap<&str, _> = builtin_signatures().into_iter().collect();
    for (name, function) in table {
        let sig = signatures.get(name).cloned().ok_or_else(|| {
            Error::new(codes::FUNCTION_INVALID, format!("no signature for {name}"))
        })?;
        registry
            .register_builtin(name, sig, function)
            .map_err(|e| Error::new(e.code, e.message))?;
    }
    Ok(())
}

/// Applies the staged writes of a successful run and queues its events.
pub(crate) fn commit(world: &mut World) -> Result<()> {
    let (staged, events, logs) = {
        let mut state = world.resource_mut::<ScriptRun>();
        (
            std::mem::take(&mut state.staged),
            std::mem::take(&mut state.events),
            std::mem::take(&mut state.logs),
        )
    };
    for write in staged {
        match write {
            Staged::SetPosition { entity, position } => {
                if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                    transform.position = position;
                }
            }
            Staged::ComponentSet {
                entity,
                name,
                field,
                value,
            } => {
                if world.get_entity(entity).is_err() {
                    continue;
                }
                let project = std::sync::Arc::clone(&world.resource::<ProjectResource>().0);
                let Some(descriptor) = project.component_catalog.0.get(&name) else {
                    continue;
                };
                let mut components = crate::catalog::snapshot(world, entity)?;
                let Some(current) = components.remove(&name) else {
                    continue;
                };
                let mut data = current.data;
                if let Some(object) = data.as_object_mut() {
                    object.insert(field, value);
                }
                crate::catalog::write_component(
                    world,
                    entity,
                    &name,
                    CustomValue {
                        version: descriptor.version,
                        data,
                    },
                )?;
            }
        }
    }
    if !events.is_empty()
        && let Err(error) = crate::emit_events(world, events)
        && let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>()
    {
        // A full event queue drops this run's events; the run itself succeeded.
        log.0.push(format!("script: events dropped ({error})"));
    }
    if let Some(mut log) = world.get_resource_mut::<crate::runtime::RuntimeLog>() {
        for line in logs {
            if log.0.len() < MAX_RUNTIME_LOG {
                log.0.push(line);
            }
        }
    }
    Ok(())
}

/// Script log lines stop being appended once the runtime log holds this many.
pub const MAX_RUNTIME_LOG: usize = 1024;

/// A short payload describing a fault, for `script.faulted` events.
pub(crate) fn fault_payload(graph: &str, code: &str, site: Option<String>, message: &str) -> Json {
    json!({"graph": graph, "code": code, "site": site, "message": message})
}

/// The authored id of `entity`, or `entity:<bits>`.
pub(crate) fn authored_id(world: &World, entity: Entity) -> String {
    world
        .get::<AuthoredId>(entity)
        .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone())
}
