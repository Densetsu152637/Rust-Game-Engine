//! Shared, write-gated model import. It never compiles or executes project code.
use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![tool(
        Spec {
            name: "asset_import_model",
            title: "Import glTF model",
            description: "Import a project-relative glTF/GLB source into new mesh, PNG texture, prefab, model metadata and supported animation assets. Prefix must be unique. Existing files are not overwritten; the manifest changes only after validation. Requires write access; does not compile or execute Rust.",
            input: schema::object(
                vec![
                    (
                        "source",
                        schema::string(
                            "Project-relative glTF/GLB path; external dependencies must remain inside the project.",
                        ),
                    ),
                    (
                        "prefix",
                        schema::string(
                            "Unique asset ID/file prefix: 1-64 ASCII letters, digits, hyphens or underscores.",
                        ),
                    ),
                ],
                &["source", "prefix"],
            ),
            output: schema::output(vec![
                ("model", json!({"type":"string"})),
                ("prefab", json!({"type":"string"})),
                ("directory", json!({"type":"string"})),
                ("assets", json!({"type":"array"})),
                ("fingerprint", json!({"type":"string"})),
                ("manifest_revision", json!({"type":"string"})),
                ("warnings", json!({"type":"array"})),
            ]),
            annotations: Annotations::mutation(false, false),
            access: AccessLevel::Write,
        },
        import,
    )]
}
fn import(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let source = args
        .get("source")
        .ok_or_else(|| schema::invalid("source", "a project-relative file"))?;
    cx.existing_file("source", source)?;
    let args = Args(args);
    let result = engine_project::model_import::import_model(
        &cx.project()?,
        Path::new(args.string("source")?),
        args.string("prefix")?,
    )
    .map_err(ToolFailure::from)?;
    let value = serde_json::to_value(result).map_err(|e| {
        ToolFailure::from(engine_project::Error::new(
            "asset.import.result",
            e.to_string(),
        ))
    })?;
    Ok(ToolResult::new(value))
}
