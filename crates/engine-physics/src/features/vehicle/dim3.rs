//! Raycast vehicles (docs/physics-vehicles.md). Three-dimensional worlds only.
//!
//! A vehicle is an existing dynamic body (the chassis) plus 2..=16 wheels. There are no wheel
//! bodies or colliders: every wheel probes the world with a ray (or a sphere cast) from its
//! mount point and the resulting spring, damper and tyre forces are applied to the chassis as
//! impulses in the extension `pre_step` hook, before Rapier integrates the step. Scene queries
//! use the broad phase built by the previous `step`, so colliders inserted since then are
//! invisible until the next step (step the world once after building a scene).
//!
//! The tyre ([`TyreDesc`]) and drivetrain ([`EngineDesc`], [`GearboxDesc`],
//! [`DifferentialDesc`]) models are in `tyre.rs` and `drivetrain.rs`; this file owns the
//! suspension, the wheel dynamics and the per-step orchestration.
use super::*;
use crate::{Error, Result};
use rapier::parry::query::ShapeCastOptions;
use rapier::parry::shape::Ball;
use rapier::pipeline::QueryFilter as RapierFilter;
use std::collections::BTreeMap;

#[path = "drivetrain.rs"]
mod drivetrain;
#[path = "tyre.rs"]
mod tyre;
pub use drivetrain::{
    DifferentialDesc, EngineDesc, GearboxDesc, GearboxMode, MAX_GEARS, MAX_TORQUE_SAMPLES,
    RAD_S_TO_RPM, split_torque,
};
pub use tyre::{PacejkaAxis, TyreDesc, TyreForces};

/// Most undrained vehicle events kept; later events are dropped (and counted).
pub const MAX_VEHICLE_EVENTS: usize = 65_536;
/// Most wheels per vehicle.
pub const MAX_WHEELS: usize = 16;
/// Most anti-roll bars per vehicle.
pub const MAX_ANTI_ROLL_BARS: usize = 8;
/// Wheel speed limit (rad/s) that keeps a runaway wheel finite.
const MAX_WHEEL_SPEED: f32 = 3000.;
/// Seconds without any wheel contact before a vehicle counts as airborne.
const AIRBORNE_DELAY: f32 = 0.1;
/// Minimum seconds between automatic shifts.
const SHIFT_COOLDOWN: f32 = 0.5;
/// Speed in m/s over which rolling resistance builds up to its full value.
const ROLLING_SPEED: f32 = 0.05;
/// Pedal level above which the automatic gearbox reads a pedal as pressed.
const PEDAL: f32 = 0.05;

/// How a wheel looks for the ground.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SuspensionCast {
    /// A ray from the mount point along the suspension direction (cheapest).
    #[default]
    Ray,
    /// A sphere of the wheel radius swept along the suspension direction; rides over curbs
    /// and gaps a ray falls into, at a higher cost.
    Sphere,
}

/// One wheel with its suspension. Lengths are meters, forces newtons, torques N m. Vectors
/// are in the chassis frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelDesc {
    /// Top of the suspension.
    pub mount: Position,
    /// Direction the wheel extends from the mount toward the ground. Default -Y.
    pub suspension_dir: Position,
    /// Spin axis. The wheel rolls forward for positive spin when `axle x up` (with `up` =
    /// `-suspension_dir`) is the vehicle forward direction. Default +X for forward +Z.
    pub axle: Position,
    pub radius: f32,
    /// Spring length with no load (fully extended).
    pub rest_length: f32,
    /// Compression the suspension can travel; the shortest length is `rest_length -
    /// max_travel`.
    pub max_travel: f32,
    /// Spring rate in N/m.
    pub stiffness: f32,
    /// Damper rate in N s/m while compressing.
    pub damping_compression: f32,
    /// Damper rate in N s/m while extending.
    pub damping_relaxation: f32,
    /// Largest suspension force (N).
    pub max_force: f32,
    /// Rotational inertia of wheel plus tyre about the axle (kg m^2).
    pub inertia: f32,
    pub steer: bool,
    pub drive: bool,
    pub brake: bool,
    pub handbrake: bool,
    /// Brake torque at full pedal (N m); only used when `brake` is set.
    pub brake_torque: f32,
    /// Torque at full handbrake (N m); only used when `handbrake` is set.
    pub handbrake_torque: f32,
}
impl Default for WheelDesc {
    fn default() -> Self {
        Self {
            mount: [0.; 3],
            suspension_dir: [0., -1., 0.],
            axle: [1., 0., 0.],
            radius: 0.34,
            rest_length: 0.3,
            max_travel: 0.2,
            stiffness: 35_000.,
            damping_compression: 3_000.,
            damping_relaxation: 4_000.,
            max_force: 60_000.,
            inertia: 1.5,
            steer: false,
            drive: false,
            brake: true,
            handbrake: false,
            brake_torque: 2_500.,
            handbrake_torque: 3_500.,
        }
    }
}
impl WheelDesc {
    fn validate(self) -> Result<Self> {
        let finite = crate::finite(&self.mount)
            && crate::finite(&self.suspension_dir)
            && crate::finite(&self.axle);
        let dir = vector(self.suspension_dir);
        let axle = vector(self.axle);
        let ok = finite
            && dir.length() > 1e-3
            && axle.length() > 1e-3
            && (0.02..=2.).contains(&self.radius)
            && (0.01..=2.).contains(&self.rest_length)
            && self.max_travel.is_finite()
            && self.max_travel > 0.001
            && self.max_travel <= self.rest_length
            && (100. ..=1e7).contains(&self.stiffness)
            && (0. ..=1e6).contains(&self.damping_compression)
            && (0. ..=1e6).contains(&self.damping_relaxation)
            && (1. ..=1e7).contains(&self.max_force)
            && (0.001..=1e4).contains(&self.inertia)
            && (0. ..=1e5).contains(&self.brake_torque)
            && (0. ..=1e5).contains(&self.handbrake_torque);
        if !ok {
            return Err(Error("invalid vehicle wheel"));
        }
        let dir = dir.normalize();
        let up = -dir;
        let axle = (axle - up * axle.dot(up)).normalize_or_zero();
        if axle == Vector::ZERO {
            return Err(Error("wheel axle must not be parallel to the suspension"));
        }
        Ok(Self {
            suspension_dir: dir.to_array(),
            axle: axle.to_array(),
            ..self
        })
    }
    fn min_length(&self) -> f32 {
        self.rest_length - self.max_travel
    }
}

/// Steering limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SteeringDesc {
    /// Steer angle at full input and low speed (radians, `0..=pi/2`).
    pub max_angle: f32,
    /// Speed (m/s) at which the angle has been reduced to `min_factor` of `max_angle`; the
    /// reduction is linear from zero speed. `0` disables the reduction.
    pub reduction_speed: f32,
    /// Fraction of `max_angle` left at `reduction_speed` and above (`0..=1`).
    pub min_factor: f32,
    /// Largest steer angle change in rad/s (`0.01..=100`).
    pub rate: f32,
}
impl Default for SteeringDesc {
    fn default() -> Self {
        Self {
            max_angle: 0.55,
            reduction_speed: 40.,
            min_factor: 0.25,
            rate: 3.,
        }
    }
}
impl SteeringDesc {
    fn validate(self) -> Result<Self> {
        let ok = (0. ..=std::f32::consts::FRAC_PI_2).contains(&self.max_angle)
            && (0. ..=1e4).contains(&self.reduction_speed)
            && (0. ..=1.).contains(&self.min_factor)
            && (0.01..=100.).contains(&self.rate);
        if !ok {
            return Err(Error("invalid steering settings"));
        }
        Ok(self)
    }
}

/// Anti-roll bar joining two wheels: pushes the more compressed wheel up and the other down
/// with `stiffness * (compression_a - compression_b)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AntiRollBar {
    pub wheel_a: usize,
    pub wheel_b: usize,
    /// N/m (`0..=1e7`).
    pub stiffness: f32,
}

/// Everything that describes a vehicle besides its chassis body.
#[derive(Clone, Debug, PartialEq)]
pub struct VehicleDesc {
    /// 2..=[`MAX_WHEELS`] wheels.
    pub wheels: Vec<WheelDesc>,
    pub tyre: TyreDesc,
    pub engine: EngineDesc,
    pub gearbox: GearboxDesc,
    pub differential: DifferentialDesc,
    pub steering: SteeringDesc,
    pub anti_roll: Vec<AntiRollBar>,
    pub cast: SuspensionCast,
    /// Vehicle forward direction in the chassis frame (default +Z).
    pub forward: Position,
    /// Aerodynamic drag in N per (m/s)^2 (`0.5 * rho * Cd * A`, `0..=1e4`).
    pub drag: f32,
    /// Downforce along the chassis up in N per (m/s)^2 (`0..=1e4`).
    pub downforce: f32,
    /// Rolling resistance coefficient: a force of `coefficient * load` against the motion of
    /// each wheel in contact (`0..=0.5`).
    pub rolling_resistance: f32,
}
impl Default for VehicleDesc {
    fn default() -> Self {
        Self {
            wheels: Vec::new(),
            tyre: TyreDesc::default(),
            engine: EngineDesc::default(),
            gearbox: GearboxDesc::default(),
            differential: DifferentialDesc::Open,
            steering: SteeringDesc::default(),
            anti_roll: Vec::new(),
            cast: SuspensionCast::Ray,
            forward: [0., 0., 1.],
            drag: 0.4,
            downforce: 0.,
            rolling_resistance: 0.012,
        }
    }
}
impl VehicleDesc {
    /// Checks every field; directions are normalized.
    pub fn validate(mut self) -> Result<Self> {
        if !(2..=MAX_WHEELS).contains(&self.wheels.len()) {
            return Err(Error("a vehicle needs 2 to 16 wheels"));
        }
        let forward = vector(self.forward);
        if !crate::finite(&self.forward) || forward.length() < 1e-3 {
            return Err(Error("invalid vehicle forward direction"));
        }
        let forward = forward.normalize();
        self.forward = forward.to_array();
        for wheel in &mut self.wheels {
            *wheel = wheel.validate()?;
            let up = -vector(wheel.suspension_dir);
            if vector(wheel.axle).cross(up).dot(forward) < 0.5 {
                return Err(Error(
                    "wheel axle x up must point along the vehicle forward direction",
                ));
            }
        }
        self.tyre = self.tyre.validate()?;
        self.engine = self.engine.validate()?;
        self.gearbox = self.gearbox.validate()?;
        self.differential = self.differential.validate()?;
        self.steering = self.steering.validate()?;
        if self.anti_roll.len() > MAX_ANTI_ROLL_BARS
            || self.anti_roll.iter().any(|bar| {
                bar.wheel_a >= self.wheels.len()
                    || bar.wheel_b >= self.wheels.len()
                    || bar.wheel_a == bar.wheel_b
                    || !bar.stiffness.is_finite()
                    || !(0. ..=1e7).contains(&bar.stiffness)
            })
        {
            return Err(Error("invalid anti-roll bar"));
        }
        let ok = (0. ..=1e4).contains(&self.drag)
            && (0. ..=1e4).contains(&self.downforce)
            && (0. ..=0.5).contains(&self.rolling_resistance);
        if !ok {
            return Err(Error("invalid vehicle aerodynamics or rolling resistance"));
        }
        Ok(self)
    }
    /// Unit vector pointing up in the chassis frame (opposite the mean suspension direction).
    fn up(&self) -> Vector {
        let sum = self
            .wheels
            .iter()
            .fold(Vector::ZERO, |sum, w| sum - vector(w.suspension_dir));
        sum.normalize_or_zero()
    }
}

/// Driver inputs. Pedals are `0..=1`, steering `-1..=1` (positive turns right, toward
/// `forward x up`... that is, to the driver's right).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VehicleControls {
    pub throttle: f32,
    pub brake: f32,
    pub steer: f32,
    pub handbrake: f32,
}
impl VehicleControls {
    pub fn validate(self) -> Result<Self> {
        let pedal = |v: f32| v.is_finite() && (0. ..=1.).contains(&v);
        if !(pedal(self.throttle)
            && pedal(self.brake)
            && pedal(self.handbrake)
            && self.steer.is_finite()
            && (-1. ..=1.).contains(&self.steer))
        {
            return Err(Error("invalid vehicle controls"));
        }
        Ok(self)
    }
}

/// Read-back of one wheel after the last step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelState {
    pub in_contact: bool,
    /// Current suspension length (m), `rest_length - max_travel..=rest_length`.
    pub suspension_length: f32,
    /// Rate of change of the suspension length (m/s, negative while compressing).
    pub suspension_velocity: f32,
    /// Spring plus damper force pushing the chassis up (N); the tyre normal load.
    pub suspension_force: f32,
    /// Slip ratio and slip angle (rad) the tyre model used.
    pub slip_ratio: f32,
    pub slip_angle: f32,
    /// Force along the wheel heading / along the axle on the chassis (N).
    pub longitudinal_force: f32,
    pub lateral_force: f32,
    /// Rotation speed about the axle (rad/s, positive rolls forward).
    pub angular_velocity: f32,
    /// Accumulated spin angle in `0..2pi`.
    pub spin: f32,
    pub steer_angle: f32,
    /// Drive torque the wheel received this step (N m).
    pub drive_torque: f32,
    pub contact_point: Position,
    pub contact_normal: Position,
    /// Application body under the wheel, if it is one.
    pub ground_body: Option<u64>,
    /// Collider friction of the surface under the wheel.
    pub ground_friction: f32,
    /// World position of the wheel centre and its orientation (chassis * steer * spin); the
    /// pose a wheel mesh whose rolling axis is the chassis X axis should be drawn with.
    pub position: Position,
    pub rotation: Orientation,
    /// Orientation relative to the chassis (steer * spin), for child entities.
    pub local_rotation: Orientation,
    /// Wheel centre in the chassis frame.
    pub local_position: Position,
}

/// Read-back of the vehicle after the last step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VehicleState {
    /// Speed along the vehicle forward direction (m/s, negative when reversing).
    pub speed: f32,
    /// Speed along the chassis side axis (m/s).
    pub lateral_speed: f32,
    pub engine_rpm: f32,
    /// Engine torque at the crank (N m), after the pedal and rev limiter.
    pub engine_torque: f32,
    /// `-1` reverse, `0` neutral, `1..` forward.
    pub gear: i32,
    /// Torque is cut for a gear change.
    pub shifting: bool,
    pub steer_angle: f32,
    pub wheels_in_contact: usize,
    /// No wheel has touched the ground for a short while.
    pub airborne: bool,
}

/// A change worth reporting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VehicleEventKind {
    GearChanged {
        from: i32,
        to: i32,
    },
    /// A wheel started (`started`) or stopped touching the ground.
    WheelContact {
        wheel: usize,
        started: bool,
        ground_body: Option<u64>,
    },
    /// No wheel touched the ground for more than 0.1 s.
    Airborne,
    /// Wheels touched down again after [`VehicleEventKind::Airborne`]; `impact_speed` is the
    /// downward speed in m/s.
    Landed {
        impact_speed: f32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VehicleEvent {
    pub id: u64,
    pub kind: VehicleEventKind,
}

struct Wheel {
    omega: f32,
    spin: f32,
    state: WheelState,
}
struct Vehicle {
    desc: VehicleDesc,
    controls: VehicleControls,
    wheels: Vec<Wheel>,
    gear: i32,
    shift_timer: f32,
    cooldown: f32,
    engine_rpm: f32,
    steer_angle: f32,
    state: VehicleState,
    airborne_time: f32,
    airborne: bool,
    shift_up: bool,
    shift_down: bool,
    requested_gear: Option<i32>,
    roll_start: bool,
}
#[derive(Default)]
struct Vehicles {
    map: BTreeMap<u64, Vehicle>,
    events: Vec<VehicleEvent>,
    dropped: u64,
}
impl Vehicles {
    fn push(events: &mut Vec<VehicleEvent>, dropped: &mut u64, id: u64, kind: VehicleEventKind) {
        if events.len() < MAX_VEHICLE_EVENTS {
            events.push(VehicleEvent { id, kind });
        } else {
            *dropped += 1;
        }
    }
}
impl Extension for Vehicles {
    fn order(&self) -> u16 {
        30
    }
    fn pre_step(&mut self, core: &mut Core, dt: f32) -> Result<()> {
        let ids: Vec<u64> = self.map.keys().copied().collect();
        for id in ids {
            let Some(vehicle) = self.map.get_mut(&id) else {
                continue;
            };
            let mut out = Vec::new();
            step_vehicle(core, id, vehicle, dt, &mut out)?;
            for kind in out {
                Self::push(&mut self.events, &mut self.dropped, id, kind);
            }
        }
        Ok(())
    }
    fn post_step(&mut self, core: &mut Core, _dt: f32) -> Result<()> {
        for (&id, vehicle) in &mut self.map {
            refresh_poses(core, id, vehicle);
        }
        Ok(())
    }
    fn body_removed(&mut self, _core: &mut Core, id: u64) {
        self.map.remove(&id);
        for vehicle in self.map.values_mut() {
            for wheel in &mut vehicle.wheels {
                if wheel.state.ground_body == Some(id) {
                    wheel.state.ground_body = None;
                }
            }
        }
    }
}

fn blank_wheel_state(desc: &WheelDesc) -> WheelState {
    WheelState {
        in_contact: false,
        suspension_length: desc.rest_length,
        suspension_velocity: 0.,
        suspension_force: 0.,
        slip_ratio: 0.,
        slip_angle: 0.,
        longitudinal_force: 0.,
        lateral_force: 0.,
        angular_velocity: 0.,
        spin: 0.,
        steer_angle: 0.,
        drive_torque: 0.,
        contact_point: [0.; 3],
        contact_normal: [0., 1., 0.],
        ground_body: None,
        ground_friction: 0.,
        position: [0.; 3],
        rotation: [0., 0., 0., 1.],
        local_rotation: [0., 0., 0., 1.],
        local_position: desc.mount,
    }
}
fn new_vehicle(desc: VehicleDesc) -> Vehicle {
    let wheels = desc
        .wheels
        .iter()
        .map(|w| Wheel {
            omega: 0.,
            spin: 0.,
            state: blank_wheel_state(w),
        })
        .collect();
    let idle = desc.engine.idle_rpm;
    Vehicle {
        desc,
        controls: VehicleControls::default(),
        wheels,
        gear: 0,
        shift_timer: 0.,
        cooldown: 0.,
        engine_rpm: idle,
        steer_angle: 0.,
        state: VehicleState {
            speed: 0.,
            lateral_speed: 0.,
            engine_rpm: idle,
            engine_torque: 0.,
            gear: 0,
            shifting: false,
            steer_angle: 0.,
            wheels_in_contact: 0,
            airborne: false,
        },
        airborne_time: 0.,
        airborne: false,
        shift_up: false,
        shift_down: false,
        requested_gear: None,
        roll_start: false,
    }
}

/// Steer angle in radians (positive turns right) of a steerable wheel.
fn steer_rotation(up: Vector, angle: f32) -> Rotation {
    Rotation::from_axis_angle(up, -angle)
}

/// Wheel centre and orientation for the current chassis pose.
fn refresh_poses(core: &Core, id: u64, vehicle: &mut Vehicle) {
    let Ok(body) = core.body(id) else {
        return;
    };
    let pose = *body.position();
    for (index, wheel) in vehicle.wheels.iter_mut().enumerate() {
        let desc = &vehicle.desc.wheels[index];
        let dir = vector(desc.suspension_dir);
        let local = vector(desc.mount) + dir * wheel.state.suspension_length;
        let steer = if desc.steer {
            steer_rotation(-dir, wheel.state.steer_angle)
        } else {
            Rotation::IDENTITY
        };
        let spin = Rotation::from_axis_angle(vector(desc.axle), wheel.spin);
        let relative = steer * spin;
        wheel.state.local_position = local.to_array();
        wheel.state.local_rotation = relative.to_array();
        wheel.state.position = pose.transform_point(local).to_array();
        wheel.state.rotation = (pose.rotation * relative).to_array();
    }
}

/// Where a wheel found the ground.
struct Probe {
    handle: ColliderHandle,
    /// Distance from the mount to the wheel centre (before clamping).
    length: f32,
    point: Vector,
    normal: Vector,
}
fn probe(
    core: &Core,
    chassis: RigidBodyHandle,
    groups: InteractionGroups,
    cast: SuspensionCast,
    desc: &WheelDesc,
    origin: Vector,
    dir: Vector,
) -> Option<Probe> {
    let filter = RapierFilter::new()
        .exclude_rigid_body(chassis)
        .exclude_sensors()
        .groups(groups);
    let rapier = &core.rapier;
    let queries = rapier.broad_phase.as_query_pipeline(
        rapier.narrow_phase.query_dispatcher(),
        &rapier.bodies,
        &rapier.colliders,
        filter,
    );
    match cast {
        SuspensionCast::Ray => {
            let ray = Ray::new(origin, dir);
            let reach = desc.rest_length + desc.radius;
            let (handle, hit) = queries.cast_ray_and_get_normal(&ray, reach, true)?;
            Some(Probe {
                handle,
                length: hit.time_of_impact - desc.radius,
                point: origin + dir * hit.time_of_impact,
                normal: hit.normal,
            })
        }
        SuspensionCast::Sphere => {
            let ball = Ball::new(desc.radius);
            let (handle, hit) = queries.cast_shape(
                &Pose::from_translation(origin),
                dir,
                &ball,
                ShapeCastOptions {
                    target_distance: 0.,
                    stop_at_penetration: false,
                    max_time_of_impact: desc.rest_length,
                    compute_impact_geometry_on_penetration: true,
                },
            )?;
            Some(Probe {
                handle,
                length: hit.time_of_impact,
                point: hit.witness1,
                normal: hit.normal1,
            })
        }
    }
}

/// Per-wheel geometry and ground contact of one step.
struct Contact {
    mount: Vector,
    dir: Vector,
    up: Vector,
    axle: Vector,
    heading: Vector,
    probe: Option<Probe>,
    length: f32,
    compression: f32,
    friction: f32,
    ground_velocity: Vector,
    ground_id: Option<u64>,
    ground_body: Option<RigidBodyHandle>,
    load: f32,
}

fn step_vehicle(
    core: &mut Core,
    id: u64,
    v: &mut Vehicle,
    dt: f32,
    events: &mut Vec<VehicleEventKind>,
) -> Result<()> {
    let entry = core.entry(id)?;
    let body = core.body(id)?;
    if !body.is_dynamic() {
        return Ok(());
    }
    let pose = *body.position();
    let rot = pose.rotation;
    let com = body.center_of_mass();
    let lin = body.linvel();
    let mass = body.mass();
    let inv_mass = if mass > 0. { mass.recip() } else { 0. };
    let inv_inertia = body.mass_properties().local_mprops.world_inv_inertia(&rot);
    let groups = core
        .rapier
        .colliders
        .get(entry.collider)
        .map_or_else(InteractionGroups::all, |c| c.collision_groups());
    let desc = v.desc.clone();
    let count = desc.wheels.len();
    let chassis_forward = rot * vector(desc.forward);
    let chassis_up = rot * desc.up();
    let chassis_side = chassis_forward.cross(chassis_up);
    let speed = lin.dot(chassis_forward);
    let inv_eff = |point: Vector, normal: Vector| {
        let arm = (point - com).cross(normal);
        inv_mass + arm.dot(inv_inertia * arm)
    };

    // Steering: speed-sensitive target, rate limited.
    let steering = desc.steering;
    let factor = if steering.reduction_speed > 0. {
        1. - (1. - steering.min_factor) * (speed.abs() / steering.reduction_speed).min(1.)
    } else {
        1.
    };
    let target = v.controls.steer * steering.max_angle * factor;
    let delta = (steering.rate * dt).max(0.);
    v.steer_angle += (target - v.steer_angle).clamp(-delta, delta);

    if v.roll_start {
        v.roll_start = false;
        for (wheel, wd) in v.wheels.iter_mut().zip(&desc.wheels) {
            wheel.omega = speed / wd.radius;
        }
    }

    // Pass 1: geometry and ground probes.
    let mut contacts: Vec<Contact> = Vec::with_capacity(count);
    for wd in &desc.wheels {
        let mount = pose.transform_point(vector(wd.mount));
        let dir = (rot * vector(wd.suspension_dir)).normalize();
        let up = -dir;
        let mut axle = rot * vector(wd.axle);
        if wd.steer {
            axle = steer_rotation(up, v.steer_angle) * axle;
        }
        let heading = axle.cross(up).normalize_or_zero();
        let found = probe(core, entry.body, groups, desc.cast, wd, mount, dir);
        let (length, compression, friction, ground_velocity, ground_id, ground_body) = match &found
        {
            Some(p) => {
                let collider = core.rapier.colliders.get(p.handle);
                let parent = collider.and_then(|c| c.parent());
                let ground = parent.and_then(|h| core.rapier.bodies.get(h));
                (
                    p.length.clamp(wd.min_length(), wd.rest_length),
                    wd.rest_length - p.length,
                    collider.map_or(0., |c| c.friction()),
                    ground.map_or(Vector::ZERO, |b| b.velocity_at_point(p.point)),
                    core.collider_body_id(p.handle),
                    parent.filter(|_| ground.is_some_and(|b| b.is_dynamic())),
                )
            }
            None => (wd.rest_length, 0., 0., Vector::ZERO, None, None),
        };
        contacts.push(Contact {
            mount,
            dir,
            up,
            axle,
            heading,
            probe: found,
            length,
            compression,
            friction,
            ground_velocity,
            ground_id,
            ground_body,
            load: 0.,
        });
    }
    let touching = contacts.iter().filter(|c| c.probe.is_some()).count();
    let share = touching.max(1) as f32;

    // Pass 2: suspension.
    let mut impulses: Vec<(Vector, Vector)> = Vec::new();
    let mut reactions: Vec<(RigidBodyHandle, Vector, Vector)> = Vec::new();
    let mut anti_roll = vec![0.; count];
    for bar in &desc.anti_roll {
        let (a, b) = (&contacts[bar.wheel_a], &contacts[bar.wheel_b]);
        if a.probe.is_some() && b.probe.is_some() {
            let force = bar.stiffness * (a.compression - b.compression);
            anti_roll[bar.wheel_a] += force;
            anti_roll[bar.wheel_b] -= force;
        }
    }
    for (index, (wd, contact)) in desc.wheels.iter().zip(&mut contacts).enumerate() {
        let wheel = &mut v.wheels[index];
        let Some(found) = &contact.probe else {
            wheel.state.suspension_velocity = 0.;
            continue;
        };
        let centre = contact.mount + contact.dir * contact.length;
        let mount_velocity = body.velocity_at_point(contact.mount);
        let extension_rate = (contact.ground_velocity - mount_velocity).dot(contact.dir);
        let compression_rate = -extension_rate;
        let rate = if compression_rate > 0. {
            wd.damping_compression
        } else {
            wd.damping_relaxation
        };
        let limit = compression_rate.abs() / (dt * inv_eff(centre, contact.up) * 4.).max(1e-9);
        let damper = (rate * compression_rate).clamp(-limit, limit);
        let force = (wd.stiffness * contact.compression + damper + anti_roll[index])
            .clamp(0., wd.max_force);
        contact.load = force;
        wheel.state.suspension_velocity = extension_rate;
        wheel.state.suspension_force = force;
        let impulse = contact.up * (force * dt);
        impulses.push((impulse, centre));
        if let Some(handle) = contact.ground_body {
            reactions.push((handle, -impulse, found.point));
        }
    }

    // Drivetrain: pedals, gear, engine torque.
    let driven: Vec<usize> = (0..count).filter(|&i| desc.wheels[i].drive).collect();
    let omega_mean = if driven.is_empty() {
        0.
    } else {
        driven.iter().map(|&i| v.wheels[i].omega).sum::<f32>() / driven.len() as f32
    };
    let gearbox = &desc.gearbox;
    let ratio_now = gearbox.overall_ratio(v.gear);
    let wheel_rpm = if ratio_now == 0. {
        0.
    } else {
        (omega_mean * ratio_now * RAD_S_TO_RPM).max(0.)
    };
    update_gear(v, gearbox, speed, wheel_rpm, dt, events);
    let ratio = gearbox.overall_ratio(v.gear);
    let (pedal, brake_pedal) = if v.gear < 0 && gearbox.mode == GearboxMode::Automatic {
        (v.controls.brake, v.controls.throttle)
    } else {
        (v.controls.throttle, v.controls.brake)
    };
    let engine = &desc.engine;
    let wheel_rpm = if ratio == 0. {
        0.
    } else {
        (omega_mean * ratio * RAD_S_TO_RPM).max(0.)
    };
    if ratio == 0. {
        let free = engine.idle_rpm + pedal * (engine.max_rpm - engine.idle_rpm) * 0.6;
        let step = 12_000. * dt;
        v.engine_rpm += (free - v.engine_rpm).clamp(-step, step);
    } else {
        v.engine_rpm = wheel_rpm.clamp(engine.idle_rpm, engine.max_rpm * 1.02);
    }
    let crank = engine.torque_at(v.engine_rpm) * pedal * engine.limiter(v.engine_rpm);
    let mut wheel_torque = 0.;
    if ratio != 0. && v.shift_timer <= 0. {
        wheel_torque = crank * ratio * gearbox.efficiency;
        if wheel_rpm > engine.idle_rpm {
            let drag = engine.engine_brake * (v.engine_rpm / engine.max_rpm) * (1. - pedal);
            wheel_torque -= drag * ratio;
        }
    }
    let speeds: Vec<f32> = driven.iter().map(|&i| v.wheels[i].omega).collect();
    let shares = split_torque(desc.differential, wheel_torque, &speeds);
    let locked = desc.differential == DifferentialDesc::Locked;
    if locked && !driven.is_empty() {
        for &i in &driven {
            v.wheels[i].omega = omega_mean;
        }
    }
    let mut drive_torque = vec![0.; count];
    for (&i, &torque) in driven.iter().zip(&shares) {
        drive_torque[i] = torque;
    }

    // Pass 3: tyres and wheel dynamics.
    let tyre = &desc.tyre;
    let mut net_torque = vec![0.; count];
    for (index, (wd, contact)) in desc.wheels.iter().zip(&contacts).enumerate() {
        let wheel = &mut v.wheels[index];
        let mut fx_chassis = 0.;
        wheel.state.slip_ratio = 0.;
        wheel.state.slip_angle = 0.;
        wheel.state.longitudinal_force = 0.;
        wheel.state.lateral_force = 0.;
        if let Some(found) = &contact.probe {
            let normal = found.normal;
            let heading =
                (contact.heading - normal * normal.dot(contact.heading)).normalize_or_zero();
            let side = (contact.axle - normal * normal.dot(contact.axle)).normalize_or_zero();
            if contact.load > 0. && heading != Vector::ZERO && side != Vector::ZERO {
                let velocity = body.velocity_at_point(found.point) - contact.ground_velocity;
                let forward = velocity.dot(heading);
                let sideways = velocity.dot(side);
                let surface = wheel.omega * wd.radius;
                let grip = contact.friction / tyre.reference_surface_friction;
                let mut f = tyre.forces(contact.load, grip, forward, sideways, surface);
                // Never apply more than it takes to cancel the slip within this step.
                let slip = surface - forward;
                let wheel_gain = wd.radius * wd.radius / wd.inertia;
                let fx_cap = slip.abs()
                    / (dt * (wheel_gain + inv_eff(found.point, heading) * share)).max(1e-9);
                let fy_cap = sideways.abs() / (dt * inv_eff(found.point, side) * share).max(1e-9);
                f.longitudinal = f.longitudinal.clamp(-fx_cap, fx_cap);
                f.lateral = f.lateral.clamp(-fy_cap, fy_cap);
                let rolling =
                    desc.rolling_resistance * contact.load * (forward / ROLLING_SPEED).tanh();
                fx_chassis = f.longitudinal;
                wheel.state.slip_ratio = f.slip_ratio;
                wheel.state.slip_angle = f.slip_angle;
                wheel.state.longitudinal_force = f.longitudinal - rolling;
                wheel.state.lateral_force = f.lateral;
                let force = heading * (f.longitudinal - rolling) + side * f.lateral;
                impulses.push((force * dt, found.point));
                if let Some(handle) = contact.ground_body {
                    reactions.push((handle, -force * dt, found.point));
                }
            }
        }
        net_torque[index] = drive_torque[index] - fx_chassis * wd.radius;
        wheel.state.drive_torque = drive_torque[index];
    }
    let brake_in = brake_pedal.clamp(0., 1.);
    let handbrake_in = v.controls.handbrake;
    let brake_torque_of = |wd: &WheelDesc| {
        let mut torque = 0.;
        if wd.brake {
            torque += brake_in * wd.brake_torque;
        }
        if wd.handbrake {
            torque += handbrake_in * wd.handbrake_torque;
        }
        torque
    };
    let integrate = |omega: f32, torque: f32, brake: f32, inertia: f32| {
        let free = omega + torque / inertia * dt;
        let stop = brake / inertia * dt;
        let out = if free.abs() <= stop {
            0.
        } else {
            free - stop.copysign(free)
        };
        out.clamp(-MAX_WHEEL_SPEED, MAX_WHEEL_SPEED)
    };
    if locked && !driven.is_empty() {
        let inertia: f32 = driven.iter().map(|&i| desc.wheels[i].inertia).sum();
        let torque: f32 = driven.iter().map(|&i| net_torque[i]).sum();
        let brake: f32 = driven
            .iter()
            .map(|&i| brake_torque_of(&desc.wheels[i]))
            .sum();
        let omega = integrate(v.wheels[driven[0]].omega, torque, brake, inertia);
        for &i in &driven {
            v.wheels[i].omega = omega;
        }
    }
    for ((wd, wheel), torque) in desc.wheels.iter().zip(&mut v.wheels).zip(&net_torque) {
        if locked && wd.drive {
            continue;
        }
        wheel.omega = integrate(wheel.omega, *torque, brake_torque_of(wd), wd.inertia);
    }

    // Aerodynamics at the centre of mass.
    let velocity = lin;
    let airspeed = velocity.length();
    if airspeed > 1e-3 {
        let drag = -velocity * (desc.drag * airspeed);
        let down = -chassis_up * (desc.downforce * airspeed * airspeed);
        impulses.push(((drag + down) * dt, com));
    }

    // Apply to the chassis and to dynamic ground bodies. The chassis receives the forces as
    // user forces so Rapier integrates them together with gravity over the whole step (a
    // lumped impulse would leave a constant velocity error at equilibrium); the vehicle owns
    // the user force and torque of its chassis and rewrites them every step. Ground bodies
    // receive impulses.
    let body = core.body_mut(id)?;
    body.reset_forces(false);
    body.reset_torques(false);
    for (impulse, point) in &impulses {
        body.add_force_at_point(*impulse / dt, *point, true);
    }
    for (handle, impulse, point) in reactions {
        if let Some(ground) = core.rapier.bodies.get_mut(handle) {
            ground.apply_impulse_at_point(impulse, point, true);
        }
    }

    // State, contact events and airborne tracking.
    let previous: Vec<bool> = v.wheels.iter().map(|w| w.state.in_contact).collect();
    for (index, (wd, contact)) in desc.wheels.iter().zip(&contacts).enumerate() {
        let wheel = &mut v.wheels[index];
        let in_contact = contact.probe.is_some();
        wheel.spin = (wheel.spin + wheel.omega * dt).rem_euclid(std::f32::consts::TAU);
        let state = &mut wheel.state;
        state.in_contact = in_contact;
        state.suspension_length = contact.length;
        state.angular_velocity = wheel.omega;
        state.spin = wheel.spin;
        state.steer_angle = if wd.steer { v.steer_angle } else { 0. };
        state.ground_body = contact.ground_id;
        state.ground_friction = contact.friction;
        if let Some(found) = &contact.probe {
            state.contact_point = found.point.to_array();
            state.contact_normal = found.normal.to_array();
        } else {
            state.suspension_force = 0.;
            state.drive_torque = drive_torque[index];
        }
        if previous[index] != in_contact {
            events.push(VehicleEventKind::WheelContact {
                wheel: index,
                started: in_contact,
                ground_body: contact.ground_id,
            });
        }
    }
    if touching == 0 {
        v.airborne_time += dt;
        if !v.airborne && v.airborne_time >= AIRBORNE_DELAY {
            v.airborne = true;
            events.push(VehicleEventKind::Airborne);
        }
    } else {
        if v.airborne {
            events.push(VehicleEventKind::Landed {
                impact_speed: (-lin.dot(chassis_up)).max(0.),
            });
        }
        v.airborne = false;
        v.airborne_time = 0.;
    }
    v.state = VehicleState {
        speed,
        lateral_speed: lin.dot(chassis_side),
        engine_rpm: v.engine_rpm,
        engine_torque: crank,
        gear: v.gear,
        shifting: v.shift_timer > 0.,
        steer_angle: v.steer_angle,
        wheels_in_contact: touching,
        airborne: v.airborne,
    };
    Ok(())
}

/// Applies shift requests and the automatic logic, then ticks the shift timers.
fn update_gear(
    v: &mut Vehicle,
    gearbox: &GearboxDesc,
    speed: f32,
    wheel_rpm: f32,
    dt: f32,
    events: &mut Vec<VehicleEventKind>,
) {
    v.shift_timer = (v.shift_timer - dt).max(0.);
    v.cooldown = (v.cooldown - dt).max(0.);
    let top = gearbox.top_gear();
    let (throttle, brake) = (v.controls.throttle, v.controls.brake);
    let mut target = v.gear;
    if let Some(gear) = v.requested_gear.take() {
        target = gear;
    } else if gearbox.mode == GearboxMode::Manual {
        if v.shift_up {
            target = (v.gear + 1).min(top);
        } else if v.shift_down {
            target = (v.gear - 1).max(-1);
        }
    } else if v.gear >= 0 {
        if brake > PEDAL && throttle < PEDAL && speed < 0.5 {
            target = -1;
        } else if v.gear == 0 {
            if throttle > PEDAL {
                target = 1;
            }
        } else if v.cooldown <= 0. {
            if wheel_rpm >= gearbox.up_shift_rpm && v.gear < top {
                target = v.gear + 1;
            } else if wheel_rpm <= gearbox.down_shift_rpm && v.gear > 1 {
                target = v.gear - 1;
            }
        }
    } else if throttle > PEDAL && brake < PEDAL && speed > -0.5 {
        target = 1;
    }
    v.shift_up = false;
    v.shift_down = false;
    if target != v.gear {
        events.push(VehicleEventKind::GearChanged {
            from: v.gear,
            to: target,
        });
        if v.gear >= 1 && target >= 1 {
            v.shift_timer = gearbox.shift_time;
        }
        v.cooldown = SHIFT_COOLDOWN.max(gearbox.shift_time);
        v.gear = target;
    }
}

fn debug_vehicles(world: &World, sink: &mut DebugSink) {
    let Some(table) = world.extension::<Vehicles>() else {
        return;
    };
    for (&id, vehicle) in &table.map {
        let Ok(body) = world.core.body(id) else {
            continue;
        };
        let pose = *body.position();
        sink.anchor(id, "vehicle", body.translation().to_array());
        for (wd, wheel) in vehicle.desc.wheels.iter().zip(&vehicle.wheels) {
            let state = &wheel.state;
            let mount = pose.transform_point(vector(wd.mount));
            let ray_end = pose.rotation * vector(wd.suspension_dir) * (wd.rest_length + wd.radius);
            let colour = if state.in_contact {
                [0.2, 0.9, 0.3, 1.]
            } else {
                [0.9, 0.8, 0.2, 1.]
            };
            sink.line(mount.to_array(), (mount + ray_end).to_array(), colour);
            if state.in_contact {
                let point = vector(state.contact_point);
                let normal = vector(state.contact_normal);
                let up = -(pose.rotation * vector(wd.suspension_dir));
                // Contact normal, suspension force (blue) and tyre force (red).
                sink.line(
                    state.contact_point,
                    (point + normal * 0.4).to_array(),
                    [0.6, 0.6, 0.6, 1.],
                );
                sink.line(
                    state.contact_point,
                    (point + up * (state.suspension_force * 2e-4)).to_array(),
                    [0.2, 0.4, 1., 1.],
                );
                let [x, y, z, w] = state.local_rotation;
                let side = pose.rotation * (Rotation::from_xyzw(x, y, z, w) * vector(wd.axle));
                let heading = side.cross(up).normalize_or_zero();
                let tyre = heading * state.longitudinal_force + side * state.lateral_force;
                sink.line(
                    state.contact_point,
                    (point + tyre * 2e-4).to_array(),
                    [0.95, 0.25, 0.2, 1.],
                );
            }
        }
    }
}

impl World {
    /// Turns dynamic body `id` into a vehicle chassis. Mass, inertia and the chassis collider
    /// come from the body; the wheels are described by `desc`.
    pub fn insert_vehicle(&mut self, id: u64, desc: VehicleDesc) -> Result<()> {
        let desc = desc.validate()?;
        if !self.core.body(id)?.is_dynamic() {
            return Err(Error("vehicle requires a dynamic body"));
        }
        if self.is_vehicle(id) {
            return Err(Error("body is already a vehicle"));
        }
        self.add_debug_contributor("vehicle", debug_vehicles);
        // A vehicle is driven every step, so it must never be put to sleep: Rapier would
        // freeze it mid-step while the suspension still pushes on it.
        let activation = self.core.body_mut(id)?.activation_mut();
        activation.normalized_linear_threshold = -1.;
        activation.angular_threshold = -1.;
        activation.sleeping = false;
        let (_, table) = self.extension_mut::<Vehicles>();
        table.map.insert(id, new_vehicle(desc));
        Ok(())
    }
    /// Stops simulating `id` as a vehicle; the body stays a plain dynamic body.
    pub fn remove_vehicle(&mut self, id: u64) -> bool {
        let (core, table) = self.extension_mut::<Vehicles>();
        let removed = table.map.remove(&id).is_some();
        if removed && let Ok(body) = core.body_mut(id) {
            *body.activation_mut() = RigidBodyActivation::active();
            body.reset_forces(false);
            body.reset_torques(false);
        }
        removed
    }
    pub fn is_vehicle(&self, id: u64) -> bool {
        self.extension::<Vehicles>()
            .is_some_and(|t| t.map.contains_key(&id))
    }
    pub fn vehicle_desc(&self, id: u64) -> Option<VehicleDesc> {
        Some(self.extension::<Vehicles>()?.map.get(&id)?.desc.clone())
    }
    /// Replaces the settings of a vehicle. Runtime state (wheel speeds, gear) is kept when
    /// the wheel count is unchanged and reset otherwise.
    pub fn set_vehicle_desc(&mut self, id: u64, desc: VehicleDesc) -> Result<()> {
        let desc = desc.validate()?;
        let vehicle = self.vehicle_mut(id)?;
        if vehicle.desc.wheels.len() == desc.wheels.len() {
            let top = desc.gearbox.top_gear();
            vehicle.gear = vehicle.gear.min(top);
            vehicle.desc = desc;
        } else {
            *vehicle = new_vehicle(desc);
        }
        Ok(())
    }
    fn vehicle_mut(&mut self, id: u64) -> Result<&mut Vehicle> {
        let (_, table) = self.extension_mut::<Vehicles>();
        table.map.get_mut(&id).ok_or(Error("unknown vehicle"))
    }
    /// Driver inputs, applied every step until changed.
    pub fn set_vehicle_controls(&mut self, id: u64, controls: VehicleControls) -> Result<()> {
        let controls = controls.validate()?;
        self.vehicle_mut(id)?.controls = controls;
        Ok(())
    }
    pub fn vehicle_controls(&self, id: u64) -> Option<VehicleControls> {
        Some(self.extension::<Vehicles>()?.map.get(&id)?.controls)
    }
    /// Requests one gear up on the next step (manual gearbox only).
    pub fn vehicle_shift_up(&mut self, id: u64) -> Result<()> {
        self.vehicle_mut(id)?.shift_up = true;
        Ok(())
    }
    /// Requests one gear down on the next step (manual gearbox only).
    pub fn vehicle_shift_down(&mut self, id: u64) -> Result<()> {
        self.vehicle_mut(id)?.shift_down = true;
        Ok(())
    }
    /// Selects `gear` (`-1` reverse, `0` neutral, `1..` forward) on the next step in either
    /// gearbox mode.
    pub fn set_vehicle_gear(&mut self, id: u64, gear: i32) -> Result<()> {
        let vehicle = self.vehicle_mut(id)?;
        if !vehicle.desc.gearbox.valid_gear(gear) {
            return Err(Error("gear out of range"));
        }
        vehicle.requested_gear = Some(gear);
        Ok(())
    }
    /// Sets every wheel to roll at the chassis speed on the next step (use after spawning a
    /// vehicle with a velocity so the tyres do not start locked).
    pub fn vehicle_roll_start(&mut self, id: u64) -> Result<()> {
        self.vehicle_mut(id)?.roll_start = true;
        Ok(())
    }
    pub fn vehicle_state(&self, id: u64) -> Option<VehicleState> {
        Some(self.extension::<Vehicles>()?.map.get(&id)?.state)
    }
    pub fn vehicle_wheel_states(&self, id: u64) -> Option<Vec<WheelState>> {
        let vehicle = self.extension::<Vehicles>()?.map.get(&id)?;
        Some(vehicle.wheels.iter().map(|w| w.state).collect())
    }
    pub fn vehicle_wheel_state(&self, id: u64, wheel: usize) -> Option<WheelState> {
        let vehicle = self.extension::<Vehicles>()?.map.get(&id)?;
        vehicle.wheels.get(wheel).map(|w| w.state)
    }
    /// Vehicles in ascending id order.
    pub fn vehicle_ids(&self) -> Vec<u64> {
        self.extension::<Vehicles>()
            .map_or_else(Vec::new, |t| t.map.keys().copied().collect())
    }
    /// Takes the events collected since the last drain (at most [`MAX_VEHICLE_EVENTS`]).
    pub fn drain_vehicle_events(&mut self) -> Vec<VehicleEvent> {
        let (_, table) = self.extension_mut::<Vehicles>();
        std::mem::take(&mut table.events)
    }
    /// Events dropped because the buffer was full.
    pub fn dropped_vehicle_events(&self) -> u64 {
        self.extension::<Vehicles>().map_or(0, |t| t.dropped)
    }
}
