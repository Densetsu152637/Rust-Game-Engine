//! Measured behaviour of the extended joint set in both dimensions.
use engine_physics::{
    AxisMode, BodyKind, BreakLimits, JointOptions, Material, MotorDesc, World2D, World3D, dim2,
    dim3,
};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

const DT: f32 = 1. / 120.;

fn mass_density(mass: f32, volume: f32) -> Material {
    Material {
        density: mass / volume,
        ..Material::default()
    }
}
fn add3(world: &mut World3D, id: u64, kind: BodyKind, position: [f32; 3], mass: f32) {
    let volume = 4. / 3. * PI * 0.2f32.powi(3);
    world
        .insert(
            id,
            dim3::BodyDesc {
                kind,
                position,
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.2 },
            mass_density(mass, volume),
        )
        .unwrap();
}
fn add2(world: &mut World2D, id: u64, kind: BodyKind, position: [f32; 2], mass: f32) {
    let volume = PI * 0.2f32.powi(2);
    world
        .insert(
            id,
            dim2::BodyDesc {
                kind,
                position,
                ..Default::default()
            },
            dim2::Shape::Ball { radius: 0.2 },
            mass_density(mass, volume),
        )
        .unwrap();
}
fn pair3(gravity: [f32; 3], at: [f32; 3], mass: f32) -> World3D {
    let mut world = World3D::new(gravity, 8).unwrap();
    add3(&mut world, 1, BodyKind::Fixed, [0.; 3], 1.);
    add3(&mut world, 2, BodyKind::Dynamic, at, mass);
    world
}
fn pair2(gravity: [f32; 2], at: [f32; 2], mass: f32) -> World2D {
    let mut world = World2D::new(gravity, 8).unwrap();
    add2(&mut world, 1, BodyKind::Fixed, [0.; 2], 1.);
    add2(&mut world, 2, BodyKind::Dynamic, at, mass);
    world
}
fn run3(world: &mut World3D, seconds: f32) {
    for _ in 0..(seconds / DT) as usize {
        world.step(DT).unwrap();
    }
}
fn run2(world: &mut World2D, seconds: f32) {
    for _ in 0..(seconds / DT) as usize {
        world.step(DT).unwrap();
    }
}
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let u = [x, y, z];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross(u, v).map(|c| 2. * c);
    let c = cross(u, t);
    [
        v[0] + w * t[0] + c[0],
        v[1] + w * t[1] + c[1],
        v[2] + w * t[2] + c[2],
    ]
}
fn quat_z(angle: f32) -> [f32; 4] {
    [0., 0., (angle / 2.).sin(), (angle / 2.).cos()]
}

#[test]
fn prismatic_3d_moves_only_along_axis_and_stops_at_limits() {
    let mut world = pair3([0., -9.81, 0.], [0.; 3], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Prismatic {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                axis: [2., 0., 0.],
                limits: Some([-0.5, 0.5]),
            },
        )
        .unwrap();
    // Gravity and an off-axis kick must not move the body off the rail.
    world.apply_impulse(2, [0., 3., 2.]).unwrap();
    run3(&mut world, 1.);
    let p = world.pose(2).unwrap();
    assert!(
        p.position[1].abs() < 1e-3 && p.position[2].abs() < 1e-3,
        "{p:?}"
    );
    world.apply_impulse(2, [6., 0., 0.]).unwrap();
    run3(&mut world, 1.5);
    let p = world.pose(2).unwrap();
    assert!((p.position[0] - 0.5).abs() < 0.02, "{p:?}");
    assert!(
        p.position[1].abs() < 1e-3 && p.position[2].abs() < 1e-3,
        "{p:?}"
    );
    world.apply_impulse(2, [-20., 0., 0.]).unwrap();
    run3(&mut world, 1.5);
    let p = world.pose(2).unwrap();
    assert!((p.position[0] + 0.5).abs() < 0.02, "{p:?}");
    let state = world.joint_state(10).unwrap();
    assert!((state.position[0] + 0.5).abs() < 0.02, "{state:?}");
    // Runtime limit edit takes effect without a rebuild.
    world
        .set_joint_limits(10, dim3::Dof::LinX, Some([-0.1, 0.1]))
        .unwrap();
    run3(&mut world, 1.);
    assert!(world.pose(2).unwrap().position[0] > -0.15);
    assert_eq!(world.joint_limits(10, dim3::Dof::LinX), Some([-0.1, 0.1]));
    world.set_joint_limits(10, dim3::Dof::LinX, None).unwrap();
    assert_eq!(world.joint_limits(10, dim3::Dof::LinX), None);
}

#[test]
fn prismatic_2d_moves_only_along_axis_and_stops_at_limits() {
    let mut world = pair2([0., -9.81], [0.; 2], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim2::JointDesc::Prismatic {
                anchor1: [0.; 2],
                anchor2: [0.; 2],
                axis: [1., 0.],
                limits: Some([-0.5, 0.5]),
            },
        )
        .unwrap();
    world.apply_impulse(2, [0., 3.]).unwrap();
    run2(&mut world, 1.);
    let p = world.pose(2).unwrap();
    assert!(
        p.position[1].abs() < 1e-3 && p.rotation.abs() < 1e-3,
        "{p:?}"
    );
    world.apply_impulse(2, [6., 0.]).unwrap();
    run2(&mut world, 1.5);
    let p = world.pose(2).unwrap();
    assert!((p.position[0] - 0.5).abs() < 0.02, "{p:?}");
    assert!(p.position[1].abs() < 1e-3, "{p:?}");
    world.apply_angular_impulse(2, 1.).unwrap();
    run2(&mut world, 0.5);
    assert!(world.pose(2).unwrap().rotation.abs() < 1e-3);
}

#[test]
fn spherical_keeps_anchors_together_and_respects_the_cone() {
    // Rod of length 1 along +X hanging from the origin; gravity swings it down.
    let build = |swing: Option<f32>| {
        let mut world = pair3([0., -9.81, 0.], [1., 0., 0.], 1.);
        world
            .insert_joint(
                10,
                1,
                2,
                dim3::JointDesc::Spherical {
                    anchor1: [0.; 3],
                    anchor2: [-1., 0., 0.],
                    axis: [1., 0., 0.],
                    swing_limit: swing,
                    twist_limits: None,
                },
            )
            .unwrap();
        world
    };
    let separation = |world: &World3D| {
        let p = world.pose(2).unwrap();
        let r = rotate(p.rotation, [-1., 0., 0.]);
        (0..3)
            .map(|i| (p.position[i] + r[i]).powi(2))
            .sum::<f32>()
            .sqrt()
    };
    let mut free = build(None);
    let mut worst = 0f32;
    for _ in 0..360 {
        free.step(DT).unwrap();
        worst = worst.max(separation(&free));
    }
    assert!(worst < 1e-3, "anchor separation {worst}");
    // Unlimited the rod swings well past 0.5 rad from +X.
    let p = free.pose(2).unwrap();
    let angle_free = p.position[1].abs().atan2(p.position[0]);
    let mut world = build(Some(0.5));
    let mut widest = 0f32;
    for _ in 0..480 {
        world.step(DT).unwrap();
        let p = world.pose(2).unwrap();
        widest = widest.max(p.position[1].abs().atan2(p.position[0]));
        assert!(separation(&world) < 1e-3);
    }
    assert!(widest < 0.5 + 0.03, "cone exceeded: {widest}");
    assert!(widest > 0.45, "cone not reached: {widest}");
    let _ = angle_free;
}

#[test]
fn spherical_twist_limits_bound_rotation_about_the_axis() {
    let mut world = pair3([0.; 3], [0.; 3], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Spherical {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                axis: [1., 0., 0.],
                swing_limit: None,
                twist_limits: Some([-0.3, 0.3]),
            },
        )
        .unwrap();
    world.apply_angular_impulse(2, [0.2, 0., 0.]).unwrap();
    run3(&mut world, 1.5);
    let twist = world.joint_state(10).unwrap().angles[0];
    assert!(twist.abs() < 0.3 + 0.03 && twist > 0.25, "{twist}");
}

#[test]
fn spring_oscillation_period_matches_theory() {
    let (mass, k) = (1.5f32, 60.);
    let expected = TAU * (mass / k).sqrt();
    let measure3 = || {
        let mut world = pair3([0.; 3], [1.4, 0., 0.], mass);
        world
            .insert_joint(
                10,
                1,
                2,
                dim3::JointDesc::Spring {
                    anchor1: [0.; 3],
                    anchor2: [0.; 3],
                    rest_length: 1.,
                    stiffness: k,
                    damping: 0.,
                },
            )
            .unwrap();
        let mut crossings = Vec::new();
        let mut previous = 0.4;
        for step in 1..2400 {
            world.step(1. / 240.).unwrap();
            let x = world.pose(2).unwrap().position[0] - 1.;
            if previous > 0. && x <= 0. {
                crossings.push(step as f32 / 240. - x / (x - previous) / 240.);
            }
            previous = x;
        }
        assert!(crossings.len() >= 3, "{crossings:?}");
        (crossings[2] - crossings[0]) / 2.
    };
    let period = measure3();
    assert!(
        (period - expected).abs() / expected < 0.05,
        "period {period} expected {expected}"
    );
    let mut world = pair2([0.; 2], [1.4, 0.], mass);
    world
        .insert_joint(
            10,
            1,
            2,
            dim2::JointDesc::Spring {
                anchor1: [0.; 2],
                anchor2: [0.; 2],
                rest_length: 1.,
                stiffness: k,
                damping: 0.,
            },
        )
        .unwrap();
    let mut crossings = Vec::new();
    let mut previous = 0.4;
    for step in 1..2400 {
        world.step(1. / 240.).unwrap();
        let x = world.pose(2).unwrap().position[0] - 1.;
        if previous > 0. && x <= 0. {
            crossings.push(step as f32 / 240.);
        }
        previous = x;
    }
    let period2 = (crossings[2] - crossings[0]) / 2.;
    assert!(
        (period2 - expected).abs() / expected < 0.05,
        "2D period {period2} expected {expected}"
    );
}

#[test]
fn damped_spring_loses_energy() {
    let mut world = pair3([0.; 3], [1.4, 0., 0.], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Spring {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                rest_length: 1.,
                stiffness: 60.,
                damping: 8.,
            },
        )
        .unwrap();
    run3(&mut world, 4.);
    assert!((world.pose(2).unwrap().position[0] - 1.).abs() < 0.01);
}

#[test]
fn rope_never_exceeds_max_and_allows_slack() {
    let mut world = pair3([0., -9.81, 0.], [0., -0.5, 0.], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Rope {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                max: 1.5,
            },
        )
        .unwrap();
    let mut longest = 0f32;
    let mut fell_freely = false;
    for step in 0..360 {
        world.step(DT).unwrap();
        let p = world.pose(2).unwrap().position;
        let d = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        longest = longest.max(d);
        // Free fall through the slack: 0.5 m takes ~0.32 s; it is well past 0.5 m at 0.2 s.
        if step == 24 && d > 0.5 + 0.1 {
            fell_freely = true;
        }
    }
    assert!(fell_freely, "rope restrained the slack region");
    assert!(longest < 1.5 + 0.02, "rope stretched to {longest}");
    assert!(longest > 1.45);

    let mut world = pair2([0., -9.81], [0., -0.5], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim2::JointDesc::Rope {
                anchor1: [0.; 2],
                anchor2: [0.; 2],
                max: 1.5,
            },
        )
        .unwrap();
    let mut longest = 0f32;
    for _ in 0..360 {
        world.step(DT).unwrap();
        let p = world.pose(2).unwrap().position;
        longest = longest.max((p[0] * p[0] + p[1] * p[1]).sqrt());
    }
    assert!(longest < 1.5 + 0.02 && longest > 1.45, "{longest}");
}

#[test]
fn distance_joint_holds_the_minimum_and_maximum() {
    let mut world = pair3([0.; 3], [1., 0., 0.], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Distance {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                min: 0.8,
                max: 1.2,
            },
        )
        .unwrap();
    world.apply_impulse(2, [-5., 0., 0.]).unwrap();
    run3(&mut world, 1.);
    let d = world.pose(2).unwrap().position[0];
    assert!(d > 0.8 - 0.02 && d < 0.8 + 0.05, "{d}");
    world.apply_impulse(2, [15., 0., 0.]).unwrap();
    run3(&mut world, 1.);
    let d = world.pose(2).unwrap().position[0];
    assert!(d < 1.2 + 0.02 && d > 1.15, "{d}");
    // The linear motor of a joint with a minimum is reserved for the hard stop.
    assert!(
        world
            .set_joint_motor(10, dim3::Dof::LinX, MotorDesc::velocity(1., 1.))
            .is_err()
    );
}

#[test]
fn generic_six_dof_locks_axes_and_follows_joint_frames() {
    let free_x_only = [
        AxisMode::Free,
        AxisMode::Locked,
        AxisMode::Locked,
        AxisMode::Locked,
        AxisMode::Locked,
        AxisMode::Locked,
    ];
    // Frame 1 rotated 90 degrees about Z: the joint's free X axis is world Y.
    let mut world = pair3([0.; 3], [0.; 3], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Generic {
                frame1: dim3::Frame {
                    position: [0.; 3],
                    rotation: quat_z(FRAC_PI_2),
                },
                frame2: dim3::Frame {
                    position: [0.; 3],
                    rotation: quat_z(FRAC_PI_2),
                },
                axes: free_x_only,
            },
        )
        .unwrap();
    world.apply_impulse(2, [3., 0., 0.]).unwrap();
    world.apply_impulse(2, [0., 2., 0.]).unwrap();
    world.apply_angular_impulse(2, [0.1, 0.1, 0.1]).unwrap();
    run3(&mut world, 1.);
    let p = world.pose(2).unwrap();
    assert!(p.position[1] > 1.5, "free axis did not move: {p:?}");
    assert!(
        p.position[0].abs() < 1e-3 && p.position[2].abs() < 1e-3,
        "{p:?}"
    );
    assert!(p.rotation[..3].iter().all(|c| c.abs() < 1e-3), "{p:?}");

    // Free angular Z only (a hinge written as 6-DoF) with limits on the other angular axes.
    let mut world = pair3([0.; 3], [0.; 3], 1.);
    world
        .insert_joint(
            11,
            1,
            2,
            dim3::JointDesc::Generic {
                frame1: dim3::Frame::default(),
                frame2: dim3::Frame::default(),
                axes: [
                    AxisMode::Locked,
                    AxisMode::Locked,
                    AxisMode::Locked,
                    AxisMode::Locked,
                    AxisMode::Locked,
                    AxisMode::Free,
                ],
            },
        )
        .unwrap();
    world.apply_angular_impulse(2, [0.05, 0.05, 0.05]).unwrap();
    run3(&mut world, 1.);
    let state = world.joint_state(11).unwrap();
    assert!(state.angles[2].abs() > 0.2, "{state:?}");
    assert!(
        state.angles[0].abs() < 1e-3 && state.angles[1].abs() < 1e-3,
        "{state:?}"
    );

    let mut world = pair2([0.; 2], [0.; 2], 1.);
    world
        .insert_joint(
            12,
            1,
            2,
            dim2::JointDesc::Generic {
                frame1: dim2::Frame::default(),
                frame2: dim2::Frame::default(),
                axes: [
                    AxisMode::Locked,
                    AxisMode::Free,
                    AxisMode::Limited {
                        min: -0.2,
                        max: 0.2,
                    },
                ],
            },
        )
        .unwrap();
    world.apply_impulse(2, [2., 2.]).unwrap();
    world.apply_angular_impulse(2, 0.05).unwrap();
    run2(&mut world, 1.5);
    let p = world.pose(2).unwrap();
    assert!(p.position[0].abs() < 1e-3 && p.position[1] > 1.5, "{p:?}");
    assert!(p.rotation.abs() < 0.2 + 0.03, "{p:?}");
}

#[test]
fn velocity_and_position_motors_drive_a_hinge() {
    let hinge3 = |world: &mut World3D| {
        world
            .insert_joint(
                10,
                1,
                2,
                dim3::JointDesc::Revolute {
                    anchor1: [0.; 3],
                    anchor2: [0.; 3],
                    axis: [0., 0., 1.],
                    limits: None,
                },
            )
            .unwrap();
    };
    let mut world = pair3([0.; 3], [0.; 3], 1.);
    hinge3(&mut world);
    world
        .set_joint_motor(10, dim3::Dof::AngX, MotorDesc::velocity(2.5, 50.))
        .unwrap();
    run3(&mut world, 1.5);
    let w = world.velocity(2).unwrap().angular[2];
    assert!((w - 2.5).abs() < 0.1, "angular velocity {w}");
    assert_eq!(
        world
            .joint_motor(10, dim3::Dof::AngX)
            .unwrap()
            .target_velocity,
        2.5
    );
    world.clear_joint_motor(10, dim3::Dof::AngX).unwrap();
    assert!(world.joint_motor(10, dim3::Dof::AngX).is_none());

    let mut world = pair3([0.; 3], [0.; 3], 1.);
    hinge3(&mut world);
    world
        .set_joint_motor(10, dim3::Dof::AngX, MotorDesc::position(0.8, 200., 30.))
        .unwrap();
    run3(&mut world, 3.);
    let angle = world.joint_state(10).unwrap().angles[0];
    assert!((angle - 0.8).abs() < 0.03, "angle {angle}");
    // Retarget in place.
    world
        .set_joint_motor(10, dim3::Dof::AngX, MotorDesc::position(-0.4, 200., 30.))
        .unwrap();
    run3(&mut world, 3.);
    let angle = world.joint_state(10).unwrap().angles[0];
    assert!((angle + 0.4).abs() < 0.03, "angle {angle}");

    let mut world = pair2([0.; 2], [0.; 2], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim2::JointDesc::Revolute {
                anchor1: [0.; 2],
                anchor2: [0.; 2],
                limits: None,
            },
        )
        .unwrap();
    world
        .set_joint_motor(10, dim2::Dof::Ang, MotorDesc::velocity(-1.5, 50.))
        .unwrap();
    run2(&mut world, 1.5);
    let w = world.velocity(2).unwrap().angular;
    assert!((w + 1.5).abs() < 0.1, "2D angular velocity {w}");
    world
        .set_joint_motor(10, dim2::Dof::Ang, MotorDesc::position(0.5, 200., 30.))
        .unwrap();
    run2(&mut world, 3.);
    let angle = world.pose(2).unwrap().rotation;
    assert!((angle - 0.5).abs() < 0.03, "2D angle {angle}");
}

#[test]
fn prismatic_motor_respects_max_force() {
    let mut world = pair3([0.; 3], [0.; 3], 2.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Prismatic {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                axis: [1., 0., 0.],
                limits: None,
            },
        )
        .unwrap();
    world
        .set_joint_motor(
            10,
            dim3::Dof::LinX,
            MotorDesc {
                target_velocity: 100.,
                damping: 1000.,
                max_force: 4.,
                force_based: true,
                ..MotorDesc::default()
            },
        )
        .unwrap();
    run3(&mut world, 1.);
    // a = F/m = 2 m/s^2 -> v = 2 m/s after one second.
    let v = world.velocity(2).unwrap().linear[0];
    assert!((v - 2.).abs() < 0.2, "{v}");
}

fn hanging(mass: f32, limits: BreakLimits) -> World3D {
    let mut world = pair3([0., -9.81, 0.], [0., -1., 0.], mass);
    world
        .insert_joint_with(
            10,
            1,
            2,
            dim3::JointDesc::Fixed {
                anchor1: [0., -1., 0.],
                anchor2: [0.; 3],
            },
            JointOptions {
                break_limits: limits,
                contacts_enabled: false,
            },
        )
        .unwrap();
    world
}

#[test]
fn breakable_joint_holds_below_and_breaks_above_the_force_threshold() {
    let limits = BreakLimits {
        force: Some(100.),
        torque: None,
    };
    let mut light = hanging(5., limits);
    run3(&mut light, 2.);
    assert!(light.has_joint(10) && light.drain_joint_broken().is_empty());
    let load = light.joint_state(10).unwrap();
    let expected = 5. * 9.81;
    assert!(
        (load.force[1].abs() - expected).abs() / expected < 0.05,
        "{load:?}"
    );
    assert!(light.pose(2).unwrap().position[1] > -1.01);

    let mut heavy = hanging(20., limits);
    run3(&mut heavy, 0.5);
    assert!(!heavy.has_joint(10));
    let events = heavy.drain_joint_broken();
    assert_eq!(events.len(), 1, "{events:?}");
    let event = events[0];
    assert_eq!((event.id, event.first, event.second), (10, 1, 2));
    let weight = 20. * 9.81;
    assert!(
        (event.force - weight).abs() / weight < 0.1,
        "force {} expected {weight}",
        event.force
    );
    assert!(heavy.drain_joint_broken().is_empty());
    assert_eq!(heavy.broken_joint(10), Some(event));
    assert_eq!(heavy.broken_joints(), vec![event]);
    // The body is free and falls.
    assert!(heavy.pose(2).unwrap().position[1] < -2.);
    // A broken ID can be re-created and clears the record.
    heavy
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Fixed {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
            },
        )
        .unwrap();
    assert!(heavy.broken_joint(10).is_none());
}

#[test]
fn breakable_joint_breaks_on_torque() {
    // Body hangs 1 m to the side of the anchor: torque m*g*L at the fixed joint.
    let mut world = pair3([0., -9.81, 0.], [1., 0., 0.], 4.);
    world
        .insert_joint_with(
            10,
            1,
            2,
            dim3::JointDesc::Fixed {
                anchor1: [0.; 3],
                anchor2: [-1., 0., 0.],
            },
            JointOptions {
                break_limits: BreakLimits {
                    force: None,
                    torque: Some(20.),
                },
                contacts_enabled: false,
            },
        )
        .unwrap();
    run3(&mut world, 0.5);
    let events = world.drain_joint_broken();
    assert_eq!(events.len(), 1);
    let expected = 4. * 9.81;
    assert!(
        (events[0].torque - expected).abs() / expected < 0.15,
        "{:?}",
        events[0]
    );

    let mut sturdy = pair3([0., -9.81, 0.], [1., 0., 0.], 4.);
    sturdy
        .insert_joint_with(
            10,
            1,
            2,
            dim3::JointDesc::Fixed {
                anchor1: [0.; 3],
                anchor2: [-1., 0., 0.],
            },
            JointOptions {
                break_limits: BreakLimits {
                    force: None,
                    torque: Some(80.),
                },
                contacts_enabled: false,
            },
        )
        .unwrap();
    run3(&mut sturdy, 1.);
    assert!(sturdy.has_joint(10));
}

#[test]
fn breakable_joint_2d_and_runtime_threshold_edit() {
    let mut world = pair2([0., -9.81], [0., -1.], 10.);
    world
        .insert_joint_with(
            10,
            1,
            2,
            dim2::JointDesc::Fixed {
                anchor1: [0., -1.],
                anchor2: [0.; 2],
            },
            JointOptions {
                break_limits: BreakLimits {
                    force: Some(300.),
                    torque: None,
                },
                contacts_enabled: false,
            },
        )
        .unwrap();
    run2(&mut world, 1.);
    assert!(world.has_joint(10));
    world
        .set_joint_break_limits(
            10,
            BreakLimits {
                force: Some(50.),
                torque: None,
            },
        )
        .unwrap();
    run2(&mut world, 0.2);
    assert!(!world.has_joint(10));
    let event = world.drain_joint_broken()[0];
    assert!((event.force - 98.1).abs() < 10., "{event:?}");
}

#[test]
fn joint_edits_validate_and_report_unknown_joints() {
    let mut world = pair3([0.; 3], [0.; 3], 1.);
    world
        .insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Prismatic {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                axis: [1., 0., 0.],
                limits: None,
            },
        )
        .unwrap();
    let bad = MotorDesc {
        stiffness: -1.,
        ..MotorDesc::default()
    };
    assert!(world.set_joint_motor(10, dim3::Dof::LinX, bad).is_err());
    assert!(
        world
            .set_joint_motor(10, dim3::Dof::LinX, MotorDesc::position(f32::NAN, 1., 1.))
            .is_err()
    );
    // LinY is locked on a prismatic joint.
    assert!(
        world
            .set_joint_motor(10, dim3::Dof::LinY, MotorDesc::velocity(1., 1.))
            .is_err()
    );
    assert!(
        world
            .set_joint_limits(10, dim3::Dof::LinY, Some([0., 1.]))
            .is_err()
    );
    assert!(
        world
            .set_joint_limits(10, dim3::Dof::LinX, Some([1., 0.]))
            .is_err()
    );
    assert!(
        world
            .set_joint_motor(99, dim3::Dof::LinX, MotorDesc::default())
            .is_err()
    );
    assert!(
        world
            .set_joint_break_limits(
                10,
                BreakLimits {
                    force: Some(-1.),
                    torque: None
                }
            )
            .is_err()
    );
    assert!(world.joint_state(99).is_none());
    let invalid = [
        dim3::JointDesc::Rope {
            anchor1: [0.; 3],
            anchor2: [0.; 3],
            max: 0.,
        },
        dim3::JointDesc::Distance {
            anchor1: [0.; 3],
            anchor2: [0.; 3],
            min: 2.,
            max: 1.,
        },
        dim3::JointDesc::Spring {
            anchor1: [0.; 3],
            anchor2: [0.; 3],
            rest_length: 1.,
            stiffness: -1.,
            damping: 0.,
        },
        dim3::JointDesc::Spherical {
            anchor1: [0.; 3],
            anchor2: [0.; 3],
            axis: [0.; 3],
            swing_limit: None,
            twist_limits: None,
        },
        dim3::JointDesc::Spherical {
            anchor1: [0.; 3],
            anchor2: [0.; 3],
            axis: [1., 0., 0.],
            swing_limit: Some(4.),
            twist_limits: None,
        },
        dim3::JointDesc::Generic {
            frame1: dim3::Frame {
                position: [0.; 3],
                rotation: [0., 0., 0., 2.],
            },
            frame2: dim3::Frame::default(),
            axes: [AxisMode::Free; 6],
        },
        dim3::JointDesc::Generic {
            frame1: dim3::Frame::default(),
            frame2: dim3::Frame::default(),
            axes: [
                AxisMode::Free,
                AxisMode::Free,
                AxisMode::Free,
                AxisMode::Limited { min: 0., max: 9. },
                AxisMode::Free,
                AxisMode::Free,
            ],
        },
    ];
    for (index, desc) in invalid.into_iter().enumerate() {
        assert!(
            world.insert_joint(20 + index as u64, 1, 2, desc).is_err(),
            "invalid joint {index} accepted"
        );
    }
    assert_eq!(world.joint_count(), 1);
    assert_eq!(world.joint_ids(), vec![10]);
    assert_eq!(world.joint_bodies(10), Some((1, 2)));
}

#[test]
fn break_records_survive_body_removal_until_the_joint_is_removed() {
    let mut world = hanging(
        20.,
        BreakLimits {
            force: Some(50.),
            torque: None,
        },
    );
    run3(&mut world, 0.2);
    assert!(world.broken_joint(10).is_some());
    assert!(world.remove(2));
    assert!(world.broken_joint(10).is_some());
    assert!(!world.remove_joint(10), "a broken joint is no longer live");
    assert!(world.broken_joint(10).is_none());
}
