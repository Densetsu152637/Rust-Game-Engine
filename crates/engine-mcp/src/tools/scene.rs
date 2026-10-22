//! Scene tools: `scene_list`, `scene_inspect`, `entity_get`.

use super::{Spec, clean, slash, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{Project, Scene, SceneEntity, load_scene};
use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let scene_arg = || {
        schema::string(
            "Project-relative scene path, for example `scenes/main.scene.json` (see scene_list).",
        )
    };
    vec![
        tool(
            Spec {
                name: "scene_list",
                title: "List scenes",
                description: "List every scene of the project (the startup scene and every scene \
                    asset) with its scene ID, entity count and current revision. Scenes that \
                    fail to load are listed with valid:false and the first error.",
                input: schema::object(vec![], &[]),
                output: schema::output(vec![(
                    "scenes",
                    json!({"type": "array", "items": {"type": "object", "properties": {
                        "path": {"type": "string"}, "asset_id": {"type": "string"},
                        "scene_id": {"type": "string"}, "startup": {"type": "boolean"},
                        "valid": {"type": "boolean"}, "entities": {"type": "integer"},
                        "revision": {"type": "string"}, "error": {"type": "string"}}}}),
                )]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            scene_list,
        ),
        tool(
            Spec {
                name: "scene_inspect",
                title: "Inspect scene",
                description: "Return a scene's authored entities and components, checked \
                    against the project's asset catalog, together with its `revision` (pass it \
                    as expected_revision to the edit tools). Large scenes are paged with \
                    offset/limit; use `entities` to fetch specific entity IDs.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "entities",
                            schema::array_of(
                                "Only return entities with these stable IDs.",
                                json!({"type": "string"}),
                                1000,
                            ),
                        ),
                        (
                            "offset",
                            schema::integer("Entities to skip (paging).", 0, 10_000_000, Some(0)),
                        ),
                        (
                            "limit",
                            schema::integer("Maximum entities to return.", 1, 1000, Some(200)),
                        ),
                    ],
                    &["scene"],
                ),
                output: schema::output(vec![
                    ("path", schema::string("Project-relative scene path.")),
                    ("schema", json!({"type": "integer"})),
                    ("scene_id", schema::string("Scene ID.")),
                    (
                        "revision",
                        schema::string("Content revision for optimistic edits."),
                    ),
                    ("entity_count", json!({"type": "integer"})),
                    (
                        "entities",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    ("returned", json!({"type": "integer"})),
                    ("offset", json!({"type": "integer"})),
                    ("truncated", json!({"type": "boolean"})),
                    ("next_offset", json!({"type": "integer"})),
                    (
                        "missing_entities",
                        json!({"type": "array", "items": {"type": "string"}}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            scene_inspect,
        ),
        tool(
            Spec {
                name: "entity_get",
                title: "Get entity",
                description: "Return one entity by its stable authored ID with all components, \
                    the resolved assets it references (id, kind, path) and the scene revision. \
                    Cheaper than scene_inspect when you know the ID.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "id",
                            schema::string("Stable entity ID, for example `player`."),
                        ),
                    ],
                    &["scene", "id"],
                ),
                output: schema::output(vec![
                    ("path", schema::string("Project-relative scene path.")),
                    ("scene_id", schema::string("Scene ID.")),
                    ("revision", schema::string("Scene revision.")),
                    ("entity", json!({"type": "object"})),
                    (
                        "assets",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            entity_get,
        ),
    ]
}

/// A scene loaded and validated against its project.
pub(super) struct Loaded {
    pub project: Project,
    pub scene: Scene,
    /// Project-relative, forward-slash scene path.
    pub path: String,
    pub revision: String,
}

/// Resolves the `scene` argument, then loads the project and the scene and
/// validates the scene against the project's catalog.
pub(super) fn load(cx: &ToolContext<'_>, args: &Map<String, Value>) -> Result<Loaded, ToolFailure> {
    let file = cx.existing_file("scene", args.get("scene").unwrap_or(&Value::Null))?;
    let relative = file
        .strip_prefix(cx.root)
        .expect("confined file")
        .to_owned();
    let project = cx.project()?;
    // Canonicalise first, then load through the same confined service. This also
    // rejects links in the project's catalog and authored scenes.
    let scene = load_scene(cx.root, &relative)
        .and_then(|scene| project.validate_scene(&scene).map(|()| scene))
        .map_err(ToolFailure::from)?;
    let revision = scene.revision();
    Ok(Loaded {
        project,
        scene,
        path: slash(&relative),
        revision,
    })
}

/// Every scene path: the startup scene first, then scene assets.
pub(super) fn scene_paths(project: &Project) -> Vec<(String, Option<String>, bool)> {
    let startup = slash(&project.startup_scene);
    let mut result = vec![(startup.clone(), None, true)];
    for asset in project.assets.values().filter(|a| a.kind == "scene") {
        let path = slash(&asset.path);
        if path == startup {
            result[0].1 = Some(asset.id.clone());
        } else {
            result.push((path, Some(asset.id.clone()), false));
        }
    }
    result
}

/// The canonical file behind a project-relative path, or `None` when it does
/// not exist (or leaves the root).
fn canonical_in(root: &Path, relative: &str) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(root.join(relative))
        .ok()
        .filter(|c| c.starts_with(root))
}

/// True when the canonical `file` is the project's startup scene or one of its
/// scene assets. Comparing canonical paths (not strings) means case variants and
/// 8.3 short names of a catalog scene still count as that scene.
pub(super) fn is_catalog_scene(root: &Path, project: &Project, file: &Path) -> bool {
    scene_paths(project)
        .iter()
        .any(|(path, _, _)| canonical_in(root, path).is_some_and(|c| c == file))
}

/// True when the canonical `file` is the project manifest `project.json`.
pub(super) fn is_manifest(root: &Path, file: &Path) -> bool {
    canonical_in(root, "project.json").is_some_and(|c| c == file)
}

fn scene_list(cx: &ToolContext<'_>, _args: &Map<String, Value>) -> ToolOutcome {
    let project = cx.project()?;
    let scenes: Vec<Value> = scene_paths(&project)
        .into_iter()
        .map(|(path, asset_id, startup)| {
            let mut entry = json!({"path": path, "startup": startup});
            if let Some(id) = asset_id {
                entry["asset_id"] = json!(id);
            }
            match load_scene(cx.root, Path::new(&path)) {
                Ok(scene) => {
                    entry["valid"] = json!(true);
                    entry["scene_id"] = json!(scene.id);
                    entry["entities"] = json!(scene.entities.len());
                    entry["revision"] = json!(scene.revision());
                }
                Err(error) => {
                    entry["valid"] = json!(false);
                    entry["error"] = json!(ToolFailure::from(error).message);
                }
            }
            entry
        })
        .collect();
    Ok(ToolResult::new(json!({"scenes": scenes})))
}

fn scene_inspect(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let filter = a.string_array("entities")?;
    let offset = a.integer("offset", 0, 10_000_000, 0)? as usize;
    let limit = a.integer("limit", 1, cx.limits.max_page_size as u64, 200)? as usize;
    let loaded = load(cx, args)?;
    let mut missing = Vec::new();
    let selected: Vec<&SceneEntity> = match &filter {
        None => loaded.scene.entities.iter().collect(),
        Some(ids) => {
            missing = ids
                .iter()
                .filter(|id| !loaded.scene.entities.iter().any(|e| &e.id == *id))
                .map(|id| (*id).to_owned())
                .collect();
            loaded
                .scene
                .entities
                .iter()
                .filter(|e| ids.contains(&e.id.as_str()))
                .collect()
        }
    };
    let page: Vec<Value> = selected
        .iter()
        .skip(offset)
        .take(limit)
        .map(|e| clean(*e))
        .collect();
    let end = offset + page.len();
    let mut result = json!({
        "path": loaded.path,
        "schema": loaded.scene.schema,
        "scene_id": loaded.scene.id,
        "revision": loaded.revision,
        "entity_count": loaded.scene.entities.len(),
        "returned": page.len(),
        "offset": offset,
        "truncated": end < selected.len(),
        "entities": page,
    });
    if end < selected.len() {
        result["next_offset"] = json!(end);
    }
    if !missing.is_empty() {
        result["missing_entities"] = json!(missing);
    }
    Ok(ToolResult::new(result).verbatim())
}

/// Catalog entries referenced by an entity's components.
fn referenced_assets(project: &Project, entity: &SceneEntity) -> Vec<Value> {
    let c = &entity.components;
    let mut ids: Vec<&str> = Vec::new();
    if let Some(script) = &c.script {
        ids.push(&script.asset);
    }
    if let Some(mesh) = &c.mesh_renderer {
        ids.push(&mesh.mesh);
        ids.extend(mesh.texture.as_deref());
    }
    if let Some(sprite) = &c.sprite_renderer {
        ids.push(&sprite.texture);
    }
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter_map(|id| project.assets.get(id))
        .map(|a| json!({"id": a.id, "kind": a.kind, "path": slash(&a.path)}))
        .collect()
}

fn entity_get(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let id = Args(args).string("id")?;
    let loaded = load(cx, args)?;
    let Some(entity) = loaded.scene.entities.iter().find(|e| e.id == id) else {
        let needle = id.to_lowercase();
        let mut similar: Vec<&str> = loaded
            .scene
            .entities
            .iter()
            .map(|e| e.id.as_str())
            .filter(|candidate| candidate.to_lowercase().contains(&needle))
            .take(10)
            .collect();
        if similar.is_empty() {
            similar = loaded
                .scene
                .entities
                .iter()
                .map(|e| e.id.as_str())
                .take(10)
                .collect();
        }
        return Err(ToolFailure::new(
            "entity_not_found",
            format!("scene {} has no entity with ID {id}", loaded.path),
        )
        .with_hint("List IDs with scene_inspect; entity IDs are exact and case-sensitive.")
        .with_details(json!({"suggestions": similar})));
    };
    Ok(ToolResult::new(json!({
        "path": loaded.path,
        "scene_id": loaded.scene.id,
        "revision": loaded.revision,
        "entity": clean(entity),
        "assets": referenced_assets(&loaded.project, entity),
    }))
    .verbatim())
}
