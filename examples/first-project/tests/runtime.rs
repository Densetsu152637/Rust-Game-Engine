use engine_project::{ActionFrame, Project, Runtime};
use first_project_gameplay::*;

fn make_runtime(asset: &str) -> Runtime {
    let mut project = Project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project.json")).unwrap();
    // Tests replace the existing stable script binding with a registered fixture;
    // its native code is part of this same game crate.
    project.assets.get_mut("asset:player-script").unwrap().id = asset.into();
    let mut script_asset = project.assets.remove("asset:player-script").unwrap();
    script_asset.id = asset.into();
    project.assets.insert(asset.into(), script_asset);
    project.scene.entities[0]
        .components
        .script
        .as_mut()
        .unwrap()
        .asset = asset.into();
    let mut runtime = Runtime::new(project).unwrap();
    configure(runtime.app_mut()).unwrap();
    runtime
}
#[test]
fn authored_owner_input_and_arbitrary_component_system() {
    let mut runtime = make_runtime("asset:player-script");
    let owner = runtime.entity("entity:player").unwrap();
    let count = runtime.snapshot().live_entities;
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert_eq!(runtime.position("entity:player").unwrap(), [0.0; 3]);
    let mut input = ActionFrame::default();
    input.pressed.insert("move_right".into());
    runtime.tick(1.0 / 60.0, input).unwrap();
    assert_eq!(runtime.position("entity:player").unwrap(), [1.0, 0.0, 0.0]);
    assert_eq!(
        runtime.app().world().get::<PlayerStats>(owner).unwrap(),
        &PlayerStats {
            ticks: 2,
            moves: 1,
            enabled: true
        }
    );
    assert_eq!(runtime.snapshot().live_entities, count);
    runtime.stop().unwrap();
}
#[test]
fn replacement_generation_uses_new_native_callback() {
    let mut runtime = make_runtime("asset:player-reload");
    let mut input = ActionFrame::default();
    input.pressed.insert("move_right".into());
    runtime.tick(1.0 / 60.0, input).unwrap();
    assert_eq!(runtime.position("entity:player").unwrap()[0], 10.0);
    runtime.stop().unwrap();
}
#[test]
fn setup_failure_runs_cleanup_and_shutdown_keeps_teardown_error() {
    let mut runtime = make_runtime("asset:create-then-fail");
    let count = runtime.app().world().entities().len();
    assert!(runtime.tick(1.0 / 60.0, ActionFrame::default()).is_err());
    assert_eq!(runtime.app().world().entities().len(), count);
    assert_eq!(
        runtime.app().world().resource::<LifecycleLog>().0,
        vec![
            "partial-on-create-effect",
            "spawned-entity-cleaned-by-script"
        ]
    );
    let mut runtime = make_runtime("asset:teardown-fails");
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert!(runtime.stop().is_err());
    assert_eq!(
        runtime
            .app()
            .world()
            .resource::<LifecycleLog>()
            .0
            .last()
            .unwrap(),
        "old-destroy"
    );
}
#[test]
fn failed_candidate_does_not_publish_component_edits() {
    let mut runtime = make_runtime("asset:system-abort");
    let owner = runtime.entity("entity:player").unwrap();
    assert!(runtime.tick(1.0 / 60.0, ActionFrame::default()).is_err());
    assert_eq!(runtime.app().world().get::<AbortState>(owner).unwrap().0, 0);
    assert_eq!(
        runtime
            .app()
            .world()
            .resource::<LifecycleLog>()
            .0
            .last()
            .unwrap(),
        "abort-state:0"
    );
}

#[test]
fn native_system_jobs_publish_structural_changes_at_end_of_schedule() {
    let mut runtime = make_runtime("asset:system-fixture");
    let owner = runtime.entity("entity:player").unwrap();
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert_eq!(runtime.app().world().get::<TestState>(owner).unwrap().0, 1);
    let spawned = runtime
        .app()
        .world()
        .resource::<FixtureState>()
        .spawned
        .unwrap();
    assert!(runtime.app().world().get::<TestState>(spawned).is_none());
    let log = &runtime.app().world().resource::<LifecycleLog>().0;
    assert_eq!(log.iter().filter(|s| s.starts_with("state:")).count(), 2);
    assert!(!log.iter().any(|s| s.starts_with("late:")));
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert!(
        runtime
            .app()
            .world()
            .resource::<LifecycleLog>()
            .0
            .iter()
            .any(|s| s.starts_with("late:"))
    );
    runtime.stop().unwrap();
    assert!(runtime.app().world().get_entity(spawned).is_err());
}

fn press(actions: &[&str]) -> ActionFrame {
    let mut input = ActionFrame::default();
    for action in actions {
        input.pressed.insert((*action).into());
    }
    input
}
fn stats(runtime: &Runtime) -> PlayerStats {
    let owner = runtime.entity("entity:player").unwrap();
    runtime
        .app()
        .world()
        .get::<PlayerStats>(owner)
        .unwrap()
        .clone()
}
#[test]
fn no_input_never_moves_the_player() {
    let mut runtime = make_runtime("asset:player-script");
    for _ in 0..30 {
        runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    }
    assert_eq!(runtime.position("entity:player").unwrap(), [0.0; 3]);
    assert_eq!(stats(&runtime).moves, 0);
    runtime.stop().unwrap();
}
#[test]
fn repeated_presses_accumulate_one_unit_each() {
    let mut runtime = make_runtime("asset:player-script");
    for _ in 0..5 {
        runtime.tick(1.0 / 60.0, press(&["move_right"])).unwrap();
    }
    assert_eq!(runtime.position("entity:player").unwrap()[0], 5.0);
    assert_eq!(stats(&runtime).moves, 5);
    runtime.stop().unwrap();
}
#[test]
fn movement_preserves_y_and_z() {
    let mut runtime = make_runtime("asset:player-script");
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    let owner = runtime.entity("entity:player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::Transform>(owner)
        .unwrap()
        .position = [0.0, 3.0, -2.0];
    runtime.tick(1.0 / 60.0, press(&["move_right"])).unwrap();
    assert_eq!(runtime.position("entity:player").unwrap(), [1.0, 3.0, -2.0]);
    runtime.stop().unwrap();
}
#[test]
fn unmapped_actions_do_not_move_the_player() {
    let mut runtime = make_runtime("asset:player-script");
    runtime
        .tick(1.0 / 60.0, press(&["move_left", "jump"]))
        .unwrap();
    assert_eq!(runtime.position("entity:player").unwrap(), [0.0; 3]);
    runtime.stop().unwrap();
}
#[test]
fn disabled_stats_are_not_advanced_by_the_native_system() {
    let mut runtime = make_runtime("asset:player-script");
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    let owner = runtime.entity("entity:player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PlayerStats>(owner)
        .unwrap()
        .enabled = false;
    runtime.tick(1.0 / 60.0, press(&["move_right"])).unwrap();
    let stats = stats(&runtime);
    assert_eq!((stats.ticks, stats.moves), (1, 0));
    runtime.stop().unwrap();
}
#[test]
fn stopping_removes_the_owned_player_and_is_idempotent() {
    let mut runtime = make_runtime("asset:player-script");
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert!(runtime.running());
    runtime.stop().unwrap();
    runtime.stop().unwrap();
    assert!(!runtime.running());
    assert!(runtime.tick(1.0 / 60.0, ActionFrame::default()).is_err());
}
#[test]
fn authored_manifest_keeps_stable_ids_and_the_single_player() {
    let runtime = make_runtime("asset:player-script");
    assert_eq!(runtime.active_scene().entities.len(), 1);
    assert_eq!(runtime.active_scene().entities[0].id, "entity:player");
    assert!(runtime.project().assets.contains_key("asset:player-mesh"));
    assert!(
        runtime
            .project()
            .assets
            .contains_key("asset:player-texture")
    );
    assert_eq!(runtime.project().input_actions["move_right"], "Right");
}
#[test]
fn undeclared_script_asset_is_rejected_before_instantiation() {
    let mut project = Project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project.json")).unwrap();
    project.scene.entities[0]
        .components
        .script
        .as_mut()
        .unwrap()
        .asset = "asset:unknown".into();
    assert!(Runtime::new(project).is_err());
}
#[test]
#[ignore = "spawns cargo; run explicitly to prove invalid candidates fail with Rust diagnostics"]
fn invalid_candidate_fixture_fails_cargo_check() {
    let output = std::process::Command::new("cargo")
        .args(["check", "--quiet", "--manifest-path"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/invalid/Cargo.toml"
        ))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error"));
}
