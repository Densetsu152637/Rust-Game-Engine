use crate::{Error, Result, schema::*};
use std::path::Path;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOperation {
    SetComponent {
        entity_id: String,
        component: String,
        value: serde_json::Value,
        #[serde(default)]
        merge: bool,
    },
    RemoveComponent {
        entity_id: String,
        component: String,
    },
    Reparent {
        entity_id: String,
        parent: Option<String>,
    },
    InstantiatePrefab {
        asset: String,
        prefix: String,
        parent: Option<String>,
    },
    Batch {
        operations: Vec<EditOperation>,
    },
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
    /// Append a new entity. Its ID must not already exist in the scene.
    CreateEntity {
        entity: SceneEntity,
    },
    /// Replace the entity whose ID equals `entity.id` wholesale.
    ReplaceEntity {
        entity: SceneEntity,
    },
    /// Remove an entity. Scene validation rejects dangling references to it.
    DeleteEntity {
        entity_id: String,
    },
}
fn unknown_entity(id: &str) -> Error {
    Error::new("scene.entity.unknown", format!("Unknown entity {id}"))
}
fn entity_mut<'a>(scene: &'a mut Scene, id: &str) -> Result<&'a mut SceneEntity> {
    scene
        .entities
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| unknown_entity(id))
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
    /// A document over an arbitrary scene of `project`, not only its startup
    /// scene. The scene's `source` decides where [`Self::save`] writes.
    pub fn for_scene(project: &Project, scene: Scene) -> Self {
        let mut project = project.clone();
        project.scene = scene;
        Self::new(project)
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
        Self::apply_candidate(&self.project, &mut candidate, operation, 0, &mut 1024)?;
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
    fn apply_candidate(
        project: &Project,
        candidate: &mut Scene,
        operation: EditOperation,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<()> {
        if *remaining == 0 {
            return Err(Error::new(
                "scene.batch.limit",
                "Total batch exceeds 1024 operations",
            ));
        }
        *remaining -= 1;
        if depth > 16 {
            return Err(Error::new("scene.batch.limit", "Batch nesting exceeds 16"));
        }
        match operation {
            EditOperation::Batch { operations } => {
                if operations.len() > 1024 {
                    return Err(Error::new(
                        "scene.batch.limit",
                        "At most 1024 edits per batch",
                    ));
                }
                for operation in operations {
                    Self::apply_candidate(project, candidate, operation, depth + 1, remaining)?;
                }
            }
            EditOperation::SetComponent {
                entity_id,
                component,
                value,
                merge,
            } => {
                let entity = entity_mut(candidate, &entity_id)?;
                let mut components = serde_json::to_value(&entity.components)?;
                let custom = component.contains('.');
                if custom && !components["custom"].is_object() {
                    components["custom"] = serde_json::json!({});
                }
                let slot = if custom {
                    &mut components["custom"][&component]
                } else {
                    &mut components[&component]
                };
                *slot = if merge {
                    match (slot.as_object(), value.as_object()) {
                        (Some(existing), Some(patch)) => {
                            let mut merged = existing.clone();
                            merged.extend(patch.clone());
                            serde_json::Value::Object(merged)
                        }
                        _ => value,
                    }
                } else {
                    value
                };
                entity.components = serde_json::from_value(components)?;
            }
            EditOperation::RemoveComponent {
                entity_id,
                component,
            } => {
                if !COMPONENT_NAMES.contains(&component.as_str())
                    && !project.component_catalog.0.contains_key(&component)
                {
                    return Err(Error::new("scene.component.unknown", "Unknown component"));
                }
                let entity = entity_mut(candidate, &entity_id)?;
                if component.contains('.') {
                    entity.components.custom.remove(&component);
                } else {
                    let mut value = serde_json::to_value(&entity.components)?;
                    value
                        .as_object_mut()
                        .ok_or_else(|| {
                            Error::new("scene.component.invalid", "Expected component object")
                        })?
                        .remove(&component);
                    entity.components = serde_json::from_value(value)?;
                }
            }
            EditOperation::Reparent { entity_id, parent } => {
                entity_mut(candidate, &entity_id)?.parent = parent;
            }
            EditOperation::InstantiatePrefab {
                asset,
                prefix,
                parent,
            } => {
                if prefix.is_empty() || prefix.len() > 128 {
                    return Err(Error::new(
                        "scene.prefab.prefix",
                        "Prefab requires a bounded nonempty ID prefix",
                    ));
                }
                let asset = project.require_asset(&asset, "prefab")?;
                let prefab = load_scene(&project.root, &asset.path)?;
                if candidate
                    .entities
                    .len()
                    .saturating_add(prefab.entities.len())
                    > 65536
                {
                    return Err(Error::new(
                        "scene.prefab.limit",
                        "Prefab would exceed scene capacity",
                    ));
                }
                let ids: std::collections::BTreeMap<_, _> = prefab
                    .entities
                    .iter()
                    .map(|e| (e.id.clone(), format!("{prefix}{}", e.id)))
                    .collect();
                for mut entity in prefab.entities {
                    entity.id = ids[&entity.id].clone();
                    entity.parent = entity
                        .parent
                        .map(|p| ids.get(&p).cloned().unwrap_or(p))
                        .or_else(|| parent.clone());
                    if let Some(camera) = &mut entity.components.camera_2d
                        && let Some(follow) = &mut camera.follow
                        && let Some(mapped) = ids.get(follow)
                    {
                        *follow = mapped.clone();
                    }
                    for (name, value) in &mut entity.components.custom {
                        if name == "engine.AnimationPlayer"
                            || name == "engine.AnimationGraphPlayer"
                            || name == "engine.SequencePlayer"
                            || name == "engine.SkinBinding"
                        {
                            crate::simulation::remap_animation_bindings(&mut value.data, &ids);
                        }
                        if let Some(descriptor) = project.component_catalog.0.get(name) {
                            for pointer in &descriptor.entity_references {
                                if let Some(reference) = value.data.pointer_mut(pointer)
                                    && let Some(mapped) =
                                        reference.as_str().and_then(|id| ids.get(id))
                                {
                                    *reference = serde_json::Value::String(mapped.clone());
                                }
                            }
                        }
                    }
                    candidate.entities.push(entity);
                }
            }
            EditOperation::SetPosition {
                entity_id,
                position,
            } => {
                let transform = entity_mut(candidate, &entity_id)?
                    .components
                    .transform
                    .as_mut()
                    .ok_or_else(|| {
                        Error::new("scene.transform.missing", "Entity has no Transform")
                    })?;
                transform.position = position;
            }
            EditOperation::SetScript { entity_id, asset } => {
                entity_mut(candidate, &entity_id)?.components.script =
                    asset.map(|asset| Script { version: 1, asset });
            }
            EditOperation::SetMeshRenderer { entity_id, value } => {
                entity_mut(candidate, &entity_id)?.components.mesh_renderer = value;
            }
            EditOperation::CreateEntity { entity } => {
                if candidate.entities.iter().any(|e| e.id == entity.id) {
                    return Err(Error::new(
                        "scene.entity.id.duplicate",
                        format!("Entity {} already exists", entity.id),
                    ));
                }
                candidate.entities.push(entity);
            }
            EditOperation::ReplaceEntity { entity } => {
                let id = entity.id.clone();
                *entity_mut(candidate, &id)? = entity;
            }
            EditOperation::DeleteEntity { entity_id } => {
                let index = candidate
                    .entities
                    .iter()
                    .position(|e| e.id == entity_id)
                    .ok_or_else(|| unknown_entity(&entity_id))?;
                candidate.entities.remove(index);
            }
        }
        Ok(())
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
        // The replacement goes through a temporary file, which Unix creates with
        // mode 0600; remember the scene's own mode and put it back afterwards.
        #[cfg(unix)]
        let original_permissions = std::fs::metadata(&destination)
            .ok()
            .map(|m| m.permissions());
        crate::persistence::atomic_replace(&destination, &bytes, None)?;
        #[cfg(unix)]
        if let Some(permissions) = original_permissions {
            // Best effort: the scene is already saved atomically.
            let _ = std::fs::set_permissions(&destination, permissions);
        }
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
