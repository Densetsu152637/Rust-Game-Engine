use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{
    self, BodyKinds, Dimension, QueryFilter, QueryPose, QueryShape, SensorFilter,
};
use engine_project::*;
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

const TOL: f32 = 1e-4;

fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":{"version":1,"data":data}}}})
}
fn body2(data: Value) -> Value {
    json!({"engine.PhysicsBody2D":{"version":1,"data":data}})
}
fn body3(data: Value) -> Value {
    json!({"engine.PhysicsBody3D":{"version":1,"data":data}})
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
            &json!({"schema":1,"name":"physics-queries","startup_scene":"scene.json","assets":[]}),
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
fn tick(runtime: &mut Runtime, dt: f32) {
    runtime.tick(dt, InputSnapshot::default()).unwrap();
}
fn world(runtime: &Runtime) -> &World {
    runtime.app().world()
}
fn close(a: Vec3, b: Vec3, tol: f32) {
    assert!(a.abs_diff_eq(b, tol), "{a} != {b}");
}
fn ent(runtime: &Runtime, id: &str) -> Entity {
    runtime.entity(id).unwrap()
}
fn fixed_box(id: &str, at: [f32; 3], half: [f32; 3], extra: Value) -> Value {
    let mut body = json!({"motion":"fixed","half_extents":half});
    for (key, value) in extra.as_object().into_iter().flatten() {
        body[key] = value.clone();
    }
    entity(id, at, body3(body))
}
/// Fixed ground slab (top at y = 0), a fixed box, a fixed ball and a sensor ball.
fn arena() -> Vec<Value> {
    vec![
        settings(json!({"gravity_3d":[0.,0.,0.],"layers":["world","trigger"]})),
        fixed_box(
            "ground",
            [0., -1., 0.],
            [50., 1., 50.],
            json!({"material":{"layer_name":"world"}}),
        ),
        fixed_box(
            "wall",
            [0., 1., 5.],
            [1., 1., 0.5],
            json!({"material":{"layer_name":"world"}}),
        ),
        entity(
            "orb",
            [8., 1., 0.],
            body3(json!({"motion":"fixed","shape":"ball","radius":2.0,
                "material":{"layer_name":"world"}})),
        ),
        entity(
            "zone",
            [0., 1., 10.],
            body3(json!({"motion":"fixed","shape":"ball","radius":1.0,
                "material":{"sensor":true,"layer_name":"trigger"}})),
        ),
        entity(
            "mover",
            [0., 1., -5.],
            body3(json!({"motion":"kinematic","half_extents":[0.5,0.5,0.5],
                "material":{"layer_name":"world"}})),
        ),
    ]
}

#[test]
fn ray_cast_returns_entity_point_normal_and_distance() {
    let (_dir, runtime) = started(arena());
    let w = world(&runtime);
    let all = QueryFilter::default();
    let hit = physics::ray_cast(
        w,
        Dimension::Three,
        Vec3::new(0., 1., 0.),
        Vec3::Z,
        100.,
        &all,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hit.entity, ent(&runtime, "wall"));
    assert!((hit.distance - 4.5).abs() < TOL);
    close(hit.point, Vec3::new(0., 1., 4.5), TOL);
    close(hit.normal, -Vec3::Z, TOL);
    // Sphere "orb" at x = 8, radius 2, along +x at y = 1: hit at x = 6.
    let hit = physics::ray_cast(
        w,
        Dimension::Three,
        Vec3::new(0., 1., 0.),
        Vec3::X,
        100.,
        &all,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hit.entity, ent(&runtime, "orb"));
    assert!((hit.distance - 6.).abs() < TOL);
    close(hit.normal, -Vec3::X, TOL);
    // ray_cast_all lists wall, then the sensor zone behind it, sorted by distance.
    let hits = physics::ray_cast_all(
        w,
        Dimension::Three,
        Vec3::new(0., 1., 0.),
        Vec3::Z,
        100.,
        &all,
        8,
    )
    .unwrap();
    let names: Vec<_> = hits.hits.iter().map(|h| h.entity).collect();
    assert_eq!(names, [ent(&runtime, "wall"), ent(&runtime, "zone")]);
    assert!(!hits.truncated);
    assert!(hits.hits[0].distance < hits.hits[1].distance);
}

#[test]
fn filters_select_layers_sensors_kinds_and_exclusions() {
    let (_dir, runtime) = started(arena());
    let w = world(&runtime);
    let origin = Vec3::new(0., 1., 0.);
    let ids = |filter: &QueryFilter, direction: Vec3| -> Vec<Entity> {
        physics::ray_cast_all(w, Dimension::Three, origin, direction, 100., filter, 16)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.entity)
            .collect()
    };
    let wall = ent(&runtime, "wall");
    let zone = ent(&runtime, "zone");
    let mover = ent(&runtime, "mover");
    let trigger_only = QueryFilter::with_mask(physics::layer_mask(w, &["trigger"]).unwrap());
    assert_eq!(ids(&trigger_only, Vec3::Z), [zone]);
    let world_only = QueryFilter::with_mask(physics::layer_mask(w, &["world"]).unwrap());
    assert_eq!(ids(&world_only, Vec3::Z), [wall]);
    let solid = QueryFilter {
        sensors: SensorFilter::Exclude,
        ..Default::default()
    };
    assert_eq!(ids(&solid, Vec3::Z), [wall]);
    let sensors = QueryFilter {
        sensors: SensorFilter::Only,
        ..Default::default()
    };
    assert_eq!(ids(&sensors, Vec3::Z), [zone]);
    let skip = QueryFilter {
        exclude: vec![wall],
        ..Default::default()
    };
    assert_eq!(ids(&skip, Vec3::Z), [zone]);
    let kinematic = QueryFilter {
        kinds: BodyKinds {
            fixed: false,
            dynamic: false,
            kinematic: true,
        },
        ..Default::default()
    };
    assert_eq!(ids(&kinematic, -Vec3::Z), [mover]);
    assert_eq!(ids(&Default::default(), -Vec3::Z), [mover]);
    assert!(physics::layer_mask(w, &["ghost"]).is_err());
}

#[test]
fn shape_cast_time_of_impact_matches_analytic_ball_to_plane() {
    let (_dir, runtime) = started(arena());
    let w = world(&runtime);
    let all = QueryFilter::default();
    let ball = QueryShape::Ball { radius: 0.5 };
    let hit = physics::shape_cast(
        w,
        Dimension::Three,
        &ball,
        QueryPose::at(Vec3::new(20., 5., 20.)),
        -Vec3::Y,
        100.,
        &all,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hit.entity, ent(&runtime, "ground"));
    assert!((hit.distance - 4.5).abs() < TOL, "{}", hit.distance);
    close(hit.normal, Vec3::Y, 1e-3);
    close(hit.point, Vec3::new(20., 0., 20.), 2e-3);
    assert!(!hit.penetrating);
    // A cast that misses everything.
    assert!(
        physics::shape_cast(
            w,
            Dimension::Three,
            &ball,
            QueryPose::at(Vec3::new(20., 5., 20.)),
            Vec3::Y,
            100.,
            &all
        )
        .unwrap()
        .is_none()
    );
    // Box sweep, rotated 90 degrees about Y, against the wall (z = 4.5 face).
    let boxed = QueryShape::Box {
        half_extents: Vec3::new(0.25, 0.25, 1.0),
    };
    let pose = QueryPose {
        position: Vec3::new(0., 1., 0.),
        rotation: glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
    };
    let hit = physics::shape_cast(w, Dimension::Three, &boxed, pose, Vec3::Z, 100., &all)
        .unwrap()
        .unwrap();
    // Rotated, its z extent is 0.25: front face at 0.25, wall face at 4.5.
    assert!((hit.distance - 4.25).abs() < TOL, "{}", hit.distance);
}

#[test]
fn overlap_and_point_queries_return_expected_sets() {
    let (_dir, runtime) = started(arena());
    let w = world(&runtime);
    let all = QueryFilter::default();
    let probe = QueryShape::Ball { radius: 3.0 };
    let hits = physics::overlap(
        w,
        Dimension::Three,
        &probe,
        QueryPose::at(Vec3::new(0., 1., 7.)),
        &all,
        16,
    )
    .unwrap();
    // Wall (z 4.5..5.5) and zone (z 9..11) are within 3 m of (0, 1, 7); ground is not
    // (top at y = 0 is 1 m below, so it intersects too).
    let mut got: Vec<_> = hits.hits.iter().map(|h| h.entity).collect();
    got.sort();
    let mut want = vec![
        ent(&runtime, "wall"),
        ent(&runtime, "zone"),
        ent(&runtime, "ground"),
    ];
    want.sort();
    assert_eq!(got, want);
    let solid = QueryFilter {
        sensors: SensorFilter::Exclude,
        ..Default::default()
    };
    let hits = physics::overlap(
        w,
        Dimension::Three,
        &probe,
        QueryPose::at(Vec3::new(0., 1., 7.)),
        &solid,
        16,
    )
    .unwrap();
    assert_eq!(hits.hits.len(), 2);
    // Point projection: 1 m in front of the orb's surface, then inside it.
    let near = physics::project_point(w, Dimension::Three, Vec3::new(5., 1., 0.), &all)
        .unwrap()
        .unwrap();
    assert_eq!(near.entity, ent(&runtime, "orb"));
    assert!((near.distance - 1.).abs() < TOL && !near.inside);
    let inside =
        physics::contains_point(w, Dimension::Three, Vec3::new(8., 1., 0.5), &all, 8).unwrap();
    assert_eq!(inside.hits.len(), 1);
    assert_eq!(inside.hits[0].entity, ent(&runtime, "orb"));
    assert!(inside.hits[0].inside);
}

#[test]
fn two_dimensional_queries_use_the_xy_plane() {
    let (_dir, runtime) = started(vec![
        settings(json!({"gravity_2d":[0.,0.]})),
        entity(
            "floor",
            [0., -1., 0.],
            body2(json!({"motion":"fixed","half_extents":[20.,1.]})),
        ),
        entity(
            "coin",
            [5., 2., 0.],
            body2(json!({"motion":"fixed","shape":"ball","radius":1.0})),
        ),
    ]);
    let w = world(&runtime);
    let all = QueryFilter::default();
    let hit = physics::ray_cast(w, Dimension::Two, Vec3::new(0., 2., 0.), Vec3::X, 50., &all)
        .unwrap()
        .unwrap();
    assert_eq!(hit.entity, ent(&runtime, "coin"));
    assert!((hit.distance - 4.).abs() < TOL);
    assert_eq!(hit.point.z, 0.);
    let swept = physics::shape_cast(
        w,
        Dimension::Two,
        &QueryShape::Ball { radius: 0.5 },
        QueryPose::at(Vec3::new(-6., 4., 9.)),
        -Vec3::Y,
        50.,
        &all,
    )
    .unwrap()
    .unwrap();
    assert_eq!(swept.entity, ent(&runtime, "floor"));
    assert!((swept.distance - 3.5).abs() < TOL);
    assert!(
        physics::shape_cast(
            w,
            Dimension::Two,
            &QueryShape::Cylinder {
                half_height: 1.,
                radius: 1.
            },
            QueryPose::at(Vec3::ZERO),
            Vec3::X,
            1.,
            &all
        )
        .is_err()
    );
}

#[test]
fn queries_see_bodies_right_after_sync_and_forget_removed_ones() {
    let (_dir, mut runtime) = started(arena());
    let all = QueryFilter::default();
    let probe = |runtime: &Runtime| {
        physics::overlap(
            world(runtime),
            Dimension::Three,
            &QueryShape::Ball { radius: 0.2 },
            QueryPose::at(Vec3::new(-20., 3., 0.)),
            &all,
            4,
        )
        .unwrap()
        .hits
        .len()
    };
    assert_eq!(probe(&runtime), 0);
    let crate_entity = runtime
        .app_mut()
        .world_mut()
        .spawn((
            Transform {
                version: 1,
                position: [-20., 3., 0.],
                rotation: [0., 0., 0., 1.],
                scale: [1.; 3],
            },
            simulation::PhysicsBody3D {
                motion: simulation::Motion::Fixed,
                ..Default::default()
            },
        ))
        .id();
    tick(&mut runtime, 0.02);
    assert_eq!(probe(&runtime), 1, "queryable right after the syncing tick");
    runtime.app_mut().world_mut().despawn(crate_entity);
    tick(&mut runtime, 0.02);
    assert_eq!(probe(&runtime), 0, "gone right after the syncing tick");
}

/// Ground, three stacked cubes and a bouncy ball: sensitive to any step-size difference.
fn playground(extra: Value) -> Vec<Value> {
    let mut config = json!({"gravity_3d":[0.,-9.81,0.]});
    for (key, value) in extra.as_object().into_iter().flatten() {
        config[key] = value.clone();
    }
    let mut entities = vec![
        settings(config),
        fixed_box("ground", [0., -1., 0.], [50., 1., 50.], json!({})),
    ];
    for i in 0..3 {
        entities.push(entity(
            &format!("cube{i}"),
            [0.1 * i as f32, 0.5 + 1.2 * i as f32, 0.],
            body3(json!({"half_extents":[0.5,0.5,0.5]})),
        ));
    }
    entities.push(entity(
        "ball",
        [3., 6., 0.3],
        body3(json!({"shape":"ball","radius":0.4,"velocity":[0.5,0.,0.2],
            "material":{"restitution":0.7}})),
    ));
    entities
}
fn pose_bits(runtime: &Runtime) -> Vec<[u32; 7]> {
    ["cube0", "cube1", "cube2", "ball"]
        .iter()
        .map(|id| {
            let t = world(runtime).get::<Transform>(ent(runtime, id)).unwrap();
            let mut bits = [0; 7];
            for (slot, value) in bits.iter_mut().zip(t.position.iter().chain(&t.rotation)) {
                *slot = value.to_bits();
            }
            bits
        })
        .collect()
}
fn run_split(frames: &[f32]) -> (Vec<[u32; 7]>, physics::StepStatus) {
    let h = 1.0f32 / 60.0;
    // Raise the cap: this test is about frame splits, not the spiral-of-death clamp.
    let (_dir, mut runtime) = started(playground(json!({"fixed_timestep":h,"max_substeps":64})));
    for dt in frames {
        tick(&mut runtime, *dt);
    }
    (
        pose_bits(&runtime),
        physics::step_status(world(&runtime)).unwrap(),
    )
}

#[test]
fn fixed_step_runs_are_bitwise_identical_and_independent_of_frame_splits() {
    let h = 1.0f32 / 60.0;
    let reference = run_split(&vec![h; 60]);
    assert_eq!(reference.1.total_steps, 60);
    assert_eq!(reference, run_split(&vec![h; 60]), "identical runs");
    let splits: [Vec<f32>; 5] = [
        vec![1. / 30.; 30],
        vec![0.05; 20],
        vec![1. / 120.; 120],
        vec![0.3, 0.3, 0.4],
        vec![0.01, 0.05, 0.02, 0.07, 0.015, 0.835],
    ];
    for split in splits {
        let total: f64 = split.iter().map(|d| f64::from(*d)).sum();
        assert!((total - 1.0).abs() < 1e-6, "{total}");
        let (bits, status) = run_split(&split);
        assert_eq!(status.total_steps, 60, "{split:?}");
        assert_eq!(bits, reference.0, "poses differ for split {split:?}");
    }
    // The scene is not trivially static: the ball moved and the cubes fell.
    let (_dir, mut runtime) = started(playground(json!({"fixed_timestep":h})));
    let before = pose_bits(&runtime);
    for _ in 0..60 {
        tick(&mut runtime, h);
    }
    assert_ne!(pose_bits(&runtime), before);
}

#[test]
fn variable_stepping_still_depends_on_the_frame_split() {
    // Documents why the fixed step exists: without it, splits differ.
    let run = |frames: &[f32]| {
        let (_dir, mut runtime) = started(playground(json!({})));
        for dt in frames {
            tick(&mut runtime, *dt);
        }
        pose_bits(&runtime)
    };
    assert_ne!(run(&[0.05; 20]), run(&[0.02; 50]));
}

#[test]
fn spiral_of_death_is_capped_reported_and_evented() {
    let h = 1.0f32 / 60.0;
    let mut scene = playground(json!({"fixed_timestep":h,"max_substeps":4}));
    scene[0]["components"]["EventBindings"] =
        json!({"bindings":[{"event":"physics.step.clamped","handler":"record"}]});
    let dir = fixture(scene);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    #[derive(Resource, Default)]
    struct Clamps(Vec<Value>);
    fn record(world: &mut World, _target: Entity, event: &EngineEvent) -> Result<()> {
        world.resource_mut::<Clamps>().0.push(event.payload.clone());
        Ok(())
    }
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Clamps::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    // A 0.5 s hitch is 30 steps due; only 4 run and 26 are dropped.
    tick(&mut runtime, 0.5);
    let status = physics::step_status(world(&runtime)).unwrap();
    assert_eq!(status.steps, 4);
    assert!(status.clamped);
    assert_eq!(status.clamped_frames, 1);
    assert!((status.dropped_time - 26. * h).abs() < 1e-4, "{status:?}");
    assert!(status.accumulator < h);
    // Event handlers run on the following dispatch.
    tick(&mut runtime, h);
    let events = &world(&runtime).resource::<Clamps>().0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["max_substeps"], 4);
    assert!((events[0]["dropped_seconds"].as_f64().unwrap() - 26. / 60.).abs() < 1e-4);
    // The follow-up frame was not clamped and keeps counting steps.
    let status = physics::step_status(world(&runtime)).unwrap();
    assert!(!status.clamped && status.steps == 1);
    assert_eq!(world(&runtime).resource::<Clamps>().0.len(), 1);
    assert_eq!(status.total_steps, 5);
}

#[test]
fn interpolation_blends_transforms_but_not_physics_state() {
    let h = 1.0f32 / 60.0;
    let frame = h * 1.5;
    let drop = |extra: Value| {
        let mut config = json!({"fixed_timestep":h});
        for (k, v) in extra.as_object().into_iter().flatten() {
            config[k] = v.clone();
        }
        let (_dir, mut runtime) = started(playground(config));
        let start = physics::body_state(world(&runtime), ent(&runtime, "ball"))
            .unwrap()
            .position;
        tick(&mut runtime, frame);
        let state = physics::body_state(world(&runtime), ent(&runtime, "ball")).unwrap();
        let transform = Vec3::from(
            world(&runtime)
                .get::<Transform>(ent(&runtime, "ball"))
                .unwrap()
                .position,
        );
        let status = physics::step_status(world(&runtime)).unwrap();
        (start, state.position, transform, status)
    };
    {
        let (_dir, runtime) = started(playground(json!({})));
        assert_eq!(physics::interpolation_alpha(world(&runtime)).unwrap(), 1.0);
    }
    let (start, plain_state, plain_transform, plain_status) = drop(json!({}));
    assert_eq!(plain_status.steps, 1);
    assert!((plain_status.alpha - 0.5).abs() < 1e-3, "{plain_status:?}");
    assert_eq!(
        plain_transform, plain_state,
        "no interpolation: latest pose"
    );
    let (_, state, transform, status) = drop(json!({"interpolation":"interpolate"}));
    assert_eq!(state, plain_state, "physics state is not interpolated");
    assert!((status.alpha - 0.5).abs() < 1e-3);
    assert!(physics::interpolation_alpha(&World::new()).is_err());
    let expected = start.lerp(plain_state, status.alpha);
    close(transform, expected, 1e-5);
    assert!(transform.y < start.y && transform.y > plain_state.y);
}

#[test]
fn debug_view_is_gated_bounded_and_does_not_disturb_the_simulation() {
    let (_dir, runtime) = started(playground(json!({})));
    let err = physics::debug_view(world(&runtime))
        .unwrap_err()
        .to_string();
    assert!(err.contains("disabled"), "{err}");
    let all = physics::debug_view_with(world(&runtime), &Default::default()).unwrap();
    assert_eq!(all.three.colliders.len(), 5);
    assert!(!all.truncated());
    drop(runtime);

    let (_dir, mut runtime) = started(playground(
        json!({"debug":true,"debug_aabbs":true,"debug_max_items":3}),
    ));
    for _ in 0..30 {
        tick(&mut runtime, 0.02);
    }
    let view = physics::debug_view(world(&runtime)).unwrap();
    assert_eq!(view.three.colliders.len(), 3);
    assert_eq!(view.three.totals.colliders, 5);
    assert!(view.three.contacts.len() <= 3);
    assert!(view.truncated());
    assert!(view.three.colliders.iter().all(|c| c.aabb.is_some()));
    let ids: Vec<_> = view
        .three
        .colliders
        .iter()
        .map(|c| physics::body_entity(c.id).unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    let lines = view.lines(10_000);
    assert!(!lines.segments.is_empty() && !lines.truncated);
    assert!(lines.segments.iter().all(|s| s.color[3] > 0.));
    let cut = view.lines(5);
    assert_eq!(cut.segments.len(), 5);
    assert!(cut.truncated);
}

#[test]
fn debug_toggle_does_not_change_simulation_results() {
    let bits = |debug: bool| {
        let (_dir, mut runtime) = started(playground(json!({"debug":debug})));
        for _ in 0..40 {
            tick(&mut runtime, 0.02);
        }
        pose_bits(&runtime)
    };
    assert_eq!(bits(false), bits(true));
}

#[test]
fn stepping_and_debug_settings_are_validated() {
    let reject = |data: Value| rejection(vec![settings(data)]);
    assert!(reject(json!({"interpolation":"interpolate"})).contains("requires fixed_timestep"));
    // Numeric bounds are enforced by the component schema before the service sees them.
    for bad in [
        json!({"fixed_timestep":0.5}),
        json!({"fixed_timestep":0.0001}),
        json!({"fixed_timestep":0.01,"max_substeps":0}),
        json!({"fixed_timestep":0.01,"max_substeps":65}),
        json!({"debug_max_items":0}),
    ] {
        let message = reject(bad.clone());
        assert!(message.contains("outside bounds"), "{bad}: {message}");
    }
    assert!(!reject(json!({"fixed_timestep":"fast"})).is_empty());
}

#[test]
fn headless_scene_queries_and_debug_summaries_round_trip() {
    let dir = fixture(arena());
    let project = || Project::load(dir.path().join("project.json")).unwrap();
    let request =
        |value: Value| -> physics::QueryRequest { serde_json::from_value(value).unwrap() };
    let ray = physics::query_scene(
        project(),
        2,
        0.02,
        &request(json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"all":true})),
    )
    .unwrap();
    assert_eq!(ray["count"], 2);
    assert_eq!(ray["hits"][0]["entity"], "wall");
    assert_eq!(ray["hits"][1]["entity"], "zone");
    assert_eq!(ray["hits"][0]["distance"], 4.5);
    assert_eq!(ray["hits"][0]["normal"], json!([0.0, 0.0, -1.0]));
    assert_eq!(ray["ticks"], 2);
    let cast = physics::query_scene(
        project(),
        0,
        0.02,
        &request(json!({"kind":"shape_cast","shape":"ball","radius":0.5,
            "position":[20,5,20],"direction":[0,-1,0],"max_distance":100,
            "filter":{"sensors":"exclude"}})),
    )
    .unwrap();
    assert_eq!(cast["hits"][0]["entity"], "ground");
    let overlap = physics::query_scene(
        project(),
        0,
        0.02,
        &request(json!({"kind":"overlap","shape":"ball","radius":3.0,
            "position":[0,1,7],"filter":{"layers":["trigger"]}})),
    )
    .unwrap();
    assert_eq!(overlap["count"], 1);
    assert_eq!(overlap["hits"][0]["entity"], "zone");
    let point = physics::query_scene(
        project(),
        0,
        0.02,
        &request(json!({"kind":"point","point":[8,1,0.5],"contains":true})),
    )
    .unwrap();
    assert_eq!(point["hits"][0]["entity"], "orb");
    assert_eq!(point["hits"][0]["inside"], true);
    for bad in [
        json!({"kind":"ray","origin":[0,1],"direction":[0,0,1]}),
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"filter":{"layers":["ghost"]}}),
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"filter":{"exclude":["nobody"]}}),
        json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1],"limit":0}),
    ] {
        assert!(
            physics::query_scene(project(), 0, 0.02, &request(bad)).is_err(),
            "must reject"
        );
    }
    assert!(
        physics::query_scene(
            project(),
            100_000,
            0.02,
            &request(json!({"kind":"ray","origin":[0,1,0],"direction":[0,0,1]}))
        )
        .is_err()
    );
    let debug = physics::debug_scene(
        project(),
        0,
        0.02,
        &physics::DebugViewOptions {
            max_items: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(debug["totals"]["colliders"], 5);
    assert_eq!(debug["colliders"].as_array().unwrap().len(), 2);
    assert_eq!(debug["truncated"], true);
    assert_eq!(debug["states"]["fixed"], 3);
    assert_eq!(debug["states"]["sensor"], 1);
    assert_eq!(debug["states"]["kinematic"], 1);
    assert!(debug["line_segments"].as_u64().unwrap() > 100);
}
