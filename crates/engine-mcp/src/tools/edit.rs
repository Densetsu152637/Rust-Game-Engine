//! Scene editing tools (write access only): `entity_create`, `entity_update`,
//! `entity_delete`, `component_set`, `component_remove`.
//!
//! Every edit goes through engine-project's revisioned [`SceneDocument`]: the
//! scene is loaded fresh from disk, the caller's `expected_revision` must match
//! its revision, the edited scene is validated against the whole project, and
//! only then is it saved atomically. A rejected edit changes nothing on disk.

use super::{
    Spec, clean,
    scene::{self, Loaded},
    tool,
};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{EditOperation, Scene, SceneDocument, SceneEntity};
use serde_json::{Map, Value, json};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let common = |extra: Vec<(&'static str, Value)>, required: &[&str]| {
        let mut properties = vec![
            (
                "scene",
                schema::string("Project-relative scene path (see scene_list)."),
            ),
            (
                "expected_revision",
                schema::string(
                    "`revision` from scene_inspect, entity_get or the previous edit. The edit is \
                     rejected with revision_conflict when the scene changed since.",
                ),
            ),
        ];
        properties.extend(extra);
        properties.push((
            "dry_run",
            schema::boolean(
                "Validate the edit and report the result without saving anything.",
                false,
            ),
        ));
        let mut all_required = vec!["scene", "expected_revision"];
        all_required.extend_from_slice(required);
        schema::object(properties, &all_required)
    };
    let components_object =
        |description: &str| json!({"type": "object", "description": description});
    let id = |description: &str| schema::string(description);
    let output = || {
        schema::output(vec![
            ("scene", schema::string("Project-relative scene path.")),
            ("scene_id", schema::string("Scene ID.")),
            (
                "saved",
                json!({"type": "boolean", "description": "false for dry runs."}),
            ),
            ("dry_run", json!({"type": "boolean"})),
            (
                "previous_revision",
                schema::string("Revision before the edit."),
            ),
            (
                "revision",
                schema::string("Revision after the edit; use it for the next edit."),
            ),
            (
                "entity",
                json!({"type": ["object", "null"], "description": "The entity after the edit; null after a delete."}),
            ),
            ("entity_count", json!({"type": "integer"})),
        ])
    };
    vec![
        tool(
            Spec {
                name: "scene_edit",
                title: "Transactional scene edits",
                description: "Atomically apply an ordered batch of document operations. Each object has op: set_component {entity_id,component,value,merge?}, remove_component {entity_id,component}, reparent {entity_id,parent}, instantiate_prefab {asset,prefix,parent}, create_entity {entity}, replace_entity {entity}, or delete_entity {entity_id}. One revision and undo unit; failures leave the scene unchanged. Custom values are {version,data}.",
                input: common(
                    vec![(
                        "operations",
                        json!({"description":"Ordered document operations applied atomically.","type":"array","maxItems":1024,"items":{"type":"object"}}),
                    )],
                    &["operations"],
                ),
                output: output(),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            scene_edit,
        ),
        tool(
            Spec {
                name: "entity_create",
                title: "Create entity",
                description: "Add a new entity to a scene. `id` is its stable authored identifier \
                    (unique in the scene); `components` maps component names to values (see \
                    component_schema). The whole scene is validated against the project before it \
                    is saved atomically. Needs expected_revision from a read tool.",
                input: common(
                    vec![
                        (
                            "id",
                            id("New stable entity ID, unique in the scene (1-256 characters)."),
                        ),
                        ("name", schema::string("Display name; defaults to the ID.")),
                        (
                            "components",
                            components_object(
                                "Components keyed by name, e.g. {\"Transform\":{\"position\":[0,0,0]}}.",
                            ),
                        ),
                    ],
                    &["id"],
                ),
                output: output(),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            entity_create,
        ),
        tool(
            Spec {
                name: "entity_update",
                title: "Update entity",
                description: "Rename an entity and/or replace its entire component set. \
                    `components`, when given, is the complete new set (components you omit are \
                    removed); use component_set or component_remove to change single components. \
                    The entity ID cannot change. Validated and saved atomically.",
                input: common(
                    vec![
                        ("id", id("ID of the existing entity.")),
                        ("name", schema::string("New display name.")),
                        (
                            "components",
                            components_object("Complete replacement component set."),
                        ),
                    ],
                    &["id"],
                ),
                output: output(),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            entity_update,
        ),
        tool(
            Spec {
                name: "entity_delete",
                title: "Delete entity",
                description: "Remove an entity from a scene. Rejected (nothing changes) when the \
                    scene would become invalid, for example when a Camera2D still follows the \
                    entity. Validated and saved atomically.",
                input: common(vec![("id", id("ID of the entity to delete."))], &["id"]),
                output: output(),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            entity_delete,
        ),
        tool(
            Spec {
                name: "component_set",
                title: "Set component",
                description: "Add or change one component of an entity. By default `value` \
                    replaces the component (fields you omit take their defaults); with \
                    merge:true the given fields are laid over the existing component. Validated \
                    and saved atomically.",
                input: common(
                    vec![
                        ("entity", id("ID of the existing entity.")),
                        (
                            "component",
                            schema::string("Built-in or declared namespaced custom component."),
                        ),
                        (
                            "value",
                            components_object(
                                "Component value; see component_schema for the fields.",
                            ),
                        ),
                        (
                            "merge",
                            schema::boolean(
                                "Overlay the given fields on the existing component instead of replacing it.",
                                false,
                            ),
                        ),
                    ],
                    &["entity", "component", "value"],
                ),
                output: output(),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            component_set,
        ),
        tool(
            Spec {
                name: "component_remove",
                title: "Remove component",
                description: "Remove one component from an entity. Rejected when that would \
                    leave the scene invalid (for example removing Transform while a SpriteRenderer \
                    needs it). Removing an absent component is not an error. Validated and saved \
                    atomically.",
                input: common(
                    vec![
                        ("entity", id("ID of the existing entity.")),
                        (
                            "component",
                            schema::string("Built-in or declared namespaced custom component."),
                        ),
                    ],
                    &["entity", "component"],
                ),
                output: output(),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            component_remove,
        ),
    ]
}

fn invalid_entity(id: &str, error: impl std::fmt::Display) -> ToolFailure {
    let message = format!("the entity is not valid: {error}");
    ToolFailure::new("component_invalid", message.clone())
        .with_hint("See component_schema for the valid components and fields.")
        .with_diagnostics(vec![
            json!({"code": "scene.component.invalid", "severity": "error",
            "file": "", "path": id, "message": message}),
        ])
}

fn entity_from(value: Value, id: &str) -> Result<SceneEntity, ToolFailure> {
    serde_json::from_value(value).map_err(|error| invalid_entity(id, error))
}

pub(super) fn find<'a>(
    scene: &'a Scene,
    id: &str,
    path: &str,
) -> Result<&'a SceneEntity, ToolFailure> {
    scene.entities.iter().find(|e| e.id == id).ok_or_else(|| {
        ToolFailure::new(
            "entity_not_found",
            format!("scene {path} has no entity with ID {id}"),
        )
        .with_hint("List IDs with scene_inspect; entity IDs are exact and case-sensitive.")
    })
}

/// Runs one edit through a revisioned document. `plan` turns the loaded scene
/// into the operation and the ID of the entity to report afterwards.
pub(super) fn apply_edit(
    cx: &ToolContext<'_>,
    args: &Map<String, Value>,
    plan: impl FnOnce(&Loaded) -> Result<(EditOperation, Option<String>), ToolFailure>,
) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let expected = a.string("expected_revision")?;
    let dry_run = a.boolean("dry_run", false)?;
    let loaded = scene::load(cx, args)?;
    if loaded.revision != expected {
        return Err(ToolFailure::new(
            "revision_conflict",
            "the scene changed since the supplied expected_revision",
        )
        .with_hint(
            "Re-read the scene with scene_inspect or entity_get, reapply your change to the \
             current content and retry with the revision it returns.",
        ));
    }
    let (operation, reported) = plan(&loaded)?;
    let Loaded {
        project,
        scene,
        path,
        revision: previous,
    } = loaded;
    let scene_id = scene.id.clone();
    let mut document = SceneDocument::for_scene(&project, scene);
    let revision = document.apply(operation, expected).map_err(|e| {
        if e.diagnostics
            .first()
            .is_some_and(|d| d.code == "project.document.malformed")
        {
            invalid_entity("", e)
        } else {
            ToolFailure::from(e)
        }
    })?;
    if !dry_run {
        document.save(&revision).map_err(ToolFailure::from)?;
    }
    let entity = reported
        .and_then(|id| {
            document
                .snapshot()
                .entities
                .iter()
                .find(|e| e.id == id)
                .map(clean)
        })
        .unwrap_or(Value::Null);
    Ok(ToolResult::new(json!({
        "scene": path, "scene_id": scene_id, "saved": !dry_run, "dry_run": dry_run,
        "previous_revision": previous, "revision": revision, "entity": entity,
        "entity_count": document.snapshot().entities.len(),
    })))
}

fn entity_create(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let id = a.string("id")?.to_owned();
    let name = a.optional_string("name")?.unwrap_or(&id).to_owned();
    let components = a
        .optional_object("components")?
        .cloned()
        .unwrap_or_default();
    apply_edit(cx, args, |_| {
        let entity = entity_from(
            json!({"id": id, "name": name, "components": components}),
            &id,
        )?;
        Ok((EditOperation::CreateEntity { entity }, Some(id.clone())))
    })
}

fn entity_update(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let id = a.string("id")?.to_owned();
    let name = a.optional_string("name")?;
    let components = a.optional_object("components")?;
    if name.is_none() && components.is_none() {
        return Err(schema::invalid(
            "name or components",
            "supplied (at least one)",
        ));
    }
    apply_edit(cx, args, |loaded| {
        let existing = find(&loaded.scene, &id, &loaded.path)?;
        let mut value = clean(existing);
        if let Some(name) = name {
            value["name"] = json!(name);
        }
        if let Some(components) = components {
            value["components"] = Value::Object(components.clone());
        }
        let entity = entity_from(value, &id)?;
        Ok((EditOperation::ReplaceEntity { entity }, Some(id.clone())))
    })
}

fn entity_delete(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let id = Args(args).string("id")?.to_owned();
    apply_edit(cx, args, |loaded| {
        find(&loaded.scene, &id, &loaded.path)?;
        Ok((
            EditOperation::DeleteEntity {
                entity_id: id.clone(),
            },
            None,
        ))
    })
}

fn component_set(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let id = a.string("entity")?.to_owned();
    let component = a.string("component")?.to_owned();
    validate_component_name(cx, &component)?;
    let value = a
        .optional_object("value")?
        .ok_or_else(|| schema::invalid("value", "an object"))?
        .clone();
    let merge = a.boolean("merge", false)?;
    apply_edit(cx, args, |loaded| {
        find(&loaded.scene, &id, &loaded.path)?;
        Ok((
            EditOperation::SetComponent {
                entity_id: id.clone(),
                component,
                value: Value::Object(value),
                merge,
            },
            Some(id),
        ))
    })
}
fn component_remove(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let id = a.string("entity")?.to_owned();
    let component = a.string("component")?.to_owned();
    validate_component_name(cx, &component)?;
    apply_edit(cx, args, |loaded| {
        find(&loaded.scene, &id, &loaded.path)?;
        Ok((
            EditOperation::RemoveComponent {
                entity_id: id.clone(),
                component,
            },
            Some(id),
        ))
    })
}
fn scene_edit(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let value = args
        .get("operations")
        .cloned()
        .ok_or_else(|| schema::invalid("operations", "an array"))?;
    let operations: Vec<EditOperation> =
        serde_json::from_value(value).map_err(|e| schema::invalid("operations", &e.to_string()))?;
    apply_edit(cx, args, |_| {
        Ok((EditOperation::Batch { operations }, None))
    })
}

fn validate_component_name(cx: &ToolContext<'_>, name: &str) -> Result<(), ToolFailure> {
    if !engine_project::COMPONENT_NAMES.contains(&name)
        && !cx.project()?.component_catalog.0.contains_key(name)
    {
        return Err(schema::invalid("component", "a declared component name"));
    }
    Ok(())
}
