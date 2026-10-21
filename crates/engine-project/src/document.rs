use crate::{Error, Result, schema::*};
use std::path::Path;
#[derive(Clone, Debug)]
pub enum EditOperation {
    SetPosition {
        entity_id: String,
        position: [f32; 3],
    },
    SetScript {
        entity_id: String,
        asset: Option<String>,
    },
    SetMeshRenderer {
        entity_id: String,
        value: Option<MeshRenderer>,
    },
}
/// Authoring history is separate from live ECS state. Revision checks reject stale edits.
pub struct SceneDocument {
    project: Project,
    scene: Scene,
    revision: String,
    saved_revision: String,
    undo: Vec<Scene>,
    redo: Vec<Scene>,
    playing: bool,
}
impl SceneDocument {
    pub fn load(manifest: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::new(Project::load(manifest)?))
    }
    pub fn new(project: Project) -> Self {
        let scene = project.scene.clone();
        let revision = scene.revision();
        Self {
            project,
            scene,
            revision: revision.clone(),
            saved_revision: revision,
            undo: vec![],
            redo: vec![],
            playing: false,
        }
    }
    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn snapshot(&self) -> &Scene {
        &self.scene
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    pub fn is_playing(&self) -> bool {
        self.playing
    }
    fn check(&self, expected: &str) -> Result<()> {
        if self.revision != expected {
            Err(Error::new(
                "scene.revision.conflict",
                "Scene changed since the supplied revision",
            ))
        } else {
            Ok(())
        }
    }
    pub fn apply(&mut self, operation: EditOperation, expected: &str) -> Result<String> {
        self.check(expected)?;
        let mut candidate = self.scene.clone();
        let id = match &operation {
            EditOperation::SetPosition { entity_id, .. }
            | EditOperation::SetScript { entity_id, .. }
            | EditOperation::SetMeshRenderer { entity_id, .. } => entity_id,
        };
        let entity = candidate
            .entities
            .iter_mut()
            .find(|e| &e.id == id)
            .ok_or_else(|| Error::new("scene.entity.unknown", format!("Unknown entity {id}")))?;
        match operation {
            EditOperation::SetPosition { position, .. } => {
                let transform = entity.components.transform.as_mut().ok_or_else(|| {
                    Error::new("scene.transform.missing", "Entity has no Transform")
                })?;
                transform.position = position;
            }
            EditOperation::SetScript { asset, .. } => {
                entity.components.script = asset.map(|asset| Script { version: 1, asset });
            }
            EditOperation::SetMeshRenderer { value, .. } => {
                entity.components.mesh_renderer = value;
            }
        }
        self.project.validate_scene(&candidate)?;
        self.undo
            .push(std::mem::replace(&mut self.scene, candidate));
        if self.undo.len() > 1024 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision = self.scene.revision();
        Ok(self.revision.clone())
    }
    pub fn undo(&mut self, expected: &str) -> Result<String> {
        self.check(expected)?;
        let previous = self
            .undo
            .pop()
            .ok_or_else(|| Error::new("scene.undo.empty", "Nothing to undo"))?;
        self.redo.push(std::mem::replace(&mut self.scene, previous));
        self.revision = self.scene.revision();
        Ok(self.revision.clone())
    }
    pub fn redo(&mut self, expected: &str) -> Result<String> {
        self.check(expected)?;
        let next = self
            .redo
            .pop()
            .ok_or_else(|| Error::new("scene.redo.empty", "Nothing to redo"))?;
        self.undo.push(std::mem::replace(&mut self.scene, next));
        self.revision = self.scene.revision();
        Ok(self.revision.clone())
    }
    pub fn save(&mut self, expected: &str) -> Result<String> {
        self.check(expected)?;
        self.project.validate_scene(&self.scene)?;
        let destination = confined_file(&self.project.root, &self.scene.source)?;
        let bytes = serde_json::to_vec_pretty(&self.scene)?;
        crate::persistence::atomic_replace(&destination, &bytes, None)?;
        self.saved_revision = self.revision.clone();
        self.project.scene = self.scene.clone();
        Ok(self.revision.clone())
    }
    pub fn reload(&mut self, discard_unsaved: bool) -> Result<()> {
        if self.playing {
            return Err(Error::new(
                "scene.play.active",
                "Stop play before reloading",
            ));
        }
        if self.is_dirty() && !discard_unsaved {
            return Err(Error::new(
                "scene.unsaved",
                "Reload would discard unsaved work",
            ));
        }
        let project = Project::load(&self.project.manifest)?;
        *self = Self::new(project);
        Ok(())
    }
    pub fn begin_play(&mut self, expected: &str) -> Result<Project> {
        self.check(expected)?;
        if self.playing {
            return Err(Error::new("scene.play.active", "Play is already active"));
        }
        self.project.validate_scene(&self.scene)?;
        let mut play = self.project.clone();
        play.scene = self.scene.clone();
        self.playing = true;
        Ok(play)
    }
    pub fn end_play(&mut self) {
        self.playing = false;
    }
}
