use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::{events::DeliveredEvents, *};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;

fn entity(id: &str, components: Value) -> Value {
    json!({"id":id,"name":id,"components":components})
}
fn descriptor(name: &str, data_only: bool) -> Value {
    json!({"name":name,"version":1,"schema":{"type":"object","properties":{"hp":{"type":"integer","minimum":0,"maximum":100}},"required":["hp"],"additionalProperties":false},"defaults":{"hp":10},"dataOnly":data_only})
}
fn fixture(entities: Vec<Value>, descriptors: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("components.json"),
        serde_json::to_vec(&descriptors).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(dir.path().join("project.json"),serde_json::to_vec(&json!({"schema":1,"name":"meta-test","startup_scene":"scene.json","componentSchemas":"components.json","assets":[{"id":"mesh","kind":"mesh","path":"mesh.cgmesh"}]})).unwrap()).unwrap();
    dir
}
fn project(dir: &tempfile::TempDir) -> Project {
    Project::load(dir.path().join("project.json")).unwrap()
}
fn health_entity() -> Value {
    entity(
        "player",
        json!({"Transform":{"position":[0,0,0]},"custom":{"game.Health":{"version":1,"data":{"hp":10}}}}),
    )
}
#[derive(Component, Serialize, Deserialize)]
struct Health {
    hp: i32,
}
#[derive(Component, Deserialize)]
struct BrokenHealth {
    hp: i32,
}
impl Serialize for BrokenHealth {
    fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(format!(
            "cannot encode {}",
            self.hp
        )))
    }
}
#[test]
fn fallible_snapshot_reports_serialization_and_schema_errors_instead_of_losing_entities() {
    let dir = fixture(
        vec![health_entity()],
        vec![descriptor("game.Health", false)],
    );
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    register_component::<BrokenHealth>(runtime.app_mut(), "game.Health", 1).unwrap();
    runtime.start().unwrap();
    assert!(runtime.try_snapshot().is_err());
    assert!(!runtime.snapshot().diagnostics.is_empty());
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    register_component::<Health>(runtime.app_mut(), "game.Health", 1).unwrap();
    runtime.start().unwrap();
    let player = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Health>(player)
        .unwrap()
        .hp = 101;
    assert!(runtime.try_snapshot().is_err());
}

#[test]
fn typed_custom_component_hydrates_and_runtime_snapshot_tracks_ecs_changes() {
    let dir = fixture(
        vec![health_entity()],
        vec![descriptor("game.Health", false)],
    );
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    register_component::<Health>(runtime.app_mut(), "game.Health", 1).unwrap();
    runtime.start().unwrap();
    let player = runtime.entity("player").unwrap();
    assert_eq!(runtime.app().world().get::<Health>(player).unwrap().hp, 10);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Health>(player)
        .unwrap()
        .hp = 7;
    assert_eq!(
        runtime.snapshot().scene.entities[0].components.custom["game.Health"].data["hp"],
        7
    );
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(player)
        .remove::<Health>();
    assert!(
        runtime.snapshot().scene.entities[0]
            .components
            .custom
            .is_empty()
    );
}
#[test]
fn native_adapter_is_required_and_same_rust_type_cannot_alias_two_names() {
    let dir = fixture(
        vec![health_entity()],
        vec![
            descriptor("game.Health", false),
            descriptor("game.Other", false),
        ],
    );
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    assert!(runtime.start().is_err());
    register_component::<Health>(runtime.app_mut(), "game.Health", 1).unwrap();
    assert!(register_component::<Health>(runtime.app_mut(), "game.Other", 1).is_err());
}
#[test]
fn declarative_validation_rejects_bounds_unknown_fields_and_unsupported_schema() {
    let dir = fixture(vec![health_entity()], vec![descriptor("game.Health", true)]);
    let mut document = SceneDocument::new(project(&dir));
    for data in [
        json!({"hp":101}),
        json!({"hp":1,"typo":true}),
        json!({"hp":"many"}),
    ] {
        let before = document.revision().to_owned();
        assert!(
            document
                .apply(
                    EditOperation::SetComponent {
                        entity_id: "player".into(),
                        component: "game.Health".into(),
                        value: json!({"version":1,"data":data}),
                        merge: false
                    },
                    &before
                )
                .is_err()
        );
        assert_eq!(document.revision(), before);
    }
    let mut invalid = descriptor("game.Health", true);
    invalid["schema"]["$ref"] = json!("https://example.invalid/schema");
    assert!(ComponentCatalog::new(vec![serde_json::from_value(invalid).unwrap()]).is_err());
}
#[test]
fn hierarchy_transforms_meshes_colliders_and_world_space_movement() {
    let parent = entity(
        "parent",
        json!({"Transform":{"position":[10,0,0],"rotation":[0,0,0.70710677,0.70710677],"scale":[2,2,1]}}),
    );
    let mut child = entity(
        "child",
        json!({"Transform":{"position":[1,0,0]},"MeshRenderer":{"mesh":"mesh"},"Collider2D":{"size":[1,1]}}),
    );
    child["parent"] = json!("parent");
    let dir = fixture(vec![parent, child], vec![]);
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    runtime.start().unwrap();
    let position = runtime.position("child").unwrap();
    assert!((position[0] - 10.0).abs() < 0.001 && (position[1] - 2.0).abs() < 0.001);
    let frame = runtime.extract_frame().unwrap();
    let p = frame.draws[0].mesh.vertices[1].position;
    assert!((p[0] - 10.0).abs() < 0.001 && (p[1] - 4.0).abs() < 0.001);
    let body = runtime
        .app()
        .world()
        .resource::<gameplay2d::World>()
        .body("child")
        .unwrap();
    assert!((body.size[0] - 2.0).abs() < 0.001);
    runtime.move_entity("child", [2.0, 0.0]).unwrap();
    let p = runtime.position("child").unwrap();
    assert!((p[0] - 12.0).abs() < 0.001);
}
#[test]
fn world_transforms_propagate_after_update_before_native_extraction() {
    let dir = fixture(
        vec![entity("player", json!({"Transform":{"position":[0,0,0]}}))],
        vec![],
    );
    let mut r = Runtime::new(project(&dir)).unwrap();
    r.app_mut()
        .schedule_mut(engine_core::Stage::Update)
        .add_systems(|mut transforms: Query<&mut Transform>| {
            for mut t in &mut transforms {
                t.position[0] = 12.0;
            }
        });
    r.app_mut().extract_component::<WorldTransform>();
    r.tick(0.01, Default::default()).unwrap();
    let extracted = r
        .app()
        .render_world()
        .iter_entities()
        .find_map(|e| e.get::<WorldTransform>())
        .unwrap();
    assert_eq!(extracted.position(), [12.0, 0.0, 0.0]);
}
#[test]
fn transactional_batch_can_create_parent_before_reparent_and_undo_as_one_unit() {
    let dir = fixture(
        vec![entity("child", json!({"Transform":{"position":[1,0,0]}}))],
        vec![],
    );
    let mut d = SceneDocument::new(project(&dir));
    let before = d.revision().to_owned();
    let parent =
        serde_json::from_value(entity("parent", json!({"Transform":{"position":[0,0,0]}})))
            .unwrap();
    let after = d
        .apply(
            EditOperation::Batch {
                operations: vec![
                    EditOperation::Reparent {
                        entity_id: "child".into(),
                        parent: Some("parent".into()),
                    },
                    EditOperation::CreateEntity { entity: parent },
                ],
            },
            &before,
        )
        .unwrap();
    assert_eq!(d.snapshot().entities.len(), 2);
    assert_eq!(d.undo(&after).unwrap(), before);
    let bad = d.apply(
        EditOperation::Reparent {
            entity_id: "child".into(),
            parent: Some("child".into()),
        },
        &before,
    );
    assert!(bad.is_err());
    assert_eq!(d.revision(), before);
}
#[test]
fn nested_batches_share_one_total_budget() {
    let dir = fixture(
        vec![entity("child", json!({"Transform":{"position":[0,0,0]}}))],
        vec![],
    );
    let mut d = SceneDocument::new(project(&dir));
    let before = d.revision().to_owned();
    let op = EditOperation::SetPosition {
        entity_id: "child".into(),
        position: [1.0, 0.0, 0.0],
    };
    let batch = EditOperation::Batch {
        operations: vec![
            EditOperation::Batch {
                operations: vec![op.clone(); 600],
            },
            EditOperation::Batch {
                operations: vec![op; 600],
            },
        ],
    };
    assert!(d.apply(batch, &before).is_err());
    assert_eq!(d.revision(), before);
}
#[derive(Resource, Default)]
struct Calls(Vec<String>);
fn callback(world: &mut World, owner: Entity, event: &EngineEvent) -> Result<()> {
    world.resource_mut::<Calls>().0.push(event.name.clone());
    if event.name == "collision.enter" {
        emit_event(
            world,
            EngineEvent {
                name: "custom.followup".into(),
                source: Some(owner),
                target: Some(owner),
                payload: json!({}),
            },
        )?;
    }
    Ok(())
}
#[test]
fn actual_collision_invokes_callback_and_callback_emission_waits_another_tick() {
    let bindings = json!({"bindings":[{"event":"collision.enter","handler":"record"},{"event":"custom.followup","handler":"record"}]});
    let dir = fixture(
        vec![
            entity(
                "trigger",
                json!({"Transform":{"position":[0,0,0]},"Collider2D":{"size":[2,2],"trigger":true},"EventBindings":bindings}),
            ),
            entity(
                "other",
                json!({"Transform":{"position":[0,0,0]},"Collider2D":{"size":[1,1]}}),
            ),
        ],
        vec![],
    );
    let mut r = Runtime::new(project(&dir)).unwrap();
    r.app_mut().world_mut().insert_resource(Calls::default());
    register_event_handler(r.app_mut(), "record", callback).unwrap();
    r.tick(0.01, Default::default()).unwrap();
    assert!(r.app().world().resource::<Calls>().0.is_empty());
    r.tick(0.01, Default::default()).unwrap();
    assert_eq!(
        r.app().world().resource::<Calls>().0,
        vec!["collision.enter"]
    );
    r.tick(0.01, Default::default()).unwrap();
    assert_eq!(
        r.app().world().resource::<Calls>().0,
        vec!["collision.enter", "custom.followup"]
    );
    assert!(
        r.app()
            .world()
            .resource::<DeliveredEvents>()
            .0
            .iter()
            .any(|e| e.name == "custom.followup")
    );
}
fn despawn_self(world: &mut World, owner: Entity, _: &EngineEvent) -> Result<()> {
    world.resource_mut::<Calls>().0.push("once".into());
    world.despawn(owner);
    Ok(())
}
#[test]
fn despawn_in_callback_skips_remaining_callbacks_and_later_queued_events() {
    let dir = fixture(
        vec![entity(
            "player",
            json!({"EventBindings":{"bindings":[{"event":"die","handler":"remove"},{"event":"die","handler":"remove"}]}}),
        )],
        vec![],
    );
    let mut r = Runtime::new(project(&dir)).unwrap();
    r.app_mut().world_mut().insert_resource(Calls::default());
    register_event_handler(r.app_mut(), "remove", despawn_self).unwrap();
    let target = r.entity("player");
    for _ in 0..2 {
        emit_event(
            r.app_mut().world_mut(),
            EngineEvent {
                name: "die".into(),
                source: None,
                target,
                payload: json!({}),
            },
        )
        .unwrap();
    }
    r.tick(0.01, Default::default()).unwrap();
    assert_eq!(r.app().world().resource::<Calls>().0, vec!["once"]);
}
#[test]
fn event_capacity_is_explicit_and_shutdown_clears_pending_deliveries() {
    let dir = fixture(vec![], vec![]);
    let mut r = Runtime::new(project(&dir)).unwrap();
    r.app_mut().world_mut().insert_resource(EventLimits {
        queued: 1,
        payload_bytes: 8,
        deliveries_per_tick: 1,
    });
    let event = EngineEvent {
        name: "test".into(),
        source: None,
        target: None,
        payload: json!({}),
    };
    emit_event(r.app_mut().world_mut(), event.clone()).unwrap();
    assert!(emit_event(r.app_mut().world_mut(), event).is_err());
    r.stop().unwrap();
    assert!(r.app().world().resource::<DeliveredEvents>().0.is_empty());
}
#[test]
fn prefab_cloning_remaps_hierarchy_and_declared_custom_entity_references() {
    let mut desc = json!({"name":"game.Target","version":1,"schema":{"type":"object","properties":{"target":{"type":"string"}},"required":["target"],"additionalProperties":false},"defaults":{"target":"child"},"entityReferences":["/target"],"dataOnly":true});
    desc["defaults"] = json!({"target":"child"});
    let dir = fixture(vec![], vec![desc]);
    let mut child = entity("child", json!({"Transform":{"position":[1,0,0]}}));
    child["parent"] = json!("root");
    let root = entity(
        "root",
        json!({"Transform":{"position":[0,0,0]},"custom":{"game.Target":{"version":1,"data":{"target":"child"}}}}),
    );
    fs::write(
        dir.path().join("prefab.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"prefab","entities":[root,child]}))
            .unwrap(),
    )
    .unwrap();
    let manifest = dir.path().join("project.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"prefab","kind":"prefab","path":"prefab.json"}));
    fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut d = SceneDocument::new(project(&dir));
    let before = d.revision().to_owned();
    let after = d
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "prefab".into(),
                prefix: "copy:".into(),
                parent: None,
            },
            &before,
        )
        .unwrap();
    assert_eq!(
        d.snapshot().entities[1].parent.as_deref(),
        Some("copy:root")
    );
    assert_eq!(
        d.snapshot().entities[0].components.custom["game.Target"].data["target"],
        "copy:child"
    );
    assert!(
        d.apply(
            EditOperation::InstantiatePrefab {
                asset: "prefab".into(),
                prefix: "copy:".into(),
                parent: None
            },
            &after
        )
        .is_err()
    );
    assert_eq!(d.undo(&after).unwrap(), before);
}
