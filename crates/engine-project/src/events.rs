//! Bounded next-tick event delivery. Callbacks never recursively dispatch.
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct EngineEvent {
    pub name: String,
    pub source: Option<Entity>,
    pub target: Option<Entity>,
    pub payload: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct EventRecord {
    pub name: String,
    pub source: Option<String>,
    pub target: Option<String>,
    pub payload: Value,
}
pub(crate) fn records(world: &World) -> Vec<EventRecord> {
    world
        .get_resource::<DeliveredRecords>()
        .map_or_else(Vec::new, |r| r.0.to_vec())
}
pub(crate) fn delivered_records(world: &World) -> &[EventRecord] {
    world
        .get_resource::<LastDispatchRecords>()
        .map_or(&[], |r| r.0.as_ref())
}
#[derive(Resource, Default)]
struct DeliveredRecords(Arc<[EventRecord]>);
/// Last dispatch survives scene replacement until the next (including empty)
/// dispatch, so hosts can observe deliveries made before a transition.
#[derive(Resource, Default)]
struct LastDispatchRecords(Arc<[EventRecord]>);
fn capture_records(world: &World, events: &[EngineEvent]) -> Vec<EventRecord> {
    let id = |entity: Entity| {
        world
            .get::<crate::runtime::AuthoredId>(entity)
            .map(|a| a.id.clone())
            .unwrap_or_else(|| format!("entity:{}", entity.to_bits()))
    };
    events
        .iter()
        .map(|event| EventRecord {
            name: event.name.clone(),
            source: event.source.map(id),
            target: event.target.map(id),
            payload: event.payload.clone(),
        })
        .collect()
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventBinding {
    pub event: String,
    pub handler: String,
}
#[derive(Component, Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventBindings {
    pub bindings: Vec<EventBinding>,
}
#[derive(Resource, Clone, Copy, Debug)]
pub struct EventLimits {
    pub queued: usize,
    pub payload_bytes: usize,
    pub deliveries_per_tick: usize,
}
impl Default for EventLimits {
    fn default() -> Self {
        Self {
            queued: 1024,
            payload_bytes: 65536,
            deliveries_per_tick: 4096,
        }
    }
}
#[derive(Resource, Default)]
pub(crate) struct EventQueue {
    pending: VecDeque<EngineEvent>,
}
/// Accepted, targeted 3D contact transitions awaiting ordinary next-tick delivery.
/// Audio observes a snapshot after physics writes poses; this does not drain the queue.
pub(crate) fn pending_audio_contacts(world: &World) -> Vec<(Entity, String)> {
    world
        .get_resource::<EventQueue>()
        .into_iter()
        .flat_map(|queue| &queue.pending)
        .filter(|event| {
            matches!(
                event.name.as_str(),
                "physics.contact.start" | "physics.contact.stop"
            ) && event.payload.get("dimension").and_then(Value::as_u64) == Some(3)
        })
        .filter_map(|event| event.target.map(|target| (target, event.name.clone())))
        .collect()
}
/// Events dispatched this tick, available to ordinary ECS systems.
#[derive(Resource, Default)]
pub struct DeliveredEvents(pub Vec<EngineEvent>);
pub type EventHandler = fn(&mut World, Entity, &EngineEvent) -> Result<()>;
#[derive(Resource, Default)]
pub(crate) struct EventHandlers(pub BTreeMap<String, EventHandler>);
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}
pub(crate) fn validate_bindings(bindings: &EventBindings) -> Result<()> {
    if bindings.bindings.len() > 64
        || bindings
            .bindings
            .iter()
            .any(|b| !valid_name(&b.event) || !valid_name(&b.handler))
    {
        return Err(Error::new(
            "event.binding.invalid",
            "Invalid event binding or capacity exceeded",
        ));
    }
    Ok(())
}
pub fn emit_event(world: &mut World, event: EngineEvent) -> Result<()> {
    emit_events(world, vec![event])
}
/// Validate and enqueue a complete batch, preserving FIFO order. A rejected batch
/// leaves the existing queue unchanged; callbacks cannot run during insertion.
pub fn emit_events(world: &mut World, events: Vec<EngineEvent>) -> Result<()> {
    let limits = world
        .get_resource::<EventLimits>()
        .copied()
        .unwrap_or_default();
    let pending = world
        .get_resource::<EventQueue>()
        .map_or(0, |q| q.pending.len());
    if events.len() > limits.queued.saturating_sub(pending) {
        return Err(Error::new(
            "event.capacity",
            "Event queue capacity exceeded",
        ));
    }
    for event in &events {
        if !valid_name(&event.name)
            || serde_json::to_vec(&event.payload)?.len() > limits.payload_bytes
        {
            return Err(Error::new(
                "event.invalid",
                "Invalid name or event payload exceeds limit",
            ));
        }
    }
    if !events.is_empty() {
        if !world.contains_resource::<EventQueue>() {
            world.insert_resource(EventQueue::default());
        }
        world.resource_mut::<EventQueue>().pending.extend(events);
    }
    Ok(())
}
pub fn register_event_handler(
    app: &mut EngineApp,
    name: &str,
    handler: EventHandler,
) -> Result<()> {
    if !valid_name(name) {
        return Err(Error::new("event.handler.invalid", "Invalid handler name"));
    }
    if !app.world().contains_resource::<EventHandlers>() {
        app.world_mut().insert_resource(EventHandlers::default());
    }
    let mut handlers = app.world_mut().resource_mut::<EventHandlers>();
    if handlers.0.contains_key(name) {
        return Err(Error::new(
            "event.handler.duplicate",
            "Handler already registered",
        ));
    }
    handlers.0.insert(name.into(), handler);
    Ok(())
}
pub(crate) fn take_events(world: &mut World) -> Vec<EngineEvent> {
    let events: Vec<_> = world
        .get_resource_mut::<EventQueue>()
        .map(|mut q| q.pending.drain(..).collect())
        .unwrap_or_default();
    let events: Vec<_> = events
        .into_iter()
        .filter(|e| e.target.is_none_or(|t| world.get_entity(t).is_ok()))
        .collect();
    let records: Arc<[EventRecord]> = capture_records(world, &events).into();
    world.insert_resource(DeliveredRecords(records.clone()));
    world.insert_resource(LastDispatchRecords(records));
    world.insert_resource(DeliveredEvents(events.clone()));
    events
}
pub(crate) fn clear(world: &mut World) {
    world.insert_resource(EventQueue::default());
    world.insert_resource(DeliveredEvents::default());
    world.insert_resource(DeliveredRecords::default());
}
pub(crate) fn discard_pending(world: &mut World) {
    world.insert_resource(EventQueue::default());
}
