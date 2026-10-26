//! The perception runtime service: gathers perceivers, targets and stimuli from the
//! world once per tick, updates every perceiver's memory, keeps the blackboards
//! of tree agents current and emits bounded events.
use super::{
    component::{
        Occlusion, PerceptionNoise, PerceptionSensor, PerceptionSettings, PerceptionStimulus,
    },
    host::{HearingHook, WorldPerceptionHost},
};
use crate::{
    Error, Result, ai::BehaviorTrees, ai::state_tree::StateTrees, events::DeliveredEvents,
    runtime::AuthoredId, world_transform,
};
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue,
    perception::{
        ANONYMOUS, Affiliation, AffiliationFilter, Damage, EventKind, Id, Known, Noise,
        PerceiverInput, Perception, Sense, SensorConfig, TargetInput, UpdateInput,
    },
};
use glam::{Mat4, Vec3};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Most perceivers one runtime supports.
pub const MAX_PERCEIVERS: usize = 512;
/// Most perceivable entities one runtime supports.
pub const MAX_STIMULI: usize = 1024;
/// Noises and damage reports handled per tick (the rest are dropped and counted).
pub const MAX_STIMULI_PER_TICK: usize = 64;
/// Reports queued natively between two ticks.
pub const MAX_PENDING: usize = 256;
/// Events one perceiver may emit per tick.
pub const MAX_EVENTS_PER_PERCEIVER: usize = 4;
/// Events all perceivers together may emit per tick.
pub const MAX_EVENTS_PER_TICK: usize = 64;
/// Perceivers listed in telemetry.
pub const STATUS_MAX_PERCEIVERS: usize = 32;
/// Memory records listed per perceiver in telemetry.
pub const STATUS_MAX_RECORDS: usize = 8;
const EPS: f64 = 1e-6;

pub const EVENT_SENSED: &str = "perception.sensed";
pub const EVENT_LOST: &str = "perception.lost";
pub const EVENT_FORGOTTEN: &str = "perception.forgotten";
/// Engine events the service consumes.
pub const EVENT_NOISE: &str = "perception.noise";
pub const EVENT_DAMAGE: &str = "perception.damage";

#[derive(Clone, Copy, Debug)]
struct PendingNoise {
    source: Option<Entity>,
    position: [f32; 3],
    loudness: f32,
    range: f32,
    team: u32,
}

#[derive(Clone, Copy, Debug)]
struct PendingDamage {
    victim: Entity,
    instigator: Option<Entity>,
    position: Option<[f32; 3]>,
    amount: f32,
}

/// The perception memory of every perceiver plus the queues natives report into.
#[derive(Resource, Default)]
pub struct Perceptions {
    memory: Perception,
    settings: PerceptionSettings,
    pending_noises: Vec<PendingNoise>,
    pending_damages: Vec<PendingDamage>,
    emitters: BTreeMap<Entity, f64>,
    hearing: Option<HearingHook>,
    dropped_events: u64,
    dropped_stimuli: u64,
    rays_cast: usize,
    rays_deferred: usize,
    noises: usize,
}

impl Perceptions {
    /// Every memory record of `perceiver`, ordered by `(subject, sense)`.
    pub fn known(&self, perceiver: Entity) -> Vec<Known> {
        self.memory.known(perceiver.to_bits())
    }
    /// How a perceiver of team `a` sees a stimulus of team `b`.
    pub fn affiliation(&self, a: u32, b: u32) -> Affiliation {
        self.memory.teams.affiliation(a, b)
    }
    /// The most relevant record among the affiliations in `filter`.
    pub fn best(&self, perceiver: Entity, filter: AffiliationFilter) -> Option<Known> {
        self.memory.best(perceiver.to_bits(), filter)
    }
}

/// Reports a noise made this tick. The team is that of the source's
/// `engine.PerceptionStimulus` (0 without one).
///
/// # Errors
/// A world without a perception service (not a project runtime), or invalid
/// numbers.
pub fn report_noise(
    world: &mut World,
    source: Option<Entity>,
    position: [f32; 3],
    loudness: f32,
    range: f32,
) -> Result<()> {
    if position.iter().any(|v| !v.is_finite())
        || !loudness.is_finite()
        || !range.is_finite()
        || range <= 0.0
    {
        return Err(Error::new(
            "perception.value.invalid",
            "noise needs a finite position, loudness and a positive range",
        ));
    }
    let team = source
        .and_then(|e| world.get::<PerceptionStimulus>(e))
        .map_or(0, |s| s.team);
    let mut service = world.get_resource_mut::<Perceptions>().ok_or_else(|| {
        Error::new(
            "perception.unavailable",
            "report_noise needs a project runtime",
        )
    })?;
    if service.pending_noises.len() >= MAX_PENDING {
        service.dropped_stimuli += 1;
    } else {
        service.pending_noises.push(PendingNoise {
            source,
            position,
            loudness: loudness.clamp(0.0, 1.0),
            range,
            team,
        });
    }
    Ok(())
}

/// Reports damage taken by `victim`; a perceiver with a damage sense learns where
/// the `instigator` was.
///
/// # Errors
/// A world without a perception service, or a non-finite amount.
pub fn report_damage(
    world: &mut World,
    victim: Entity,
    instigator: Option<Entity>,
    amount: f32,
) -> Result<()> {
    if !amount.is_finite() {
        return Err(Error::new(
            "perception.value.invalid",
            "damage amount must be finite",
        ));
    }
    let mut service = world.get_resource_mut::<Perceptions>().ok_or_else(|| {
        Error::new(
            "perception.unavailable",
            "report_damage needs a project runtime",
        )
    })?;
    if service.pending_damages.len() >= MAX_PENDING {
        service.dropped_stimuli += 1;
    } else {
        service.pending_damages.push(PendingDamage {
            victim,
            instigator,
            position: None,
            amount,
        });
    }
    Ok(())
}

pub(super) fn set_hearing_hook(world: &mut World, hook: HearingHook) -> Result<()> {
    let mut service = world.get_resource_mut::<Perceptions>().ok_or_else(|| {
        Error::new(
            "perception.unavailable",
            "register_hearing_attenuation needs a project runtime",
        )
    })?;
    if service.hearing.is_some() {
        return Err(Error::new(
            "perception.hearing.duplicate",
            "a hearing attenuation hook is already registered",
        ));
    }
    service.hearing = Some(hook);
    Ok(())
}

/// Brings the service in line with the world; called at runtime start and after
/// scene transitions.
pub fn prepare(world: &mut World) -> Result<()> {
    with_state(world, |world, state| {
        sync_settings(world, state)?;
        // Memories of removed perceivers go at the next update; emitters of removed
        // entities go now.
        state.emitters.retain(|e, _| world.get_entity(*e).is_ok());
        Ok(())
    })
}

/// Updates perception for one runtime tick. Runs before the tree services so their
/// blackboards are current when they tick.
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    with_state(world, |world, state| step_inner(world, state, dt))
}

fn with_state(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut Perceptions) -> Result<()>,
) -> Result<()> {
    let Some(mut state) = world.remove_resource::<Perceptions>() else {
        return Ok(());
    };
    let result = run(world, &mut state);
    world.insert_resource(state);
    result
}

fn sync_settings(world: &mut World, state: &mut Perceptions) -> Result<()> {
    let found: Vec<PerceptionSettings> = world
        .query::<&PerceptionSettings>()
        .iter(world)
        .cloned()
        .collect();
    if found.len() > 1 {
        return Err(Error::new(
            "perception.settings.duplicate",
            "at most one entity may carry engine.PerceptionSettings",
        ));
    }
    let settings = found.into_iter().next().unwrap_or_default();
    settings.check()?;
    if settings != state.settings {
        state.memory.teams = settings.team_table();
        state.settings = settings;
    }
    Ok(())
}

fn position_of(world: &World, entity: Entity) -> Option<[f32; 3]> {
    world_transform(world, entity).ok().map(|t| t.position())
}

fn forward_of(world: &World, entity: Entity, local: [f32; 3]) -> Option<[f32; 3]> {
    let transform = world_transform(world, entity).ok()?;
    let direction =
        Mat4::from_cols_array_2d(&transform.matrix).transform_vector3(Vec3::from(local));
    let normalized = direction.normalize_or_zero();
    (normalized != Vec3::ZERO).then(|| normalized.to_array())
}

fn authored(world: &World, id: Id) -> Option<String> {
    if id == ANONYMOUS {
        return None;
    }
    let entity = Entity::from_bits(id);
    world.get_entity(entity).ok()?;
    Some(
        world
            .get::<AuthoredId>(entity)
            .map_or_else(|| format!("entity:{id}"), |a| a.id.clone()),
    )
}

fn number(payload: &Value, key: &str, default: f64) -> f64 {
    payload.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn vector(payload: &Value, key: &str) -> Option<[f32; 3]> {
    let items = payload.get(key)?.as_array()?;
    if items.len() != 3 {
        return None;
    }
    let mut out = [0.0f32; 3];
    for (slot, item) in out.iter_mut().zip(items) {
        let v = item.as_f64()? as f32;
        if !v.is_finite() {
            return None;
        }
        *slot = v;
    }
    Some(out)
}

fn step_inner(world: &mut World, state: &mut Perceptions, dt: f32) -> Result<()> {
    sync_settings(world, state)?;
    let sensors: Vec<(Entity, PerceptionSensor)> = world
        .query::<(Entity, &PerceptionSensor)>()
        .iter(world)
        .filter(|(_, s)| s.enabled)
        .map(|(e, s)| (e, s.clone()))
        .collect();
    if sensors.len() > MAX_PERCEIVERS {
        return Err(Error::new(
            "perception.capacity",
            format!("more than {MAX_PERCEIVERS} perception sensors"),
        ));
    }
    if sensors.is_empty() && state.memory.perceiver_count() == 0 {
        state.pending_noises.clear();
        state.pending_damages.clear();
        return Ok(());
    }
    let mut input = UpdateInput {
        max_rays: state.settings.max_rays_per_tick as usize,
        ..UpdateInput::default()
    };
    for (entity, sensor) in &sensors {
        let (Some(position), Some(forward)) = (
            position_of(world, *entity),
            forward_of(world, *entity, sensor.forward),
        ) else {
            continue;
        };
        input.perceivers.push(PerceiverInput {
            id: entity.to_bits(),
            position,
            forward,
            config: SensorConfig {
                team: sensor.team,
                sight: sensor.sight.clone(),
                hearing: sensor.hearing.clone(),
                damage: sensor.damage.clone(),
            },
        });
    }
    let stimuli: Vec<(Entity, PerceptionStimulus)> = world
        .query::<(Entity, &PerceptionStimulus)>()
        .iter(world)
        .filter(|(_, s)| s.enabled && s.visible)
        .map(|(e, s)| (e, s.clone()))
        .collect();
    if stimuli.len() > MAX_STIMULI {
        return Err(Error::new(
            "perception.capacity",
            format!("more than {MAX_STIMULI} perception stimuli"),
        ));
    }
    for (entity, stimulus) in &stimuli {
        if let Some(position) = position_of(world, *entity) {
            input.targets.push(TargetInput {
                id: entity.to_bits(),
                position,
                team: stimulus.team,
                height_offset: stimulus.height_offset,
            });
        }
    }
    gather_noises(world, state, dt, &mut input);
    gather_damages(world, state, &mut input);
    state.noises = input.noises.len();

    let mask = if state.settings.occlusion_layers.is_empty() {
        u32::MAX
    } else {
        let names: Vec<&str> = state
            .settings
            .occlusion_layers
            .iter()
            .map(String::as_str)
            .collect();
        crate::simulation::physics::layer_mask(world, &names).map_err(|e| {
            Error::new(
                "perception.occlusion.layers",
                format!("occlusion_layers: {e}"),
            )
        })?
    };
    let report = {
        let mut host = WorldPerceptionHost {
            world,
            occlusion: if state.settings.occlusion == Occlusion::None {
                Occlusion::None
            } else {
                Occlusion::Physics3d
            },
            mask,
            hearing: state.hearing,
        };
        state.memory.update(dt, &input, &mut host)
    };
    state.rays_cast = report.rays_cast;
    state.rays_deferred = report.rays_deferred;
    state.dropped_events += report.dropped_events as u64;
    write_blackboards(world, state, &sensors)?;
    emit(world, state, &report.events)
}

fn gather_noises(world: &mut World, state: &mut Perceptions, dt: f32, input: &mut UpdateInput) {
    let mut pending: Vec<PendingNoise> = std::mem::take(&mut state.pending_noises);
    // Periodic emitters, in entity order.
    let emitters: Vec<(Entity, PerceptionNoise)> = world
        .query::<(Entity, &PerceptionNoise)>()
        .iter(world)
        .filter(|(_, n)| n.enabled)
        .map(|(e, n)| (e, n.clone()))
        .collect();
    state
        .emitters
        .retain(|e, _| emitters.iter().any(|(live, _)| live == e));
    for (entity, emitter) in &emitters {
        let elapsed = state
            .emitters
            .entry(*entity)
            .or_insert(f64::from(emitter.interval));
        *elapsed += f64::from(dt);
        if f64::from(emitter.interval) > 0.0 && *elapsed + EPS < f64::from(emitter.interval) {
            continue;
        }
        *elapsed = if emitter.interval > 0.0 {
            (*elapsed - f64::from(emitter.interval)).max(0.0)
        } else {
            0.0
        };
        let (Some(position), team) = (
            position_of(world, *entity),
            world
                .get::<PerceptionStimulus>(*entity)
                .map_or(0, |s| s.team),
        ) else {
            continue;
        };
        pending.push(PendingNoise {
            source: Some(*entity),
            position,
            loudness: emitter.loudness,
            range: emitter.range,
            team,
        });
    }
    // Engine events delivered this tick.
    let delivered: Vec<(Option<Entity>, Option<Entity>, Value)> = world
        .get_resource::<DeliveredEvents>()
        .map(|d| {
            d.0.iter()
                .filter(|e| e.name == EVENT_NOISE)
                .map(|e| (e.source, e.target, e.payload.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (source, target, payload) in delivered {
        let origin = source.or(target);
        let position =
            vector(&payload, "position").or_else(|| origin.and_then(|e| position_of(world, e)));
        let Some(position) = position else {
            state.dropped_stimuli += 1;
            continue;
        };
        let team = payload
            .get("team")
            .and_then(Value::as_u64)
            .map(|t| t as u32)
            .or_else(|| {
                origin
                    .and_then(|e| world.get::<PerceptionStimulus>(e))
                    .map(|s| s.team)
            })
            .unwrap_or(0);
        let range = number(&payload, "range", 20.0) as f32;
        if !(range > 0.0 && range.is_finite()) {
            state.dropped_stimuli += 1;
            continue;
        }
        pending.push(PendingNoise {
            source: origin,
            position,
            loudness: (number(&payload, "loudness", 1.0) as f32).clamp(0.0, 1.0),
            range,
            team,
        });
    }
    if pending.len() > MAX_STIMULI_PER_TICK {
        state.dropped_stimuli += (pending.len() - MAX_STIMULI_PER_TICK) as u64;
        pending.truncate(MAX_STIMULI_PER_TICK);
    }
    input.noises = pending
        .into_iter()
        .map(|n| Noise {
            source: n.source.map(Entity::to_bits),
            position: n.position,
            loudness: n.loudness,
            range: n.range,
            team: n.team,
        })
        .collect();
}

fn gather_damages(world: &mut World, state: &mut Perceptions, input: &mut UpdateInput) {
    let mut pending: Vec<PendingDamage> = std::mem::take(&mut state.pending_damages);
    let delivered: Vec<(Option<Entity>, Option<Entity>, Value)> = world
        .get_resource::<DeliveredEvents>()
        .map(|d| {
            d.0.iter()
                .filter(|e| e.name == EVENT_DAMAGE)
                .map(|e| (e.source, e.target, e.payload.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (source, target, payload) in delivered {
        let Some(victim) = target else {
            state.dropped_stimuli += 1;
            continue;
        };
        pending.push(PendingDamage {
            victim,
            instigator: source,
            position: vector(&payload, "position"),
            amount: number(&payload, "amount", 1.0) as f32,
        });
    }
    if pending.len() > MAX_STIMULI_PER_TICK {
        state.dropped_stimuli += (pending.len() - MAX_STIMULI_PER_TICK) as u64;
        pending.truncate(MAX_STIMULI_PER_TICK);
    }
    for damage in pending {
        let position = damage
            .instigator
            .and_then(|e| position_of(world, e))
            .or(damage.position)
            .or_else(|| position_of(world, damage.victim));
        let Some(position) = position else {
            continue;
        };
        let team = damage
            .instigator
            .and_then(|e| world.get::<PerceptionStimulus>(e))
            .map_or(0, |s| s.team);
        input.damages.push(Damage {
            victim: damage.victim.to_bits(),
            instigator: damage.instigator.map(Entity::to_bits),
            position,
            amount: damage.amount,
            team,
        });
    }
}

/// Writes each sensor's best known target into the blackboards of the entity's
/// tree agents.
fn write_blackboards(
    world: &mut World,
    state: &Perceptions,
    sensors: &[(Entity, PerceptionSensor)],
) -> Result<()> {
    for (entity, sensor) in sensors {
        let Some(keys) = &sensor.blackboard else {
            continue;
        };
        let best = state.memory.best(entity.to_bits(), keys.affiliations);
        let target = best.and_then(|k| authored(world, k.subject));
        let writes: [(&Option<String>, Option<BbValue>); 3] = [
            (&keys.target, target.map(BbValue::Entity)),
            (&keys.position, best.map(|k| BbValue::Vec3(k.position))),
            (
                &keys.sensed,
                Some(BbValue::Bool(best.is_some_and(|k| k.sensed))),
            ),
        ];
        for (key, value) in writes {
            let Some(key) = key else {
                continue;
            };
            let apply = |board: &mut engine_ai::Blackboard| -> Result<()> {
                match &value {
                    Some(value) => board.set(key, value.clone()).map(|_| ()),
                    None => board.clear(key).map(|_| ()),
                }
                .map_err(|e| Error::new("perception.blackboard.invalid", format!("{key}: {e}")))
            };
            if let Some(mut trees) = world.get_resource_mut::<BehaviorTrees>()
                && let Some(board) = trees.blackboard_mut(*entity)
            {
                apply(board)?;
            }
            if let Some(mut trees) = world.get_resource_mut::<StateTrees>()
                && let Some(board) = trees.blackboard_mut(*entity)
            {
                apply(board)?;
            }
        }
    }
    Ok(())
}

fn emit(
    world: &mut World,
    state: &mut Perceptions,
    events: &[engine_ai::perception::PerceptionEvent],
) -> Result<()> {
    let mut per_perceiver: BTreeMap<Id, usize> = BTreeMap::new();
    let mut out: Vec<crate::EngineEvent> = Vec::new();
    let mut dropped = 0u64;
    for event in events {
        let count = per_perceiver.entry(event.perceiver).or_default();
        if *count >= MAX_EVENTS_PER_PERCEIVER || out.len() >= MAX_EVENTS_PER_TICK {
            dropped += 1;
            continue;
        }
        let perceiver = Entity::from_bits(event.perceiver);
        if world.get_entity(perceiver).is_err() {
            continue;
        }
        *count += 1;
        let name = match event.kind {
            EventKind::Sensed => EVENT_SENSED,
            EventKind::Lost => EVENT_LOST,
            EventKind::Forgotten => EVENT_FORGOTTEN,
        };
        let subject_entity = (event.subject != ANONYMOUS)
            .then(|| Entity::from_bits(event.subject))
            .filter(|e| world.get_entity(*e).is_ok());
        out.push(crate::EngineEvent {
            name: name.to_owned(),
            source: subject_entity,
            target: Some(perceiver),
            payload: json!({
                "subject": authored(world, event.subject),
                "sense": event.sense.as_str(),
                "position": event.position,
                "strength": event.strength,
            }),
        });
    }
    state.dropped_events += dropped;
    if out.is_empty() {
        return Ok(());
    }
    let count = out.len() as u64;
    match crate::emit_events(world, out) {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .diagnostics
                .first()
                .is_some_and(|d| d.code == "event.capacity") =>
        {
            state.dropped_events += count;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Telemetry for one memory record.
#[derive(Clone, Debug, Serialize)]
pub struct KnownStatus {
    /// Authored id of the subject; absent for anonymous noises and despawned entities.
    pub subject: Option<String>,
    pub sense: Sense,
    pub affiliation: Affiliation,
    pub position: [f32; 3],
    pub strength: f32,
    pub sensed: bool,
    pub age: f32,
}

/// Telemetry for one perceiver.
#[derive(Clone, Debug, Serialize)]
pub struct PerceiverStatus {
    pub entity: String,
    pub team: u32,
    pub known: usize,
    pub sensed: usize,
    pub best: Option<KnownStatus>,
    /// The first records by `(subject, sense)`, at most [`STATUS_MAX_RECORDS`].
    pub records: Vec<KnownStatus>,
}

/// The `perception` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct PerceptionStatus {
    /// Enabled sensors; the first [`STATUS_MAX_PERCEIVERS`] by authored id are listed.
    pub total: usize,
    pub stimuli: usize,
    pub perceivers: Vec<PerceiverStatus>,
    /// Sight rays cast and deferred by the latest tick.
    pub rays_cast: usize,
    pub rays_deferred: usize,
    /// Noises processed by the latest tick.
    pub noises: usize,
    /// Events dropped by the per-tick bounds or a full queue.
    pub dropped_events: u64,
    /// Noise and damage reports dropped by the per-tick bounds.
    pub dropped_stimuli: u64,
}

fn known_status(world: &World, known: Known) -> KnownStatus {
    KnownStatus {
        subject: authored(world, known.subject),
        sense: known.sense,
        affiliation: known.affiliation,
        position: known.position,
        strength: known.strength,
        sensed: known.sensed,
        age: known.age,
    }
}

/// Bounded telemetry; `None` when the world has no perception sensors.
pub fn status(world: &World) -> Option<PerceptionStatus> {
    let state = world.get_resource::<Perceptions>()?;
    let mut sensors: Vec<(String, Entity, u32)> = world
        .iter_entities()
        .filter_map(|e| {
            let sensor = e.get::<PerceptionSensor>()?;
            let id = e
                .get::<AuthoredId>()
                .map_or_else(|| format!("entity:{}", e.id().to_bits()), |a| a.id.clone());
            Some((id, e.id(), sensor.team))
        })
        .collect();
    if sensors.is_empty() {
        return None;
    }
    sensors.sort();
    let total = sensors.len();
    let perceivers = sensors
        .into_iter()
        .take(STATUS_MAX_PERCEIVERS)
        .map(|(entity_id, entity, team)| {
            let known = state.known(entity);
            PerceiverStatus {
                entity: entity_id,
                team,
                known: known.len(),
                sensed: known.iter().filter(|k| k.sensed).count(),
                best: state
                    .best(entity, AffiliationFilter::ALL)
                    .map(|k| known_status(world, k)),
                records: known
                    .into_iter()
                    .take(STATUS_MAX_RECORDS)
                    .map(|k| known_status(world, k))
                    .collect(),
            }
        })
        .collect();
    Some(PerceptionStatus {
        total,
        stimuli: world
            .iter_entities()
            .filter(|e| e.get::<PerceptionStimulus>().is_some_and(|s| s.enabled))
            .count(),
        perceivers,
        rays_cast: state.rays_cast,
        rays_deferred: state.rays_deferred,
        noises: state.noises,
        dropped_events: state.dropped_events,
        dropped_stimuli: state.dropped_stimuli,
    })
}
