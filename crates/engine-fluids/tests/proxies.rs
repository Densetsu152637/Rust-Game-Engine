//! Two-way coupling with host-supplied rigid-body proxies.

mod common;

use common::{DT, block, params, run, tank};
use engine_fluids::{
    ColliderProxy, Domain, FluidParams, FluidWorld, ProxyShape, Region, VolumeDesc,
};
use glam::{Quat, Vec3};

fn sphere(id: u64, position: Vec3, radius: f32) -> ColliderProxy {
    ColliderProxy {
        id,
        position,
        rotation: Quat::IDENTITY,
        linear_velocity: Vec3::ZERO,
        angular_velocity: Vec3::ZERO,
        shape: ProxyShape::Sphere { radius },
        friction: 0.2,
    }
}

#[test]
fn fixed_sphere_proxy_is_not_penetrated() {
    let p = params();
    let mut volume = tank(p, Vec3::ZERO, Vec3::new(0.6, 1.0, 0.6), 2048);
    volume
        .fill(
            block(Vec3::new(0.0, 0.35, 0.0), Vec3::new(0.6, 0.75, 0.6)),
            Vec3::ZERO,
            None,
        )
        .unwrap();
    let center = Vec3::new(0.3, 0.2, 0.3);
    let radius = 0.1;
    let proxies = [sphere(9, center, radius)];
    let mut worst = f32::MAX;
    let mut touched = false;
    for _ in 0..90 {
        let report = volume.step(DT, &proxies).unwrap();
        touched |= report.proxy_impulses[0].linear.y < 0.0;
        for x in volume.positions() {
            worst = worst.min(x.distance(center) - radius - p.particle_radius);
        }
    }
    println!("worst clearance {worst}");
    assert!(touched, "the fluid never pressed on the sphere");
    assert!(
        worst > -0.05 * p.particle_radius,
        "sphere penetrated by {}",
        -worst
    );
}

#[test]
fn moving_box_pushes_fluid_and_impulse_matches_momentum_change() {
    let p = FluidParams {
        gravity: Vec3::ZERO,
        ..params()
    };
    let mut volume = tank(p, Vec3::splat(-1.0), Vec3::new(2.0, 1.0, 1.0), 1024);
    volume
        .fill(
            block(Vec3::new(0.0, -0.15, -0.15), Vec3::new(0.3, 0.15, 0.15)),
            Vec3::ZERO,
            None,
        )
        .unwrap();
    let mass = volume.particle_mass();
    let fluid_mass = mass * volume.len() as f32;
    let momentum = |v: &engine_fluids::FluidVolume| {
        v.velocities().iter().fold(Vec3::ZERO, |sum, &u| sum + u) * mass
    };
    let speed = 1.0;
    let mut position = Vec3::new(-0.2, 0.0, 0.0);
    let mut impulse = Vec3::ZERO;
    for _ in 0..24 {
        let proxy = ColliderProxy {
            id: 3,
            position,
            rotation: Quat::IDENTITY,
            linear_velocity: Vec3::new(speed, 0.0, 0.0),
            angular_velocity: Vec3::ZERO,
            shape: ProxyShape::Box {
                half_extents: Vec3::new(0.1, 0.2, 0.2),
            },
            friction: 0.3,
        };
        let report = volume.step(DT, &[proxy]).unwrap();
        impulse += report.proxy_impulses[0].linear;
        position.x += speed * DT;
    }
    let gained = momentum(&volume);
    println!("fluid momentum {gained} proxy impulse {impulse} fluid mass {fluid_mass}");
    assert!(
        gained.x > 0.2 * fluid_mass * speed,
        "box did not push the fluid: {gained}"
    );
    // Momentum the fluid gained equals the impulse the box received, with opposite sign.
    let error = (gained + impulse).length();
    assert!(
        error < 1.0e-3 * gained.length(),
        "momentum {gained} vs impulse {impulse}"
    );
}

/// Mean vertical force (N) on a sphere of `radius` at `center` in a settled 0.5 m pool.
fn mean_vertical_force(center: Vec3, radius: f32) -> f32 {
    let p = params();
    let min = Vec3::ZERO;
    let mut volume = tank(p, min, Vec3::new(0.6, 1.0, 0.6), 2048);
    let clearance = radius + p.particle_radius;
    let points: Vec<Vec3> = Region::Box {
        center: Vec3::new(0.3, 0.25, 0.3),
        half_extents: Vec3::new(0.3, 0.25, 0.3),
    }
    .lattice_points(p.spacing())
    .into_iter()
    .filter(|x| x.distance(center) >= clearance)
    .collect();
    volume.spawn(&points, Vec3::ZERO, None).unwrap();
    let proxies = [sphere(1, center, radius)];
    run(&mut volume, 1.5, &proxies);
    let reports = run(&mut volume, 1.0, &proxies);
    let total: f32 = reports.iter().map(|r| r.proxy_impulses[0].linear.y).sum();
    total / 1.0
}

#[test]
fn submerged_proxy_receives_buoyancy_and_one_in_air_does_not() {
    let radius = 0.1;
    let submerged = mean_vertical_force(Vec3::new(0.3, 0.25, 0.3), radius);
    let in_air = mean_vertical_force(Vec3::new(0.3, 0.85, 0.3), radius);
    let p = params();
    let archimedes = p.rest_density * 9.81 * 4.0 / 3.0 * std::f32::consts::PI * radius.powi(3);
    println!("submerged {submerged} N, in air {in_air} N, archimedes {archimedes} N");
    assert!(submerged > 0.0, "no upward force: {submerged}");
    assert!(in_air.abs() < 1.0e-3, "force in air: {in_air}");
    assert!(
        submerged > 0.3 * archimedes && submerged < 3.0 * archimedes,
        "buoyancy {submerged} N vs Archimedes {archimedes} N"
    );
}

#[test]
fn world_steps_volumes_by_id_and_reports_impulses_sorted_by_proxy_id() {
    let p = params();
    let mut world = FluidWorld::new();
    for id in [20, 10] {
        let volume = world
            .create_volume(
                id,
                VolumeDesc {
                    params: p,
                    domain: Domain::new(Vec3::ZERO, Vec3::new(0.4, 0.6, 0.4)),
                    capacity: 512,
                },
            )
            .unwrap();
        volume
            .fill(
                block(Vec3::ZERO, Vec3::new(0.4, 0.2, 0.4)),
                Vec3::ZERO,
                None,
            )
            .unwrap();
    }
    let floor = ColliderProxy {
        id: 7,
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        linear_velocity: Vec3::ZERO,
        angular_velocity: Vec3::ZERO,
        shape: ProxyShape::Plane {
            normal: Vec3::Y,
            offset: 0.1,
        },
        friction: 0.0,
    };
    let unused = sphere(2, Vec3::new(5.0, 5.0, 5.0), 0.1);
    let mut total = Vec3::ZERO;
    for _ in 0..30 {
        let report = world.step(DT, &[floor, unused]).unwrap();
        let ids: Vec<u64> = report.volumes.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [10, 20]);
        let proxy_ids: Vec<u64> = report.proxy_impulses.iter().map(|i| i.id).collect();
        assert_eq!(proxy_ids, [2, 7]);
        let summed = report.volumes[0].1.proxy_impulses[1].linear
            + report.volumes[1].1.proxy_impulses[1].linear;
        assert_eq!(summed, report.proxy_impulses[1].linear);
        assert_eq!(report.proxy_impulses[0].linear, Vec3::ZERO);
        total += report.proxy_impulses[1].linear;
    }
    // The plane lifts both pools: every particle ends above it.
    for id in world.ids().collect::<Vec<_>>() {
        let volume = world.volume(id).unwrap();
        assert!(
            volume
                .positions()
                .iter()
                .all(|x| x.y >= 0.1 + p.particle_radius - 1.0e-4)
        );
    }
    assert!(total.y < 0.0, "fluid did not press on the plane: {total}");
    assert!(world.volume_mut(99).is_err());
}
