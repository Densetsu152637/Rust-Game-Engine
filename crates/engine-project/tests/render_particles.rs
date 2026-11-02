//! Authored GPU particles: `ParticleEmitter` validation, schema documentation,
//! document edits (what MCP `component_set` applies) and the per-tick
//! `ParticleFrame` extraction that drives the renderer.
use engine_project::particles::{MAX_EMITTER_CAPACITY, MAX_PARTICLE_EMITTERS, ParticleBlend3D};
use engine_project::{ActionFrame, EditOperation, Project, Runtime, Scene, SceneDocument};
use serde_json::json;
use std::fs;

fn fixture(entities: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"particles","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

fn scene_with(components: serde_json::Value) -> Result<Scene, String> {
    let scene: Scene = serde_json::from_value(json!({"schema":1,"scene_id":"s","entities":[
        {"id":"e","name":"E","components":components}]}))
    .map_err(|e| e.to_string())?;
    scene.validate().map_err(|e| e.to_string())?;
    Ok(scene)
}

fn emitter_entity(id: &str, position: [f32; 3], emitter: serde_json::Value) -> serde_json::Value {
    json!({"id": id, "name": id, "components":
        {"Transform": {"position": position}, "ParticleEmitter": emitter}})
}

#[test]
fn the_component_validates_and_is_documented() {
    scene_with(json!({"Transform":{"position":[0,1,0]},"ParticleEmitter":{}})).unwrap();
    scene_with(json!({"Transform":{"position":[0,1,0]},"ParticleEmitter":{
        "capacity": 64, "spawn_rate": 12.5, "lifetime": [0.5, 1.0], "blend": "additive",
        "start_color": [4.0, 2.0, 1.0, 1.0], "seed": 4_000_000_000u32}}))
    .unwrap();

    let transform = json!({"position":[0,0,0]});
    for (name, emitter) in [
        ("zero capacity", json!({"capacity": 0})),
        (
            "capacity over the limit",
            json!({"capacity": MAX_EMITTER_CAPACITY + 1}),
        ),
        ("negative rate", json!({"spawn_rate": -1.0})),
        ("zero lifetime", json!({"lifetime": [0.0, 1.0]})),
        ("negative lifetime", json!({"lifetime": [-1.0, 1.0]})),
        ("inverted lifetime", json!({"lifetime": [2.0, 1.0]})),
        ("negative speed", json!({"speed": [-1.0, 1.0]})),
        ("wide spread", json!({"spread": 2.0})),
        ("zero direction", json!({"direction": [0, 0, 0]})),
        ("negative size", json!({"size": [-1.0, 1.0]})),
        ("alpha above one", json!({"start_color": [1, 1, 1, 2]})),
        ("unknown field", json!({"sparkle": true})),
        ("unknown blend", json!({"blend": "multiply"})),
        ("future version", json!({"version": 2})),
    ] {
        assert!(
            scene_with(json!({"Transform": transform, "ParticleEmitter": emitter})).is_err(),
            "{name} must be rejected"
        );
    }
    // A Transform is required.
    assert!(scene_with(json!({"ParticleEmitter": {}})).is_err());

    let docs = engine_project::component_docs();
    assert!(engine_project::COMPONENT_NAMES.contains(&"ParticleEmitter"));
    let doc = docs.iter().find(|d| d.name == "ParticleEmitter").unwrap();
    assert_eq!(doc.requires, &["Transform"]);
    assert_eq!(doc.schema["additionalProperties"], false);
    for field in [
        "capacity",
        "spawn_rate",
        "lifetime",
        "speed",
        "spread",
        "gravity",
        "drag",
        "size",
        "start_color",
        "end_color",
        "blend",
        "seed",
    ] {
        assert!(doc.schema["properties"][field].is_object(), "{field}");
    }
    scene_with(json!({"Transform": transform, "ParticleEmitter": doc.example.clone()})).unwrap();
}

#[test]
fn component_set_edits_apply_and_reject_invalid_values() {
    let dir = fixture(json!([emitter_entity(
        "fountain",
        [0.0, 0.0, 0.0],
        json!({})
    )]));
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |value: serde_json::Value, merge: bool| EditOperation::SetComponent {
        entity_id: "fountain".into(),
        component: "ParticleEmitter".into(),
        value,
        merge,
    };
    let revision = document.revision().to_owned();
    let revision = document
        .apply(
            set(json!({"capacity": 256, "blend": "additive"}), true),
            &revision,
        )
        .unwrap();
    let emitter = document.snapshot().entities[0]
        .components
        .particle_emitter
        .clone()
        .unwrap();
    assert_eq!(emitter.capacity, 256);
    assert_eq!(emitter.blend, ParticleBlend3D::Additive);
    let error = document
        .apply(set(json!({"capacity": 0}), true), &revision)
        .unwrap_err();
    assert!(error.to_string().contains("ParticleEmitter"), "{error}");
    document.save(&revision).unwrap();
}

#[test]
fn the_particle_frame_carries_the_scene_tick_and_the_enabled_emitters() {
    let dir = fixture(json!([
        emitter_entity("b_smoke", [1.0, 2.0, 3.0], json!({"capacity": 128})),
        emitter_entity("a_spark", [-4.0, 0.0, 0.5], json!({"capacity": 64, "seed": 9})),
        emitter_entity("c_off", [0.0, 0.0, 0.0], json!({"enabled": false})),
        {"id": "plain", "name": "Plain", "components": {"Transform": {"position": [0, 0, 0]}}},
    ]));
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();

    // Before the first tick there is no step to simulate.
    let before = runtime.extract_particles().unwrap();
    assert_eq!((before.tick, before.dt), (0, 0.0));
    let ids: Vec<_> = before.emitters.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["a_spark", "b_smoke"], "enabled emitters in ID order");

    runtime.tick(0.05, ActionFrame::default()).unwrap();
    let first = runtime.extract_particles().unwrap();
    assert_eq!((first.tick, first.dt), (1, 0.05));
    // Extracting again without a tick repeats the frame: the same tick id.
    assert_eq!(runtime.extract_particles().unwrap(), first);
    // Rendering-side extraction does not advance the particle clock either.
    runtime.extract_scene_frame().unwrap();
    assert_eq!(runtime.extract_particles().unwrap().tick, 1);

    let smoke = &first.emitters[1];
    assert_eq!(smoke.emitter.capacity, 128);
    assert_eq!(
        &smoke.world[3][..3],
        &[1.0, 2.0, 3.0],
        "position in column 3"
    );
    assert_eq!(first.emitters[0].emitter.seed, 9);

    runtime.tick(0.02, ActionFrame::default()).unwrap();
    let second = runtime.extract_particles().unwrap();
    assert_eq!((second.tick, second.dt), (2, 0.02));
}

#[test]
fn emitter_budgets_are_enforced_at_extraction() {
    let many: Vec<_> = (0..=MAX_PARTICLE_EMITTERS)
        .map(|i| emitter_entity(&format!("e{i:02}"), [0.0; 3], json!({"capacity": 8})))
        .collect();
    let dir = fixture(serde_json::Value::Array(many));
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let error = runtime.extract_particles().unwrap_err();
    assert!(error.to_string().contains("at most"), "{error}");

    let big: Vec<_> = (0..5)
        .map(|i| {
            emitter_entity(
                &format!("big{i}"),
                [0.0; 3],
                json!({"capacity": MAX_EMITTER_CAPACITY}),
            )
        })
        .collect();
    let dir = fixture(serde_json::Value::Array(big));
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    assert!(
        runtime.extract_particles().is_err(),
        "total capacity budget"
    );
}
