//! Platform-independent editor controller behaviour: open/reload/save, edit
//! history and dirty tracking, selection, play isolation, polling and teardown.
//! No Win32 window is created; the play host is an in-memory fake.

use std::{cell::RefCell, collections::VecDeque, fs, path::PathBuf, rc::Rc};

use engine_core::{Result, anyhow::bail};
use engine_editor::{EditorController, PlayHost, PlayStatus};
use engine_project::{EditOperation, MeshRenderer, Project};
use tempfile::TempDir;

const SCENE: &str = r#"{"schema":1,"scene_id":"scene:main","entities":[
    {"id":"object:hero","name":"Hero","components":{"Transform":{"position":[0,0,0]}}},
    {"id":"object:rock","name":"Rock","components":{"Transform":{"position":[5,0,0]}}},
    {"id":"object:ghost","name":"Ghost","components":{}}
]}"#;
const EMPTY_SCENE: &str = r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#;

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    manifest: PathBuf,
    scene: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::with_scene(SCENE)
    }
    fn with_scene(scene_json: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("editor-")
            .tempdir()
            .unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts/player.rs"), "pub fn configure() {}\n").unwrap();
        fs::write(root.join("scripts/enemy.rs"), "pub fn configure() {}\n").unwrap();
        fs::write(
            root.join("hero.mesh"),
            "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
        )
        .unwrap();
        fs::write(root.join("hero.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
        fs::write(
            root.join("project.json"),
            r#"{"schema":1,"name":"Editor fixture","startup_scene":"scenes/main.json","assets":[
                {"id":"asset:player","path":"scripts/player.rs","kind":"script"},
                {"id":"asset:enemy","path":"scripts/enemy.rs","kind":"script"},
                {"id":"asset:mesh","path":"hero.mesh","kind":"mesh"},
                {"id":"asset:texture","path":"hero.ppm","kind":"texture"}]}"#,
        )
        .unwrap();
        fs::write(root.join("scenes/main.json"), scene_json).unwrap();
        Self {
            manifest: root.join("project.json"),
            scene: root.join("scenes/main.json"),
            root,
            _dir: dir,
        }
    }
}

#[derive(Default)]
struct HostState {
    captured: Option<Project>,
    starts: usize,
    replacements: Vec<Project>,
    reload_cancellations: usize,
    fail_reload: bool,
    fail_cancel_reload: bool,
    stops: usize,
    polls: usize,
    fail_start: bool,
    fail_stop: bool,
    script: VecDeque<std::result::Result<PlayStatus, String>>,
}
type Shared = Rc<RefCell<HostState>>;
struct Host(Shared);
impl PlayHost for Host {
    fn start(&mut self, project: Project) -> Result<()> {
        let mut s = self.0.borrow_mut();
        s.starts += 1;
        if s.fail_start {
            bail!("compile rejected");
        }
        s.captured = Some(project);
        Ok(())
    }
    fn reload(&mut self, project: Project) -> Result<()> {
        let mut state = self.0.borrow_mut();
        if state.fail_reload {
            bail!("replacement rejected");
        }
        state.replacements.push(project);
        Ok(())
    }
    fn cancel_reload(&mut self) -> Result<()> {
        let mut state = self.0.borrow_mut();
        if state.fail_cancel_reload {
            bail!("replacement still cancelling");
        }
        state.reload_cancellations += 1;
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        let mut s = self.0.borrow_mut();
        s.stops += 1;
        if s.fail_stop {
            bail!("generation still stopping");
        }
        s.captured = None;
        Ok(())
    }
    fn poll(&mut self) -> Result<PlayStatus> {
        let mut s = self.0.borrow_mut();
        s.polls += 1;
        match s.script.pop_front() {
            Some(Ok(status)) => Ok(status),
            Some(Err(message)) => bail!(message),
            None => Ok(PlayStatus::Running),
        }
    }
}

fn unopened() -> (EditorController, Shared) {
    let state = Shared::default();
    (EditorController::new(Box::new(Host(state.clone()))), state)
}
fn opened(fixture: &Fixture) -> (EditorController, Shared) {
    let (mut controller, state) = unopened();
    controller.open(&fixture.manifest, false).unwrap();
    (controller, state)
}
fn revision(c: &EditorController) -> String {
    c.document().unwrap().revision().to_owned()
}
fn edit(c: &mut EditorController, operation: EditOperation) -> Result<()> {
    let rev = revision(c);
    c.apply(operation, &rev)
}
fn set_position(id: &str, x: f32) -> EditOperation {
    EditOperation::SetPosition {
        entity_id: id.into(),
        position: [x, 1.0, 2.0],
    }
}
fn move_hero(c: &mut EditorController, x: f32) -> Result<()> {
    edit(c, set_position("object:hero", x))
}
fn hero_x(c: &EditorController) -> f32 {
    c.document().unwrap().snapshot().entities[0]
        .components
        .transform
        .as_ref()
        .unwrap()
        .position[0]
}
fn error_text(result: Result<()>) -> String {
    format!("{:#}", result.expect_err("operation must fail"))
}

// ---- no project ------------------------------------------------------------------------

#[test]
fn new_controller_starts_empty_with_a_status_line() {
    let (c, _) = unopened();
    assert!(c.document().is_none());
    assert!(c.manifest().is_none());
    assert!(c.selected().is_none());
    assert!(!c.is_dirty() && !c.is_playing());
    assert_eq!(c.status(), "No project open");
    assert!(c.diagnostics().is_empty() && c.log().is_empty());
}

macro_rules! needs_a_project {
    ($($name:ident: $call:expr;)*) => {$(
        #[test]
        fn $name() {
            let (mut c, host) = unopened();
            let call: fn(&mut EditorController) -> Result<()> = $call;
            assert!(error_text(call(&mut c)).contains("no project is open"));
            assert_eq!(host.borrow().starts, 0);
            assert!(c.status().contains("no project is open"));
            assert_eq!(c.diagnostics().len(), 1);
        }
    )*};
}

needs_a_project! {
    apply_without_a_project_fails: |c| c.apply(set_position("object:hero", 1.0), "rev");
    save_without_a_project_fails: |c| c.save();
    undo_without_a_project_fails: |c| c.undo();
    redo_without_a_project_fails: |c| c.redo();
    play_without_a_project_fails: |c| c.play();
}

#[test]
fn reload_without_a_project_fails() {
    let (mut c, _) = unopened();
    assert!(error_text(c.reload(true)).contains("no project is open"));
}

#[test]
fn stop_poll_and_close_without_a_project_are_harmless() {
    let (mut c, host) = unopened();
    c.stop().unwrap();
    c.poll().unwrap();
    c.close(false).unwrap();
    assert_eq!((host.borrow().stops, host.borrow().polls), (0, 0));
    assert_eq!(c.status(), "Editor closed");
}

#[test]
fn selecting_an_entity_without_a_project_fails_but_clearing_works() {
    let (mut c, _) = unopened();
    assert!(c.select(Some("object:hero".into())).is_err());
    c.select(None).unwrap();
}

// ---- open / reload ---------------------------------------------------------------------

#[test]
fn open_loads_the_document_and_selects_the_first_entity() {
    let f = Fixture::new();
    let (c, _) = opened(&f);
    assert_eq!(c.manifest(), Some(f.manifest.as_path()));
    assert_eq!(c.selected(), Some("object:hero"));
    assert_eq!(c.document().unwrap().snapshot().entities.len(), 3);
    assert_eq!(c.status(), "Project opened");
    assert_eq!(c.log().back().map(String::as_str), Some("Project opened"));
}

#[test]
fn opening_an_empty_scene_selects_nothing() {
    let f = Fixture::with_scene(EMPTY_SCENE);
    let (c, _) = opened(&f);
    assert!(c.selected().is_none());
    assert!(c.document().unwrap().snapshot().entities.is_empty());
}

#[test]
fn opening_a_missing_manifest_fails_and_leaves_the_controller_empty() {
    let f = Fixture::new();
    let (mut c, _) = unopened();
    assert!(c.open(f.root.join("nope.json"), false).is_err());
    assert!(c.document().is_none() && c.manifest().is_none());
    assert!(!c.diagnostics().is_empty());
}

#[test]
fn opening_a_corrupt_scene_keeps_the_previous_document() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    let other = Fixture::with_scene("{ broken");
    assert!(c.open(&other.manifest, false).is_err());
    assert_eq!(c.manifest(), Some(f.manifest.as_path()));
    assert_eq!(c.document().unwrap().snapshot().entities.len(), 3);
}

#[test]
fn opening_another_project_replaces_manifest_document_and_selection() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    c.select(Some("object:rock".into())).unwrap();
    let other = Fixture::with_scene(EMPTY_SCENE);
    c.open(&other.manifest, false).unwrap();
    assert_eq!(c.manifest(), Some(other.manifest.as_path()));
    assert!(c.selected().is_none());
}

#[test]
fn opening_over_unsaved_edits_requires_explicit_discard() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 9.0).unwrap();
    let other = Fixture::with_scene(EMPTY_SCENE);
    assert!(error_text(c.open(&other.manifest, false)).contains("unsaved edits"));
    assert_eq!(c.manifest(), Some(f.manifest.as_path()));
    assert!(c.is_dirty());
    c.open(&other.manifest, true).unwrap();
    assert!(!c.is_dirty());
    assert_eq!(c.manifest(), Some(other.manifest.as_path()));
}

#[test]
fn reload_picks_up_external_changes_when_clean() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    fs::write(&f.scene, EMPTY_SCENE).unwrap();
    c.reload(false).unwrap();
    assert!(c.document().unwrap().snapshot().entities.is_empty());
    assert!(c.selected().is_none());
}

#[test]
fn reload_refuses_to_discard_unsaved_edits_without_permission() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 3.0).unwrap();
    assert!(c.reload(false).is_err());
    assert_eq!(hero_x(&c), 3.0);
    c.reload(true).unwrap();
    assert_eq!(hero_x(&c), 0.0);
    assert!(!c.is_dirty());
}

#[test]
fn reload_discards_undo_history() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 3.0).unwrap();
    c.save().unwrap();
    c.reload(false).unwrap();
    assert!(error_text(c.undo()).contains("Nothing to undo"));
}

#[test]
fn failed_reload_keeps_the_in_memory_document() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 6.0).unwrap();
    fs::write(&f.scene, "not json").unwrap();
    assert!(c.reload(true).is_err());
    assert_eq!(hero_x(&c), 6.0);
    assert!(c.is_dirty());
}

#[test]
fn reload_reselects_the_first_entity() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    c.select(Some("object:ghost".into())).unwrap();
    c.reload(false).unwrap();
    assert_eq!(c.selected(), Some("object:hero"));
}

// ---- selection -------------------------------------------------------------------------

#[test]
fn select_accepts_existing_entities_and_none() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    c.select(Some("object:rock".into())).unwrap();
    assert_eq!(c.selected(), Some("object:rock"));
    c.select(None).unwrap();
    assert!(c.selected().is_none());
}

#[test]
fn selecting_an_unknown_entity_fails_and_keeps_the_selection() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(error_text(c.select(Some("object:nobody".into()))).contains("does not exist"));
    assert_eq!(c.selected(), Some("object:hero"));
}

#[test]
fn selection_survives_edits_and_undo() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    c.select(Some("object:rock".into())).unwrap();
    edit(&mut c, set_position("object:rock", 8.0)).unwrap();
    c.undo().unwrap();
    assert_eq!(c.selected(), Some("object:rock"));
}

// ---- edits, history, dirty tracking ----------------------------------------------------

#[test]
fn edits_change_the_snapshot_and_revision() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    let before = revision(&c);
    move_hero(&mut c, 4.0).unwrap();
    assert_eq!(hero_x(&c), 4.0);
    assert_ne!(revision(&c), before);
    assert_eq!(c.status(), "Scene edit applied");
}

#[test]
fn dirty_flag_follows_undo_redo_and_save() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(!c.is_dirty());
    move_hero(&mut c, 1.0).unwrap();
    assert!(c.is_dirty());
    c.undo().unwrap();
    assert!(!c.is_dirty(), "undoing back to the loaded state is clean");
    c.redo().unwrap();
    assert!(c.is_dirty());
    c.save().unwrap();
    assert!(!c.is_dirty());
    c.undo().unwrap();
    assert!(c.is_dirty(), "undo past the saved state is dirty");
    c.redo().unwrap();
    assert!(!c.is_dirty());
}

#[test]
fn an_edit_that_changes_nothing_leaves_the_document_clean() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    edit(
        &mut c,
        EditOperation::SetPosition {
            entity_id: "object:hero".into(),
            position: [0.0; 3],
        },
    )
    .unwrap();
    assert!(!c.is_dirty());
}

#[test]
fn stale_revisions_are_rejected_without_changing_the_scene() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    let stale = revision(&c);
    move_hero(&mut c, 1.0).unwrap();
    let error = error_text(c.apply(set_position("object:hero", 50.0), &stale));
    assert!(error.contains("revision"), "{error}");
    assert_eq!(hero_x(&c), 1.0);
    assert!(c.diagnostics()[0].contains("revision"));
}

#[test]
fn garbage_revisions_are_rejected() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(c.apply(set_position("object:hero", 2.0), "").is_err());
    assert!(
        c.apply(set_position("object:hero", 2.0), "not-a-revision")
            .is_err()
    );
    assert_eq!(hero_x(&c), 0.0);
}

#[test]
fn editing_an_unknown_entity_fails() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(
        error_text(edit(&mut c, set_position("object:nobody", 1.0))).contains("Unknown entity")
    );
    assert!(!c.is_dirty());
}

#[test]
fn position_edit_requires_a_transform_component() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(error_text(edit(&mut c, set_position("object:ghost", 1.0))).contains("no Transform"));
}

#[test]
fn script_edit_accepts_script_assets_and_clears_with_none() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    let set = |asset: Option<&str>| EditOperation::SetScript {
        entity_id: "object:hero".into(),
        asset: asset.map(Into::into),
    };
    edit(&mut c, set(Some("asset:player"))).unwrap();
    let script = c.document().unwrap().snapshot().entities[0]
        .components
        .script
        .clone();
    assert_eq!(script.unwrap().asset, "asset:player");
    edit(&mut c, set(None)).unwrap();
    assert!(
        c.document().unwrap().snapshot().entities[0]
            .components
            .script
            .is_none()
    );
}

#[test]
fn script_edit_rejects_unknown_and_wrong_kind_assets() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    for asset in ["asset:ghost", "asset:mesh", "asset:texture", ""] {
        let op = EditOperation::SetScript {
            entity_id: "object:hero".into(),
            asset: Some(asset.into()),
        };
        assert!(edit(&mut c, op).is_err(), "{asset:?}");
    }
    assert!(!c.is_dirty());
}

#[test]
fn mesh_renderer_edit_validates_asset_kinds() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    let renderer = |mesh: &str, texture: Option<&str>| EditOperation::SetMeshRenderer {
        entity_id: "object:hero".into(),
        value: Some(MeshRenderer {
            version: 1,
            mesh: mesh.into(),
            texture: texture.map(Into::into),
        }),
    };
    assert!(
        edit(&mut c, renderer("asset:player", None)).is_err(),
        "script is not a mesh"
    );
    assert!(
        edit(&mut c, renderer("asset:mesh", Some("asset:mesh"))).is_err(),
        "mesh is not a texture"
    );
    assert!(edit(&mut c, renderer("asset:missing", None)).is_err());
    edit(&mut c, renderer("asset:mesh", Some("asset:texture"))).unwrap();
    edit(
        &mut c,
        EditOperation::SetMeshRenderer {
            entity_id: "object:hero".into(),
            value: None,
        },
    )
    .unwrap();
    assert!(
        c.document().unwrap().snapshot().entities[0]
            .components
            .mesh_renderer
            .is_none()
    );
}

#[test]
fn rejected_edits_do_not_create_history_entries() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(edit(&mut c, set_position("object:nobody", 1.0)).is_err());
    assert!(error_text(c.undo()).contains("Nothing to undo"));
}

#[test]
fn undo_and_redo_report_empty_history() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(error_text(c.undo()).contains("Nothing to undo"));
    assert!(error_text(c.redo()).contains("Nothing to redo"));
}

#[test]
fn a_new_edit_discards_the_redo_branch() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 1.0).unwrap();
    c.undo().unwrap();
    move_hero(&mut c, 2.0).unwrap();
    assert!(error_text(c.redo()).contains("Nothing to redo"));
    assert_eq!(hero_x(&c), 2.0);
}

#[test]
fn full_undo_then_full_redo_restores_every_state() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    for x in [1.0, 2.0, 3.0, 4.0] {
        move_hero(&mut c, x).unwrap();
    }
    for expected in [3.0, 2.0, 1.0, 0.0] {
        c.undo().unwrap();
        assert_eq!(hero_x(&c), expected);
    }
    for expected in [1.0, 2.0, 3.0, 4.0] {
        c.redo().unwrap();
        assert_eq!(hero_x(&c), expected);
    }
}

#[test]
fn undo_history_is_bounded_to_1024_steps() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    for step in 1..=1030 {
        move_hero(&mut c, step as f32).unwrap();
    }
    for _ in 0..1024 {
        c.undo().unwrap();
    }
    assert!(c.undo().is_err());
    assert_eq!(hero_x(&c), 6.0, "the six oldest states were evicted");
}

// ---- save ------------------------------------------------------------------------------

#[test]
fn save_persists_the_scene_and_reload_reads_it_back() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 12.5).unwrap();
    c.save().unwrap();
    assert!(fs::read_to_string(&f.scene).unwrap().contains("12.5"));
    let (fresh, _) = opened(&f);
    assert_eq!(hero_x(&fresh), 12.5);
}

#[test]
fn unsaved_edits_never_reach_disk() {
    let f = Fixture::new();
    let before = fs::read(&f.scene).unwrap();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 12.5).unwrap();
    c.undo().unwrap();
    c.redo().unwrap();
    assert_eq!(fs::read(&f.scene).unwrap(), before);
}

#[test]
fn saving_a_clean_document_succeeds() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    c.save().unwrap();
    assert_eq!(c.status(), "Scene saved");
    assert!(!c.is_dirty());
}

#[test]
fn save_only_rewrites_the_scene_file() {
    let f = Fixture::new();
    let manifest_before = fs::read(&f.manifest).unwrap();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 3.0).unwrap();
    c.save().unwrap();
    assert_eq!(fs::read(&f.manifest).unwrap(), manifest_before);
    let mut names: Vec<_> = fs::read_dir(f.root.join("scenes"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["main.json"], "no temporary files are left behind");
}

#[test]
fn save_failure_is_reported_and_keeps_the_document_dirty() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 3.0).unwrap();
    fs::remove_file(&f.scene).unwrap();
    fs::create_dir(&f.scene).unwrap();
    assert!(c.save().is_err());
    assert!(c.is_dirty());
    assert_eq!(hero_x(&c), 3.0);
    assert!(!c.diagnostics().is_empty());
}

// ---- play isolation --------------------------------------------------------------------

#[test]
fn play_hands_the_host_a_clone_with_unsaved_edits() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    move_hero(&mut c, 7.0).unwrap();
    c.play().unwrap();
    let state = host.borrow();
    let project = state.captured.as_ref().unwrap();
    assert_eq!(
        project.scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        7.0
    );
    assert_eq!(state.starts, 1);
    assert!(c.is_playing() && c.document().unwrap().is_playing());
}

#[test]
fn host_mutations_of_the_clone_never_reach_the_authored_document() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut()
        .captured
        .as_mut()
        .unwrap()
        .scene
        .entities
        .clear();
    assert_eq!(c.document().unwrap().snapshot().entities.len(), 3);
    assert!(!c.is_dirty());
}

#[test]
fn play_never_writes_the_scene_file() {
    let f = Fixture::new();
    let before = fs::read(&f.scene).unwrap();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 7.0).unwrap();
    c.play().unwrap();
    c.stop().unwrap();
    assert_eq!(fs::read(&f.scene).unwrap(), before);
}

#[test]
fn edits_made_during_play_do_not_alter_the_running_clone() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    move_hero(&mut c, 33.0).unwrap();
    let state = host.borrow();
    let running = state.captured.as_ref().unwrap();
    assert_eq!(
        running.scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        0.0
    );
    drop(state);
    assert_eq!(hero_x(&c), 33.0);
}

#[test]
fn starting_a_second_play_session_is_refused() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    assert!(error_text(c.play()).contains("already active"));
    assert_eq!(host.borrow().starts, 1);
    assert!(c.is_playing());
}

#[test]
fn rejected_compile_releases_the_play_lock() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    host.borrow_mut().fail_start = true;
    assert!(error_text(c.play()).contains("compile rejected"));
    assert!(!c.is_playing() && !c.document().unwrap().is_playing());
    host.borrow_mut().fail_start = false;
    c.play().unwrap();
    assert_eq!(host.borrow().starts, 2);
}

#[test]
fn stop_discards_the_runtime_and_keeps_authoring_state() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    move_hero(&mut c, 2.0).unwrap();
    c.play().unwrap();
    c.stop().unwrap();
    assert!(!c.is_playing() && !c.document().unwrap().is_playing());
    assert!(host.borrow().captured.is_none());
    assert_eq!(hero_x(&c), 2.0);
    assert!(c.is_dirty());
    assert_eq!(c.status(), "Play stopped; runtime state discarded");
}

#[test]
fn stop_without_play_does_not_call_the_host() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.stop().unwrap();
    assert_eq!(host.borrow().stops, 0);
}

#[test]
fn failed_stop_keeps_play_active_and_can_be_retried() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut().fail_stop = true;
    assert!(error_text(c.stop()).contains("still stopping"));
    assert!(c.is_playing() && c.document().unwrap().is_playing());
    host.borrow_mut().fail_stop = false;
    c.stop().unwrap();
    assert!(!c.is_playing());
    assert_eq!(host.borrow().stops, 2);
}

#[test]
fn play_can_restart_after_stop() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    c.stop().unwrap();
    c.play().unwrap();
    assert_eq!(host.borrow().starts, 2);
    assert!(c.is_playing());
}

// ---- polling ---------------------------------------------------------------------------

#[test]
fn polling_when_not_playing_does_not_call_the_host() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.poll().unwrap();
    assert_eq!(host.borrow().polls, 0);
}

#[test]
fn compiling_and_running_poll_results_update_the_status() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut()
        .script
        .extend([Ok(PlayStatus::Compiling), Ok(PlayStatus::Running)]);
    c.poll().unwrap();
    assert!(c.status().starts_with("Compiling native runtime"));
    c.poll().unwrap();
    assert!(c.status().starts_with("Native play running"));
    assert!(c.is_playing());
}

#[test]
fn successful_completion_returns_to_authoring() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut().script.push_back(Ok(PlayStatus::Stopped {
        success: true,
        message: "exit 0".into(),
    }));
    c.poll().unwrap();
    assert!(!c.is_playing() && !c.document().unwrap().is_playing());
    assert_eq!(c.status(), "Play finished: exit 0");
    assert!(c.diagnostics().is_empty());
}

#[test]
fn crashed_generation_returns_to_authoring_with_a_diagnostic() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut().script.push_back(Ok(PlayStatus::Stopped {
        success: false,
        message: "segfault".into(),
    }));
    assert!(error_text(c.poll()).contains("segfault"));
    assert!(!c.is_playing());
    assert_eq!(c.diagnostics(), ["segfault"]);
    assert_eq!(
        host.borrow().stops,
        0,
        "an exited process needs no stop call"
    );
}

#[test]
fn poll_errors_are_reported_but_play_stays_active() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut()
        .script
        .push_back(Err("status pipe broken".into()));
    assert!(error_text(c.poll()).contains("status pipe broken"));
    assert!(c.is_playing());
    c.poll().unwrap();
}

#[test]
fn polling_after_completion_does_nothing() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut().script.push_back(Ok(PlayStatus::Stopped {
        success: true,
        message: "done".into(),
    }));
    c.poll().unwrap();
    c.poll().unwrap();
    assert_eq!(host.borrow().polls, 1);
}

// ---- interaction of play with other commands ------------------------------------------

#[test]
fn opening_a_project_stops_active_play_first() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    let other = Fixture::with_scene(EMPTY_SCENE);
    c.open(&other.manifest, false).unwrap();
    assert!(!c.is_playing());
    assert_eq!(host.borrow().stops, 1);
    assert_eq!(c.manifest(), Some(other.manifest.as_path()));
}

#[test]
fn a_failed_open_does_not_disturb_active_play() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    assert!(c.open(f.root.join("missing.json"), false).is_err());
    assert!(c.is_playing());
    assert_eq!(host.borrow().stops, 0);
}

#[test]
fn open_keeps_the_old_document_when_the_host_cannot_stop() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut().fail_stop = true;
    let other = Fixture::with_scene(EMPTY_SCENE);
    assert!(c.open(&other.manifest, false).is_err());
    assert_eq!(c.manifest(), Some(f.manifest.as_path()));
    assert_eq!(c.document().unwrap().snapshot().entities.len(), 3);
    assert!(c.is_playing());
}

#[test]
fn reload_stops_play_and_restores_the_disk_state() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    c.reload(false).unwrap();
    assert!(!c.is_playing());
    assert_eq!(host.borrow().stops, 1);
}

#[test]
fn close_without_unsaved_edits_stops_play() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    c.close(false).unwrap();
    assert!(!c.is_playing());
    assert_eq!(host.borrow().stops, 1);
    assert_eq!(c.status(), "Editor closed");
}

#[test]
fn close_with_unsaved_edits_is_refused_and_play_continues() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    move_hero(&mut c, 1.0).unwrap();
    c.play().unwrap();
    assert!(error_text(c.close(false)).contains("unsaved edits"));
    assert!(c.is_playing());
    assert_eq!(host.borrow().stops, 0);
}

#[test]
fn close_with_discard_stops_play_even_when_dirty() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    move_hero(&mut c, 1.0).unwrap();
    c.play().unwrap();
    c.close(true).unwrap();
    assert!(!c.is_playing());
    assert_eq!(host.borrow().stops, 1);
}

#[test]
fn close_reports_a_host_that_will_not_stop() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    host.borrow_mut().fail_stop = true;
    assert!(c.close(false).is_err());
    assert!(c.is_playing());
}

#[test]
fn dropping_the_controller_stops_a_running_generation() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    drop(c);
    assert_eq!(host.borrow().stops, 1);
}

#[test]
fn dropping_an_idle_controller_never_touches_the_host() {
    let f = Fixture::new();
    let (c, host) = opened(&f);
    drop(c);
    assert_eq!(host.borrow().stops, 0);
}

#[test]
fn dropping_after_an_explicit_stop_does_not_stop_twice() {
    let f = Fixture::new();
    let (mut c, host) = opened(&f);
    c.play().unwrap();
    c.stop().unwrap();
    drop(c);
    assert_eq!(host.borrow().stops, 1);
}

// ---- status, diagnostics and log -------------------------------------------------------

#[test]
fn success_clears_previous_diagnostics() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(c.undo().is_err());
    assert_eq!(c.diagnostics().len(), 1);
    move_hero(&mut c, 1.0).unwrap();
    assert!(c.diagnostics().is_empty());
}

#[test]
fn failures_replace_the_status_with_the_error_text() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    assert!(c.redo().is_err());
    assert!(c.status().contains("Nothing to redo"));
    assert_eq!(c.log().back().unwrap(), c.status());
}

#[test]
fn every_operation_appends_to_the_log_in_order() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    move_hero(&mut c, 1.0).unwrap();
    c.undo().unwrap();
    c.redo().unwrap();
    c.save().unwrap();
    let log: Vec<_> = c.log().iter().map(String::as_str).collect();
    assert_eq!(
        log,
        [
            "Project opened",
            "Scene edit applied",
            "Undo applied",
            "Redo applied",
            "Scene saved"
        ]
    );
}

#[test]
fn log_keeps_only_the_newest_200_entries() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    for step in 0..250 {
        move_hero(&mut c, (step + 1) as f32).unwrap();
    }
    assert_eq!(c.log().len(), 200);
    assert_eq!(c.log().front().unwrap(), "Scene edit applied");
    assert!(
        !c.log().iter().any(|m| m == "Project opened"),
        "the oldest entry was evicted"
    );
}

#[test]
fn play_status_messages_are_distinct_for_each_phase() {
    let f = Fixture::new();
    let (mut c, _) = opened(&f);
    c.play().unwrap();
    let playing = c.status().to_owned();
    c.stop().unwrap();
    assert_ne!(playing, c.status());
    assert!(playing.contains("authored scene is unchanged"));
}

#[test]
fn play_status_value_semantics() {
    assert_eq!(PlayStatus::Running, PlayStatus::Running);
    assert_ne!(PlayStatus::Running, PlayStatus::Compiling);
    assert_ne!(
        PlayStatus::Stopped {
            success: true,
            message: "a".into()
        },
        PlayStatus::Stopped {
            success: false,
            message: "a".into()
        }
    );
    assert!(format!("{:?}", PlayStatus::Compiling).contains("Compiling"));
}

#[test]
fn reload_game_snapshots_unsaved_edits_and_rejection_keeps_play_and_history() {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.scene).unwrap();
    let (mut controller, host) = opened(&fixture);
    assert!(controller.reload_play().is_err());
    controller.play().unwrap();
    assert!(!controller.can_reload_play());
    controller.poll().unwrap();
    assert!(controller.can_reload_play());
    edit(
        &mut controller,
        EditOperation::SetPosition {
            entity_id: "object:hero".into(),
            position: [7.0, 0.0, 0.0],
        },
    )
    .unwrap();
    let revision = revision(&controller);
    controller.reload_play().unwrap();
    assert!(controller.is_reloading_play());
    assert!(!controller.can_reload_play());
    assert!(controller.reload_play().is_err());
    assert_eq!(host.borrow().replacements.len(), 1);
    assert_eq!(
        host.borrow().replacements[0].scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        7.0
    );
    host.borrow_mut()
        .script
        .push_back(Ok(PlayStatus::ReloadFailed {
            message: "compile error; previous game continues".into(),
        }));
    assert!(controller.poll().is_err());
    assert!(controller.is_playing() && controller.document().unwrap().is_playing());
    assert!(!controller.is_reloading_play() && controller.can_reload_play());
    assert_eq!(controller.document().unwrap().revision(), revision);
    assert!(!controller.diagnostics().is_empty());
    controller.poll().unwrap();
    assert!(
        !controller.diagnostics().is_empty(),
        "running polls retain failure diagnostics"
    );
    assert_eq!(host.borrow().stops, 0);
    assert_eq!(fs::read(&fixture.scene).unwrap(), original);
    controller.undo().unwrap();
    assert!(!controller.is_dirty());
}

#[test]
fn reload_success_and_cancel_leave_document_in_play_until_stop() {
    let fixture = Fixture::new();
    let (mut controller, host) = opened(&fixture);
    controller.play().unwrap();
    controller.poll().unwrap();
    controller.reload_play().unwrap();
    host.borrow_mut()
        .script
        .push_back(Ok(PlayStatus::Reloading));
    controller.poll().unwrap();
    assert!(controller.is_reloading_play());
    controller.cancel_play_reload().unwrap();
    assert_eq!(host.borrow().reload_cancellations, 1);
    assert!(controller.is_playing() && controller.document().unwrap().is_playing());
    assert!(controller.can_reload_play());
    controller.reload_play().unwrap();
    controller.poll().unwrap();
    assert!(!controller.is_reloading_play());
    assert!(controller.status().contains("Game reloaded"));
    controller.reload_play().unwrap();
    controller.stop().unwrap();
    assert!(
        !controller.is_playing()
            && !controller.is_reloading_play()
            && !controller.can_reload_play()
    );
    assert!(!controller.document().unwrap().is_playing());
    assert_eq!(host.borrow().stops, 1);
}

#[test]
fn synchronous_reload_errors_and_terminal_teardown_failures_are_truthful() {
    let fixture = Fixture::new();
    let (mut controller, host) = opened(&fixture);
    controller.play().unwrap();
    controller.poll().unwrap();
    host.borrow_mut().fail_reload = true;
    assert!(controller.reload_play().is_err());
    assert!(controller.can_reload_play() && controller.is_playing());
    host.borrow_mut().fail_reload = false;
    controller.reload_play().unwrap();
    host.borrow_mut().fail_cancel_reload = true;
    assert!(controller.cancel_play_reload().is_err());
    assert!(controller.is_reloading_play());
    host.borrow_mut().script.push_back(Ok(PlayStatus::Stopped {
        success: false,
        message: "old teardown failed".into(),
    }));
    assert!(controller.poll().is_err());
    assert!(
        !controller.is_playing()
            && !controller.is_reloading_play()
            && !controller.can_reload_play()
    );
    assert!(!controller.document().unwrap().is_playing());
}
