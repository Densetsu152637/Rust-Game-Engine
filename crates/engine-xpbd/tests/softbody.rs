mod common;

use common::{max_speed, proxy, run};
use engine_xpbd::{ProxyShape, SoftBodyDesc, XpbdSettings, XpbdWorld};
use glam::{Quat, Vec3};

const RADIUS: f32 = 0.01;
const SIDE: f32 = 0.5;

struct Settled {
    min_y: f32,
    height: f32,
    volume_error: f32,
    speed: f32,
}

/// Drops a 0.5 m soft cube (density 200 kg/m^3) from 0.3 m onto a ground plane.
fn drop_cube(edge_compliance: f32, volume_compliance: f32) -> (XpbdWorld, Settled) {
    let mut desc = SoftBodyDesc::cuboid(
        Vec3::new(-0.25, 0.3, -0.25),
        Vec3::new(0.25, 0.3 + SIDE, 0.25),
        [4, 4, 4],
        200.0,
    )
    .unwrap();
    desc.params.edge_compliance = edge_compliance;
    desc.params.volume_compliance = volume_compliance;
    desc.params.damping = 1.0;
    desc.params.particle_radius = RADIUS;
    let mut world = XpbdWorld::new(XpbdSettings::default(), 1_000).unwrap();
    world.insert_soft_body(5, desc).unwrap();
    let ground = proxy(
        1,
        Vec3::ZERO,
        Quat::IDENTITY,
        ProxyShape::Plane {
            normal: Vec3::Y,
            offset: 0.0,
        },
        0.5,
    );
    run(&mut world, 3.0, &[ground]);
    let body = world.soft_body(5).unwrap();
    let ys = body.positions().iter().map(|p| p.y);
    let min_y = ys.clone().fold(f32::INFINITY, f32::min);
    let max_y = ys.fold(f32::NEG_INFINITY, f32::max);
    let settled = Settled {
        min_y,
        height: max_y - min_y,
        volume_error: body.volume() / body.rest_volume() - 1.0,
        speed: max_speed(body.velocities()),
    };
    (world, settled)
}

#[test]
fn stiff_cube_rests_on_plane_and_preserves_volume() {
    let (world, stiff) = drop_cube(0.0, 0.0);
    println!(
        "stiff cube: min y {:.5} (radius {RADIUS}), height {:.5}, volume error {:+.5}, max speed {:.5}",
        stiff.min_y, stiff.height, stiff.volume_error, stiff.speed
    );
    assert!(
        (stiff.min_y - RADIUS).abs() < 2.0e-3,
        "min y {}",
        stiff.min_y
    );
    assert!(
        stiff.volume_error.abs() < 0.05,
        "volume error {}",
        stiff.volume_error
    );
    assert!(stiff.speed < 0.05, "not at rest: {}", stiff.speed);
    let body = world.soft_body(5).unwrap();
    assert_eq!(body.surface_triangles().len(), 6 * 16 * 2);
    assert_eq!(body.tets().len(), 64 * 6);
}

#[test]
fn higher_compliance_deforms_more() {
    let (_, stiff) = drop_cube(0.0, 0.0);
    let (_, soft) = drop_cube(3.0e-3, 3.0e-4);
    let stiff_squash = SIDE - stiff.height;
    let soft_squash = SIDE - soft.height;
    println!(
        "squash: stiff {stiff_squash:.5} m (volume error {:+.5}), soft {soft_squash:.5} m (volume error {:+.5}), soft min y {:.5}",
        stiff.volume_error, soft.volume_error, soft.min_y
    );
    assert!(
        soft_squash > stiff_squash + 0.01,
        "stiff {stiff_squash} soft {soft_squash}"
    );
    assert!(soft.volume_error.abs() > stiff.volume_error.abs());
    assert!(
        soft.min_y > RADIUS - 2.0e-3,
        "soft cube sank: {}",
        soft.min_y
    );
}
