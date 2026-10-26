use engine_animation::{retarget::*, *};
use std::sync::Arc;
fn rig(parents: Vec<Option<usize>>, rest: Vec<LocalPose>) -> Arc<Skeleton> {
    Arc::new(Skeleton::new(parents, vec![Mat4::IDENTITY; rest.len()], rest).unwrap())
}
fn joint(source: usize, target: usize, basis: Quat) -> JointMapping {
    JointMapping {
        source,
        target,
        basis: basis.to_array(),
    }
}
fn definition(joints: Vec<JointMapping>) -> RetargetDefinition {
    RetargetDefinition {
        joints,
        root: RootTranslation::Discard,
    }
}
fn same_rotation(a: Quat, b: Quat) {
    assert!(a.dot(b).abs() > 1. - 2e-5, "{a:?} != {b:?}");
}
fn position(rig: &Skeleton, pose: &[LocalPose], i: usize) -> Vec3 {
    rig.global_matrices(pose).unwrap()[i].w_axis.truncate()
}
#[test]
fn rest_relative_motion_preserves_target_proportions_and_unmapped_local_rest() {
    let source = rig(
        vec![None, Some(0)],
        vec![
            LocalPose {
                rotation: Quat::from_rotation_z(0.3),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    );
    let target = rig(
        vec![None, Some(0), Some(1)],
        vec![
            LocalPose {
                translation: Vec3::X * 5.,
                rotation: Quat::from_rotation_y(0.5),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X * 3.,
                rotation: Quat::from_rotation_z(0.2),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X * 4.,
                rotation: Quat::from_rotation_x(0.1),
                ..Default::default()
            },
        ],
    );
    let map = RetargetMap::new(
        source.clone(),
        target.clone(),
        &definition(vec![
            joint(1, 1, Quat::IDENTITY),
            joint(0, 0, Quat::IDENTITY),
        ]),
    )
    .unwrap();
    let rest = map.apply(source.rest_pose()).unwrap();
    for (a, b) in rest.iter().zip(target.rest_pose()) {
        same_rotation(a.rotation, b.rotation);
        assert_eq!(a.translation, b.translation);
        assert_eq!(a.scale, b.scale);
    }
    let mut pose = source.rest_pose().to_vec();
    pose[0].rotation *= Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
    pose[1].rotation *= Quat::from_rotation_z(0.4);
    pose[0].translation = Vec3::splat(100.);
    pose[0].scale = Vec3::splat(2.);
    let out = map.apply(&pose).unwrap();
    same_rotation(
        out[0].rotation,
        target.rest_pose()[0].rotation * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
    );
    same_rotation(
        out[1].rotation,
        target.rest_pose()[1].rotation * Quat::from_rotation_z(0.4),
    );
    assert_eq!(out[2], target.rest_pose()[2]);
    for (a, b) in out.iter().zip(target.rest_pose()) {
        assert_eq!(a.translation, b.translation);
        assert_eq!(a.scale, b.scale);
    }
    assert!((position(&target, &out, 1).distance(position(&target, &out, 0)) - 3.).abs() < 3e-4);
    assert!((position(&target, &out, 2).distance(position(&target, &out, 1)) - 4.).abs() < 3e-4);
}
#[test]
fn explicit_joint_axis_basis_transfers_twist_between_different_bone_axes() {
    let source = rig(
        vec![None, Some(0)],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    );
    let target = rig(
        vec![None, Some(0)],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::Y * 2.,
                ..Default::default()
            },
        ],
    );
    let basis = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    let map = RetargetMap::new(
        source.clone(),
        target.clone(),
        &definition(vec![joint(0, 0, basis)]),
    )
    .unwrap();
    let mut pose = source.rest_pose().to_vec();
    pose[0].rotation = Quat::from_rotation_x(1.2);
    let out = map.apply(&pose).unwrap();
    same_rotation(out[0].rotation, Quat::from_rotation_y(1.2));
    assert!(position(&target, &out, 1).distance(Vec3::Y * 2.) < 3e-4);
}
#[test]
fn ancestor_gaps_use_rest_relative_motion_and_do_not_double_apply_mapped_parent_rotation() {
    let source = rig(
        vec![None, Some(0), Some(1)],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::X,
                rotation: Quat::from_rotation_z(0.4),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    );
    let target = rig(
        vec![None, Some(0), Some(1)],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::Y * 2.,
                rotation: Quat::from_rotation_y(0.3),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::Y * 2.,
                ..Default::default()
            },
        ],
    );
    let map = RetargetMap::new(
        source.clone(),
        target.clone(),
        &definition(vec![
            joint(0, 0, Quat::IDENTITY),
            joint(2, 2, Quat::IDENTITY),
        ]),
    )
    .unwrap();
    let mut pose = source.rest_pose().to_vec();
    pose[0].rotation = Quat::from_rotation_x(0.7);
    let inherited = map.apply(&pose).unwrap();
    same_rotation(inherited[0].rotation, pose[0].rotation);
    same_rotation(inherited[2].rotation, target.rest_pose()[2].rotation);
    assert_eq!(inherited[1], target.rest_pose()[1]);
    pose[1].rotation *= Quat::from_rotation_z(0.6);
    let moved = map.apply(&pose).unwrap();
    same_rotation(moved[2].rotation, Quat::from_rotation_z(0.6));
    assert_eq!(moved[1], target.rest_pose()[1]);
}
#[test]
fn explicit_root_translation_scale_and_alignment_operate_in_rest_parent_frames() {
    let source = rig(
        vec![None, Some(0)],
        vec![
            LocalPose {
                rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                scale: Vec3::splat(2.),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    );
    let target = rig(
        vec![None, Some(0)],
        vec![
            LocalPose {
                scale: Vec3::splat(4.),
                ..Default::default()
            },
            LocalPose {
                translation: Vec3::X * 3.,
                ..Default::default()
            },
        ],
    );
    let mut config = definition(vec![joint(1, 1, Quat::IDENTITY)]);
    config.root = RootTranslation::Scaled {
        source: 1,
        target: 1,
        scale: 3.,
        alignment: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2).to_array(),
    };
    let map = RetargetMap::new(source.clone(), target.clone(), &config).unwrap();
    let mut pose = source.rest_pose().to_vec();
    pose[1].translation += Vec3::X * 2.;
    let out = map.apply(&pose).unwrap();
    assert!(out[1].translation.distance(Vec3::X * 6.) < 3e-4);
    assert_eq!(out[0], target.rest_pose()[0]);
    let discard = RetargetMap::new(
        source,
        target.clone(),
        &definition(vec![joint(1, 1, Quat::IDENTITY)]),
    )
    .unwrap()
    .apply(&pose)
    .unwrap();
    assert_eq!(discard[1].translation, target.rest_pose()[1].translation);
}
#[test]
fn malformed_maps_topology_degenerate_segments_and_reflections_are_rejected() {
    let skeleton = rig(
        vec![None, Some(0)],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    );
    for config in [
        definition(vec![]),
        definition(vec![
            joint(0, 0, Quat::IDENTITY),
            joint(0, 1, Quat::IDENTITY),
        ]),
        definition(vec![
            joint(0, 0, Quat::IDENTITY),
            joint(1, 0, Quat::IDENTITY),
        ]),
        definition(vec![joint(9, 0, Quat::IDENTITY)]),
        definition(vec![
            joint(0, 1, Quat::IDENTITY),
            joint(1, 0, Quat::IDENTITY),
        ]),
        definition(vec![joint(0, 0, Quat::from_xyzw(0., 0., 0., 0.))]),
    ] {
        assert!(RetargetMap::new(skeleton.clone(), skeleton.clone(), &config).is_err());
    }
    let zero = rig(vec![None, Some(0)], vec![LocalPose::default(); 2]);
    assert!(
        RetargetMap::new(
            skeleton.clone(),
            zero,
            &definition(vec![
                joint(0, 0, Quat::IDENTITY),
                joint(1, 1, Quat::IDENTITY)
            ])
        )
        .is_err()
    );
    let config = definition(vec![
        joint(0, 0, Quat::IDENTITY),
        joint(1, 1, Quat::IDENTITY),
    ]);
    let map = RetargetMap::new(skeleton.clone(), skeleton.clone(), &config).unwrap();
    for scale in [Vec3::NEG_ONE, Vec3::new(1., 2., 1.)] {
        let mut pose = skeleton.rest_pose().to_vec();
        pose[0].scale = scale;
        assert!(map.apply(&pose).is_err());
        let bad = rig(skeleton.parents().to_vec(), pose);
        assert!(RetargetMap::new(bad.clone(), skeleton.clone(), &config).is_err());
        assert!(RetargetMap::new(skeleton.clone(), bad, &config).is_err());
    }
    assert!(map.apply(&[LocalPose::default()]).is_err());
    let mut pose = skeleton.rest_pose().to_vec();
    pose[1].rotation = Quat::from_xyzw(f32::NAN, 0., 0., 1.);
    assert!(map.apply(&pose).is_err());
}

#[test]
fn invalid_root_translation_pair_and_scale_are_rejected() {
    let skeleton = rig(
        vec![None, Some(0)],
        vec![
            LocalPose::default(),
            LocalPose {
                translation: Vec3::X,
                ..Default::default()
            },
        ],
    );
    for root in [
        RootTranslation::Scaled {
            source: 1,
            target: 1,
            scale: 1.,
            alignment: Quat::IDENTITY.to_array(),
        },
        RootTranslation::Scaled {
            source: 0,
            target: 0,
            scale: 0.,
            alignment: Quat::IDENTITY.to_array(),
        },
        RootTranslation::Scaled {
            source: 0,
            target: 0,
            scale: f32::NAN,
            alignment: Quat::IDENTITY.to_array(),
        },
        RootTranslation::Scaled {
            source: 0,
            target: 1,
            scale: 1.,
            alignment: Quat::IDENTITY.to_array(),
        },
    ] {
        let mut config = definition(vec![
            joint(0, 0, Quat::IDENTITY),
            joint(1, 1, Quat::IDENTITY),
        ]);
        config.root = root;
        assert!(RetargetMap::new(skeleton.clone(), skeleton.clone(), &config).is_err());
    }
}
