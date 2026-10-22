//! The tool abstraction and registry. Built-in tools live in [`crate::tools`];
//! other crates (for example `engine-tooling`, which owns native compilation)
//! contribute tools through [`ToolProvider`] without a dependency cycle.

use crate::{
    config::{Access, AccessLevel, Limits, ServerConfig},
    error::{Error, Result},
    paths,
};
use engine_project::Project;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
};

/// MCP tool annotations. All hints are advisory for clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Annotations {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

impl Annotations {
    pub const READ_ONLY: Self = Self {
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
    };
    /// A closed-world mutation. `destructive` means existing data may be
    /// overwritten or removed; `idempotent` means repeating the call with the same
    /// arguments has no further effect.
    pub const fn mutation(destructive: bool, idempotent: bool) -> Self {
        Self {
            read_only: false,
            destructive,
            idempotent,
            open_world: false,
        }
    }
    pub fn to_json(self) -> Value {
        json!({"readOnlyHint": self.read_only, "destructiveHint": self.destructive,
            "idempotentHint": self.idempotent, "openWorldHint": self.open_world})
    }
}

/// Everything a client sees about a tool.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub title: String,
    pub description: String,
    /// JSON Schema (`type: object`). The registry enforces its `properties` and
    /// `required` members before a tool runs.
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Annotations,
    pub access: AccessLevel,
}

impl ToolDefinition {
    pub fn to_json(&self) -> Value {
        let mut tool = json!({"name": self.name, "title": self.title,
            "description": self.description, "inputSchema": self.input_schema,
            "annotations": self.annotations.to_json()});
        if let Some(schema) = &self.output_schema {
            tool["outputSchema"] = schema.clone();
        }
        tool
    }
}

/// One callable tool. Implementations must be deterministic for a given project
/// state, bounded in time and output, and must stay inside `cx.root`.
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn call(&self, cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome;
}

/// Supplies additional tools to a server. The server lists a provider's tool only
/// when [`Access`] allows its declared [`AccessLevel`].
pub trait ToolProvider: Send + Sync {
    fn tools(&self) -> Vec<Box<dyn Tool>>;
}

pub type ToolOutcome = std::result::Result<ToolResult, ToolFailure>;

/// Extra MCP content blocks returned next to the structured JSON result.
#[derive(Clone, Debug)]
pub enum Content {
    Text(String),
    /// Base64-encoded image data.
    Image {
        data: String,
        mime_type: String,
    },
}

impl Content {
    fn to_json(&self) -> Value {
        match self {
            Self::Text(text) => json!({"type": "text", "text": text}),
            Self::Image { data, mime_type } => {
                json!({"type": "image", "data": data, "mimeType": mime_type})
            }
        }
    }
}

/// A successful tool result. `data` is returned as `structuredContent` and
/// mirrored as the first text content block; `ok: true` is added when missing.
#[derive(Clone, Debug)]
pub struct ToolResult {
    pub data: Value,
    pub extra_content: Vec<Content>,
}

impl ToolResult {
    pub fn new(mut data: Value) -> Self {
        if let Some(object) = data.as_object_mut() {
            object.entry("ok").or_insert(Value::Bool(true));
        }
        Self {
            data,
            extra_content: Vec::new(),
        }
    }
    pub fn with_content(mut self, content: Content) -> Self {
        self.extra_content.push(content);
        self
    }
}

/// A structured tool failure, returned as `{ok:false, error:{code,message,hint}}`
/// (plus `diagnostics` when engine diagnostics are available) with `isError`.
///
/// The data is boxed so `Result<_, ToolFailure>` stays small; fields are reached
/// through `Deref`/`DerefMut` (`failure.code`, `failure.hint = ...`).
#[derive(Clone, Debug)]
pub struct ToolFailure(Box<FailureData>);

#[derive(Clone, Debug)]
pub struct FailureData {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub details: Option<Value>,
    pub diagnostics: Vec<Value>,
}

impl std::ops::Deref for ToolFailure {
    type Target = FailureData;
    fn deref(&self) -> &FailureData {
        &self.0
    }
}

impl std::ops::DerefMut for ToolFailure {
    fn deref_mut(&mut self) -> &mut FailureData {
        &mut self.0
    }
}

impl ToolFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self(Box::new(FailureData {
            code: code.into(),
            message: message.into(),
            hint: None,
            details: None,
            diagnostics: Vec::new(),
        }))
    }
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.0.hint = Some(hint.into());
        self
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.0.details = Some(details);
        self
    }
    pub fn with_diagnostics(mut self, diagnostics: Vec<Value>) -> Self {
        self.0.diagnostics = diagnostics;
        self
    }
    pub fn to_json(&self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(hint) = &self.hint {
            error["hint"] = json!(hint);
        }
        if let Some(details) = &self.details {
            error["details"] = details.clone();
        }
        let mut value = json!({"ok": false, "error": error});
        if !self.diagnostics.is_empty() {
            value["diagnostics"] = Value::Array(self.diagnostics.clone());
        }
        value
    }

    /// Removes the host's project root from messages and diagnostics so results
    /// never reveal where the project lives on disk.
    pub(crate) fn sanitize(&mut self, root: &Path) {
        let verbatim = root.to_string_lossy().replace('\\', "/");
        let plain = verbatim
            .strip_prefix("//?/")
            .unwrap_or(&verbatim)
            .to_owned();
        let scrub = |text: &str| -> String {
            let slashed = text.replace('\\', "/");
            if ![verbatim.as_str(), plain.as_str()]
                .iter()
                .any(|needle| !needle.is_empty() && slashed.contains(needle))
            {
                return text.to_owned();
            }
            let mut text = slashed;
            for needle in [verbatim.as_str(), plain.as_str()] {
                if !needle.is_empty() {
                    text = text.replace(&format!("{needle}/"), "").replace(needle, ".");
                }
            }
            text
        };
        self.message = scrub(&self.message);
        for diagnostic in &mut self.diagnostics {
            for key in ["file", "path", "message"] {
                if let Some(text) = diagnostic[key].as_str() {
                    let cleaned = scrub(text);
                    // Anything still absolute cannot be shown relative to the
                    // project: keep only its final component.
                    let cleaned = if key == "file" && looks_absolute(&cleaned) {
                        cleaned.rsplit('/').next().unwrap_or("").to_owned()
                    } else {
                        cleaned
                    };
                    diagnostic[key] = json!(cleaned);
                }
            }
        }
    }
}

fn looks_absolute(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':')
}

impl From<engine_project::Error> for ToolFailure {
    fn from(error: engine_project::Error) -> Self {
        let diagnostics: Vec<Value> = error
            .diagnostics
            .iter()
            .map(|d| {
                json!({"code": d.code, "severity": d.severity,
                    "file": d.file.to_string_lossy().replace('\\', "/"),
                    "path": d.path, "message": d.message})
            })
            .collect();
        let Some(first) = error.diagnostics.first() else {
            return Self::new(
                "project_invalid",
                "the project reported an unspecified error",
            );
        };
        let mut message = first.message.clone();
        if error.diagnostics.len() > 1 {
            message.push_str(&format!(
                " (and {} more diagnostics)",
                error.diagnostics.len() - 1
            ));
        }
        let mut failure = if first.code == "scene.revision.conflict" {
            Self::new("revision_conflict", message).with_hint(
                "The scene changed since the revision you supplied. Re-read it with \
                 scene_inspect or entity_get and retry with the returned revision.",
            )
        } else {
            Self::new(first.code.clone(), message)
        };
        if failure.hint.is_none() {
            failure.hint = Some(
                "Run project_validate for the complete diagnostics list; fix the reported \
                 file/path, then retry."
                    .into(),
            );
        }
        failure.diagnostics = diagnostics;
        failure
    }
}

/// Per-call environment handed to tools.
pub struct ToolContext<'a> {
    /// Canonical project root. Tools never touch files outside it.
    pub root: &'a Path,
    pub limits: &'a Limits,
    pub access: Access,
}

impl ToolContext<'_> {
    /// Loads and validates `project.json` in the project root.
    pub fn project(&self) -> std::result::Result<Project, ToolFailure> {
        let Ok(manifest) = engine_project::confined_file(self.root, Path::new("project.json"))
        else {
            return Err(ToolFailure::new(
                "project_missing",
                "opened project manifest project.json is missing or outside the project root",
            )
            .with_hint("Point the server at a directory that contains project.json."));
        };
        Project::load(manifest).map_err(ToolFailure::from)
    }

    /// Resolves a client-supplied project-relative path to an existing,
    /// canonical regular file inside the root. `field` names the argument in the
    /// error. Rejects absolute paths, `..`, links leaving the root and `.git`.
    pub fn existing_file(
        &self,
        field: &str,
        input: &Value,
    ) -> std::result::Result<PathBuf, ToolFailure> {
        let invalid = || paths::invalid_path(field);
        let text = input.as_str().ok_or_else(invalid)?;
        paths::resolve_existing_file(self.root, text).ok_or_else(invalid)
    }

    /// Forward-slash path of a canonical file relative to the root.
    pub fn relative(&self, canonical: &Path) -> String {
        paths::display_relative(self.root, canonical)
    }

    /// Defence in depth: write and exec tools re-check the server's access.
    pub fn require(&self, level: AccessLevel) -> std::result::Result<(), ToolFailure> {
        if self.access.allows(level) {
            Ok(())
        } else {
            Err(disabled_failure(level))
        }
    }
}

pub(crate) fn disabled_failure(level: AccessLevel) -> ToolFailure {
    let flag = match level {
        AccessLevel::Read => "",
        AccessLevel::Write => "--allow-write",
        AccessLevel::Exec => "--allow-exec",
    };
    ToolFailure::new(
        format!("{}_disabled", level.as_str()),
        format!("this server was started without {} access", level.as_str()),
    )
    .with_hint(format!(
        "Restart the server with `{flag}` to enable this tool."
    ))
}

/// Tools shipped by other crates that are gated even when not registered, so a
/// client calling them gets a helpful error rather than "unknown tool".
const KNOWN_EXEC_TOOLS: [&str; 3] = ["project_build", "project_run_headless", "render_screenshot"];

struct Entry {
    definition: ToolDefinition,
    json: Value,
    tool: Box<dyn Tool>,
}

/// The tools a server exposes, fixed at construction.
pub struct Registry {
    entries: Vec<Entry>,
    gated: BTreeMap<String, AccessLevel>,
}

impl Registry {
    pub(crate) fn build(config: &ServerConfig) -> Result<Self> {
        let mut registry = Self {
            entries: Vec::new(),
            gated: KNOWN_EXEC_TOOLS
                .iter()
                .map(|name| ((*name).to_owned(), AccessLevel::Exec))
                .collect(),
        };
        let mut candidates = crate::tools::builtin();
        for provider in &config.providers {
            candidates.extend(provider.tools());
        }
        for tool in candidates {
            let definition = tool.definition();
            if registry
                .entries
                .iter()
                .any(|e| e.definition.name == definition.name)
            {
                return Err(Error::InvalidConfig(format!(
                    "duplicate tool name {}",
                    definition.name
                )));
            }
            if config.access.allows(definition.access) {
                registry.gated.remove(&definition.name);
                registry.entries.push(Entry {
                    json: definition.to_json(),
                    definition,
                    tool,
                });
            } else {
                registry.gated.insert(definition.name, definition.access);
            }
        }
        Ok(registry)
    }

    pub fn definitions(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.entries.iter().map(|e| &e.definition)
    }

    pub(crate) fn list_json(&self) -> Vec<Value> {
        self.entries.iter().map(|e| e.json.clone()).collect()
    }

    pub(crate) fn gated_level(&self, name: &str) -> Option<AccessLevel> {
        self.gated.get(name).copied()
    }

    /// Runs a tool. `Err` is a JSON-RPC invalid-params message (unknown tool,
    /// unknown or missing argument); everything else is a tool result.
    pub(crate) fn call(
        &self,
        cx: &ToolContext<'_>,
        name: &str,
        args: &Map<String, Value>,
    ) -> std::result::Result<Value, String> {
        let Some(entry) = self.entries.iter().find(|e| e.definition.name == name) else {
            return Err(match self.gated_level(name) {
                Some(level) => format!(
                    "tool {name} is disabled: the server was started without {} access",
                    level.as_str()
                ),
                None => format!("unknown tool: {name}"),
            });
        };
        validate_arguments(&entry.definition, args)?;
        let outcome =
            catch_unwind(AssertUnwindSafe(|| entry.tool.call(cx, args))).unwrap_or_else(|_| {
                Err(ToolFailure::new(
                    "internal_error",
                    format!("tool {name} failed unexpectedly"),
                ))
            });
        Ok(match outcome {
            Ok(result) => tool_result_json(&result.data, &result.extra_content, false),
            Err(mut failure) => {
                failure.sanitize(cx.root);
                tool_result_json(&failure.to_json(), &[], true)
            }
        })
    }
}

fn validate_arguments(
    definition: &ToolDefinition,
    args: &Map<String, Value>,
) -> std::result::Result<(), String> {
    let name = &definition.name;
    let properties = definition.input_schema["properties"].as_object();
    let accepted = || {
        properties
            .map(|p| p.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    };
    for key in args.keys() {
        if !properties.is_some_and(|p| p.contains_key(key)) {
            return Err(format!(
                "{name} does not accept argument `{key}`; accepted arguments: {}",
                accepted()
            ));
        }
    }
    if let Some(required) = definition.input_schema["required"].as_array() {
        for key in required.iter().filter_map(Value::as_str) {
            if !args.contains_key(key) {
                return Err(format!("{name} requires argument `{key}`"));
            }
        }
    }
    Ok(())
}

pub(crate) fn tool_result_json(data: &Value, extra: &[Content], failed: bool) -> Value {
    let mut content = vec![json!({"type": "text", "text": data.to_string()})];
    content.extend(extra.iter().map(Content::to_json));
    json!({"content": content, "structuredContent": data, "isError": failed})
}
