//! Raycast vehicles (docs/physics-vehicles.md): tyre and drivetrain maths plus whole-vehicle
//! behaviour on flat ground.
use engine_physics::dim3::{
    BodyDesc, DifferentialDesc, EngineDesc, GearboxDesc, GearboxMode, PacejkaAxis, Shape,
    SteeringDesc, SuspensionCast, TyreDesc, VehicleControls, VehicleDesc, VehicleEventKind,
    WheelDesc, World, split_torque,
};
use engine_physics::{BodyKind, Material};

const DT: f32 = 1. / 60.;
const G: f32 = 9.81;
const GROUND: u64 = 1;
const CAR: u64 = 2;
const MASS: f32 = 1500.;
const RADIUS: f32 = 0.34;
const REST: f32 = 0.3;
const STIFFNESS: f32 = 35_000.;

fn world() -> World {
    World::new([0., -G, 0.], 64).unwrap()
}

/// Large flat floor with its top at y = 0 and the given collider friction.
fn floor(world: &mut World, id: u64, centre_x: f32, half_x: f32, friction: f32) {
    floor_sized(world, id, centre_x, [half_x, 9_000.], friction);
}
/// Like [`floor`] with a chosen half size; sphere casts lose precision on very large boxes.
fn floor_sized(world: &mut World, id: u64, centre_x: f32, half: [f32; 2], friction: f32) {
    world
        .insert(
            id,
            BodyDesc {
                kind: BodyKind::Fixed,
                position: [centre_x, -0.5, 0.],
                ..Default::default()
            },
            Shape::Box {
                half_extents: [half[0], 0.5, half[1]],
            },
            Material {
                friction,
                ..Default::default()
            },
        )
        .unwrap();
}

/// Four-wheeler: 2.6 m wheelbase, 1.6 m track, front steering, rear drive.
fn car_desc() -> VehicleDesc {
    let wheel = |x: f32, z: f32, steer: bool, drive: bool| WheelDesc {
        mount: [x, 0., z],
        radius: RADIUS,
        rest_length: REST,
        max_travel: 0.2,
        stiffness: STIFFNESS,
        steer,
        drive,
        ..Default::default()
    };
    VehicleDesc {
        wheels: vec![
            wheel(-0.8, 1.3, true, false),
            wheel(0.8, 1.3, true, false),
            wheel(-0.8, -1.3, false, true),
            wheel(0.8, -1.3, false, true),
        ],
        tyre: TyreDesc {
            load_sensitivity: 0.,
            ..Default::default()
        },
        steering: SteeringDesc {
            max_angle: 0.2,
            reduction_speed: 0.,
            min_factor: 1.,
            rate: 100.,
        },
        gearbox: GearboxDesc {
            forward: vec![3.0, 1.8, 1.2],
            final_drive: 3.4,
            up_shift_rpm: 5800.,
            down_shift_rpm: 2500.,
            ..Default::default()
        },
        drag: 1.2,
        ..Default::default()
    }
}

/// Chassis box (half extents 0.9 x 0.3 x 2.0, 1500 kg) at `height` above the floor.
fn spawn(world: &mut World, at: [f32; 3], velocity: [f32; 3], desc: VehicleDesc) {
    let volume = 8. * 0.9 * 0.3 * 2.0;
    world
        .insert(
            CAR,
            BodyDesc {
                position: at,
                velocity,
                ..Default::default()
            },
            Shape::Box {
                half_extents: [0.9, 0.3, 2.0],
            },
            Material {
                density: MASS / volume,
                ..Default::default()
            },
        )
        .unwrap();
    world.insert_vehicle(CAR, desc).unwrap();
}

/// Chassis height at which the springs are compressed by `compression`.
fn ride_height(compression: f32) -> f32 {
    RADIUS + REST - compression
}
fn static_compression(count: f32) -> f32 {
    MASS * G / (STIFFNESS * count)
}

fn run(world: &mut World, seconds: f32) {
    for _ in 0..(seconds / DT).round() as usize {
        world.step(DT).unwrap();
    }
}
fn controls(throttle: f32, brake: f32, steer: f32, handbrake: f32) -> VehicleControls {
    VehicleControls {
        throttle,
        brake,
        steer,
        handbrake,
    }
}

// ---------------------------------------------------------------------------------------
// Tyre model.

#[test]
fn tyre_curves_peak_at_the_configured_slip_and_saturate() {
    let tyre = TyreDesc::default();
    for axis in [tyre.longitudinal, tyre.lateral] {
        let peak = axis.peak_slip();
        let at_peak = axis.curve(peak);
        // Magic formula peak is D (normalized to 1).
        assert!((at_peak - 1.).abs() < 0.01, "peak value {at_peak}");
        // Rises up to the peak, then saturates below it without collapsing.
        assert!(axis.curve(peak * 0.5) < at_peak);
        assert!(axis.curve(peak * 2.) < at_peak);
        assert!(axis.curve(1.5) > 0.5 * at_peak, "tail {}", axis.curve(1.5));
        // Odd function.
        assert!((axis.curve(-peak) + at_peak).abs() < 1e-5);
    }
    // For E = 0 the peak is where B x = tan(pi / (2 C)).
    let axis = PacejkaAxis {
        b: 10.,
        c: 1.5,
        e: 0.,
    };
    let analytic = (std::f32::consts::PI / (2. * axis.c)).tan() / axis.b;
    assert!((axis.peak_slip() - analytic).abs() < 0.002);
}

#[test]
fn tyre_force_peaks_at_mu_times_load() {
    let tyre = TyreDesc {
        load_sensitivity: 0.,
        ..Default::default()
    };
    let load = tyre.nominal_load;
    let speed = 20.;
    let mut best = 0.0_f32;
    for i in 0..400 {
        let ratio = i as f32 * 0.002;
        let forces = tyre.forces(load, 1., speed, 0., speed * (1. + ratio));
        best = best.max(forces.longitudinal);
    }
    let expected = tyre.friction * load;
    assert!(
        (best - expected).abs() < 0.01 * expected,
        "peak {best} vs {expected}"
    );
}

#[test]
fn combined_slip_stays_inside_the_friction_ellipse() {
    let tyre = TyreDesc::default();
    let load = 3500.;
    let pure_lateral = tyre.forces(load, 1., 15., 3., 15.);
    for (side, ratio) in [(1., 0.1), (3., 0.15), (5., 0.5), (8., 1.), (0.5, 0.05)] {
        let f = tyre.forces(load, 1., 15., side, 15. * (1. + ratio));
        let used =
            (f.longitudinal / f.peak_longitudinal).powi(2) + (f.lateral / f.peak_lateral).powi(2);
        assert!(used <= 1. + 1e-4, "ellipse use {used}");
    }
    // Adding longitudinal slip takes grip away from the lateral force.
    let combined = tyre.forces(load, 1., 15., 3., 15. * 1.15);
    assert!(combined.lateral.abs() < pure_lateral.lateral.abs());
    assert!(combined.longitudinal > 0.);
    // Opposing slips give opposing forces.
    assert!(pure_lateral.lateral < 0.);
}

#[test]
fn load_sensitivity_and_surface_scale_change_the_peak() {
    let tyre = TyreDesc {
        load_sensitivity: 0.2,
        ..Default::default()
    };
    let nominal = tyre.peak_force(tyre.nominal_load, 1.);
    assert!((nominal - tyre.friction * tyre.nominal_load).abs() < 1e-2);
    // Twice the load gives less than twice the force.
    assert!(tyre.peak_force(2. * tyre.nominal_load, 1.) < 2. * nominal);
    assert!((tyre.peak_force(tyre.nominal_load, 0.2) - 0.2 * nominal).abs() < 1e-2);
    assert_eq!(tyre.peak_force(0., 1.), 0.);
}

#[test]
fn engine_torque_curve_and_differential_split() {
    let engine = EngineDesc::default();
    assert_eq!(engine.torque_at(0.), 180.);
    assert!((engine.torque_at(3250.) - 280.).abs() < 1e-3);
    assert_eq!(engine.torque_at(9_000.), 220.);
    assert_eq!(engine.limiter(1000.), 1.);
    assert_eq!(engine.limiter(6500.), 0.);
    let open = split_torque(DifferentialDesc::Open, 100., &[10., 50.]);
    assert_eq!(open, vec![50., 50.]);
    let lsd = split_torque(
        DifferentialDesc::LimitedSlip {
            preload: 20.,
            viscous: 1.,
            max_torque: 200.,
        },
        100.,
        &[10., 50.],
    );
    // Torque moves from the faster wheel to the slower one and the total is conserved.
    assert!(lsd[0] > 50. && lsd[1] < 50.);
    assert!((lsd[0] + lsd[1] - 100.).abs() < 1e-3);
}

// ---------------------------------------------------------------------------------------
// Whole vehicle.

#[test]
fn settles_at_the_analytic_compression_and_stays_put() {
    for cast in [SuspensionCast::Ray, SuspensionCast::Sphere] {
        let mut w = world();
        floor_sized(&mut w, GROUND, 0., [50., 50.], 0.5);
        let desc = VehicleDesc { cast, ..car_desc() };
        spawn(&mut w, [0., ride_height(0.) + 0.15, 0.], [0.; 3], desc);
        run(&mut w, 4.);
        let compression = static_compression(4.);
        let expected = ride_height(compression);
        let before = w.pose(CAR).unwrap().position;
        run(&mut w, 5.);
        let after = w.pose(CAR).unwrap().position;
        assert!(
            (after[1] - expected).abs() < 5e-3,
            "{cast:?}: height {} expected {expected}",
            after[1]
        );
        let drift = ((after[0] - before[0]).powi(2)
            + (after[1] - before[1]).powi(2)
            + (after[2] - before[2]).powi(2))
        .sqrt();
        assert!(drift < 1e-2, "{cast:?}: drift {drift}");
        let state = w.vehicle_state(CAR).unwrap();
        assert_eq!(state.wheels_in_contact, 4);
        assert!(state.speed.abs() < 1e-2);
        for wheel in w.vehicle_wheel_states(CAR).unwrap() {
            assert!(wheel.in_contact);
            assert!(
                (wheel.suspension_length - (REST - compression)).abs() < 5e-3,
                "length {}",
                wheel.suspension_length
            );
            assert!((wheel.suspension_force - MASS * G / 4.).abs() < 0.02 * MASS * G / 4.);
        }
    }
}

#[test]
fn full_throttle_accelerates_monotonically_shifts_and_reaches_the_drag_limit() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    let desc = car_desc();
    spawn(
        &mut w,
        [0., ride_height(0.) + 0.1, 0.],
        [0.; 3],
        desc.clone(),
    );
    w.set_vehicle_controls(CAR, controls(1., 0., 0., 0.))
        .unwrap();
    let mut samples = Vec::new();
    let mut shifts = Vec::new();
    let mut last_rpm = 0.;
    for step in 0..(120. / DT) as usize {
        w.step(DT).unwrap();
        let state = w.vehicle_state(CAR).unwrap();
        for event in w.drain_vehicle_events() {
            if let VehicleEventKind::GearChanged { from, to } = event.kind {
                shifts.push((from, to, last_rpm));
            }
        }
        last_rpm = state.engine_rpm;
        if step % 60 == 59 {
            samples.push(state.speed);
        }
    }
    // Monotonic until the speed saturates against drag.
    for window in samples[..40].windows(2) {
        assert!(window[1] > window[0], "speed fell: {window:?}");
    }
    // Gear 0 -> 1 at launch, then automatic upshifts at the configured rpm.
    assert_eq!(shifts[0].0, 0);
    assert_eq!(shifts[0].1, 1);
    let upshifts: Vec<_> = shifts.iter().filter(|s| s.1 > s.0 && s.0 >= 1).collect();
    assert_eq!(upshifts.len(), 2, "shifts {shifts:?}");
    assert_eq!((upshifts[0].0, upshifts[0].1), (1, 2));
    assert_eq!((upshifts[1].0, upshifts[1].1), (2, 3));
    for shift in &upshifts {
        assert!(
            shift.2 >= 0.97 * desc.gearbox.up_shift_rpm,
            "upshift at {} rpm",
            shift.2
        );
    }
    // Top speed: bisect the balance of wheel force against drag and rolling resistance.
    let top = desc.gearbox.overall_ratio(3);
    let radius = RADIUS;
    let balance = |v: f32| {
        let rpm = v / radius * top * 60. / std::f32::consts::TAU;
        let force =
            desc.engine.torque_at(rpm) * desc.engine.limiter(rpm) * top * desc.gearbox.efficiency
                / radius;
        force - desc.drag * v * v - desc.rolling_resistance * MASS * G
    };
    let (mut lo, mut hi) = (1., 80.);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if balance(mid) > 0. {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let measured = *samples.last().unwrap();
    assert!(
        (measured - lo).abs() < 0.1 * lo,
        "top speed {measured} vs analytic {lo}"
    );
    assert_eq!(w.vehicle_state(CAR).unwrap().gear, 3);
}

#[test]
fn braking_distance_matches_the_friction_limit() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    let desc = car_desc();
    let mu = desc.tyre.friction;
    spawn(
        &mut w,
        [
            0.,
            static_compression(4.).mul_add(-1., ride_height(0.)),
            -150.,
        ],
        [0., 0., 20.],
        desc,
    );
    w.vehicle_roll_start(CAR).unwrap();
    run(&mut w, 0.5);
    let start = w.pose(CAR).unwrap().position[2];
    let v0 = w.vehicle_state(CAR).unwrap().speed;
    assert!((v0 - 20.).abs() < 1., "entry speed {v0}");
    w.set_vehicle_controls(CAR, controls(0., 1., 0., 0.))
        .unwrap();
    for _ in 0..(8. / DT) as usize {
        w.step(DT).unwrap();
        if w.vehicle_state(CAR).unwrap().speed < 0.05 {
            break;
        }
    }
    let stop = w.pose(CAR).unwrap().position[2];
    let distance = stop - start;
    let expected = v0 * v0 / (2. * mu * G);
    assert!(
        (distance - expected).abs() < 0.2 * expected,
        "stopped in {distance} m, mu*g estimate {expected} m"
    );
    assert!(w.vehicle_state(CAR).unwrap().speed.abs() < 0.2);
}

#[test]
fn constant_steer_follows_the_wheelbase_circle() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 400., 0.5);
    let desc = car_desc();
    let angle = desc.steering.max_angle;
    spawn(
        &mut w,
        [0., ride_height(static_compression(4.)), 0.],
        [0., 0., 5.],
        desc,
    );
    w.vehicle_roll_start(CAR).unwrap();
    let mut radii = Vec::new();
    for step in 0..(8. / DT) as usize {
        let speed = w.vehicle_state(CAR).unwrap().speed;
        // Hold roughly 5 m/s with a small throttle / brake controller.
        let throttle = ((5. - speed) * 0.3).clamp(0., 0.4);
        let brake = ((speed - 5.) * 0.2).clamp(0., 0.3);
        w.set_vehicle_controls(CAR, controls(throttle, brake, 1., 0.))
            .unwrap();
        w.step(DT).unwrap();
        if step > (4. / DT) as usize {
            let yaw = w.velocity(CAR).unwrap().angular[1];
            let planar = w.velocity(CAR).unwrap().linear;
            let v = (planar[0] * planar[0] + planar[2] * planar[2]).sqrt();
            assert!(
                yaw < 0.,
                "positive steer turns right (negative yaw about +Y)"
            );
            radii.push(v / yaw.abs());
        }
    }
    let radius = radii.iter().sum::<f32>() / radii.len() as f32;
    let wheelbase = 2.6;
    let rear_axle = wheelbase / angle.tan();
    let expected = (rear_axle * rear_axle + (wheelbase / 2.).powi(2)).sqrt();
    assert!(
        (radius - expected).abs() < 0.15 * expected,
        "turn radius {radius} vs {expected}"
    );
}

/// Full throttle in first gear on a split-mu surface (left slab icy, right slab grippy).
fn split_mu_run(differential: DifferentialDesc) -> (f32, f32, f32, f32) {
    let mut w = world();
    floor(&mut w, GROUND, -100., 100., 0.1);
    floor(&mut w, 10, 100., 100., 1.0);
    let desc = VehicleDesc {
        differential,
        ..car_desc()
    };
    spawn(
        &mut w,
        [0., ride_height(static_compression(4.)), 0.],
        [0.; 3],
        desc,
    );
    w.set_vehicle_gear(CAR, 1).unwrap();
    w.set_vehicle_controls(CAR, controls(1., 0., 0., 0.))
        .unwrap();
    let mut slip_low = 0.;
    let mut slip_high = 0.;
    for step in 0..(4. / DT) as usize {
        w.step(DT).unwrap();
        // Keep the gearbox in first gear for the comparison.
        if step % 30 == 0 {
            w.set_vehicle_gear(CAR, 1).unwrap();
        }
        let wheels = w.vehicle_wheel_states(CAR).unwrap();
        slip_low = wheels[2].slip_ratio;
        slip_high = wheels[3].slip_ratio;
    }
    let wheels = w.vehicle_wheel_states(CAR).unwrap();
    let z = w.pose(CAR).unwrap().position[2];
    (
        wheels[2].angular_velocity,
        wheels[3].angular_velocity,
        z,
        slip_low.abs() - slip_high.abs(),
    )
}

#[test]
fn open_and_locked_differentials_differ_on_split_mu() {
    let (open_low, open_high, open_z, open_slip) = split_mu_run(DifferentialDesc::Open);
    let (locked_low, locked_high, locked_z, _) = split_mu_run(DifferentialDesc::Locked);
    // Open: the wheel on ice (x = -0.8, left slab) spins much faster than the gripping wheel.
    assert!(open_low > 2. * open_high, "open {open_low} vs {open_high}");
    assert!(open_slip > 0.3, "slip difference {open_slip}");
    // Locked: one speed for both.
    assert!(
        (locked_low - locked_high).abs() < 1e-3 * locked_low.abs().max(1.),
        "locked {locked_low} vs {locked_high}"
    );
    // The locked axle uses the grip of the good wheel and covers clearly more ground.
    assert!(
        locked_z > 1.5 * open_z,
        "locked {locked_z} m vs open {open_z} m"
    );
}

#[test]
fn drop_lands_without_nan_and_with_bounded_suspension_force() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    let desc = car_desc();
    let max_force = desc.wheels[0].max_force;
    spawn(&mut w, [0., 3., 0.], [0.; 3], desc);
    let mut peak = 0.0_f32;
    let mut kinds = Vec::new();
    for _ in 0..(6. / DT) as usize {
        w.step(DT).unwrap();
        let pose = w.pose(CAR).unwrap();
        assert!(pose.position.iter().all(|v| v.is_finite()));
        assert!(pose.rotation.iter().all(|v| v.is_finite()));
        for wheel in w.vehicle_wheel_states(CAR).unwrap() {
            assert!(wheel.suspension_force.is_finite());
            peak = peak.max(wheel.suspension_force);
            assert!(wheel.angular_velocity.is_finite());
        }
        kinds.extend(w.drain_vehicle_events().into_iter().map(|e| e.kind));
    }
    assert!(peak <= max_force, "peak {peak}");
    assert!(
        peak > MASS * G / 4.,
        "landing should exceed the static load"
    );
    let expected = ride_height(static_compression(4.));
    assert!((w.pose(CAR).unwrap().position[1] - expected).abs() < 1e-2);
    assert!(kinds.contains(&VehicleEventKind::Airborne));
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, VehicleEventKind::Landed { impact_speed } if *impact_speed > 5.))
    );
    let started = kinds
        .iter()
        .filter(|k| matches!(k, VehicleEventKind::WheelContact { started: true, .. }))
        .count();
    assert!(started >= 4);
}

#[test]
fn manual_gearbox_shifts_only_on_request() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    let desc = VehicleDesc {
        gearbox: GearboxDesc {
            mode: GearboxMode::Manual,
            ..car_desc().gearbox
        },
        ..car_desc()
    };
    spawn(&mut w, [0., ride_height(0.), 0.], [0.; 3], desc);
    w.set_vehicle_controls(CAR, controls(1., 0., 0., 0.))
        .unwrap();
    run(&mut w, 1.);
    // Neutral: the engine revs but nothing moves.
    assert_eq!(w.vehicle_state(CAR).unwrap().gear, 0);
    assert!(w.vehicle_state(CAR).unwrap().speed.abs() < 0.05);
    w.vehicle_shift_up(CAR).unwrap();
    run(&mut w, 6.);
    let state = w.vehicle_state(CAR).unwrap();
    assert_eq!(state.gear, 1, "manual mode never shifts by itself");
    assert!(state.speed > 5.);
    assert!(state.engine_rpm > 4000.);
    w.set_vehicle_gear(CAR, 9).unwrap_err();
    w.vehicle_shift_up(CAR).unwrap();
    run(&mut w, 0.1);
    assert_eq!(w.vehicle_state(CAR).unwrap().gear, 2);
    w.set_vehicle_gear(CAR, -1).unwrap();
    run(&mut w, 0.1);
    assert_eq!(w.vehicle_state(CAR).unwrap().gear, -1);
}

#[test]
fn automatic_gearbox_reverses_from_a_standstill_with_the_brake_pedal() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    spawn(&mut w, [0., ride_height(0.), 0.], [0.; 3], car_desc());
    w.set_vehicle_controls(CAR, controls(0., 1., 0., 0.))
        .unwrap();
    run(&mut w, 4.);
    let state = w.vehicle_state(CAR).unwrap();
    assert_eq!(state.gear, -1);
    assert!(state.speed < -1., "reversing at {}", state.speed);
}

#[test]
fn handbrake_locks_the_rear_wheels() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    let mut desc = car_desc();
    desc.wheels[2].handbrake = true;
    desc.wheels[3].handbrake = true;
    desc.wheels[0].brake = false;
    desc.wheels[1].brake = false;
    spawn(&mut w, [0., ride_height(0.), 0.], [0., 0., 15.], desc);
    w.vehicle_roll_start(CAR).unwrap();
    run(&mut w, 0.3);
    w.set_vehicle_controls(CAR, controls(0., 0., 0., 1.))
        .unwrap();
    run(&mut w, 0.5);
    let wheels = w.vehicle_wheel_states(CAR).unwrap();
    assert!(wheels[2].angular_velocity.abs() < 1e-3);
    assert!(wheels[3].angular_velocity.abs() < 1e-3);
    assert!(wheels[0].angular_velocity > 20.);
}

#[test]
fn identical_runs_are_bitwise_equal() {
    let run_once = || {
        let mut w = world();
        floor(&mut w, GROUND, 0., 200., 0.5);
        spawn(&mut w, [0., 1.0, 0.], [0.; 3], car_desc());
        for step in 0..600 {
            let steer = ((step as f32) * 0.02).sin();
            w.set_vehicle_controls(CAR, controls(0.8, 0., steer, 0.))
                .unwrap();
            w.step(DT).unwrap();
        }
        let pose = w.pose(CAR).unwrap();
        let wheels = w.vehicle_wheel_states(CAR).unwrap();
        let mut bits: Vec<u32> = pose
            .position
            .iter()
            .chain(pose.rotation.iter())
            .map(|v| v.to_bits())
            .collect();
        bits.extend(wheels.iter().map(|s| s.angular_velocity.to_bits()));
        bits.extend(wheels.iter().map(|s| s.suspension_force.to_bits()));
        bits
    };
    assert_eq!(run_once(), run_once());
}

#[test]
fn wheel_poses_follow_steer_spin_and_suspension() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    spawn(&mut w, [0., ride_height(0.), 0.], [0.; 3], car_desc());
    w.set_vehicle_controls(CAR, controls(0.3, 0., 1., 0.))
        .unwrap();
    run(&mut w, 2.);
    let wheels = w.vehicle_wheel_states(CAR).unwrap();
    // Front wheels steer, rear wheels do not.
    assert!(wheels[0].steer_angle > 0.19 && wheels[2].steer_angle == 0.);
    // Wheel centre sits one radius above the contact point.
    for wheel in &wheels {
        let rise = wheel.position[1] - wheel.contact_point[1];
        assert!(
            (rise - RADIUS).abs() < 0.02,
            "wheel centre {rise} above contact"
        );
    }
    // The driven wheels turn forward.
    assert!(wheels[2].angular_velocity > 0.);
}

#[test]
fn wheels_report_the_ground_body_and_validation_rejects_bad_input() {
    let mut w = world();
    floor(&mut w, GROUND, 0., 200., 0.5);
    spawn(&mut w, [0., ride_height(0.), 0.], [0.; 3], car_desc());
    run(&mut w, 1.);
    for wheel in w.vehicle_wheel_states(CAR).unwrap() {
        assert_eq!(wheel.ground_body, Some(GROUND));
        assert!((wheel.ground_friction - 0.5).abs() < 1e-6);
    }
    assert!(w.is_vehicle(CAR));
    assert_eq!(w.vehicle_ids(), vec![CAR]);
    // Invalid controls and descriptions are rejected with errors.
    w.set_vehicle_controls(CAR, controls(2., 0., 0., 0.))
        .unwrap_err();
    w.set_vehicle_controls(CAR, controls(0., 0., f32::NAN, 0.))
        .unwrap_err();
    let mut few = car_desc();
    few.wheels.truncate(1);
    assert!(w.set_vehicle_desc(CAR, few).is_err());
    let mut backwards = car_desc();
    backwards.wheels[0].axle = [-1., 0., 0.];
    assert!(w.set_vehicle_desc(CAR, backwards).is_err());
    let mut weak = car_desc();
    weak.wheels[1].max_travel = 5.;
    assert!(w.set_vehicle_desc(CAR, weak).is_err());
    let mut bad_bar = car_desc();
    bad_bar.anti_roll.push(engine_physics::dim3::AntiRollBar {
        wheel_a: 0,
        wheel_b: 9,
        stiffness: 1.,
    });
    assert!(w.set_vehicle_desc(CAR, bad_bar).is_err());
    // Vehicles need dynamic bodies and unique ids.
    assert!(w.insert_vehicle(GROUND, car_desc()).is_err());
    assert!(w.insert_vehicle(CAR, car_desc()).is_err());
    // Removing the body removes the vehicle.
    assert!(w.remove(CAR));
    assert!(!w.is_vehicle(CAR));
}

#[test]
fn anti_roll_bar_reduces_body_roll_in_a_turn() {
    let roll_after_turn = |stiffness: f32| {
        let mut w = world();
        floor(&mut w, GROUND, 0., 400., 0.5);
        let mut desc = car_desc();
        desc.steering.max_angle = 0.3;
        if stiffness > 0. {
            desc.anti_roll = vec![
                engine_physics::dim3::AntiRollBar {
                    wheel_a: 0,
                    wheel_b: 1,
                    stiffness,
                },
                engine_physics::dim3::AntiRollBar {
                    wheel_a: 2,
                    wheel_b: 3,
                    stiffness,
                },
            ];
        }
        spawn(
            &mut w,
            [0., ride_height(static_compression(4.)), 0.],
            [0., 0., 12.],
            desc,
        );
        w.vehicle_roll_start(CAR).unwrap();
        let mut roll = 0.0_f32;
        for _ in 0..(3. / DT) as usize {
            let speed = w.vehicle_state(CAR).unwrap().speed;
            let throttle = ((12. - speed) * 0.3).clamp(0., 0.5);
            w.set_vehicle_controls(CAR, controls(throttle, 0., 1., 0.))
                .unwrap();
            w.step(DT).unwrap();
            // Roll about the chassis forward axis: sideways tilt of the chassis up vector.
            let q = w.pose(CAR).unwrap().rotation;
            let up_x = 2. * (q[0] * q[1] - q[2] * q[3]);
            roll = roll.max(up_x.abs());
        }
        roll
    };
    let soft = roll_after_turn(0.);
    let stiff = roll_after_turn(60_000.);
    assert!(soft > 0.01, "the turn should roll the body, got {soft}");
    assert!(stiff < soft, "anti-roll {stiff} should be below {soft}");
}
