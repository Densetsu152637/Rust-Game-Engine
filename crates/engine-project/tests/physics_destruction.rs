//! Authored destruction: thresholds, fracture, debris pooling, sleeping, expiry and events.
use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{self, Destructible3D};
use engine_project::*;
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom,
        "EventBindings":{"bindings":[
            {"event":"physics.destruction.fractured","handler":"record"},
            {"event":"physics.destruction.debris_expired","handler":"record"}]}}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn parts(components: Vec<(&str, Value)>) -> Value {
    let mut map = serde_json::Map::new();
    for (name, data) in components {
        map.insert(name.into(), value(data));
    }
    Value::Object(map)
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
            &json!({"schema":1,"name":"destruction-test","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
#[derive(Resource, Default)]
struct Log(Vec<(String, Value)>);
fn record(world: &mut World, target: Entity, event: &EngineEvent) -> Result<()> {
    let id = world.get::<AuthoredId>(target).unwrap().id.clone();
    let mut payload = event.payload.clone();
    payload["target"] = json!(id);
    world
        .resource_mut::<Log>()
        .0
        .push((event.name.clone(), payload));
    Ok(())
}
fn started(entities: Vec<Value>) -> (tempfile::TempDir, Runtime) {
    let dir = fixture(entities);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}
fn rejection(entities: Vec<Value>) -> String {
    let dir = fixture(entities);
    let result = Project::load(dir.path().join("project.json"))
        .and_then(Runtime::new)
        .and_then(|mut runtime| {
            register_event_handler(runtime.app_mut(), "record", record)?;
            runtime.start()?;
            runtime.tick(0.02, InputSnapshot::default())
        });
    result.expect_err("scene must be rejected").to_string()
}
fn tick(runtime: &mut Runtime) {
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
}
fn ticks(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        tick(runtime);
    }
}
fn entity_of(runtime: &Runtime, id: &str) -> Entity {
    runtime.entity(id).unwrap()
}
fn events(runtime: &Runtime, name: &str) -> Vec<Value> {
    runtime
        .app()
        .world()
        .resource::<Log>()
        .0
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .collect()
}
fn zero_gravity() -> Value {
    settings(json!({"gravity_2d":[0.,0.],"gravity_3d":[0.,0.,0.]}))
}
fn mass(runtime: &Runtime, entity: Entity) -> f32 {
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    three.mass(physics::body_id(entity)).unwrap()
}
fn fragment_entities(runtime: &Runtime, owner: Entity) -> Vec<Entity> {
    physics::fragments(runtime.app().world(), owner)
        .into_iter()
        .map(|f| f.entity)
        .collect()
}
const FRACTURED: &str = "physics.destruction.fractured";
const EXPIRED: &str = "physics.destruction.debris_expired";

/// A 1 m box (1 kg at density 1) with a destructible component, and a ball flying at it.
fn target_and_ball(speed: f32, destructible: Value, box_motion: &str) -> Vec<Value> {
    vec![
        zero_gravity(),
        entity(
            "box",
            [0.; 3],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"motion":box_motion,"shape":"box","half_extents":[0.5,0.5,0.5]}),
                ),
                ("engine.Destructible3D", destructible),
            ]),
        ),
        entity(
            "ball",
            [2.5, 0., 0.],
            parts(vec![(
                "engine.PhysicsBody3D",
                json!({"shape":"ball","radius":0.25,"velocity":[-speed,0.,0.],"ccd":true,
                    "material":{"density":20.0}}),
            )]),
        ),
    ]
}

#[test]
fn a_fast_ball_fractures_a_destructible_box_and_a_slow_one_does_not() {
    let config = json!({"fragment_count":12,"threshold":3.0,"seed":7});
    let (_d, mut fast) = started(target_and_ball(40., config.clone(), "dynamic"));
    let owner = entity_of(&fast, "box");
    for _ in 0..40 {
        tick(&mut fast);
    }
    let fractured = events(&fast, FRACTURED);
    assert_eq!(
        fractured.len(),
        1,
        "exactly one fracture event: {fractured:?}"
    );
    let payload = &fractured[0];
    assert_eq!(payload["target"], "box");
    assert_eq!(payload["fragments"], 12);
    assert_eq!(payload["cause"], "threshold");
    let impulse = payload["impulse"].as_f64().unwrap();
    assert!(impulse >= 3., "impulse {impulse}");
    println!("fast hit: peak substep impulse {impulse:.2} N*s");
    assert!(
        fast.app()
            .world()
            .get::<physics::Fractured>(owner)
            .is_some()
    );
    let frags = fragment_entities(&fast, owner);
    assert_eq!(frags.len(), 12);
    // Mass is conserved to 1%: the box had 1 kg.
    let total: f32 = frags.iter().map(|f| mass(&fast, *f)).sum();
    assert!((total - 1.).abs() < 0.01, "fragment mass {total}");
    ticks(&mut fast, 100);
    assert_eq!(events(&fast, FRACTURED).len(), 1, "still exactly one event");
    assert!(physics::debris_count(fast.app().world(), None) <= 128);

    let (_d, mut slow) = started(target_and_ball(2., config, "dynamic"));
    let owner = entity_of(&slow, "box");
    ticks(&mut slow, 80);
    assert!(events(&slow, FRACTURED).is_empty());
    assert!(
        slow.app()
            .world()
            .get::<physics::Fractured>(owner)
            .is_none()
    );
    assert_eq!(physics::debris_count(slow.app().world(), None), 0);
    let status = physics::destructible_status(slow.app().world(), owner).unwrap();
    assert!(!status.fractured && status.threshold == 3.0);
}

#[test]
fn a_fixed_destructible_wall_fractures_under_a_fast_ball() {
    let config = json!({"fragment_count":8,"threshold":3.0});
    let (_d, mut runtime) = started(target_and_ball(40., config, "fixed"));
    ticks(&mut runtime, 30);
    assert_eq!(events(&runtime, FRACTURED).len(), 1);
    let owner = entity_of(&runtime, "box");
    let frags = fragment_entities(&runtime, owner);
    assert_eq!(frags.len(), 8);
    ticks(&mut runtime, 2);
    // A fixed body has no mass, so fragments take the material density (1 kg/m^3 x 1 m^3).
    let total: f32 = frags.iter().map(|f| mass(&runtime, *f)).sum();
    assert!((total - 1.).abs() < 0.01, "fragment mass {total}");
}

#[test]
fn accumulated_damage_fractures_a_box_that_no_single_hit_could() {
    let config = json!({"fragment_count":6,"threshold":1000.0,"health":0.6,"damage_floor":0.1});
    let (_d, mut runtime) = started(target_and_ball(4., config, "dynamic"));
    ticks(&mut runtime, 60);
    let fractured = events(&runtime, FRACTURED);
    assert_eq!(fractured.len(), 1);
    assert_eq!(fractured[0]["cause"], "damage");
}

#[test]
fn fragments_inherit_momentum_and_scatter_adds_none() {
    let config = json!({"fragment_count":10,"threshold":1e6,"seed":3,
        "debris":{"scatter_speed":3.0,"inherit_velocity":1.0,"lifetime":0.0}});
    let entities = vec![
        zero_gravity(),
        entity(
            "box",
            [0.; 3],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"shape":"box","half_extents":[0.5,0.5,0.5],
                        "velocity":[2.,1.,-1.],"angular_velocity":[0.,3.,0.],
                        "material":{"density":3.0}}),
                ),
                ("engine.Destructible3D", config),
            ]),
        ),
    ];
    let (_d, mut runtime) = started(entities);
    ticks(&mut runtime, 3);
    let owner = entity_of(&runtime, "box");
    let report = physics::fracture_now(
        runtime.app_mut().world_mut(),
        owner,
        Vec3::new(0.5, 0., 0.),
        4.,
    )
    .unwrap();
    assert_eq!(report.fragments.len(), 10);
    // The event is queued by `fracture_now` and delivered on the next tick, which also
    // creates the fragment bodies and steps them once.
    tick(&mut runtime);
    let payload = events(&runtime, FRACTURED).remove(0);
    assert_eq!(payload["cause"], "api");
    assert_eq!(payload["impulse"], 4.0);
    let parent_mass = payload["mass"].as_f64().unwrap() as f32;
    assert!((parent_mass - 3.).abs() < 1e-4, "{parent_mass}");
    let linear: Vec3 = serde_json::from_value::<[f32; 3]>(payload["linear_velocity"].clone())
        .unwrap()
        .into();
    let before = linear * parent_mass;
    let mut total_mass = 0.;
    let mut momentum = Vec3::ZERO;
    let mut fastest_relative = 0f32;
    for f in &report.fragments {
        let m = mass(&runtime, *f);
        let state = physics::body_state(runtime.app().world(), *f).unwrap();
        total_mass += m;
        momentum += state.linear_velocity * m;
        fastest_relative = fastest_relative.max((state.linear_velocity - linear).length());
    }
    assert!(
        (total_mass - parent_mass).abs() / parent_mass < 0.01,
        "mass {total_mass}"
    );
    let error = (momentum - before).length() / before.length();
    println!("momentum {momentum:?} vs {before:?}: relative error {error:.2e}");
    assert!(error < 0.01, "momentum error {error}");
    // The scatter really moved fragments apart (up to 3 m/s plus rotation).
    assert!(
        fastest_relative > 1.5,
        "fastest relative speed {fastest_relative}"
    );
}

#[test]
fn prefracture_caches_and_matches_on_demand_fragments() {
    let make = |prefracture: bool| {
        let config =
            json!({"fragment_count":9,"seed":42,"threshold":1e6,"prefracture":prefracture});
        vec![
            zero_gravity(),
            entity(
                "box",
                [0.; 3],
                parts(vec![
                    (
                        "engine.PhysicsBody3D",
                        json!({"shape":"box","half_extents":[0.5,0.25,0.75]}),
                    ),
                    ("engine.Destructible3D", config),
                ]),
            ),
        ]
    };
    let volumes = |runtime: &mut Runtime| {
        ticks(runtime, 2);
        let owner = entity_of(runtime, "box");
        let cached = physics::destructible_status(runtime.app().world(), owner)
            .unwrap()
            .prefractured;
        physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::ZERO, 1.).unwrap();
        let list = physics::fragments(runtime.app().world(), owner);
        (cached, list.iter().map(|f| f.volume).collect::<Vec<_>>())
    };
    let (_a, mut cached) = started(make(true));
    let (_b, mut on_demand) = started(make(false));
    let (was_cached, a) = volumes(&mut cached);
    let (was_cached_2, b) = volumes(&mut on_demand);
    assert!(was_cached && !was_cached_2);
    assert_eq!(a.len(), 9);
    assert_eq!(a, b);
    assert!(
        (a.iter().sum::<f32>() - 0.75).abs() < 1e-3,
        "box volume 1x0.5x1.5"
    );
}

#[test]
fn impact_pattern_makes_smaller_fragments_near_the_hit() {
    let config = json!({"fragment_count":40,"pattern":"impact","impact_radius":0.3,"threshold":1e6,"seed":9});
    let entities = vec![
        zero_gravity(),
        entity(
            "box",
            [0.; 3],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"shape":"box","half_extents":[0.5,0.5,0.5]}),
                ),
                ("engine.Destructible3D", config),
            ]),
        ),
    ];
    let (_d, mut runtime) = started(entities);
    ticks(&mut runtime, 2);
    let owner = entity_of(&runtime, "box");
    let hit = Vec3::new(0.5, 0., 0.);
    physics::fracture_now(runtime.app_mut().world_mut(), owner, hit, 1.).unwrap();
    let (mut near, mut far) = (vec![], vec![]);
    for f in physics::fragments(runtime.app().world(), owner) {
        let position = Vec3::from(
            world_transform(runtime.app().world(), f.entity)
                .unwrap()
                .position(),
        );
        if (position - hit).length() < 0.4 {
            near.push(f.volume);
        } else {
            far.push(f.volume);
        }
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    println!(
        "impact fragments: near {} mean {:.5}, far {} mean {:.5}",
        near.len(),
        mean(&near),
        far.len(),
        mean(&far)
    );
    assert!(mean(&near) < mean(&far));
}

#[test]
fn fragments_expose_render_meshes_with_flagged_fracture_surface() {
    let config = json!({"fragment_count":5,"threshold":1e6});
    let entities = vec![
        zero_gravity(),
        entity(
            "box",
            [0.; 3],
            parts(vec![
                ("engine.PhysicsBody3D", json!({"shape":"box"})),
                ("engine.Destructible3D", config),
            ]),
        ),
    ];
    let (_d, mut runtime) = started(entities);
    ticks(&mut runtime, 2);
    let owner = entity_of(&runtime, "box");
    let report =
        physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::ZERO, 1.).unwrap();
    for f in &report.fragments {
        let mesh = physics::fragment_mesh(runtime.app().world(), *f).unwrap();
        assert!(mesh.triangles.len() >= 4);
        assert_eq!(mesh.interior.len(), mesh.triangles.len());
        assert!(mesh.interior.iter().any(|&i| i));
        // Meshes are relative to the fragment origin (its centroid).
        let centre: Vec3 =
            mesh.positions.iter().copied().sum::<Vec3>() / mesh.positions.len() as f32;
        assert!(centre.length() < 0.5);
        assert!(
            runtime
                .entity(&format!(
                    "box#f{}",
                    physics::fragments(runtime.app().world(), owner)
                        .iter()
                        .find(|i| i.entity == *f)
                        .unwrap()
                        .index
                ))
                .is_some()
        );
    }
}

/// A fixed floor and a falling destructible box with a given debris configuration.
fn floor_scene(debris: Value, boxes: &[(&str, f32)], count: u32) -> Vec<Value> {
    let mut entities = vec![
        settings(json!({"gravity_3d":[0.,-9.81,0.]})),
        entity(
            "floor",
            [0., -0.5, 0.],
            parts(vec![(
                "engine.PhysicsBody3D",
                json!({"motion":"fixed","shape":"box","half_extents":[30.,0.5,30.]}),
            )]),
        ),
    ];
    for (id, x) in boxes {
        entities.push(entity(
            id,
            [*x, 0.5, 0.],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"shape":"box","half_extents":[0.5,0.5,0.5]}),
                ),
                (
                    "engine.Destructible3D",
                    json!({"fragment_count":count,"threshold":1e6,"debris":debris}),
                ),
            ]),
        ));
    }
    entities
}

#[test]
fn debris_goes_to_sleep_and_expires_after_its_lifetime() {
    let debris = json!({"lifetime":6.0,"sleep_velocity":0.5,"scatter_speed":0.5,"max_active":64});
    let (_d, mut runtime) = started(floor_scene(debris, &[("crate", 0.)], 10));
    ticks(&mut runtime, 2);
    let owner = entity_of(&runtime, "crate");
    physics::fracture_now(
        runtime.app_mut().world_mut(),
        owner,
        Vec3::new(0., 0.5, 0.),
        1.,
    )
    .unwrap();
    let frags = fragment_entities(&runtime, owner);
    assert_eq!(frags.len(), 10);
    // 3 s after the fracture every fragment has settled and been put to sleep.
    ticks(&mut runtime, 150);
    let (_, three) = physics::physics_worlds(runtime.app().world()).unwrap();
    let asleep = frags
        .iter()
        .filter(|f| three.is_sleeping(physics::body_id(**f)) == Ok(true))
        .count();
    println!("{asleep}/10 fragments asleep after 3 s");
    assert_eq!(asleep, 10);
    assert!(events(&runtime, EXPIRED).is_empty());
    // Lifetime 6 s: gone by 7 s, one event each, entities despawned.
    ticks(&mut runtime, 200);
    let expired = events(&runtime, EXPIRED);
    assert_eq!(expired.len(), 10);
    assert!(
        expired
            .iter()
            .all(|e| e["reason"] == "lifetime" && e["target"] == "crate")
    );
    assert!(expired.iter().all(|e| e["age"].as_f64().unwrap() >= 6.0));
    assert_eq!(physics::debris_count(runtime.app().world(), None), 0);
    for f in frags {
        assert!(runtime.app().world().get_entity(f).is_err());
    }
}

#[test]
fn the_debris_pool_never_exceeds_its_cap_and_recycles_oldest_or_smallest() {
    for (policy, seed_note) in [("oldest", "A first"), ("smallest", "smallest first")] {
        let debris = json!({"lifetime":0.0,"max_active":10,"recycle":policy,"pool":"shared",
            "sleep_velocity":0.0});
        let (_d, mut runtime) = started(floor_scene(debris, &[("a", -3.), ("b", 3.)], 8));
        ticks(&mut runtime, 2);
        let (a, b) = (entity_of(&runtime, "a"), entity_of(&runtime, "b"));
        physics::fracture_now(
            runtime.app_mut().world_mut(),
            a,
            Vec3::new(-3., 0.5, 0.),
            1.,
        )
        .unwrap();
        assert_eq!(
            physics::debris_count(runtime.app().world(), Some("shared")),
            8
        );
        let a_volumes: Vec<f32> = physics::fragments(runtime.app().world(), a)
            .iter()
            .map(|f| f.volume)
            .collect();
        physics::fracture_now(runtime.app_mut().world_mut(), b, Vec3::new(3., 0.5, 0.), 1.)
            .unwrap();
        // 16 fragments were made; the pool holds 10.
        assert_eq!(
            physics::debris_count(runtime.app().world(), Some("shared")),
            10,
            "{seed_note}"
        );
        for _ in 0..30 {
            tick(&mut runtime);
            assert!(physics::debris_count(runtime.app().world(), Some("shared")) <= 10);
        }
        let recycled = events(&runtime, EXPIRED);
        assert_eq!(recycled.len(), 6, "{policy}");
        assert!(recycled.iter().all(|e| e["reason"] == "recycled"));
        let survivors: Vec<f32> = [a, b]
            .iter()
            .flat_map(|o| physics::fragments(runtime.app().world(), *o))
            .map(|f| f.volume)
            .collect();
        assert_eq!(survivors.len(), 10);
        let gone: Vec<f32> = recycled
            .iter()
            .map(|e| e["volume"].as_f64().unwrap() as f32)
            .collect();
        if policy == "oldest" {
            // Box A's fragments are the oldest: six of them went, box B kept all eight.
            assert_eq!(physics::fragments(runtime.app().world(), b).len(), 8);
            assert_eq!(physics::fragments(runtime.app().world(), a).len(), 2);
            assert!(recycled.iter().all(|e| e["target"] == "a"));
            let _ = &a_volumes;
        } else {
            let largest_gone = gone.iter().copied().fold(0., f32::max);
            let smallest_kept = survivors.iter().copied().fold(f32::MAX, f32::min);
            println!(
                "smallest policy: largest recycled {largest_gone:.5}, smallest kept {smallest_kept:.5}"
            );
            assert!(largest_gone <= smallest_kept);
        }
    }
}

#[test]
fn component_edits_are_validated_and_the_threshold_api_works() {
    let entities = |destructible: Value, body_json: Value| {
        vec![
            zero_gravity(),
            entity(
                "box",
                [0.; 3],
                parts(vec![
                    ("engine.PhysicsBody3D", body_json),
                    ("engine.Destructible3D", destructible),
                ]),
            ),
        ]
    };
    let boxed = json!({"shape":"box"});
    for (bad, keyword) in [
        // Single-field bounds are caught by the component schema at load.
        (json!({"threshold":0.0}), "outside bounds"),
        (json!({"fragment_count":1}), "outside bounds"),
        (
            json!({"fragment_count":20,"debris":{"max_active":8}}),
            "max_active",
        ),
        (json!({"debris":{"lifetime":-1.0}}), "outside bounds"),
        (json!({"health":0.0}), "outside bounds"),
        // The cross-field rule is the feature's own check.
    ] {
        let message = rejection(entities(bad.clone(), boxed.clone()));
        println!("{bad} -> {message}");
        assert!(message.contains(keyword), "{bad}: {message}");
    }
    let message = rejection(entities(json!({}), json!({"shape":"ball"})));
    println!("ball body -> {message}");
    assert!(message.contains("box or convex_hull"), "{message}");
    // A Destructible3D without a body is refused by the schema (`requires`) or the feature.
    let missing = vec![
        zero_gravity(),
        entity(
            "box",
            [0.; 3],
            parts(vec![("engine.Destructible3D", json!({}))]),
        ),
    ];
    assert!(!rejection(missing).is_empty());
    // An authored mesh lets any body shape be fractured; check the tetrahedron.
    let tetra = json!({"fragment_count":4,"threshold":1e6,"mesh":{
        "vertices":[[0.,0.,0.],[1.,0.,0.],[0.,1.,0.],[0.,0.,1.]],
        "indices":[[0,2,1],[0,1,3],[0,3,2],[1,2,3]]}});
    let (_d, mut runtime) = started(entities(tetra, json!({"shape":"ball","radius":0.5})));
    ticks(&mut runtime, 2);
    let owner = entity_of(&runtime, "box");
    let report =
        physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::splat(0.2), 1.).unwrap();
    assert_eq!(report.fragments.len(), 4);
    let volume: f32 = physics::fragments(runtime.app().world(), owner)
        .iter()
        .map(|f| f.volume)
        .sum();
    assert!(
        (volume - 1. / 6.).abs() < 1e-4,
        "tetrahedron volume {volume}"
    );
    assert!(
        physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::ZERO, 1.)
            .unwrap_err()
            .to_string()
            .contains("already fractured")
    );

    let (_d, mut runtime) = started(entities(json!({"threshold":10.0}), boxed));
    ticks(&mut runtime, 2);
    let owner = entity_of(&runtime, "box");
    physics::set_fracture_threshold(runtime.app_mut().world_mut(), owner, 2.5).unwrap();
    assert_eq!(
        physics::destructible_status(runtime.app().world(), owner)
            .unwrap()
            .threshold,
        2.5
    );
    assert!(physics::set_fracture_threshold(runtime.app_mut().world_mut(), owner, -1.).is_err());
    let config: &Destructible3D = runtime.app().world().get(owner).unwrap();
    assert_eq!(config.threshold, 2.5);
    // The read-only preview reports the fragment volumes without touching the world.
    let body: &physics::PhysicsBody3D = runtime.app().world().get(owner).unwrap();
    let preview = physics::preview_fracture(config, body, Vec3::ONE).unwrap();
    assert_eq!(preview.len(), 12);
    assert!((preview.iter().sum::<f32>() - 1.).abs() < 1e-4);
}

#[test]
fn the_catalog_declares_the_component_with_its_defaults() {
    let dir = fixture(vec![]);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let descriptor = &project.component_catalog.0["engine.Destructible3D"];
    assert_eq!(
        descriptor.requires,
        vec!["engine.PhysicsBody3D".to_string()]
    );
    assert_eq!(descriptor.defaults["fragment_count"], 12);
    assert_eq!(descriptor.defaults["debris"]["pool"], "default");
    assert_eq!(
        descriptor.schema["properties"]["pattern"]["enum"],
        json!(["uniform", "impact"])
    );
}

// ---- Critic fixes: fractured owners, partial spawns, event pressure ------------------------

fn joint_status(runtime: &Runtime, entity: Entity) -> physics::JointStatus {
    physics::joint_status(runtime.app().world(), entity, physics::Dimension::Three).unwrap()
}
fn wall_at(x: f32, extra: Vec<(&str, Value)>) -> Value {
    let mut components = vec![(
        "engine.PhysicsBody3D",
        json!({"motion":"fixed","half_extents":[0.5,0.5,0.5]}),
    )];
    components.extend(extra);
    entity("wall", [x, 0., 0.], parts(components))
}
fn door(extra: Vec<(&str, Value)>) -> Value {
    let mut components = vec![
        (
            "engine.PhysicsBody3D",
            json!({"half_extents":[0.5,0.5,0.5]}),
        ),
        (
            "engine.PhysicsJoint3D",
            json!({"kind":"spherical","body_b":"wall",
                "anchor_a":[1.5,0.,0.],"anchor_b":[-1.5,0.,0.]}),
        ),
    ];
    components.extend(extra);
    entity("door", [0.; 3], parts(components))
}
fn destructible() -> (&'static str, Value) {
    (
        "engine.Destructible3D",
        json!({"fragment_count":6,"threshold":1e9}),
    )
}

/// H1: a joint to a fractured body (either endpoint) is inactive, not a scene error: the
/// simulation keeps running.
#[test]
fn a_joint_to_a_fractured_body_is_removed_without_stopping_the_simulation() {
    for fractured_side in ["door", "wall"] {
        let (door, wall) = if fractured_side == "door" {
            (door(vec![destructible()]), wall_at(3., vec![]))
        } else {
            (door(vec![]), wall_at(3., vec![destructible()]))
        };
        let (_d, mut runtime) = started(vec![zero_gravity(), wall, door]);
        let (door, wall) = (entity_of(&runtime, "door"), entity_of(&runtime, "wall"));
        ticks(&mut runtime, 3);
        assert_eq!(joint_status(&runtime, door), physics::JointStatus::Active);
        let target = if fractured_side == "door" { door } else { wall };
        physics::fracture_now(runtime.app_mut().world_mut(), target, Vec3::ZERO, 1.).unwrap();
        // Used to fail validation on every tick from here on.
        ticks(&mut runtime, 5);
        assert_eq!(
            joint_status(&runtime, door),
            physics::JointStatus::Inactive,
            "{fractured_side} fractured"
        );
        assert!(
            physics::broken_joints(runtime.app().world())
                .unwrap()
                .is_empty(),
            "removed, not broken"
        );
    }
}

/// H1: a destructible chassis and a destructible character body deactivate their vehicle and
/// controller when they fracture.
#[test]
fn a_fractured_vehicle_or_character_body_deactivates_its_controller() {
    let (_d, mut runtime) = started(vec![
        settings(json!({"gravity_3d":[0.,-9.81,0.]})),
        entity(
            "floor",
            [0., -0.5, 0.],
            parts(vec![(
                "engine.PhysicsBody3D",
                json!({"motion":"fixed","half_extents":[50.,0.5,50.]}),
            )]),
        ),
        entity(
            "car",
            [0., 0.6, 0.],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"half_extents":[0.9,0.3,2.0],"material":{"density":350.}}),
                ),
                ("engine.Vehicle3D", json!({})),
                destructible(),
            ]),
        ),
        entity(
            "hero",
            [10., 2., 0.],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"motion":"kinematic","shape":"box","half_extents":[0.3,0.9,0.3]}),
                ),
                ("engine.CharacterController3D", json!({})),
                destructible(),
            ]),
        ),
    ]);
    let (car, hero) = (entity_of(&runtime, "car"), entity_of(&runtime, "hero"));
    ticks(&mut runtime, 5);
    assert!(physics::vehicle_state(runtime.app().world(), car).is_ok());
    assert!(physics::character_state(runtime.app().world(), hero).is_ok());
    for target in [car, hero] {
        physics::fracture_now(runtime.app_mut().world_mut(), target, Vec3::ZERO, 1.).unwrap();
    }
    ticks(&mut runtime, 5);
    assert!(physics::vehicle_state(runtime.app().world(), car).is_err());
    assert!(physics::character_state(runtime.app().world(), hero).is_err());
}

/// M3: a fracture whose fragment densities fall outside the accepted range fails before
/// anything spawns: the intact body stays and no partial debris is left next to it.
#[test]
fn a_fracture_that_cannot_complete_spawns_nothing() {
    // The body is as dense as a body can be (1e4 kg/m^3), so any fragment whose hull volume
    // is a hair smaller than its volume share exceeds the accepted density.
    let mut refused = 0;
    for seed in 1..=24u64 {
        let (_d, mut runtime) = started(vec![
            zero_gravity(),
            entity(
                "box",
                [0.; 3],
                parts(vec![
                    (
                        "engine.PhysicsBody3D",
                        json!({"half_extents":[0.5,0.5,0.5],"material":{"density":10000.0}}),
                    ),
                    (
                        "engine.Destructible3D",
                        json!({"fragment_count":16,"threshold":1e9,"seed":seed}),
                    ),
                ]),
            ),
        ]);
        let owner = entity_of(&runtime, "box");
        ticks(&mut runtime, 2);
        let before = runtime.app().world().iter_entities().count();
        let result = physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::ZERO, 1.);
        let world = runtime.app().world();
        if let Err(error) = result {
            refused += 1;
            assert!(error.to_string().contains("density"), "{error}");
            assert_eq!(world.iter_entities().count(), before, "seed {seed}");
            assert_eq!(physics::debris_count(world, None), 0, "seed {seed}");
            assert!(world.get::<physics::Fractured>(owner).is_none());
            assert!(world.get::<physics::PhysicsBody3D>(owner).is_some());
            ticks(&mut runtime, 3);
        }
    }
    println!("{refused}/24 seeds were refused for density");
    assert!(refused > 0, "no seed reached the density error");
}

/// S1/L4: a 256-fragment fracture whose cells touch at spawn must not abort the step through
/// event overflow, and its fragments emit no contact events between themselves.
#[test]
fn a_large_fracture_neither_aborts_the_step_nor_floods_sibling_contact_events() {
    let (_d, mut runtime) = started(vec![
        zero_gravity(),
        entity(
            "box",
            [0.; 3],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"half_extents":[0.5,0.5,0.5]}),
                ),
                (
                    "engine.Destructible3D",
                    json!({"fragment_count":256,"threshold":1e9,"debris":{"max_active":256,
                        "scatter_speed":0.0,"lifetime":0.0}}),
                ),
            ]),
        ),
    ]);
    let owner = entity_of(&runtime, "box");
    ticks(&mut runtime, 2);
    physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::ZERO, 1.).unwrap();
    let mut sibling_contacts = 0;
    let mut other_events = 0;
    for _ in 0..4 {
        // Used to fail with "Event queue capacity exceeded" on the first tick.
        tick(&mut runtime);
        for event in runtime.snapshot().events {
            if event.name == "physics.contact.start" || event.name == "physics.contact.stop" {
                sibling_contacts += 1;
            } else {
                other_events += 1;
            }
        }
    }
    assert_eq!(physics::debris_count(runtime.app().world(), None), 256);
    println!("sibling contact events {sibling_contacts}, other events {other_events}");
    assert_eq!(sibling_contacts, 0);
    let pressure = physics::pressure_status(runtime.app().world()).unwrap();
    assert_eq!(pressure.events_dropped, 0, "{pressure:?}");
}

/// L4: running out of body capacity at runtime (here a fracture whose fragments do not fit)
/// leaves the extra fragments without a body, reports it once, and never fails a tick.
#[test]
fn running_out_of_body_capacity_degrades_gracefully() {
    let mut entities = vec![
        settings(json!({"gravity_3d":[0.,0.,0.]})),
        entity(
            "box",
            [0.; 3],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"half_extents":[0.5,0.5,0.5]}),
                ),
                (
                    "engine.Destructible3D",
                    json!({"fragment_count":24,"threshold":1e9,
                        "debris":{"max_active":24,"lifetime":0.0,"scatter_speed":0.0}}),
                ),
            ]),
        ),
    ];
    entities[0]["components"]["EventBindings"] =
        json!({"bindings":[{"event":"physics.capacity.exceeded","handler":"record"}]});
    for i in 0..10 {
        entities.push(entity(
            &format!("rock{i}"),
            [5. + 3. * i as f32, 0., 0.],
            parts(vec![(
                "engine.PhysicsBody3D",
                json!({"shape":"ball","radius":0.5}),
            )]),
        ));
    }
    let dir = fixture(entities);
    let mut runtime = Runtime::new_with_simulation(
        Project::load(dir.path().join("project.json")).unwrap(),
        Some(engine_project::simulation::SimulationConfig {
            body_capacity: 20,
            ..Default::default()
        }),
    )
    .unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    let owner = entity_of(&runtime, "box");
    ticks(&mut runtime, 3);
    let status = physics::pressure_status(runtime.app().world()).unwrap();
    assert_eq!((status.bodies_3d, status.starved_bodies), (11, 0));
    physics::fracture_now(runtime.app_mut().world_mut(), owner, Vec3::ZERO, 1.).unwrap();
    // Used to fail every tick from here on with "body capacity exceeded".
    ticks(&mut runtime, 4);
    let status = physics::pressure_status(runtime.app().world()).unwrap();
    println!("{status:?}");
    // 10 rocks + 24 fragments want 34 bodies in a world of 20.
    assert_eq!(status.bodies_3d, 20);
    assert_eq!(status.starved_bodies, 14);
    assert_eq!(physics::debris_count(runtime.app().world(), None), 24);
    let warnings = events(&runtime, "physics.capacity.exceeded");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["capacity"], 20);
    assert_eq!(warnings[0]["starved"], 14);
    // Removing rocks makes room: starved fragments get their bodies on the next sync, up to
    // the capacity of 20.
    for i in 0..10 {
        let rock = entity_of(&runtime, &format!("rock{i}"));
        runtime.app_mut().world_mut().despawn(rock);
    }
    ticks(&mut runtime, 3);
    let status = physics::pressure_status(runtime.app().world()).unwrap();
    assert_eq!(
        (status.bodies_3d, status.starved_bodies),
        (20, 4),
        "{status:?}"
    );
    assert_eq!(events(&runtime, "physics.capacity.exceeded").len(), 1);
}

const REFUSED: &str = "physics.destruction.refused";

/// An impact (or resting load) whose fracture cannot complete is refused with a status reason
/// and one event instead of aborting the step on every later tick.
#[test]
fn a_refused_impact_fracture_reports_once_and_never_aborts_the_step() {
    let mut refused_seeds = 0;
    for seed in 1..=24u64 {
        let mut crate_ = entity(
            "box",
            [0., 0.51, 0.],
            parts(vec![
                (
                    "engine.PhysicsBody3D",
                    json!({"half_extents":[0.5,0.5,0.5],"material":{"density":10000.0}}),
                ),
                (
                    "engine.Destructible3D",
                    json!({"fragment_count":16,"threshold":1.0,"threshold_mode":"force",
                        "seed":seed}),
                ),
            ]),
        );
        crate_["components"]["EventBindings"] = json!({"bindings":[
            {"event":REFUSED,"handler":"record"},{"event":FRACTURED,"handler":"record"}]});
        let (_d, mut runtime) = started(vec![
            settings(json!({"gravity_3d":[0.,-9.81,0.]})),
            entity(
                "floor",
                [0., -0.5, 0.],
                parts(vec![(
                    "engine.PhysicsBody3D",
                    json!({"motion":"fixed","half_extents":[5.,0.5,5.]}),
                )]),
            ),
            crate_,
        ]);
        let owner = entity_of(&runtime, "box");
        // The box rests on the floor under its own weight (about 98 kN): every tick is a
        // force-mode trigger. No tick may fail.
        ticks(&mut runtime, 40);
        let refused = events(&runtime, REFUSED);
        let fractured = events(&runtime, FRACTURED);
        let status = physics::destructible_status(runtime.app().world(), owner).unwrap();
        if fractured.is_empty() {
            refused_seeds += 1;
            assert_eq!(
                refused.len(),
                1,
                "seed {seed}: one refusal, no loop: {refused:?}"
            );
            assert!(
                refused[0]["reason"].as_str().unwrap().contains("density"),
                "{refused:?}"
            );
            assert_eq!(status.refused.as_deref(), refused[0]["reason"].as_str());
            assert!(!status.fractured);
        } else {
            assert!(refused.is_empty(), "seed {seed}: {refused:?}");
            assert!(status.refused.is_none());
        }
    }
    println!("{refused_seeds}/24 seeds refused");
    assert!(refused_seeds > 0, "no seed reached the density refusal");
}
