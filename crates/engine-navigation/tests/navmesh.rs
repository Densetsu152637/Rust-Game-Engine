use engine_navigation::*;
fn base_input() -> BakeInput {
    BakeInput {
        vertices: vec![],
        triangles: vec![],
        obstacles: vec![],
        settings: BakeSettings {
            cell_size: 0.5,
            agent_radius: 0.,
            ..Default::default()
        },
    }
}
fn floor(input: &mut BakeInput, min: [f32; 3], max: [f32; 3]) {
    let n = input.vertices.len() as u32;
    input
        .vertices
        .extend([min, [max[0], max[1], min[2]], max, [min[0], min[1], max[2]]]);
    input
        .triangles
        .extend([[n, n + 1, n + 2], [n, n + 2, n + 3]]);
}
fn points(mesh: &NavMesh, a: [f32; 3], b: [f32; 3]) -> Vec<[f32; 3]> {
    match mesh
        .find_path(
            a,
            b,
            QuerySettings {
                max_distance: 0.01,
                ..Default::default()
            },
        )
        .unwrap()
    {
        NavPath::Reached { points, .. } => points,
        other => panic!("{other:?}"),
    }
}
#[test]
fn continuous_straight_path_and_obstacle_funnel_stay_in_eroded_surface() {
    let mut input = base_input();
    floor(&mut input, [0., 0., 0.], [10., 0., 10.]);
    assert_eq!(
        points(&bake(&input).unwrap(), [1., 0., 1.], [9., 0., 9.]),
        vec![[1., 0., 1.], [9., 0., 9.]]
    );
    input.settings.agent_radius = 0.3;
    input.obstacles.push(Obstacle {
        min: [4., -1., 3.],
        max: [6., 3., 7.],
    });
    let mesh = bake(&input).unwrap();
    let path = points(&mesh, [2., 0., 5.], [8., 0., 5.]);
    assert!(path.len() > 2, "{path:?}");
    for pair in path.windows(2) {
        for i in 0..=100 {
            let t = i as f32 / 100.;
            let x = pair[0][0] * (1. - t) + pair[1][0] * t;
            let z = pair[0][2] * (1. - t) + pair[1][2] * t;
            assert!(
                x <= 3.5 + 1e-5 || x >= 6.5 - 1e-5 || z <= 2.5 + 1e-5 || z >= 7.5 - 1e-5,
                "path crossed clearance boundary at {x},{z}: {path:?}"
            );
        }
    }
}
#[test]
fn islands_narrow_passage_and_endpoint_projection_are_bounded() {
    let mut input = base_input();
    floor(&mut input, [0., 0., 0.], [3., 0., 3.]);
    floor(&mut input, [5., 0., 0.], [8., 0., 3.]);
    let mesh = bake(&input).unwrap();
    assert_eq!(
        mesh.find_path([1., 0., 1.], [6., 0., 1.], QuerySettings::default())
            .unwrap(),
        NavPath::Unreachable
    );
    assert!(
        mesh.nearest([100., 0., 100.], QuerySettings::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        mesh.find_path(
            [1., 0., 1.],
            [2., 0., 2.],
            QuerySettings {
                max_expansions: 0,
                ..Default::default()
            }
        )
        .unwrap(),
        NavPath::BudgetExceeded
    );
    assert_eq!(
        mesh.find_path(
            [1., 0., 1.],
            [2., 0., 2.],
            QuerySettings {
                max_polygons: 1,
                ..Default::default()
            }
        )
        .unwrap(),
        NavPath::BudgetExceeded
    );
    input = input_empty_corridor();
    assert!(!points(&bake(&input).unwrap(), [0.5, 0., 0.5], [5.5, 0., 0.5]).is_empty());
    input.settings.agent_radius = 0.6;
    assert_eq!(bake(&input).unwrap().polygon_count(), 0);
}
fn input_empty_corridor() -> BakeInput {
    let mut i = base_input();
    floor(&mut i, [0., 0., 0.], [6., 0., 1.]);
    i
}
#[test]
fn layered_floors_headroom_steps_and_slopes() {
    let mut input = base_input();
    floor(&mut input, [0., 0., 0.], [4., 0., 4.]);
    floor(&mut input, [0., 3., 0.], [4., 3., 4.]);
    let mesh = bake(&input).unwrap();
    assert_eq!(mesh.polygon_count(), 128);
    assert_eq!(
        points(&mesh, [1., 3., 1.], [3., 3., 3.]),
        vec![[1., 3., 1.], [3., 3., 3.]]
    );
    assert_eq!(
        mesh.find_path(
            [1., 0., 1.],
            [1., 3., 1.],
            QuerySettings {
                max_distance: 0.1,
                ..Default::default()
            }
        )
        .unwrap(),
        NavPath::Unreachable
    );
    input.settings.agent_height = 3.1;
    assert_eq!(bake(&input).unwrap().polygon_count(), 64);
    let mut step = input_empty_corridor();
    floor(&mut step, [6., 0.2, 0.], [9., 0.2, 1.]);
    let path = points(&bake(&step).unwrap(), [1., 0., 0.5], [8., 0.2, 0.5]);
    assert!(path.iter().any(|p| (p[0] - 6.).abs() < 1e-5 && p[1] == 0.2));
    step.settings.max_step = 0.1;
    assert_eq!(
        bake(&step)
            .unwrap()
            .find_path(
                [1., 0., 0.5],
                [8., 0.2, 0.5],
                QuerySettings {
                    max_distance: 0.1,
                    ..Default::default()
                }
            )
            .unwrap(),
        NavPath::Unreachable
    );
    let mut ramp = base_input();
    floor(&mut ramp, [0., 0., 0.], [4., 2., 4.]);
    let mesh = bake(&ramp).unwrap();
    let nearest = mesh
        .nearest(
            [1., 1.5, 1.],
            QuerySettings {
                max_distance: 0.95,
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert!(
        (nearest.position[0] - 1.4).abs() < 1e-5 && (nearest.position[1] - 0.7).abs() < 1e-5,
        "{nearest:?}"
    );
    ramp.settings.max_slope_degrees = 10.;
    assert_eq!(bake(&ramp).unwrap().polygon_count(), 0);
}
#[test]
fn rejects_invalid_oversized_overlapping_and_nonfinite_geometry() {
    let mut i = input_empty_corridor();
    i.vertices[0][0] = f32::NAN;
    assert!(bake(&i).is_err());
    let mut i = input_empty_corridor();
    i.triangles[0][0] = 99;
    assert!(bake(&i).is_err());
    let mut i = input_empty_corridor();
    i.triangles.push(i.triangles[0]);
    assert!(bake(&i).is_err());
    let mut i = base_input();
    floor(&mut i, [0., 0., 0.], [10000., 0., 10000.]);
    assert!(bake(&i).is_err());
    let mut i = input_empty_corridor();
    i.settings.cell_size = 0.;
    assert!(bake(&i).is_err());
    let mut i = input_empty_corridor();
    i.obstacles.push(Obstacle {
        min: [0.; 3],
        max: [f32::INFINITY; 3],
    });
    assert!(bake(&i).is_err());
}

#[test]
fn subcell_holes_and_partial_ceilings_are_not_lost_in_rasterization() {
    let mut input = base_input();
    floor(&mut input, [0., 0., 0.], [4.1, 0., 10.]);
    floor(&mut input, [4.2, 0., 0.], [10., 0., 10.]);
    floor(&mut input, [4.1, 0., 0.], [4.2, 0., 4.1]);
    floor(&mut input, [4.1, 0., 4.2], [4.2, 0., 10.]);
    let mesh = bake(&input).unwrap();
    assert!(
        mesh.nearest(
            [4.15, 0., 4.15],
            QuerySettings {
                max_distance: 0.01,
                ..Default::default()
            }
        )
        .unwrap()
        .is_none()
    );
    let mut input = base_input();
    floor(&mut input, [0., 0., 0.], [10., 0., 10.]);
    floor(&mut input, [4.1, 1., 4.1], [4.2, 1., 4.2]);
    assert!(
        bake(&input)
            .unwrap()
            .nearest(
                [4.15, 0., 4.15],
                QuerySettings {
                    max_distance: 0.01,
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn funnel_weaves_through_multiple_turns_without_cutting_removed_polygons() {
    let mut input = base_input();
    floor(&mut input, [0., 0., 0.], [10., 0., 10.]);
    input.settings.cell_size = 0.25;
    input.settings.agent_radius = 0.25;
    input.obstacles = vec![
        Obstacle {
            min: [2., -1., 0.],
            max: [3., 3., 7.],
        },
        Obstacle {
            min: [5., -1., 3.],
            max: [6., 3., 10.],
        },
        Obstacle {
            min: [8., -1., 0.],
            max: [9., 3., 7.],
        },
    ];
    let mesh = bake(&input).unwrap();
    let path = points(&mesh, [1., 0., 1.], [9.5, 0., 9.]);
    assert!(path.len() >= 6, "{path:?}");
    for pair in path.windows(2) {
        for i in 0..=100 {
            let t = i as f32 / 100.;
            let point = std::array::from_fn(|axis| pair[0][axis] * (1. - t) + pair[1][axis] * t);
            assert!(
                mesh.nearest(
                    point,
                    QuerySettings {
                        max_distance: 0.0001,
                        ..Default::default()
                    }
                )
                .unwrap()
                .is_some(),
                "{point:?} escaped mesh; {path:?}"
            );
        }
    }
}

#[test]
fn radius_clearance_accounts_for_neighbor_ceiling_at_step_elevation() {
    let mut input = base_input();
    input.settings.agent_radius = 0.2;
    floor(&mut input, [0., 0., 0.], [2., 0., 4.]);
    floor(&mut input, [2., 0.25, 0.], [4., 0.25, 4.]);
    input.obstacles.push(Obstacle {
        min: [0., 2., 0.],
        max: [2., 3., 4.],
    });
    let query = QuerySettings {
        max_distance: 0.01,
        ..Default::default()
    };
    let mesh = bake(&input).unwrap();
    assert!(mesh.nearest([2.1, 0.25, 2.], query).unwrap().is_none());
    assert_eq!(
        mesh.find_path([1., 0., 2.], [3., 0.25, 2.], query).unwrap(),
        NavPath::Unreachable
    );
    input.obstacles[0].min[1] = 2.3;
    assert!(matches!(
        bake(&input)
            .unwrap()
            .find_path([1., 0., 2.], [3., 0.25, 2.], query)
            .unwrap(),
        NavPath::Reached { .. }
    ));
    input.obstacles.clear();
    floor(&mut input, [0., 2., 0.], [2., 2., 4.]);
    assert_eq!(
        bake(&input)
            .unwrap()
            .find_path([1., 0., 2.], [3., 0.25, 2.], query)
            .unwrap(),
        NavPath::Unreachable
    );
    let mut ramp = base_input();
    ramp.settings.agent_radius = 0.2;
    floor(&mut ramp, [0., 0., 0.], [4., 2., 4.]);
    floor(&mut ramp, [0., 3., 0.], [4., 5., 4.]);
    assert!(matches!(
        bake(&ramp)
            .unwrap()
            .find_path([1., 0.5, 1.], [3., 1.5, 3.], query)
            .unwrap(),
        NavPath::Reached { .. }
    ));
}

#[test]
fn large_cells_and_translated_geometry_preserve_small_interior_holes() {
    for offset in [0., 10_000.] {
        let mut input = base_input();
        input.settings.cell_size = 100.;
        input.settings.agent_radius = 0.05;
        floor(
            &mut input,
            [offset, 0., offset],
            [offset + 149.9, 0., offset + 300.],
        );
        floor(
            &mut input,
            [offset + 150.1, 0., offset],
            [offset + 300., 0., offset + 300.],
        );
        floor(
            &mut input,
            [offset + 149.9, 0., offset],
            [offset + 150.1, 0., offset + 149.9],
        );
        floor(
            &mut input,
            [offset + 149.9, 0., offset + 150.1],
            [offset + 150.1, 0., offset + 300.],
        );
        let mesh = bake(&input).unwrap();
        let query = QuerySettings {
            max_distance: 0.01,
            ..Default::default()
        };
        assert!(
            mesh.nearest([offset + 150., 0., offset + 150.], query)
                .unwrap()
                .is_none(),
            "offset {offset}"
        );
        assert_eq!(
            mesh.find_path(
                [offset + 120., 0., offset + 150.],
                [offset + 180., 0., offset + 150.],
                query
            )
            .unwrap(),
            NavPath::Unreachable
        );
    }
}

#[test]
fn coverage_does_not_round_a_tiny_near_origin_hole_into_a_large_floor() {
    let mut input = base_input();
    input.settings.cell_size = 1.;
    floor(&mut input, [-1., 0., -1.], [1e-6, 0., 2.]);
    floor(&mut input, [2e-6, 0., -1.], [2., 0., 2.]);
    floor(&mut input, [1e-6, 0., -1.], [2e-6, 0., 1e-6]);
    floor(&mut input, [1e-6, 0., 2e-6], [2e-6, 0., 2.]);
    assert!(
        bake(&input)
            .unwrap()
            .nearest(
                [1.5e-6, 0., 1.5e-6],
                QuerySettings {
                    max_distance: 1e-8,
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
    );
}
