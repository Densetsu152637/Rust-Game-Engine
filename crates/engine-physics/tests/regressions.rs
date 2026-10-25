//! Regression guards from the physics critic review: sticky distance minimum, impulse scaling
//! with the solver iteration count, and break-history eviction order.
use engine_physics::{BodyKind, BreakLimits, JointOptions, Material, World3D, dim3};

const DT: f32 = 1. / 120.;
const G: f32 = 9.81;

fn ball(world: &mut World3D, id: u64, kind: BodyKind, position: [f32; 3]) {
    world
        .insert(
            id,
            dim3::BodyDesc {
                kind,
                position,
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.1 },
            Material {
                density: 1000.,
                ..Material::default()
            },
        )
        .unwrap();
}
fn distance(world: &World3D, a: u64, b: u64) -> f32 {
    let (a, b) = (
        world.pose(a).unwrap().position,
        world.pose(b).unwrap().position,
    );
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// S2: a vertical distance joint resting at its minimum while gravity pulls the bodies apart
/// must separate and reach its maximum; the minimum is a one-sided stop.
#[test]
fn a_distance_joint_resting_at_its_minimum_separates_under_gravity() {
    let mut world = World3D::new([0., -G, 0.], 8).unwrap();
    ball(&mut world, 1, BodyKind::Fixed, [0., 0., 0.]);
    ball(&mut world, 2, BodyKind::Dynamic, [0., -0.8, 0.]);
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
    let mut farthest = 0f32;
    for _ in 0..240 {
        world.step(DT).unwrap();
        farthest = farthest.max(distance(&world, 1, 2));
    }
    let end = distance(&world, 1, 2);
    println!("distance after 2 s hanging: end {end:.4}, farthest {farthest:.4}");
    assert!(
        (end - 1.2).abs() < 0.03,
        "the body hangs at the maximum, not the minimum: {end}"
    );
    // The stop still works in the approach direction.
    world.apply_impulse(2, [0., 8., 0.]).unwrap();
    let mut nearest = f32::MAX;
    for _ in 0..120 {
        world.step(DT).unwrap();
        nearest = nearest.min(distance(&world, 1, 2));
    }
    println!("nearest approach after an upward impulse: {nearest:.4}");
    assert!(nearest > 0.8 - 0.03, "{nearest}");
}

/// S2 (guard): the same stop holds a load that presses the bodies together, with no creep.
#[test]
fn a_distance_joint_minimum_holds_a_pressing_load_without_creep() {
    let mut world = World3D::new([0., -G, 0.], 8).unwrap();
    ball(&mut world, 1, BodyKind::Fixed, [0., 0., 0.]);
    ball(&mut world, 2, BodyKind::Dynamic, [0., 0.8, 0.]);
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
    let mut nearest = f32::MAX;
    for _ in 0..360 {
        world.step(DT).unwrap();
        nearest = nearest.min(distance(&world, 1, 2));
    }
    let end = distance(&world, 1, 2);
    println!("pressing load: end {end:.4}, nearest {nearest:.4}");
    assert!(nearest > 0.8 - 0.01 && end < 0.8 + 0.02, "{nearest} {end}");
}

fn ground(world: &mut World3D) {
    world
        .insert(
            1,
            dim3::BodyDesc {
                kind: BodyKind::Fixed,
                position: [0., -0.5, 0.],
                ..Default::default()
            },
            dim3::Shape::Box {
                half_extents: [10., 0.5, 10.],
            },
            Material::default(),
        )
        .unwrap();
}

/// S3: the per-step contact impulse of a resting load is `m g dt` whatever the solver
/// iteration count, so force-mode thresholds do not depend on `solver_iterations`.
#[test]
fn resting_contact_impulse_is_independent_of_solver_iterations() {
    let mut measured = vec![];
    for iterations in [1, 4, 16, 64] {
        let mut world = World3D::new([0., -G, 0.], 8).unwrap();
        world.set_solver_iterations(iterations).unwrap();
        ground(&mut world);
        world
            .insert(
                2,
                dim3::BodyDesc {
                    position: [0., 0.5, 0.],
                    ..Default::default()
                },
                dim3::Shape::Box {
                    half_extents: [0.5; 3],
                },
                Material::default(),
            )
            .unwrap();
        let mass = world.mass(2).unwrap();
        for _ in 0..120 {
            world.step(DT).unwrap();
        }
        let impulse = world.contact_impulse(2).unwrap().impulse;
        let expected = mass * G * DT;
        println!("iterations {iterations}: impulse {impulse:.5}, m g dt {expected:.5}");
        measured.push((iterations, impulse / expected));
    }
    for (iterations, ratio) in measured {
        assert!(
            (ratio - 1.).abs() < 0.1,
            "{iterations} iterations measure {ratio:.3} x m g dt"
        );
    }
}

/// S3 (impacts): the impulses of a landing add up to the momentum lost, at any iteration count.
#[test]
fn landing_impulse_equals_the_momentum_lost_at_any_iteration_count() {
    for iterations in [1, 4, 16, 64] {
        let mut world = World3D::new([0.; 3], 8).unwrap();
        world.set_solver_iterations(iterations).unwrap();
        ground(&mut world);
        world
            .insert(
                2,
                dim3::BodyDesc {
                    position: [0., 1.5, 0.],
                    velocity: [0., -8., 0.],
                    ccd: true,
                    ..Default::default()
                },
                dim3::Shape::Ball { radius: 0.5 },
                Material::default(),
            )
            .unwrap();
        let expected = world.mass(2).unwrap() * 8.;
        let mut total = 0.;
        for _ in 0..90 {
            world.step(DT).unwrap();
            total += world.contact_impulse(2).unwrap().impulse;
        }
        println!("iterations {iterations}: landing impulse {total:.4}, m v {expected:.4}");
        assert!(
            (total - expected).abs() / expected < 0.1,
            "{iterations} iterations: {total} vs {expected}"
        );
    }
}

/// S3 (joints): a hanging load reads `m g` as constraint force at any iteration count.
#[test]
fn joint_force_readout_is_independent_of_solver_iterations() {
    for iterations in [1, 4, 16, 64] {
        let mut world = World3D::new([0., -G, 0.], 8).unwrap();
        world.set_solver_iterations(iterations).unwrap();
        ball(&mut world, 1, BodyKind::Fixed, [0., 0., 0.]);
        ball(&mut world, 2, BodyKind::Dynamic, [0., -1., 0.]);
        world
            .insert_joint(
                10,
                1,
                2,
                dim3::JointDesc::Fixed {
                    anchor1: [0., -1., 0.],
                    anchor2: [0.; 3],
                },
            )
            .unwrap();
        for _ in 0..120 {
            world.step(DT).unwrap();
        }
        let force = world.joint_state(10).unwrap().force;
        let magnitude = (force[0].powi(2) + force[1].powi(2) + force[2].powi(2)).sqrt();
        let expected = world.mass(2).unwrap() * G;
        println!("iterations {iterations}: joint force {magnitude:.4} N, m g {expected:.4} N");
        assert!(
            (magnitude - expected).abs() / expected < 0.1,
            "{iterations} iterations: {magnitude} vs {expected}"
        );
    }
}

/// M2: the bounded break history evicts the oldest record, not the lowest joint ID.
#[test]
fn break_history_evicts_the_oldest_records_first() {
    const PER_WAVE: u64 = 600;
    let mut world = World3D::new([0., -G, 0.], 1300).unwrap();
    ball(&mut world, 1, BodyKind::Fixed, [0., 0., 0.]);
    let weak = BreakLimits {
        force: Some(1.),
        torque: None,
    };
    // Wave A holds the HIGH ids and breaks first; wave B holds the LOW ids and breaks later.
    // Every body hangs from exactly one joint, so each joint carries the whole weight.
    let wave_a = |n: u64| 9_000_000 + n;
    let wave_b = |n: u64| 1_000_000 + n;
    for n in 0..PER_WAVE {
        for (wave, id, limits) in [(0, wave_a(n), weak), (1, wave_b(n), BreakLimits::default())] {
            let body = 10 + n + wave * PER_WAVE;
            let x = (n + wave * PER_WAVE) as f32 * 3.;
            ball(&mut world, body, BodyKind::Dynamic, [x, -1., 0.]);
            world
                .insert_joint_with(
                    id,
                    1,
                    body,
                    dim3::JointDesc::Fixed {
                        anchor1: [x, -1., 0.],
                        anchor2: [0.; 3],
                    },
                    JointOptions {
                        break_limits: limits,
                        ..JointOptions::default()
                    },
                )
                .unwrap();
        }
    }
    for _ in 0..10 {
        world.step(DT).unwrap();
    }
    assert_eq!(
        world.broken_joints().len(),
        PER_WAVE as usize,
        "wave A breaks"
    );
    for n in 0..PER_WAVE {
        world.set_joint_break_limits(wave_b(n), weak).unwrap();
    }
    for _ in 0..10 {
        world.step(DT).unwrap();
    }
    let broken = world.broken_joints();
    println!(
        "{} broken records kept; newest wave first id kept: {}; oldest wave first id kept: {}",
        broken.len(),
        world.broken_joint(wave_b(0)).is_some(),
        world.broken_joint(wave_a(0)).is_some()
    );
    assert_eq!(broken.len(), 1024);
    for n in 0..PER_WAVE {
        assert!(
            world.broken_joint(wave_b(n)).is_some(),
            "the newest break {} was evicted",
            wave_b(n)
        );
    }
    // 600 + 600 - 1024 = 176 oldest records were dropped, in break order (ascending id
    // within the first wave).
    assert!(world.broken_joint(wave_a(0)).is_none());
    assert!(world.broken_joint(wave_a(175)).is_none());
    assert!(world.broken_joint(wave_a(176)).is_some());
    assert!(world.broken_joint(wave_a(PER_WAVE - 1)).is_some());
}

/// H3 support: bodies sharing a contact exclusion group never touch, whatever their layers,
/// while bodies outside the group still collide with them.
#[test]
fn contact_exclusion_groups_remove_pairs_and_their_events() {
    let mut world = World3D::new([0.; 3], 8).unwrap();
    let add = |world: &mut World3D, id: u64, x: f32| {
        world
            .insert(
                id,
                dim3::BodyDesc {
                    position: [x, 0., 0.],
                    ..Default::default()
                },
                dim3::Shape::Ball { radius: 0.5 },
                Material::default(),
            )
            .unwrap();
    };
    add(&mut world, 2, 0.);
    add(&mut world, 3, 0.1);
    add(&mut world, 4, 5.);
    add(&mut world, 5, 5.1);
    world.set_contact_exclusion(2, 7).unwrap();
    world.set_contact_exclusion(3, 7).unwrap();
    assert_eq!(world.contact_exclusion(2), Some(7));
    assert_eq!(world.contact_exclusion(4), Some(0));
    let mut events = 0;
    for _ in 0..60 {
        world.step(DT).unwrap();
        events += world.drain_events().filter(|e| e.first < 4).count();
    }
    let excluded = distance(&world, 2, 3);
    let colliding = distance(&world, 4, 5);
    println!(
        "excluded pair {excluded:.3} m apart, colliding pair {colliding:.3} m, events {events}"
    );
    assert!((excluded - 0.1).abs() < 1e-4, "{excluded}");
    assert_eq!(events, 0);
    assert!(colliding > 0.9, "{colliding}");
    // Joining a group later is allowed; group 0 leaves it.
    world.set_contact_exclusion(2, 0).unwrap();
    for _ in 0..60 {
        world.step(DT).unwrap();
    }
    assert!(distance(&world, 2, 3) > 0.9);
    assert!(world.set_contact_exclusion(99, 1).is_err());
}
