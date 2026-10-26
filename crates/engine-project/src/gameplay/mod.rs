//! Gameplay abilities in the runtime (`engine-gameplay` wrapped for projects).
//!
//! * The `gameplay` asset kind: one JSON [`Definitions`] document holding attribute sets,
//!   effects and abilities, validated with the project (`project_validate`).
//! * `engine.AbilitySystem` ([`AbilitySystemComponent`]): which attribute sets, initial
//!   values, granted abilities, initial effects and loose tags an entity starts with.
//! * The service: one [`GameplayRuntime`] per scene (every component must name the same
//!   definitions asset), stepped once per tick before behavior trees.
//! * Events `ability.*`, `effect.*`, `attribute.changed` and authored `emit_event` steps,
//!   targeted at the actor, bounded per tick.
//! * Native API: [`register_ability_behaviour`], [`try_activate_ability`],
//!   [`apply_effect`], [`attribute`], [`has_tag`] and friends; [`with_abilities`] for
//!   prediction-ledger calls.
//! * The `engine.activate_ability` / `engine.wait_ability_end` tree tasks and the
//!   `abilities` runtime snapshot telemetry.
//!
//! See `docs/gameplay-abilities.md`.
use crate::{ComponentDescriptor, EngineEvent, Error, Project, Result, Scene, runtime::AuthoredId};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
pub use engine_gameplay::ability::{
    self as gas, AbilityBehaviour, AbilityRuntime, AbilitySystem, ActivationRequest, ActorInit,
    ApplyFailure, AttributeValue, BehaviourContext, BehaviourFactory, BehaviourRegistry,
    BehaviourStatus, CancelReason, Definitions, EffectHandle, EffectInfo, FailReason,
    GameplayEvent, InstanceId, Library, Outcome,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) mod tasks;

pub const ABILITY_SYSTEM: &str = "engine.AbilitySystem";
/// Asset kind of the definitions document.
pub const ASSET_KIND: &str = "gameplay";
/// Largest definitions document.
pub const MAX_DOCUMENT_BYTES: u64 = 1 << 20;
/// Engine events the service emits per tick; the rest are dropped and counted.
pub const MAX_EVENTS_PER_TICK: usize = 256;
/// Of those, `attribute.changed` events (coalesced per actor and attribute per batch).
pub const MAX_ATTRIBUTE_EVENTS_PER_TICK: usize = 128;
/// Actors listed in telemetry.
pub const STATUS_MAX_ACTORS: usize = 16;
const STATUS_MAX_ITEMS: usize = 16;
/// Event handler that turns any bound event into a gameplay event of the same name on
/// the bound entity (instigator = the event's source).
pub const TRIGGER_HANDLER: &str = "engine.trigger_ability";

pub const EVENT_ACTIVATED: &str = "ability.activated";
pub const EVENT_COMMITTED: &str = "ability.committed";
pub const EVENT_ENDED: &str = "ability.ended";
pub const EVENT_CANCELLED: &str = "ability.cancelled";
pub const EVENT_FAILED: &str = "ability.failed";
pub const EVENT_EFFECT_APPLIED: &str = "effect.applied";
pub const EVENT_EFFECT_REMOVED: &str = "effect.removed";
pub const EVENT_ATTRIBUTE_CHANGED: &str = "attribute.changed";

/// The ability runtime with the Bevy world as host.
pub type GameplayRuntime = AbilityRuntime<World>;

/// An ability granted at start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrantedAbility {
    pub ability: String,
    pub level: u32,
}

impl Default for GrantedAbility {
    fn default() -> Self {
        Self {
            ability: String::new(),
            level: 1,
        }
    }
}

/// An effect applied (by the entity to itself) at start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InitialEffect {
    pub effect: String,
    pub level: u32,
}

impl Default for InitialEffect {
    fn default() -> Self {
        Self {
            effect: String::new(),
            level: 1,
        }
    }
}

/// Authored start state of an actor. The live attributes, tags and effects live in the
/// service, so this component never changes while the game runs; editing it at runtime
/// re-initialises the actor.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AbilitySystemComponent {
    /// Id of the `gameplay` asset.
    pub definitions: String,
    /// Attribute set names, in evaluation order.
    pub attribute_sets: Vec<String>,
    /// Initial base values overriding the definitions.
    pub attributes: BTreeMap<String, f32>,
    pub abilities: Vec<GrantedAbility>,
    pub effects: Vec<InitialEffect>,
    /// Loose gameplay tags.
    pub tags: Vec<String>,
}

impl AbilitySystemComponent {
    fn init(&self) -> ActorInit {
        ActorInit {
            sets: self.attribute_sets.clone(),
            attributes: self.attributes.clone(),
            abilities: self
                .abilities
                .iter()
                .map(|a| (a.ability.clone(), a.level))
                .collect(),
            tags: self.tags.clone(),
        }
    }
}

/// Catalog descriptor.
pub fn descriptor() -> ComponentDescriptor {
    let grant = |field: &str, what: &str| {
        json!({"type":"array","maxItems":64,"items":{"type":"object","additionalProperties":false,
            "required":[field],"properties":{
                field: {"type":"string","maxLength":64,"description":what},
                "level": {"type":"integer","minimum":1,"maximum":1000}}}})
    };
    ComponentDescriptor {
        name: ABILITY_SYSTEM.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "definitions": {"type":"string","maxLength":128,
                "description":"Id of the gameplay asset holding attribute sets, effects and abilities."},
            "attribute_sets": {"type":"array","maxItems":16,"items":{"type":"string","maxLength":64}},
            "attributes": {"type":"object",
                "description":"Initial base values by attribute name (numbers)."},
            "abilities": grant("ability", "Granted ability name."),
            "effects": grant("effect", "Effect the entity applies to itself at start."),
            "tags": {"type":"array","maxItems":64,"items":{"type":"string","maxLength":128}}
        }}),
        defaults: serde_json::to_value(AbilitySystemComponent::default()).expect("defaults"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

/// The service resource: native behaviour registry and the live runtime.
#[derive(Resource, Default)]
pub struct Gameplay {
    registry: BehaviourRegistry<World>,
    runtime: Option<GameplayRuntime>,
    definitions: Option<String>,
    configs: BTreeMap<Entity, AbilitySystemComponent>,
    dropped_events: u64,
    emitted: usize,
    attribute_events: usize,
}

impl Gameplay {
    pub fn runtime(&self) -> Option<&GameplayRuntime> {
        self.runtime.as_ref()
    }
}

fn unavailable() -> Error {
    Error::new(
        "gameplay.unavailable",
        "no ability system is running (no engine.AbilitySystem in the scene, or called from inside an ability behaviour)",
    )
}

/// Loads and validates a `gameplay` asset.
///
/// # Errors
/// Unknown asset, wrong kind, oversize file or invalid definitions.
pub fn load_definitions(project: &Project, id: &str) -> Result<Arc<Library>> {
    project.require_asset(id, ASSET_KIND)?;
    let file = project.resolve_asset(id)?;
    let size = std::fs::metadata(&file)?.len();
    if size > MAX_DOCUMENT_BYTES {
        return Err(Error::new(
            "gameplay.definitions.invalid",
            format!("gameplay asset {id} exceeds {MAX_DOCUMENT_BYTES} bytes"),
        )
        .at(&file, ""));
    }
    let bytes = std::fs::read(&file)?;
    Definitions::from_json(&bytes).map_err(|m| {
        Error::new(
            "gameplay.definitions.invalid",
            format!("gameplay asset {id}: {m}"),
        )
        .at(&file, "")
    })
}

/// Scene checks: every `engine.AbilitySystem` names the same existing `gameplay` asset,
/// and its sets, attributes, abilities, effects and tags exist and are valid.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    let mut definitions: Option<(String, Arc<Library>)> = None;
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(ABILITY_SYSTEM) else {
            continue;
        };
        let component: AbilitySystemComponent = serde_json::from_value(value.data.clone())?;
        let invalid = |m: String| {
            Error::new(
                "gameplay.component.invalid",
                format!("entity {}: {m}", entity.id),
            )
        };
        if component.definitions.is_empty() {
            return Err(invalid("definitions must name a gameplay asset".into()));
        }
        let library = match &definitions {
            Some((id, _)) if *id != component.definitions => {
                return Err(Error::new(
                    "gameplay.definitions.mixed",
                    format!(
                        "entity {}: every engine.AbilitySystem of a scene must use one gameplay asset ({id} and {})",
                        entity.id, component.definitions
                    ),
                ));
            }
            Some((_, library)) => library.clone(),
            None => {
                let library = load_definitions(project, &component.definitions)?;
                definitions = Some((component.definitions.clone(), library.clone()));
                library
            }
        };
        let mut probe = AbilitySystem::new(library.clone());
        probe.add_actor(1, &component.init()).map_err(invalid)?;
        for effect in &component.effects {
            if library.effect(&effect.effect).is_none() || effect.level == 0 {
                return Err(invalid(format!(
                    "unknown initial effect {:?} or level 0",
                    effect.effect
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<AbilitySystemComponent>(app, ABILITY_SYSTEM, 1)?;
    app.world_mut().insert_resource(Gameplay::default());
    crate::register_event_handler(app, TRIGGER_HANDLER, trigger_handler)
}

fn trigger_handler(world: &mut World, entity: Entity, event: &EngineEvent) -> Result<()> {
    match send_gameplay_event(world, entity, &event.name, event.source) {
        Err(e) if e.diagnostics[0].code == "gameplay.unavailable" => Ok(()),
        other => other.map(|_| ()),
    }
}

/// Registers a native ability behaviour under `name` for abilities whose `native` names
/// it. Register before the runtime starts (later registrations also reach a running
/// service).
///
/// # Errors
/// A repeated or invalid name, or an `app` that is not a project runtime.
pub fn register_ability_behaviour(
    app: &mut EngineApp,
    name: &str,
    factory: BehaviourFactory<World>,
) -> Result<()> {
    let mut service = app
        .world_mut()
        .get_resource_mut::<Gameplay>()
        .ok_or_else(|| {
            Error::new(
                "gameplay.unavailable",
                "register_ability_behaviour needs a project runtime",
            )
        })?;
    let duplicate = |m| Error::new("gameplay.behaviour.duplicate", m);
    service
        .registry
        .register(name, factory)
        .map_err(duplicate)?;
    if let Some(runtime) = service.runtime.as_mut() {
        runtime
            .registry_mut()
            .register(name, factory)
            .map_err(duplicate)?;
    }
    Ok(())
}

fn with_service<T>(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut Gameplay) -> T,
) -> Option<T> {
    let mut service = world.remove_resource::<Gameplay>()?;
    let out = run(world, &mut service);
    world.insert_resource(service);
    Some(out)
}

fn project(world: &World) -> Option<Arc<Project>> {
    world
        .get_resource::<crate::runtime::ProjectResource>()
        .map(|p| p.0.clone())
}

/// Brings the service in line with the world; called at runtime start and after scene
/// transitions.
pub fn prepare(world: &mut World) -> Result<()> {
    let Some(project) = project(world) else {
        return Ok(());
    };
    with_service(world, |world, service| sync(world, service, &project)).transpose()?;
    Ok(())
}

/// Syncs components, then advances effects and ability instances by `dt`. Runs once per
/// tick before behavior trees.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    let Some(project) = project(world) else {
        return Ok(());
    };
    with_service(world, |world, service| -> Result<()> {
        service.emitted = 0;
        service.attribute_events = 0;
        sync(world, service, &project)?;
        if let Some(runtime) = service.runtime.as_mut() {
            runtime.step(world, dt);
        }
        publish(world, service);
        Ok(())
    })
    .transpose()?;
    Ok(())
}

/// Cancels every instance without events; called when the runtime stops.
pub fn shutdown(world: &mut World) {
    with_service(world, |world, service| {
        if let Some(runtime) = service.runtime.as_mut() {
            runtime.shutdown(world);
            runtime.system.drain_events();
        }
    });
}

fn sync(world: &mut World, service: &mut Gameplay, project: &Project) -> Result<()> {
    let configs: BTreeMap<Entity, AbilitySystemComponent> = world
        .query::<(Entity, &AbilitySystemComponent)>()
        .iter(world)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    let mut wanted: Option<&str> = None;
    for config in configs.values() {
        match wanted {
            Some(id) if id != config.definitions => {
                return Err(Error::new(
                    "gameplay.definitions.mixed",
                    "every engine.AbilitySystem must use one gameplay asset",
                ));
            }
            _ => wanted = Some(&config.definitions),
        }
    }
    if wanted != service.definitions.as_deref() {
        if let Some(runtime) = service.runtime.as_mut() {
            runtime.shutdown(world);
        }
        service.runtime = match wanted {
            Some(id) => Some(AbilityRuntime::new(
                AbilitySystem::new(load_definitions(project, id)?),
                service.registry.clone(),
            )),
            None => None,
        };
        service.definitions = wanted.map(str::to_owned);
        service.configs.clear();
    }
    let Some(runtime) = service.runtime.as_mut() else {
        return Ok(());
    };
    let stale: Vec<Entity> = service
        .configs
        .iter()
        .filter(|(e, c)| configs.get(e) != Some(c))
        .map(|(e, _)| *e)
        .collect();
    for entity in stale {
        runtime.remove_actor(world, entity.to_bits());
        service.configs.remove(&entity);
    }
    for (entity, config) in configs {
        if service.configs.contains_key(&entity) {
            continue;
        }
        let actor = entity.to_bits();
        let invalid = |m: String| Error::new("gameplay.component.invalid", m);
        runtime
            .system
            .add_actor(actor, &config.init())
            .map_err(invalid)?;
        for effect in &config.effects {
            // Requirements and capacity are runtime outcomes, not authoring errors.
            if let Err(ApplyFailure::UnknownEffect) =
                runtime
                    .system
                    .apply_effect(actor, actor, &effect.effect, effect.level, None)
            {
                return Err(invalid(format!("unknown effect {:?}", effect.effect)));
            }
        }
        service.configs.insert(entity, config);
    }
    publish(world, service);
    Ok(())
}

fn authored(world: &World, entity: Entity) -> Option<String> {
    world.get_entity(entity).ok()?;
    Some(
        world
            .get::<AuthoredId>(entity)
            .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone()),
    )
}

/// `f32` as the shortest decimal JSON number.
fn num(v: f32) -> Value {
    v.to_string()
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// Drains the system's events into engine events (targeted at the actor), coalescing
/// attribute changes and honouring the per-tick budgets.
fn publish(world: &mut World, service: &mut Gameplay) {
    let Some(runtime) = service.runtime.as_mut() else {
        return;
    };
    let events = runtime.system.drain_events();
    let mut out: Vec<EngineEvent> = Vec::new();
    let mut attributes: BTreeMap<(u64, String), usize> = BTreeMap::new();
    let actor_of = |id: u64| {
        let e = Entity::from_bits(id);
        world.get_entity(e).is_ok().then_some(e)
    };
    for event in events {
        let (actor, name, source, payload) = match event {
            GameplayEvent::AbilityActivated {
                actor,
                ability,
                instance,
                target,
                predicted,
            } => (
                actor,
                EVENT_ACTIVATED.to_owned(),
                None,
                json!({"ability":ability,"instance":instance.0,
                    "target":target.and_then(|t| authored(world, Entity::from_bits(t))),
                    "predicted":predicted}),
            ),
            GameplayEvent::AbilityCommitted {
                actor,
                ability,
                instance,
            } => (
                actor,
                EVENT_COMMITTED.to_owned(),
                None,
                json!({"ability":ability,"instance":instance.0}),
            ),
            GameplayEvent::AbilityEnded {
                actor,
                ability,
                instance,
            } => (
                actor,
                EVENT_ENDED.to_owned(),
                None,
                json!({"ability":ability,"instance":instance.0}),
            ),
            GameplayEvent::AbilityCancelled {
                actor,
                ability,
                instance,
                reason,
            } => (
                actor,
                EVENT_CANCELLED.to_owned(),
                None,
                json!({"ability":ability,"instance":instance.0,"reason":reason.as_str()}),
            ),
            GameplayEvent::AbilityFailed {
                actor,
                ability,
                reason,
            } => (
                actor,
                EVENT_FAILED.to_owned(),
                None,
                json!({"ability":ability,"reason":reason.as_str()}),
            ),
            GameplayEvent::EffectApplied {
                target,
                source,
                effect,
                handle,
                stacks,
                predicted,
            } => (
                target,
                EVENT_EFFECT_APPLIED.to_owned(),
                actor_of(source),
                json!({"effect":effect,"handle":handle.map(|h| h.0),"stacks":stacks,
                    "source":authored(world, Entity::from_bits(source)),"predicted":predicted}),
            ),
            GameplayEvent::EffectRemoved {
                target,
                effect,
                handle,
                reason,
            } => (
                target,
                EVENT_EFFECT_REMOVED.to_owned(),
                None,
                json!({"effect":effect,"handle":handle.0,"reason":reason.as_str()}),
            ),
            GameplayEvent::AttributeChanged {
                actor,
                attribute,
                old,
                new,
                base,
            } => {
                if actor_of(actor).is_none() {
                    continue;
                }
                if let Some(&i) = attributes.get(&(actor, attribute.clone())) {
                    out[i].payload["new"] = num(new);
                    out[i].payload["base"] = num(base);
                    continue;
                }
                if service.attribute_events >= MAX_ATTRIBUTE_EVENTS_PER_TICK {
                    service.dropped_events += 1;
                    continue;
                }
                service.attribute_events += 1;
                attributes.insert((actor, attribute.clone()), out.len());
                (
                    actor,
                    EVENT_ATTRIBUTE_CHANGED.to_owned(),
                    None,
                    json!({"attribute":attribute,"old":num(old),"new":num(new),"base":num(base)}),
                )
            }
            GameplayEvent::Custom {
                actor,
                event,
                payload,
            } => (actor, event, None, payload),
        };
        let Some(target) = actor_of(actor) else {
            continue;
        };
        if service.emitted >= MAX_EVENTS_PER_TICK {
            service.dropped_events += 1;
            continue;
        }
        service.emitted += 1;
        out.push(EngineEvent {
            name,
            source,
            target: Some(target),
            payload,
        });
    }
    let count = out.len() as u64;
    if count > 0 && crate::emit_events(world, out).is_err() {
        service.dropped_events += count;
    }
}

// ------------------------------------------------------------------ native API

/// Runs `run` against the live runtime with the world, then delivers behaviour ends and
/// publishes the resulting events. Use it for prediction-ledger calls:
/// `with_abilities(world, |_, rt| ledger.confirm(key, &mut rt.system))`.
///
/// # Errors
/// No running ability system.
pub fn with_abilities<T>(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut GameplayRuntime) -> T,
) -> Result<T> {
    with_service(world, |world, service| {
        let out = {
            let runtime = service.runtime.as_mut()?;
            let out = run(world, runtime);
            runtime.deliver_ends(world);
            out
        };
        publish(world, service);
        Some(out)
    })
    .flatten()
    .ok_or_else(unavailable)
}

/// Tries to activate a granted ability on `actor`. A refusal (`ability.failed`) is the
/// inner `Err`.
///
/// # Errors
/// No running ability system.
pub fn try_activate_ability(
    world: &mut World,
    actor: Entity,
    ability: &str,
    target: Option<Entity>,
) -> Result<std::result::Result<InstanceId, FailReason>> {
    let mut request = ActivationRequest::new(actor.to_bits(), ability);
    request.target = target.map(Entity::to_bits);
    activate_ability(world, request)
}

/// [`try_activate_ability`] with a full request (source, prediction key).
///
/// # Errors
/// No running ability system.
pub fn activate_ability(
    world: &mut World,
    request: ActivationRequest,
) -> Result<std::result::Result<InstanceId, FailReason>> {
    with_abilities(world, |world, rt| rt.try_activate(world, request))
}

/// Applies `effect` from `source` to `target` at `level`. Instant effects return
/// `Ok(None)`.
///
/// # Errors
/// No running ability system. A refused application is the inner `Err`.
pub fn apply_effect(
    world: &mut World,
    source: Entity,
    target: Entity,
    effect: &str,
    level: u32,
) -> Result<std::result::Result<Option<EffectHandle>, ApplyFailure>> {
    with_abilities(world, |_, rt| {
        rt.system
            .apply_effect(source.to_bits(), target.to_bits(), effect, level, None)
    })
}

/// # Errors
/// No running ability system.
pub fn remove_effect(world: &mut World, target: Entity, handle: EffectHandle) -> Result<bool> {
    with_abilities(world, |_, rt| {
        rt.system.remove_effect(target.to_bits(), handle)
    })
}

/// Cancels every active instance of `ability` on `actor`; returns how many.
///
/// # Errors
/// No running ability system.
pub fn cancel_ability(world: &mut World, actor: Entity, ability: &str) -> Result<usize> {
    with_abilities(world, |_, rt| {
        rt.system.cancel_ability(actor.to_bits(), ability)
    })
}

/// # Errors
/// No running ability system.
pub fn cancel_instance(world: &mut World, instance: InstanceId) -> Result<bool> {
    with_abilities(world, |_, rt| rt.system.cancel_instance(instance))
}

/// Sends a gameplay event to `actor`: every granted ability it triggers tries to
/// activate. Returns how many activated.
///
/// # Errors
/// No running ability system.
pub fn send_gameplay_event(
    world: &mut World,
    actor: Entity,
    event: &str,
    instigator: Option<Entity>,
) -> Result<usize> {
    with_abilities(world, |world, rt| {
        rt.send_event(
            world,
            actor.to_bits(),
            event,
            instigator.map(Entity::to_bits),
            None,
        )
        .iter()
        .filter(|(_, r)| r.is_ok())
        .count()
    })
}

/// Adds a loose tag.
///
/// # Errors
/// No running ability system, unknown actor or invalid tag.
pub fn add_tag(world: &mut World, actor: Entity, tag: &str) -> Result<()> {
    with_abilities(world, |_, rt| rt.system.add_loose_tag(actor.to_bits(), tag))?
        .map_err(|m| Error::new("gameplay.tag.invalid", m))
}

/// Removes one count of a loose tag.
///
/// # Errors
/// No running ability system.
pub fn remove_tag(world: &mut World, actor: Entity, tag: &str) -> Result<bool> {
    with_abilities(world, |_, rt| {
        rt.system.remove_loose_tag(actor.to_bits(), tag)
    })
}

fn system(world: &World) -> Option<&AbilitySystem> {
    world
        .get_resource::<Gameplay>()?
        .runtime
        .as_ref()
        .map(|r| &r.system)
}

/// Base and current value of an attribute.
pub fn attribute(world: &World, actor: Entity, name: &str) -> Option<AttributeValue> {
    system(world)?.attribute(actor.to_bits(), name)
}

/// Whether `actor` owns `tag` (exactly, or the tag or a descendant).
pub fn has_tag(world: &World, actor: Entity, tag: &str, exact: bool) -> bool {
    system(world).is_some_and(|s| s.has_tag(actor.to_bits(), tag, exact))
}

/// Owned tags, ordered by name.
pub fn tags(world: &World, actor: Entity) -> Vec<String> {
    system(world)
        .and_then(|s| s.tags(actor.to_bits()))
        .map_or_else(Vec::new, |t| t.iter().map(|(k, _)| k.to_owned()).collect())
}

/// Active effects on `actor`.
pub fn effects(world: &World, actor: Entity) -> Vec<EffectInfo> {
    system(world).map_or_else(Vec::new, |s| s.effects(actor.to_bits()))
}

pub fn instance_outcome(world: &World, instance: InstanceId) -> Option<Outcome> {
    system(world)?.outcome(instance)
}

pub fn is_ability_active(world: &World, actor: Entity, ability: &str) -> bool {
    system(world).is_some_and(|s| s.is_active(actor.to_bits(), ability))
}

// -------------------------------------------------------------------- telemetry

#[derive(Clone, Debug, Serialize)]
pub struct AttributeStatus {
    pub name: String,
    pub base: f32,
    pub current: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectStatus {
    pub effect: String,
    pub stacks: u32,
    /// Seconds left; absent for infinite effects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f32>,
    pub predicted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActorStatus {
    pub entity: String,
    /// The first 16 attributes in evaluation order.
    pub attributes: Vec<AttributeStatus>,
    /// The first 16 owned tags by name.
    pub tags: Vec<String>,
    /// The first 16 active effects by handle.
    pub effects: Vec<EffectStatus>,
    /// Active ability instances (at most 8).
    pub active: Vec<String>,
    pub granted: usize,
}

/// The `abilities` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct AbilitiesStatus {
    pub definitions: String,
    pub actors: usize,
    pub active_effects: usize,
    pub active_abilities: usize,
    pub pending_predictions: usize,
    pub dropped_events: u64,
    /// The first [`STATUS_MAX_ACTORS`] actors by authored id.
    pub listed: Vec<ActorStatus>,
}

/// Bounded telemetry; `None` without ability systems.
pub fn status(world: &World) -> Option<AbilitiesStatus> {
    let service = world.get_resource::<Gameplay>()?;
    let system = &service.runtime.as_ref()?.system;
    let ids = system.actor_ids();
    if ids.is_empty() {
        return None;
    }
    let mut listed: BTreeMap<String, ActorStatus> = BTreeMap::new();
    let mut active_effects = 0;
    for &id in &ids {
        let effects = system.effects(id);
        active_effects += effects.len();
        let Some(entity) = authored(world, Entity::from_bits(id)) else {
            continue;
        };
        listed.insert(
            entity.clone(),
            ActorStatus {
                entity,
                attributes: system
                    .attributes(id)
                    .into_iter()
                    .take(STATUS_MAX_ITEMS)
                    .map(|(name, v)| AttributeStatus {
                        name,
                        base: v.base,
                        current: v.current,
                    })
                    .collect(),
                tags: system.tags(id).map_or_else(Vec::new, |t| {
                    t.iter()
                        .take(STATUS_MAX_ITEMS)
                        .map(|(k, _)| k.to_owned())
                        .collect()
                }),
                effects: effects
                    .into_iter()
                    .take(STATUS_MAX_ITEMS)
                    .map(|e| EffectStatus {
                        effect: e.effect,
                        stacks: e.stacks,
                        remaining: e.remaining,
                        predicted: e.predicted,
                    })
                    .collect(),
                active: system
                    .instances(id)
                    .into_iter()
                    .take(8)
                    .map(|i| i.ability)
                    .collect(),
                granted: system.granted(id).len(),
            },
        );
    }
    Some(AbilitiesStatus {
        definitions: service.definitions.clone().unwrap_or_default(),
        actors: ids.len(),
        active_effects,
        active_abilities: system.instance_ids().len(),
        pending_predictions: system.pending_predictions(),
        dropped_events: service.dropped_events,
        listed: listed.into_values().take(STATUS_MAX_ACTORS).collect(),
    })
}
