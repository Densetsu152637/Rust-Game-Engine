//! Stress runs at the edge of the accepted ranges: a validated scene must never reach a
//! non-finite state (the step would then fail forever, see `docs/physics.md`).
mod common;

use common::proxy;
use engine_xpbd::{
    ClothDesc, ColliderProxy, MAX_SPEED, MAX_STEP, ProxyShape, SoftBodyDesc, XpbdSettings,
    XpbdWorld,
};
use glam::{Quat, Vec3};

fn extreme_settings(substeps: u32, iterations: u32) -> XpbdSettings {
    XpbdSettings {
        gravity: Vec3::new(0.0, -MAX_SPEED, 0.0),
        wind: Vec3::splat(MAX_SPEED),
        air_density: 100.0,
        substeps,
        iterations,
    }
}
fn cloth_world(settings: XpbdSettings) -> XpbdWorld {
    let mut world = XpbdWorld::new(settings, 4_000).unwrap();
    let mut cloth = ClothDesc::grid(
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::X * 2.0,
        Vec3::Z * 2.0,
        12,
        12,
    )
    .unwrap();
    cloth.params.stretch_compliance = 0.0;
    cloth.params.bend_compliance = 0.0;
    cloth.params.drag = 10.0;
    cloth.params.lift = 10.0;
    world.insert_cloth(1, cloth).unwrap();
    world
}
fn soft_world(settings: XpbdSettings) -> XpbdWorld {
    let mut world = XpbdWorld::new(settings, 4_000).unwrap();
    let mut soft = SoftBodyDesc::cuboid(
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(3.0, 1.0, 1.0),
        [3, 3, 3],
        1.0,
    )
    .unwrap();
    soft.params.volume_compliance = 0.0;
    world.insert_soft_body(2, soft).unwrap();
    world
}
fn hammer(frame: usize) -> ColliderProxy {
    let mut hammer = proxy(
        1,
        Vec3::new(frame as f32 * 0.3 - 20.0, 0.5, 0.0),
        Quat::IDENTITY,
        ProxyShape::Box {
            half_extents: Vec3::splat(2.0),
        },
        10.0,
    );
    hammer.linear_velocity = Vec3::new(MAX_SPEED, -MAX_SPEED, MAX_SPEED);
    hammer.angular_velocity = Vec3::splat(100.0);
    hammer
}
fn run(world: &mut XpbdWorld, with_hammer: bool, label: &str) {
    for frame in 0..120 {
        let proxies: Vec<ColliderProxy> = if with_hammer {
            vec![hammer(frame)]
        } else {
            vec![]
        };
        world
            .step(MAX_STEP, &proxies)
            .unwrap_or_else(|e| panic!("{label} frame {frame}: {e}"));
    }
}

#[test]
fn extreme_gravity_and_wind_keep_cloth_finite() {
    for (s, i) in [(1, 1), (2, 2), (8, 4)] {
        run(
            &mut cloth_world(extreme_settings(s, i)),
            false,
            &format!("cloth {s}x{i}"),
        );
    }
}
#[test]
fn extreme_gravity_keeps_soft_bodies_finite() {
    for (s, i) in [(1, 1), (2, 2), (8, 4)] {
        run(
            &mut soft_world(extreme_settings(s, i)),
            false,
            &format!("soft {s}x{i}"),
        );
    }
}
#[test]
fn a_hammer_at_the_speed_limit_keeps_cloth_finite() {
    for (s, i) in [(1, 1), (2, 2), (8, 4)] {
        run(
            &mut cloth_world(XpbdSettings {
                substeps: s,
                iterations: i,
                ..Default::default()
            }),
            true,
            &format!("cloth hammer {s}x{i}"),
        );
    }
}
#[test]
fn a_hammer_at_the_speed_limit_keeps_soft_bodies_finite() {
    for (s, i) in [(1, 1), (2, 2), (8, 4)] {
        run(
            &mut soft_world(XpbdSettings {
                substeps: s,
                iterations: i,
                ..Default::default()
            }),
            true,
            &format!("soft hammer {s}x{i}"),
        );
    }
}
