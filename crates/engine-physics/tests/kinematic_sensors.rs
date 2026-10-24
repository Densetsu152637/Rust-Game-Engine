use engine_physics::{BodyKind, Material, World2D, World3D, dim2, dim3};
#[test]
fn fixed_sensors_report_kinematic_enter_and_exit_in_both_dimensions() {
    let sensor = Material {
        sensor: true,
        ..Default::default()
    };
    let mut two = World2D::new([0.; 2], 4).unwrap();
    two.insert(
        1,
        dim2::BodyDesc {
            kind: BodyKind::Fixed,
            ..Default::default()
        },
        dim2::Shape::Ball { radius: 2. },
        sensor,
    )
    .unwrap();
    two.insert(
        2,
        dim2::BodyDesc {
            kind: BodyKind::Kinematic,
            position: [3., 0.],
            ..Default::default()
        },
        dim2::Shape::Ball { radius: 0.5 },
        Material::default(),
    )
    .unwrap();
    two.step(0.02).unwrap();
    assert!(two.drain_events().next().is_none());
    two.set_kinematic_target(2, [0.; 2], 0.).unwrap();
    // Rapier's contact phase precedes integration of the kinematic target.
    two.step(0.02).unwrap();
    two.step(0.02).unwrap();
    assert!(two.drain_events().any(|e| e.started));
    two.set_kinematic_target(2, [3., 0.], 0.).unwrap();
    two.step(0.02).unwrap();
    two.step(0.02).unwrap();
    assert!(two.drain_events().any(|e| !e.started));
    let mut three = World3D::new([0.; 3], 4).unwrap();
    three
        .insert(
            1,
            dim3::BodyDesc {
                kind: BodyKind::Fixed,
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 2. },
            sensor,
        )
        .unwrap();
    three
        .insert(
            2,
            dim3::BodyDesc {
                kind: BodyKind::Kinematic,
                position: [3., 0., 0.],
                ..Default::default()
            },
            dim3::Shape::Ball { radius: 0.5 },
            Material::default(),
        )
        .unwrap();
    three.step(0.02).unwrap();
    assert!(three.drain_events().next().is_none());
    three
        .set_kinematic_target(2, [0.; 3], [0., 0., 0., 1.])
        .unwrap();
    three.step(0.02).unwrap();
    three.step(0.02).unwrap();
    assert!(three.drain_events().any(|e| e.started));
    three
        .set_kinematic_target(2, [3., 0., 0.], [0., 0., 0., 1.])
        .unwrap();
    three.step(0.02).unwrap();
    three.step(0.02).unwrap();
    assert!(three.drain_events().any(|e| !e.started));
}
