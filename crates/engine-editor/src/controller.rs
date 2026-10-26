use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use engine_core::{
    Result,
    anyhow::{anyhow, ensure},
};
use engine_project::{EditOperation, Project, SceneDocument};

/// Native runtime state reported by the compiler/launcher owned by tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayStatus {
    Compiling,
    Running,
    /// The previous game runs while a candidate compiles/reaches readiness.
    Reloading,
    /// Candidate rejected before teardown; the previous game is still active.
    ReloadFailed {
        message: String,
    },
    Stopped {
        success: bool,
        message: String,
    },
}

/// Tooling supplies native compilation and process ownership. `start` receives an
/// isolated clone containing unsaved authored edits. It must not alter the source
/// project files. `stop` must release the generation before reporting success.
/// Hosts queue compilation in a worker. Polling activation may wait for the
/// previous generation's bounded graceful teardown before starting a candidate.
pub trait PlayHost {
    fn start(&mut self, project: Project) -> Result<()>;
    /// Prepare a replacement without stopping the running game. Activation
    /// happens at poll after readiness and successful previous-game teardown.
    fn reload(&mut self, project: Project) -> Result<()>;
    /// Discard only the pending candidate; retain the previous game.
    fn cancel_reload(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn poll(&mut self) -> Result<PlayStatus>;
}

/// Safe editor state independent of native controls. Failed open/reload retains
/// the current document; all edits use optimistic revision checks from the model.
pub struct EditorController {
    document: Option<SceneDocument>,
    manifest: Option<PathBuf>,
    /// Selected entity IDs in selection order; the last is the active entity.
    selection: Vec<String>,
    host: Box<dyn PlayHost>,
    playing: bool,
    runtime_running: bool,
    reloading: bool,
    status: String,
    diagnostics: Vec<String>,
    log: VecDeque<String>,
}

impl EditorController {
    pub fn new(host: Box<dyn PlayHost>) -> Self {
        Self {
            document: None,
            manifest: None,
            selection: Vec::new(),
            host,
            playing: false,
            runtime_running: false,
            reloading: false,
            status: "No project open".into(),
            diagnostics: Vec::new(),
            log: VecDeque::new(),
        }
    }

    pub fn document(&self) -> Option<&SceneDocument> {
        self.document.as_ref()
    }
    pub fn manifest(&self) -> Option<&Path> {
        self.manifest.as_deref()
    }
    /// The active entity: the most recently selected one.
    pub fn selected(&self) -> Option<&str> {
        self.selection.last().map(String::as_str)
    }
    /// Every selected entity in selection order (the last is active).
    pub fn selection(&self) -> &[String] {
        &self.selection
    }
    pub fn is_selected(&self, id: &str) -> bool {
        self.selection.iter().any(|selected| selected == id)
    }
    pub fn is_dirty(&self) -> bool {
        self.document.as_ref().is_some_and(SceneDocument::is_dirty)
    }
    pub fn is_playing(&self) -> bool {
        self.playing
    }
    pub fn can_reload_play(&self) -> bool {
        self.runtime_running && !self.reloading
    }
    pub fn is_reloading_play(&self) -> bool {
        self.reloading
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn log(&self) -> &VecDeque<String> {
        &self.log
    }

    pub fn select(&mut self, id: Option<String>) -> Result<()> {
        self.select_many(id.into_iter().collect())
    }

    /// Replaces the selection. Duplicates collapse (keeping the last position);
    /// the last entry becomes the active entity. Unknown IDs are rejected and
    /// leave the selection unchanged.
    pub fn select_many(&mut self, ids: Vec<String>) -> Result<()> {
        for id in &ids {
            ensure!(self.has_entity(id), "selected entity does not exist: {id}");
        }
        let mut selection: Vec<String> = Vec::with_capacity(ids.len());
        for id in ids {
            selection.retain(|existing| existing != &id);
            selection.push(id);
        }
        self.selection = selection;
        Ok(())
    }

    /// Adds `id` if absent (making it active) or removes it if present.
    pub fn toggle_selection(&mut self, id: &str) -> Result<()> {
        let mut selection = self.selection.clone();
        if let Some(index) = selection.iter().position(|existing| existing == id) {
            selection.remove(index);
        } else {
            selection.push(id.to_owned());
        }
        self.select_many(selection)
    }

    /// Adds `id` to the selection and makes it active.
    pub fn extend_selection(&mut self, id: &str) -> Result<()> {
        let mut selection = self.selection.clone();
        selection.push(id.to_owned());
        self.select_many(selection)
    }

    fn has_entity(&self, id: &str) -> bool {
        self.document
            .as_ref()
            .is_some_and(|d| d.snapshot().entities.iter().any(|e| e.id == id))
    }

    fn prune_selection(&mut self) {
        let Some(document) = &self.document else {
            self.selection.clear();
            return;
        };
        let entities = &document.snapshot().entities;
        self.selection
            .retain(|id| entities.iter().any(|entity| &entity.id == id));
    }

    pub fn open(&mut self, manifest: impl AsRef<Path>, discard_unsaved: bool) -> Result<()> {
        let result = (|| {
            ensure!(
                !self.is_dirty() || discard_unsaved,
                "unsaved edits: save or explicitly discard before opening another project"
            );
            let candidate = SceneDocument::load(manifest.as_ref())?;
            if self.playing {
                self.stop()?;
            }
            self.document = Some(candidate);
            self.manifest = Some(manifest.as_ref().to_path_buf());
            self.selection = self
                .document
                .as_ref()
                .and_then(|d| d.snapshot().entities.first().map(|e| e.id.clone()))
                .into_iter()
                .collect();
            Ok(())
        })();
        self.finish("Project opened", result)
    }

    pub fn reload(&mut self, discard_unsaved: bool) -> Result<()> {
        let path = self
            .manifest
            .clone()
            .ok_or_else(|| anyhow!("no project is open"))?;
        self.open(path, discard_unsaved)
    }

    pub fn apply(&mut self, operation: EditOperation, expected_revision: &str) -> Result<()> {
        let result = self.document_mut().and_then(|document| {
            document
                .apply(operation, expected_revision)
                .map(|_| ())
                .map_err(Into::into)
        });
        self.finish("Scene edit applied", result)
    }

    pub fn save(&mut self) -> Result<()> {
        let result = self.document_mut().and_then(|document| {
            let revision = document.revision().to_owned();
            document.save(&revision).map(|_| ()).map_err(Into::into)
        });
        self.finish("Scene saved", result)
    }

    pub fn undo(&mut self) -> Result<()> {
        let result = self.document_mut().and_then(|document| {
            let revision = document.revision().to_owned();
            document.undo(&revision).map(|_| ()).map_err(Into::into)
        });
        self.finish("Undo applied", result)
    }

    pub fn redo(&mut self) -> Result<()> {
        let result = self.document_mut().and_then(|document| {
            let revision = document.revision().to_owned();
            document.redo(&revision).map(|_| ()).map_err(Into::into)
        });
        self.finish("Redo applied", result)
    }

    pub fn play(&mut self) -> Result<()> {
        let result = (|| {
            ensure!(!self.playing, "an isolated play session is already active");
            let document = self.document_mut()?;
            let revision = document.revision().to_owned();
            let project = document.begin_play(&revision)?;
            if let Err(error) = self.host.start(project) {
                self.document_mut()?.end_play();
                return Err(error);
            }
            self.playing = true;
            Ok(())
        })();
        self.finish(
            "Preparing isolated native play; authored scene is unchanged",
            result,
        )
    }

    /// Rebuild gameplay using the current authored snapshot, including unsaved
    /// edits. Runtime state remains in the old process until replacement.
    pub fn reload_play(&mut self) -> Result<()> {
        let result = (|| {
            ensure!(
                self.can_reload_play(),
                "a running game with no pending reload is required"
            );
            let document = self
                .document
                .as_ref()
                .ok_or_else(|| anyhow!("no project is open"))?;
            let mut project = document.project().clone();
            project.scene = document.snapshot().clone();
            project.validate_scene(&project.scene)?;
            self.host.reload(project)?;
            self.reloading = true;
            Ok(())
        })();
        self.finish("Preparing replacement; previous game continues", result)
    }

    pub fn cancel_play_reload(&mut self) -> Result<()> {
        let result = (|| {
            ensure!(self.reloading, "no game reload is pending");
            self.host.cancel_reload()?;
            self.reloading = false;
            Ok(())
        })();
        self.finish(
            "Reload cancelled; previous game retained if still running",
            result,
        )
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.playing {
            return Ok(());
        }
        let result = (|| {
            self.host.stop()?;
            self.playing = false;
            self.runtime_running = false;
            self.reloading = false;
            if let Some(document) = &mut self.document {
                document.end_play();
            }
            Ok(())
        })();
        self.finish("Play stopped; runtime state discarded", result)
    }

    /// Polls compiler/runtime progress without modifying authored state.
    pub fn poll(&mut self) -> Result<()> {
        if !self.playing {
            return Ok(());
        }
        match self.host.poll() {
            Ok(PlayStatus::Compiling) => {
                self.runtime_running = false;
                self.status = "Compiling native runtime; authored scene is unchanged".into();
                Ok(())
            }
            Ok(PlayStatus::Reloading) => {
                self.runtime_running = true;
                self.reloading = true;
                self.status = "Preparing replacement; previous game continues".into();
                Ok(())
            }
            Ok(PlayStatus::ReloadFailed { message }) => {
                self.runtime_running = true;
                self.reloading = false;
                self.finish("Reload failed", Err(anyhow!(message)))
            }
            Ok(PlayStatus::Running) => {
                self.runtime_running = true;
                if self.reloading {
                    self.reloading = false;
                    return self.finish(
                        "Game reloaded from authored snapshot; runtime state restarted",
                        Ok(()),
                    );
                }
                self.status = "Native play running; authored scene is unchanged".into();
                Ok(())
            }
            Ok(PlayStatus::Stopped { success, message }) => {
                self.playing = false;
                self.runtime_running = false;
                self.reloading = false;
                if let Some(document) = &mut self.document {
                    document.end_play();
                }
                if success {
                    self.finish(&format!("Play finished: {message}"), Ok(()))
                } else {
                    self.finish("Play failed", Err(anyhow!(message)))
                }
            }
            Err(error) => self.finish("Play status failed", Err(error)),
        }
    }

    /// Protects unsaved content before a native window closes.
    pub fn close(&mut self, discard_unsaved: bool) -> Result<()> {
        let result = (|| {
            ensure!(
                !self.is_dirty() || discard_unsaved,
                "unsaved edits: save or explicitly discard before closing"
            );
            self.stop()
        })();
        self.finish("Editor closed", result)
    }

    pub(crate) fn record_error(&mut self, error: impl std::fmt::Display) {
        let message = error.to_string();
        self.status = message.clone();
        self.diagnostics = vec![message.clone()];
        self.append_log(message);
    }

    fn document_mut(&mut self) -> Result<&mut SceneDocument> {
        self.document
            .as_mut()
            .ok_or_else(|| anyhow!("no project is open"))
    }

    fn finish(&mut self, success: &str, result: Result<()>) -> Result<()> {
        match &result {
            Ok(()) => {
                self.prune_selection();
                self.status = success.into();
                self.diagnostics.clear();
                self.append_log(success.into());
            }
            Err(error) => self.record_error(format!("{error:#}")),
        }
        result
    }

    fn append_log(&mut self, message: String) {
        if self.log.len() == 200 {
            self.log.pop_front();
        }
        self.log.push_back(message);
    }
}

impl Drop for EditorController {
    fn drop(&mut self) {
        // Best effort process cleanup if the host closes unexpectedly. Explicit
        // stop/close is required to report errors to the user.
        if self.playing {
            let _ = self.host.stop();
        }
    }
}
