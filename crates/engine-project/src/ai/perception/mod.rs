//! AI perception for the runtime.
//!
//! * `engine.PerceptionSensor` ([`PerceptionSensor`]): sight, hearing and damage
//!   senses, a team, and optional blackboard keys kept current on the entity's
//!   behavior tree or state tree agent.
//! * `engine.PerceptionStimulus`: what sight can see (team, aim height).
//! * `engine.PerceptionNoise`: a periodic noise at the entity position.
//! * `engine.PerceptionSettings`: the team relation table, the ray budget and the
//!   occlusion layers (one per scene).
//! * Natives: [`report_noise`], [`report_damage`], [`register_hearing_attenuation`];
//!   engine events `perception.noise` and `perception.damage` do the same.
//! * `perception.sensed` / `perception.lost` / `perception.forgotten` events and the
//!   `perception` telemetry of the runtime snapshot.
//!
//! The service runs before the tree services in the AI step, so a tree ticks with
//! the blackboard its sensor just updated. See `docs/ai-perception.md`.
use crate::{Error, Project, Result, Scene};
use engine_core::EngineApp;

mod component;
mod host;
mod runtime;

pub use component::{
    MAX_RAYS_PER_TICK, MAX_RELATIONS, Occlusion, PERCEPTION_NOISE, PERCEPTION_SENSOR,
    PERCEPTION_SETTINGS, PERCEPTION_STIMULUS, PerceptionNoise, PerceptionSensor,
    PerceptionSettings, PerceptionStimulus, SensorBlackboard, TeamRelation, descriptors,
};
pub use host::HearingHook;
pub use runtime::{
    EVENT_DAMAGE, EVENT_FORGOTTEN, EVENT_LOST, EVENT_NOISE, EVENT_SENSED, KnownStatus,
    MAX_EVENTS_PER_PERCEIVER, MAX_EVENTS_PER_TICK, MAX_PENDING, MAX_PERCEIVERS, MAX_STIMULI,
    MAX_STIMULI_PER_TICK, PerceiverStatus, PerceptionStatus, Perceptions, STATUS_MAX_PERCEIVERS,
    STATUS_MAX_RECORDS, prepare, report_damage, report_noise, status, step,
};

/// Registers the component adapters and the runtime service.
pub fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<PerceptionSensor>(app, PERCEPTION_SENSOR, 1)?;
    crate::register_component::<PerceptionStimulus>(app, PERCEPTION_STIMULUS, 1)?;
    crate::register_component::<PerceptionNoise>(app, PERCEPTION_NOISE, 1)?;
    crate::register_component::<PerceptionSettings>(app, PERCEPTION_SETTINGS, 1)?;
    app.world_mut().insert_resource(Perceptions::default());
    Ok(())
}

/// Registers the hearing attenuation hook: called for every noise a listener could
/// hear, it returns the factor in `[0, 1]` applied to the noise's loudness. This is
/// the seam for occlusion-aware hearing (for example from acoustics analysis).
///
/// # Errors
/// A second registration, or an `app` that is not a project runtime.
pub fn register_hearing_attenuation(app: &mut EngineApp, hook: HearingHook) -> Result<()> {
    runtime::set_hearing_hook(app.world_mut(), hook)
}

fn invalid(entity: &str, error: Error) -> Error {
    let message = error
        .diagnostics
        .first()
        .map_or_else(|| error.to_string(), |d| d.message.clone());
    Error::new(
        "perception.value.invalid",
        format!("entity {entity}: {message}"),
    )
}

/// Scene-level checks: component values are within bounds, at most one settings
/// entity exists, and sensor blackboard keys name variables of the right type in
/// the graph of the entity's behavior tree or state tree agent.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    let mut settings = 0;
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if let Some(value) = custom.get(PERCEPTION_SETTINGS) {
            settings += 1;
            let parsed: PerceptionSettings = serde_json::from_value(value.data.clone())?;
            parsed.check()?;
        }
        if let Some(value) = custom.get(PERCEPTION_STIMULUS) {
            let parsed: PerceptionStimulus = serde_json::from_value(value.data.clone())?;
            parsed.check().map_err(|e| invalid(&entity.id, e))?;
        }
        if let Some(value) = custom.get(PERCEPTION_NOISE) {
            let parsed: PerceptionNoise = serde_json::from_value(value.data.clone())?;
            parsed.check().map_err(|e| invalid(&entity.id, e))?;
        }
        let Some(value) = custom.get(PERCEPTION_SENSOR) else {
            continue;
        };
        let sensor: PerceptionSensor = serde_json::from_value(value.data.clone())?;
        sensor.check().map_err(|e| invalid(&entity.id, e))?;
        let Some(keys) = &sensor.blackboard else {
            continue;
        };
        let graphs: Vec<String> = ["engine.BehaviorTreeAgent", "engine.StateTreeAgent"]
            .into_iter()
            .filter_map(|name| custom.get(name))
            .filter_map(|v| v.data.get("graph")?.as_str().map(str::to_owned))
            .collect();
        if graphs.is_empty() {
            return Err(Error::new(
                "perception.blackboard.agent",
                format!(
                    "entity {}: sensor blackboard keys need an engine.BehaviorTreeAgent or engine.StateTreeAgent",
                    entity.id
                ),
            ));
        }
        for graph in &graphs {
            let document = crate::graphs::load_graph(project, graph)?;
            let checks = [
                (&keys.target, engine_graph::PortType::Entity),
                (&keys.position, engine_graph::PortType::Vec3),
                (&keys.sensed, engine_graph::PortType::Bool),
            ];
            for (key, ty) in checks {
                let Some(key) = key else {
                    continue;
                };
                if document.variable(key).map(|v| &v.ty) != Some(&ty) {
                    return Err(Error::new(
                        "perception.blackboard.invalid",
                        format!(
                            "entity {}: graph {graph} needs a {ty:?} variable named {key:?}",
                            entity.id
                        ),
                    ));
                }
            }
        }
    }
    if settings > 1 {
        return Err(Error::new(
            "perception.settings.duplicate",
            "at most one entity may carry engine.PerceptionSettings",
        ));
    }
    Ok(())
}
