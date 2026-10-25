//! Authored character controllers: movement from `desired_velocity`, gravity, events,
//! validation, schema editing, native API and fixed-step determinism.
use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{
    self, CharacterController2D, CharacterController3D, Dimension,
};
use engine_project::*;
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

const H: f32 = 1. / 60.;

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn floor3() -> Value {
    entity(
        "floor",
        [0., -0.5, 0.],
        json!({"engine.PhysicsBody3D":value(json!({"motion":"fixed","half_extents":[60.,0.5,60.]}))}),
    )
}
fn floor2() -> Value {
    entity(
        "floor",
        [0., -0.5, 0.],
        json!({"engine.PhysicsBody2D":value(json!({"motion":"fixed","half_extents":[60.,0.5]}))}),
    )
}
fn hero3(position: [f32; 3], controller: Value) -> Value {
    entity(
        "hero",
        position,
        json!({
            "engine.PhysicsBody3D":value(json!({"motion":"kinematic","shape":"capsule","half_height":0.5,"radius":0.3})),
            "engine.CharacterController3D":value(controller),
        }),
    )
}
fn hero2(position: [f32; 3], controller: Value) -> Value {
    entity(
        "hero",
        position,
        json!({
            "engine.PhysicsBody2D":value(json!({"motion":"kinematic","shape":"capsule","half_height":0.5,"radius":0.3})),
            "engine.CharacterController2D":value(controller),
        }),
    )
}
fn bound(mut entity: Value) -> Value {
    entity["components"]["EventBindings"] = json!({"bindings":[
        {"event":"physics.character.grounded","handler":"record"},
        {"event":"physics.character.landed","handler":"record"},
        {"event":"physics.character.left_ground","handler":"record"},
        {"event":"physics.character.wall","handler":"record"}]});
    entity
}
fn fixture(entities: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"character-test","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
fn started(entities: Vec<Value>) -> (tempfile::TempDir, Runtime) {
    let dir = fixture(entities);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Events::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}
fn rejection(entities: Vec<Value>) -> String {
    let dir = fixture(entities);
    let result = Project::load(dir.path().join("project.json"))
        .and_then(Runtime::new)
        .and_then(|mut runtime| {
            runtime.start()?;
            runtime.tick(H, InputSnapshot::default())
        });
    result.expect_err("scene must be rejected").to_string()
}
fn ticks(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.tick(H, InputSnapshot::default()).unwrap();
    }
}
fn position(runtime: &Runtime, id: &str) -> Vec3 {
    let entity = runtime.entity(id).unwrap();
    Vec3::from(
        world_transform(runtime.app().world(), entity)
            .unwrap()
            .position(),
    )
}
fn hero(runtime: &Runtime) -> Entity {
    runtime.entity("hero").unwrap()
}

#[derive(Resource, Default)]
struct Events(Vec<(String, Value)>);
fn record(world: &mut World, _target: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Events>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}
fn events(runtime: &Runtime, name: &str) -> Vec<Value> {
    runtime
        .app()
        .world()
        .resource::<Events>()
        .0
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .collect()
}

#[test]
fn desired_velocity_moves_a_3d_character_across_the_floor() {
    let (_dir, mut runtime) = started(vec![
        floor3(),
        hero3([0., 0.85, 0.], json!({"desired_velocity":[2.,0.,0.]})),
    ]);
    ticks(&mut runtime, 60);
    let p = position(&runtime, "hero");
    assert!((p.x - 2.).abs() < 0.12, "x={}", p.x);
    assert!((p.y - 0.81).abs() < 0.02, "stays on the floor, y={}", p.y);
    assert!(p.z.abs() < 1e-3);
    let state = physics::character_state(runtime.app().world(), hero(&runtime)).unwrap();
    assert!(state.grounded);
    assert_eq!(state.ground_entity, runtime.entity("floor"));
    assert!((state.velocity.x - 2.).abs() < 0.05, "{:?}", state.velocity);
}

#[test]
fn desired_velocity_moves_a_2d_character_across_the_floor() {
    let (_dir, mut runtime) = started(vec![
        floor2(),
        hero2([0., 0.85, 0.], json!({"desired_velocity":[2.,0.]})),
    ]);
    ticks(&mut runtime, 60);
    let p = position(&runtime, "hero");
    assert!((p.x - 2.).abs() < 0.12, "x={}", p.x);
    assert!((p.y - 0.81).abs() < 0.02, "y={}", p.y);
    let state = physics::character_state(runtime.app().world(), hero(&runtime)).unwrap();
    assert_eq!(state.dimension, Dimension::Two);
    assert!(state.grounded);
}

#[test]
fn component_set_edits_the_authored_velocity_and_runtime_edits_move_the_entity() {
    let dir = fixture(vec![floor3(), hero3([0., 0.85, 0.], json!({}))]);
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |data: Value| EditOperation::SetComponent {
        entity_id: "hero".into(),
        component: "engine.CharacterController3D".into(),
        value: value(data),
        merge: true,
    };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(set(json!({"desired_velocity":[0.,0.,3.]})), &revision)
        .unwrap();
    for bad in [
        json!({"desired_velocity":[1.,2.]}),
        json!({"offset":0.0}),
        json!({"unknown":1}),
        json!({"autostep":{"max_height":-1.}}),
    ] {
        assert!(
            document.apply(set(bad.clone()), &revision).is_err(),
            "{bad}"
        );
    }
    document.save(&revision).unwrap();

    let project = Project::load(dir.path().join("project.json")).unwrap();
    for (name, n) in [
        ("engine.CharacterController2D", 2),
        ("engine.CharacterController3D", 3),
    ] {
        let descriptor = &project.component_catalog.0[name];
        assert_eq!(
            descriptor.schema["properties"]["desired_velocity"]["minItems"],
            n
        );
        assert!(
            descriptor
                .requires
                .iter()
                .any(|r| r.starts_with("engine.PhysicsBody"))
        );
    }
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    ticks(&mut runtime, 60);
    let p = position(&runtime, "hero");
    assert!(
        (p.z - 3.).abs() < 0.2,
        "authored velocity applied: z={}",
        p.z
    );

    // Runtime edits of the component take effect at the next sync.
    let e = hero(&runtime);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<CharacterController3D>(e)
        .unwrap()
        .desired_velocity = [-2., 0., 0.];
    let before = position(&runtime, "hero");
    ticks(&mut runtime, 30);
    let after = position(&runtime, "hero");
    assert!((after.x - before.x + 1.).abs() < 0.1, "{before} -> {after}");
}

#[test]
fn gravity_landing_and_wall_events_are_delivered_with_payloads() {
    let (_dir, mut runtime) = started(vec![
        floor3(),
        entity(
            "wall",
            [4., 5., 0.],
            json!({"engine.PhysicsBody3D":value(json!({"motion":"fixed","half_extents":[0.5,5.,5.]}))}),
        ),
        bound(hero3([0., 2.85, 0.], json!({"landing_min_speed":2.}))),
    ]);
    ticks(&mut runtime, 120);
    let landed = events(&runtime, "physics.character.landed");
    assert_eq!(landed.len(), 1, "{landed:?}");
    let expected = (2. * 9.81f32 * 2.).sqrt();
    let impact = landed[0]["impact_speed"].as_f64().unwrap() as f32;
    assert!(
        (impact - expected).abs() < 0.4,
        "impact {impact} vs {expected}"
    );
    assert_eq!(landed[0]["dimension"], 3);
    assert_eq!(events(&runtime, "physics.character.grounded").len(), 1);
    assert!(events(&runtime, "physics.character.left_ground").is_empty());

    let e = hero(&runtime);
    physics::set_character_velocity(runtime.app_mut().world_mut(), e, Vec3::X * 2.).unwrap();
    ticks(&mut runtime, 150);
    let walls = events(&runtime, "physics.character.wall");
    assert_eq!(walls.len(), 1, "{walls:?}");
    assert_eq!(walls[0]["phase"], "start");
    assert!(
        walls[0]["normal"][0].as_f64().unwrap() < -0.9,
        "{:?}",
        walls[0]
    );
    let state = physics::character_state(runtime.app().world(), hero(&runtime)).unwrap();
    assert!(state.wall_contact && state.wall_normal.x < -0.9);
    assert!(position(&runtime, "hero").x < 3.3);
}

#[test]
fn native_jump_api_leaves_the_ground_and_lands_again() {
    let (_dir, mut runtime) = started(vec![
        floor3(),
        bound(hero3([0., 0.85, 0.], json!({"jump_speed":5.}))),
    ]);
    ticks(&mut runtime, 10);
    let e = hero(&runtime);
    assert!(
        physics::character_state(runtime.app().world(), e)
            .unwrap()
            .grounded
    );
    physics::character_jump(runtime.app_mut().world_mut(), e).unwrap();
    let mut apex: f32 = 0.;
    for _ in 0..90 {
        ticks(&mut runtime, 1);
        apex = apex.max(position(&runtime, "hero").y);
    }
    let rise = apex - 0.81;
    assert!((rise - 25. / (2. * 9.81)).abs() < 0.12, "rise {rise}");
    assert_eq!(events(&runtime, "physics.character.left_ground").len(), 1);
    assert_eq!(events(&runtime, "physics.character.landed").len(), 1);
    // A direct impulse works in the air; a counter request is ignored there.
    physics::character_impulse(runtime.app_mut().world_mut(), e, 4.).unwrap();
    ticks(&mut runtime, 5);
    assert!(position(&runtime, "hero").y > 1.0);
    let floor = runtime.entity("floor").unwrap();
    assert!(physics::character_impulse(runtime.app_mut().world_mut(), floor, 1.).is_err());
}

#[test]
fn characters_must_be_kinematic_bodies_without_a_navigation_agent() {
    let controller = |body: Value, extra: Option<(&str, Value)>| {
        let mut custom = json!({
            "engine.PhysicsBody3D":value(body),
            "engine.CharacterController3D":value(json!({})),
        });
        if let Some((name, data)) = extra {
            custom[name] = value(data);
        }
        vec![floor3(), entity("hero", [0., 1., 0.], custom)]
    };
    let dynamic = rejection(controller(json!({"motion":"dynamic"}), None));
    assert!(dynamic.contains("kinematic"), "{dynamic}");
    let shape = rejection(controller(
        json!({"motion":"kinematic","shape":"heightfield","heights":[0.,0.,0.,0.],"heightfield_rows":2,"heightfield_columns":2}),
        None,
    ));
    assert!(shape.contains("Character shapes"), "{shape}");
    let nav = rejection(controller(
        json!({"motion":"kinematic","shape":"capsule"}),
        Some(("engine.NavigationAgent", json!({"waypoints":[[1.,0.,0.]]}))),
    ));
    assert!(nav.contains("NavigationAgent"), "{nav}");
    let two = rejection(vec![
        floor2(),
        entity(
            "hero",
            [0., 1., 0.],
            json!({
                "engine.PhysicsBody3D":value(json!({"motion":"kinematic"})),
                "engine.CharacterController2D":value(json!({})),
            }),
        ),
    ]);
    assert!(two.contains("requires engine.PhysicsBody2D"), "{two}");
    let bad = rejection(vec![
        floor3(),
        hero3([0., 1., 0.], json!({"max_slope_climb_angle":3.})),
    ]);
    assert!(bad.contains("outside bounds"), "{bad}");
}

#[test]
fn removing_the_controller_releases_the_body() {
    let (_dir, mut runtime) = started(vec![
        floor3(),
        hero3([0., 0.85, 0.], json!({"desired_velocity":[1.,0.,0.]})),
    ]);
    ticks(&mut runtime, 20);
    let e = hero(&runtime);
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(e)
        .remove::<CharacterController3D>();
    ticks(&mut runtime, 2);
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert!(!three.is_character(e.to_bits()));
    assert!(physics::character_state(runtime.app().world(), e).is_err());
    let rest = position(&runtime, "hero");
    ticks(&mut runtime, 20);
    assert_eq!(
        position(&runtime, "hero"),
        rest,
        "now an ordinary kinematic body"
    );
    let _ = CharacterController2D::default();
}

fn run_script(frames: &[f32]) -> Vec<[u32; 3]> {
    let h = 1.0f32 / 60.0;
    let (_dir, mut runtime) = started(vec![
        settings(json!({"fixed_timestep":h,"max_substeps":64})),
        floor3(),
        entity(
            "step",
            [3., 0.1, 0.],
            json!({"engine.PhysicsBody3D":value(json!({"motion":"fixed","half_extents":[0.5,0.1,5.]}))}),
        ),
        entity(
            "crate",
            [2., 0.25, 0.1],
            json!({"engine.PhysicsBody3D":value(json!({"half_extents":[0.25,0.25,0.25]}))}),
        ),
        hero3(
            [0., 1.85, 0.],
            json!({"push_mass":2.,"autostep":{"max_height":0.3,"min_width":0.1},"desired_velocity":[2.,0.,0.1]}),
        ),
    ]);
    let e = hero(&runtime);
    for (i, dt) in frames.iter().enumerate() {
        if i == 40 {
            physics::character_jump(runtime.app_mut().world_mut(), e).unwrap();
        }
        runtime.tick(*dt, InputSnapshot::default()).unwrap();
    }
    ["hero", "crate"]
        .iter()
        .map(|id| position(&runtime, id).to_array().map(f32::to_bits))
        .collect()
}

#[test]
fn fixed_step_character_runs_are_bitwise_identical_and_independent_of_frame_splits() {
    let h = 1.0f32 / 60.0;
    let reference = run_script(&vec![h; 120]);
    assert_eq!(reference, run_script(&vec![h; 120]), "identical runs");
    // The scene is not trivially static: the character moved and pushed the crate.
    let start = [0., 1.85, 0.].map(f32::to_bits);
    assert_ne!(reference[0], start);
    assert_ne!(reference[1], [2., 0.25, 0.1].map(f32::to_bits));
    let uneven: Vec<f32> = (0..60).flat_map(|_| [h * 1.5, h * 0.5]).collect();
    assert_eq!(uneven.len(), 120);
    assert_eq!(run_script(&uneven), reference, "frame splits do not matter");
}

#[test]
fn a_character_rides_a_kinematic_platform_entity() {
    let (_dir, mut runtime) = started(vec![
        floor3(),
        entity(
            "lift",
            [0., -0.25, 5.],
            json!({"engine.PhysicsBody3D":value(json!({"motion":"kinematic","half_extents":[3.,0.25,3.]}))}),
        ),
        hero3([0., 0.85, 5.], json!({})),
    ]);
    ticks(&mut runtime, 20);
    let lift = runtime.entity("lift").unwrap();
    for step in 0..90 {
        {
            let mut transform = runtime
                .app_mut()
                .world_mut()
                .get_mut::<Transform>(lift)
                .unwrap();
            transform.position[0] += 2. * H;
            transform.position[1] += if step < 45 { 1. } else { -1. } * H;
        }
        ticks(&mut runtime, 1);
    }
    let platform = position(&runtime, "lift");
    let hero_position = position(&runtime, "hero");
    assert!(
        (hero_position.x - platform.x).abs() < 0.06,
        "{hero_position} vs {platform}"
    );
    assert!((hero_position.y - (platform.y + 0.25 + 0.8)).abs() < 0.06);
    let state = physics::character_state(runtime.app().world(), hero(&runtime)).unwrap();
    assert!(state.grounded);
    assert_eq!(state.ground_entity, Some(lift));
}

// ---- Critic fixes: teleporting and per-frame visuals ----------------------------------------

const FRAME: f32 = 1. / 144.;

/// M1: writing the `Transform` of a character moves it there; the controller used to overwrite
/// the kinematic target with its own position and discard the write.
#[test]
fn writing_the_transform_teleports_a_character_and_resets_its_ground_state() {
    let (_dir, mut runtime) = started(vec![
        floor3(),
        hero3([0., 0.85, 0.], json!({"desired_velocity":[2.,0.,0.]})),
    ]);
    ticks(&mut runtime, 60);
    let before = position(&runtime, "hero");
    assert!((before.x - 2.).abs() < 0.1 && before.y < 1., "{before}");
    assert!(
        physics::character_state(runtime.app().world(), hero(&runtime))
            .unwrap()
            .grounded
    );
    let hero_entity = hero(&runtime);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(hero_entity)
        .unwrap()
        .position = [20., 3., 0.];
    ticks(&mut runtime, 1);
    let after = position(&runtime, "hero");
    println!("teleported to {after} one tick after writing (20, 3, 0)");
    assert!((after.x - (20. + 2. * H)).abs() < 0.05, "{after}");
    assert!(after.y > 2.9 && after.y < 3.05, "{after}");
    let state = physics::character_state(runtime.app().world(), hero_entity).unwrap();
    assert!(!state.grounded && state.ground_entity.is_none());
    // It falls from the new place, lands on the floor, and keeps walking.
    ticks(&mut runtime, 120);
    let end = position(&runtime, "hero");
    assert!((end.x - (20. + 2. * 121. * H)).abs() < 0.1, "{end}");
    assert!((end.y - 0.85).abs() < 0.06, "{end}");
    assert!(
        physics::character_state(runtime.app().world(), hero_entity)
            .unwrap()
            .grounded
    );
}

fn walking_with(settings_data: Value) -> (tempfile::TempDir, Runtime) {
    started(vec![
        settings(settings_data),
        floor3(),
        hero3([0., 0.85, 0.], json!({"desired_velocity":[3.,0.,0.]})),
    ])
}
fn frame_steps(runtime: &mut Runtime, frames: usize) -> Vec<f32> {
    let mut xs = vec![];
    for _ in 0..frames {
        runtime.tick(FRAME, InputSnapshot::default()).unwrap();
        xs.push(position(runtime, "hero").x);
    }
    xs.windows(2).map(|w| w[1] - w[0]).collect()
}

/// H2/D1: with interpolation on, a character's `Transform` moves smoothly between the fixed
/// steps (144 Hz frames, 60 Hz steps); without it, it holds the latest step's pose.
#[test]
fn characters_interpolate_their_transform_when_interpolation_is_on() {
    let (_dir, mut smooth) =
        walking_with(json!({"fixed_timestep":H,"interpolation":"interpolate"}));
    ticks(&mut smooth, 20);
    let deltas = frame_steps(&mut smooth, 120);
    let expected = 3. * FRAME;
    let (lo, hi) = deltas
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    println!("interpolated: per-frame step {lo:.4}..{hi:.4} m, expected {expected:.4} m");
    assert!(lo > 0.8 * expected && hi < 1.2 * expected, "{lo}..{hi}");

    let (_dir, mut held) = walking_with(json!({"fixed_timestep":H}));
    ticks(&mut held, 20);
    let deltas = frame_steps(&mut held, 120);
    let idle = deltas.iter().filter(|d| d.abs() < 1e-6).count();
    println!("not interpolated: {idle} of 120 frames hold still");
    assert!(idle > 40, "{idle}");
}
