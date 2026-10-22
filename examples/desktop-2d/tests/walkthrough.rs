use std::process::Command;
fn user_data() -> tempfile::TempDir {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/example-test-data");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
#[test]
fn both_variants_use_real_runtime_and_restore_in_separate_processes() {
    for variant in ["male", "female"] {
        let user_data = user_data();
        for mode in ["new", "restore"] {
            // Launch from a foreign directory: the game locates authored content
            // by its manifest, while persistence uses the supplied user-data root.
            let output = Command::new(env!("CARGO_BIN_EXE_lab-walkthrough"))
                .args([mode, variant, user_data.path().to_str().unwrap()])
                .current_dir(user_data.path())
                .output()
                .unwrap();
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(
                output.status.success(),
                "{variant} {mode}: {text}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["ok"], true);
            assert_eq!(json["audioVoicesAfterStop"], 0);
            let marker = format!(
                "DESKTOP2D_{}PASS:{variant}",
                if mode == "new" {
                    "WALKTHROUGH_"
                } else {
                    "RESTART_"
                }
            );
            assert!(
                json["snapshot"]["logs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s == &marker)
            );
        }
    }
}
#[test]
fn missing_injected_ui_action_fails_instead_of_reaching_tick_limit() {
    let user_data = user_data();
    let output = Command::new(env!("CARGO_BIN_EXE_lab-walkthrough"))
        .args([
            "new",
            "male",
            user_data.path().to_str().unwrap(),
            "--omit-confirm",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], false);
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("inject ui_confirm:6")
    );
}

#[test]
fn newly_opened_modal_masks_later_actions_in_same_callback() {
    use engine_project::{ActionFrame, Project, Runtime, runtime::GameState};
    let user_data = user_data();
    let mut runtime =
        Runtime::new(Project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project.json")).unwrap())
            .unwrap();
    runtime.set_user_data_root(user_data.path().into()).unwrap();
    desktop_2d_gameplay::configure(runtime.app_mut()).unwrap();
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    let before = runtime.position("actor:player").unwrap();
    let mut input = ActionFrame::default();
    for action in ["journal", "story_demo", "move_right"] {
        input.pressed.insert(action.into());
        input.held.insert(action.into());
        input.values.insert(action.into(), 1.0);
    }
    runtime.tick(1.0 / 60.0, input).unwrap();
    assert_eq!(runtime.position("actor:player").unwrap(), before);
    assert!(
        runtime.app().world().resource::<GameState>().0["reveals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    runtime.stop().unwrap();
}

#[test]
fn failed_durable_save_shows_failure_and_does_not_play_success_cue() {
    use engine_audio::{AudioConfig, AudioEngine};
    use engine_project::{
        ActionFrame, Project, Runtime, Transform,
        persistence::{PersistenceStage, PersistenceStore},
        runtime::Persistence,
        ui::UiModel,
    };
    let user_data = user_data();
    let mut runtime =
        Runtime::new(Project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project.json")).unwrap())
            .unwrap();
    runtime.set_user_data_root(user_data.path().into()).unwrap();
    desktop_2d_gameplay::configure(runtime.app_mut()).unwrap();
    let audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    runtime.set_audio(audio.control()).unwrap();
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    let owner = runtime.entity("actor:player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(owner)
        .unwrap()
        .position = [-4.0, 2.0, 0.0];
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    let mut options = runtime
        .app()
        .world()
        .resource::<Persistence>()
        .0
        .options()
        .clone();
    options.fail = Some(|stage| stage == PersistenceStage::Write);
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Persistence(std::sync::Arc::new(
            PersistenceStore::new(options).unwrap(),
        )));
    let before = audio.voice_count();
    let mut input = ActionFrame::default();
    input.pressed.insert("rest".into());
    runtime.tick(1.0 / 60.0, input).unwrap();
    assert_eq!(audio.voice_count(), before);
    assert!(
        runtime
            .app()
            .world()
            .resource::<UiModel>()
            .panels()
            .iter()
            .any(|p| p.options.text.contains("SAVE FAILED"))
    );
    assert!(
        runtime
            .app()
            .world()
            .resource::<Persistence>()
            .0
            .load("desktop-slot", None)
            .is_err()
    );
    runtime.stop().unwrap();
    assert_eq!(audio.voice_count(), 0);
}

fn run_bin(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lab-walkthrough"))
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn restore_without_a_prior_new_session_fails_visibly() {
    let user_data = user_data();
    let output = run_bin(&["restore", "male", user_data.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], false);
}
#[test]
fn restoring_the_other_variant_branch_is_rejected() {
    let user_data = user_data();
    let path = user_data.path().to_str().unwrap();
    assert!(run_bin(&["new", "male", path]).status.success());
    let output = run_bin(&["restore", "female", path]);
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("wrong saved branch")
    );
}
#[test]
fn invalid_driver_arguments_are_rejected() {
    let user_data = user_data();
    let path = user_data.path().to_str().unwrap();
    for args in [["sideways", "male", path], ["new", "robot", path]] {
        let output = run_bin(&args);
        assert!(!output.status.success());
    }
    assert!(!run_bin(&["new"]).status.success());
}
#[test]
fn restore_is_idempotent_for_a_second_restart() {
    let user_data = user_data();
    let path = user_data.path().to_str().unwrap();
    assert!(run_bin(&["new", "female", path]).status.success());
    assert!(run_bin(&["restore", "female", path]).status.success());
    assert!(run_bin(&["restore", "female", path]).status.success());
}
