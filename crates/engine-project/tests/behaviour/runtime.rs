//! Runtime lifecycle: stable authored IDs, native scripts, ownership cleanup,
//! input gating, collision synchronisation, scene transitions and services.
use super::common::*;
use bevy_ecs::prelude::{Component, Entity, Query, Res, ResMut, Resource, World};
use engine_core::Stage;
use engine_project::{
    ActionFrame, ActionMapper, InputSnapshot, NativeScript, RawInput, Result, Runtime, Script,
    Transform, gameplay2d,
    runtime::*,
    ui::{PanelOptions, UiModel},
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicUsize, Ordering},
};

fn id_of(w: &World, e: Entity) -> String {
    w.get::<AuthoredId>(e)
        .map(|a| a.id.clone())
        .unwrap_or_default()
}

fn note(w: &mut World, kind: &str, e: Entity) {
    let line = format!("{kind}:{}", id_of(w, e));
    w.resource_mut::<RuntimeLog>().0.push(line);
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn pressed(items: &[&str]) -> ActionFrame {
    ActionFrame {
        pressed: names(items),
        held: names(items),
        ..Default::default()
    }
}

fn logs(rt: &Runtime) -> Vec<String> {
    rt.snapshot().logs
}

fn live(rt: &Runtime) -> usize {
    rt.snapshot().live_entities
}

fn tick(rt: &mut Runtime) {
    rt.tick(0.1, ActionFrame::default()).unwrap();
}

// ---- scripts ----------------------------------------------------------------------------

struct Logger;
impl NativeScript for Logger {
    fn on_create(&mut self, w: &mut World, e: Entity) -> Result<()> {
        note(w, "create", e);
        Ok(())
    }
    fn on_update(&mut self, w: &mut World, e: Entity, _: f32) -> Result<()> {
        note(w, "update", e);
        Ok(())
    }
    fn on_destroy(&mut self, w: &mut World, e: Entity) -> Result<()> {
        note(w, "destroy", e);
        Ok(())
    }
}
fn logger(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Logger))
}

struct OtherLogger;
impl NativeScript for OtherLogger {
    fn on_create(&mut self, w: &mut World, e: Entity) -> Result<()> {
        note(w, "other-create", e);
        Ok(())
    }
    fn on_update(&mut self, w: &mut World, e: Entity, _: f32) -> Result<()> {
        note(w, "other-update", e);
        Ok(())
    }
    fn on_destroy(&mut self, w: &mut World, e: Entity) -> Result<()> {
        note(w, "other-destroy", e);
        Ok(())
    }
}
fn other_logger(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(OtherLogger))
}

struct Failer;
impl NativeScript for Failer {
    fn on_update(&mut self, _: &mut World, _: Entity, _: f32) -> Result<()> {
        Err(engine_project::Error::new(
            "fixture.update.failed",
            "deliberate",
        ))
    }
    fn on_destroy(&mut self, w: &mut World, e: Entity) -> Result<()> {
        note(w, "destroy-failer", e);
        Ok(())
    }
}
fn failer(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Failer))
}

enum PanicAt {
    Create,
    Update,
    Destroy,
}
struct Panicker(PanicAt);
impl NativeScript for Panicker {
    fn on_create(&mut self, _: &mut World, _: Entity) -> Result<()> {
        if matches!(self.0, PanicAt::Create) {
            panic!("deliberate create panic");
        }
        Ok(())
    }
    fn on_update(&mut self, _: &mut World, _: Entity, _: f32) -> Result<()> {
        if matches!(self.0, PanicAt::Update) {
            panic!("deliberate update panic");
        }
        Ok(())
    }
    fn on_destroy(&mut self, _: &mut World, _: Entity) -> Result<()> {
        if matches!(self.0, PanicAt::Destroy) {
            panic!("deliberate destroy panic");
        }
        Ok(())
    }
}
fn panic_create(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Panicker(PanicAt::Create)))
}
fn panic_update(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Panicker(PanicAt::Update)))
}
fn panic_destroy(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Panicker(PanicAt::Destroy)))
}

#[derive(Component)]
struct Tag(u32);

struct Spawner {
    done: bool,
}
impl NativeScript for Spawner {
    fn on_update(&mut self, w: &mut World, owner: Entity, _: f32) -> Result<()> {
        if !self.done {
            self.done = true;
            let child = w.spawn(Tag(1)).id();
            spawn_owned(w, child, Tag(2))?;
            w.spawn(Tag(3));
            let _ = owner;
        }
        Ok(())
    }
}
fn spawner(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Spawner { done: false }))
}

struct Requester {
    done: bool,
}
impl NativeScript for Requester {
    fn on_update(&mut self, w: &mut World, _: Entity, _: f32) -> Result<()> {
        if !self.done {
            self.done = true;
            request_scene(
                w,
                SceneRequest {
                    asset: "scene:b".into(),
                    spawn: "door_b".into(),
                    traveller: "player".into(),
                },
            )?;
        }
        Ok(())
    }
}
fn requester(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Requester { done: false }))
}

static DESTROYED: AtomicUsize = AtomicUsize::new(0);
struct DropProbe;
impl NativeScript for DropProbe {
    fn on_destroy(&mut self, _: &mut World, _: Entity) -> Result<()> {
        DESTROYED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn drop_probe(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(DropProbe))
}

struct Mover;
impl NativeScript for Mover {
    fn on_update(&mut self, w: &mut World, e: Entity, dt: f32) -> Result<()> {
        let input = w.resource::<InputSnapshot>().clone();
        let mut d = [0.0_f32; 2];
        if input.held("right") {
            d[0] += 1.0;
        }
        if input.held("left") {
            d[0] -= 1.0;
        }
        if input.held("up") {
            d[1] += 1.0;
        }
        if input.held("down") {
            d[1] -= 1.0;
        }
        let length = d[0].hypot(d[1]);
        if length > 0.0 {
            let id = id_of(w, e);
            move_entity(w, &id, [d[0] / length * 4.0 * dt, d[1] / length * 4.0 * dt])?;
        }
        Ok(())
    }
}
fn mover(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Mover))
}

// ---- project fixtures ---------------------------------------------------------------------

fn scripted(ids: &[&str]) -> Proj {
    let mut p = Proj::new();
    for id in ids {
        p.add_entity(ent(id, json!({"Script":{"asset":"player"}})));
    }
    p
}

fn rooms() -> Proj {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json(
            "room:b",
            vec![
                marker("door_b", 10.0, 0.0),
                wall("pillar", 10.0, 3.0, 1.0, 1.0),
            ],
        ),
    );
    p
}

fn walking() -> Proj {
    let mut p = Proj::new();
    p.manifest.as_object_mut().unwrap().remove("inputActions");
    p.manifest["inputBindings"] = json!({
        "left": ["Key:A"], "right": ["Key:D"], "up": ["Key:W"], "down": ["Key:S"]
    });
    p.set_player(json!({
        "Transform": {"position": [0, 0, 3]},
        "Collider2D": {"size": [1, 1]},
        "Script": {"asset": "player"}
    }));
    p
}

fn drive(rt: &mut Runtime, mapper: &mut ActionMapper, keys: &[&str], ticks: usize) {
    let raw = RawInput {
        down: names(keys),
        ..Default::default()
    };
    for _ in 0..ticks {
        let frame = mapper.sample(&raw);
        rt.tick(0.25, frame).unwrap();
    }
}

fn walker(p: &Proj) -> (Runtime, ActionMapper) {
    let project = p.project();
    let mapper = ActionMapper::new(project.input_bindings.clone()).unwrap();
    let mut rt = Runtime::new(project).unwrap();
    rt.register_script("player", mover).unwrap();
    (rt, mapper)
}

// ---- construction and identity -----------------------------------------------------------

#[test]
fn authored_entities_receive_their_persistent_ids_and_names() {
    let mut p = Proj::new();
    p.entity("player")["name"] = json!("The Hero");
    let rt = p.runtime();
    let entity = rt.entity("player").unwrap();
    let id = rt.app().world().get::<AuthoredId>(entity).unwrap();
    assert_eq!((id.id.as_str(), id.name.as_str()), ("player", "The Hero"));
}

#[test]
fn unknown_authored_ids_have_no_entity() {
    assert!(Proj::new().runtime().entity("ghost").is_none());
}

#[test]
fn distinct_authored_ids_map_to_distinct_handles() {
    let rt = Proj::new().runtime();
    assert_ne!(rt.entity("player"), rt.entity("camera"));
}

#[test]
fn authored_id_to_handle_mapping_is_stable_across_ticks() {
    let mut rt = Proj::new().runtime();
    let before = rt.entity("player");
    for _ in 0..5 {
        tick(&mut rt);
    }
    assert_eq!(rt.entity("player"), before);
}

#[test]
fn handles_are_runtime_values_while_authored_ids_survive_a_restart() {
    let p = Proj::new();
    let a = p.runtime();
    let b = p.runtime();
    assert_eq!(
        a.snapshot().scene.entities[1].id,
        b.snapshot().scene.entities[1].id
    );
    assert!(
        a.app()
            .world()
            .get::<AuthoredId>(a.entity("player").unwrap())
            .is_some()
    );
}

#[test]
fn entity_lookup_forgets_despawned_entities() {
    let mut rt = Proj::new().runtime();
    let e = rt.entity("player").unwrap();
    rt.app_mut().world_mut().despawn(e);
    assert!(rt.entity("player").is_none());
    assert!(find_entity(rt.app().world(), "player").is_none());
}

#[test]
fn position_reads_the_live_transform() {
    let mut rt = Proj::new().runtime();
    assert_eq!(rt.position("player"), Some([0.0, 0.0, 0.0]));
    let e = rt.entity("player").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(e)
        .unwrap()
        .position = [1.0, 2.0, 3.0];
    assert_eq!(rt.position("player"), Some([1.0, 2.0, 3.0]));
}

#[test]
fn entities_without_a_transform_have_no_position() {
    assert_eq!(Proj::new().runtime().position("camera"), None);
}

#[test]
fn fresh_runtime_snapshot_is_empty_of_activity() {
    let s = Proj::new().runtime().snapshot();
    assert_eq!((s.ticks, s.scene_revision, s.active_scripts), (0, 0, 0));
    assert_eq!(s.live_entities, 2);
    assert_eq!(s.scene.id, "room:a");
    assert!(s.triggers.is_empty() && s.logs.is_empty());
}

#[test]
fn snapshot_entities_are_sorted_by_authored_id() {
    let s = Proj::new().runtime().snapshot();
    let ids: Vec<_> = s.scene.entities.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["camera", "player"]);
}

#[test]
fn snapshot_reflects_live_edits_without_touching_the_authored_scene() {
    let mut rt = Proj::new().runtime();
    rt.move_entity("player", [2.0, 0.0]).unwrap();
    let snapshot = rt.snapshot();
    let moved = snapshot
        .scene
        .entities
        .iter()
        .find(|e| e.id == "player")
        .unwrap();
    assert_eq!(
        moved.components.transform.as_ref().unwrap().position[0],
        2.0
    );
    let authored = &rt
        .active_scene()
        .entities
        .iter()
        .find(|e| e.id == "player")
        .unwrap();
    assert_eq!(
        authored.components.transform.as_ref().unwrap().position[0],
        0.0
    );
}

#[test]
fn snapshot_serializes_to_json() {
    let value = serde_json::to_value(Proj::new().runtime().snapshot()).unwrap();
    assert_eq!(value["scene"]["scene_id"], "room:a");
    assert_eq!(value["ticks"], 0);
}

#[test]
fn runtime_exposes_the_project_it_was_built_from() {
    let rt = Proj::new().runtime();
    assert_eq!(rt.project().name, "Behaviour Project");
    assert_eq!(rt.active_scene().id, "room:a");
}

#[test]
fn project_resource_is_shared_with_native_code() {
    let rt = Proj::new().runtime();
    let resource = rt.app().world().resource::<ProjectResource>();
    assert_eq!(resource.0.name, "Behaviour Project");
}

#[test]
fn runtime_creation_fails_when_a_texture_breaks_after_project_load() {
    let p = Proj::new();
    let project = p.project();
    p.write("white.ppm", "garbage");
    expect_code(Runtime::new(project), "asset.texture.invalid");
}

#[test]
fn runtime_creation_fails_when_an_asset_disappears_after_project_load() {
    let p = Proj::new();
    let project = p.project();
    std::fs::remove_file(p.root().join("quad.mesh")).unwrap();
    expect_code(Runtime::new(project), "project.asset.missing");
}

#[test]
fn bindings_come_from_the_project_manifest() {
    let rt = Proj::new().runtime();
    assert_eq!(rt.bindings()["right"], vec!["Key:Right"]);
}

#[test]
fn rebinding_updates_bindings_and_bumps_the_revision() {
    let mut rt = Proj::new().runtime();
    let mut settings = rt.app().world().resource::<InputSettings>().clone();
    let revision = settings.revision;
    settings
        .rebind(
            [("jump".to_string(), vec!["Key:J".to_string()])].into(),
            0.3,
        )
        .unwrap();
    assert_eq!(settings.revision, revision + 1);
    assert_eq!(settings.deadzone, 0.3);
    rt.app_mut().world_mut().insert_resource(settings);
    assert_eq!(rt.bindings()["jump"], vec!["Key:J"]);
}

#[test]
fn invalid_rebinding_leaves_settings_unchanged() {
    let rt = Proj::new().runtime();
    let mut settings = rt.app().world().resource::<InputSettings>().clone();
    let before = settings.clone();
    let result = settings.rebind(
        [("x".to_string(), vec!["Key:Nope".to_string()])].into(),
        0.2,
    );
    expect_code(result, "input.bindings.invalid");
    assert_eq!(settings.bindings, before.bindings);
    assert_eq!(settings.revision, before.revision);
}

#[test]
fn rebinding_with_an_invalid_deadzone_is_refused() {
    let rt = Proj::new().runtime();
    let mut settings = rt.app().world().resource::<InputSettings>().clone();
    expect_code(
        settings.rebind([("x".to_string(), vec!["Key:A".to_string()])].into(), 1.0),
        "input.bindings.invalid",
    );
    assert_eq!(settings.deadzone, 0.2);
}

// ---- lifecycle ------------------------------------------------------------------------------

fn bad_delta(dt: f32) {
    let mut rt = Proj::new().runtime();
    expect_code(rt.tick(dt, ActionFrame::default()), "runtime.delta.invalid");
    assert!(!rt.running());
    rt.tick(0.1, ActionFrame::default()).unwrap();
    assert!(rt.running());
}

table! { bad_delta;
    zero_delta_is_refused => (0.0);
    negative_delta_is_refused => (-0.1);
    nan_delta_is_refused => (f32::NAN);
    infinite_delta_is_refused => (f32::INFINITY);
}

#[test]
fn first_tick_starts_the_runtime() {
    let mut rt = Proj::new().runtime();
    assert!(!rt.running());
    tick(&mut rt);
    assert!(rt.running());
}

#[test]
fn tick_counter_advances_once_per_successful_tick() {
    let mut rt = Proj::new().runtime();
    for _ in 0..7 {
        tick(&mut rt);
    }
    assert_eq!(rt.snapshot().ticks, 7);
}

#[test]
fn start_is_idempotent() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", logger).unwrap();
    rt.start().unwrap();
    rt.start().unwrap();
    assert_eq!(logs(&rt), vec!["create:a"]);
}

#[test]
fn stop_is_idempotent_and_runs_teardown_once() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", logger).unwrap();
    rt.start().unwrap();
    rt.stop().unwrap();
    rt.stop().unwrap();
    assert_eq!(logs(&rt), vec!["create:a", "destroy:a"]);
    assert!(!rt.running());
}

#[test]
fn a_stopped_runtime_cannot_restart_or_tick() {
    let mut rt = Proj::new().runtime();
    tick(&mut rt);
    rt.stop().unwrap();
    expect_code(rt.start(), "runtime.stopped");
    expect_code(rt.tick(0.1, ActionFrame::default()), "runtime.stopped");
}

#[test]
fn configuration_is_locked_after_startup() {
    let mut rt = Proj::new().runtime();
    rt.start().unwrap();
    expect_code(rt.register_script("x", logger), "runtime.started");
    expect_code(
        rt.set_user_data_root(std::env::temp_dir()),
        "runtime.started",
    );
    let engine = engine_audio::AudioEngine::offline(Default::default()).unwrap();
    expect_code(rt.set_audio(engine.control()), "runtime.started");
}

#[test]
fn registering_an_empty_script_id_is_refused() {
    let mut rt = Proj::new().runtime();
    expect_code(rt.register_script("", logger), "script.asset.invalid");
}

#[test]
fn run_headless_executes_the_requested_ticks_then_stops() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", logger).unwrap();
    rt.run_headless(3, 0.1).unwrap();
    assert_eq!(rt.snapshot().ticks, 3);
    assert!(!rt.running());
    let updates = logs(&rt).iter().filter(|l| l.starts_with("update")).count();
    assert_eq!(updates, 3);
}

#[test]
fn run_headless_rejects_a_second_run() {
    let mut rt = Proj::new().runtime();
    rt.run_headless(1, 0.1).unwrap();
    expect_code(rt.run_headless(1, 0.1), "runtime.stopped");
}

#[test]
fn run_headless_reports_script_failures_and_still_stops() {
    let mut p = scripted(&[]);
    p.add_entity(ent("z", json!({"Script":{"asset":"other"}})));
    let mut rt = p.runtime();
    rt.register_script("other", failer).unwrap();
    expect_code(rt.run_headless(5, 0.1), "fixture.update.failed");
    assert!(!rt.running());
    assert_eq!(rt.snapshot().ticks, 0);
}

#[test]
fn run_headless_with_zero_ticks_still_cleans_up() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", logger).unwrap();
    rt.run_headless(0, 0.1).unwrap();
    assert!(!rt.running());
}

#[test]
fn dropping_a_runtime_destroys_live_scripts() {
    let before = DESTROYED.load(Ordering::SeqCst);
    let mut rt = scripted(&["a", "b"]).runtime();
    rt.register_script("player", drop_probe).unwrap();
    tick(&mut rt);
    drop(rt);
    assert_eq!(DESTROYED.load(Ordering::SeqCst) - before, 2);
}

// ---- native script lifecycle -------------------------------------------------------------------

#[test]
fn scripts_are_created_updated_and_destroyed_in_deterministic_order() {
    let mut rt = scripted(&["b", "a", "c"]).runtime();
    rt.register_script("player", logger).unwrap();
    tick(&mut rt);
    tick(&mut rt);
    rt.stop().unwrap();
    assert_eq!(
        logs(&rt),
        vec![
            "create:a",
            "create:b",
            "create:c",
            "update:a",
            "update:b",
            "update:c",
            "update:a",
            "update:b",
            "update:c",
            "destroy:c",
            "destroy:b",
            "destroy:a"
        ]
    );
}

#[test]
fn script_update_receives_the_tick_delta() {
    struct Dt;
    impl NativeScript for Dt {
        fn on_update(&mut self, w: &mut World, _: Entity, dt: f32) -> Result<()> {
            w.resource_mut::<RuntimeLog>().0.push(format!("dt={dt}"));
            Ok(())
        }
    }
    fn dt_script(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
        Ok(Box::new(Dt))
    }
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", dt_script).unwrap();
    rt.tick(0.25, ActionFrame::default()).unwrap();
    rt.tick(0.5, ActionFrame::default()).unwrap();
    assert_eq!(logs(&rt), vec!["dt=0.25", "dt=0.5"]);
}

#[test]
fn active_script_count_tracks_attached_scripts() {
    let mut rt = scripted(&["a", "b"]).runtime();
    rt.register_script("player", logger).unwrap();
    tick(&mut rt);
    assert_eq!(rt.snapshot().active_scripts, 2);
}

#[test]
fn unregistered_script_reports_the_entity_and_scene_file() {
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    let error = rt.start().unwrap_err();
    assert_eq!(error.diagnostics[0].code, "script.asset.unregistered");
    assert_eq!(error.diagnostics[0].path, "player");
}

#[test]
fn registering_a_factory_later_fixes_a_failed_start_check() {
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    assert!(rt.start().is_err());
    rt.register_script("other", other_logger).unwrap();
    rt.start().unwrap();
    assert_eq!(logs(&rt), vec!["other-create:player"]);
}

#[test]
fn a_failing_update_stops_the_generation_and_destroys_everything_in_reverse() {
    let mut p = scripted(&["a"]);
    p.add_entity(ent("z", json!({"Script":{"asset":"other"}})));
    let mut rt = p.runtime();
    rt.register_script("player", logger).unwrap();
    rt.register_script("other", failer).unwrap();
    expect_code(
        rt.tick(0.1, ActionFrame::default()),
        "fixture.update.failed",
    );
    assert!(!rt.running());
    assert_eq!(
        logs(&rt),
        vec!["create:a", "update:a", "destroy-failer:z", "destroy:a"]
    );
}

#[test]
fn a_failed_tick_is_not_counted() {
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    rt.register_script("other", failer).unwrap();
    assert!(rt.tick(0.1, ActionFrame::default()).is_err());
    assert_eq!(rt.snapshot().ticks, 0);
}

#[test]
fn panic_in_update_is_reported_as_a_script_error() {
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    rt.register_script("other", panic_update).unwrap();
    expect_code(rt.tick(0.1, ActionFrame::default()), "script.panic");
    assert!(!rt.running());
}

#[test]
fn panic_in_create_is_reported_and_cleans_up() {
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    rt.register_script("other", panic_create).unwrap();
    expect_code(rt.start(), "script.panic");
    assert!(!rt.running());
}

#[test]
fn panic_in_destroy_surfaces_from_stop_but_finishes_cleanup() {
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    rt.register_script("other", panic_destroy).unwrap();
    tick(&mut rt);
    expect_code(rt.stop(), "script.panic");
    assert!(!rt.running());
    assert_eq!(rt.snapshot().active_scripts, 0);
}

#[test]
fn factory_errors_abort_startup() {
    fn bad(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
        Err(engine_project::Error::new("fixture.factory", "no"))
    }
    let mut p = Proj::new();
    p.with_script("player", "other");
    let mut rt = p.runtime();
    rt.register_script("other", bad).unwrap();
    expect_code(rt.start(), "fixture.factory");
}

#[test]
fn entities_spawned_by_a_script_are_owned_and_removed_on_stop() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", spawner).unwrap();
    tick(&mut rt);
    let tagged = |rt: &Runtime| {
        rt.app()
            .world()
            .iter_entities()
            .filter(|e| e.contains::<Tag>())
            .count()
    };
    assert_eq!(tagged(&rt), 3);
    let mut values: Vec<u32> = rt
        .app()
        .world()
        .iter_entities()
        .filter_map(|e| e.get::<Tag>().map(|t| t.0))
        .collect();
    values.sort();
    assert_eq!(values, vec![1, 2, 3]);
    let owned = rt
        .app()
        .world()
        .iter_entities()
        .filter(|e| e.contains::<OwnedBy>())
        .count();
    assert_eq!(owned, 3);
    rt.stop().unwrap();
    assert_eq!(tagged(&rt), 0);
}

#[test]
fn ownership_chains_are_despawned_recursively() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", spawner).unwrap();
    tick(&mut rt);
    let before = live(&rt);
    let owner = rt.entity("a").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(owner)
        .remove::<Script>();
    tick(&mut rt);
    assert_eq!(live(&rt), before - 3);
}

#[test]
fn removing_a_script_component_destroys_it_once_and_stops_updates() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", logger).unwrap();
    tick(&mut rt);
    let e = rt.entity("a").unwrap();
    rt.app_mut().world_mut().entity_mut(e).remove::<Script>();
    tick(&mut rt);
    tick(&mut rt);
    assert_eq!(logs(&rt), vec!["create:a", "update:a", "destroy:a"]);
}

#[test]
fn swapping_the_script_asset_retires_the_old_script_and_creates_the_new_one() {
    let mut rt = scripted(&["a"]).runtime();
    rt.register_script("player", logger).unwrap();
    rt.register_script("other", other_logger).unwrap();
    tick(&mut rt);
    let e = rt.entity("a").unwrap();
    rt.app_mut().world_mut().entity_mut(e).insert(Script {
        version: 1,
        asset: "other".into(),
    });
    tick(&mut rt);
    assert_eq!(
        logs(&rt),
        vec![
            "create:a",
            "update:a",
            "other-create:a",
            "other-update:a",
            "destroy:a"
        ]
    );
}

#[test]
fn scripts_added_at_runtime_attach_on_the_next_tick() {
    let mut rt = Proj::new().runtime();
    rt.register_script("player", logger).unwrap();
    tick(&mut rt);
    let e = rt.entity("camera").unwrap();
    rt.app_mut().world_mut().entity_mut(e).insert(Script {
        version: 1,
        asset: "player".into(),
    });
    tick(&mut rt);
    assert_eq!(logs(&rt), vec!["create:camera", "update:camera"]);
}

#[test]
fn spawn_owned_refuses_dead_owners() {
    let mut rt = Proj::new().runtime();
    let world = rt.app_mut().world_mut();
    let owner = world.spawn(Tag(0)).id();
    world.despawn(owner);
    expect_code(spawn_owned(world, owner, Tag(1)), "script.owner.dead");
}

#[test]
fn spawn_owned_links_the_new_entity_to_its_owner() {
    let mut rt = Proj::new().runtime();
    let world = rt.app_mut().world_mut();
    let owner = world.spawn(Tag(0)).id();
    let child = spawn_owned(world, owner, Tag(9)).unwrap();
    assert_eq!(world.get::<OwnedBy>(child).unwrap().0, owner);
}

// ---- input gating ---------------------------------------------------------------------------------

#[test]
fn scripts_see_the_frame_passed_to_tick() {
    let mut rt = Proj::new().runtime();
    rt.tick(
        0.1,
        ActionFrame {
            pointer: [4.0, 5.0],
            ..pressed(&["go"])
        },
    )
    .unwrap();
    let frame = rt.app().world().resource::<InputSnapshot>();
    assert!(frame.pressed("go"));
    assert_eq!(frame.pointer, [4.0, 5.0]);
}

#[test]
fn control_locks_blank_gameplay_input_but_keep_focus() {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .insert("cutscene".into());
    rt.tick(
        0.1,
        ActionFrame {
            focused: false,
            ..pressed(&["go"])
        },
    )
    .unwrap();
    let frame = rt.app().world().resource::<InputSnapshot>();
    assert!(!frame.pressed("go") && !frame.focused);
}

#[test]
fn releasing_the_last_lock_restores_input_on_the_next_tick() {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .insert("cutscene".into());
    rt.tick(0.1, pressed(&["go"])).unwrap();
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .clear();
    rt.tick(0.1, pressed(&["go"])).unwrap();
    assert!(rt.app().world().resource::<InputSnapshot>().pressed("go"));
}

#[test]
fn two_locks_must_both_be_released() {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .extend(["a".to_string(), "b".to_string()]);
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .remove("a");
    rt.tick(0.1, pressed(&["go"])).unwrap();
    assert!(!rt.app().world().resource::<InputSnapshot>().pressed("go"));
}

#[test]
fn modal_panels_capture_input_for_scripts() {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .open(PanelOptions {
            id: "dialog".into(),
            ..Default::default()
        })
        .unwrap();
    rt.tick(0.1, pressed(&["go"])).unwrap();
    assert!(!rt.app().world().resource::<InputSnapshot>().held("go"));
}

#[test]
fn modal_capture_ends_when_the_panel_closes_and_the_key_is_released() {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .open(PanelOptions {
            id: "dialog".into(),
            ..Default::default()
        })
        .unwrap();
    rt.tick(0.1, pressed(&["go"])).unwrap();
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .close("dialog");
    rt.tick(0.1, pressed(&["go"])).unwrap();
    assert!(!rt.app().world().resource::<InputSnapshot>().held("go"));
    rt.tick(0.1, ActionFrame::default()).unwrap();
    rt.tick(0.1, pressed(&["go"])).unwrap();
    assert!(rt.app().world().resource::<InputSnapshot>().pressed("go"));
}

#[test]
fn stopping_the_runtime_clears_locks_and_panels() {
    let mut rt = Proj::new().runtime();
    tick(&mut rt);
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .insert("x".into());
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .open(PanelOptions {
            id: "p".into(),
            ..Default::default()
        })
        .unwrap();
    rt.stop().unwrap();
    assert!(rt.app().world().resource::<ControlLocks>().0.is_empty());
    assert!(rt.app().world().resource::<UiModel>().panels().is_empty());
}

#[test]
fn gameplay_input_helper_follows_locks_immediately() {
    let mut rt = Proj::new().runtime();
    rt.tick(0.1, pressed(&["go"])).unwrap();
    let world = rt.app_mut().world_mut();
    assert!(gameplay_input(world).pressed("go"));
    world.resource_mut::<ControlLocks>().0.insert("x".into());
    assert!(!gameplay_input(world).pressed("go"));
}

#[test]
fn gameplay_input_helper_follows_a_freshly_opened_modal() {
    let mut rt = Proj::new().runtime();
    rt.tick(0.1, pressed(&["go"])).unwrap();
    let world = rt.app_mut().world_mut();
    world
        .resource_mut::<UiModel>()
        .open(PanelOptions {
            id: "p".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(!gameplay_input(world).pressed("go"));
    assert!(world.resource::<InputSnapshot>().pressed("go"));
    refresh_gameplay_input(world);
    assert!(!world.resource::<InputSnapshot>().pressed("go"));
}

// ---- collision synchronisation and triggers ----------------------------------------------------------

#[test]
fn runtime_move_entity_updates_the_transform_and_reports_contacts() {
    let mut p = Proj::new();
    p.add_entity(wall("wall", 3.0, 0.0, 1.0, 1.0));
    let mut rt = p.runtime();
    let result = rt.move_entity("player", [8.0, 0.0]).unwrap();
    assert_eq!(result.position, [2.0, 0.0]);
    assert_eq!(result.contacts, vec!["wall"]);
    assert_eq!(rt.position("player"), Some([2.0, 0.0, 0.0]));
}

#[test]
fn moving_in_the_plane_preserves_depth() {
    let p = walking();
    let mut rt = p.runtime();
    rt.move_entity("player", [1.0, 1.0]).unwrap();
    assert_eq!(rt.position("player"), Some([1.0, 1.0, 3.0]));
}

#[test]
fn moving_an_unknown_entity_is_an_error() {
    expect_code(
        Proj::new().runtime().move_entity("ghost", [1.0, 0.0]),
        "collision.body.unknown",
    );
}

#[test]
fn moving_an_entity_without_a_collider_is_an_error() {
    expect_code(
        Proj::new().runtime().move_entity("camera", [1.0, 0.0]),
        "collision.body.unknown",
    );
}

#[test]
fn collision_follows_transforms_edited_directly_in_the_world() {
    let mut p = Proj::new();
    p.add_entity(wall("wall", 3.0, 0.0, 1.0, 1.0));
    let mut rt = p.runtime();
    let wall_entity = rt.entity("wall").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(wall_entity)
        .unwrap()
        .position = [3.0, 20.0, 0.0];
    let result = rt.move_entity("player", [8.0, 0.0]).unwrap();
    assert_eq!(result.position, [8.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn despawned_walls_stop_blocking() {
    let mut p = Proj::new();
    p.add_entity(wall("wall", 3.0, 0.0, 1.0, 1.0));
    let mut rt = p.runtime();
    let wall_entity = rt.entity("wall").unwrap();
    rt.app_mut().world_mut().despawn(wall_entity);
    assert_eq!(
        rt.move_entity("player", [8.0, 0.0]).unwrap().position,
        [8.0, 0.0]
    );
}

#[test]
fn removing_a_collider_component_stops_it_blocking() {
    let mut p = Proj::new();
    p.add_entity(wall("wall", 3.0, 0.0, 1.0, 1.0));
    let mut rt = p.runtime();
    let wall_entity = rt.entity("wall").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(wall_entity)
        .remove::<engine_project::Collider2D>();
    assert_eq!(
        rt.move_entity("player", [8.0, 0.0]).unwrap().position,
        [8.0, 0.0]
    );
}

#[test]
fn trigger_overlap_is_reported_as_enter_stay_exit_across_ticks() {
    let mut p = Proj::new();
    p.add_entity(trigger("zone", 3.0, 0.0));
    let mut rt = p.runtime();
    tick(&mut rt);
    assert!(rt.snapshot().triggers.is_empty());
    rt.move_entity("player", [3.0, 0.0]).unwrap();
    tick(&mut rt);
    let enter = rt.snapshot().triggers;
    assert_eq!(enter.len(), 1);
    assert_eq!(
        (enter[0].first.as_str(), enter[0].second.as_str()),
        ("player", "zone")
    );
    assert_eq!(enter[0].phase, gameplay2d::TriggerPhase::Enter);
    tick(&mut rt);
    assert_eq!(
        rt.snapshot().triggers[0].phase,
        gameplay2d::TriggerPhase::Stay
    );
    rt.move_entity("player", [-6.0, 0.0]).unwrap();
    tick(&mut rt);
    assert_eq!(
        rt.snapshot().triggers[0].phase,
        gameplay2d::TriggerPhase::Exit
    );
    tick(&mut rt);
    assert!(rt.snapshot().triggers.is_empty());
}

#[test]
fn trigger_events_are_available_to_native_code_as_a_resource() {
    let mut p = Proj::new();
    p.add_entity(trigger("zone", 0.0, 0.0));
    let mut rt = p.runtime();
    tick(&mut rt);
    assert_eq!(rt.app().world().resource::<TriggerEvents>().0.len(), 1);
}

// ---- Bevy schedule integration ----------------------------------------------------------------------------

#[derive(Resource, Default)]
struct Counter(u32);

fn bump(mut counter: ResMut<Counter>) {
    counter.0 += 1;
}

fn teleport(mut query: Query<(&AuthoredId, &mut Transform)>) {
    for (id, mut transform) in &mut query {
        if id.id == "player" {
            transform.position[0] = 5.0;
        }
    }
}

fn read_counter(counter: Res<Counter>, mut log: ResMut<RuntimeLog>) {
    log.0.push(format!("counter={}", counter.0));
}

#[test]
fn update_systems_run_once_per_tick() {
    let mut rt = Proj::new().runtime();
    rt.app_mut().world_mut().insert_resource(Counter::default());
    rt.app_mut().schedule_mut(Stage::Update).add_systems(bump);
    for _ in 0..4 {
        tick(&mut rt);
    }
    assert_eq!(rt.app().world().resource::<Counter>().0, 4);
}

#[test]
fn startup_systems_run_once_when_the_runtime_starts() {
    let mut rt = Proj::new().runtime();
    rt.app_mut().world_mut().insert_resource(Counter::default());
    rt.app_mut().schedule_mut(Stage::Startup).add_systems(bump);
    tick(&mut rt);
    tick(&mut rt);
    assert_eq!(rt.app().world().resource::<Counter>().0, 1);
}

#[test]
fn shutdown_systems_run_when_the_runtime_stops() {
    let mut rt = Proj::new().runtime();
    rt.app_mut().world_mut().insert_resource(Counter::default());
    rt.app_mut().schedule_mut(Stage::Shutdown).add_systems(bump);
    tick(&mut rt);
    assert_eq!(rt.app().world().resource::<Counter>().0, 0);
    rt.stop().unwrap();
    assert_eq!(rt.app().world().resource::<Counter>().0, 1);
}

#[test]
fn transforms_written_by_systems_reach_collision_in_the_same_tick() {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(teleport);
    tick(&mut rt);
    let body = rt
        .app()
        .world()
        .resource::<gameplay2d::World>()
        .body("player")
        .unwrap();
    assert_eq!(body.position[0], 5.0);
}

#[test]
fn scripts_run_before_systems_within_a_tick() {
    struct Mark;
    impl NativeScript for Mark {
        fn on_update(&mut self, w: &mut World, _: Entity, _: f32) -> Result<()> {
            w.resource_mut::<RuntimeLog>().0.push("script".into());
            w.resource_mut::<Counter>().0 = 10;
            Ok(())
        }
    }
    fn mark(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
        Ok(Box::new(Mark))
    }
    let mut rt = scripted(&["a"]).runtime();
    rt.app_mut().world_mut().insert_resource(Counter::default());
    rt.app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(read_counter);
    rt.register_script("player", mark).unwrap();
    tick(&mut rt);
    assert_eq!(logs(&rt), vec!["script", "counter=10"]);
}

// ---- movement through input -> script -> collision --------------------------------------------------------------

fn player_xy(rt: &Runtime) -> [f32; 2] {
    let p = rt.position("player").unwrap();
    [p[0], p[1]]
}

fn walk(keys: &[&str], ticks: usize) -> (Runtime, [f32; 2]) {
    let (mut rt, mut mapper) = walker(&walking());
    drive(&mut rt, &mut mapper, keys, ticks);
    let at = player_xy(&rt);
    (rt, at)
}

#[test]
fn no_input_means_no_movement() {
    let (rt, at) = walk(&[], 10);
    assert_eq!(at, [0.0, 0.0]);
    assert_eq!(rt.position("player").unwrap()[2], 3.0);
}

#[test]
fn right_moves_along_positive_x() {
    assert_eq!(walk(&["Key:D"], 3).1, [3.0, 0.0]);
}

#[test]
fn left_moves_along_negative_x() {
    assert_eq!(walk(&["Key:A"], 3).1, [-3.0, 0.0]);
}

#[test]
fn up_moves_along_positive_y() {
    assert_eq!(walk(&["Key:W"], 2).1, [0.0, 2.0]);
}

#[test]
fn down_moves_along_negative_y() {
    assert_eq!(walk(&["Key:S"], 2).1, [0.0, -2.0]);
}

#[test]
fn opposite_keys_cancel() {
    assert_eq!(walk(&["Key:A", "Key:D"], 5).1, [0.0, 0.0]);
    assert_eq!(walk(&["Key:W", "Key:S"], 5).1, [0.0, 0.0]);
}

#[test]
fn diagonal_movement_is_normalized() {
    let (_, at) = walk(&["Key:D", "Key:W"], 4);
    let component = 4.0 / 2.0_f32.sqrt();
    assert!(near(at[0], component) && near(at[1], component), "{at:?}");
}

#[test]
fn diagonal_speed_equals_cardinal_speed() {
    let (_, diagonal) = walk(&["Key:A", "Key:S"], 6);
    let (_, cardinal) = walk(&["Key:D"], 6);
    let speed = |p: [f32; 2]| p[0].hypot(p[1]);
    assert!((speed(diagonal) - speed(cardinal)).abs() < 1e-4);
    assert!(diagonal[0] < 0.0 && diagonal[1] < 0.0);
}

#[test]
fn a_cancelled_axis_leaves_pure_cardinal_motion() {
    assert_eq!(walk(&["Key:A", "Key:D", "Key:W"], 3).1, [0.0, 3.0]);
}

#[test]
fn movement_preserves_the_authored_depth() {
    let (rt, _) = walk(&["Key:D", "Key:W"], 8);
    assert_eq!(rt.position("player").unwrap()[2], 3.0);
}

#[test]
fn fixed_stepping_is_deterministic_across_runs() {
    let keys = ["Key:D", "Key:W"];
    assert_eq!(walk(&keys, 120).1, walk(&keys, 120).1);
}

#[test]
fn walls_stop_one_axis_while_the_other_continues() {
    let mut p = walking();
    p.add_entity(wall("wall", 3.0, 0.0, 1.0, 10.0));
    let (mut rt, mut mapper) = walker(&p);
    drive(&mut rt, &mut mapper, &["Key:D", "Key:W"], 8);
    let at = player_xy(&rt);
    assert_eq!(at[0], 2.0);
    assert!(at[1] > 4.0);
}

#[test]
fn releasing_keys_stops_the_walker() {
    let (mut rt, mut mapper) = walker(&walking());
    drive(&mut rt, &mut mapper, &["Key:D"], 2);
    drive(&mut rt, &mut mapper, &[], 3);
    assert_eq!(player_xy(&rt), [2.0, 0.0]);
}

#[test]
fn losing_window_focus_stops_the_walker_immediately() {
    let (mut rt, mut mapper) = walker(&walking());
    drive(&mut rt, &mut mapper, &["Key:D"], 2);
    let raw = RawInput {
        down: names(&["Key:D"]),
        focused: false,
        ..Default::default()
    };
    for _ in 0..3 {
        rt.tick(0.25, mapper.sample(&raw)).unwrap();
    }
    assert_eq!(player_xy(&rt), [2.0, 0.0]);
}

#[test]
fn a_key_held_through_refocus_does_not_resume_movement() {
    let (mut rt, mut mapper) = walker(&walking());
    let mut raw = RawInput {
        down: names(&["Key:D"]),
        ..Default::default()
    };
    rt.tick(0.25, mapper.sample(&raw)).unwrap();
    raw.focused = false;
    rt.tick(0.25, mapper.sample(&raw)).unwrap();
    raw.focused = true;
    for _ in 0..3 {
        rt.tick(0.25, mapper.sample(&raw)).unwrap();
    }
    assert_eq!(player_xy(&rt), [1.0, 0.0]);
}

#[test]
fn a_modal_dialog_freezes_the_walker_until_closed_and_rereleased() {
    let (mut rt, mut mapper) = walker(&walking());
    drive(&mut rt, &mut mapper, &["Key:D"], 1);
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .open(PanelOptions {
            id: "menu".into(),
            ..Default::default()
        })
        .unwrap();
    drive(&mut rt, &mut mapper, &["Key:D"], 3);
    assert_eq!(player_xy(&rt), [1.0, 0.0]);
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .close("menu");
    drive(&mut rt, &mut mapper, &["Key:D"], 2);
    assert_eq!(player_xy(&rt), [1.0, 0.0]);
    drive(&mut rt, &mut mapper, &[], 1);
    drive(&mut rt, &mut mapper, &["Key:D"], 2);
    assert_eq!(player_xy(&rt), [3.0, 0.0]);
}

#[test]
fn a_control_lock_freezes_the_walker() {
    let (mut rt, mut mapper) = walker(&walking());
    rt.app_mut()
        .world_mut()
        .resource_mut::<ControlLocks>()
        .0
        .insert("cutscene".into());
    drive(&mut rt, &mut mapper, &["Key:D"], 4);
    assert_eq!(player_xy(&rt), [0.0, 0.0]);
}

// ---- scene transitions ------------------------------------------------------------------------------------------------

#[test]
fn transition_to_an_unknown_scene_asset_is_refused() {
    let p = rooms();
    let mut rt = p.runtime();
    expect_code(rt.request_scene("ghost", "", ""), "project.asset.unknown");
}

#[test]
fn transition_to_a_non_scene_asset_is_refused() {
    let p = rooms();
    let mut rt = p.runtime();
    expect_code(rt.request_scene("texture", "", ""), "project.asset.kind");
}

#[test]
fn transition_with_an_unknown_traveller_is_refused() {
    let p = rooms();
    let mut rt = p.runtime();
    expect_code(
        rt.request_scene("scene:b", "", "ghost"),
        "runtime.scene.traveller",
    );
}

#[test]
fn transition_with_an_unknown_spawn_is_refused() {
    let p = rooms();
    let mut rt = p.runtime();
    expect_code(
        rt.request_scene("scene:b", "ghost", "player"),
        "runtime.scene.spawn",
    );
}

#[test]
fn spawn_without_a_traveller_is_refused() {
    let p = rooms();
    let mut rt = p.runtime();
    expect_code(
        rt.request_scene("scene:b", "door_b", ""),
        "runtime.scene.traveller",
    );
}

#[test]
fn spawn_on_an_entity_without_a_transform_is_refused() {
    let mut p = rooms();
    p.add_scene(
        "scene:c",
        "c.json",
        scene_json("room:c", vec![ent("bare", json!({}))]),
    );
    let mut rt = p.runtime();
    expect_code(
        rt.request_scene("scene:c", "bare", "player"),
        "runtime.scene.spawn",
    );
}

#[test]
fn a_second_pending_request_is_refused_and_the_first_still_runs() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    expect_code(rt.request_scene("scene:b", "", ""), "runtime.scene.pending");
    tick(&mut rt);
    assert_eq!(rt.snapshot().scene.id, "room:b");
}

#[test]
fn refused_requests_leave_nothing_pending_and_the_scene_unchanged() {
    let p = rooms();
    let mut rt = p.runtime();
    assert!(rt.request_scene("scene:b", "ghost", "player").is_err());
    assert!(rt.app().world().resource::<PendingScene>().0.is_none());
    tick(&mut rt);
    assert_eq!(rt.snapshot().scene.id, "room:a");
    assert_eq!(rt.snapshot().scene_revision, 0);
}

#[test]
fn transition_waits_for_the_end_of_the_tick() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    assert_eq!(rt.snapshot().scene.id, "room:a");
    assert!(rt.entity("door_b").is_none());
    tick(&mut rt);
    assert!(rt.entity("door_b").is_some());
}

#[test]
fn traveller_arrives_at_the_spawn_with_its_identity_intact() {
    let p = rooms();
    let mut rt = p.runtime();
    let handle = rt.entity("player").unwrap();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert_eq!(rt.entity("player"), Some(handle));
    assert_eq!(rt.position("player"), Some([10.0, 0.0, 0.0]));
}

#[test]
fn old_room_entities_are_removed_by_a_transition() {
    let p = rooms();
    let mut rt = p.runtime();
    let camera = rt.entity("camera").unwrap();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert!(rt.entity("camera").is_none());
    assert!(rt.app().world().get_entity(camera).is_err());
}

#[test]
fn new_room_entities_exist_after_a_transition() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert!(rt.entity("door_b").is_some() && rt.entity("pillar").is_some());
    assert_eq!(rt.snapshot().scene.id, "room:b");
    assert_eq!(rt.snapshot().scene_revision, 1);
}

#[test]
fn the_cheap_scene_revision_accessor_tracks_transitions() {
    let p = rooms();
    let mut rt = p.runtime();
    assert_eq!(rt.scene_revision(), 0);
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    assert_eq!(rt.scene_revision(), 0, "requested, not yet performed");
    tick(&mut rt);
    assert_eq!(rt.scene_revision(), 1);
    assert_eq!(rt.scene_revision(), rt.snapshot().scene_revision);
}

#[test]
fn transition_without_a_traveller_replaces_everything() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "", "").unwrap();
    tick(&mut rt);
    assert!(rt.entity("player").is_none());
    assert_eq!(live(&rt), 2);
}

#[test]
fn traveller_absent_from_the_destination_is_carried_in() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "", "player").unwrap();
    tick(&mut rt);
    assert!(rt.entity("player").is_some());
    assert_eq!(live(&rt), 3);
}

#[test]
fn traveller_state_wins_over_a_destination_entity_with_the_same_id() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json(
            "room:b",
            vec![marker("player", 50.0, 50.0), marker("door_b", 10.0, 0.0)],
        ),
    );
    let mut rt = p.runtime();
    rt.move_entity("player", [2.0, 0.0]).unwrap();
    rt.request_scene("scene:b", "", "player").unwrap();
    tick(&mut rt);
    assert_eq!(rt.position("player"), Some([2.0, 0.0, 0.0]));
}

#[test]
fn traveller_keeps_custom_components_and_gains_the_new_room() {
    #[derive(Component)]
    struct Coins(u32);
    let p = rooms();
    let mut rt = p.runtime();
    let e = rt.entity("player").unwrap();
    rt.app_mut().world_mut().entity_mut(e).insert(Coins(7));
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert_eq!(rt.app().world().get::<Coins>(e).unwrap().0, 7);
}

#[test]
fn spawn_blocked_by_a_solid_is_refused_without_side_effects() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json(
            "room:b",
            vec![
                marker("door_b", 10.0, 0.0),
                wall("block", 10.0, 0.0, 1.0, 1.0),
            ],
        ),
    );
    let mut rt = p.runtime();
    expect_code(
        rt.request_scene("scene:b", "door_b", "player"),
        "runtime.scene.spawn.blocked",
    );
    assert!(rt.app().world().resource::<PendingScene>().0.is_none());
    assert_eq!(rt.snapshot().scene.id, "room:a");
}

#[test]
fn destination_scripts_without_factories_are_refused_up_front() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json(
            "room:b",
            vec![ent("npc", json!({"Script":{"asset":"other"}}))],
        ),
    );
    let mut rt = p.runtime();
    expect_code(
        rt.request_scene("scene:b", "", "player"),
        "script.asset.unregistered",
    );
}

#[test]
fn game_state_survives_transitions() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<GameState>()
        .0
        .insert("coins".into(), json!(3));
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert_eq!(rt.app().world().resource::<GameState>().0["coins"], 3);
}

#[test]
fn ui_panels_are_cleared_by_a_transition() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .open(PanelOptions {
            id: "p".into(),
            ..Default::default()
        })
        .unwrap();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert!(rt.app().world().resource::<UiModel>().panels().is_empty());
}

#[test]
fn trigger_events_are_cleared_by_a_transition() {
    let mut p = rooms();
    p.add_entity(trigger("zone", 0.0, 0.0));
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    assert!(rt.snapshot().triggers.is_empty());
}

#[test]
fn collision_is_rebuilt_for_the_destination_room() {
    let p = rooms();
    let mut rt = p.runtime();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    let result = rt.move_entity("player", [0.0, 8.0]).unwrap();
    assert_eq!(result.position, [10.0, 2.0]);
    assert_eq!(result.contacts, vec!["pillar"]);
}

#[test]
fn rooms_can_be_chained_back_and_forth() {
    let mut p = rooms();
    p.add_entity(marker("door_a", -4.0, 0.0));
    let mut rt = p.runtime();
    let handle = rt.entity("player");
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    rt.request_scene("scene:a", "door_a", "player").unwrap();
    tick(&mut rt);
    assert_eq!(rt.snapshot().scene.id, "room:a");
    assert_eq!(rt.snapshot().scene_revision, 2);
    assert_eq!(rt.entity("player"), handle);
    assert_eq!(rt.position("player"), Some([-4.0, 0.0, 0.0]));
}

#[test]
fn only_non_traveller_scripts_are_retired_by_a_transition() {
    let mut p = rooms();
    p.with_script("player", "player");
    p.with_script("camera", "player");
    let mut rt = p.runtime();
    rt.register_script("player", logger).unwrap();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt);
    tick(&mut rt);
    let log = logs(&rt);
    assert!(log.contains(&"destroy:camera".to_string()));
    assert!(!log.contains(&"destroy:player".to_string()));
    assert_eq!(log.iter().filter(|l| *l == "create:player").count(), 1);
    assert_eq!(log.iter().filter(|l| *l == "update:player").count(), 2);
}

#[test]
fn destination_scripts_are_created_by_the_transition() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json(
            "room:b",
            vec![ent("npc", json!({"Script":{"asset":"other"}}))],
        ),
    );
    let mut rt = p.runtime();
    rt.register_script("other", other_logger).unwrap();
    rt.request_scene("scene:b", "", "player").unwrap();
    tick(&mut rt);
    assert_eq!(logs(&rt), vec!["other-create:npc"]);
    tick(&mut rt);
    assert_eq!(logs(&rt).last().unwrap(), "other-update:npc");
}

#[test]
fn scripts_can_request_transitions_from_their_update() {
    let mut p = rooms();
    p.with_script("player", "player");
    let mut rt = p.runtime();
    rt.register_script("player", requester).unwrap();
    tick(&mut rt);
    assert_eq!(rt.snapshot().scene.id, "room:b");
    assert_eq!(rt.position("player"), Some([10.0, 0.0, 0.0]));
}

#[test]
fn service_request_scene_validates_before_queueing() {
    let p = rooms();
    let mut rt = p.runtime();
    let world = rt.app_mut().world_mut();
    let bad = SceneRequest {
        asset: "ghost".into(),
        spawn: String::new(),
        traveller: String::new(),
    };
    assert!(request_scene(world, bad).is_err());
    assert!(world.resource::<PendingScene>().0.is_none());
    let good = SceneRequest {
        asset: "scene:b".into(),
        spawn: String::new(),
        traveller: String::new(),
    };
    request_scene(world, good.clone()).unwrap();
    expect_code(request_scene(world, good), "runtime.scene.pending");
}

#[test]
fn a_transition_failure_in_teardown_stops_the_runtime() {
    struct BadEnd;
    impl NativeScript for BadEnd {
        fn on_destroy(&mut self, _: &mut World, _: Entity) -> Result<()> {
            Err(engine_project::Error::new("fixture.destroy.failed", "no"))
        }
    }
    fn bad_end(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
        Ok(Box::new(BadEnd))
    }
    let mut p = rooms();
    p.with_script("camera", "player");
    let mut rt = p.runtime();
    rt.register_script("player", bad_end).unwrap();
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    expect_code(
        rt.tick(0.1, ActionFrame::default()),
        "fixture.destroy.failed",
    );
    assert!(!rt.running());
}

// ---- service functions on a bare world -------------------------------------------------------------------------------------

#[test]
fn services_on_a_bare_world_degrade_gracefully() {
    let world = World::new();
    assert!(find_entity(&world, "player").is_none());
    expect_code(
        resolve_asset(&world, "texture"),
        "runtime.project.unavailable",
    );
}

#[test]
fn resolve_asset_service_reports_unknown_ids() {
    let mut rt = Proj::new().runtime();
    expect_code(
        resolve_asset(rt.app_mut().world(), "ghost"),
        "project.asset.unknown",
    );
}

#[test]
fn move_entity_service_requires_a_known_body() {
    let mut rt = Proj::new().runtime();
    expect_code(
        move_entity(rt.app_mut().world_mut(), "ghost", [1.0, 0.0]),
        "collision.body.unknown",
    );
}

#[test]
fn move_entity_service_keeps_depth_and_updates_xy() {
    let mut rt = walking().runtime();
    let result = move_entity(rt.app_mut().world_mut(), "player", [1.5, -0.5]).unwrap();
    assert_eq!(result.position, [1.5, -0.5]);
    assert_eq!(rt.position("player"), Some([1.5, -0.5, 3.0]));
}

#[test]
fn safe_position_finds_a_free_spot_next_to_an_overlapping_wall() {
    let mut p = Proj::new();
    p.add_entity(wall("wall", 0.0, 0.0, 1.0, 1.0));
    let mut rt = p.runtime();
    let spot = safe_position(rt.app_mut().world_mut(), "player", 2.0, 1.0).unwrap();
    assert_eq!(spot, Some([-1.0, 0.0]));
}

#[test]
fn safe_position_of_an_unobstructed_entity_is_its_own_position() {
    let mut rt = Proj::new().runtime();
    assert_eq!(
        safe_position(rt.app_mut().world_mut(), "player", 2.0, 1.0).unwrap(),
        Some([0.0, 0.0])
    );
}

#[test]
fn safe_position_of_an_unknown_entity_is_an_error() {
    let mut rt = Proj::new().runtime();
    expect_code(
        safe_position(rt.app_mut().world_mut(), "ghost", 2.0, 1.0),
        "collision.body.unknown",
    );
}

#[test]
fn safe_position_rejects_bad_search_parameters() {
    let mut rt = Proj::new().runtime();
    expect_code(
        safe_position(rt.app_mut().world_mut(), "player", 2.0, 0.0),
        "collision.search.invalid",
    );
}
