//! Authored raycast vehicles (`engine.Vehicle3D`) driven by engine-physics' vehicle model.
//!
//! The component sits on an entity that also carries a **dynamic** `engine.PhysicsBody3D`
//! (the chassis; its collider, mass and inertia are the vehicle's). It lists the wheels
//! (suspension, brakes, steering, drive flags), the tyre, engine, gearbox and differential
//! settings and an input mapping. Every physics step the feature copies the effective
//! controls into engine-physics: the direct fields (`throttle`, `brake`, `steer`,
//! `handbrake`, written by scripts, behaviours or MCP) combined with the mapped gameplay
//! actions from the `ActionFrame` resource. Wheel poses are written to the optional per-wheel
//! `entity` references after the step. See `docs/physics-vehicles.md`.
use super::super::{invalid, mapped, world_pose, write_world_pose};
use super::{Context, Feature, Frame, Resolved, Worlds, emit, service, service_mut};
use crate::{ActionFrame, Result};
use bevy_ecs::prelude::*;
use engine_physics::dim3;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Most wheels per vehicle.
pub const MAX_WHEELS: usize = dim3::MAX_WHEELS;

/// One pacejka axis (`sin(C atan(B x - E (B x - atan(B x))))`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CurveSettings {
    pub b: f32,
    pub c: f32,
    pub e: f32,
}
impl From<dim3::PacejkaAxis> for CurveSettings {
    fn from(a: dim3::PacejkaAxis) -> Self {
        Self {
            b: a.b,
            c: a.c,
            e: a.e,
        }
    }
}
impl Default for CurveSettings {
    fn default() -> Self {
        dim3::TyreDesc::default().longitudinal.into()
    }
}
impl From<CurveSettings> for dim3::PacejkaAxis {
    fn from(c: CurveSettings) -> Self {
        Self {
            b: c.b,
            c: c.c,
            e: c.e,
        }
    }
}

/// Slip-based tyre model shared by all wheels (see `docs/physics-vehicles.md#tyre-model`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TyreSettings {
    pub longitudinal: CurveSettings,
    pub lateral: CurveSettings,
    pub friction: f32,
    pub lateral_scale: f32,
    pub nominal_load: f32,
    pub load_sensitivity: f32,
    pub reference_surface_friction: f32,
    pub low_speed: f32,
}
impl Default for TyreSettings {
    fn default() -> Self {
        let d = dim3::TyreDesc::default();
        Self {
            longitudinal: d.longitudinal.into(),
            lateral: d.lateral.into(),
            friction: d.friction,
            lateral_scale: d.lateral_scale,
            nominal_load: d.nominal_load,
            load_sensitivity: d.load_sensitivity,
            reference_surface_friction: d.reference_surface_friction,
            low_speed: d.low_speed,
        }
    }
}
impl From<TyreSettings> for dim3::TyreDesc {
    fn from(t: TyreSettings) -> Self {
        Self {
            longitudinal: t.longitudinal.into(),
            lateral: t.lateral.into(),
            friction: t.friction,
            lateral_scale: t.lateral_scale,
            nominal_load: t.nominal_load,
            load_sensitivity: t.load_sensitivity,
            reference_surface_friction: t.reference_surface_friction,
            low_speed: t.low_speed,
        }
    }
}

/// Engine torque curve and limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineSettings {
    /// `[rpm, torque in N m]` samples at full throttle, ascending in rpm.
    pub torque_curve: Vec<[f32; 2]>,
    pub idle_rpm: f32,
    pub max_rpm: f32,
    pub engine_brake: f32,
}
impl Default for EngineSettings {
    fn default() -> Self {
        let d = dim3::EngineDesc::default();
        Self {
            torque_curve: d.torque_curve,
            idle_rpm: d.idle_rpm,
            max_rpm: d.max_rpm,
            engine_brake: d.engine_brake,
        }
    }
}

/// Who changes gear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GearMode {
    #[default]
    Automatic,
    Manual,
}

/// Gear ratios and automatic shifting thresholds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GearboxSettings {
    pub mode: GearMode,
    pub forward: Vec<f32>,
    pub reverse: f32,
    pub final_drive: f32,
    pub efficiency: f32,
    pub up_shift_rpm: f32,
    pub down_shift_rpm: f32,
    pub shift_time: f32,
}
impl Default for GearboxSettings {
    fn default() -> Self {
        let d = dim3::GearboxDesc::default();
        Self {
            mode: GearMode::Automatic,
            forward: d.forward,
            reverse: d.reverse,
            final_drive: d.final_drive,
            efficiency: d.efficiency,
            up_shift_rpm: d.up_shift_rpm,
            down_shift_rpm: d.down_shift_rpm,
            shift_time: d.shift_time,
        }
    }
}

/// Differential type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferentialKind {
    #[default]
    Open,
    Locked,
    LimitedSlip,
}
/// How drive torque is split between driven wheels. `preload`, `viscous` and `max_torque`
/// only apply to `limited_slip`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DifferentialSettings {
    pub kind: DifferentialKind,
    pub preload: f32,
    pub viscous: f32,
    pub max_torque: f32,
}
impl Default for DifferentialSettings {
    fn default() -> Self {
        Self {
            kind: DifferentialKind::Open,
            preload: 50.,
            viscous: 10.,
            max_torque: 400.,
        }
    }
}

/// Steering limits (see `docs/physics-vehicles.md#steering`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SteeringSettings {
    pub max_angle: f32,
    pub reduction_speed: f32,
    pub min_factor: f32,
    pub rate: f32,
}
impl Default for SteeringSettings {
    fn default() -> Self {
        let d = dim3::SteeringDesc::default();
        Self {
            max_angle: d.max_angle,
            reduction_speed: d.reduction_speed,
            min_factor: d.min_factor,
            rate: d.rate,
        }
    }
}

/// Anti-roll bar joining two wheels by index.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AntiRollSettings {
    pub wheel_a: u32,
    pub wheel_b: u32,
    pub stiffness: f32,
}
impl Default for AntiRollSettings {
    fn default() -> Self {
        Self {
            wheel_a: 0,
            wheel_b: 1,
            stiffness: 20_000.,
        }
    }
}

/// Suspension probe shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CastKind {
    #[default]
    Ray,
    Sphere,
}

/// One wheel. Vectors are in the chassis frame, lengths in meters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WheelSettings {
    pub mount: [f32; 3],
    pub suspension_dir: [f32; 3],
    pub axle: [f32; 3],
    pub radius: f32,
    pub rest_length: f32,
    pub max_travel: f32,
    pub stiffness: f32,
    pub damping_compression: f32,
    pub damping_relaxation: f32,
    pub max_force: f32,
    pub inertia: f32,
    pub steer: bool,
    pub drive: bool,
    pub brake: bool,
    pub handbrake: bool,
    pub brake_torque: f32,
    pub handbrake_torque: f32,
    /// Entity id of a visual that receives this wheel's pose (position, steer and spin)
    /// after every physics step. Usually a child of the chassis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}
impl Default for WheelSettings {
    fn default() -> Self {
        let d = dim3::WheelDesc::default();
        Self {
            mount: d.mount,
            suspension_dir: d.suspension_dir,
            axle: d.axle,
            radius: d.radius,
            rest_length: d.rest_length,
            max_travel: d.max_travel,
            stiffness: d.stiffness,
            damping_compression: d.damping_compression,
            damping_relaxation: d.damping_relaxation,
            max_force: d.max_force,
            inertia: d.inertia,
            steer: d.steer,
            drive: d.drive,
            brake: d.brake,
            handbrake: d.handbrake,
            brake_torque: d.brake_torque,
            handbrake_torque: d.handbrake_torque,
            entity: None,
        }
    }
}
impl From<&WheelSettings> for dim3::WheelDesc {
    fn from(w: &WheelSettings) -> Self {
        Self {
            mount: w.mount,
            suspension_dir: w.suspension_dir,
            axle: w.axle,
            radius: w.radius,
            rest_length: w.rest_length,
            max_travel: w.max_travel,
            stiffness: w.stiffness,
            damping_compression: w.damping_compression,
            damping_relaxation: w.damping_relaxation,
            max_force: w.max_force,
            inertia: w.inertia,
            steer: w.steer,
            drive: w.drive,
            brake: w.brake,
            handbrake: w.handbrake,
            brake_torque: w.brake_torque,
            handbrake_torque: w.handbrake_torque,
        }
    }
}

/// Gameplay action names that drive the vehicle; an empty name is unbound. Pedals and the two
/// steering actions read the action's analog value (actions are `0..=1`; a stick axis is bound
/// to a positive and a negative action) or 1 while held; the shift actions fire on the press.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputMapping {
    pub throttle: String,
    pub brake: String,
    pub steer_left: String,
    pub steer_right: String,
    pub handbrake: String,
    pub shift_up: String,
    pub shift_down: String,
}
impl Default for InputMapping {
    fn default() -> Self {
        let name = |s: &str| s.to_owned();
        Self {
            throttle: name("throttle"),
            brake: name("brake"),
            steer_left: name("steer_left"),
            steer_right: name("steer_right"),
            handbrake: name("handbrake"),
            shift_up: name("shift_up"),
            shift_down: name("shift_down"),
        }
    }
}

fn default_wheels() -> Vec<WheelSettings> {
    let wheel = |x: f32, z: f32, steer: bool, drive: bool| WheelSettings {
        mount: [x, 0., z],
        steer,
        drive,
        ..Default::default()
    };
    vec![
        wheel(-0.8, 1.3, true, false),
        wheel(0.8, 1.3, true, false),
        wheel(-0.8, -1.3, false, true),
        wheel(0.8, -1.3, false, true),
    ]
}

/// Authored raycast vehicle. Requires a dynamic `engine.PhysicsBody3D` on the same entity.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Vehicle3D {
    pub wheels: Vec<WheelSettings>,
    pub tyre: TyreSettings,
    pub engine: EngineSettings,
    pub gearbox: GearboxSettings,
    pub differential: DifferentialSettings,
    pub steering: SteeringSettings,
    pub anti_roll: Vec<AntiRollSettings>,
    pub suspension_cast: CastKind,
    /// Vehicle forward direction in the chassis frame.
    pub forward: [f32; 3],
    pub drag: f32,
    pub downforce: f32,
    pub rolling_resistance: f32,
    /// Set the wheels spinning at the chassis speed when the vehicle is created, so a vehicle
    /// spawned with a velocity does not start with locked tyres.
    pub spawn_rolling: bool,
    pub input: InputMapping,
    /// Direct controls (`0..=1`), combined with the mapped actions by taking the larger.
    pub throttle: f32,
    pub brake: f32,
    pub handbrake: f32,
    /// Direct steering (`-1..=1`, positive turns right), added to the mapped steering.
    pub steer: f32,
    /// Increment to request one gear up / down (manual gearbox).
    pub shift_up: u64,
    pub shift_down: u64,
}
impl Default for Vehicle3D {
    fn default() -> Self {
        Self {
            wheels: default_wheels(),
            tyre: TyreSettings::default(),
            engine: EngineSettings::default(),
            gearbox: GearboxSettings::default(),
            differential: DifferentialSettings::default(),
            steering: SteeringSettings::default(),
            anti_roll: vec![],
            suspension_cast: CastKind::Ray,
            forward: [0., 0., 1.],
            drag: 0.4,
            downforce: 0.,
            rolling_resistance: 0.012,
            spawn_rolling: false,
            input: InputMapping::default(),
            throttle: 0.,
            brake: 0.,
            handbrake: 0.,
            steer: 0.,
            shift_up: 0,
            shift_down: 0,
        }
    }
}
impl Vehicle3D {
    fn desc(&self) -> Result<dim3::VehicleDesc> {
        if self.wheels.len() > MAX_WHEELS {
            return Err(invalid("A vehicle has at most 16 wheels"));
        }
        let differential = match self.differential.kind {
            DifferentialKind::Open => dim3::DifferentialDesc::Open,
            DifferentialKind::Locked => dim3::DifferentialDesc::Locked,
            DifferentialKind::LimitedSlip => dim3::DifferentialDesc::LimitedSlip {
                preload: self.differential.preload,
                viscous: self.differential.viscous,
                max_torque: self.differential.max_torque,
            },
        };
        let desc = dim3::VehicleDesc {
            wheels: self.wheels.iter().map(Into::into).collect(),
            tyre: self.tyre.into(),
            engine: dim3::EngineDesc {
                torque_curve: self.engine.torque_curve.clone(),
                idle_rpm: self.engine.idle_rpm,
                max_rpm: self.engine.max_rpm,
                engine_brake: self.engine.engine_brake,
            },
            gearbox: dim3::GearboxDesc {
                forward: self.gearbox.forward.clone(),
                reverse: self.gearbox.reverse,
                final_drive: self.gearbox.final_drive,
                efficiency: self.gearbox.efficiency,
                mode: match self.gearbox.mode {
                    GearMode::Automatic => dim3::GearboxMode::Automatic,
                    GearMode::Manual => dim3::GearboxMode::Manual,
                },
                up_shift_rpm: self.gearbox.up_shift_rpm,
                down_shift_rpm: self.gearbox.down_shift_rpm,
                shift_time: self.gearbox.shift_time,
            },
            differential,
            steering: dim3::SteeringDesc {
                max_angle: self.steering.max_angle,
                reduction_speed: self.steering.reduction_speed,
                min_factor: self.steering.min_factor,
                rate: self.steering.rate,
            },
            anti_roll: self
                .anti_roll
                .iter()
                .map(|bar| dim3::AntiRollBar {
                    wheel_a: bar.wheel_a as usize,
                    wheel_b: bar.wheel_b as usize,
                    stiffness: bar.stiffness,
                })
                .collect(),
            cast: match self.suspension_cast {
                CastKind::Ray => dim3::SuspensionCast::Ray,
                CastKind::Sphere => dim3::SuspensionCast::Sphere,
            },
            forward: self.forward,
            drag: self.drag,
            downforce: self.downforce,
            rolling_resistance: self.rolling_resistance,
        };
        desc.validate().map_err(mapped)
    }
    /// The direct controls as engine-physics controls.
    fn direct(&self) -> Result<dim3::VehicleControls> {
        dim3::VehicleControls {
            throttle: self.throttle,
            brake: self.brake,
            steer: self.steer,
            handbrake: self.handbrake,
        }
        .validate()
        .map_err(|_| invalid("Vehicle throttle, brake and handbrake are 0..1, steer is -1..1"))
    }
}

/// Analog value of `action`: its axis value when it has one, else 1 while held.
fn action_value(frame: Option<&ActionFrame>, action: &str) -> f32 {
    let Some(frame) = frame.filter(|_| !action.is_empty()) else {
        return 0.;
    };
    match frame.values.get(action) {
        Some(value) if value.is_finite() => *value,
        _ => f32::from(u8::from(frame.held(action))),
    }
}
/// Direct controls combined with the mapped actions.
fn effective_controls(
    config: &Vehicle3D,
    frame: Option<&ActionFrame>,
) -> Result<dim3::VehicleControls> {
    let direct = config.direct()?;
    let map = &config.input;
    let pedal = |name: &str, direct: f32| direct.max(action_value(frame, name).clamp(0., 1.));
    let steer = action_value(frame, &map.steer_right).clamp(0., 1.)
        - action_value(frame, &map.steer_left).clamp(0., 1.);
    Ok(dim3::VehicleControls {
        throttle: pedal(&map.throttle, direct.throttle),
        brake: pedal(&map.brake, direct.brake),
        handbrake: pedal(&map.handbrake, direct.handbrake),
        steer: (direct.steer + steer).clamp(-1., 1.),
    })
}

/// What the feature last pushed into engine-physics for one entity.
struct Link {
    desc: dim3::VehicleDesc,
    shift_up: u64,
    shift_down: u64,
}
pub(super) struct Vehicles {
    links: BTreeMap<Entity, Link>,
}
impl Vehicles {
    pub(super) fn new() -> Self {
        Self {
            links: BTreeMap::new(),
        }
    }
}

fn check_scene(world: &World) -> Result<()> {
    for entity in world.iter_entities() {
        let Some(config) = entity.get::<Vehicle3D>() else {
            continue;
        };
        config.desc()?;
        config.direct()?;
        let Some(body) = entity.get::<super::PhysicsBody3D>() else {
            // A fractured chassis lost its body on purpose: the vehicle is inactive.
            if entity.contains::<super::Fractured>() {
                continue;
            }
            return Err(invalid("A vehicle requires an engine.PhysicsBody3D"));
        };
        if body.motion != super::Motion::Dynamic {
            return Err(invalid("A vehicle requires a dynamic physics body"));
        }
        if entity.contains::<super::CharacterController3D>() {
            return Err(invalid(
                "A vehicle cannot share an entity with a character controller",
            ));
        }
        for wheel in &config.wheels {
            if let Some(id) = wheel.entity.as_deref().filter(|id| !id.is_empty()) {
                let target = crate::services::find_entity(world, id)
                    .ok_or_else(|| invalid(format!("Unknown wheel entity {id}")))?;
                if target == entity.id() {
                    return Err(invalid("A wheel entity must differ from the chassis"));
                }
            }
        }
    }
    Ok(())
}

fn sync_vehicles(
    worlds: &mut Worlds,
    bodies: &super::Bodies,
    links: &mut BTreeMap<Entity, Link>,
    world: &World,
) -> Result<()> {
    let physics = &mut worlds.three;
    let stale: Vec<Entity> = links
        .keys()
        .filter(|e| world.get::<Vehicle3D>(**e).is_none())
        .copied()
        .collect();
    for entity in stale {
        physics.remove_vehicle(entity.to_bits());
        links.remove(&entity);
    }
    let frame = world.get_resource::<ActionFrame>();
    for entity in world.iter_entities() {
        let e = entity.id();
        let Some(config) = entity.get::<Vehicle3D>() else {
            continue;
        };
        if !bodies.three.contains_key(&e) {
            // No body (starved, or fractured): the vehicle waits, or ends with its chassis.
            if links.remove(&e).is_some() {
                physics.remove_vehicle(e.to_bits());
            }
            continue;
        }
        let id = e.to_bits();
        let desc = config.desc()?;
        if !physics.is_vehicle(id) {
            physics.insert_vehicle(id, desc.clone()).map_err(mapped)?;
            if config.spawn_rolling {
                physics.vehicle_roll_start(id).map_err(mapped)?;
            }
            links.insert(
                e,
                Link {
                    desc,
                    shift_up: config.shift_up,
                    shift_down: config.shift_down,
                },
            );
        } else if let Some(link) = links.get_mut(&e)
            && link.desc != desc
        {
            physics.set_vehicle_desc(id, desc.clone()).map_err(mapped)?;
            link.desc = desc;
        }
        let controls = effective_controls(config, frame)?;
        physics.set_vehicle_controls(id, controls).map_err(mapped)?;
        let Some(link) = links.get_mut(&e) else {
            continue;
        };
        let pressed = |name: &str| frame.is_some_and(|f| !name.is_empty() && f.pressed(name));
        if config.shift_up != link.shift_up || pressed(&config.input.shift_up) {
            link.shift_up = config.shift_up;
            physics.vehicle_shift_up(id).map_err(mapped)?;
        }
        if config.shift_down != link.shift_down || pressed(&config.input.shift_down) {
            link.shift_down = config.shift_down;
            physics.vehicle_shift_down(id).map_err(mapped)?;
        }
    }
    Ok(())
}

fn authored(world: &World, entity: Option<u64>) -> Value {
    entity
        .and_then(Entity::try_from_bits)
        .and_then(|e| world.get::<crate::AuthoredId>(e))
        .map_or(Value::Null, |id| json!(id.id))
}

impl Feature for Vehicles {
    fn name(&self) -> &'static str {
        "vehicles"
    }
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        descriptors()
    }
    fn register_components(&self, app: &mut engine_core::EngineApp) -> Result<()> {
        crate::register_component::<Vehicle3D>(app, "engine.Vehicle3D", 1)
    }
    fn validate(&self, world: &World, _settings: &Resolved) -> Result<()> {
        check_scene(world)
    }
    fn sync(&mut self, cx: Context<'_>, world: &World) -> Result<()> {
        sync_vehicles(cx.worlds, cx.bodies, &mut self.links, world)
    }
    fn after_substep(&mut self, cx: Context<'_>, world: &mut World, _dt: f32) -> Result<()> {
        for event in cx.worlds.three.drain_vehicle_events() {
            let Some(target) = Entity::try_from_bits(event.id) else {
                continue;
            };
            if world.get_entity(target).is_err() {
                continue;
            }
            let (name, payload) = match event.kind {
                dim3::VehicleEventKind::GearChanged { from, to } => {
                    ("gear_changed", json!({"from": from, "to": to, "gear": to}))
                }
                dim3::VehicleEventKind::WheelContact {
                    wheel,
                    started,
                    ground_body,
                } => (
                    "wheel_contact",
                    json!({
                        "wheel": wheel,
                        "phase": if started { "start" } else { "stop" },
                        "ground": authored(world, ground_body),
                    }),
                ),
                dim3::VehicleEventKind::Airborne => ("airborne", json!({})),
                dim3::VehicleEventKind::Landed { impact_speed } => {
                    ("landed", json!({"impact_speed": impact_speed}))
                }
            };
            emit(world, self, name, target, None, payload)?;
        }
        Ok(())
    }
    fn after_frame(&mut self, cx: Context<'_>, world: &mut World, _frame: Frame) -> Result<()> {
        // Wheel visuals: chassis world pose composed with the simulated wheel-local pose, so
        // the result also holds when the chassis `Transform` is interpolated. Runs every
        // frame, so visuals that are not children of the chassis follow it on frames without
        // a physics step too.
        let mut writes = Vec::new();
        for &entity in self.links.keys() {
            let Some(config) = world.get::<Vehicle3D>(entity) else {
                continue;
            };
            let Some(states) = cx.worlds.three.vehicle_wheel_states(entity.to_bits()) else {
                continue;
            };
            let Ok((_, chassis_rotation, chassis_position)) = world_pose(world, entity) else {
                continue;
            };
            for (wheel, state) in config.wheels.iter().zip(&states) {
                let Some(visual) = wheel
                    .entity
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .and_then(|id| crate::services::find_entity(world, id))
                else {
                    continue;
                };
                let rotation = chassis_rotation * Quat::from_array(state.local_rotation);
                let position =
                    chassis_position + chassis_rotation * Vec3::from(state.local_position);
                writes.push((visual, rotation, position));
            }
        }
        // Parents before children, like dynamic write-back.
        writes.sort_by_key(|(e, ..)| super::bodies::depth(world, *e));
        for (visual, rotation, position) in writes {
            let (scale, ..) = world_pose(world, visual)?;
            write_world_pose(world, visual, scale, rotation, position)?;
        }
        Ok(())
    }
}

fn descriptors() -> Vec<crate::ComponentDescriptor> {
    let vector =
        |n: usize| json!({"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n});
    let number = |min: f64, max: f64, doc: &str| json!({"type":"number","minimum":min,"maximum":max,"description":doc});
    let curve = |doc: &str| {
        json!({"type":"object","additionalProperties":false,"description":doc,"properties":{
            "b":number(0.1, 100., "Stiffness factor B."),
            "c":number(1.05, 2., "Shape factor C."),
            "e":number(-10., 1., "Curvature factor E.")}})
    };
    let tyre = json!({"type":"object","additionalProperties":false,"properties":{
        "longitudinal":curve("Pacejka axis over the slip ratio."),
        "lateral":curve("Pacejka axis over the slip angle (radians)."),
        "friction":number(0.05, 5., "Peak friction coefficient at the nominal load on the reference surface."),
        "lateral_scale":number(0.2, 2., "Lateral peak as a fraction of the longitudinal peak."),
        "nominal_load":number(1., 1e6, "Normal load in N at which friction applies."),
        "load_sensitivity":number(0., 1., "Fractional friction loss per unit of extra relative load."),
        "reference_surface_friction":number(0.01, 100., "Collider friction treated as the reference surface."),
        "low_speed":number(0.05, 10., "Speed below which slip is computed against this speed.")}});
    let engine = json!({"type":"object","additionalProperties":false,"properties":{
        "torque_curve":{"type":"array","minItems":2,"maxItems":dim3::MAX_TORQUE_SAMPLES,
            "items":vector(2),"description":"[rpm, N m] samples at full throttle, ascending in rpm."},
        "idle_rpm":number(100., 20_000., "Idle speed."),
        "max_rpm":number(200., 50_000., "Rev limiter speed."),
        "engine_brake":number(0., 1e5, "Closed-throttle drag torque in N m at max_rpm.")}});
    let gearbox = json!({"type":"object","additionalProperties":false,"properties":{
        "mode":{"type":"string","enum":["automatic","manual"]},
        "forward":{"type":"array","minItems":1,"maxItems":dim3::MAX_GEARS,
            "items":number(0.1, 30., "Forward gear ratio.")},
        "reverse":number(0.1, 30., "Reverse ratio (positive magnitude)."),
        "final_drive":number(0.1, 30., "Final drive ratio."),
        "efficiency":number(0.1, 1., "Driveline efficiency."),
        "up_shift_rpm":number(0., 50_000., "Automatic upshift speed."),
        "down_shift_rpm":number(0., 50_000., "Automatic downshift speed."),
        "shift_time":number(0., 5., "Seconds of torque cut per shift.")}});
    let differential = json!({"type":"object","additionalProperties":false,"properties":{
        "kind":{"type":"string","enum":["open","locked","limited_slip"]},
        "preload":number(0., 1e5, "limited_slip: constant transfer torque in N m."),
        "viscous":number(0., 1e5, "limited_slip: transfer N m per rad/s of speed difference."),
        "max_torque":number(0., 1e5, "limited_slip: largest transfer torque in N m.")}});
    let steering = json!({"type":"object","additionalProperties":false,"properties":{
        "max_angle":number(0., std::f64::consts::FRAC_PI_2, "Steer angle at full input, radians."),
        "reduction_speed":number(0., 1e4, "Speed at which the angle is reduced to min_factor; 0 disables."),
        "min_factor":number(0., 1., "Fraction of max_angle left at reduction_speed."),
        "rate":number(0.01, 100., "Largest steer angle change in rad/s.")}});
    let wheel = json!({"type":"object","additionalProperties":false,"properties":{
        "mount":vector(3),"suspension_dir":vector(3),"axle":vector(3),
        "radius":number(0.02, 2., "Wheel radius in meters."),
        "rest_length":number(0.01, 2., "Unloaded spring length."),
        "max_travel":number(0.001, 2., "Compression travel; at most rest_length."),
        "stiffness":number(100., 1e7, "Spring rate N/m."),
        "damping_compression":number(0., 1e6, "Damper N s/m while compressing."),
        "damping_relaxation":number(0., 1e6, "Damper N s/m while extending."),
        "max_force":number(1., 1e7, "Largest suspension force in N."),
        "inertia":number(0.001, 1e4, "Wheel inertia about the axle, kg m^2."),
        "steer":{"type":"boolean"},"drive":{"type":"boolean"},
        "brake":{"type":"boolean"},"handbrake":{"type":"boolean"},
        "brake_torque":number(0., 1e5, "Torque at full brake pedal, N m."),
        "handbrake_torque":number(0., 1e5, "Torque at full handbrake, N m."),
        "entity":{"type":"string","description":"Entity id that receives the wheel pose."}}});
    let bar = json!({"type":"object","additionalProperties":false,"properties":{
        "wheel_a":{"type":"integer","minimum":0,"maximum":MAX_WHEELS - 1},
        "wheel_b":{"type":"integer","minimum":0,"maximum":MAX_WHEELS - 1},
        "stiffness":number(0., 1e7, "Bar stiffness N/m.")}});
    let action = |doc: &str| json!({"type":"string","maxLength":256,"description":doc});
    let input = json!({"type":"object","additionalProperties":false,"properties":{
        "throttle":action("Pedal action (analog value or held)."),
        "brake":action("Pedal action."),
        "steer_left":action("Digital or analog (0..1) steer left."),
        "steer_right":action("Digital or analog (0..1) steer right."),
        "handbrake":action("Pedal action."),
        "shift_up":action("Pressed action requesting one gear up."),
        "shift_down":action("Pressed action requesting one gear down.")}});
    let schema = json!({"type":"object","additionalProperties":false,"properties":{
        "wheels":{"type":"array","minItems":2,"maxItems":MAX_WHEELS,"items":wheel},
        "tyre":tyre,"engine":engine,"gearbox":gearbox,"differential":differential,
        "steering":steering,
        "anti_roll":{"type":"array","maxItems":dim3::MAX_ANTI_ROLL_BARS,"items":bar},
        "suspension_cast":{"type":"string","enum":["ray","sphere"]},
        "forward":vector(3),
        "drag":number(0., 1e4, "Aerodynamic drag in N per (m/s)^2."),
        "downforce":number(0., 1e4, "Downforce in N per (m/s)^2."),
        "rolling_resistance":number(0., 0.5, "Rolling resistance coefficient."),
        "spawn_rolling":{"type":"boolean"},
        "input":input,
        "throttle":number(0., 1., "Direct throttle pedal."),
        "brake":number(0., 1., "Direct brake pedal."),
        "handbrake":number(0., 1., "Direct handbrake."),
        "steer":number(-1., 1., "Direct steering, positive turns right."),
        "shift_up":{"type":"integer","minimum":0,"description":"Increment to request one gear up."},
        "shift_down":{"type":"integer","minimum":0,"description":"Increment to request one gear down."}
    }});
    vec![crate::ComponentDescriptor {
        name: "engine.Vehicle3D".into(),
        version: 1,
        schema,
        defaults: serde_json::to_value(Vehicle3D::default()).expect("finite defaults"),
        requires: vec!["Transform".into(), "engine.PhysicsBody3D".into()],
        entity_references: (0..MAX_WHEELS)
            .map(|i| format!("/wheels/{i}/entity"))
            .collect(),
        data_only: false,
    }]
}

// ---- Native API ----------------------------------------------------------------------

/// One wheel after the last physics step, from [`vehicle_wheels`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelReadout {
    pub in_contact: bool,
    pub suspension_length: f32,
    pub suspension_velocity: f32,
    /// Spring plus damper force, which is also the tyre normal load (N).
    pub suspension_force: f32,
    pub slip_ratio: f32,
    pub slip_angle: f32,
    pub longitudinal_force: f32,
    pub lateral_force: f32,
    /// Rotation speed about the axle in rad/s (positive rolls forward).
    pub angular_velocity: f32,
    pub spin: f32,
    pub steer_angle: f32,
    pub contact_point: Vec3,
    pub contact_normal: Vec3,
    /// The physics body entity under the wheel.
    pub ground_entity: Option<Entity>,
    pub ground_friction: f32,
    /// Wheel centre and orientation in world space.
    pub position: Vec3,
    pub rotation: Quat,
    /// Wheel centre and orientation relative to the chassis (steer and spin included).
    pub local_position: Vec3,
    pub local_rotation: Quat,
}
impl From<&dim3::WheelState> for WheelReadout {
    fn from(s: &dim3::WheelState) -> Self {
        Self {
            in_contact: s.in_contact,
            suspension_length: s.suspension_length,
            suspension_velocity: s.suspension_velocity,
            suspension_force: s.suspension_force,
            slip_ratio: s.slip_ratio,
            slip_angle: s.slip_angle,
            longitudinal_force: s.longitudinal_force,
            lateral_force: s.lateral_force,
            angular_velocity: s.angular_velocity,
            spin: s.spin,
            steer_angle: s.steer_angle,
            contact_point: Vec3::from(s.contact_point),
            contact_normal: Vec3::from(s.contact_normal),
            ground_entity: s.ground_body.and_then(Entity::try_from_bits),
            ground_friction: s.ground_friction,
            position: Vec3::from(s.position),
            rotation: Quat::from_array(s.rotation),
            local_position: Vec3::from(s.local_position),
            local_rotation: Quat::from_array(s.local_rotation),
        }
    }
}
/// Vehicle state after the last physics step, from [`vehicle_state`].
#[derive(Clone, Debug, PartialEq)]
pub struct VehicleReadout {
    /// Speed along the vehicle forward direction in m/s (negative when reversing).
    pub speed: f32,
    pub lateral_speed: f32,
    pub engine_rpm: f32,
    pub engine_torque: f32,
    /// `-1` reverse, `0` neutral, `1..` forward.
    pub gear: i32,
    pub shifting: bool,
    pub steer_angle: f32,
    pub wheels_in_contact: usize,
    pub airborne: bool,
    pub wheels: Vec<WheelReadout>,
}

fn missing() -> crate::Error {
    invalid("Entity has no synced engine.Vehicle3D")
}
/// Sets the direct controls (`throttle`, `brake`, `handbrake` in 0..1, `steer` in -1..1) of
/// the entity's vehicle; they apply at the next physics sync.
pub fn set_vehicle_controls(
    world: &mut World,
    entity: Entity,
    throttle: f32,
    brake: f32,
    steer: f32,
    handbrake: f32,
) -> Result<()> {
    dim3::VehicleControls {
        throttle,
        brake,
        steer,
        handbrake,
    }
    .validate()
    .map_err(mapped)?;
    let mut vehicle = world
        .get_mut::<Vehicle3D>(entity)
        .ok_or_else(|| invalid("Entity has no engine.Vehicle3D"))?;
    vehicle.throttle = throttle;
    vehicle.brake = brake;
    vehicle.steer = steer;
    vehicle.handbrake = handbrake;
    Ok(())
}
/// Requests one gear up on the next sync (manual gearbox).
pub fn vehicle_shift_up(world: &mut World, entity: Entity) -> Result<()> {
    world
        .get_mut::<Vehicle3D>(entity)
        .ok_or_else(|| invalid("Entity has no engine.Vehicle3D"))?
        .shift_up += 1;
    Ok(())
}
/// Requests one gear down on the next sync (manual gearbox).
pub fn vehicle_shift_down(world: &mut World, entity: Entity) -> Result<()> {
    world
        .get_mut::<Vehicle3D>(entity)
        .ok_or_else(|| invalid("Entity has no engine.Vehicle3D"))?
        .shift_down += 1;
    Ok(())
}
/// Selects `gear` (`-1` reverse, `0` neutral, `1..`) on the next physics step. The vehicle
/// must be synced.
pub fn vehicle_set_gear(world: &mut World, entity: Entity, gear: i32) -> Result<()> {
    service_mut(world)?
        .worlds
        .three
        .set_vehicle_gear(entity.to_bits(), gear)
        .map_err(mapped)
}
/// Per-wheel state after the last physics step, in wheel order.
pub fn vehicle_wheels(world: &World, entity: Entity) -> Result<Vec<WheelReadout>> {
    let states = service(world)?
        .worlds
        .three
        .vehicle_wheel_states(entity.to_bits())
        .ok_or_else(missing)?;
    Ok(states.iter().map(Into::into).collect())
}
/// World pose of wheel `index` for a visual, if the vehicle has such a wheel.
pub fn vehicle_wheel_pose(world: &World, entity: Entity, index: usize) -> Result<(Vec3, Quat)> {
    let wheel = vehicle_wheels(world, entity)?
        .get(index)
        .copied()
        .ok_or_else(|| invalid("Vehicle has no such wheel"))?;
    Ok((wheel.position, wheel.rotation))
}
/// Vehicle and wheel state after the last physics step.
pub fn vehicle_state(world: &World, entity: Entity) -> Result<VehicleReadout> {
    let physics = service(world)?;
    let id = entity.to_bits();
    let state = physics.worlds.three.vehicle_state(id).ok_or_else(missing)?;
    Ok(VehicleReadout {
        speed: state.speed,
        lateral_speed: state.lateral_speed,
        engine_rpm: state.engine_rpm,
        engine_torque: state.engine_torque,
        gear: state.gear,
        shifting: state.shifting,
        steer_angle: state.steer_angle,
        wheels_in_contact: state.wheels_in_contact,
        airborne: state.airborne,
        wheels: vehicle_wheels(world, entity)?,
    })
}
