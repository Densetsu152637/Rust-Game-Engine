use engine_physics::{
    BodyKind, CollisionLayers, CombineRule, Material, World2D, World3D, dim2, dim3,
};

const DT: f32 = 1. / 60.;
fn ground(world: &mut World3D, material: Material) {
    world
        .insert(
            1,
            dim3::BodyDesc {
                kind: BodyKind::Fixed,
                position: [0., -0.5, 0.],
                ..Default::default()
            },
            dim3::Shape::Box {
                half_extents: [100., 0.5, 10.],
            },
            material,
        )
        .unwrap();
}
/// Distance a 1 m box launched at 5 m/s slides over a frictionless ground in 1.5 s.
fn sliding_distance(friction_combine: CombineRule) -> f32 {
    let mut world = World3D::new([0., -9.81, 0.], 4).unwrap();
    ground(
        &mut world,
        Material {
            friction: 0.,
            ..Default::default()
        },
    );
    world
        .insert(
            2,
            dim3::BodyDesc {
                position: [0., 0.5, 0.],
                velocity: [5., 0., 0.],
                ..Default::default()
            },
            dim3::Shape::Box {
                half_extents: [0.5; 3],
            },
            Material {
                friction: 1.,
                friction_combine,
                ..Default::default()
            },
        )
        .unwrap();
    for _ in 0..90 {
        world.step(DT).unwrap();
    }
    world.pose(2).unwrap().position[0]
}
/// Highest centre height after the first bounce of a ball dropped from 3 m.
fn bounce_height(restitution_combine: CombineRule) -> f32 {
    let mut world = World3D::new([0., -9.81, 0.], 4).unwrap();
    ground(&mut world, Material::default());
    world
        .insert(
            2,
            dim3::BodyDesc {
                position: [0., 3., 0.],
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.5 },
            Material {
                restitution: 1.,
                restitution_combine,
                ..Default::default()
            },
        )
        .unwrap();
    let mut bounced = false;
    let mut peak: f32 = 0.;
    for _ in 0..150 {
        world.step(DT).unwrap();
        let pose = world.pose(2).unwrap();
        bounced |= pose.position[1] < 0.6;
        if bounced {
            peak = peak.max(pose.position[1]);
        }
    }
    peak
}

#[test]
fn friction_combine_rules_change_measured_sliding_distance() {
    // Ground 0.0 and box 1.0: min/multiply give 0, average 0.5, max 1.0.
    let min = sliding_distance(CombineRule::Min);
    let multiply = sliding_distance(CombineRule::Multiply);
    let average = sliding_distance(CombineRule::Average);
    let max = sliding_distance(CombineRule::Max);
    // Frictionless: 5 m/s for 1.5 s.
    assert!((min - 7.5).abs() < 0.2, "{min}");
    assert!((multiply - 7.5).abs() < 0.2, "{multiply}");
    // Ideal v^2 / (2 mu g) is 2.55 m at mu 0.5 and 1.27 m at mu 1.0; Rapier's box contact
    // manifold decelerates slightly faster (measured ~1.04 m at mu 1.0).
    assert!((average - 2.55).abs() < 0.4, "{average}");
    assert!((0.9..1.4).contains(&max), "{max}");
    eprintln!("sliding distance min {min} multiply {multiply} average {average} max {max}");
}

#[test]
fn restitution_combine_rules_change_measured_bounce_height() {
    // Ground 0.0 and ball 1.0: min gives no bounce, max an almost elastic one.
    let min = bounce_height(CombineRule::Min);
    let max = bounce_height(CombineRule::Max);
    let average = bounce_height(CombineRule::Average);
    assert!(min < 0.6, "{min}");
    assert!(max > 2.6, "{max}");
    // Restitution 0.5 returns a quarter of the 2.5 m drop height above the 0.5 m rest.
    assert!((average - (0.5 + 2.5 * 0.25)).abs() < 0.2, "{average}");
    eprintln!("bounce peak min {min} average {average} max {max}");
}

#[test]
fn layer_matrix_disabled_pairs_never_touch_and_enabled_pairs_do() {
    let mut layers = CollisionLayers::new(["player", "enemy", "debris"]).unwrap();
    layers.set_collides("debris", "player", false).unwrap();
    layers.set_collides("enemy", "enemy", false).unwrap();
    assert!(!layers.collides("player", "debris").unwrap());
    assert!(layers.collides("player", "enemy").unwrap());
    assert!(layers.collides("debris", "debris").unwrap());
    let on = |name| layers.apply(Material::default(), name).unwrap();
    let mut world = World3D::new([0.; 3], 16).unwrap();
    // Overlapping pairs: (1 player, 2 debris), (3 player, 4 enemy), (5 enemy, 6 enemy).
    for (id, x, layer) in [
        (1, 0., "player"),
        (2, 0.5, "debris"),
        (3, 10., "player"),
        (4, 10.5, "enemy"),
        (5, 20., "enemy"),
        (6, 20.5, "enemy"),
    ] {
        world
            .insert(
                id,
                dim3::BodyDesc {
                    position: [x, 0., 0.],
                    ..Default::default()
                },
                dim3::Shape::Ball { radius: 0.5 },
                on(layer),
            )
            .unwrap();
    }
    let mut pairs = std::collections::BTreeSet::new();
    for _ in 0..30 {
        world.step(DT).unwrap();
        pairs.extend(world.drain_events().map(|e| (e.first, e.second)));
    }
    assert_eq!(pairs, [(3, 4)].into());
    // Disabled pairs keep overlapping instead of being pushed apart.
    assert!((world.pose(2).unwrap().position[0] - 0.5).abs() < 1e-4);
    assert!((world.pose(6).unwrap().position[0] - 20.5).abs() < 1e-4);
    assert!(world.pose(4).unwrap().position[0] > 10.6);
    let mask = layers.query_mask(&["enemy"]).unwrap();
    let hit = world
        .raycast([10.5, 5., 0.], [0., -1., 0.], 10., mask)
        .unwrap()
        .unwrap();
    assert_eq!(hit.id, 4);
}

#[test]
fn layer_names_are_bounded_unique_and_resolved_in_2d() {
    assert!(CollisionLayers::new((0..33).map(|i| format!("l{i}"))).is_err());
    let all = CollisionLayers::new((0..32).map(|i| format!("l{i}"))).unwrap();
    assert_eq!(all.groups("l31").unwrap(), (1 << 31, u32::MAX));
    assert!(CollisionLayers::new(["a", "a"]).is_err());
    assert!(CollisionLayers::new(["bad name"]).is_err());
    assert!(CollisionLayers::new([""]).is_err());
    let mut layers = CollisionLayers::new(["a", "b"]).unwrap();
    assert!(layers.set_collides("a", "missing", false).is_err());
    assert!(layers.groups("missing").is_err());
    layers.set_collides("a", "b", false).unwrap();
    assert_eq!(layers.groups("a").unwrap(), (0b01, 0b01));
    assert_eq!(layers.groups("b").unwrap(), (0b10, 0b10));
    let mut world = World2D::new([0.; 2], 4).unwrap();
    for (id, layer) in [(1, "a"), (2, "b")] {
        world
            .insert(
                id,
                dim2::BodyDesc::default(),
                dim2::Shape::Ball { radius: 0.5 },
                layers.apply(Material::default(), layer).unwrap(),
            )
            .unwrap();
    }
    world.step(DT).unwrap();
    assert_eq!(world.drain_events().count(), 0);
    layers.set_collides("a", "b", true).unwrap();
    world.remove(2);
    world
        .insert(
            2,
            dim2::BodyDesc::default(),
            dim2::Shape::Ball { radius: 0.5 },
            layers.apply(Material::default(), "b").unwrap(),
        )
        .unwrap();
    world.remove(1);
    world
        .insert(
            1,
            dim2::BodyDesc::default(),
            dim2::Shape::Ball { radius: 0.5 },
            layers.apply(Material::default(), "a").unwrap(),
        )
        .unwrap();
    world.step(DT).unwrap();
    assert!(world.drain_events().any(|e| e.started));
}

#[test]
fn world_parameters_and_velocity_reads_are_validated() {
    let mut world = World3D::new([0., -9.81, 0.], 4).unwrap();
    world
        .insert(
            1,
            dim3::BodyDesc {
                angular_velocity: [0., 2., 0.],
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.5 },
            Material::default(),
        )
        .unwrap();
    assert_eq!(world.solver_iterations(), 4);
    world.set_solver_iterations(8).unwrap();
    assert_eq!(world.solver_iterations(), 8);
    assert!(world.set_solver_iterations(0).is_err());
    assert!(world.set_solver_iterations(65).is_err());
    world.set_gravity([0., 0., -2.]).unwrap();
    assert_eq!(world.gravity(), [0., 0., -2.]);
    assert!(world.set_gravity([f32::NAN, 0., 0.]).is_err());
    for _ in 0..60 {
        world.step(DT).unwrap();
    }
    let velocity = world.velocity(1).unwrap();
    assert!((velocity.linear[2] + 2.).abs() < 0.01, "{velocity:?}");
    assert!(velocity.linear[1].abs() < 1e-4);
    assert!((velocity.angular[1] - 2.).abs() < 0.05);
    assert!(world.velocity(9).is_none());
    assert!(world.contains(1) && !world.contains(9));
}
