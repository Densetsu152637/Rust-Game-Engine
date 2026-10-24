use engine_physics::{BodyKind, Material, World2D, World3D, dim2, dim3};

const DT: f32 = 1. / 60.;
fn fixed3(world: &mut World3D, id: u64, position: [f32; 3], shape: dim3::Shape) {
    world
        .insert(
            id,
            dim3::BodyDesc {
                kind: BodyKind::Fixed,
                position,
                ..Default::default()
            },
            shape,
            Material::default(),
        )
        .unwrap();
}
fn dynamic3(world: &mut World3D, id: u64, position: [f32; 3], shape: dim3::Shape, density: f32) {
    world
        .insert(
            id,
            dim3::BodyDesc {
                position,
                ..Default::default()
            },
            shape,
            Material {
                density,
                ..Default::default()
            },
        )
        .unwrap();
}
fn fixed2(world: &mut World2D, id: u64, position: [f32; 2], shape: dim2::Shape) {
    world
        .insert(
            id,
            dim2::BodyDesc {
                kind: BodyKind::Fixed,
                position,
                ..Default::default()
            },
            shape,
            Material::default(),
        )
        .unwrap();
}
fn dynamic2(world: &mut World2D, id: u64, position: [f32; 2], shape: dim2::Shape) {
    world
        .insert(
            id,
            dim2::BodyDesc {
                position,
                ..Default::default()
            },
            shape,
            Material::default(),
        )
        .unwrap();
}
fn ground3(world: &mut World3D) {
    fixed3(
        world,
        1,
        [0., -0.5, 0.],
        dim3::Shape::Box {
            half_extents: [10., 0.5, 10.],
        },
    );
}
fn ground2(world: &mut World2D) {
    fixed2(
        world,
        1,
        [0., -0.5],
        dim2::Shape::Box {
            half_extents: [10., 0.5],
        },
    );
}
fn settle3(world: &mut World3D, steps: usize) {
    for _ in 0..steps {
        world.step(DT).unwrap();
    }
}
fn settle2(world: &mut World2D, steps: usize) {
    for _ in 0..steps {
        world.step(DT).unwrap();
    }
}
fn cube_corners(half: f32) -> Vec<[f32; 3]> {
    let mut points = vec![[0.; 3]];
    for x in [-half, half] {
        for y in [-half, half] {
            for z in [-half, half] {
                points.push([x, y, z]);
            }
        }
    }
    points
}

#[test]
fn heightfield_3d_maps_rows_to_z_columns_to_x_and_supports_a_resting_ball() {
    let mut world = World3D::new([0., -9.81, 0.], 8).unwrap();
    // Row-major heights: rows along Z, columns along X.
    let heights = (0..9)
        .map(|i| (i % 3) as f32 * 0.5 + (i / 3) as f32 * 0.1)
        .collect();
    fixed3(
        &mut world,
        1,
        [0.; 3],
        dim3::Shape::HeightField {
            rows: 3,
            columns: 3,
            heights,
            scale: [4., 1., 4.],
        },
    );
    let down = |world: &World3D, x: f32, z: f32| {
        5. - world
            .raycast([x, 5., z], [0., -1., 0.], 10., u32::MAX)
            .unwrap()
            .unwrap()
            .distance
    };
    // Row 0 / column 2 corner (x=+2, z=-2) is 1.0; row 2 / column 0 (x=-2, z=+2) is 0.2.
    assert!((down(&world, 1.99, -1.99) - 1.0).abs() < 0.02);
    assert!((down(&world, -1.99, 1.99) - 0.2).abs() < 0.02);
    let mut flat = World3D::new([0., -9.81, 0.], 8).unwrap();
    fixed3(
        &mut flat,
        1,
        [0.; 3],
        dim3::Shape::HeightField {
            rows: 4,
            columns: 4,
            heights: vec![0.25; 16],
            scale: [10., 1., 10.],
        },
    );
    dynamic3(
        &mut flat,
        2,
        [0.3, 3., -0.2],
        dim3::Shape::Ball { radius: 0.5 },
        1.,
    );
    settle3(&mut flat, 240);
    let y = flat.pose(2).unwrap().position[1];
    assert!((y - 0.75).abs() < 0.05, "{y}");
}

#[test]
fn triangle_mesh_3d_supports_a_resting_ball_and_rejects_dynamic_or_bad_indices() {
    let mut world = World3D::new([0., -9.81, 0.], 8).unwrap();
    let vertices = vec![[-5., 0., -5.], [5., 0., -5.], [5., 0., 5.], [-5., 0., 5.]];
    let mesh = dim3::Shape::TriMesh {
        vertices: vertices.clone(),
        indices: vec![[0, 2, 1], [0, 3, 2]],
    };
    fixed3(&mut world, 1, [0.; 3], mesh.clone());
    dynamic3(
        &mut world,
        2,
        [0.5, 3., 0.5],
        dim3::Shape::Ball { radius: 0.5 },
        1.,
    );
    settle3(&mut world, 240);
    let y = world.pose(2).unwrap().position[1];
    assert!((y - 0.5).abs() < 0.05, "{y}");
    assert!(
        world
            .insert(3, dim3::BodyDesc::default(), mesh, Material::default())
            .is_err()
    );
    let bad = dim3::Shape::TriMesh {
        vertices,
        indices: vec![[0, 1, 9]],
    };
    let fixed = dim3::BodyDesc {
        kind: BodyKind::Fixed,
        ..Default::default()
    };
    assert!(world.insert(4, fixed, bad, Material::default()).is_err());
}

#[test]
fn convex_hull_and_cylinder_have_volume_mass_and_rest_on_their_extent() {
    let mut world = World3D::new([0., -9.81, 0.], 8).unwrap();
    ground3(&mut world);
    // Interior point is ignored by the hull; density 2 over a unit cube gives 2 kg.
    dynamic3(
        &mut world,
        2,
        [0., 2., 0.],
        dim3::Shape::ConvexHull {
            points: cube_corners(0.5),
        },
        2.,
    );
    dynamic3(
        &mut world,
        3,
        [3., 2., 0.],
        dim3::Shape::Cylinder {
            half_height: 0.5,
            radius: 0.25,
        },
        1.,
    );
    let hull_mass = world.mass(2).unwrap();
    assert!((hull_mass - 2.).abs() < 1e-3, "{hull_mass}");
    let cylinder_mass = world.mass(3).unwrap();
    let expected = std::f32::consts::PI * 0.25 * 0.25;
    assert!((cylinder_mass - expected).abs() < 1e-3, "{cylinder_mass}");
    settle3(&mut world, 240);
    assert!((world.pose(2).unwrap().position[1] - 0.5).abs() < 0.05);
    assert!((world.pose(3).unwrap().position[1] - 0.5).abs() < 0.05);
    let coplanar = dim3::Shape::ConvexHull {
        points: vec![[0., 0., 0.], [1., 0., 0.], [0., 0., 1.], [1., 0., 1.]],
    };
    assert!(
        world
            .insert(4, dim3::BodyDesc::default(), coplanar, Material::default())
            .is_err()
    );
    let nan = dim3::Shape::ConvexHull {
        points: vec![[f32::NAN; 3]; 8],
    };
    assert!(
        world
            .insert(5, dim3::BodyDesc::default(), nan, Material::default())
            .is_err()
    );
    let flat = dim3::Shape::Cylinder {
        half_height: 0.,
        radius: 1.,
    };
    assert!(
        world
            .insert(6, dim3::BodyDesc::default(), flat, Material::default())
            .is_err()
    );
}

#[test]
fn compound_parts_contribute_mass_contacts_and_ray_hits() {
    let mut world = World3D::new([0., -9.81, 0.], 8).unwrap();
    ground3(&mut world);
    let part = |x: f32| dim3::CompoundPart {
        position: [x, 0., 0.],
        rotation: [0., 0., 0., 1.],
        shape: dim3::Shape::Ball { radius: 0.25 },
    };
    dynamic3(
        &mut world,
        2,
        [0., 2., 0.],
        dim3::Shape::Compound {
            parts: vec![part(-1.), part(1.)],
        },
        1.,
    );
    let ball = 4. / 3. * std::f32::consts::PI * 0.25f32.powi(3);
    assert!((world.mass(2).unwrap() - 2. * ball).abs() < 1e-3);
    let hit = world
        .raycast([1., 5., 0.], [0., -1., 0.], 10., u32::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(hit.id, 2);
    assert!((hit.distance - 2.75).abs() < 1e-3);
    // The gap between the parts is empty space.
    assert_eq!(
        world
            .raycast([0., 5., 0.], [0., -1., 0.], 10., u32::MAX)
            .unwrap()
            .unwrap()
            .id,
        1
    );
    settle3(&mut world, 240);
    assert!((world.pose(2).unwrap().position[1] - 0.25).abs() < 0.05);
    let nested = dim3::Shape::Compound {
        parts: vec![dim3::CompoundPart {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            shape: dim3::Shape::Compound {
                parts: vec![part(0.)],
            },
        }],
    };
    assert!(
        world
            .insert(3, dim3::BodyDesc::default(), nested, Material::default())
            .is_err()
    );
    assert!(
        world
            .insert(
                4,
                dim3::BodyDesc::default(),
                dim3::Shape::Compound { parts: vec![] },
                Material::default()
            )
            .is_err()
    );
}

#[test]
fn two_dimensional_hull_mesh_heightfield_and_compound_shapes() {
    let mut world = World2D::new([0., -9.81], 16).unwrap();
    ground2(&mut world);
    let square = vec![
        [-0.5, -0.5],
        [0.5, -0.5],
        [0.5, 0.5],
        [-0.5, 0.5],
        [0.1, 0.],
    ];
    dynamic2(
        &mut world,
        2,
        [0., 2.],
        dim2::Shape::ConvexHull { points: square },
    );
    assert!((world.mass(2).unwrap() - 1.).abs() < 1e-3);
    let pair = |x: f32| dim2::CompoundPart {
        position: [x, 0.],
        rotation: 0.,
        shape: dim2::Shape::Ball { radius: 0.25 },
    };
    dynamic2(
        &mut world,
        3,
        [4., 2.],
        dim2::Shape::Compound {
            parts: vec![pair(-0.5), pair(0.5)],
        },
    );
    settle2(&mut world, 240);
    assert!((world.pose(2).unwrap().position[1] - 0.5).abs() < 0.05);
    assert!((world.pose(3).unwrap().position[1] - 0.25).abs() < 0.05);
    assert!(
        world
            .insert(
                9,
                dim2::BodyDesc::default(),
                dim2::Shape::ConvexHull {
                    points: vec![[0., 0.], [1., 1.], [2., 2.]]
                },
                Material::default()
            )
            .is_err()
    );

    let mut terrain = World2D::new([0., -9.81], 8).unwrap();
    fixed2(
        &mut terrain,
        1,
        [0.; 2],
        dim2::Shape::HeightField {
            heights: vec![0.5, 0.5, 0.5, 0.5, 0.5],
            scale: [20., 1.],
        },
    );
    fixed2(
        &mut terrain,
        2,
        [30., 0.],
        dim2::Shape::TriMesh {
            vertices: vec![[-5., -1.], [5., -1.], [5., 0.], [-5., 0.]],
            indices: vec![[0, 1, 2], [0, 2, 3]],
        },
    );
    dynamic2(
        &mut terrain,
        3,
        [0.4, 3.],
        dim2::Shape::Ball { radius: 0.5 },
    );
    dynamic2(
        &mut terrain,
        4,
        [30.4, 3.],
        dim2::Shape::Ball { radius: 0.5 },
    );
    settle2(&mut terrain, 240);
    let on_heightfield = terrain.pose(3).unwrap().position[1];
    let on_mesh = terrain.pose(4).unwrap().position[1];
    assert!((on_heightfield - 1.).abs() < 0.05, "{on_heightfield}");
    assert!((on_mesh - 0.5).abs() < 0.05, "{on_mesh}");
    assert!(
        terrain
            .insert(
                5,
                dim2::BodyDesc::default(),
                dim2::Shape::HeightField {
                    heights: vec![0.; 4],
                    scale: [1., 1.]
                },
                Material::default()
            )
            .is_err()
    );
}
