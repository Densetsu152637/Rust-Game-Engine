use engine_animation::*;
use std::sync::Arc;
fn moving_clip() -> Clip {
    Clip::new(
        1.,
        1,
        vec![Track {
            joint: 0,
            translation: Some(
                VectorTrack::new(
                    vec![0., 1.],
                    vec![Vec3::ZERO, Vec3::X * 4.],
                    Interpolation::Linear,
                )
                .unwrap(),
            ),
            rotation: Some(
                RotationTrack::new(
                    vec![0., 1.],
                    vec![
                        Quat::IDENTITY,
                        Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                    ],
                    Interpolation::Linear,
                )
                .unwrap(),
            ),
            scale: None,
        }],
        vec![
            ClipEvent {
                time: 0.,
                name: "loop".into(),
            },
            ClipEvent {
                time: 0.5,
                name: "step".into(),
            },
        ],
    )
    .unwrap()
}
#[test]
fn samples_translation_rotation_loop_and_clamp() {
    let clip = moving_clip();
    let rest = [LocalPose::default()];
    let half = clip.sample(0.5, false, &rest).unwrap()[0];
    assert!((half.translation.x - 2.).abs() < 1e-5);
    let direction = half.rotation * Vec3::X;
    assert!((direction.x - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    assert!((clip.sample(1.25, true, &rest).unwrap()[0].translation.x - 1.).abs() < 1e-5);
    assert_eq!(
        clip.sample(8., false, &rest).unwrap()[0].translation,
        Vec3::X * 4.
    );
    assert!(clip.sample(f32::NAN, false, &rest).is_err());
}
#[test]
fn crossfade_is_continuous_and_events_survive_loop_boundaries() {
    let mut animator = Animator::new(vec![LocalPose::default()]).unwrap();
    let clip = Arc::new(moving_clip());
    animator.play(clip.clone(), Playback::default()).unwrap();
    assert_eq!(
        animator
            .update(0.75)
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        vec!["step"]
    );
    assert_eq!(
        animator
            .update(0.5)
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        vec!["loop"]
    );
    let from = animator.pose()[0].translation;
    animator
        .transition(
            clip.clone(),
            Playback {
                looping: false,
                speed: 0.,
            },
            1.,
        )
        .unwrap();
    assert_eq!(animator.pose()[0].translation, from);
    animator.update(0.5).unwrap();
    assert!((animator.pose()[0].translation - from * 0.5).length() < 1e-6);
    animator.update(0.5).unwrap();
    assert_eq!(animator.pose()[0].translation, Vec3::ZERO);
    let names = clip
        .events_between(0.25, 2.6, true)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["step", "loop", "step", "loop", "step"]);
    assert!(clip.events_between(0., 2000., true).is_err());
}
#[test]
fn skeleton_hierarchy_and_skinning_deform_geometry() {
    let rest = vec![
        LocalPose {
            translation: Vec3::Y,
            ..Default::default()
        },
        LocalPose::default(),
    ];
    // Parent comes after child in source order.
    let skeleton = Skeleton::new(
        vec![Some(1), None],
        vec![Mat4::from_translation(-Vec3::Y), Mat4::IDENTITY],
        rest.clone(),
    )
    .unwrap();
    let bind = skeleton.skin_matrices(&rest).unwrap();
    assert!(bind.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)));
    let mut pose = rest;
    pose[1].translation = Vec3::X * 2.;
    let matrices = skeleton.skin_matrices(&pose).unwrap();
    let (positions, normals) = skin_vertices(
        &[Vec3::Y],
        &[Vec3::Y],
        &[[0, 1, 0, 0]],
        &[[0.5, 0.5, 0., 0.]],
        &matrices,
    )
    .unwrap();
    assert!((positions[0] - Vec3::new(2., 1., 0.)).length() < 1e-5);
    assert_eq!(normals[0], Vec3::Y);
    assert!(
        skin_vertices(
            &[Vec3::ZERO],
            &[Vec3::Y],
            &[[9; 4]],
            &[[1., 0., 0., 0.]],
            &matrices
        )
        .is_err()
    );
}
#[test]
fn malformed_keys_skeletons_and_weights_are_rejected() {
    assert!(VectorTrack::new(vec![0., 0.], vec![Vec3::ZERO; 2], Interpolation::Linear).is_err());
    assert!(
        RotationTrack::new(
            vec![0.],
            vec![Quat::from_xyzw(0., 0., 0., 0.)],
            Interpolation::Linear
        )
        .is_err()
    );
    assert!(
        Skeleton::new(
            vec![Some(1), Some(0)],
            vec![Mat4::IDENTITY; 2],
            vec![LocalPose::default(); 2]
        )
        .is_err()
    );
    assert!(
        Skeleton::new(
            vec![Some(2)],
            vec![Mat4::IDENTITY],
            vec![LocalPose::default()]
        )
        .is_err()
    );
    assert!(
        skin_vertices(
            &[Vec3::ZERO],
            &[Vec3::Y],
            &[[0; 4]],
            &[[0.; 4]],
            &[Mat4::IDENTITY]
        )
        .is_err()
    );
    let step = Clip::new(
        1.,
        1,
        vec![Track {
            joint: 0,
            translation: Some(
                VectorTrack::new(vec![0., 1.], vec![Vec3::ZERO, Vec3::X], Interpolation::Step)
                    .unwrap(),
            ),
            rotation: None,
            scale: None,
        }],
        vec![],
    )
    .unwrap();
    assert_eq!(
        step.sample(0.9, false, &[LocalPose::default()]).unwrap()[0].translation,
        Vec3::ZERO
    );
}
