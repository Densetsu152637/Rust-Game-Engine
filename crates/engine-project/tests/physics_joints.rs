//! Authored joints: constraints between entities, in-place motor updates, break events,
//! validation, schema and native API.
use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{
    self, Dimension, JointMotor, JointStatus, PhysicsJoint3D,
};
use engine_project::*;
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn body3(data: Value) -> Value {
    json!({"engine.PhysicsBody3D":value(data)})
}
fn body2(data: Value) -> Value {
    json!({"engine.PhysicsBody2D":value(data)})
}
fn with(mut base: Value, name: &str, data: Value) -> Value {
    base[name] = value(data);
    base
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
            &json!({"schema":1,"name":"joints-test","startup_scene":"scene.json","assets":[]}),
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
fn entity_of(runtime: &Runtime, id: &str) -> Entity {
    runtime.entity(id).unwrap()
}
fn zero_gravity() -> Value {
    settings(json!({"gravity_2d":[0.,0.],"gravity_3d":[0.,0.,0.]}))
}

#[derive(Resource, Default)]
struct Broken(Vec<Value>);
fn record(world: &mut World, target: Entity, event: &EngineEvent) -> Result<()> {
    let id = world.get::<AuthoredId>(target).unwrap().id.clone();
    let mut payload = event.payload.clone();
    payload["target"] = json!(id);
    world.resource_mut::<Broken>().0.push(payload);
    Ok(())
}
fn bound(mut entity: Value) -> Value {
    entity["components"]["EventBindings"] =
        json!({"bindings":[{"event":"physics.joints.broken","handler":"record"}]});
    entity
}

#[test]
fn an_authored_spherical_joint_holds_a_pendulum_to_its_anchor() {
    let (_dir, mut runtime) = started(vec![
        entity(
            "anchor",
            [0.; 3],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
        entity(
            "bob",
            [1., 0., 0.],
            with(
                body3(json!({"shape":"ball","radius":0.1})),
                "engine.PhysicsJoint3D",
                json!({"kind":"spherical","body_b":"anchor","anchor_a":[-1.,0.,0.],"anchor_b":[0.,0.,0.]}),
            ),
        ),
        entity(
            "free",
            [3., 0., 0.],
            body3(json!({"shape":"ball","radius":0.1})),
        ),
    ]);
    let mut farthest = 0f32;
    let mut lowest = 0f32;
    for _ in 0..150 {
        ticks(&mut runtime, 1);
        let bob = position(&runtime, "bob");
        farthest = farthest.max(bob.length());
        lowest = lowest.min(bob.y);
    }
    assert!(farthest < 1.005, "pendulum stretched to {farthest}");
    assert!(lowest < -0.8, "pendulum did not swing: {lowest}");
    // The unjointed body just fell.
    assert!(position(&runtime, "free").y < -20.);
    let world = runtime.app().world();
    let bob = entity_of(&runtime, "bob");
    assert_eq!(
        physics::joint_status(world, bob, Dimension::Three).unwrap(),
        JointStatus::Active
    );
    let state = physics::joint_state(world, bob, Dimension::Three).unwrap();
    assert!(state.position.length() < 0.01, "{state:?}");
    let (_, three) = physics::physics_worlds(world).unwrap();
    assert_eq!(three.joint_count(), 1);
}

#[test]
fn a_standalone_2d_rope_entity_limits_two_bodies() {
    let (_dir, mut runtime) = started(vec![
        entity(
            "post",
            [0.; 3],
            body2(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
        entity(
            "weight",
            [0., -0.5, 0.],
            body2(json!({"shape":"ball","radius":0.1})),
        ),
        entity(
            "rope",
            [9., 9., 0.],
            with(
                json!({}),
                "engine.PhysicsJoint2D",
                json!({"kind":"rope","body_a":"post","body_b":"weight","max_distance":1.5}),
            ),
        ),
    ]);
    let mut longest = 0f32;
    for _ in 0..150 {
        ticks(&mut runtime, 1);
        longest = longest.max(position(&runtime, "weight").truncate().length());
    }
    assert!(longest < 1.52 && longest > 1.45, "rope length {longest}");
}

#[test]
fn motor_edits_apply_in_place_and_follow_the_component() {
    let (_dir, mut runtime) = started(vec![
        zero_gravity(),
        entity(
            "base",
            [0.; 3],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
        entity(
            "wheel",
            [0.; 3],
            with(
                body3(json!({"shape":"ball","radius":0.1})),
                "engine.PhysicsJoint3D",
                json!({"kind":"revolute","body_b":"base","axis":[0.,0.,1.],
                    "motors":[{"axis":"ang_x","target_velocity":2.0,"damping":50.0}]}),
            ),
        ),
    ]);
    let wheel = entity_of(&runtime, "wheel");
    ticks(&mut runtime, 75);
    let spin = physics::body_state(runtime.app().world(), wheel)
        .unwrap()
        .angular_velocity
        .z;
    // Motors drive body B relative to body A, so the wheel (A) turns the other way.
    assert!((spin + 2.).abs() < 0.15, "spin {spin}");
    // Retarget through the native API: same joint, new velocity.
    physics::set_joint_motor(
        runtime.app_mut().world_mut(),
        wheel,
        Dimension::Three,
        JointMotor {
            axis: "ang_x".into(),
            target_velocity: -3.,
            damping: 50.,
            ..JointMotor::default()
        },
    )
    .unwrap();
    ticks(&mut runtime, 75);
    let spin = physics::body_state(runtime.app().world(), wheel)
        .unwrap()
        .angular_velocity
        .z;
    assert!((spin - 3.).abs() < 0.2, "spin {spin}");
    // Clearing the motor stops driving; the wheel coasts at its current speed.
    physics::clear_joint_motor(
        runtime.app_mut().world_mut(),
        wheel,
        Dimension::Three,
        "ang_x",
    )
    .unwrap();
    ticks(&mut runtime, 25);
    let coast = physics::body_state(runtime.app().world(), wheel)
        .unwrap()
        .angular_velocity
        .z;
    assert!((coast - 3.).abs() < 0.3, "coast {coast}");
    // Locked axes cannot be motorized.
    let err = physics::set_joint_motor(
        runtime.app_mut().world_mut(),
        wheel,
        Dimension::Three,
        JointMotor {
            axis: "lin_x".into(),
            ..JointMotor::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("locked"), "{err}");
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert_eq!(three.joint_count(), 1);
}

#[test]
fn component_set_documents_validate_and_merge_motor_edits() {
    let dir = fixture(vec![
        entity("base", [0.; 3], body3(json!({"motion":"fixed"}))),
        entity(
            "arm",
            [0.; 3],
            with(
                body3(json!({})),
                "engine.PhysicsJoint3D",
                json!({"kind":"revolute","body_b":"base"}),
            ),
        ),
    ]);
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |data: Value, merge: bool| EditOperation::SetComponent {
        entity_id: "arm".into(),
        component: "engine.PhysicsJoint3D".into(),
        value: value(data),
        merge,
    };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(
            set(
                json!({"motors":[{"axis":"ang_x","target_position":0.5,"stiffness":20.0}]}),
                true,
            ),
            &revision,
        )
        .unwrap();
    // Unknown fields and bad enum values are schema errors.
    assert!(
        document
            .apply(set(json!({"no_such_field":1}), true), &revision)
            .is_err()
    );
    assert!(
        document
            .apply(set(json!({"kind":"hinge"}), true), &revision)
            .is_err()
    );
    // A reference to an entity that does not exist is rejected.
    assert!(
        document
            .apply(set(json!({"body_b":"ghost"}), true), &revision)
            .is_err()
    );
    // Prefab remapping covers both references.
    let project = Project::load(dir.path().join("project.json")).unwrap();
    for name in ["engine.PhysicsJoint2D", "engine.PhysicsJoint3D"] {
        let descriptor = &project.component_catalog.0[name];
        assert_eq!(
            descriptor.entity_references,
            ["/body_a", "/body_b"],
            "{name}"
        );
    }
    let schema = &project.component_catalog.0["engine.PhysicsJoint3D"].schema;
    assert_eq!(
        schema["properties"]["kind"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let schema2 = &project.component_catalog.0["engine.PhysicsJoint2D"].schema;
    assert!(
        !schema2["properties"]["kind"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("spherical"))
    );
}

#[test]
fn a_joint_beyond_its_break_force_breaks_reports_and_can_be_repaired() {
    let dir = fixture(vec![
        entity(
            "ceiling",
            [0., 5., 0.],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
        bound(entity(
            "crate",
            [0., 4., 0.],
            with(
                body3(json!({"shape":"ball","radius":0.5,"material":{"density":10.0}})),
                "engine.PhysicsJoint3D",
                json!({"kind":"fixed","body_b":"ceiling","anchor_a":[0.,1.,0.],
                    "break_force":150.0}),
            ),
        )),
    ]);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Broken::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    let crate_ = entity_of(&runtime, "crate");
    // mass = 10 * 4/3 pi 0.5^3 = 5.236 kg: weight 51 N, well below 150 N.
    ticks(&mut runtime, 50);
    assert_eq!(
        physics::joint_status(runtime.app().world(), crate_, Dimension::Three).unwrap(),
        JointStatus::Active
    );
    assert!((position(&runtime, "crate").y - 4.).abs() < 0.02);
    assert!(runtime.app().world().resource::<Broken>().0.is_empty());
    // Lower the threshold below the weight: the joint breaks and reports.
    physics::set_joint_break_limits(
        runtime.app_mut().world_mut(),
        crate_,
        Dimension::Three,
        Some(30.),
        None,
    )
    .unwrap();
    ticks(&mut runtime, 10);
    let status = physics::joint_status(runtime.app().world(), crate_, Dimension::Three).unwrap();
    let weight = 10. * 4. / 3. * std::f32::consts::PI * 0.5f32.powi(3) * 9.81;
    let JointStatus::Broken { force, .. } = status else {
        panic!("expected a broken joint, got {status:?}");
    };
    assert!((force - weight).abs() / weight < 0.1, "{force} vs {weight}");
    let events = runtime.app().world().resource::<Broken>().0.clone();
    assert!(!events.is_empty());
    let first = &events[0];
    assert_eq!(first["joint"], "crate");
    assert_eq!(first["dimension"], 3);
    assert_eq!(first["body_a"], "crate");
    assert_eq!(first["body_b"], "ceiling");
    let reported = first["force"].as_f64().unwrap() as f32;
    assert!((reported - weight).abs() / weight < 0.1, "{reported}");
    assert!(first["torque"].as_f64().unwrap().is_finite());
    let broken = physics::broken_joints(runtime.app().world()).unwrap();
    assert_eq!(broken.len(), 1);
    assert_eq!(
        (broken[0].entity, broken[0].dimension),
        (crate_, Dimension::Three)
    );
    ticks(&mut runtime, 25);
    assert!(position(&runtime, "crate").y < 3.);
    // Still broken (not re-created) while the component is unchanged.
    assert!(matches!(
        physics::joint_status(runtime.app().world(), crate_, Dimension::Three).unwrap(),
        JointStatus::Broken { .. }
    ));
    // Editing the component re-creates the joint.
    let count = runtime.app().world().resource::<Broken>().0.len();
    physics::set_joint_break_limits(
        runtime.app_mut().world_mut(),
        crate_,
        Dimension::Three,
        Some(5000.),
        None,
    )
    .unwrap();
    ticks(&mut runtime, 2);
    assert_eq!(
        physics::joint_status(runtime.app().world(), crate_, Dimension::Three).unwrap(),
        JointStatus::Active
    );
    assert!(
        physics::broken_joints(runtime.app().world())
            .unwrap()
            .is_empty()
    );
    assert_eq!(runtime.app().world().resource::<Broken>().0.len(), count);
}

#[test]
fn rebuilding_a_body_restores_its_joint() {
    let (_dir, mut runtime) = started(vec![
        zero_gravity(),
        entity(
            "base",
            [0.; 3],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
        entity(
            "arm",
            [1., 0., 0.],
            with(
                body3(json!({"shape":"ball","radius":0.1})),
                "engine.PhysicsJoint3D",
                json!({"kind":"prismatic","body_b":"base","axis":[1.,0.,0.],"limits":[-1.,1.]}),
            ),
        ),
    ]);
    let arm = entity_of(&runtime, "arm");
    ticks(&mut runtime, 2);
    // Changing the body config rebuilds it (and drops its engine-physics joint).
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<physics::PhysicsBody3D>(arm)
        .unwrap()
        .radius = 0.2;
    ticks(&mut runtime, 2);
    assert_eq!(
        physics::joint_status(runtime.app().world(), arm, Dimension::Three).unwrap(),
        JointStatus::Active
    );
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert_eq!(three.joint_count(), 1);
    // Removing the component removes the constraint.
    assert!(physics::remove_joint(
        runtime.app_mut().world_mut(),
        arm,
        Dimension::Three
    ));
    ticks(&mut runtime, 2);
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert_eq!(three.joint_count(), 0);
}

#[test]
fn native_api_adds_joints_and_rejects_invalid_ones() {
    let (_dir, mut runtime) = started(vec![
        zero_gravity(),
        entity("base", [0.; 3], body3(json!({"motion":"fixed"}))),
        entity("arm", [1., 0., 0.], body3(json!({}))),
    ]);
    let arm = entity_of(&runtime, "arm");
    let mut joint = PhysicsJoint3D {
        kind: physics::JointKind::Distance,
        body_b: Some("base".into()),
        min_distance: 0.5,
        max_distance: 1.5,
        ..PhysicsJoint3D::default()
    };
    physics::add_joint_3d(runtime.app_mut().world_mut(), arm, joint.clone()).unwrap();
    ticks(&mut runtime, 2);
    assert_eq!(
        physics::joint_status(runtime.app().world(), arm, Dimension::Three).unwrap(),
        JointStatus::Active
    );
    physics::apply_impulse(runtime.app_mut().world_mut(), arm, Vec3::new(8., 0., 0.)).unwrap();
    ticks(&mut runtime, 100);
    let reach = position(&runtime, "arm").length();
    assert!(reach < 1.52 && reach > 1.4, "{reach}");
    joint.max_distance = f32::NAN;
    assert!(physics::add_joint_3d(runtime.app_mut().world_mut(), arm, joint).is_err());
    let missing = PhysicsJoint3D::default();
    let err = physics::add_joint_3d(runtime.app_mut().world_mut(), arm, missing)
        .unwrap_err()
        .to_string();
    assert!(err.contains("body_b"), "{err}");
}

#[test]
fn invalid_joint_authoring_is_rejected_with_precise_messages() {
    let joint = |dim: u8, data: Value| {
        let name = format!("engine.PhysicsJoint{dim}D");
        let make = |id: &str, extra: Value| {
            let mut e = entity(
                id,
                [0.; 3],
                if dim == 2 {
                    body2(json!({}))
                } else {
                    body3(json!({}))
                },
            );
            if !extra.is_null() {
                e["components"]["custom"][&name] = value(extra);
            }
            e
        };
        vec![make("a", data), make("b", Value::Null)]
    };
    // The 2D schema has no spherical kind; the native API explains why.
    let spherical = rejection(joint(2, json!({"kind":"spherical","body_b":"b"})));
    assert!(spherical.contains("not in enum"), "{spherical}");
    let (_dir, mut runtime) = started(vec![entity("a", [0.; 3], body2(json!({})))]);
    let a = runtime.entity("a").unwrap();
    let native = physics::add_joint_2d(
        runtime.app_mut().world_mut(),
        a,
        physics::PhysicsJoint2D {
            kind: physics::JointKind::Spherical,
            body_b: Some("a".into()),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(native.contains("3D only"), "{native}");
    let no_target = rejection(joint(3, json!({"kind":"fixed"})));
    assert!(no_target.contains("requires body_b"), "{no_target}");
    let to_self = rejection(joint(3, json!({"kind":"fixed","body_b":"a"})));
    assert!(to_self.contains("two different bodies"), "{to_self}");
    let locked = rejection(joint(
        3,
        json!({"kind":"fixed","body_b":"b","motors":[{"axis":"ang_x","damping":1.0}]}),
    ));
    assert!(locked.contains("locked"), "{locked}");
    let limits = rejection(joint(
        3,
        json!({"kind":"prismatic","body_b":"b","limits":[1.0,-1.0]}),
    ));
    assert!(limits.contains("ordered"), "{limits}");
    let range = rejection(joint(
        3,
        json!({"kind":"distance","body_b":"b","min_distance":2.0,"max_distance":1.0}),
    ));
    assert!(range.contains("min <= max"), "{range}");
    let axes = rejection(joint(
        2,
        json!({"kind":"fixed","body_b":"b","axes":[{"axis":"lin_x"}]}),
    ));
    assert!(axes.contains("generic"), "{axes}");
    let break_force = rejection(joint(
        3,
        json!({"kind":"fixed","body_b":"b","break_force":0.0}),
    ));
    assert!(break_force.contains("positive"), "{break_force}");
    let mismatch = rejection(vec![
        entity("a", [0.; 3], body3(json!({}))),
        entity(
            "b",
            [0.; 3],
            with(
                body2(json!({})),
                "engine.PhysicsJoint2D",
                json!({"kind":"fixed","body_b":"a"}),
            ),
        ),
    ]);
    assert!(mismatch.contains("engine.PhysicsBody2D"), "{mismatch}");
}

#[test]
fn a_generic_joint_frees_only_the_listed_axes() {
    let (_dir, mut runtime) = started(vec![
        zero_gravity(),
        entity(
            "base",
            [0.; 3],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
        entity(
            "slider",
            [0.; 3],
            with(
                body3(json!({"shape":"ball","radius":0.1})),
                "engine.PhysicsJoint3D",
                json!({"kind":"generic","body_b":"base",
                    "axes":[{"axis":"lin_y","mode":"limited","min":-0.5,"max":0.5}]}),
            ),
        ),
    ]);
    let slider = entity_of(&runtime, "slider");
    physics::apply_impulse(runtime.app_mut().world_mut(), slider, Vec3::new(3., 4., 2.)).unwrap();
    physics::apply_angular_impulse(
        runtime.app_mut().world_mut(),
        slider,
        Vec3::new(0.1, 0.1, 0.1),
    )
    .unwrap();
    ticks(&mut runtime, 100);
    let p = position(&runtime, "slider");
    assert!((p.y - 0.5).abs() < 0.02, "{p:?}");
    assert!(p.x.abs() < 1e-3 && p.z.abs() < 1e-3, "{p:?}");
    let spin = physics::body_state(runtime.app().world(), slider)
        .unwrap()
        .angular_velocity;
    assert!(spin.length() < 1e-2, "{spin:?}");
}

// ---- Critic fixes: bounded break history ----------------------------------------------------

/// M2: engine-physics keeps the last 1024 break records. A joint whose record was evicted must
/// stay broken, not be re-created (and break again) by the next sync.
#[test]
fn joints_evicted_from_the_break_history_are_not_re_created() {
    const WAVE: usize = 400;
    let mut entities = vec![
        settings(json!({"gravity_3d":[0.,-9.81,0.]})),
        entity(
            "anchor",
            [0.; 3],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
    ];
    for i in 0..3 * WAVE {
        let x = i as f32 * 3.;
        entities.push(entity(
            &format!("b{i}"),
            [x, -1., 0.],
            with(
                body3(json!({"shape":"ball","radius":0.1,"material":{"density":1000.0}})),
                "engine.PhysicsJoint3D",
                json!({"kind":"fixed","body_b":"anchor","anchor_a":[0.,0.,0.],
                    "anchor_b":[x, -1., 0.]}),
            ),
        ));
    }
    let (_dir, mut runtime) = started(entities);
    ticks(&mut runtime, 3);
    let status = |runtime: &Runtime, i: usize| {
        let e = entity_of(runtime, &format!("b{i}"));
        physics::joint_status(runtime.app().world(), e, Dimension::Three).unwrap()
    };
    assert_eq!(status(&runtime, 0), JointStatus::Active);
    // Three waves of 400 break on successive ticks: 1200 breaks, 1024 records.
    for wave in 0..3 {
        for i in wave * WAVE..(wave + 1) * WAVE {
            let e = entity_of(&runtime, &format!("b{i}"));
            physics::set_joint_break_limits(
                runtime.app_mut().world_mut(),
                e,
                Dimension::Three,
                Some(1.),
                None,
            )
            .unwrap();
        }
        ticks(&mut runtime, 1);
    }
    ticks(&mut runtime, 2);
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    println!(
        "{} broken records after 1200 breaks, {} live joints",
        three.broken_joints().len(),
        three.joint_count()
    );
    assert_eq!(three.broken_joints().len(), 1024);
    assert_eq!(three.joint_count(), 0);
    // The oldest 176 records (the first wave breaks first) were evicted: those joints read as
    // not simulated, all others as broken, and nothing re-creates them.
    let statuses: Vec<JointStatus> = (0..3 * WAVE).map(|i| status(&runtime, i)).collect();
    let inactive: Vec<usize> = (0..3 * WAVE)
        .filter(|i| statuses[*i] == JointStatus::Inactive)
        .collect();
    assert_eq!(inactive.len(), 176, "{inactive:?}");
    assert!(inactive.iter().all(|i| *i < WAVE), "{inactive:?}");
    assert!(
        statuses
            .iter()
            .filter(|s| **s != JointStatus::Inactive)
            .all(|s| matches!(s, JointStatus::Broken { .. }))
    );
    for _ in 0..4 {
        ticks(&mut runtime, 1);
        let repeated = runtime
            .snapshot()
            .events
            .iter()
            .filter(|e| e.name == "physics.joints.broken")
            .count();
        assert_eq!(repeated, 0, "evicted joints broke again");
    }
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert_eq!(three.joint_count(), 0);
}

/// L4: more events in one tick than the engine event queue holds are dropped in a bounded,
/// counted way, with one warning event; the tick itself succeeds.
#[test]
fn an_event_flood_drops_events_with_a_counter_and_one_warning() {
    const COUNT: usize = 700;
    let mut settings_entity = settings(json!({"gravity_3d":[0.,-9.81,0.]}));
    settings_entity["components"]["EventBindings"] =
        json!({"bindings":[{"event":"physics.events.dropped","handler":"record"}]});
    let mut entities = vec![
        settings_entity,
        entity(
            "anchor",
            [0.; 3],
            body3(json!({"motion":"fixed","shape":"ball","radius":0.1})),
        ),
    ];
    for i in 0..COUNT {
        let x = i as f32 * 3.;
        entities.push(entity(
            &format!("b{i}"),
            [x, -1., 0.],
            with(
                body3(json!({"shape":"ball","radius":0.1,"material":{"density":1000.0}})),
                "engine.PhysicsJoint3D",
                json!({"kind":"fixed","body_b":"anchor","anchor_a":[0.,0.,0.],
                    "anchor_b":[x, -1., 0.],"break_force":1.0}),
            ),
        ));
    }
    let dir = fixture(entities);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Broken::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    // Every joint breaks in the first step: two events each (the body and the anchor) is more
    // than the 1024-event queue holds. This used to abort the step with "Event queue capacity
    // exceeded".
    ticks(&mut runtime, 1);
    let status = physics::pressure_status(runtime.app().world()).unwrap();
    println!("{status:?}");
    assert!(
        status.events_dropped >= 2 * COUNT as u64 - 1024,
        "{status:?}"
    );
    assert!(status.events_dropped < 2 * COUNT as u64, "{status:?}");
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    assert_eq!(three.broken_joints().len(), COUNT);
    // The warning is delivered once, one tick later, with the count.
    ticks(&mut runtime, 2);
    ticks(&mut runtime, 1);
    let warnings: Vec<_> = runtime
        .app()
        .world()
        .resource::<Broken>()
        .0
        .iter()
        .filter(|p| p.get("dropped").is_some())
        .cloned()
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["dropped"], status.events_dropped);
    assert_eq!(warnings[0]["target"], "settings");
    assert_eq!(
        physics::pressure_status(runtime.app().world())
            .unwrap()
            .events_dropped,
        status.events_dropped
    );
}
