use std::time::Instant;

use engine_xpbd::{ClothDesc, ColliderProxy, ProxyShape, XpbdSettings, XpbdWorld};
use glam::{Quat, Vec3};

/// Release timing for a 32x32-segment cloth (1089 particles) draped on a sphere with wind.
/// Run with `cargo test -p engine-xpbd --release --test perf -- --ignored --nocapture`.
#[test]
#[ignore = "performance measurement; run in release"]
fn cloth_32x32_step_time() {
    let settings = XpbdSettings {
        wind: Vec3::new(2.0, 0.0, 0.0),
        ..XpbdSettings::default()
    };
    let mut world = XpbdWorld::new(settings, 10_000).unwrap();
    let mut desc = ClothDesc::grid(
        Vec3::new(-0.6, 0.5, -0.6),
        Vec3::X * 1.2,
        Vec3::Z * 1.2,
        32,
        32,
    )
    .unwrap();
    desc.params.tether_slack = Some(1.1);
    desc.pinned = vec![0, 32];
    world.insert_cloth(1, desc).unwrap();
    let sphere = ColliderProxy {
        id: 1,
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        linear_velocity: Vec3::ZERO,
        angular_velocity: Vec3::ZERO,
        shape: ProxyShape::Sphere { radius: 0.3 },
        friction: 0.5,
    };
    let frames = 600;
    let start = Instant::now();
    for _ in 0..frames {
        world.step(1.0 / 60.0, &[sphere]).unwrap();
    }
    let per_step = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    let (stretch, bend, tethers) = world.cloth(1).unwrap().constraint_counts();
    println!(
        "32x32 cloth: {per_step:.3} ms per 1/60 s step ({} substeps; {stretch} stretch, {bend} bend, {tethers} tether constraints)",
        settings.substeps
    );
}
