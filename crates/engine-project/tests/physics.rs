use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics;
use engine_project::*;
use glam::{Vec2, Vec3};
use serde_json::{Value, json};
use std::fs;

fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn body2(data: Value) -> Value {
    json!({"engine.PhysicsBody2D":value(data)})
}
fn body3(data: Value) -> Value {
    json!({"engine.PhysicsBody3D":value(data)})
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
            &json!({"schema":1,"name":"physics-test","startup_scene":"scene.json","assets":[]}),
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
    runtime.start().unwrap();
    (dir, runtime)
}
/// Error message from whichever stage (load, validation, start) rejects the scene.
fn rejection(entities: Vec<Value>) -> String {
    let dir = fixture(entities);
    let result = Project::load(dir.path().join("project.json"))
        .and_then(Runtime::new)
        .and_then(|mut runtime| {
            runtime.start()?;
            runtime.tick(0.02, InputSnapshot::default())
        });
    result.expect_err("scene must be rejected").to_string()
}
fn ticks(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.tick(0.02, InputSnapshot::default()).unwrap();
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
#[derive(Resource, Default)]
struct Contacts(Vec<String>);
fn record(world: &mut World, target: Entity, event: &EngineEvent) -> Result<()> {
    let id = world.get::<AuthoredId>(target).unwrap().id.clone();
    let other = event.payload["other"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    world
        .resource_mut::<Contacts>()
        .0
        .push(format!("{id}>{other}"));
    Ok(())
}

#[test]
fn settings_override_gravity_and_solver_iterations() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({"gravity_2d":[0.,-1.],"gravity_3d":[0.,-2.,0.],"solver_iterations":8})),
        entity("two", [0., 10., 0.], body2(json!({}))),
        entity("three", [0., 10., 0.], body3(json!({}))),
    ]);
    ticks(&mut runtime, 50);
    // One second of semi-implicit Euler at 0.01 s substeps: g * 0.0001 * 5050.
    let drop2 = 10. - position(&runtime, "two").y;
    let drop3 = 10. - position(&runtime, "three").y;
    assert!((drop2 - 0.505).abs() < 0.02, "{drop2}");
    assert!((drop3 - 1.01).abs() < 0.03, "{drop3}");
    let (two, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert_eq!(two.gravity(), [0., -1.]);
    assert_eq!(three.solver_iterations(), 8);
}

#[test]
fn named_layers_and_material_presets_change_contacts_and_sliding() {
    let layered = |id: &str, x: f32, layer: &str| {
        let mut e = entity(
            id,
            [x, 0., 0.],
            body2(json!({"shape":"ball","radius":0.5,"material":{"layer_name":layer}})),
        );
        e["components"]["EventBindings"] =
            json!({"bindings":[{"event":"physics.contact.start","handler":"record"}]});
        e
    };
    let slider = |id: &str, z: f32, material: Value| {
        entity(
            id,
            [0., 0.5, z],
            body3(json!({"velocity":[4.,0.,0.],"material":material})),
        )
    };
    let dir = fixture(vec![
        settings(json!({
            "gravity_2d":[0.,0.],
            "layers":["player","debris"],
            "disabled_pairs":[["player","debris"]],
            "materials":[{"name":"ice","friction":0.,"friction_combine":"min"}]
        })),
        layered("player", 0., "player"),
        layered("debris", 0.5, "debris"),
        layered("player2", 10., "player"),
        layered("player3", 10.5, "player"),
        entity(
            "ground",
            [0., -0.5, 0.],
            body3(
                json!({"motion":"fixed","half_extents":[50.,0.5,50.],"material":{"friction":1.}}),
            ),
        ),
        slider("icy", 0., json!({"preset":"ice"})),
        slider("plain", 5., json!({})),
    ]);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Contacts::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    ticks(&mut runtime, 50);
    let mut contacts = runtime.app().world().resource::<Contacts>().0.clone();
    contacts.sort();
    assert_eq!(contacts, ["player2>player3", "player3>player2"]);
    // Disabled pairs keep overlapping; enabled same-layer pairs separate.
    assert!((position(&runtime, "debris").x - 0.5).abs() < 1e-3);
    assert!(position(&runtime, "player3").x - position(&runtime, "player2").x > 0.9);
    // Preset ice (friction 0, min) slides 4 m/s for 1 s; plain (avg 0.75) stops near
    // v^2 / (2 mu g) = 1.09 m.
    let icy = position(&runtime, "icy").x;
    let plain = position(&runtime, "plain").x;
    assert!((icy - 4.).abs() < 0.1, "{icy}");
    assert!(plain > 0.5 && plain < 1.3, "{plain}");
    let mask = physics::layer_mask(runtime.app().world(), &["debris"]).unwrap();
    assert_eq!(mask, 0b10);
    assert!(physics::layer_mask(runtime.app().world(), &["ghost"]).is_err());
}

#[test]
fn invalid_or_duplicate_settings_and_unknown_names_are_rejected() {
    let ball = |material: Value| {
        entity(
            "ball",
            [0.; 3],
            body3(json!({"shape":"ball","material":material})),
        )
    };
    let mut second = settings(json!({}));
    second["id"] = json!("settings2");
    let duplicate = rejection(vec![settings(json!({})), second]);
    assert!(
        duplicate.contains("At most one engine.PhysicsSettings"),
        "{duplicate}"
    );
    let layer = rejection(vec![
        settings(json!({"layers":["a"]})),
        ball(json!({"layer_name":"ghost"})),
    ]);
    assert!(layer.contains("Unknown physics layer ghost"), "{layer}");
    let preset = rejection(vec![ball(json!({"preset":"rubber"}))]);
    assert!(
        preset.contains("Unknown physics material preset rubber"),
        "{preset}"
    );
    let pair = rejection(vec![settings(
        json!({"layers":["a"],"disabled_pairs":[["a","b"]]}),
    )]);
    assert!(pair.contains("unknown layer pair"), "{pair}");
    let names: Vec<_> = (0..33).map(|i| format!("l{i}")).collect();
    rejection(vec![settings(json!({ "layers": names }))]);
    let repeated = rejection(vec![settings(json!({"layers":["a","a"]}))]);
    assert!(repeated.contains("duplicate collision layer"), "{repeated}");
    let materials = rejection(vec![settings(
        json!({"materials":[{"name":"m"},{"name":"m"}]}),
    )]);
    assert!(
        materials.contains("Duplicate physics material m"),
        "{materials}"
    );
    rejection(vec![settings(json!({"solver_iterations":0}))]);
    let dynamic_mesh = rejection(vec![entity(
        "mesh",
        [0.; 3],
        body3(
            json!({"shape":"trimesh","vertices":[[0.,0.,0.],[1.,0.,0.],[0.,0.,1.]],"indices":[[0,2,1]]}),
        ),
    )]);
    assert!(
        dynamic_mesh.contains("fixed or kinematic"),
        "{dynamic_mesh}"
    );
    // The 2D schema has no `cylinder` shape.
    let flat_cylinder = rejection(vec![entity(
        "cylinder",
        [0.; 3],
        body2(json!({"shape":"cylinder"})),
    )]);
    assert!(flat_cylinder.contains("not in enum"), "{flat_cylinder}");
    let disabled = fixture(vec![settings(json!({}))]);
    let project = Project::load(disabled.path().join("project.json")).unwrap();
    assert!(
        Runtime::new_with_simulation(project, None)
            .unwrap()
            .start()
            .is_err()
    );
}

#[test]
fn authored_shapes_rest_on_heightfield_trimesh_and_by_their_extent() {
    let ball = json!({"shape":"ball","radius":0.5});
    let (_dir, mut runtime) = started(vec![
        entity(
            "terrain",
            [0.; 3],
            body3(
                json!({"motion":"fixed","shape":"heightfield","heights":[0.25,0.25,0.25,0.25,0.25,0.25,0.25,0.25,0.25],
                "heightfield_rows":3,"heightfield_columns":3,"heightfield_scale":[8.,1.,8.]}),
            ),
        ),
        entity("on_terrain", [0.2, 3., 0.1], body3(ball.clone())),
        entity(
            "mesh",
            [20., 0., 0.],
            body3(json!({"motion":"fixed","shape":"trimesh",
                "vertices":[[-4.,0.,-4.],[4.,0.,-4.],[4.,0.,4.],[-4.,0.,4.]],
                "indices":[[0,2,1],[0,3,2]]})),
        ),
        entity("on_mesh", [20.2, 3., 0.1], body3(ball)),
        entity(
            "floor",
            [0., -0.5, 20.],
            body3(json!({"motion":"fixed","half_extents":[30.,0.5,5.]})),
        ),
        entity(
            "cylinder",
            [0., 2., 20.],
            body3(json!({"shape":"cylinder","radius":0.25,"half_height":0.75})),
        ),
        // Scaled 2x: the unit hull becomes a 1 m cube.
        {
            let mut hull = entity(
                "hull",
                [5., 2., 20.],
                body3(json!({"shape":"convex_hull","points":[
                    [-0.25,-0.25,-0.25],[0.25,-0.25,-0.25],[0.25,0.25,-0.25],[-0.25,0.25,-0.25],
                    [-0.25,-0.25,0.25],[0.25,-0.25,0.25],[0.25,0.25,0.25],[-0.25,0.25,0.25]]})),
            );
            hull["components"]["Transform"]["scale"] = json!([2., 2., 2.]);
            hull
        },
        entity(
            "compound",
            [10., 2., 20.],
            body3(json!({"shape":"compound","parts":[
                {"shape":"ball","radius":0.25,"position":[-1.,0.,0.]},
                {"shape":"cylinder","radius":0.25,"half_height":0.25,"position":[1.,0.,0.]}]})),
        ),
        entity(
            "ground2d",
            [0., -0.5, 0.],
            body2(json!({"motion":"fixed","half_extents":[30.,0.5]})),
        ),
        entity(
            "hull2d",
            [0., 2., 0.],
            body2(json!({"shape":"convex_hull","points":[[-0.5,-0.5],[0.5,-0.5],[0.,0.5]]})),
        ),
        entity(
            "heightfield2d",
            [20., 0., 0.],
            body2(
                json!({"motion":"fixed","shape":"heightfield","heights":[1.,1.,1.],"heightfield_scale":[6.,1.]}),
            ),
        ),
        entity(
            "on_heightfield2d",
            [20.3, 3., 0.],
            body2(json!({"shape":"ball","radius":0.5})),
        ),
    ]);
    ticks(&mut runtime, 150);
    let expect = |id: &str, y: f32| {
        let actual = position(&runtime, id).y;
        assert!((actual - y).abs() < 0.05, "{id}: {actual} vs {y}");
    };
    expect("on_terrain", 0.75);
    expect("on_mesh", 0.5);
    expect("cylinder", 0.75);
    expect("hull", 0.5);
    expect("compound", 0.25);
    // The triangle's base is 0.5 m below its body origin.
    expect("hull2d", 0.5);
    expect("on_heightfield2d", 1.5);
}

#[test]
fn native_api_reads_state_applies_impulses_and_raycasts_to_entities() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({"gravity_2d":[0.,0.],"gravity_3d":[0.,0.,0.],"layers":["world","props"]})),
        entity(
            "wall",
            [5., 0., 0.],
            body3(
                json!({"motion":"fixed","half_extents":[0.5,2.,2.],"material":{"layer_name":"world"}}),
            ),
        ),
        entity(
            "crate",
            [0.; 3],
            body3(json!({"material":{"density":2.,"layer_name":"props"}})),
        ),
        entity(
            "disc",
            [0., 0., 3.],
            body2(json!({"shape":"ball","radius":0.5})),
        ),
    ]);
    let crate_entity = runtime.entity("crate").unwrap();
    let disc = runtime.entity("disc").unwrap();
    let world = runtime.app_mut().world_mut();
    // A 1 m cube of density 2 weighs 2 kg: a 4 N*s impulse gives 2 m/s.
    physics::apply_impulse(world, crate_entity, Vec3::new(4., 0., 0.)).unwrap();
    physics::apply_angular_impulse(world, disc, Vec3::new(0., 0., 0.5)).unwrap();
    physics::apply_impulse(world, disc, Vec3::new(0., 1., 0.)).unwrap();
    assert!(physics::apply_impulse(world, disc, Vec3::new(0., 0., 1.)).is_err());
    assert!(physics::apply_angular_impulse(world, disc, Vec3::X).is_err());
    let wall = runtime.entity("wall").unwrap();
    let world = runtime.app_mut().world_mut();
    assert!(physics::apply_impulse(world, wall, Vec3::X).is_err());
    ticks(&mut runtime, 50);
    let world = runtime.app().world();
    let state = physics::body_state(world, crate_entity).unwrap();
    assert_eq!(state.dimension, physics::Dimension::Three);
    assert!((state.linear_velocity.x - 2.).abs() < 0.01, "{state:?}");
    // 1 s at 2 m/s.
    assert!((state.position.x - 2.).abs() < 0.03, "{state:?}");
    let disc_state = physics::body_state(world, disc).unwrap();
    assert_eq!(disc_state.dimension, physics::Dimension::Two);
    assert!((disc_state.position.z - 3.).abs() < 1e-5);
    assert!(disc_state.angular_velocity.z > 0. && disc_state.linear_velocity.y > 0.);
    let hit = physics::raycast_3d(world, Vec3::new(10., 0., 0.), Vec3::NEG_X, 20., u32::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(hit.entity, wall);
    assert!((hit.distance - 4.5).abs() < 1e-3);
    let props = physics::layer_mask(world, &["props"]).unwrap();
    let hit = physics::raycast_3d(world, Vec3::new(10., 0., 0.), Vec3::NEG_X, 20., props)
        .unwrap()
        .unwrap();
    assert_eq!(hit.entity, crate_entity);
    let hit = physics::raycast_2d(
        world,
        Vec2::new(-5., disc_state.position.y),
        Vec2::X,
        20.,
        u32::MAX,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hit.entity, disc);
    assert_eq!(physics::body_entity(physics::body_id(disc)), Some(disc));
    let ghost = runtime.app_mut().world_mut().spawn_empty().id();
    assert!(physics::body_state(runtime.app().world(), ghost).is_err());
}

// ---- Critic fixes: rebuilds keep live state, kinematic targets are spread over steps -------

const STEP: f32 = 1. / 60.;

fn frames(runtime: &mut Runtime, n: usize, dt: f32) {
    for _ in 0..n {
        runtime.tick(dt, InputSnapshot::default()).unwrap();
    }
}

/// M4: editing a dynamic body's configuration rebuilds it at its current physics pose and with
/// its current velocity; the authored `velocity` applies only when the body is created (or
/// when that field itself is edited). With interpolation on, the `Transform` holds a blended
/// pose, which must not leak into the rebuilt body.
#[test]
fn rebuilding_a_dynamic_body_keeps_its_live_pose_and_velocity() {
    let (_dir, mut runtime) = started(vec![
        settings(
            json!({"fixed_timestep":STEP,"interpolation":"interpolate","gravity_3d":[0.,-10.,0.]}),
        ),
        entity(
            "ball",
            [0., 100., 0.],
            body3(json!({"shape":"ball","radius":0.5})),
        ),
    ]);
    let ball = runtime.entity("ball").unwrap();
    frames(&mut runtime, 100, 1. / 144.);
    let before = physics::body_state(runtime.app().world(), ball).unwrap();
    let blended = position(&runtime, "ball");
    println!(
        "live pose y {} velocity {}; blended Transform y {}",
        before.position.y, before.linear_velocity.y, blended.y
    );
    assert!(
        before.linear_velocity.y < -6.,
        "{:?}",
        before.linear_velocity
    );
    assert!(
        (blended.y - before.position.y).abs() > 1e-3,
        "blend is visible"
    );
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<physics::PhysicsBody3D>(ball)
        .unwrap()
        .material
        .friction = 0.9;
    // A frame too short to run a step: the rebuilt body must still be where it was.
    let accumulator = physics::step_status(runtime.app().world())
        .unwrap()
        .accumulator;
    assert!(accumulator < STEP - 0.001, "{accumulator}");
    frames(&mut runtime, 1, 1e-4);
    assert_eq!(
        physics::step_status(runtime.app().world()).unwrap().steps,
        0
    );
    let after = physics::body_state(runtime.app().world(), ball).unwrap();
    assert!(
        (after.position - before.position).length() < 1e-4,
        "pose {:?} vs {:?}",
        after.position,
        before.position
    );
    assert!(
        (after.linear_velocity - before.linear_velocity).length() < 1e-3,
        "velocity {:?} vs {:?}",
        after.linear_velocity,
        before.linear_velocity
    );
    // Editing the authored velocity itself is an explicit request and does apply.
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<physics::PhysicsBody3D>(ball)
        .unwrap()
        .velocity = [3., 0., 0.];
    frames(&mut runtime, 1, 1e-4);
    let set = physics::body_state(runtime.app().world(), ball).unwrap();
    assert!((set.linear_velocity - Vec3::new(3., 0., 0.)).length() < 1e-3);
}

/// M5: a kinematic body driven through its `Transform` once per frame moves at the driven
/// speed in every fixed step of the frame, not in one jump followed by standing still.
#[test]
fn kinematic_targets_are_spread_over_the_fixed_steps_of_a_frame() {
    let (_dir, mut runtime) = started(vec![
        settings(json!({"fixed_timestep":STEP,"gravity_3d":[0.,0.,0.]})),
        entity(
            "platform",
            [0.; 3],
            body3(json!({"motion":"kinematic","half_extents":[1.,0.1,1.]})),
        ),
    ]);
    let platform = runtime.entity("platform").unwrap();
    let mut x = 0.;
    let mut speeds = vec![];
    for _ in 0..20 {
        x += 3. / 30.;
        runtime
            .app_mut()
            .world_mut()
            .get_mut::<Transform>(platform)
            .unwrap()
            .position[0] = x;
        runtime.tick(1. / 30., InputSnapshot::default()).unwrap();
        assert_eq!(
            physics::step_status(runtime.app().world()).unwrap().steps,
            2
        );
        let state = physics::body_state(runtime.app().world(), platform).unwrap();
        assert!(
            (state.position.x - x).abs() < 1e-4,
            "{} vs {x}",
            state.position.x
        );
        speeds.push(state.linear_velocity.x);
    }
    println!(
        "kinematic speed in the last step of each frame: {:?}",
        &speeds[..4]
    );
    for speed in &speeds[2..] {
        assert!((speed - 3.).abs() < 0.05, "{speeds:?}");
    }
}
