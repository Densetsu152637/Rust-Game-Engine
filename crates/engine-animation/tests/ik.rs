use engine_animation::{ik::*, *};
fn skeleton() -> Skeleton {
    Skeleton::new(
        vec![None, Some(0), Some(1)],
        vec![Mat4::IDENTITY; 3],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    )
    .unwrap()
}
fn positions(skeleton: &Skeleton, pose: &[LocalPose]) -> Vec<Vec3> {
    skeleton
        .global_matrices(pose)
        .unwrap()
        .into_iter()
        .map(|m| m.transform_point3(Vec3::ZERO))
        .collect()
}
fn close(a: Vec3, b: Vec3) {
    assert!(a.distance(b) < 3e-4, "{a:?} != {b:?}");
}
fn lengths(p: &[Vec3]) {
    assert!((p[1].distance(p[0]) - 1.).abs() < 2e-4);
    assert!((p[2].distance(p[1]) - 1.).abs() < 2e-4);
}
#[test]
fn reachable_targets_and_opposite_poles_preserve_lengths_and_local_offsets() {
    let skeleton = skeleton();
    let solver = TwoBone::new(&skeleton, [0, 1, 2], BendLimits::default()).unwrap();
    let rest = skeleton.rest_pose().to_vec();
    let up = solver
        .solve(&skeleton, &rest, Vec3::new(1., 1., 0.), Vec3::Y * 2., 1.)
        .unwrap();
    let down = solver
        .solve(
            &skeleton,
            &rest,
            Vec3::new(1., 1., 0.),
            Vec3::NEG_Y * 2.,
            1.,
        )
        .unwrap();
    let up_positions = positions(&skeleton, &up);
    let down_positions = positions(&skeleton, &down);
    close(up_positions[2], Vec3::new(1., 1., 0.));
    close(down_positions[2], Vec3::new(1., 1., 0.));
    close(up_positions[1], Vec3::Y);
    close(down_positions[1], Vec3::X);
    lengths(&up_positions);
    lengths(&down_positions);
    for pole in [Vec3::Y, Vec3::NEG_Y] {
        let target = Vec3::new(1., 0., 1.);
        let solved = solver.solve(&skeleton, &rest, target, pole, 1.).unwrap();
        let p = positions(&skeleton, &solved);
        close(p[2], target);
        lengths(&p);
        assert!(p[1].y * pole.y > 0.7);
    }
    for (a, b) in rest.iter().zip(up) {
        assert_eq!(a.translation, b.translation);
        assert_eq!(a.scale, b.scale);
    }
    assert_eq!(rest, skeleton.rest_pose());
}
#[test]
fn unreachable_targets_clamp_and_elbow_limits_bound_flexion() {
    let skeleton = skeleton();
    let solver = TwoBone::new(&skeleton, [0, 1, 2], BendLimits::default()).unwrap();
    let out = solver
        .solve(&skeleton, skeleton.rest_pose(), Vec3::X * 20., Vec3::Y, 1.)
        .unwrap();
    let p = positions(&skeleton, &out);
    close(p[2], Vec3::X * 2.);
    lengths(&p);
    let opposite = solver
        .solve(
            &skeleton,
            skeleton.rest_pose(),
            Vec3::NEG_X * 20.,
            Vec3::Y,
            1.,
        )
        .unwrap();
    close(positions(&skeleton, &opposite)[2], Vec3::NEG_X * 2.);
    close(opposite[0].rotation * Vec3::Z, Vec3::Z);
    let mut unequal_rest = skeleton.rest_pose().to_vec();
    unequal_rest[2].translation = Vec3::X * 2.;
    let unequal = Skeleton::new(
        skeleton.parents().to_vec(),
        vec![Mat4::IDENTITY; 3],
        unequal_rest,
    )
    .unwrap();
    let inner = TwoBone::new(&unequal, [0, 1, 2], BendLimits::default())
        .unwrap()
        .solve(&unequal, unequal.rest_pose(), Vec3::X * 0.1, Vec3::Y, 1.)
        .unwrap();
    let inner_positions = positions(&unequal, &inner);
    close(inner_positions[2], Vec3::X);
    assert!((inner_positions[1].distance(inner_positions[0]) - 1.).abs() < 2e-4);
    assert!((inner_positions[2].distance(inner_positions[1]) - 2.).abs() < 2e-4);
    let limited = TwoBone::new(&skeleton, [0, 1, 2], BendLimits::new(0.3, 0.5).unwrap()).unwrap();
    for target in [Vec3::new(0.1, 0.1, 0.), Vec3::new(20., 1., 0.)] {
        let out = limited
            .solve(&skeleton, skeleton.rest_pose(), target, Vec3::Y * 4., 1.)
            .unwrap();
        let p = positions(&skeleton, &out);
        lengths(&p);
        let angle = (p[1] - p[0])
            .normalize()
            .dot((p[2] - p[1]).normalize())
            .clamp(-1., 1.)
            .acos();
        assert!((0.3 - 2e-4..=0.5 + 2e-4).contains(&angle), "{angle}");
    }
}
#[test]
fn weighted_results_remain_finite_and_preserve_lengths() {
    let skeleton = skeleton();
    let solver = TwoBone::new(&skeleton, [0, 1, 2], BendLimits::default()).unwrap();
    assert_eq!(
        solver
            .solve(&skeleton, skeleton.rest_pose(), Vec3::ZERO, Vec3::ZERO, 0.)
            .unwrap(),
        skeleton.rest_pose()
    );
    let out = solver
        .solve(
            &skeleton,
            skeleton.rest_pose(),
            Vec3::new(1., 1., 0.),
            Vec3::Y * 2.,
            0.5,
        )
        .unwrap();
    let p = positions(&skeleton, &out);
    lengths(&p);
    assert!(p.iter().all(|v| v.is_finite()));
    assert!(p[2].distance(Vec3::new(1., 1., 0.)) > 0.1);
}
#[test]
fn validated_non_joint_ancestors_and_uniform_scale_remain_in_skeleton_space() {
    let rest = vec![
        LocalPose {
            translation: Vec3::new(3., 1., 0.),
            rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            scale: Vec3::splat(2.),
        },
        LocalPose::default(),
        LocalPose {
            translation: Vec3::X,
            ..Default::default()
        },
        LocalPose {
            translation: Vec3::X,
            ..Default::default()
        },
    ];
    let skeleton = Skeleton::new(
        vec![None, Some(0), Some(1), Some(2)],
        vec![Mat4::IDENTITY; 4],
        rest,
    )
    .unwrap();
    let solver = TwoBone::new(&skeleton, [1, 2, 3], BendLimits::default()).unwrap();
    let out = solver
        .solve(
            &skeleton,
            skeleton.rest_pose(),
            Vec3::new(1., 3., 0.),
            Vec3::new(1., 1., 0.),
            1.,
        )
        .unwrap();
    let p = positions(&skeleton, &out);
    close(p[3], Vec3::new(1., 3., 0.));
    assert_eq!(out[0], skeleton.rest_pose()[0]);
    assert!((p[2].distance(p[1]) - 2.).abs() < 3e-4);
    assert!((p[3].distance(p[2]) - 2.).abs() < 3e-4);
}
#[test]
fn invalid_topology_limits_scales_and_ambiguous_targets_are_explicit_errors() {
    let skeleton = skeleton();
    assert!(TwoBone::new(&skeleton, [0, 2, 1], BendLimits::default()).is_err());
    assert!(TwoBone::new(&skeleton, [0, 1, 3], BendLimits::default()).is_err());
    assert!(BendLimits::new(1., 0.).is_err());
    assert!(BendLimits::new(0., f32::NAN).is_err());
    let solver = TwoBone::new(&skeleton, [0, 1, 2], BendLimits::default()).unwrap();
    let rest = skeleton.rest_pose();
    for (target, pole, weight) in [
        (Vec3::ZERO, Vec3::Y, 1.),
        (Vec3::X, Vec3::X * 2., 1.),
        (Vec3::splat(f32::NAN), Vec3::Y, 1.),
        (Vec3::X, Vec3::Y, 1.1),
    ] {
        assert!(solver.solve(&skeleton, rest, target, pole, weight).is_err());
    }
    for scale in [Vec3::new(1., 2., 1.), Vec3::splat(-1.)] {
        let mut pose = rest.to_vec();
        pose[0].scale = scale;
        assert!(
            solver
                .solve(&skeleton, &pose, Vec3::X, Vec3::Y, 1.)
                .is_err()
        );
    }
    let mut pose = rest.to_vec();
    pose[1].translation = Vec3::ZERO;
    assert!(
        solver
            .solve(&skeleton, &pose, Vec3::X, Vec3::Y, 1.)
            .is_err()
    );
    let folded = TwoBone::new(
        &skeleton,
        [0, 1, 2],
        BendLimits::new(std::f32::consts::PI, std::f32::consts::PI).unwrap(),
    )
    .unwrap();
    assert!(folded.solve(&skeleton, rest, Vec3::X, Vec3::Y, 1.).is_err());
}
