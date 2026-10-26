//! Smart objects in the runtime: the `engine.SmartObject` component, slot claims
//! across entities, activation events and telemetry.
//!
//! Slots are claimed, occupied and released through the native functions below (the
//! `engine.use_smart_object` task wraps them for trees). The service keeps the
//! registry in step with the components, expires unoccupied claims and releases the
//! slots of despawned users.
use crate::{ComponentDescriptor, Error, Result, runtime::AuthoredId};
use bevy_ecs::prelude::*;
use engine_ai::{
    perception::Id,
    smart_object::{
        ObjectDef, ReleaseReason, SlotDef, SlotError, SlotRef, SlotState, SmartEvent, SmartObjects,
    },
};
use engine_core::EngineApp;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub const SMART_OBJECT: &str = "engine.SmartObject";
/// Objects listed in telemetry.
pub const STATUS_MAX_OBJECTS: usize = 16;

pub const EVENT_CLAIMED: &str = "smart_object.claimed";
pub const EVENT_ACTIVATED: &str = "smart_object.activated";
pub const EVENT_RELEASED: &str = "smart_object.released";

/// Slots users can claim on an entity.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmartObject {
    pub enabled: bool,
    /// Seconds a claim may wait for its occupation (0: never expires).
    pub claim_timeout: f32,
    pub slots: Vec<SlotDef>,
}

impl Default for SmartObject {
    fn default() -> Self {
        Self {
            enabled: true,
            claim_timeout: 10.0,
            slots: Vec::new(),
        }
    }
}

impl SmartObject {
    pub(crate) fn def(&self) -> ObjectDef {
        ObjectDef {
            slots: self.slots.clone(),
            claim_timeout: self.claim_timeout,
        }
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.def()
            .validate()
            .map_err(|m| Error::new("smart_object.value.invalid", m))
    }
}

/// Catalog descriptor.
pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: SMART_OBJECT.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "enabled": {"type":"boolean"},
            "claim_timeout": {"type":"number","minimum":0,"maximum":3600,
                "description":"Seconds a claim may wait for its occupation; 0 never expires."},
            "slots": {"type":"array","maxItems":engine_ai::smart_object::MAX_SLOTS,"items":{
                "type":"object","additionalProperties":false,"properties":{
                    "name": {"type":"string","minLength":1,"maxLength":64},
                    "offset": {"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3,
                        "description":"Position relative to the entity transform."},
                    "tags": {"type":"array","maxItems":engine_ai::smart_object::MAX_TAGS,
                        "items":{"type":"string","maxLength":64}},
                    "behavior": {"type":"string","maxLength":128,
                        "description":"Id of the behaviour the user runs at the slot."}}}}
        }}),
        defaults: serde_json::to_value(SmartObject::default()).expect("finite defaults"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

/// The slot registry plus the configs it was built from.
#[derive(Resource, Default)]
pub struct SmartObjectService {
    objects: SmartObjects,
    configs: BTreeMap<Entity, SmartObject>,
    dropped_events: u64,
}

/// A slot as the game sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotInfo {
    pub slot: SlotRef,
    pub entity: Entity,
    pub name: String,
    pub position: [f32; 3],
    pub tags: Vec<String>,
    pub behavior: String,
    pub state: SlotState,
}

impl SmartObjectService {
    pub fn objects(&self) -> &SmartObjects {
        &self.objects
    }
}

pub(super) fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<SmartObject>(app, SMART_OBJECT, 1)?;
    app.world_mut()
        .insert_resource(SmartObjectService::default());
    Ok(())
}

fn with_service<T>(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut SmartObjectService) -> T,
) -> Option<T> {
    let mut service = world.remove_resource::<SmartObjectService>()?;
    let out = run(world, &mut service);
    world.insert_resource(service);
    Some(out)
}

fn missing() -> Error {
    Error::new(
        "smart_object.unavailable",
        "smart objects need a project runtime",
    )
}

fn authored(world: &World, entity: Entity) -> String {
    world
        .get::<AuthoredId>(entity)
        .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone())
}

/// World position of a slot: the object's transform applied to the slot offset.
fn slot_position(world: &World, entity: Entity, offset: [f32; 3]) -> Option<[f32; 3]> {
    crate::world_transform(world, entity)
        .ok()
        .map(|t| t.transform_point(offset))
}

/// Emits the registry events as engine events (bounded by the event queue).
fn publish(world: &mut World, service: &mut SmartObjectService, events: Vec<SmartEvent>) {
    let mut out = Vec::new();
    for event in events {
        let (name, slot, user, reason) = match event {
            SmartEvent::Claimed { slot, user } => (EVENT_CLAIMED, slot, user, None),
            SmartEvent::Activated { slot, user } => (EVENT_ACTIVATED, slot, user, None),
            SmartEvent::Released { slot, user, reason } => {
                (EVENT_RELEASED, slot, user, Some(reason))
            }
        };
        let object = Entity::from_bits(slot.object);
        if world.get_entity(object).is_err() {
            continue;
        }
        let user_entity = Entity::from_bits(user);
        let slot_def = service.objects.slot_def(slot);
        let mut payload = json!({
            "slot": slot_def.map(|s| s.name.clone()),
            "index": slot.slot,
            "behavior": slot_def.map(|s| s.behavior.clone()),
            "user": world.get_entity(user_entity).is_ok().then(|| authored(world, user_entity)),
        });
        if let Some(reason) = reason {
            payload["reason"] = json!(reason.as_str());
        }
        out.push(crate::EngineEvent {
            name: name.to_owned(),
            source: world.get_entity(user_entity).is_ok().then_some(user_entity),
            target: Some(object),
            payload,
        });
    }
    if out.is_empty() {
        return;
    }
    let count = out.len() as u64;
    if crate::emit_events(world, out).is_err() {
        service.dropped_events += count;
    }
}

/// Keeps the registry in step with the components and expires claims. Runs once per
/// tick before the trees.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    with_service(world, |world, service| -> Result<()> {
        sync(world, service)?;
        let mut events = service.objects.tick(dt);
        // Users that vanished lose their slots.
        let holders: Vec<Id> = service
            .objects
            .slots()
            .filter_map(|(_, _, state)| state.user())
            .collect();
        for user in holders {
            if world.get_entity(Entity::from_bits(user)).is_err() {
                events.extend(service.objects.purge_user(user));
            }
        }
        publish(world, service, events);
        Ok(())
    })
    .transpose()?;
    Ok(())
}

/// Brings the registry in line with the world; called at runtime start and after
/// scene transitions.
pub fn prepare(world: &mut World) -> Result<()> {
    with_service(world, sync).transpose()?;
    Ok(())
}

fn sync(world: &mut World, service: &mut SmartObjectService) -> Result<()> {
    let configs: BTreeMap<Entity, SmartObject> = world
        .query::<(Entity, &SmartObject)>()
        .iter(world)
        .filter(|(_, c)| c.enabled)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    let mut events = Vec::new();
    let removed: Vec<Entity> = service
        .configs
        .keys()
        .filter(|e| !configs.contains_key(e))
        .copied()
        .collect();
    for entity in removed {
        service.configs.remove(&entity);
        events.extend(service.objects.remove_object(entity.to_bits()));
    }
    for (entity, config) in configs {
        if service.configs.get(&entity) == Some(&config) {
            continue;
        }
        config.check()?;
        events.extend(
            service
                .objects
                .set_object(entity.to_bits(), config.def())
                .map_err(|m| Error::new("smart_object.capacity", m))?,
        );
        service.configs.insert(entity, config);
    }
    publish(world, service, events);
    Ok(())
}

fn info(world: &World, service: &SmartObjectService, slot: SlotRef) -> Option<SlotInfo> {
    let entity = Entity::from_bits(slot.object);
    let def = service.objects.slot_def(slot)?;
    Some(SlotInfo {
        slot,
        entity,
        name: def.name.clone(),
        position: slot_position(world, entity, def.offset)?,
        tags: def.tags.clone(),
        behavior: def.behavior.clone(),
        state: service.objects.state(slot)?,
    })
}

/// Slots carrying all `tags`, optionally only free ones, ordered by distance to `near`
/// when given (ties by slot) and by slot otherwise.
///
/// # Errors
/// A world without a smart object service.
pub fn find_slots(
    world: &mut World,
    tags: &[String],
    free_only: bool,
    near: Option<[f32; 3]>,
) -> Result<Vec<SlotInfo>> {
    with_service(world, |world, service| {
        let mut found: Vec<SlotInfo> = service
            .objects
            .find(tags, free_only)
            .into_iter()
            .filter_map(|slot| info(world, service, slot))
            .collect();
        if let Some(near) = near {
            let distance = |s: &SlotInfo| {
                let d = [
                    s.position[0] - near[0],
                    s.position[1] - near[1],
                    s.position[2] - near[2],
                ];
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
            };
            found.sort_by(|a, b| {
                distance(a)
                    .total_cmp(&distance(b))
                    .then(a.slot.cmp(&b.slot))
            });
        }
        found
    })
    .ok_or_else(missing)
}

/// Like [`find_slots`] for a shared world: slots carrying all `tags`, ordered by slot.
pub fn find_slots_in(world: &World, tags: &[String], free_only: bool) -> Vec<SlotInfo> {
    let Some(service) = world.get_resource::<SmartObjectService>() else {
        return Vec::new();
    };
    service
        .objects
        .find(tags, free_only)
        .into_iter()
        .filter_map(|slot| info(world, service, slot))
        .collect()
}

/// The slot's current info.
///
/// # Errors
/// A world without a smart object service.
pub fn slot_info(world: &mut World, slot: SlotRef) -> Result<Option<SlotInfo>> {
    with_service(world, |world, service| info(world, service, slot)).ok_or_else(missing)
}

fn apply(
    world: &mut World,
    run: impl FnOnce(&mut SmartObjects) -> std::result::Result<SmartEvent, SlotError>,
) -> Result<std::result::Result<(), SlotError>> {
    with_service(world, |world, service| match run(&mut service.objects) {
        Ok(event) => {
            publish(world, service, vec![event]);
            Ok(())
        }
        Err(error) => Err(error),
    })
    .ok_or_else(missing)
}

/// Reserves a free slot for `user`; the claim expires after the object's
/// `claim_timeout` unless it is occupied.
///
/// # Errors
/// A world without a smart object service. A refused claim is the inner `Err`.
pub fn claim_slot(
    world: &mut World,
    slot: SlotRef,
    user: Entity,
) -> Result<std::result::Result<(), SlotError>> {
    apply(world, |o| o.claim(slot, user.to_bits()))
}

/// Starts the use of a slot `user` claimed (emits `smart_object.activated`).
///
/// # Errors
/// A world without a smart object service. A refusal is the inner `Err`.
pub fn occupy_slot(
    world: &mut World,
    slot: SlotRef,
    user: Entity,
) -> Result<std::result::Result<(), SlotError>> {
    apply(world, |o| o.occupy(slot, user.to_bits()))
}

/// Frees a slot `user` holds.
///
/// # Errors
/// A world without a smart object service. A refusal is the inner `Err`.
pub fn release_slot(
    world: &mut World,
    slot: SlotRef,
    user: Entity,
    reason: ReleaseReason,
) -> Result<std::result::Result<(), SlotError>> {
    apply(world, |o| o.release(slot, user.to_bits(), reason))
}

/// Telemetry for one object.
#[derive(Clone, Debug, Serialize)]
pub struct ObjectStatus {
    pub entity: String,
    pub slots: Vec<SlotStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SlotStatus {
    pub name: String,
    pub tags: Vec<String>,
    /// `free`, `claimed` or `occupied`.
    pub state: &'static str,
    /// Authored id of the holder.
    pub user: Option<String>,
}

/// The `smart_objects` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct SmartObjectStatus {
    pub objects: usize,
    pub slots: usize,
    pub free: usize,
    pub claimed: usize,
    pub occupied: usize,
    /// The first [`STATUS_MAX_OBJECTS`] objects by authored id.
    pub listed: Vec<ObjectStatus>,
    pub dropped_events: u64,
}

/// Bounded telemetry; `None` without smart objects.
pub fn status(world: &World) -> Option<SmartObjectStatus> {
    let service = world.get_resource::<SmartObjectService>()?;
    if service.objects.object_count() == 0 {
        return None;
    }
    let (mut slots, mut free, mut claimed, mut occupied) = (0, 0, 0, 0);
    let mut by_object: BTreeMap<String, Vec<SlotStatus>> = BTreeMap::new();
    for (slot, def, state) in service.objects.slots() {
        slots += 1;
        let label = match state {
            SlotState::Free => {
                free += 1;
                "free"
            }
            SlotState::Claimed { .. } => {
                claimed += 1;
                "claimed"
            }
            SlotState::Occupied { .. } => {
                occupied += 1;
                "occupied"
            }
        };
        by_object
            .entry(authored(world, Entity::from_bits(slot.object)))
            .or_default()
            .push(SlotStatus {
                name: def.name.clone(),
                tags: def.tags.clone(),
                state: label,
                user: state.user().map(|u| authored(world, Entity::from_bits(u))),
            });
    }
    Some(SmartObjectStatus {
        objects: service.objects.object_count(),
        slots,
        free,
        claimed,
        occupied,
        listed: by_object
            .into_iter()
            .take(STATUS_MAX_OBJECTS)
            .map(|(entity, slots)| ObjectStatus { entity, slots })
            .collect(),
        dropped_events: service.dropped_events,
    })
}
