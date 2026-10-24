//! `component_schema`: the authored component types as JSON Schema.
//!
//! The schemas are written out by hand next to engine-project's serde types
//! (`schema.rs`) and pinned to them by tests: every example must deserialize,
//! every serialized default must only use documented properties, and every
//! documented property must be accepted by the strict (`deny_unknown_fields`)
//! deserializer.

use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use serde_json::{Map, Value, json};

pub use engine_project::schema::{COMPONENT_NAMES, ComponentDoc, SCENE_RULES, component_docs};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![tool(
        Spec {
            name: "component_schema",
            title: "Component schemas",
            description: "Describe the authored component types an entity can carry \
                (Transform, Script, MeshRenderer, SpriteRenderer, Camera2D, Collider2D) as JSON \
                Schema with descriptions, defaults and a valid example each, plus the scene \
                rules the engine enforces. Use it before building `components` for \
                entity_create, entity_update or component_set.",
            input: schema::object(
                vec![(
                    "component",
                    schema::string("Built-in or namespaced custom component name."),
                )],
                &[],
            ),
            output: schema::output(vec![
                (
                    "components",
                    json!({"type": "array", "items": {"type": "object",
                    "properties": {"name": {"type": "string"}, "description": {"type": "string"},
                        "requires": {"type": "array", "items": {"type": "string"}},
                        "schema": {"type": "object"}, "example": {"type": "object"}}}}),
                ),
                ("entity", json!({"type": "object"})),
                (
                    "scene_rules",
                    json!({"type": "array", "items": {"type": "string"}}),
                ),
            ]),
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        },
        component_schema,
    )]
}

fn component_schema(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let only = Args(args).optional_string("component")?;
    let mut components: Vec<Value> = component_docs()
        .into_iter()
        .filter(|doc| only.is_none_or(|name| name == doc.name))
        .map(|doc| {
            json!({"name": doc.name, "description": doc.description, "requires": doc.requires,
                "schema": doc.schema, "example": doc.example})
        })
        .collect();
    let project = cx.project()?;
    for descriptor in project
        .component_catalog
        .0
        .values()
        .filter(|d| only.is_none_or(|name| name == d.name))
    {
        components.push(json!({"name": descriptor.name, "version": descriptor.version, "requires": descriptor.requires,
            "schema": {"type":"object","properties":{"version":{"type":"integer","const":descriptor.version},"data":descriptor.schema},"required":["version","data"],"additionalProperties":false},
            "example": {"version":descriptor.version,"data":descriptor.defaults}, "dataOnly":descriptor.data_only, "entityReferences":descriptor.entity_references}));
    }
    if only.is_some() && components.is_empty() {
        return Err(crate::schema::invalid(
            "component",
            "a declared component name (for example Transform)",
        ));
    }
    Ok(ToolResult::new(json!({
        "components": components,
        "entity": {"type": "object", "properties": {
            "id": {"type": "string", "description": "Stable unique entity ID."},
            "name": {"type": "string", "description": "Display name (1-256 characters)."}, "parent": {"type":["string","null"]},
            "components": {"type": "object", "description":
                "Component values keyed by component name.",
                "properties": std::iter::once(("custom".to_owned(), json!({"type":"object","description":"Namespaced custom components, each containing version and data; see component_schema."}))).chain(COMPONENT_NAMES.iter().map(|n| ((*n).to_owned(),
                    json!({"type": "object", "description":
                        format!("See the `{n}` entry of `components`.")})))).collect::<Map<String, Value>>(),
                "additionalProperties": false}},
            "required": ["id", "name", "components"], "additionalProperties": false},
        "scene_rules": SCENE_RULES,
    })))
}
