mod common;

use common::{DT, centroid, momentum, proxy, total_mass};
use engine_xpbd::{ClothDesc, ProxyShape, SoftBodyDesc, XpbdSettings, XpbdWorld};
use glam::{Quat, Vec3};

fn no_air() -> XpbdSettings {
    XpbdSettings {
        air_density: 0.0,
        ..XpbdSettings::default()
    }
}

#[test]
fn proxy_impulse_matches_particle_momentum_change() {
    let mut desc = SoftBodyDesc::cuboid(
        Vec3::new(-0.15, 0.05, -0.15),
        Vec3::new(0.15, 0.35, 0.15),
        [3, 3, 3],
        100.0,
    )
    .unwrap();
    desc.params.damping = 0.0;
    let settings = no_air();
    let mut world = XpbdWorld::new(settings, 1_000).unwrap();
    world.insert_soft_body(1, desc).unwrap();
    let slab = proxy(
        7,
        Vec3::new(0.0, -0.1, 0.0),
        Quat::IDENTITY,
        ProxyShape::Box {
            half_extents: Vec3::new(1.0, 0.1, 1.0),
        },
        0.5,
    );
    let inverse_masses = world.soft_body(1).unwrap().inverse_masses().to_vec();
    let mass = total_mass(&inverse_masses);
    let weight_impulse = settings.gravity * mass * DT;
    let mut peak = 0.0f32;
    let mut worst = 0.0f32;
    for _ in 0..120 {
        let before = momentum(world.soft_body(1).unwrap().velocities(), &inverse_masses);
        world.step(DT, &[slab]).unwrap();
        let after = momentum(world.soft_body(1).unwrap().velocities(), &inverse_masses);
        let reported = world
            .proxy_impulses()
            .iter()
            .find(|i| i.id == 7)
            .map_or(Vec3::ZERO, |i| i.impulse);
        // Particles gain gravity plus the contact impulse; the proxy gets the reaction.
        let contact_on_particles = after - before - weight_impulse;
        let error = (contact_on_particles + reported).length();
        let scale = reported.length().max(weight_impulse.length());
        worst = worst.max(error / scale);
        peak = peak.max(reported.length());
    }
    let resting = world.proxy_impulses()[0];
    println!(
        "coupling: mass {mass:.4} kg, worst relative momentum mismatch {worst:e}, peak impulse {peak:.4} N s (weight impulse {:.4}), resting impulse {} at {}",
        weight_impulse.length(),
        resting.impulse,
        resting.point
    );
    assert!(worst < 1.0e-3, "momentum mismatch {worst}");
    assert!(
        peak > 2.0 * weight_impulse.length(),
        "impact not observed: {peak}"
    );
    // At rest the proxy carries the body's weight, applied under its centre.
    assert!(
        resting
            .impulse
            .abs_diff_eq(weight_impulse, 0.02 * weight_impulse.length()),
        "resting impulse {}",
        resting.impulse
    );
    assert!(
        resting.point.abs_diff_eq(Vec3::ZERO, 0.01),
        "point {}",
        resting.point
    );
    assert!(resting.angular_impulse.length() < 0.01 * weight_impulse.length());
}

#[test]
fn friction_drags_cloth_with_a_moving_proxy_surface() {
    let mut desc = ClothDesc::grid(
        Vec3::new(-0.15, 0.011, -0.15),
        Vec3::X * 0.3,
        Vec3::Z * 0.3,
        6,
        6,
    )
    .unwrap();
    desc.params.friction = 0.8;
    desc.params.damping = 0.0;
    let mut world = XpbdWorld::new(no_air(), 1_000).unwrap();
    world.insert_cloth(1, desc).unwrap();
    let speed = 0.5;
    let mut platform = proxy(
        4,
        Vec3::new(0.0, -0.1, 0.0),
        Quat::IDENTITY,
        ProxyShape::Box {
            half_extents: Vec3::new(2.0, 0.1, 2.0),
        },
        0.8,
    );
    platform.linear_velocity = Vec3::new(speed, 0.0, 0.0);
    let start = centroid(world.cloth(1).unwrap().positions());
    let mut reaction_x = 0.0;
    for _ in 0..60 {
        world.step(DT, &[platform]).unwrap();
        reaction_x += world.proxy_impulses()[0].impulse.x;
        platform.position.x += speed * DT;
    }
    let cloth = world.cloth(1).unwrap();
    let travelled = centroid(cloth.positions()).x - start.x;
    let velocity = common::centroid(cloth.velocities()).x;
    println!(
        "moving platform: cloth travelled {travelled:.4} m in 1 s, velocity {velocity:.4} m/s, reaction impulse x {reaction_x:.5}"
    );
    assert!(
        (velocity - speed).abs() < 0.02 * speed,
        "velocity {velocity}"
    );
    assert!(travelled > 0.45, "travelled {travelled}");
    // Accelerating the cloth pushes the platform backwards by the cloth's momentum.
    // (Tolerance covers f32 rounding of the tiny per-substep friction displacements.)
    let cloth_momentum = common::momentum(cloth.velocities(), cloth.inverse_masses()).x;
    let mismatch = (reaction_x + cloth_momentum).abs() / cloth_momentum;
    println!(
        "moving platform: cloth momentum {cloth_momentum:.6}, relative reaction mismatch {mismatch:e}"
    );
    assert!(mismatch < 0.02, "{reaction_x} {cloth_momentum}");
}
