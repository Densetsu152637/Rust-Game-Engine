//! Contact-impulse read-back and forced sleep (the engine-physics support for destruction).
use engine_physics::{BodyKind, Material, World3D, dim3};

fn ground(world: &mut World3D) {
    world
        .insert(
            1,
            dim3::BodyDesc {
                kind: BodyKind::Fixed,
                position: [0., -0.5, 0.],
                ..dim3::BodyDesc::default()
            },
            dim3::Shape::Box {
                half_extents: [10., 0.5, 10.],
            },
            Material::default(),
        )
        .unwrap();
}

#[test]
fn contact_impulse_sums_to_the_momentum_a_landing_ball_loses() {
    let mut world = World3D::new([0., 0., 0.], 16).unwrap();
    ground(&mut world);
    let radius = 0.5;
    world
        .insert(
            2,
            dim3::BodyDesc {
                position: [0.3, 1.5, -0.2],
                velocity: [0., -10., 0.],
                ccd: true,
                ..dim3::BodyDesc::default()
            },
            dim3::Shape::Ball { radius },
            Material::default(),
        )
        .unwrap();
    let mass = world.mass(2).unwrap();
    assert_eq!(world.contact_impulse(2).unwrap().impulse, 0.);
    let (mut total, mut peak, mut point) = (0., 0f32, [0f32; 3]);
    let mut partner = None;
    for _ in 0..90 {
        world.step(1. / 120.).unwrap();
        let contact = world.contact_impulse(2).unwrap();
        total += contact.impulse;
        if contact.impulse > peak {
            peak = contact.impulse;
            point = contact.point;
            partner = contact.other;
        }
    }
    let expected = mass * 10.;
    println!("landing impulse {total:.3} N*s (m v = {expected:.3}), peak step {peak:.3}");
    assert!(
        (total - expected).abs() / expected < 0.1,
        "{total} vs {expected}"
    );
    assert_eq!(partner, Some(1));
    // The contact point is under the ball, on the ground plane.
    assert!(point[1].abs() < 0.05, "{point:?}");
    assert!(
        (point[0] - 0.3).abs() < 0.1 && (point[2] + 0.2).abs() < 0.1,
        "{point:?}"
    );
    assert!(world.contact_impulse(99).is_err());
}

#[test]
fn forced_sleep_zeroes_velocity_and_any_impulse_wakes_the_body() {
    let mut world = World3D::new([0., 0., 0.], 16).unwrap();
    ground(&mut world);
    world
        .insert(
            2,
            dim3::BodyDesc {
                position: [0., 5., 0.],
                velocity: [1., 0., 0.],
                ..dim3::BodyDesc::default()
            },
            dim3::Shape::Box {
                half_extents: [0.5, 0.5, 0.5],
            },
            Material::default(),
        )
        .unwrap();
    assert_eq!(world.is_sleeping(2), Ok(false));
    world.sleep(2).unwrap();
    assert_eq!(world.is_sleeping(2), Ok(true));
    assert_eq!(world.velocity(2).unwrap().linear, [0., 0., 0.]);
    world.step(1. / 60.).unwrap();
    assert_eq!(world.pose(2).unwrap().position, [0., 5., 0.]);
    world.apply_impulse(2, [3., 0., 0.]).unwrap();
    assert_eq!(world.is_sleeping(2), Ok(false));
    assert!(
        world.sleep(1).is_err(),
        "fixed bodies cannot be put to sleep"
    );
    assert_eq!(world.is_sleeping(1), Ok(false), "fixed bodies never sleep");
    assert!(world.sleep(42).is_err());
}
