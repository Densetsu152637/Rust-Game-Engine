use engine_net_predict::{
    ColliderHistory, ColliderHistoryConfig, HitShape, Hitbox, LagCompensator, Pose, PredictError,
    QueryFilter, Ray, RewindError,
};
use glam::{Quat, Vec3};
use std::time::Duration;

const TARGET: u64 = 7;
const SHOOTER: u64 = 1;

/// Target moves +1 m/tick along x; the shooter stands still at the origin.
fn history() -> ColliderHistory {
    let mut h = ColliderHistory::new(ColliderHistoryConfig {
        max_ticks: 32,
        ..ColliderHistoryConfig::default()
    })
    .unwrap();
    h.set_hitboxes(
        TARGET,
        0b10,
        vec![
            Hitbox {
                shape: HitShape::Sphere {
                    center: Vec3::new(0.0, 1.6, 0.0),
                    radius: 0.25,
                },
                part: 1,
            },
            Hitbox {
                shape: HitShape::Capsule {
                    a: Vec3::new(0.0, 0.3, 0.0),
                    b: Vec3::new(0.0, 1.2, 0.0),
                    radius: 0.3,
                },
                part: 2,
            },
        ],
    )
    .unwrap();
    h.set_hitboxes(
        SHOOTER,
        0b01,
        vec![Hitbox {
            shape: HitShape::OrientedBox {
                center: Vec3::new(0.0, 1.0, 0.0),
                half_extents: Vec3::new(0.3, 1.0, 0.3),
                rotation: Quat::IDENTITY,
            },
            part: 0,
        }],
    )
    .unwrap();
    for tick in 0..=40u64 {
        h.record(
            tick,
            [
                (
                    TARGET,
                    Pose::new(Vec3::new(tick as f32, 0.0, 10.0), Quat::IDENTITY),
                ),
                (
                    SHOOTER,
                    Pose::new(Vec3::ZERO, Quat::from_rotation_y(tick as f32 * 0.1)),
                ),
            ],
        )
        .unwrap();
    }
    h
}

/// Ray aimed from the shooter's eye at where the target's torso was at `tick`.
fn shot_at(x: f32) -> Ray {
    Ray {
        origin: Vec3::new(0.0, 1.0, 0.0),
        direction: Vec3::new(x, 0.0, 10.0),
        max_distance: 100.0,
    }
}

#[test]
fn rewound_ray_hits_the_past_pose_and_misses_the_current_one() {
    let h = history();
    assert_eq!(
        (h.oldest_tick(), h.newest_tick(), h.len()),
        (Some(9), Some(40), 32)
    );
    let comp = LagCompensator::new(12).unwrap();
    let filter = QueryFilter {
        exclude: &[SHOOTER],
        ..QueryFilter::default()
    };
    let current = 40;
    let (rewind, hit) = comp
        .raycast(&h, 33.0, current, shot_at(33.0), &filter)
        .unwrap();
    assert!(!rewind.clamped);
    let hit = hit.expect("rewound shot hits");
    assert_eq!((hit.entity, hit.part), (TARGET, 2));
    assert!((hit.point.x - 33.0).abs() < 0.35 && (hit.point.z - 10.0).abs() < 0.35);
    // The same shot against the current pose misses (target moved 7 m).
    assert!(h.raycast(current as f64, shot_at(33.0), &filter).is_none());

    // Fractional view ticks interpolate the pose.
    let (_, hit) = comp
        .raycast(&h, 33.5, current, shot_at(33.5), &filter)
        .unwrap();
    assert_eq!(hit.unwrap().entity, TARGET);
    let pose = h.pose_at(TARGET, 33.5).unwrap();
    assert!((pose.translation.x - 33.5).abs() < 1.0e-5);
    let spun = h.pose_at(SHOOTER, 20.5).unwrap();
    assert!(spun.rotation.dot(Quat::from_rotation_y(2.05)).abs() > 1.0 - 1.0e-6);

    // Head shot via the sphere hitbox.
    let head = Ray {
        origin: Vec3::new(30.0, 1.6, 0.0),
        direction: Vec3::Z,
        max_distance: 20.0,
    };
    let (_, hit) = comp.raycast(&h, 30.0, current, head, &filter).unwrap();
    let hit = hit.unwrap();
    assert_eq!(hit.part, 1);
    assert!((hit.distance - 9.75).abs() < 1.0e-4);

    // Overlap and filters.
    let (_, overlaps) = comp
        .overlap_sphere(
            &h,
            35.0,
            current,
            Vec3::new(35.0, 1.0, 10.5),
            0.5,
            &QueryFilter::default(),
        )
        .unwrap();
    assert_eq!(overlaps.iter().map(|o| o.part).collect::<Vec<_>>(), [2]);
    let shooters_only = QueryFilter {
        layer_mask: 0b01,
        exclude: &[],
    };
    let (_, overlaps) = comp
        .overlap_sphere(
            &h,
            35.0,
            current,
            Vec3::new(35.0, 1.0, 10.5),
            0.5,
            &shooters_only,
        )
        .unwrap();
    assert!(overlaps.is_empty());
    let box_hit = h
        .raycast(
            40.0,
            Ray {
                origin: Vec3::new(0.0, 1.0, -5.0),
                direction: Vec3::Z,
                max_distance: 10.0,
            },
            &shooters_only,
        )
        .unwrap();
    assert_eq!(box_hit.entity, SHOOTER);
    assert!(
        box_hit.distance > 4.5 && box_hit.distance < 4.8,
        "{}",
        box_hit.distance
    );
}

#[test]
fn malicious_view_ticks_are_clamped_or_rejected() {
    let h = history();
    let comp =
        LagCompensator::from_duration(Duration::from_millis(200), Duration::from_nanos(16_666_667))
            .unwrap();
    assert_eq!(comp.max_rewind_ticks(), 11.0);
    let filter = QueryFilter {
        exclude: &[SHOOTER],
        ..QueryFilter::default()
    };
    // Claims a 30-tick-old view to hit a stale position: clamped to current - 11.
    let (rewind, hit) = comp.raycast(&h, 10.0, 40, shot_at(10.0), &filter).unwrap();
    assert!(rewind.clamped);
    assert_eq!(rewind.tick, 29.0);
    assert!(
        hit.is_none(),
        "clamped rewind no longer sees the stale pose"
    );
    assert_eq!(
        comp.raycast(&h, 41.0, 40, shot_at(41.0), &filter)
            .unwrap_err(),
        RewindError::FutureTick
    );
    assert_eq!(comp.rewind_tick(f64::NAN, 40), Err(RewindError::NonFinite));
    assert_eq!(
        comp.rewind_tick(f64::INFINITY, 40),
        Err(RewindError::NonFinite)
    );
    assert_eq!(comp.rewind_tick(-1.0e300, 40).unwrap().tick, 29.0);
    // History bound clamps too: max rewind beyond the retained window.
    let wide = LagCompensator::new(1000).unwrap();
    let rewind = wide.rewind_in(&h, 0.0, 40).unwrap();
    assert_eq!((rewind.tick, rewind.clamped), (9.0, true));
    let empty = ColliderHistory::new(ColliderHistoryConfig::default()).unwrap();
    assert_eq!(wide.rewind_in(&empty, 0.0, 0), Err(RewindError::NoHistory));
}

#[test]
fn history_is_bounded_and_validates_input() {
    let mut h = ColliderHistory::new(ColliderHistoryConfig {
        max_ticks: 4,
        max_entities: 2,
        max_hitboxes_per_entity: 1,
    })
    .unwrap();
    let sphere = Hitbox {
        shape: HitShape::Sphere {
            center: Vec3::ZERO,
            radius: 1.0,
        },
        part: 0,
    };
    h.set_hitboxes(1, 1, vec![sphere]).unwrap();
    h.set_hitboxes(2, 1, vec![sphere]).unwrap();
    assert!(matches!(
        h.set_hitboxes(3, 1, vec![sphere]),
        Err(PredictError::Full(_))
    ));
    assert!(h.set_hitboxes(1, 1, vec![sphere, sphere]).is_err());
    let bad = Hitbox {
        shape: HitShape::Sphere {
            center: Vec3::NAN,
            radius: 1.0,
        },
        part: 0,
    };
    assert!(h.set_hitboxes(1, 1, vec![bad]).is_err());
    let pose = Pose::new(Vec3::ZERO, Quat::IDENTITY);
    for tick in 0..1000 {
        h.record(tick, [(1, pose), (2, pose)]).unwrap();
    }
    assert_eq!(h.len(), 4);
    assert!(h.record(999, [(1, pose)]).is_err(), "ticks increase");
    assert!(
        h.record(1000, [(1, pose), (1, pose)]).is_err(),
        "duplicate entity"
    );
    assert!(
        h.record(1000, [(1, pose), (2, pose), (3, pose)]).is_err(),
        "entity cap"
    );
    let skewed = Pose::new(Vec3::ZERO, Quat::from_xyzw(0.0, 0.0, 0.0, 2.0));
    assert!(h.record(1000, [(1, skewed)]).is_err(), "non-unit rotation");
    assert_eq!(h.newest_tick(), Some(999), "failed records change nothing");
    assert_eq!(h.len(), 4);
    assert!(h.pose_at(1, 995.0).is_none(), "outside retained window");
    // Entity absent from a bracketing tick is not interpolated.
    h.record(1000, [(1, pose)]).unwrap();
    assert!(h.pose_at(2, 999.5).is_none());
    assert!(h.pose_at(2, 999.0).is_some());
}
