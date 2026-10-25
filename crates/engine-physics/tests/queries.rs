use engine_physics::dim2::{self, QueryFilter as Filter2, SensorFilter as Sensors2};
use engine_physics::dim3::{self, KindFilter, QueryFilter, SensorFilter};
use engine_physics::{BodyKind, Material, World2D, World3D};

const TOL: f32 = 1e-4;

fn add3(world: &mut World3D, id: u64, kind: BodyKind, at: [f32; 3], shape: dim3::Shape) {
    add3m(world, id, kind, at, shape, Material::default());
}
fn add3m(
    world: &mut World3D,
    id: u64,
    kind: BodyKind,
    at: [f32; 3],
    shape: dim3::Shape,
    material: Material,
) {
    world
        .insert(
            id,
            dim3::BodyDesc {
                kind,
                position: at,
                ..Default::default()
            },
            shape,
            material,
        )
        .unwrap();
}
fn add2(world: &mut World2D, id: u64, kind: BodyKind, at: [f32; 2], shape: dim2::Shape) {
    world
        .insert(
            id,
            dim2::BodyDesc {
                kind,
                position: at,
                ..Default::default()
            },
            shape,
            Material::default(),
        )
        .unwrap();
}
fn ball3(radius: f32) -> dim3::Shape {
    dim3::Shape::Ball { radius }
}
fn cube3(half: f32) -> dim3::Shape {
    dim3::Shape::Box {
        half_extents: [half; 3],
    }
}
fn close(a: [f32; 3], b: [f32; 3]) {
    for i in 0..3 {
        assert!((a[i] - b[i]).abs() < TOL, "{a:?} != {b:?}");
    }
}
fn world3() -> World3D {
    World3D::new([0., -9.81, 0.], 64).unwrap()
}

#[test]
fn ray_hit_point_and_normal_match_analytic_box_and_sphere() {
    let mut world = world3();
    add3(&mut world, 1, BodyKind::Fixed, [0., 0., 5.], cube3(1.));
    add3(&mut world, 2, BodyKind::Fixed, [10., 0., 0.], ball3(2.));
    let all = QueryFilter::default();
    let hit = world
        .raycast_hit([0., 0., 0.], [0., 0., 1.], 100., &all)
        .unwrap()
        .unwrap();
    assert_eq!(hit.id, 1);
    assert!((hit.distance - 4.).abs() < TOL);
    close(hit.point, [0., 0., 4.]);
    close(hit.normal, [0., 0., -1.]);
    // Sphere, off-axis ray y = 1 along +x: hit at x = 10 - sqrt(4 - 1).
    let hit = world
        .raycast_hit([0., 1., 0.], [1., 0., 0.], 100., &all)
        .unwrap()
        .unwrap();
    let x = 10. - 3f32.sqrt();
    assert_eq!(hit.id, 2);
    assert!((hit.distance - x).abs() < TOL);
    close(hit.point, [x, 1., 0.]);
    let expected = [(x - 10.) / 2., 0.5, 0.];
    close(hit.normal, expected);
    // Direction need not be unit length; max distance clips.
    assert!(
        world
            .raycast_hit([0., 0., 0.], [0., 0., 7.], 3.9, &all)
            .unwrap()
            .is_none()
    );
    assert!(world.raycast_hit([0.; 3], [0.; 3], 1., &all).is_err());
}

#[test]
fn ray_hit_2d_matches_analytic_box() {
    let mut world = World2D::new([0., -9.81], 8).unwrap();
    add2(
        &mut world,
        1,
        BodyKind::Fixed,
        [4., 0.],
        dim2::Shape::Box {
            half_extents: [1., 1.],
        },
    );
    let hit = world
        .raycast_hit([0., 0.], [1., 0.], 50., &Filter2::default())
        .unwrap()
        .unwrap();
    assert_eq!(hit.id, 1);
    assert!((hit.distance - 3.).abs() < TOL);
    assert!((hit.point[0] - 3.).abs() < TOL && hit.point[1].abs() < TOL);
    assert!((hit.normal[0] + 1.).abs() < TOL && hit.normal[1].abs() < TOL);
}

#[test]
fn raycast_all_is_sorted_by_distance_then_id_and_truncates_nearest() {
    let mut world = world3();
    // Ids deliberately not in distance order; 5 and 3 are the same distance apart.
    for (id, z) in [(7, 9.), (5, 3.), (3, 3.), (9, 6.)] {
        add3(&mut world, id, BodyKind::Fixed, [0., 0., z], cube3(0.5));
    }
    let all = QueryFilter::default();
    let hits = world
        .raycast_all([0.; 3], [0., 0., 1.], 100., &all, 16)
        .unwrap();
    let ids: Vec<_> = hits.hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, [3, 5, 9, 7]);
    assert!(!hits.truncated);
    let cut = world
        .raycast_all([0.; 3], [0., 0., 1.], 100., &all, 2)
        .unwrap();
    assert_eq!(cut.hits.iter().map(|h| h.id).collect::<Vec<_>>(), [3, 5]);
    assert!(cut.truncated);
    assert!(
        world
            .raycast_all([0.; 3], [0., 0., 1.], 100., &all, 0)
            .is_err()
    );
    let closest = world
        .raycast_hit([0.; 3], [0., 0., 1.], 100., &all)
        .unwrap()
        .unwrap();
    assert_eq!(closest.id, 3, "equal distances resolve to the lowest id");
}

#[test]
fn filters_apply_to_every_query() {
    let mut world = world3();
    let sensor = Material {
        sensor: true,
        layer: 0b10,
        ..Default::default()
    };
    add3(&mut world, 1, BodyKind::Fixed, [0., 0., 2.], cube3(0.5));
    add3m(
        &mut world,
        2,
        BodyKind::Fixed,
        [0., 0., 4.],
        cube3(0.5),
        sensor,
    );
    add3(&mut world, 3, BodyKind::Kinematic, [0., 0., 6.], cube3(0.5));
    add3m(
        &mut world,
        4,
        BodyKind::Dynamic,
        [0., 0., 8.],
        cube3(0.5),
        Material {
            layer: 0b100,
            ..Default::default()
        },
    );
    let ids = |filter: &QueryFilter| -> Vec<u64> {
        world
            .raycast_all([0.; 3], [0., 0., 1.], 100., filter, 16)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id)
            .collect()
    };
    assert_eq!(ids(&QueryFilter::default()), [1, 2, 3, 4]);
    assert_eq!(ids(&QueryFilter::with_mask(0b100)), [4]);
    assert_eq!(ids(&QueryFilter::with_mask(0b10)), [2]);
    let mut filter = QueryFilter {
        sensors: SensorFilter::Exclude,
        ..Default::default()
    };
    assert_eq!(ids(&filter), [1, 3, 4]);
    filter.sensors = SensorFilter::Only;
    assert_eq!(ids(&filter), [2]);
    filter = QueryFilter {
        exclude: vec![1, 3],
        ..Default::default()
    };
    assert_eq!(ids(&filter), [2, 4]);
    filter = QueryFilter {
        kinds: KindFilter {
            fixed: false,
            dynamic: true,
            kinematic: true,
        },
        ..Default::default()
    };
    assert_eq!(ids(&filter), [3, 4]);
    // The same filter limits overlaps and point queries.
    let probe = |filter: &QueryFilter| -> Vec<u64> {
        world
            .overlap_shape(
                &dim3::Shape::Box {
                    half_extents: [5., 5., 20.],
                },
                [0.; 3],
                [0., 0., 0., 1.],
                filter,
                16,
            )
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id)
            .collect()
    };
    assert_eq!(probe(&QueryFilter::default()), [1, 2, 3, 4]);
    assert_eq!(
        probe(&QueryFilter {
            exclude: vec![2],
            sensors: SensorFilter::Exclude,
            ..Default::default()
        }),
        [1, 3, 4]
    );
    let too_many = QueryFilter {
        exclude: vec![0; engine_physics::dim3::MAX_QUERY_EXCLUDES + 1],
        ..Default::default()
    };
    assert!(world.project_point([0.; 3], &too_many).is_err());
}

#[test]
fn shape_cast_time_of_impact_matches_analytic_ball_to_plane() {
    let mut world = world3();
    // Ground "plane": a wide slab whose top face is y = 0.
    add3(
        &mut world,
        1,
        BodyKind::Fixed,
        [0., -1., 0.],
        dim3::Shape::Box {
            half_extents: [100., 1., 100.],
        },
    );
    let all = QueryFilter::default();
    let rotation = [0., 0., 0., 1.];
    let hit = world
        .cast_shape(
            &ball3(0.5),
            [3., 5., -2.],
            rotation,
            [0., -1., 0.],
            100.,
            &all,
        )
        .unwrap()
        .unwrap();
    assert_eq!(hit.id, 1);
    assert!((hit.distance - 4.5).abs() < TOL, "{}", hit.distance);
    assert!(!hit.penetrating);
    // Witness points come from an iterative GJK/EPA solve: accurate to ~1e-3, not 1e-4.
    for (got, want) in hit.point.iter().zip([3., 0., -2.]) {
        assert!((got - want).abs() < 2e-3, "{:?}", hit.point);
    }
    assert!((hit.normal[1] - 1.).abs() < 1e-3);
    // A tilted sweep: toi is path length, height drops by dist * sin.
    let dir = [1., -1., 0.];
    let hit = world
        .cast_shape(&ball3(0.5), [0., 5.5, 0.], rotation, dir, 100., &all)
        .unwrap()
        .unwrap();
    let expected = 5.0 * 2f32.sqrt();
    // Oblique sweeps converge by conservative advancement: ~1e-3, axis-aligned ones 1e-4.
    assert!((hit.distance - expected).abs() < 2e-3, "{}", hit.distance);
    // Box and capsule shapes sweep too.
    let boxed = world
        .cast_shape(
            &cube3(1.),
            [0., 3., 0.],
            rotation,
            [0., -1., 0.],
            100.,
            &all,
        )
        .unwrap()
        .unwrap();
    assert!((boxed.distance - 2.).abs() < TOL);
    let capsule = world
        .cast_shape(
            &dim3::Shape::Capsule {
                half_height: 1.,
                radius: 0.5,
            },
            [0., 4., 0.],
            rotation,
            [0., -1., 0.],
            100.,
            &all,
        )
        .unwrap()
        .unwrap();
    assert!((capsule.distance - 2.5).abs() < TOL, "{}", capsule.distance);
    // Out of range, wrong direction and starting overlap.
    assert!(
        world
            .cast_shape(&ball3(0.5), [0., 5., 0.], rotation, [0., -1., 0.], 4., &all)
            .unwrap()
            .is_none()
    );
    assert!(
        world
            .cast_shape(
                &ball3(0.5),
                [0., 5., 0.],
                rotation,
                [0., 1., 0.],
                100.,
                &all
            )
            .unwrap()
            .is_none()
    );
    let inside = world
        .cast_shape(
            &ball3(0.5),
            [0., 0.2, 0.],
            rotation,
            [1., 0., 0.],
            10.,
            &all,
        )
        .unwrap()
        .unwrap();
    assert!(inside.penetrating && inside.distance == 0.);
    let swept = world
        .cast_shape_all(
            &ball3(0.5),
            [0., 5., 0.],
            rotation,
            [0., -1., 0.],
            100.,
            &all,
            4,
        )
        .unwrap();
    assert_eq!(swept.hits.len(), 1);
}

#[test]
fn shape_cast_2d_matches_analytic_ball_to_ground() {
    let mut world = World2D::new([0., -9.81], 8).unwrap();
    add2(
        &mut world,
        1,
        BodyKind::Fixed,
        [0., -1.],
        dim2::Shape::Box {
            half_extents: [50., 1.],
        },
    );
    let hit = world
        .cast_shape(
            &dim2::Shape::Ball { radius: 0.25 },
            [1., 3.],
            0.,
            [0., -2.],
            100.,
            &Filter2::default(),
        )
        .unwrap()
        .unwrap();
    assert!((hit.distance - 2.75).abs() < TOL);
    assert!((hit.normal[1] - 1.).abs() < TOL);
}

#[test]
fn overlap_returns_exactly_the_intersecting_ids() {
    let mut world = world3();
    add3(&mut world, 10, BodyKind::Fixed, [0., 0., 0.], cube3(1.));
    add3(&mut world, 11, BodyKind::Dynamic, [1.8, 0., 0.], ball3(0.5));
    add3(&mut world, 12, BodyKind::Fixed, [5., 0., 0.], cube3(1.));
    add3(&mut world, 13, BodyKind::Fixed, [0., 2.4, 0.], cube3(0.5));
    let probe = ball3(1.);
    let rotation = [0., 0., 0., 1.];
    let hits = world
        .overlap_shape(&probe, [0.9, 0., 0.], rotation, &QueryFilter::default(), 16)
        .unwrap();
    // Distances are probe origin -> body origin: 10 at 0.9, 11 at 0.9 (tie -> id).
    let ids: Vec<_> = hits.hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, [10, 11]);
    assert!(!hits.truncated);
    // Moving the probe up touches the small box only.
    let hits = world
        .overlap_shape(&probe, [0., 3., 0.], rotation, &QueryFilter::default(), 16)
        .unwrap();
    assert_eq!(hits.hits.iter().map(|h| h.id).collect::<Vec<_>>(), [13]);
    // Limit keeps the nearest and flags truncation.
    let hits = world
        .overlap_shape(&probe, [0.9, 0., 0.], rotation, &QueryFilter::default(), 1)
        .unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert!(hits.truncated);
    assert!(
        world
            .overlap_shape(&probe, [50., 0., 0.], rotation, &QueryFilter::default(), 4)
            .unwrap()
            .hits
            .is_empty()
    );
}

#[test]
fn point_projection_and_containment() {
    let mut world = world3();
    add3(&mut world, 1, BodyKind::Fixed, [0., 0., 0.], cube3(1.));
    add3(&mut world, 2, BodyKind::Fixed, [10., 0., 0.], ball3(1.));
    let all = QueryFilter::default();
    let hit = world.project_point([3., 0., 0.], &all).unwrap().unwrap();
    assert_eq!(hit.id, 1);
    assert!(!hit.inside);
    assert!((hit.distance - 2.).abs() < TOL);
    close(hit.point, [1., 0., 0.]);
    let hit = world.project_point([8.5, 0., 0.], &all).unwrap().unwrap();
    assert_eq!(hit.id, 2);
    assert!((hit.distance - 0.5).abs() < TOL);
    let inside = world.contains_point([0.5, 0.5, 0.5], &all, 8).unwrap();
    assert_eq!(inside.hits.len(), 1);
    assert_eq!(inside.hits[0].id, 1);
    assert!(inside.hits[0].inside && inside.hits[0].distance == 0.);
    assert!(
        world
            .contains_point([5., 0., 0.], &all, 8)
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(world.project_point([f32::NAN, 0., 0.], &all).is_err());
}

#[test]
fn queries_are_fresh_immediately_after_insert_and_remove() {
    let mut world = world3();
    let all = QueryFilter::default();
    let ray = |world: &World3D| {
        world
            .raycast_hit([0.; 3], [0., 0., 1.], 50., &all)
            .unwrap()
            .map(|h| h.id)
    };
    assert_eq!(ray(&world), None);
    add3(&mut world, 1, BodyKind::Fixed, [0., 0., 5.], cube3(0.5));
    assert_eq!(ray(&world), Some(1), "visible before any step");
    world.step(1. / 60.).unwrap();
    add3(&mut world, 2, BodyKind::Fixed, [0., 0., 2.], cube3(0.5));
    assert_eq!(
        ray(&world),
        Some(2),
        "visible after a step, before the next"
    );
    assert_eq!(
        world
            .overlap_shape(&ball3(0.1), [0., 0., 2.], [0., 0., 0., 1.], &all, 4)
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(world.remove(2));
    assert_eq!(ray(&world), Some(1), "removed bodies vanish at once");
    assert!(
        world
            .overlap_shape(&ball3(0.1), [0., 0., 2.], [0., 0., 0., 1.], &all, 4)
            .unwrap()
            .hits
            .is_empty()
    );
}

#[test]
fn sensor_filter_2d_and_defaults() {
    let mut world = World2D::new([0., 0.], 8).unwrap();
    world
        .insert(
            1,
            dim2::BodyDesc {
                kind: BodyKind::Fixed,
                position: [2., 0.],
                ..Default::default()
            },
            dim2::Shape::Ball { radius: 0.5 },
            Material {
                sensor: true,
                ..Default::default()
            },
        )
        .unwrap();
    let default = Filter2::default();
    assert!(
        world
            .raycast_hit([0.; 2], [1., 0.], 10., &default)
            .unwrap()
            .is_some()
    );
    let solid_only = Filter2 {
        sensors: Sensors2::Exclude,
        ..Default::default()
    };
    assert!(
        world
            .raycast_hit([0.; 2], [1., 0.], 10., &solid_only)
            .unwrap()
            .is_none()
    );
}

fn stack() -> World3D {
    let mut world = world3();
    add3(
        &mut world,
        1,
        BodyKind::Fixed,
        [0., -1., 0.],
        dim3::Shape::Box {
            half_extents: [20., 1., 20.],
        },
    );
    for i in 0..3u64 {
        add3(
            &mut world,
            2 + i,
            BodyKind::Dynamic,
            [0., 0.5 + i as f32 * 1.05, 0.],
            cube3(0.5),
        );
    }
    add3m(
        &mut world,
        10,
        BodyKind::Fixed,
        [5., 1., 0.],
        ball3(0.5),
        Material {
            sensor: true,
            ..Default::default()
        },
    );
    world
}

#[test]
fn debug_view_lists_colliders_states_contacts_and_wireframe() {
    let mut world = stack();
    for _ in 0..30 {
        world.step(1. / 60.).unwrap();
    }
    let options = dim3::DebugOptions {
        aabbs: true,
        ..Default::default()
    };
    let view = world.debug_view(&options).unwrap();
    assert!(!view.truncated);
    assert_eq!(view.totals.colliders, 5);
    assert_eq!(
        view.colliders.iter().map(|c| c.id).collect::<Vec<_>>(),
        [1, 2, 3, 4, 10]
    );
    let state = |id: u64| view.colliders.iter().find(|c| c.id == id).unwrap().state;
    assert_eq!(state(1), dim3::DebugState::Fixed);
    assert_eq!(state(2), dim3::DebugState::Dynamic);
    assert_eq!(state(10), dim3::DebugState::Sensor);
    assert!(view.colliders.iter().all(|c| c.aabb.is_some()));
    assert!(!view.contacts.is_empty());
    assert!(view.contacts.iter().all(|c| c.depth >= 0.));
    // The ground contact normal is vertical (either direction depending on pair order).
    let ground = view
        .contacts
        .iter()
        .find(|c| c.first == 1 || c.second == 1)
        .unwrap();
    assert!((ground.normal[1].abs() - 1.).abs() < 1e-3);
    // Identical worlds give identical views.
    assert_eq!(view, world.debug_view(&options).unwrap());
    let lines = view.wireframe(100_000);
    // 4 boxes x 12 edges + 1 ball x 72 segments + 5 AABBs x 12 + contact markers.
    assert_eq!(
        lines.lines.len(),
        4 * 12 + 72 + 5 * 12 + view.contacts.len()
    );
    assert!(!lines.truncated);
    let cut = view.wireframe(10);
    assert_eq!(cut.lines.len(), 10);
    assert!(cut.truncated);
    // Sleeping bodies get their own key once the stack settles.
    for _ in 0..600 {
        world.step(1. / 60.).unwrap();
    }
    let settled = world.debug_view(&Default::default()).unwrap();
    assert!(
        settled
            .colliders
            .iter()
            .any(|c| c.state == dim3::DebugState::Sleeping)
    );
}

#[test]
fn debug_view_caps_items_and_flags_truncation() {
    let mut world = stack();
    for _ in 0..30 {
        world.step(1. / 60.).unwrap();
    }
    let view = world
        .debug_view(&dim3::DebugOptions {
            max_colliders: 2,
            max_contacts: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(view.colliders.len(), 2);
    assert_eq!(view.contacts.len(), 1);
    assert_eq!(view.totals.colliders, 5);
    assert!(view.totals.contacts > 1);
    assert!(view.truncated);
    assert!(
        world
            .debug_view(&dim3::DebugOptions {
                max_colliders: dim3::MAX_DEBUG_ITEMS + 1,
                ..Default::default()
            })
            .is_err()
    );
    let none = world
        .debug_view(&dim3::DebugOptions {
            contacts: false,
            ..Default::default()
        })
        .unwrap();
    assert!(none.contacts.is_empty() && !none.truncated);
}

#[test]
fn debug_view_reports_joint_anchors_and_contributed_markers() {
    let mut world = world3();
    add3(&mut world, 1, BodyKind::Fixed, [0., 0., 0.], ball3(0.2));
    add3(&mut world, 2, BodyKind::Dynamic, [1., 0., 0.], ball3(0.2));
    world
        .insert_joint(
            9,
            1,
            2,
            dim3::JointDesc::Fixed {
                anchor1: [0.5, 0., 0.],
                anchor2: [-0.5, 0., 0.],
            },
        )
        .unwrap();
    let view = world.debug_view(&Default::default()).unwrap();
    assert_eq!(view.anchors.len(), 2);
    assert_eq!(view.anchors[0].owner, 1);
    close(view.anchors[0].position, [0.5, 0., 0.]);
    assert_eq!(view.anchors[1].owner, 2);
    close(view.anchors[1].position, [0.5, 0., 0.]);
    fn marker(world: &dim3::World, sink: &mut dim3::DebugSink) {
        sink.anchor(1, "marker", [0., 3., 0.]);
        sink.line([0.; 3], [1.; 3], [1., 0., 1., 1.]);
        assert!(world.contains(1));
    }
    world.add_debug_contributor("marker", marker);
    world.add_debug_contributor("marker", marker);
    let view = world.debug_view(&Default::default()).unwrap();
    assert_eq!(view.anchors.len(), 3, "registering twice runs once");
    assert_eq!(view.lines.len(), 1);
    assert!(view.anchors.iter().any(|a| a.kind == "marker"));
    // Removing a joined body drops its anchors.
    world.remove(2);
    let view = world.debug_view(&Default::default()).unwrap();
    assert_eq!(view.anchors.iter().filter(|a| a.kind == "joint").count(), 0);
}

#[test]
fn debug_view_2d_describes_shapes() {
    let mut world = World2D::new([0., -9.81], 8).unwrap();
    add2(
        &mut world,
        1,
        BodyKind::Fixed,
        [0., -1.],
        dim2::Shape::Box {
            half_extents: [10., 1.],
        },
    );
    add2(
        &mut world,
        2,
        BodyKind::Dynamic,
        [0., 2.],
        dim2::Shape::Capsule {
            half_height: 0.5,
            radius: 0.25,
        },
    );
    let view = world
        .debug_view(&dim2::DebugOptions {
            aabbs: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(view.colliders.len(), 2);
    assert_eq!(
        view.colliders[1].shape,
        dim2::DebugShape::Capsule {
            half_height: 0.5,
            radius: 0.25
        }
    );
    // Box 4 + capsule (12 + 12 arc segments + 2 sides) + 2 AABBs x 4.
    assert_eq!(view.wireframe(1000).lines.len(), 4 + 26 + 8);
}

#[test]
fn identical_worlds_stay_bitwise_identical() {
    let run = || {
        let mut world = stack();
        for _ in 0..120 {
            world.step(1. / 60.).unwrap();
        }
        (2..=4)
            .map(|id| {
                let pose = world.pose(id).unwrap();
                (
                    pose.position.map(f32::to_bits),
                    pose.rotation.map(f32::to_bits),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}
