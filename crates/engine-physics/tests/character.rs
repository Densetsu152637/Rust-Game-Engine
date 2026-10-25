//! Kinematic character controller behaviour in both dimensions (docs/physics-character.md).
use engine_physics::{BodyKind, Material};

const DT: f32 = 1. / 60.;
const G: f32 = 9.81;
const GROUND: u64 = 1;
const HERO: u64 = 2;
/// Capsule half segment + radius: the character stands this high above its feet.
const HALF: f32 = 0.8;

macro_rules! character_tests {
    ($dim:ident, $world:ty) => {
        use super::*;
        use engine_physics::$dim::{Autostep, BodyDesc, CharacterDesc, CharacterEventKind, Shape};

        fn world() -> $world {
            <$world>::new(p(0., -G), 64).unwrap()
        }
        fn fixed(world: &mut $world, id: u64, at: [f32; 2], angle: f32, half: [f32; 2]) {
            world
                .insert(
                    id,
                    BodyDesc {
                        kind: BodyKind::Fixed,
                        position: p(at[0], at[1]),
                        rotation: rot(angle),
                        ..Default::default()
                    },
                    cuboid(half[0], half[1]),
                    Material::default(),
                )
                .unwrap();
        }
        /// Flat floor with its top surface at y = 0.
        fn floor(world: &mut $world) {
            fixed(world, GROUND, [0., -0.5], 0., [60., 0.5]);
        }
        /// A slope through the origin rising by `angle` radians toward +x.
        fn slope(world: &mut $world, angle: f32) {
            fixed(
                world,
                GROUND,
                [0.5 * angle.sin(), -0.5 * angle.cos()],
                angle,
                [40., 0.5],
            );
        }
        fn hero(world: &mut $world, at: [f32; 2], desc: CharacterDesc) {
            world
                .insert(
                    HERO,
                    BodyDesc {
                        kind: BodyKind::Kinematic,
                        position: p(at[0], at[1]),
                        ..Default::default()
                    },
                    Shape::Capsule {
                        half_height: 0.5,
                        radius: 0.3,
                    },
                    Material::default(),
                )
                .unwrap();
            world.insert_character(HERO, desc).unwrap();
        }
        fn run(world: &mut $world, steps: usize) {
            for _ in 0..steps {
                world.step(DT).unwrap();
            }
        }
        fn x(world: &$world) -> f32 {
            world.pose(HERO).unwrap().position[0]
        }
        fn y(world: &$world) -> f32 {
            world.pose(HERO).unwrap().position[1]
        }

        #[test]
        fn walks_a_gentle_slope_and_is_stopped_by_a_steep_one() {
            let desc = CharacterDesc::default();
            let mut gentle = world();
            slope(&mut gentle, 30f32.to_radians());
            hero(&mut gentle, [0., HALF + 0.3], desc);
            run(&mut gentle, 30);
            let start = x(&gentle);
            gentle.set_character_velocity(HERO, p(3., 0.)).unwrap();
            run(&mut gentle, 120);
            let gained = x(&gentle) - start;
            assert!(gained > 4., "30 degree slope progress {gained}");
            assert!(gentle.character_state(HERO).unwrap().grounded);
            assert!(y(&gentle) > 2., "should have climbed, y={}", y(&gentle));

            let mut steep = world();
            slope(&mut steep, 60f32.to_radians());
            hero(&mut steep, [0., HALF + 0.3], desc);
            run(&mut steep, 30);
            let start = x(&steep);
            steep.set_character_velocity(HERO, p(3., 0.)).unwrap();
            run(&mut steep, 120);
            let progress = x(&steep) - start;
            assert!(progress < 0.15, "60 degree slope progress {progress}");
        }

        #[test]
        fn slopes_rising_toward_negative_x_behave_the_same() {
            for (angle, walks) in [(-30f32, true), (-60., false)] {
                let mut world = world();
                slope(&mut world, angle.to_radians());
                hero(&mut world, [0., HALF + 0.3], CharacterDesc::default());
                run(&mut world, 30);
                let start = x(&world);
                world.set_character_velocity(HERO, p(-3., 0.)).unwrap();
                run(&mut world, 120);
                let progress = start - x(&world);
                if walks {
                    assert!(progress > 4., "{angle} degree slope progress {progress}");
                } else {
                    assert!(progress < 0.15, "{angle} degree slope progress {progress}");
                }
            }
        }

        #[test]
        fn slides_down_slopes_steeper_than_the_slide_angle() {
            let mut steep = world();
            slope(&mut steep, 60f32.to_radians());
            hero(&mut steep, [0., HALF + 0.3], CharacterDesc::default());
            run(&mut steep, 20);
            let before = x(&steep);
            run(&mut steep, 90);
            let state = steep.character_state(HERO).unwrap();
            assert!(x(&steep) < before - 0.3, "slid {}", before - x(&steep));
            assert!(state.on_slope && state.grounded);

            let mut gentle = world();
            slope(&mut gentle, 30f32.to_radians());
            hero(&mut gentle, [0., HALF + 0.3], CharacterDesc::default());
            run(&mut gentle, 20);
            let before = x(&gentle);
            run(&mut gentle, 90);
            assert!(
                (x(&gentle) - before).abs() < 0.02,
                "a 30 degree slope holds"
            );
            assert!(!gentle.character_state(HERO).unwrap().sliding);
        }

        fn step_world(height: f32, autostep: Option<Autostep>) -> $world {
            let mut world = world();
            floor(&mut world);
            fixed(&mut world, 5, [11., height / 2.], 0., [9., height / 2.]);
            hero(
                &mut world,
                [0., HALF + 0.05],
                CharacterDesc {
                    autostep,
                    ..Default::default()
                },
            );
            run(&mut world, 20);
            world.set_character_velocity(HERO, p(1.5, 0.)).unwrap();
            run(&mut world, 180);
            world
        }

        #[test]
        fn autostep_climbs_low_steps_and_is_blocked_by_high_ones() {
            let auto = Some(Autostep {
                max_height: 0.3,
                min_width: 0.1,
                include_dynamic_bodies: true,
            });
            let low = step_world(0.2, auto);
            assert!(x(&low) > 3.5, "climbed 0.2 m step: x={}", x(&low));
            assert!(y(&low) > HALF + 0.15);
            let high = step_world(0.4, auto);
            assert!(x(&high) < 1.75, "blocked by 0.4 m step: x={}", x(&high));
            let none = step_world(0.2, None);
            assert!(x(&none) < 1.75, "no autostep: x={}", x(&none));
            let bodies = step_world(
                0.25,
                Some(Autostep {
                    include_dynamic_bodies: false,
                    ..auto.unwrap()
                }),
            );
            assert!(
                x(&bodies) > 3.5,
                "fixed step with dynamic bodies ignored: x={}",
                x(&bodies)
            );
        }

        #[test]
        fn snap_to_ground_keeps_the_character_grounded_going_downhill() {
            let mut hill = world();
            slope(&mut hill, -20f32.to_radians());
            hero(&mut hill, [0., HALF + 0.3], CharacterDesc::default());
            run(&mut hill, 30);
            hill.set_character_velocity(HERO, p(3., 0.)).unwrap();
            let mut airborne = 0;
            for _ in 0..120 {
                hill.step(DT).unwrap();
                airborne += usize::from(!hill.character_state(HERO).unwrap().grounded);
            }
            assert_eq!(airborne, 0, "left the slope");
            assert!(x(&hill) > 5., "x={}", x(&hill));

            let mut stairs = world();
            for i in 0..12 {
                let top = -0.1 * i as f32;
                fixed(
                    &mut stairs,
                    10 + i,
                    [i as f32 * 0.6 + 0.3, top - 0.5],
                    0.,
                    [0.3, 0.5],
                );
            }
            hero(&mut stairs, [0.3, HALF + 0.05], CharacterDesc::default());
            run(&mut stairs, 20);
            stairs.set_character_velocity(HERO, p(2., 0.)).unwrap();
            airborne = 0;
            for _ in 0..180 {
                stairs.step(DT).unwrap();
                airborne += usize::from(!stairs.character_state(HERO).unwrap().grounded);
            }
            assert_eq!(airborne, 0, "left the stairs");
            assert!(y(&stairs) < 0., "descended: y={}", y(&stairs));

            let mut unsnapped = world();
            slope(&mut unsnapped, -20f32.to_radians());
            hero(
                &mut unsnapped,
                [0., HALF + 0.3],
                CharacterDesc {
                    snap_to_ground: None,
                    ..Default::default()
                },
            );
            run(&mut unsnapped, 30);
            unsnapped.set_character_velocity(HERO, p(3., 0.)).unwrap();
            let mut left = 0;
            for _ in 0..120 {
                unsnapped.step(DT).unwrap();
                left += usize::from(!unsnapped.character_state(HERO).unwrap().grounded);
            }
            assert!(
                left > 5,
                "without snapping the character should hop: {left}"
            );
        }

        fn platform(world: &mut $world) {
            world
                .insert(
                    GROUND,
                    BodyDesc {
                        kind: BodyKind::Kinematic,
                        position: p(0., -0.25),
                        ..Default::default()
                    },
                    cuboid(5., 0.25),
                    Material::default(),
                )
                .unwrap();
            hero(world, [0., HALF + 0.02], CharacterDesc::default());
            run(world, 20);
        }

        #[test]
        fn rides_a_kinematic_platform() {
            let mut world = world();
            platform(&mut world);
            let mut platform_x = 0.;
            let mut airborne = 0;
            for _ in 0..120 {
                platform_x += 2. * DT;
                world
                    .set_kinematic_target(GROUND, p(platform_x, -0.25), rot(0.))
                    .unwrap();
                world.step(DT).unwrap();
                airborne += usize::from(!world.character_state(HERO).unwrap().grounded);
            }
            let state = world.character_state(HERO).unwrap();
            assert_eq!(airborne, 0);
            assert_eq!(state.ground_body, Some(GROUND));
            assert!(
                (state.velocity[0] - 2.).abs() < 0.05,
                "world velocity {:?}",
                state.velocity
            );
            assert!(
                (x(&world) - platform_x).abs() < 0.05,
                "hero {} platform {platform_x}",
                x(&world)
            );
            assert!((state.ground_velocity[0] - 2.).abs() < 0.05);
        }

        #[test]
        fn rides_a_rising_and_falling_platform() {
            let mut world = world();
            platform(&mut world);
            let mut platform_y = -0.25;
            let mut airborne = 0;
            for step in 0..180 {
                platform_y += if step < 90 { 1. } else { -1. } * DT;
                world
                    .set_kinematic_target(GROUND, p(0., platform_y), rot(0.))
                    .unwrap();
                world.step(DT).unwrap();
                airborne += usize::from(!world.character_state(HERO).unwrap().grounded);
            }
            assert_eq!(airborne, 0, "stayed on the lift");
            assert!(
                (y(&world) - (platform_y + 0.25 + HALF)).abs() < 0.05,
                "hero {} platform top {}",
                y(&world),
                platform_y + 0.25
            );
        }

        /// Distance the box moves when the character pushes with `factor` times the box mass.
        fn push_distance(factor: f32) -> f32 {
            let mut world = world();
            floor(&mut world);
            world
                .insert(
                    7,
                    BodyDesc {
                        position: p(1.5, 0.25),
                        ..Default::default()
                    },
                    cuboid(0.25, 0.25),
                    Material::default(),
                )
                .unwrap();
            let box_mass = world.mass(7).unwrap();
            hero(
                &mut world,
                [0., HALF + 0.05],
                CharacterDesc {
                    push_mass: factor * box_mass,
                    ..Default::default()
                },
            );
            run(&mut world, 20);
            world.set_character_velocity(HERO, p(2., 0.)).unwrap();
            // Contact starts after about 27 steps; stop early, before the box reaches the
            // character's own speed and the push saturates.
            run(&mut world, 40);
            world.pose(7).unwrap().position[0] - 1.5
        }

        #[test]
        fn pushes_dynamic_bodies_in_proportion_to_the_push_mass() {
            let none = push_distance(0.);
            let light = push_distance(0.02);
            let heavy = push_distance(5.);
            assert!(none < 0.01, "no push mass: moved {none}");
            assert!(light > none + 0.002, "light {light} none {none}");
            assert!(heavy > light * 1.5, "heavy {heavy} light {light}");
        }

        #[test]
        fn dropping_onto_ground_lands_once_with_the_impact_speed() {
            let mut world = world();
            floor(&mut world);
            hero(&mut world, [0., HALF + 0.05 + 2.], CharacterDesc::default());
            run(&mut world, 120);
            let events = world.drain_character_events();
            let grounded: Vec<_> = events
                .iter()
                .filter_map(|e| match e.kind {
                    CharacterEventKind::Grounded { impact_speed } => Some(impact_speed),
                    _ => None,
                })
                .collect();
            assert_eq!(grounded.len(), 1, "{events:?}");
            let expected = (2. * G * 2.).sqrt();
            assert!(
                (grounded[0] - expected).abs() < 0.35,
                "impact {} expected {expected}",
                grounded[0]
            );
            assert!(world.character_state(HERO).unwrap().grounded);
            assert!(world.drain_character_events().is_empty());
        }

        #[test]
        fn jumping_leaves_and_regains_the_ground() {
            let mut world = world();
            floor(&mut world);
            hero(&mut world, [0., HALF + 0.05], CharacterDesc::default());
            run(&mut world, 20);
            world.drain_character_events();
            world.character_jump(HERO, 5.).unwrap();
            let rest = y(&world);
            let mut apex = rest;
            for _ in 0..90 {
                world.step(DT).unwrap();
                apex = apex.max(y(&world));
            }
            let expected = 25. / (2. * G);
            assert!(
                (apex - rest - expected).abs() < 0.12,
                "apex {} expected {expected}",
                apex - rest
            );
            let kinds: Vec<_> = world
                .drain_character_events()
                .into_iter()
                .map(|e| e.kind)
                .collect();
            assert!(
                matches!(kinds[0], CharacterEventKind::LeftGround),
                "{kinds:?}"
            );
            assert!(matches!(kinds[1], CharacterEventKind::Grounded { .. }));
            assert_eq!(kinds.len(), 2);
        }

        #[test]
        fn walking_into_a_wall_reports_one_wall_start_facing_the_character() {
            let mut world = world();
            floor(&mut world);
            fixed(&mut world, 5, [3.5, 5.], 0., [0.5, 5.]);
            hero(&mut world, [0., HALF + 0.05], CharacterDesc::default());
            run(&mut world, 20);
            world.drain_character_events();
            world.set_character_velocity(HERO, p(2., 0.)).unwrap();
            run(&mut world, 120);
            let state = world.character_state(HERO).unwrap();
            assert!(state.wall_contact);
            assert!(state.wall_normal[0] < -0.95, "{:?}", state.wall_normal);
            let starts: Vec<_> = world
                .drain_character_events()
                .into_iter()
                .filter_map(|e| match e.kind {
                    CharacterEventKind::WallStart { normal } => Some(normal),
                    _ => None,
                })
                .collect();
            assert_eq!(starts.len(), 1, "{starts:?}");
            assert!(starts[0][0] < -0.95);
            assert!(x(&world) < 2.75);
            world.set_character_velocity(HERO, p(-2., 0.)).unwrap();
            run(&mut world, 10);
            assert!(
                world
                    .drain_character_events()
                    .iter()
                    .any(|e| matches!(e.kind, CharacterEventKind::WallStop { .. }))
            );
        }

        #[test]
        fn teleporting_moves_the_character_at_once_and_resets_its_ground_state() {
            let mut world = world();
            floor(&mut world);
            hero(&mut world, [0., HALF + 0.05], CharacterDesc::default());
            run(&mut world, 30);
            assert!(world.character_state(HERO).unwrap().grounded);
            world.set_character_velocity(HERO, p(2., 0.)).unwrap();
            world.set_character_position(HERO, p(10., 5.)).unwrap();
            let pose = world.pose(HERO).unwrap().position;
            assert_eq!((pose[0], pose[1]), (10., 5.));
            let state = world.character_state(HERO).unwrap();
            assert!(!state.grounded && state.ground_body.is_none());
            assert_eq!(state.vertical_speed, 0.);
            // One step later it has fallen by one step of gravity and walked 2 m/s, not been
            // pulled back to where it stood.
            run(&mut world, 1);
            assert!((x(&world) - (10. + 2. * DT)).abs() < 1e-4, "{}", x(&world));
            assert!((y(&world) - 5.).abs() < 0.01, "{}", y(&world));
            run(&mut world, 180);
            let state = world.character_state(HERO).unwrap();
            assert!(state.grounded);
            assert!((y(&world) - (HALF + 0.05)).abs() < 0.06, "{}", y(&world));
            assert!(world.set_character_position(GROUND, p(0., 0.)).is_err());
            assert!(world.set_character_position(HERO, p(f32::NAN, 0.)).is_err());
        }

        #[test]
        fn rejects_invalid_characters() {
            let mut world = world();
            floor(&mut world);
            assert!(
                world
                    .insert_character(GROUND, CharacterDesc::default())
                    .is_err()
            );
            assert!(
                world
                    .insert_character(99, CharacterDesc::default())
                    .is_err()
            );
            hero(&mut world, [0., 1.], CharacterDesc::default());
            assert!(
                world
                    .insert_character(HERO, CharacterDesc::default())
                    .is_err()
            );
            for bad in [
                CharacterDesc {
                    offset: 0.,
                    ..Default::default()
                },
                CharacterDesc {
                    up: p(0., 0.),
                    ..Default::default()
                },
                CharacterDesc {
                    max_slope_climb_angle: 3.,
                    ..Default::default()
                },
                CharacterDesc {
                    push_mass: f32::NAN,
                    ..Default::default()
                },
            ] {
                assert!(world.set_character_desc(HERO, bad).is_err());
            }
            assert!(world.set_character_velocity(HERO, p(f32::NAN, 0.)).is_err());
            assert!(world.character_jump(HERO, -1.).is_err());
            assert!(world.character_state(GROUND).is_none());
            assert!(world.remove(HERO));
            assert!(!world.is_character(HERO));
        }
    };
}

mod two {
    use engine_physics::World2D;
    fn p(x: f32, y: f32) -> [f32; 2] {
        [x, y]
    }
    fn rot(angle: f32) -> f32 {
        angle
    }
    fn cuboid(hx: f32, hy: f32) -> engine_physics::dim2::Shape {
        engine_physics::dim2::Shape::Box {
            half_extents: [hx, hy],
        }
    }
    character_tests!(dim2, World2D);
}
mod three {
    use engine_physics::World3D;
    fn p(x: f32, y: f32) -> [f32; 3] {
        [x, y, 0.]
    }
    fn rot(angle: f32) -> [f32; 4] {
        [0., 0., (angle / 2.).sin(), (angle / 2.).cos()]
    }
    fn cuboid(hx: f32, hy: f32) -> engine_physics::dim3::Shape {
        engine_physics::dim3::Shape::Box {
            half_extents: [hx, hy, 5.],
        }
    }
    character_tests!(dim3, World3D);
}
