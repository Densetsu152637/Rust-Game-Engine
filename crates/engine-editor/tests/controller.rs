use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use engine_core::{Result, anyhow::bail};
use engine_editor::{EditorController, PlayHost, PlayStatus};
use engine_project::{EditOperation, MeshRenderer, Project};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    manifest: PathBuf,
    scene: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("document-")
            .tempdir()
            .unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts/player.rs"), "pub fn configure() {}\n").unwrap();
        fs::write(
            root.join("hero.mesh"),
            "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
        )
        .unwrap();
        fs::write(root.join("hero.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
        fs::write(root.join("project.json"),r#"{"schema":1,"name":"Editor fixture","startup_scene":"scenes/main.json","assets":[{"id":"asset:player","path":"scripts/player.rs","kind":"script"},{"id":"asset:mesh","path":"hero.mesh","kind":"mesh"},{"id":"asset:texture","path":"hero.ppm","kind":"texture"}]}"#).unwrap();
        fs::write(root.join("scenes/main.json"),r#"{"schema":1,"scene_id":"scene:main","entities":[{"id":"object:hero","name":"Hero","components":{"Transform":{"position":[0,0,0]}}}]}"#).unwrap();
        let manifest = root.join("project.json");
        let scene = root.join("scenes/main.json");
        Self {
            _dir: dir,
            manifest,
            scene,
        }
    }
}

#[derive(Default)]
struct HostState {
    captured: Option<Project>,
    starts: usize,
    stops: usize,
    fail_start: bool,
    fail_stop: bool,
    finished: bool,
}
struct Host(Rc<RefCell<HostState>>);
impl PlayHost for Host {
    fn start(&mut self, mut project: Project) -> Result<()> {
        let mut state = self.0.borrow_mut();
        state.starts += 1;
        if state.fail_start {
            bail!("compile rejected");
        }
        project.scene.entities[0]
            .components
            .transform
            .as_mut()
            .unwrap()
            .position[0] += 100.0;
        state.captured = Some(project);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        let mut state = self.0.borrow_mut();
        state.stops += 1;
        if state.fail_stop {
            bail!("generation still stopping");
        }
        state.captured = None;
        Ok(())
    }
    fn poll(&mut self) -> Result<PlayStatus> {
        if self.0.borrow().finished {
            Ok(PlayStatus::Stopped {
                success: false,
                message: "native generation crashed".into(),
            })
        } else {
            Ok(PlayStatus::Running)
        }
    }
}

fn controller(path: &Path) -> (EditorController, Rc<RefCell<HostState>>) {
    let state = Rc::new(RefCell::new(HostState::default()));
    let mut controller = EditorController::new(Box::new(Host(Rc::clone(&state))));
    controller.open(path, false).unwrap();
    (controller, state)
}

fn edit(controller: &mut EditorController, operation: EditOperation) -> Result<()> {
    let revision = controller.document().unwrap().revision().to_owned();
    controller.apply(operation, &revision)
}

fn move_hero(controller: &mut EditorController, x: f32) -> Result<()> {
    edit(
        controller,
        EditOperation::SetPosition {
            entity_id: "object:hero".into(),
            position: [x, 2.0, 3.0],
        },
    )
}

#[test]
fn typed_edits_history_assets_save_and_reload() -> Result<()> {
    let fixture = Fixture::new();
    let (mut controller, _) = controller(&fixture.manifest);
    assert!(!controller.is_dirty());
    move_hero(&mut controller, 4.0)?;
    assert!(controller.is_dirty());
    controller.undo()?;
    assert!(!controller.is_dirty());
    controller.redo()?;
    edit(
        &mut controller,
        EditOperation::SetScript {
            entity_id: "object:hero".into(),
            asset: Some("asset:player".into()),
        },
    )?;
    edit(
        &mut controller,
        EditOperation::SetMeshRenderer {
            entity_id: "object:hero".into(),
            value: Some(MeshRenderer {
                version: 1,
                mesh: "asset:mesh".into(),
                texture: Some("asset:texture".into()),
            }),
        },
    )?;
    let before = controller.document().unwrap().snapshot().clone();
    assert!(
        edit(
            &mut controller,
            EditOperation::SetMeshRenderer {
                entity_id: "object:hero".into(),
                value: Some(MeshRenderer {
                    version: 1,
                    mesh: "asset:player".into(),
                    texture: None
                })
            }
        )
        .is_err()
    );
    assert_eq!(controller.document().unwrap().snapshot(), &before);
    controller.save()?;
    assert!(!controller.is_dirty());
    controller.reload(false)?;
    assert_eq!(controller.document().unwrap().snapshot(), &before);
    assert!(controller.diagnostics().is_empty());
    Ok(())
}

#[test]
fn stale_revision_unsaved_guards_and_failed_replacement_preserve_document() -> Result<()> {
    let fixture = Fixture::new();
    let (mut controller, _) = controller(&fixture.manifest);
    let original = controller.document().unwrap().revision().to_owned();
    move_hero(&mut controller, 8.0)?;
    let before = controller.document().unwrap().snapshot().clone();
    assert!(
        controller
            .apply(
                EditOperation::SetPosition {
                    entity_id: "object:hero".into(),
                    position: [99.0; 3]
                },
                &original
            )
            .is_err()
    );
    assert!(controller.reload(false).is_err());
    assert!(controller.open(&fixture.manifest, false).is_err());
    assert!(controller.close(false).is_err());
    assert!(
        controller
            .open(fixture.manifest.with_file_name("missing.json"), true)
            .is_err()
    );
    fs::write(&fixture.scene, "invalid json")?;
    assert!(controller.reload(true).is_err());
    assert_eq!(controller.document().unwrap().snapshot(), &before);
    assert!(controller.is_dirty());
    Ok(())
}

#[test]
fn play_receives_unsaved_clone_and_runtime_mutation_never_replaces_authoring() -> Result<()> {
    let fixture = Fixture::new();
    let source = fs::read(&fixture.scene)?;
    let (mut controller, host) = controller(&fixture.manifest);
    move_hero(&mut controller, 7.0)?;
    let before = controller.document().unwrap().snapshot().clone();
    controller.play()?;
    assert!(controller.is_playing());
    assert_eq!(
        host.borrow().captured.as_ref().unwrap().scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        107.0
    );
    assert_eq!(controller.document().unwrap().snapshot(), &before);
    assert_eq!(fs::read(&fixture.scene)?, source);
    assert!(controller.play().is_err());
    controller.stop()?;
    assert!(!controller.is_playing());
    assert!(!controller.document().unwrap().is_playing());
    assert_eq!(controller.document().unwrap().snapshot(), &before);
    assert!(controller.is_dirty());
    Ok(())
}

#[test]
fn failed_compile_releases_play_and_failed_stop_preserves_retry_state() -> Result<()> {
    let fixture = Fixture::new();
    let (mut controller, host) = controller(&fixture.manifest);
    host.borrow_mut().fail_start = true;
    assert!(controller.play().is_err());
    assert!(!controller.is_playing());
    assert!(!controller.document().unwrap().is_playing());
    assert!(controller.status().contains("compile rejected"));
    host.borrow_mut().fail_start = false;
    controller.play()?;
    host.borrow_mut().fail_stop = true;
    assert!(controller.stop().is_err());
    assert!(controller.is_playing());
    assert!(controller.document().unwrap().is_playing());
    host.borrow_mut().fail_stop = false;
    controller.stop()?;
    assert!(!controller.is_playing());
    Ok(())
}

#[test]
fn completed_or_crashed_generation_returns_to_authoring_and_logs_diagnostic() -> Result<()> {
    let fixture = Fixture::new();
    let (mut controller, host) = controller(&fixture.manifest);
    controller.play()?;
    host.borrow_mut().finished = true;
    assert!(controller.poll().is_err());
    assert!(!controller.is_playing());
    assert!(controller.diagnostics()[0].contains("crashed"));
    controller.play()?;
    controller.stop()?;
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore = "requires an interactive Windows desktop; run explicitly for native acceptance"]
fn real_native_controls_smoke_closes_without_saving() -> Result<()> {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.scene)?;
    let host = Rc::new(RefCell::new(HostState::default()));
    let report = engine_editor::native_smoke(&fixture.manifest, Box::new(Host(host)))?;
    assert!(report.controls_created >= 20);
    assert_eq!(report.entities_listed, 1);
    assert!(report.picker_edit_applied);
    assert!(report.window_closed);
    assert_eq!(fs::read(&fixture.scene)?, original);
    Ok(())
}
