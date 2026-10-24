use engine_physics::{BodyKind, Material, World2D, World3D, dim2, dim3};

fn pair2(gravity: [f32; 2]) -> World2D {
    let mut world = World2D::new(gravity, 4).unwrap();
    for (id, kind, position) in [
        (1, BodyKind::Fixed, [0.; 2]),
        (2, BodyKind::Dynamic, [1., 0.]),
    ] {
        world
            .insert(
                id,
                dim2::BodyDesc {
                    kind,
                    position,
                    ..Default::default()
                },
                dim2::Shape::Ball { radius: 0.2 },
                Material::default(),
            )
            .unwrap();
    }
    world
}
fn pair3(gravity: [f32; 3]) -> World3D {
    let mut world = World3D::new(gravity, 4).unwrap();
    for (id, kind, position) in [
        (1, BodyKind::Fixed, [0.; 3]),
        (2, BodyKind::Dynamic, [1., 0., 0.]),
    ] {
        world
            .insert(
                id,
                dim3::BodyDesc {
                    kind,
                    position,
                    ..Default::default()
                },
                dim3::Shape::Ball { radius: 0.2 },
                Material::default(),
            )
            .unwrap();
    }
    world
}
#[test]
fn fixed_constraints_hold_bodies_against_gravity_in_both_dimensions() {
    let mut a = pair2([0., -9.81]);
    a.insert_joint(
        10,
        1,
        2,
        dim2::JointDesc::Fixed {
            anchor1: [1., 0.],
            anchor2: [0.; 2],
        },
    )
    .unwrap();
    let mut b = pair3([0., -9.81, 0.]);
    b.insert_joint(
        10,
        1,
        2,
        dim3::JointDesc::Fixed {
            anchor1: [1., 0., 0.],
            anchor2: [0.; 3],
        },
    )
    .unwrap();
    for _ in 0..120 {
        a.step(1. / 60.).unwrap();
        b.step(1. / 60.).unwrap();
    }
    assert!(a.pose(2).unwrap().position[1].abs() < 0.02);
    assert!(b.pose(2).unwrap().position[1].abs() < 0.02);
    assert!(a.remove_joint(10));
    assert!(!a.remove_joint(10));
    assert!(b.remove_joint(10));
    for _ in 0..30 {
        a.step(1. / 60.).unwrap();
        b.step(1. / 60.).unwrap();
    }
    assert!(a.pose(2).unwrap().position[1] < -0.5);
    assert!(b.pose(2).unwrap().position[1] < -0.5);
}
#[test]
fn hinges_allow_angular_impulse_but_preserve_anchor_in_both_dimensions() {
    let mut a = pair2([0.; 2]);
    a.insert_joint(
        10,
        1,
        2,
        dim2::JointDesc::Revolute {
            anchor1: [1., 0.],
            anchor2: [0.; 2],
            limits: None,
        },
    )
    .unwrap();
    let mut b = pair3([0.; 3]);
    b.insert_joint(
        10,
        1,
        2,
        dim3::JointDesc::Revolute {
            anchor1: [1., 0., 0.],
            anchor2: [0.; 3],
            axis: [0., 0., 2.],
            limits: None,
        },
    )
    .unwrap();
    a.apply_angular_impulse(2, 0.002).unwrap();
    b.apply_angular_impulse(2, [0., 0., 0.0005]).unwrap();
    for _ in 0..30 {
        a.step(1. / 60.).unwrap();
        b.step(1. / 60.).unwrap();
    }
    let pa = a.pose(2).unwrap();
    let pb = b.pose(2).unwrap();
    assert!(pa.rotation.abs() > 0.1, "{pa:?}");
    assert!(pb.rotation[2].abs() > 0.1, "{pb:?}");
    assert!((pa.position[0] - 1.).abs() < 0.02 && pa.position[1].abs() < 0.02);
    assert!((pb.position[0] - 1.).abs() < 0.02 && pb.position[1].abs() < 0.02);
    assert!(a.remove(2));
    assert!(b.remove(2));
    assert_eq!(a.joint_count(), 0);
    assert_eq!(b.joint_count(), 0);
    assert!(!a.has_joint(10));
    assert!(!b.has_joint(10));
    a.step(1. / 60.).unwrap();
    b.step(1. / 60.).unwrap();
}

#[test]
fn linear_impulses_work_before_first_step() {
    let mut a = pair2([0.; 2]);
    let mut b = pair3([0.; 3]);
    a.apply_impulse(2, [0., 1.]).unwrap();
    b.apply_impulse(2, [0., 1., 0.]).unwrap();
    assert!(a.pose(2).unwrap().velocity[1] > 0.);
    assert!(b.pose(2).unwrap().velocity[1] > 0.);
}
#[test]
fn hinge_limits_and_joint_validation() {
    let mut a = pair2([0.; 2]);
    a.insert_joint(
        10,
        1,
        2,
        dim2::JointDesc::Revolute {
            anchor1: [1., 0.],
            anchor2: [0.; 2],
            limits: Some([-0.2, 0.2]),
        },
    )
    .unwrap();
    a.step(1. / 60.).unwrap();
    a.apply_angular_impulse(2, 0.02).unwrap();
    for _ in 0..120 {
        a.step(1. / 60.).unwrap();
    }
    assert!(a.pose(2).unwrap().rotation.abs() < 0.23);
    let desc = dim2::JointDesc::Fixed {
        anchor1: [0.; 2],
        anchor2: [0.; 2],
    };
    assert!(a.insert_joint(10, 1, 2, desc).is_err());
    assert!(a.insert_joint(11, 1, 1, desc).is_err());
    assert!(a.insert_joint(11, 1, 99, desc).is_err());
    assert!(a.apply_angular_impulse(1, 1.).is_err());
    assert!(a.apply_angular_impulse(2, f32::NAN).is_err());
    let mut b = pair3([0.; 3]);
    assert!(
        b.insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Revolute {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                axis: [0.; 3],
                limits: None
            }
        )
        .is_err()
    );
    assert!(
        b.insert_joint(
            10,
            1,
            2,
            dim3::JointDesc::Revolute {
                anchor1: [0.; 3],
                anchor2: [0.; 3],
                axis: [0., 0., 1.],
                limits: Some([1., -1.])
            }
        )
        .is_err()
    );
}
