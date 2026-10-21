use bevy_ecs::prelude::{Component, Entity, Resource, World};
use engine_project::{
    content::*,
    gameplay2d::{self, Body, TriggerPhase},
    input::*,
    persistence::*,
    runtime::*,
    ui::*,
    *,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::Arc,
};

fn fixture(script: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("player.rs"), "pub fn configure() {}\n").unwrap();
    fs::write(
        dir.path().join("quad.mesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(dir.path().join("white.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
    let mut components = json!({"Transform":{"position":[0,0,0]},"SpriteRenderer":{"texture":"texture","size":[1,1]},"Collider2D":{"size":[1,1]}});
    if script {
        components["Script"] = json!({"asset":"player"});
    }
    let scene = json!({"schema":1,"scene_id":"room:a","entities":[{"id":"player","name":"Player","components":components},{"id":"camera","name":"Camera","components":{"Camera2D":{"follow":"player"}}}]});
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    fs::write(dir.path().join("project.json"),serde_json::to_vec(&json!({"schema":1,"name":"Test Project","startup_scene":"scene.json","inputActions":{"right":"Right"},"assets":[{"id":"player","kind":"script","path":"player.rs"},{"id":"mesh","kind":"mesh","path":"quad.mesh"},{"id":"texture","kind":"texture","path":"white.ppm"},{"id":"scene:a","kind":"scene","path":"scene.json"}]})).unwrap()).unwrap();
    dir
}
fn project(dir: &tempfile::TempDir) -> Project {
    Project::load(dir.path().join("project.json")).unwrap()
}
fn set_scene(dir: &tempfile::TempDir, scene: &Value) {
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(scene).unwrap(),
    )
    .unwrap();
}
fn body(position: [f32; 2]) -> Body {
    Body {
        position,
        ..Default::default()
    }
}

#[test]
fn schema_roundtrip_stable_ids_and_root_confinement() {
    let dir = fixture(false);
    let p = project(&dir);
    assert_eq!(p.input_bindings["right"], vec!["Key:Right"]);
    assert_eq!(p.scene.entities[0].id, "player");
    let data = serde_json::to_value(&p.scene).unwrap();
    assert_eq!(
        data["entities"][0]["components"]["SpriteRenderer"]["framesPerSecond"],
        0.0
    );
    assert!(schema::confined_file(&p.root, std::path::Path::new("../outside")).is_err());
    assert!(schema::confined_file(&p.root, &p.root.join("scene.json")).is_err());
}
#[test]
fn unknown_fields_versions_duplicate_ids_and_missing_transform_rejected() {
    let dir = fixture(false);
    let mut s = serde_json::to_value(&project(&dir).scene).unwrap();
    s["entities"][0]["components"]["Transform"]["version"] = json!(2);
    set_scene(&dir, &s);
    assert!(Project::load(dir.path().join("project.json")).is_err());
    s["entities"][0]["components"]["Transform"]["version"] = json!(1);
    s["entities"][0]["components"]["Transform"]["typo"] = json!(true);
    set_scene(&dir, &s);
    assert!(Project::load(dir.path().join("project.json")).is_err());
    s["entities"][0]["components"]
        .as_object_mut()
        .unwrap()
        .remove("Transform");
    set_scene(&dir, &s);
    assert!(Project::load(dir.path().join("project.json")).is_err());
}
#[test]
fn native_script_language_is_explicit() {
    let dir = fixture(false);
    let manifest = dir.path().join("project.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    fs::write(dir.path().join("old.lua"), "return {}").unwrap();
    value["assets"][0]["path"] = json!("old.lua");
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        Project::load(&manifest).unwrap_err().diagnostics[0].code,
        "project.script.language"
    );
}
#[test]
fn loaders_preserve_vertices_channels_and_reject_trailing_data() {
    let dir = fixture(false);
    assert_eq!(
        load_mesh(&dir.path().join("quad.mesh")).unwrap().indices,
        vec![0, 1, 2]
    );
    let t = load_texture(&dir.path().join("white.ppm")).unwrap();
    assert_eq!(t.pixels, vec![255; 4]);
    fs::write(dir.path().join("bad.ppm"), "P3 1 1 255 0 0 0 extra").unwrap();
    assert!(load_texture(&dir.path().join("bad.ppm")).is_err());
    let png = dir.path().join("texture.png");
    image::RgbaImage::from_raw(1, 1, vec![10, 20, 30, 40])
        .unwrap()
        .save(&png)
        .unwrap();
    assert_eq!(load_texture(&png).unwrap().pixels, vec![10, 20, 30, 40]);
}
#[test]
fn animation_letterbox_hidpi_and_atlas_bounds() {
    assert_eq!(animation_frame(1.0, 6.0, 4, true), 2);
    assert_eq!(animation_frame(100.0, 6.0, 4, false), 3);
    let v = camera_viewport([320, 180], [1000, 700]);
    assert_eq!(v.scale, 3.0);
    assert_eq!(v.x, 20.0);
    assert_eq!(
        logical_pointer([250.0, 175.0], [500, 350], [1000, 700], [320, 180]),
        [160.0, 90.0]
    );
    let t = Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    };
    assert!(sprite_quad([0.0; 3], [1.0; 2], [0.5; 2], [0, 0, 2, 1], &t, [1.0; 4]).is_err());
}
#[test]
fn font_metrics_unicode_fallback_and_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("font.json");
    fs::write(&path,serde_json::to_vec(&json!({"schema":1,"texture":"font-texture","lineHeight":10,"fallback":63,"glyphs":[{"codepoint":63,"source":[0,0,1,1],"advance":2,"bearing":[0,0]},{"codepoint":937,"source":[0,0,1,1],"advance":3,"bearing":[0,0]}]})).unwrap()).unwrap();
    let f = load_font(&path).unwrap();
    let t = Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    };
    f.validate(1, 1).unwrap();
    assert_eq!(f.glyph('Ω').advance, 3.0);
    assert_eq!(f.glyph('十').advance, 2.0);
    let mesh = text_mesh(&f, &t, "Ω十", [0.0; 2], 0.0, 1.0, [1.0; 4]).unwrap();
    assert_eq!(mesh.vertices.len(), 8);
    assert_eq!(mesh.vertices[4].position[0], 3.0);
}
#[test]
fn swept_collision_prevents_tunneling_and_slides() {
    let mut w = gameplay2d::World::default();
    w.upsert("actor", body([0.0, 0.0])).unwrap();
    w.upsert(
        "wall",
        Body {
            position: [3.0, 0.0],
            size: [1.0, 10.0],
            ..Default::default()
        },
    )
    .unwrap();
    let result = w.move_body("actor", [100.0, 2.0]).unwrap();
    assert_eq!(result.position, [2.0, 2.0]);
    assert_eq!(result.contacts, vec!["wall"]);
    assert!(w.is_safe(w.body("actor").unwrap(), "actor").unwrap());
}
#[test]
fn touching_and_corner_grazing_are_not_overlap() {
    let mut w = gameplay2d::World::default();
    w.upsert("actor", body([0.0, 0.0])).unwrap();
    w.upsert("wall", body([1.0, 1.0])).unwrap();
    assert!(w.overlaps("actor").unwrap().is_empty());
    let result = w.move_body("actor", [2.0, -2.0]).unwrap();
    assert_eq!(result.position, [2.0, -2.0]);
    assert!(result.contacts.is_empty());
}
#[test]
fn triggers_are_ordered_and_removal_emits_exit() {
    let mut w = gameplay2d::World::default();
    w.upsert("z", body([0.0; 2])).unwrap();
    w.upsert(
        "a",
        Body {
            trigger: true,
            ..body([0.0; 2])
        },
    )
    .unwrap();
    assert_eq!(w.advance_triggers()[0].phase, TriggerPhase::Enter);
    assert_eq!(w.advance_triggers()[0].first, "a");
    w.remove("z");
    assert_eq!(w.advance_triggers()[0].phase, TriggerPhase::Exit);
    assert!(w.advance_triggers().is_empty());
}
#[test]
fn safe_search_follow_and_flock_remain_bounded() {
    let mut w = gameplay2d::World::default();
    w.upsert("solid", body([0.0; 2])).unwrap();
    assert!(
        w.find_safe(body([0.0; 2]), 2.0, 1.0, "actor")
            .unwrap()
            .is_some()
    );
    assert!(w.find_safe(body([0.0; 2]), 100.0, 0.01, "actor").is_err());
    assert_eq!(
        gameplay2d::follow_velocity([0.0; 2], [1.0, 0.0], 2.0, 1.0).unwrap(),
        [0.0; 2]
    );
    assert_eq!(
        gameplay2d::flock_target([0.0; 2], 0, 4, 2.0).unwrap(),
        [2.0, 0.0]
    );
}
#[test]
fn actions_focus_and_rebind_quarantine_held_keys() {
    let bindings = BTreeMap::from([("move".into(), vec!["Key:A".into()])]);
    let mut mapper = ActionMapper::new(bindings.clone()).unwrap();
    let mut raw = RawInput {
        down: BTreeSet::from(["Key:A".into()]),
        ..Default::default()
    };
    assert!(mapper.sample(&raw).pressed("move"));
    raw.focused = false;
    assert!(mapper.sample(&raw).released("move"));
    raw.focused = true;
    assert!(!mapper.sample(&raw).held("move"));
    raw.down.clear();
    mapper.sample(&raw);
    raw.down.insert("Key:A".into());
    assert!(mapper.sample(&raw).pressed("move"));
    mapper.rebind(bindings, false);
    assert!(!mapper.sample(&raw).held("move"));
}
#[test]
fn gamepad_deadzone_disconnect_reconnect_suppresses_stuck_controls() {
    let mut mapper = ActionMapper::new(BTreeMap::from([(
        "move".into(),
        vec!["Gamepad:LeftXPositive".into()],
    )]))
    .unwrap();
    let mut raw = RawInput {
        gamepad_connected: true,
        axes: BTreeMap::from([("LeftX".into(), 0.6)]),
        ..Default::default()
    };
    assert!((mapper.sample(&raw).value("move") - 0.5).abs() < 1e-6);
    raw.gamepad_connected = false;
    assert!(mapper.sample(&raw).released("move"));
    raw.gamepad_connected = true;
    assert!(!mapper.sample(&raw).held("move"));
    raw.axes.insert("LeftX".into(), 0.0);
    mapper.sample(&raw);
    raw.axes.insert("LeftX".into(), 1.0);
    assert!(mapper.sample(&raw).pressed("move"));
}
#[test]
fn modal_capture_consumes_release_after_closing() {
    let mut ui = UiModel::default();
    let input = ActionFrame {
        pressed: BTreeSet::from(["interact".into()]),
        held: BTreeSet::from(["interact".into()]),
        ..Default::default()
    };
    ui.tick(input.clone());
    ui.open(PanelOptions {
        id: "panel".into(),
        text: "text".into(),
        ..Default::default()
    })
    .unwrap();
    assert!(!ui.gameplay_frame().held("interact"));
    ui.close("panel");
    ui.tick(input);
    assert!(!ui.gameplay_frame().held("interact"));
    ui.tick(ActionFrame {
        released: BTreeSet::from(["interact".into()]),
        ..Default::default()
    });
    assert!(!ui.gameplay_frame().released("interact"));
}
#[test]
fn ui_choice_navigation_scrolls_and_emits_events() {
    let mut ui = UiModel::default();
    ui.open(PanelOptions {
        id: "panel".into(),
        text: "hello".into(),
        choices: vec!["one".into(), "two".into()],
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
        },
        ..Default::default()
    })
    .unwrap();
    ui.tick(ActionFrame {
        pressed: BTreeSet::from(["ui_down".into()]),
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].selected, 1);
    assert!(ui.panels()[0].scroll_offset > 0.0);
    assert_eq!(ui.poll_event().unwrap().kind, "selection");
}
#[test]
fn document_revision_undo_save_and_play_do_not_mutate_authored_state() {
    let dir = fixture(false);
    let mut doc = SceneDocument::new(project(&dir));
    let first = doc.revision().to_owned();
    let second = doc
        .apply(
            EditOperation::SetPosition {
                entity_id: "player".into(),
                position: [2.0, 0.0, 0.0],
            },
            &first,
        )
        .unwrap();
    assert!(doc.is_dirty());
    assert!(
        doc.apply(
            EditOperation::SetPosition {
                entity_id: "player".into(),
                position: [3.0, 0.0, 0.0]
            },
            &first
        )
        .is_err()
    );
    let mut play = doc.begin_play(&second).unwrap();
    play.scene.entities[0]
        .components
        .transform
        .as_mut()
        .unwrap()
        .position = [99.0; 3];
    assert_eq!(
        doc.snapshot().entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        2.0
    );
    doc.end_play();
    doc.undo(&second).unwrap();
    assert!(!doc.is_dirty());
    let revision = doc.revision().to_owned();
    doc.redo(&revision).unwrap();
    let revision = doc.revision().to_owned();
    doc.save(&revision).unwrap();
    assert!(!doc.is_dirty());
    assert_eq!(
        project(&dir).scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        2.0
    );
}
#[test]
fn persistence_backup_recovery_and_failed_replace_retain_previous_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let options = PersistenceOptions::new(dir.path().to_owned(), "game".into());
    let store = PersistenceStore::new(options.clone()).unwrap();
    store
        .save("slot", json!({"n":1}).as_object().unwrap().clone(), None)
        .unwrap();
    store
        .save("slot", json!({"n":2}).as_object().unwrap().clone(), None)
        .unwrap();
    assert_eq!(store.load_backup("slot", None).unwrap()["n"], 1);
    let mut failed = options;
    failed.fail = Some(|stage| stage == PersistenceStage::Replace);
    let failed = PersistenceStore::new(failed).unwrap();
    assert!(
        failed
            .save("slot", json!({"n":3}).as_object().unwrap().clone(), None)
            .is_err()
    );
    assert_eq!(store.load("slot", None).unwrap()["n"], 2);
    fs::write(dir.path().join("game/save/slot.json"), b"corrupt").unwrap();
    assert!(store.load("slot", None).is_err());
    store.recover_backup("slot", None).unwrap();
    assert_eq!(store.load("slot", None).unwrap()["n"], 2);
    assert!(store.save("../bad", Default::default(), None).is_err());
}
#[derive(Resource, Default)]
struct Counters {
    create: u32,
    update: u32,
    destroy: u32,
}
struct Player;
impl NativeScript for Player {
    fn on_create(&mut self, w: &mut World, _: Entity) -> Result<()> {
        w.resource_mut::<Counters>().create += 1;
        Ok(())
    }
    fn on_update(&mut self, w: &mut World, e: Entity, dt: f32) -> Result<()> {
        w.resource_mut::<Counters>().update += 1;
        w.get_mut::<Transform>(e).unwrap().position[0] += 2.0 * dt;
        Ok(())
    }
    fn on_destroy(&mut self, w: &mut World, _: Entity) -> Result<()> {
        w.resource_mut::<Counters>().destroy += 1;
        Ok(())
    }
}
fn player(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Player))
}
#[test]
fn unregistered_scripts_fail_and_registered_lifecycle_executes_native_code() {
    let dir = fixture(true);
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    assert_eq!(
        runtime.start().unwrap_err().diagnostics[0].code,
        "script.asset.unregistered"
    );
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Counters::default());
    register_script(runtime.app_mut(), "player", player).unwrap();
    runtime.tick(0.5, ActionFrame::default()).unwrap();
    assert_eq!(runtime.position("player").unwrap()[0], 1.0);
    runtime.stop().unwrap();
    let c = runtime.app().world().resource::<Counters>();
    assert_eq!((c.create, c.update, c.destroy), (1, 1, 1));
    assert!(runtime.start().is_err());
}
#[test]
fn render_geometry_and_texture_arcs_are_stable_until_changed() {
    let dir = fixture(false);
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    let a = runtime.extract_frame().unwrap();
    let b = runtime.extract_frame().unwrap();
    assert!(Arc::ptr_eq(&a.draws[0].mesh, &b.draws[0].mesh));
    assert!(Arc::ptr_eq(
        a.draws[0].texture.as_ref().unwrap(),
        b.draws[0].texture.as_ref().unwrap()
    ));
    let entity = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(entity)
        .unwrap()
        .position[0] = 1.0;
    let c = runtime.extract_frame().unwrap();
    assert!(!Arc::ptr_eq(&a.draws[0].mesh, &c.draws[0].mesh));
    assert_eq!(c.camera.center, [1.0, 0.0]);
}
#[derive(Component)]
struct CustomCounter(u32);
#[test]
fn validated_scene_transition_retains_traveller_identity_and_custom_components() {
    let dir = fixture(false);
    let mut runtime = Runtime::new(project(&dir)).unwrap();
    let entity = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .insert(CustomCounter(42));
    assert!(
        runtime
            .request_scene("scene:a", "missing", "player")
            .is_err()
    );
    assert_eq!(runtime.entity("player"), Some(entity));
    runtime
        .request_scene("scene:a", "player", "player")
        .unwrap();
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert_eq!(runtime.entity("player"), Some(entity));
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<CustomCounter>(entity)
            .unwrap()
            .0,
        42
    );
    assert_eq!(runtime.snapshot().scene_revision, 1);
}
