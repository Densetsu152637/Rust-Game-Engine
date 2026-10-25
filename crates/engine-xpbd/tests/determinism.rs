mod common;

use common::{DT, proxy};
use engine_xpbd::{ClothDesc, ColliderProxy, ProxyShape, SoftBodyDesc, XpbdSettings, XpbdWorld};
use glam::{Quat, Vec3};

fn scene() -> (XpbdWorld, Vec<ColliderProxy>) {
    let settings = XpbdSettings {
        wind: Vec3::new(1.5, 0.0, 2.0),
        iterations: 2,
        ..XpbdSettings::default()
    };
    let mut world = XpbdWorld::new(settings, 10_000).unwrap();
    let mut cloth = ClothDesc::grid(Vec3::new(-0.5, 1.0, -0.5), Vec3::X, Vec3::Z, 12, 12).unwrap();
    cloth.pinned = vec![0, 12];
    cloth.params.tether_slack = Some(1.05);
    world.insert_cloth(20, cloth).unwrap();
    let body = SoftBodyDesc::cuboid(
        Vec3::new(0.6, 0.4, -0.2),
        Vec3::new(1.0, 0.8, 0.2),
        [2, 2, 2],
        300.0,
    )
    .unwrap();
    world.insert_soft_body(10, body).unwrap();
    let proxies = vec![
        proxy(
            3,
            Vec3::ZERO,
            Quat::IDENTITY,
            ProxyShape::Plane {
                normal: Vec3::Y,
                offset: 0.0,
            },
            0.6,
        ),
        proxy(
            1,
            Vec3::new(0.0, 0.5, 0.0),
            Quat::IDENTITY,
            ProxyShape::Sphere { radius: 0.25 },
            0.4,
        ),
        proxy(
            2,
            Vec3::new(0.8, 0.2, 0.0),
            Quat::from_rotation_x(0.3),
            ProxyShape::Capsule {
                half_height: 0.3,
                radius: 0.1,
            },
            0.2,
        ),
    ];
    (world, proxies)
}

fn simulate() -> (Vec<u32>, Vec<u32>) {
    let (mut world, mut proxies) = scene();
    for frame in 0..90 {
        let t = frame as f32 * DT;
        world
            .cloth_mut(20)
            .unwrap()
            .set_pin_target(0, Vec3::new(-0.5 + 0.2 * t.sin(), 1.0, -0.5))
            .unwrap();
        proxies[2].angular_velocity = Vec3::new(0.0, 1.0, 0.0);
        world.step(DT, &proxies).unwrap();
    }
    let mut bits = Vec::new();
    for id in world.ids().collect::<Vec<_>>() {
        let positions = match world.cloth(id) {
            Some(cloth) => cloth.positions().to_vec(),
            None => world.soft_body(id).unwrap().positions().to_vec(),
        };
        bits.extend(
            positions
                .iter()
                .flat_map(|p| p.to_array().map(f32::to_bits)),
        );
    }
    let impulses = world
        .proxy_impulses()
        .iter()
        .flat_map(|i| [i.impulse, i.point, i.angular_impulse])
        .flat_map(|v| v.to_array().map(f32::to_bits))
        .collect();
    let ids: Vec<u64> = world.proxy_impulses().iter().map(|i| i.id).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "impulses sorted by id: {ids:?}"
    );
    (bits, impulses)
}

#[test]
fn identical_runs_are_bitwise_identical() {
    let first = simulate();
    let second = simulate();
    assert!(!first.1.is_empty(), "scene should produce contacts");
    assert_eq!(first, second);
}
