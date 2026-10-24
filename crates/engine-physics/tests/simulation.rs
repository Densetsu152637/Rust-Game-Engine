use engine_physics::{BodyKind, Material, World2D, World3D, dim2, dim3};

#[test]
fn ray_queries_observe_insertions_and_removals_without_advancing_time() {
    let mut world = World2D::new([0.; 2], 4).unwrap();
    world
        .insert(
            4,
            dim2::BodyDesc {
                kind: BodyKind::Fixed,
                position: [3., 0.],
                ..Default::default()
            },
            dim2::Shape::Ball { radius: 1. },
            Material {
                layer: 2,
                mask: 0,
                ..Default::default()
            },
        )
        .unwrap();
    let hit = world.raycast([0.; 2], [2., 0.], 10., 2).unwrap().unwrap();
    assert_eq!(hit.id, 4);
    assert!((hit.distance - 2.).abs() < 1e-5);
    assert!(world.raycast([0.; 2], [2., 0.], 10., 1).unwrap().is_none());
    world.remove(4);
    assert!(world.raycast([0.; 2], [2., 0.], 10., 2).unwrap().is_none());
}

#[test]
fn two_dimensional_body_falls_settles_and_receives_impulse() {
    let mut world = World2D::new([0., -9.81], 8).unwrap();
    world
        .insert(
            1,
            dim2::BodyDesc {
                kind: BodyKind::Fixed,
                position: [0., -0.5],
                ..Default::default()
            },
            dim2::Shape::Box {
                half_extents: [10., 0.5],
            },
            Material::default(),
        )
        .unwrap();
    world
        .insert(
            2,
            dim2::BodyDesc {
                position: [0., 4.],
                ..Default::default()
            },
            dim2::Shape::Ball { radius: 0.5 },
            Material::default(),
        )
        .unwrap();
    for _ in 0..240 {
        world.step(1. / 60.).unwrap();
    }
    let pose = world.pose(2).unwrap();
    assert!((pose.position[1] - 0.5).abs() < 0.05, "{pose:?}");
    world.apply_impulse(2, [2., 4.]).unwrap();
    world.step(1. / 60.).unwrap();
    assert!(world.pose(2).unwrap().velocity[1] > 0.);
    assert!(world.apply_impulse(1, [0., 1.]).is_err());
}
#[test]
fn three_dimensional_body_falls_rotates_and_raycast_uses_world_distance() {
    let mut world = World3D::new([0., -9.81, 0.], 8).unwrap();
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
    world
        .insert(
            2,
            dim3::BodyDesc {
                position: [0., 4., 0.],
                angular_velocity: [0., 2., 0.],
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.5 },
            Material::default(),
        )
        .unwrap();
    for _ in 0..30 {
        world.step(1. / 60.).unwrap();
    }
    assert!(world.pose(2).unwrap().rotation[1].abs() > 0.1);
    for _ in 0..210 {
        world.step(1. / 60.).unwrap();
    }
    assert!((world.pose(2).unwrap().position[1] - 0.5).abs() < 0.05);
    let hit = world
        .raycast([4., 2., 0.], [0., -7., 0.], 4., u32::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(hit.id, 1);
    assert!((hit.distance - 2.).abs() < 0.01);
    assert!(world.raycast([0.; 3], [0.; 3], 3., 1).is_err());
}
#[test]
fn sensors_report_enter_exit_and_removed_bodies_in_both_dimensions() {
    let mut a = World2D::new([0.; 2], 8).unwrap();
    a.insert(
        1,
        dim2::BodyDesc {
            kind: BodyKind::Fixed,
            ..Default::default()
        },
        dim2::Shape::Box {
            half_extents: [1.; 2],
        },
        Material {
            sensor: true,
            ..Default::default()
        },
    )
    .unwrap();
    a.insert(
        2,
        dim2::BodyDesc::default(),
        dim2::Shape::Ball { radius: 0.25 },
        Material::default(),
    )
    .unwrap();
    a.step(1. / 60.).unwrap();
    assert!(
        a.drain_events()
            .any(|e| e.started && e.first == 1 && e.second == 2)
    );
    a.step(1. / 60.).unwrap();
    assert_eq!(a.drain_events().count(), 0);
    assert!(a.remove(2));
    a.step(1. / 60.).unwrap();
    assert!(a.drain_events().any(|e| !e.started));
    assert!(a.pose(2).is_none());
    assert!(!a.remove(2));
    let mut b = World3D::new([0.; 3], 8).unwrap();
    b.insert(
        1,
        dim3::BodyDesc {
            kind: BodyKind::Fixed,
            ..Default::default()
        },
        dim3::Shape::Box {
            half_extents: [1.; 3],
        },
        Material {
            sensor: true,
            ..Default::default()
        },
    )
    .unwrap();
    b.insert(
        2,
        dim3::BodyDesc {
            velocity: [10., 0., 0.],
            ..Default::default()
        },
        dim3::Shape::Ball { radius: 0.25 },
        Material::default(),
    )
    .unwrap();
    b.step(1. / 60.).unwrap();
    assert!(b.drain_events().any(|e| e.started));
    let mut exited = false;
    for _ in 0..30 {
        b.step(1. / 60.).unwrap();
        exited |= b.drain_events().any(|e| !e.started);
    }
    assert!(exited);
}
#[test]
fn masks_kinematic_targets_and_validation() {
    let mut world = World2D::new([0.; 2], 3).unwrap();
    world
        .insert(
            1,
            dim2::BodyDesc {
                kind: BodyKind::Kinematic,
                ..Default::default()
            },
            dim2::Shape::Capsule {
                half_height: 0.5,
                radius: 0.2,
            },
            Material {
                mask: 0,
                ..Default::default()
            },
        )
        .unwrap();
    world
        .insert(
            2,
            dim2::BodyDesc::default(),
            dim2::Shape::Ball { radius: 1. },
            Material::default(),
        )
        .unwrap();
    world.step(1. / 60.).unwrap();
    assert_eq!(world.drain_events().count(), 0);
    world.set_kinematic_target(1, [3., 2.], 0.4).unwrap();
    world.step(1. / 60.).unwrap();
    assert_eq!(world.pose(1).unwrap().position, [3., 2.]);
    assert!((world.pose(1).unwrap().rotation - 0.4).abs() < 0.001);
    assert!(world.set_kinematic_target(2, [0.; 2], 0.).is_err());
    assert!(
        world
            .insert(
                2,
                dim2::BodyDesc::default(),
                dim2::Shape::Ball { radius: 1. },
                Material::default()
            )
            .is_err()
    );
    assert!(
        world
            .insert(
                3,
                dim2::BodyDesc::default(),
                dim2::Shape::Ball { radius: f32::NAN },
                Material::default()
            )
            .is_err()
    );
    assert!(world.step(0.).is_err());
    assert!(world.step(f32::NAN).is_err());
}
#[test]
fn ccd_blocks_fast_ball_at_thin_wall() {
    let mut world = World3D::new([0.; 3], 4).unwrap();
    world
        .insert(
            1,
            dim3::BodyDesc {
                kind: BodyKind::Fixed,
                ..Default::default()
            },
            dim3::Shape::Box {
                half_extents: [0.05, 3., 3.],
            },
            Material::default(),
        )
        .unwrap();
    world
        .insert(
            2,
            dim3::BodyDesc {
                position: [-3., 0., 0.],
                velocity: [300., 0., 0.],
                ccd: true,
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.1 },
            Material::default(),
        )
        .unwrap();
    world.step(1. / 60.).unwrap();
    assert!(
        world.pose(2).unwrap().position[0] < 0.,
        "fast body tunneled through wall"
    );
}
