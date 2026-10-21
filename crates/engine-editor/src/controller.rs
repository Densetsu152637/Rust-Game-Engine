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
    Stopped { success: bool, message: String },
}

/// Tooling supplies native compilation and process ownership. `start` receives an
/// isolated clone containing unsaved authored edits. It must not alter the source
/// project files. `stop` must release the generation before reporting success.
/// Hosts should return promptly from start/poll, doing compilation in a worker.
pub trait PlayHost {
    fn start(&mut self, project: Project) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn poll(&mut self) -> Result<PlayStatus>;
}

/// Safe editor state independent of native controls. Failed open/reload retains
/// the current document; all edits use optimistic revision checks from the model.
pub struct EditorController {
    document: Option<SceneDocument>,
    manifest: Option<PathBuf>,
    selected: Option<String>,
    host: Box<dyn PlayHost>,
    playing: bool,
    status: String,
    diagnostics: Vec<String>,
    log: VecDeque<String>,
}

impl EditorController {
    pub fn new(host: Box<dyn PlayHost>) -> Self {
        Self {
            document: None,
            manifest: None,
            selected: None,
            host,
            playing: false,
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
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    pub fn is_dirty(&self) -> bool {
        self.document.as_ref().is_some_and(SceneDocument::is_dirty)
    }
    pub fn is_playing(&self) -> bool {
        self.playing
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
        if let Some(id) = &id {
            ensure!(
                self.document.as_ref().is_some_and(|d| d
                    .snapshot()
                    .entities
                    .iter()
                    .any(|e| &e.id == id)),
                "selected entity does not exist: {id}"
            );
        }
        self.selected = id;
        Ok(())
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
            self.selected = self
                .document
                .as_ref()
                .and_then(|d| d.snapshot().entities.first().map(|e| e.id.clone()));
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

    pub fn stop(&mut self) -> Result<()> {
        if !self.playing {
            return Ok(());
        }
        let result = (|| {
            self.host.stop()?;
            self.playing = false;
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
                self.status = "Compiling native runtime; authored scene is unchanged".into();
                Ok(())
            }
            Ok(PlayStatus::Running) => {
                self.status = "Native play running; authored scene is unchanged".into();
                Ok(())
            }
            Ok(PlayStatus::Stopped { success, message }) => {
                self.playing = false;
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
