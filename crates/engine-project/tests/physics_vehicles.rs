//! Authored raycast vehicles: component_set editing, direct and action-mapped controls,
//! events, wheel visuals, validation, native API and fixed-step determinism.
use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{self, Vehicle3D};
use engine_project::*;
use glam::Vec3;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;

const H: f32 = 1. / 60.;
/// Chassis box 0.9 x 0.3 x 2.0 half extents at 1500 kg.
const DENSITY: f32 = 1500. / 4.32;

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn floor() -> Value {
    entity(
        "floor",
        [0., -0.5, 0.],
        json!({"engine.PhysicsBody3D":value(json!({"motion":"fixed","half_extents":[100.,0.5,5000.]}))}),
    )
}
fn car(vehicle: Value) -> Value {
    car_at([0., 0.6, 0.], vehicle)
}
fn car_at(position: [f32; 3], vehicle: Value) -> Value {
    let mut car = entity(
        "car",
        position,
        json!({
            "engine.PhysicsBody3D":value(json!({
                "half_extents":[0.9,0.3,2.0],
                "material":{"density":DENSITY},
            })),
            "engine.Vehicle3D":value(vehicle),
        }),
    );
    car["components"]["EventBindings"] = json!({"bindings":[
        {"event":"physics.vehicles.gear_changed","handler":"record"},
        {"event":"physics.vehicles.wheel_contact","handler":"record"},
        {"event":"physics.vehicles.airborne","handler":"record"},
        {"event":"physics.vehicles.landed","handler":"record"}]});
    car
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
            &json!({"schema":1,"name":"vehicle-test","startup_scene":"scene.json","assets":[]}),
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
            runtime
                .app_mut()
                .world_mut()
                .insert_resource(Events::default());
            register_event_handler(runtime.app_mut(), "record", record)?;
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
fn held_ticks(runtime: &mut Runtime, n: usize, actions: &[&str]) {
    let frame = InputSnapshot {
        held: actions
            .iter()
            .map(|a| (*a).to_owned())
            .collect::<BTreeSet<_>>(),
        ..Default::default()
    };
    for _ in 0..n {
        runtime.tick(H, frame.clone()).unwrap();
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
fn chassis(runtime: &Runtime) -> Entity {
    runtime.entity("car").unwrap()
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
fn component_set_authored_controls_drive_the_vehicle_forward() {
    let dir = fixture(vec![floor(), car(json!({}))]);
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |data: Value| EditOperation::SetComponent {
        entity_id: "car".into(),
        component: "engine.Vehicle3D".into(),
        value: value(data),
        merge: true,
    };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(set(json!({"throttle":1.0,"steer":0.0})), &revision)
        .unwrap();
    for bad in [
        json!({"throttle":1.5}),
        json!({"steer":-2.0}),
        json!({"unknown":1}),
        json!({"tyre":{"friction":0.0}}),
        json!({"gearbox":{"mode":"sequential"}}),
        json!({"differential":{"kind":"spool"}}),
        json!({"wheels":[{"radius":-1.0},{}]}),
    ] {
        assert!(
            document.apply(set(bad.clone()), &revision).is_err(),
            "{bad}"
        );
    }
    document.save(&revision).unwrap();

    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Events::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    ticks(&mut runtime, 60 * 4);
    let p = position(&runtime, "car");
    assert!(p.z > 10., "authored throttle applied: {p}");
}

#[test]
fn authored_throttle_accelerates_the_car_and_shifts_gears() {
    let dir = fixture(vec![floor(), car(json!({"throttle":1.0}))]);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let descriptor = &project.component_catalog.0["engine.Vehicle3D"];
    assert_eq!(descriptor.schema["properties"]["wheels"]["minItems"], 2);
    assert!(
        descriptor
            .requires
            .iter()
            .any(|r| r == "engine.PhysicsBody3D")
    );
    assert_eq!(descriptor.entity_references.len(), 16);
    let mut runtime = Runtime::new(project).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Events::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    ticks(&mut runtime, 60 * 8);
    let p = position(&runtime, "car");
    assert!(p.z > 20., "drove forward: z={}", p.z);
    assert!(p.x.abs() < 0.5 && (p.y - 0.535).abs() < 0.08, "{p}");
    let state = physics::vehicle_state(runtime.app().world(), chassis(&runtime)).unwrap();
    assert!(state.speed > 10. && state.gear >= 2, "{state:?}");
    assert_eq!(state.wheels_in_contact, 4);
    assert_eq!(state.wheels.len(), 4);
    let gears = events(&runtime, "physics.vehicles.gear_changed");
    assert!(gears.len() >= 2, "{gears:?}");
    assert_eq!(gears[0]["from"], 0);
    assert_eq!(gears[0]["to"], 1);
    let contacts = events(&runtime, "physics.vehicles.wheel_contact");
    assert!(contacts.len() >= 4);
    assert!(
        contacts
            .iter()
            .any(|c| c["phase"] == "start" && c["ground"] == "floor")
    );
    // Wheel poses are readable: centres sit one radius above the road.
    for wheel in &state.wheels {
        assert!(wheel.in_contact);
        assert!((wheel.position.y - wheel.contact_point.y - 0.34).abs() < 0.03);
        assert_eq!(wheel.ground_entity, runtime.entity("floor"));
    }
}

#[test]
fn gameplay_actions_drive_throttle_steering_and_brake() {
    let (_dir, mut runtime) = started(vec![floor(), car(json!({"steering":{"max_angle":0.4}}))]);
    ticks(&mut runtime, 30);
    held_ticks(&mut runtime, 180, &["throttle"]);
    let straight = position(&runtime, "car");
    assert!(straight.z > 8., "throttle action moves the car: {straight}");
    assert!(straight.x.abs() < 0.3);
    let speed = physics::vehicle_state(runtime.app().world(), chassis(&runtime))
        .unwrap()
        .speed;
    // steer_right turns right (toward -X when facing +Z).
    held_ticks(&mut runtime, 120, &["throttle", "steer_right"]);
    let turned = position(&runtime, "car");
    assert!(turned.x < straight.x - 2., "{straight} -> {turned}");
    // Holding the brake slows the car to a stop (before the gearbox would select reverse).
    for _ in 0..240 {
        held_ticks(&mut runtime, 1, &["brake"]);
        let speed = physics::vehicle_state(runtime.app().world(), chassis(&runtime))
            .unwrap()
            .speed;
        if speed < 1. {
            break;
        }
    }
    let slowed = physics::vehicle_state(runtime.app().world(), chassis(&runtime))
        .unwrap()
        .speed;
    assert!(slowed < 1. && speed > 8., "{speed} -> {slowed}");
    // An unbound action name does nothing.
    let before = position(&runtime, "car");
    held_ticks(&mut runtime, 30, &["unrelated"]);
    assert!((position(&runtime, "car") - before).length() < 1.0);
}

#[test]
fn custom_action_names_and_analog_values_are_honoured() {
    let (_dir, mut runtime) = started(vec![
        floor(),
        car(json!({"input":{"throttle":"gas","steer_left":"wheel_left","steer_right":""}})),
    ]);
    ticks(&mut runtime, 20);
    let mut frame = InputSnapshot::default();
    frame.held.insert("gas".into());
    frame.held.insert("wheel_left".into());
    frame.values.insert("wheel_left".into(), 0.6);
    for _ in 0..180 {
        runtime.tick(H, frame.clone()).unwrap();
    }
    let p = position(&runtime, "car");
    assert!(
        p.z > 5. && p.x > 0.5,
        "analog steer_left turns left (+X): {p}"
    );
    // The default action name no longer applies.
    held_ticks(&mut runtime, 120, &[]);
    let before = position(&runtime, "car");
    held_ticks(&mut runtime, 60, &["throttle"]);
    let after = position(&runtime, "car");
    assert!(
        (after - before).length() < 8.0,
        "coasting only: {before} -> {after}"
    );
}

#[test]
fn manual_gearbox_native_shifting_and_runtime_component_edits() {
    let (_dir, mut runtime) = started(vec![
        floor(),
        car(json!({"gearbox":{"mode":"manual"},"throttle":1.0})),
    ]);
    let e = chassis(&runtime);
    ticks(&mut runtime, 60);
    let state = physics::vehicle_state(runtime.app().world(), e).unwrap();
    assert_eq!(state.gear, 0);
    assert!(state.speed.abs() < 0.2);
    physics::vehicle_shift_up(runtime.app_mut().world_mut(), e).unwrap();
    ticks(&mut runtime, 120);
    let state = physics::vehicle_state(runtime.app().world(), e).unwrap();
    assert_eq!(state.gear, 1);
    assert!(state.speed > 3.);
    physics::vehicle_set_gear(runtime.app_mut().world_mut(), e, 2).unwrap();
    ticks(&mut runtime, 5);
    assert_eq!(
        physics::vehicle_state(runtime.app().world(), e)
            .unwrap()
            .gear,
        2
    );
    assert!(physics::vehicle_set_gear(runtime.app_mut().world_mut(), e, 12).is_err());
    // Runtime edits apply at the next sync: cut the throttle and brake hard.
    physics::set_vehicle_controls(runtime.app_mut().world_mut(), e, 0., 1., 0., 0.).unwrap();
    ticks(&mut runtime, 240);
    let state = physics::vehicle_state(runtime.app().world(), e).unwrap();
    assert!(state.speed.abs() < 0.5, "{state:?}");
    assert!(
        physics::set_vehicle_controls(runtime.app_mut().world_mut(), e, 2., 0., 0., 0.).is_err()
    );
    // Editing a wheel's tyre settings through the component rebuilds the model in place.
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Vehicle3D>(e)
        .unwrap()
        .tyre
        .friction = 0.4;
    ticks(&mut runtime, 5);
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert!((three.vehicle_desc(e.to_bits()).unwrap().tyre.friction - 0.4).abs() < 1e-6);
}

#[test]
fn wheel_visual_entities_follow_the_simulated_wheels() {
    let mut visual = entity("wheel_fl", [0., 0., 0.], json!({}));
    visual["parent"] = json!("car");
    let mut rear = entity("wheel_rr", [0., 0., 0.], json!({}));
    rear["parent"] = json!("car");
    let mut wheels = Vec::new();
    for (i, (x, z)) in [(-0.8, 1.3), (0.8, 1.3), (-0.8, -1.3), (0.8, -1.3)]
        .into_iter()
        .enumerate()
    {
        let mut wheel = json!({"mount":[x, 0., z],"steer":i < 2,"drive":i >= 2});
        if i == 0 {
            wheel["entity"] = json!("wheel_fl");
        }
        if i == 3 {
            wheel["entity"] = json!("wheel_rr");
        }
        wheels.push(wheel);
    }
    let (_dir, mut runtime) = started(vec![
        floor(),
        car(json!({"wheels":wheels,"throttle":0.4,"steer":1.0,"steering":{"max_angle":0.3}})),
        visual,
        rear,
    ]);
    ticks(&mut runtime, 120);
    let car = chassis(&runtime);
    let readout = physics::vehicle_wheels(runtime.app().world(), car).unwrap();
    for (id, index) in [("wheel_fl", 0usize), ("wheel_rr", 3)] {
        let world = world_transform(runtime.app().world(), runtime.entity(id).unwrap()).unwrap();
        let at = Vec3::from(world.position());
        let (expected, _) = physics::vehicle_wheel_pose(runtime.app().world(), car, index).unwrap();
        assert!((at - expected).length() < 0.06, "{id}: {at} vs {expected}");
    }
    // The steered front wheel's visual is turned about up; the rear one is not.
    let front =
        world_transform(runtime.app().world(), runtime.entity("wheel_fl").unwrap()).unwrap();
    let matrix = glam::Mat4::from_cols_array_2d(&front.matrix);
    let (_, rotation, _) = matrix.to_scale_rotation_translation();
    let chassis_rotation = {
        let m = world_transform(runtime.app().world(), car).unwrap();
        glam::Mat4::from_cols_array_2d(&m.matrix)
            .to_scale_rotation_translation()
            .1
    };
    let relative = chassis_rotation.inverse() * rotation;
    let (axis, angle) = relative.to_axis_angle();
    assert!(angle.abs() > 0.05, "wheel turned: {axis} {angle}");
    assert!(readout[0].steer_angle.abs() > 0.2);
    assert_eq!(readout[2].steer_angle, 0.);
}

#[test]
fn vehicles_need_a_dynamic_body_valid_wheels_and_known_wheel_entities() {
    let kinematic = rejection(vec![
        floor(),
        entity(
            "car",
            [0., 1., 0.],
            json!({
                "engine.PhysicsBody3D":value(json!({"motion":"kinematic"})),
                "engine.Vehicle3D":value(json!({})),
            }),
        ),
    ]);
    assert!(kinematic.contains("dynamic"), "{kinematic}");
    let one_wheel = rejection(vec![floor(), car(json!({"wheels":[{"mount":[0.,0.,0.]}]}))]);
    assert!(one_wheel.contains("outside bounds"), "{one_wheel}");
    let unknown = rejection(vec![
        floor(),
        car(json!({"wheels":[{"mount":[-1.,0.,1.],"entity":"missing"},{"mount":[1.,0.,1.]}]})),
    ]);
    assert!(unknown.contains("missing"), "{unknown}");
    let bad_axle = rejection(vec![
        floor(),
        car(json!({"wheels":[{"axle":[-1.,0.,0.]},{"mount":[1.,0.,0.]}]})),
    ]);
    assert!(bad_axle.contains("forward"), "{bad_axle}");
    let bad_bar = rejection(vec![
        floor(),
        car(json!({"anti_roll":[{"wheel_a":0,"wheel_b":9,"stiffness":1.0}]})),
    ]);
    assert!(bad_bar.contains("anti-roll"), "{bad_bar}");
}

fn run_script(frames: &[f32]) -> Vec<[u32; 3]> {
    let h = 1.0f32 / 60.0;
    let (_dir, mut runtime) = started(vec![
        settings(json!({"fixed_timestep":h,"max_substeps":64})),
        floor(),
        car_at(
            [0., 0.9, 0.],
            json!({"differential":{"kind":"limited_slip"}}),
        ),
    ]);
    let e = chassis(&runtime);
    for (i, dt) in frames.iter().enumerate() {
        let steer = ((i as f32) * 0.05).sin();
        physics::set_vehicle_controls(runtime.app_mut().world_mut(), e, 0.8, 0., steer, 0.)
            .unwrap();
        runtime.tick(*dt, InputSnapshot::default()).unwrap();
    }
    let wheel = physics::vehicle_wheels(runtime.app().world(), e).unwrap()[2];
    vec![
        position(&runtime, "car").to_array().map(f32::to_bits),
        [
            wheel.angular_velocity.to_bits(),
            wheel.suspension_force.to_bits(),
            wheel.slip_ratio.to_bits(),
        ],
    ]
}

#[test]
fn fixed_step_vehicle_runs_are_bitwise_identical() {
    let h = 1.0f32 / 60.0;
    let reference = run_script(&vec![h; 240]);
    assert_eq!(reference, run_script(&vec![h; 240]), "identical runs");
    assert_ne!(reference[0], [0., 0.9, 0.].map(f32::to_bits));
    let uneven: Vec<f32> = (0..120).flat_map(|_| [h * 1.5, h * 0.5]).collect();
    assert_eq!(uneven.len(), 240);
    assert_eq!(run_script(&uneven), reference, "frame splits do not matter");
}

#[test]
fn removing_the_component_returns_the_chassis_to_plain_physics() {
    let (_dir, mut runtime) = started(vec![floor(), car(json!({"throttle":1.0}))]);
    ticks(&mut runtime, 30);
    let e = chassis(&runtime);
    assert!(physics::vehicle_state(runtime.app().world(), e).is_ok());
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(e)
        .remove::<Vehicle3D>();
    ticks(&mut runtime, 2);
    assert!(physics::vehicle_state(runtime.app().world(), e).is_err());
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert!(!three.is_vehicle(e.to_bits()));
    // Without suspension the box simply falls onto the floor.
    ticks(&mut runtime, 120);
    assert!(position(&runtime, "car").y < 0.4);
}

// ---- Critic fixes: wheel visuals on frames without a physics step ---------------------------

/// H2/D1: wheel visuals that are not children of the chassis follow the chassis pose on every
/// frame, including frames that run no physics step (144 Hz frames, 60 Hz steps).
#[test]
fn wheel_visuals_stay_with_an_interpolated_chassis_on_frames_without_a_step() {
    let mut wheels = Vec::new();
    let mut visuals = Vec::new();
    for (i, (x, z)) in [(-0.8, 1.3), (0.8, 1.3), (-0.8, -1.3), (0.8, -1.3)]
        .into_iter()
        .enumerate()
    {
        let id = format!("wheel_{i}");
        wheels.push(json!({"mount":[x, 0., z],"steer":i < 2,"drive":i >= 2,"entity":id}));
        visuals.push(entity(&id, [0., 0., 0.], json!({})));
    }
    let mut entities = vec![
        settings(json!({"fixed_timestep":H,"interpolation":"interpolate"})),
        floor(),
        car(json!({"wheels":wheels,"throttle":1.0})),
    ];
    entities.extend(visuals);
    let (_dir, mut runtime) = started(entities);
    let car = chassis(&runtime);
    let mut idle = 0;
    let mut worst = 0f32;
    for frame in 0..240 {
        runtime.tick(1. / 144., InputSnapshot::default()).unwrap();
        if frame < 90 {
            continue;
        }
        idle += usize::from(physics::step_status(runtime.app().world()).unwrap().steps == 0);
        let world = runtime.app().world();
        let chassis_matrix =
            glam::Mat4::from_cols_array_2d(&world_transform(world, car).unwrap().matrix);
        let (_, chassis_rotation, chassis_position) =
            chassis_matrix.to_scale_rotation_translation();
        for (i, wheel) in physics::vehicle_wheels(world, car)
            .unwrap()
            .iter()
            .enumerate()
        {
            let expected = chassis_position + chassis_rotation * wheel.local_position;
            let at = position(&runtime, &format!("wheel_{i}"));
            worst = worst.max((at - expected).length());
        }
    }
    let speed = physics::vehicle_state(runtime.app().world(), car)
        .unwrap()
        .speed;
    println!("{idle} idle frames at {speed:.1} m/s; worst wheel visual offset {worst:.2e} m");
    assert!(idle > 40 && speed > 3., "{idle} idle frames at {speed} m/s");
    assert!(worst < 1e-4, "wheel visuals lag the chassis by {worst} m");
}
