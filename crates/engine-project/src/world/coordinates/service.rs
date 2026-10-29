//! The rebasing service: origin bookkeeping, the threshold check, the one shift that moves
//! every root `Transform` and simulated body, telemetry and the shift event.
use super::WorldOrigin;
use super::component::{AbsolutePosition, LargeWorld, MAX_ABSOLUTE};
use crate::runtime::{AuthoredId, RuntimeLog};
use crate::simulation::physics::{
    self, BodyState, Cloth, FluidVolume, Motion, PhysicsBody2D, PhysicsBody3D, Ragdoll, SoftBody,
};
use crate::{EngineEvent, Error, Parent, Result, Transform};
use bevy_ecs::prelude::*;
use glam::Vec3;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

/// Event raised after every origin shift. Payload: `delta` and `offset` (f64 metres, the new
/// offset minus the previous one and the new offset), `previous`, `shifts` (count so far) and
/// `cause` (`threshold`, `api` or `external`).
pub const EVENT_ORIGIN_SHIFTED: &str = "world.origin_shifted";
/// Most diagnostics kept in [`WorldOriginStatus`].
pub const MAX_DIAGNOSTICS: usize = 8;
const MAX_DIAGNOSTIC_CHARS: usize = 256;

/// Why the origin moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShiftCause {
    /// The `engine.LargeWorld` anchor passed the threshold.
    Threshold,
    /// [`rebase_now`](super::rebase_now) was called.
    Api,
    /// [`WorldOrigin`] was written directly; the next tick reconciled the world with it.
    External,
}

/// What one shift did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShiftReport {
    pub previous: [f64; 3],
    pub offset: [f64; 3],
    /// `offset - previous`; every root `Transform` moved by `-delta`.
    pub delta: [f64; 3],
    /// Root entities whose `Transform` moved.
    pub roots: usize,
    /// Dynamic and kinematic bodies teleported through `set_body_pose`.
    pub bodies: usize,
}

/// Telemetry of the coordinates service, also in `RuntimeSnapshot::world_origin`. Systems that
/// cache world-space positions compare the offset they cached with [`WorldOrigin`] (or watch
/// `shifts`) and re-express their data by `cached offset - current offset`.
#[derive(Resource, Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorldOriginStatus {
    /// Current `WorldOrigin.offset`.
    pub offset: [f64; 3],
    /// Shifts since the runtime started.
    pub shifts: u64,
    /// Delta of the last shift.
    pub last_delta: [f64; 3],
    /// Coordinates tick (1 = the first tick) of the last shift.
    pub last_shift_tick: Option<u64>,
    pub last_cause: Option<ShiftCause>,
    /// Authored id of the `engine.LargeWorld` entity in use.
    pub settings: Option<String>,
    /// Authored id of the resolved anchor.
    pub anchor: Option<String>,
    /// Largest per-axis distance of the anchor from the origin at the last check, in metres.
    pub anchor_distance: f64,
    /// Why a due shift did not happen (for example physics state the public API cannot move).
    pub blocked: Option<String>,
    pub diagnostics: Vec<String>,
}

/// Internal bookkeeping: the offset the local transforms are expressed in.
#[derive(Resource, Default)]
pub(super) struct Coordinates {
    /// `None` until the first tick or API call latches the initial origin.
    applied: Option<[f64; 3]>,
    ticks: u64,
}

pub(super) fn install(world: &mut World) {
    world.init_resource::<Coordinates>();
    world.init_resource::<WorldOriginStatus>();
}

pub(super) fn current_origin(world: &World) -> [f64; 3] {
    world
        .get_resource::<WorldOrigin>()
        .map_or([0.0; 3], |o| o.offset)
}

/// Offset of the coordinate frame that current local transforms actually use.
pub(super) fn render_origin(world: &World) -> [f64; 3] {
    world
        .get_resource::<Coordinates>()
        .and_then(|state| state.applied)
        .unwrap_or_else(|| current_origin(world))
}

/// The offset local transforms are expressed in, latching the current one the first time.
pub(super) fn applied_origin(world: &mut World) -> [f64; 3] {
    let current = current_origin(world);
    if !world.contains_resource::<Coordinates>() {
        world.init_resource::<Coordinates>();
    }
    *world
        .resource_mut::<Coordinates>()
        .applied
        .get_or_insert(current)
}

fn note(world: &mut World, message: String) {
    let mut message = message;
    if message.chars().count() > MAX_DIAGNOSTIC_CHARS {
        message = message.chars().take(MAX_DIAGNOSTIC_CHARS - 1).collect();
        message.push('…');
    }
    let mut status = world.resource_mut::<WorldOriginStatus>();
    if status.diagnostics.last() == Some(&message) {
        return;
    }
    if status.diagnostics.len() >= MAX_DIAGNOSTICS {
        status.diagnostics.remove(0);
    }
    status.diagnostics.push(message.clone());
    if let Some(mut log) = world.get_resource_mut::<RuntimeLog>() {
        log.0.push(format!("coordinates: {message}"));
    }
}

pub(super) fn pre_simulation(world: &mut World) -> Result<()> {
    if !world.contains_resource::<WorldOriginStatus>() {
        install(world);
    }
    world.resource_mut::<Coordinates>().ticks += 1;
    let applied = applied_origin(world);
    let current = current_origin(world);
    if applied != current {
        if let Err(error) = shift(world, applied, current, ShiftCause::External) {
            // The world cannot follow the write: keep the origin the transforms are in.
            world.resource_mut::<WorldOrigin>().offset = applied;
            world.resource_mut::<WorldOriginStatus>().blocked = Some(error.to_string());
            note(world, format!("direct WorldOrigin write reverted: {error}"));
        }
    } else {
        auto_rebase(world)?;
    }
    world.resource_mut::<WorldOriginStatus>().offset = current_origin(world);
    Ok(())
}

/// The `engine.LargeWorld` in use (lowest authored id) and how many there are.
fn settings(world: &mut World) -> Option<(String, LargeWorld, usize)> {
    let mut query = world.query::<(Entity, &LargeWorld, Option<&AuthoredId>)>();
    let mut found: Vec<(String, LargeWorld)> = query
        .iter(world)
        .map(|(entity, settings, id)| {
            let id = id.map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone());
            (id, settings.clone())
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let count = found.len();
    found.into_iter().next().map(|(id, s)| (id, s, count))
}

/// The anchor entity: the named one, or the active `Camera3D` with the lowest authored id.
fn anchor(world: &mut World, name: &str) -> Option<(String, Entity)> {
    if !name.is_empty() {
        return crate::services::find_entity(world, name).map(|e| (name.to_owned(), e));
    }
    let mut query = world.query::<(Entity, &crate::rendering3d::Camera3D, Option<&AuthoredId>)>();
    query
        .iter(world)
        .filter(|(_, camera, _)| camera.active)
        .map(|(entity, _, id)| {
            let id = id.map_or_else(|| entity.to_bits().to_string(), |a| a.id.clone());
            (id, entity)
        })
        .min_by(|a, b| a.0.cmp(&b.0))
}

fn auto_rebase(world: &mut World) -> Result<()> {
    let Some((id, settings, count)) = settings(world) else {
        let mut status = world.resource_mut::<WorldOriginStatus>();
        status.settings = None;
        status.anchor = None;
        status.blocked = None;
        return Ok(());
    };
    if count > 1 {
        note(
            world,
            format!("{count} engine.LargeWorld components; using `{id}`"),
        );
    }
    world.resource_mut::<WorldOriginStatus>().settings = Some(id.clone());
    if let Err(error) = settings.check() {
        note(world, format!("{id}: {error}"));
        return Ok(());
    }
    if !settings.enabled {
        world.resource_mut::<WorldOriginStatus>().blocked = None;
        return Ok(());
    }
    let Some((anchor_id, entity)) = anchor(world, &settings.anchor) else {
        world.resource_mut::<WorldOriginStatus>().anchor = None;
        if !settings.anchor.is_empty() {
            note(world, format!("anchor `{}` not found", settings.anchor));
        }
        return Ok(());
    };
    let offset = applied_origin(world);
    let Ok(absolute) = super::api::absolute_position(world, entity) else {
        note(
            world,
            format!("anchor `{anchor_id}` has no usable transform"),
        );
        return Ok(());
    };
    let distance = (0..3)
        .map(|i| (absolute[i] - offset[i]).abs())
        .fold(0.0, f64::max);
    {
        let mut status = world.resource_mut::<WorldOriginStatus>();
        status.anchor = Some(anchor_id);
        status.anchor_distance = distance;
    }
    if distance <= settings.threshold {
        world.resource_mut::<WorldOriginStatus>().blocked = None;
        return Ok(());
    }
    if let Some(reason) = unsupported(world) {
        if world.resource::<WorldOriginStatus>().blocked.as_ref() != Some(&reason) {
            note(world, format!("rebase deferred: {reason}"));
        }
        world.resource_mut::<WorldOriginStatus>().blocked = Some(reason);
        return Ok(());
    }
    let snap = f64::from(settings.snap);
    let target: [f64; 3] = std::array::from_fn(|i| (absolute[i] / snap).round() * snap);
    // A shift that cannot happen (an anchor beyond MAX_ABSOLUTE, unsupported physics state)
    // leaves the world where it is: report it, do not fail the tick.
    if let Err(error) = shift(world, offset, target, ShiftCause::Threshold) {
        let reason = error.to_string();
        if world.resource::<WorldOriginStatus>().blocked.as_ref() != Some(&reason) {
            note(world, format!("rebase failed: {reason}"));
        }
        world.resource_mut::<WorldOriginStatus>().blocked = Some(reason);
    }
    Ok(())
}

/// The first physics object whose state lives in physics-owned world space that the public
/// physics API cannot translate (ragdoll bone bodies, cloth, soft-body and fluid particles).
fn unsupported(world: &mut World) -> Option<String> {
    let mut query = world.query_filtered::<(Entity, Option<&AuthoredId>), Or<(
        With<Ragdoll>,
        With<Cloth>,
        With<SoftBody>,
        With<FluidVolume>,
    )>>();
    let (entity, id) = query.iter(world).next()?;
    let id = id.map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone());
    Some(format!(
        "`{id}` has ragdoll or deformable physics (engine.Ragdoll, Cloth, SoftBody or \
         FluidVolume), whose bones and particles live in physics world space that the public \
         physics API cannot translate"
    ))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("world.origin.invalid", message)
}

/// Dynamic and kinematic bodies with a synced simulation body, in entity order.
fn moving_bodies(world: &mut World) -> Vec<(Entity, BodyState)> {
    let mut query = world.query::<(Entity, Option<&PhysicsBody2D>, Option<&PhysicsBody3D>)>();
    let candidates: Vec<Entity> = query
        .iter(world)
        .filter(|(_, two, three)| {
            two.is_some_and(|b| b.motion != Motion::Fixed)
                || three.is_some_and(|b| b.motion != Motion::Fixed)
        })
        .map(|(entity, ..)| entity)
        .collect();
    candidates
        .into_iter()
        .filter_map(|entity| {
            physics::body_state(world, entity)
                .ok()
                .map(|state| (entity, state))
        })
        .collect()
}

/// Moves the origin from `from` to `to`: every root `Transform` moves by `from - to`, roots
/// with an exact `engine.AbsolutePosition` are recomputed from their f64 value, dynamic and
/// kinematic bodies are teleported with their velocities kept, world transforms are refreshed
/// and [`EVENT_ORIGIN_SHIFTED`] is emitted. Fixed bodies follow their `Transform` (the physics
/// sync rebuilds them). Fails without changing anything when `to` is out of range or physics
/// state that cannot be translated exists.
pub(super) fn shift(
    world: &mut World,
    from: [f64; 3],
    to: [f64; 3],
    cause: ShiftCause,
) -> Result<ShiftReport> {
    if to.iter().any(|v| !v.is_finite() || v.abs() > MAX_ABSOLUTE) {
        return Err(invalid(
            "origin offsets must be finite with |x|, |y|, |z| <= 1e12 metres",
        ));
    }
    let delta: [f64; 3] = std::array::from_fn(|i| to[i] - from[i]);
    let mut report = ShiftReport {
        previous: from,
        offset: to,
        delta,
        roots: 0,
        bodies: 0,
    };
    if from == to {
        return Ok(report);
    }
    if let Some(reason) = unsupported(world) {
        return Err(Error::new("world.origin.unsupported", reason));
    }
    let bodies = moving_bodies(world);
    // Roots first, so a teleported child body converts its pose against shifted parents.
    let roots: Vec<Entity> = world
        .query_filtered::<Entity, (With<Transform>, Without<Parent>)>()
        .iter(world)
        .collect();
    let target = WorldOrigin { offset: to };
    let mut exact = BTreeMap::new();
    let mut rebased = crate::sequencer::RootRebases::new();
    for &entity in &roots {
        let local = world.get::<Transform>(entity).map(|t| t.position);
        let Some(local) = local else { continue };
        let absolute_before = world.get::<AbsolutePosition>(entity).cloned();
        let moved = match world.get::<AbsolutePosition>(entity) {
            Some(absolute) if absolute.exact_for(local) => {
                let residual = absolute.residual_for(local);
                let moved: [f64; 3] =
                    std::array::from_fn(|i| absolute.position[i] - to[i] + residual[i]);
                if moved
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > f64::from(f32::MAX))
                {
                    return Err(invalid("Root position is outside finite local range"));
                }
                let moved = moved.map(|v| v as f32);
                exact.insert(entity, moved);
                moved
            }
            _ => std::array::from_fn(|i| (f64::from(local[i]) - delta[i]) as f32),
        };
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = moved;
            rebased.insert(
                entity,
                crate::sequencer::RootRebase {
                    before: local,
                    after: moved,
                    absolute_before,
                    absolute_after: None,
                },
            );
        }
        report.roots += 1;
    }
    for (entity, state) in bodies {
        let position = match exact.get(&entity) {
            Some(local) => Vec3::from(*local),
            None => Vec3::from(std::array::from_fn::<f32, 3, _>(|i| {
                (f64::from(state.position[i]) - delta[i]) as f32
            })),
        };
        match physics::set_body_pose(world, entity, position, state.rotation, None) {
            Ok(()) => report.bodies += 1,
            Err(error) => note(world, format!("body {}: {error}", entity.to_bits())),
        }
    }
    for &entity in &roots {
        let Some(local) = world.get::<Transform>(entity).map(|t| t.position) else {
            continue;
        };
        let keep = exact.contains_key(&entity);
        if let Some(mut absolute) = world.get_mut::<AbsolutePosition>(entity) {
            if !keep {
                absolute.position = target.absolute(local);
                absolute.mark_local(local, to);
            } else {
                let residual = rebased
                    .get(&entity)
                    .and_then(|rebase| {
                        rebase
                            .absolute_before
                            .as_ref()
                            .map(|before| before.residual_for(rebase.before))
                    })
                    .unwrap_or([0.; 3]);
                absolute.mark_applied(local, residual);
            }
        }
        if let Some(rebase) = rebased.get_mut(&entity) {
            rebase.absolute_after = world.get::<AbsolutePosition>(entity).cloned();
        }
    }
    world.resource_mut::<WorldOrigin>().offset = to;
    world.resource_mut::<Coordinates>().applied = Some(to);
    crate::sequencer::origin_shifted(world, from, to, &rebased);
    crate::transforms::refresh_world_transforms(world)?;
    let tick = world.resource::<Coordinates>().ticks;
    let shifts = {
        let mut status = world.resource_mut::<WorldOriginStatus>();
        status.offset = to;
        status.shifts += 1;
        status.last_delta = delta;
        status.last_shift_tick = Some(tick);
        status.last_cause = Some(cause);
        status.blocked = None;
        status.shifts
    };
    let event = EngineEvent {
        name: EVENT_ORIGIN_SHIFTED.into(),
        source: None,
        target: None,
        payload: json!({
            "delta": delta,
            "offset": to,
            "previous": from,
            "shifts": shifts,
            "cause": cause,
        }),
    };
    if let Err(error) = crate::emit_events(world, vec![event]) {
        note(world, format!("origin shift event dropped: {error}"));
    }
    Ok(report)
}

/// `post_transforms`: an entity that moved since its f64 value was derived gets the value
/// `offset + local`, so readers of the component see where it is now.
pub(super) fn sync_absolute(world: &mut World) {
    let origin = WorldOrigin {
        offset: current_origin(world),
    };
    let mut query = world.query_filtered::<(&Transform, &mut AbsolutePosition), Without<Parent>>();
    for (transform, mut absolute) in query.iter_mut(world) {
        if absolute.applied.is_some() && !absolute.exact_for(transform.position) {
            absolute.position = origin.absolute(transform.position);
            absolute.mark_local(transform.position, origin.offset);
        }
    }
}
