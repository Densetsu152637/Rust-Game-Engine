//! Graceful degradation under pressure: a full event queue or a full body table drops work
//! in a bounded, counted way instead of failing the step.
//!
//! - **Events.** [`emit`] (and [`emit_named`]) enqueue a `physics.<feature>.<event>`. If the
//!   engine event queue (1024 events by default) is full, the event is dropped and counted.
//!   The next step reports the drops once with `physics.events.dropped`
//!   (`{dropped, total}`), delivered to the `engine.PhysicsSettings` entity when the scene has
//!   one and broadcast otherwise. Contact events are the bulk traffic, so they may use only
//!   `queued - queued / 8` slots of the queue within one physics frame: the remaining eighth
//!   (128 of 1024) is reserved for the rare, important events (`physics.joints.broken`,
//!   `physics.destruction.fractured`, ...), which are dropped only when the whole queue is full.
//! - **Solver proxies.** A cloth, soft body or fluid volume couples with at most
//!   `engine_xpbd::MAX_PROXIES` / `engine_fluids::MAX_PROXIES` rigid bodies near it. Beyond
//!   that the nearest ones (ties by entity id) are kept and the rest are left out for that
//!   step: each omission is counted ([`PressureStatus::proxies_omitted`]) and the next step
//!   announces the episode once with `physics.proxies.limited` (`{omitted, total}`); a frame
//!   without omissions ends the episode, so a new one is announced again.
//! - **Bodies.** A body that would exceed `SimulationConfig::body_capacity` is not inserted:
//!   its entity stays in the "starved" set and is retried every sync, so it simulates as soon
//!   as room appears. Ragdolls need room for all their bones at once. The next step reports
//!   newly starved entities once with `physics.capacity.exceeded`
//!   (`{capacity, starved, new}`).
//!
//! [`pressure_status`] reads the counters.
use super::{Feature, service};
use crate::Result;
use bevy_ecs::prelude::*;
use serde_json::json;

/// Event drop counters, kept in the world because `emit` only has `&mut World`.
#[derive(Resource, Default)]
pub(crate) struct EventPressure {
    total: u64,
    unreported: u64,
    /// Contact events queued by the current physics frame.
    contacts: usize,
    proxies_omitted: u64,
    proxies_unreported: u64,
    /// Omissions since the last report pass, and whether the condition was already announced
    /// (it announces once per episode, not once per step).
    proxies_recent: u64,
    proxies_announced: bool,
}

/// Contact events queued per physics frame before they are dropped to keep the rest of the
/// queue for the other physics events: all but one eighth of the queue.
fn contact_budget(world: &World) -> usize {
    let queued = world
        .get_resource::<crate::EventLimits>()
        .copied()
        .unwrap_or_default()
        .queued;
    queued - queued / 8
}

/// Capacity and event-queue pressure, from [`pressure_status`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PressureStatus {
    /// `SimulationConfig::body_capacity`, per dimension.
    pub body_capacity: usize,
    /// Bodies in the 2D and 3D engine-physics worlds (ragdoll bones included).
    pub bodies_2d: usize,
    pub bodies_3d: usize,
    /// Entities that have no engine-physics body (or ragdoll) because the table was full.
    pub starved_bodies: usize,
    /// Physics events dropped so far because the event queue was full.
    pub events_dropped: u64,
    /// Rigid bodies left out of a cloth, soft-body or fluid coupling so far because more
    /// bodies than the solver's proxy limit were near it (summed over objects and steps).
    pub proxies_omitted: u64,
}

/// Emits `physics.<feature>.<event>` to `target`; a full event queue drops the event.
pub(crate) fn emit(
    world: &mut World,
    feature: &dyn Feature,
    event: &str,
    target: Entity,
    source: Option<Entity>,
    payload: serde_json::Value,
) -> Result<()> {
    let name = format!("physics.{}.{event}", feature.name());
    emit_named(world, &name, target, source, payload)
}
fn queue_full(error: &crate::Error) -> bool {
    error
        .diagnostics
        .first()
        .is_some_and(|d| d.code == "event.capacity")
}
/// Emits the fully named event to `target`, counting a drop when the queue is full. Other
/// failures (an invalid name or payload) are real errors and propagate.
pub(crate) fn emit_named(
    world: &mut World,
    name: &str,
    target: Entity,
    source: Option<Entity>,
    payload: serde_json::Value,
) -> Result<()> {
    let contact = name.starts_with("physics.contact.");
    if contact {
        let budget = contact_budget(world);
        let mut pressure = world.get_resource_or_insert_with(EventPressure::default);
        if pressure.contacts >= budget {
            pressure.total += 1;
            pressure.unreported += 1;
            return Ok(());
        }
    }
    match super::super::emit(world, target, source, name, payload) {
        Err(error) if queue_full(&error) => {
            let mut pressure = world.get_resource_or_insert_with(EventPressure::default);
            pressure.total += 1;
            pressure.unreported += 1;
            Ok(())
        }
        other => {
            if contact && other.is_ok() {
                world.resource_mut::<EventPressure>().contacts += 1;
            }
            other
        }
    }
}

/// Counts rigid bodies left out of a deformable's coupling because of the solver's proxy limit.
pub(crate) fn note_proxies_omitted(world: &mut World, omitted: usize) {
    if omitted == 0 {
        return;
    }
    let mut pressure = world.get_resource_or_insert_with(EventPressure::default);
    pressure.proxies_omitted += omitted as u64;
    pressure.proxies_unreported += omitted as u64;
    pressure.proxies_recent += omitted as u64;
}
/// Delivers the pending drop warning, if any. Runs at the start of a step, when the previous
/// tick's events were already dispatched and the queue has room again.
pub(crate) fn report_events(world: &mut World) -> Result<()> {
    let Some(mut pressure) = world.get_resource_mut::<EventPressure>() else {
        return Ok(());
    };
    pressure.contacts = 0;
    let (total, unreported) = (pressure.total, pressure.unreported);
    let (omitted_total, omitted) = (pressure.proxies_omitted, pressure.proxies_unreported);
    let (recent, announced) = (pressure.proxies_recent, pressure.proxies_announced);
    pressure.proxies_recent = 0;
    if recent == 0 {
        pressure.proxies_announced = false;
    } else if !announced {
        let payload = json!({"omitted": omitted, "total": omitted_total});
        if warn(world, "physics.proxies.limited", payload)? {
            let mut pressure = world.resource_mut::<EventPressure>();
            pressure.proxies_unreported = 0;
            pressure.proxies_announced = true;
        }
    }
    if unreported == 0 {
        return Ok(());
    }
    let payload = json!({"dropped": unreported, "total": total});
    if warn(world, "physics.events.dropped", payload)? {
        world.resource_mut::<EventPressure>().unreported = 0;
    }
    Ok(())
}
/// Delivers `physics.capacity.exceeded` for entities that newly could not get a body.
pub(crate) fn report_capacity(
    world: &mut World,
    capacity: usize,
    starved: usize,
    new: usize,
) -> Result<bool> {
    let payload = json!({"capacity": capacity, "starved": starved, "new": new});
    warn(world, "physics.capacity.exceeded", payload)
}
/// Sends a warning to the settings entity (or to every bound entity without one). Returns
/// whether it was queued; a full queue keeps the warning pending.
fn warn(world: &mut World, name: &str, payload: serde_json::Value) -> Result<bool> {
    let target = world
        .iter_entities()
        .find(|entity| entity.contains::<super::PhysicsSettings>())
        .map(|entity| entity.id());
    let result = crate::emit_event(
        world,
        crate::EngineEvent {
            name: name.into(),
            source: None,
            target,
            payload,
        },
    );
    match result {
        Ok(()) => Ok(true),
        Err(error) if queue_full(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Capacity and event-queue pressure of the physics service.
pub fn pressure_status(world: &World) -> Result<PressureStatus> {
    let physics = service(world)?;
    Ok(PressureStatus {
        body_capacity: physics.config.body_capacity,
        bodies_2d: physics.worlds.two.len(),
        bodies_3d: physics.worlds.three.len(),
        starved_bodies: physics.worlds.starved.len(),
        events_dropped: world.get_resource::<EventPressure>().map_or(0, |p| p.total),
        proxies_omitted: world
            .get_resource::<EventPressure>()
            .map_or(0, |p| p.proxies_omitted),
    })
}
