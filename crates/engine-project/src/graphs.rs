//! Graph assets: the `graph` asset kind and the registry of graph kinds.
//!
//! A graph asset is one JSON [`GraphDocument`]. Its `kind` field selects a
//! [`GraphKind`] from [`kinds`], which supplies the node types and the semantic
//! checks. Project validation, the runtime and the MCP graph tools all go through
//! this module, so a kind registered here is validated, listed and edited
//! everywhere.
//!
//! # Adding a graph kind
//!
//! A domain defines `fn kind() -> GraphKind` in its own crate (see
//! `engine_ai::behavior_tree::kind`) and appends it to the array in [`build`].
//! Nothing else changes: `graph_schema`, `graph_validate`, `graph_edit` and
//! project validation pick it up. See `docs/ai-graphs.md`.
use crate::{Diagnostic, Error, Project, Result};
use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphKind, MAX_DOCUMENT_BYTES, Strictness, validate,
};
use std::{collections::BTreeMap, path::Path, sync::OnceLock};

/// Diagnostic code for a document whose `kind` no domain registered.
pub const KIND_UNKNOWN: &str = "graph.kind.unknown";

/// Every registered graph kind, by name.
pub struct GraphKinds {
    kinds: BTreeMap<String, GraphKind>,
}

impl GraphKinds {
    pub fn get(&self, name: &str) -> Option<&GraphKind> {
        self.kinds.get(name)
    }
    /// Kinds ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &GraphKind> {
        self.kinds.values()
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.kinds.keys().map(String::as_str)
    }
}

/// The kinds shipped with the engine. Append other domains' kinds here.
fn build() -> GraphKinds {
    let kinds = [
        engine_ai::behavior_tree::kind(),
        engine_ai::state_tree::kind(),
        engine_gameplay::dialogue::kind(),
    ];
    GraphKinds {
        kinds: kinds.into_iter().map(|k| (k.name.clone(), k)).collect(),
    }
}

/// The process-wide kind registry.
pub fn kinds() -> &'static GraphKinds {
    static KINDS: OnceLock<GraphKinds> = OnceLock::new();
    KINDS.get_or_init(build)
}

/// Parses a graph document (bounded, unknown fields rejected).
pub fn parse_document(bytes: &[u8]) -> std::result::Result<GraphDocument, GraphDiagnostic> {
    GraphDocument::from_json(bytes)
}

/// Validates `document` against its own kind.
pub fn validate_document(document: &GraphDocument, level: Strictness) -> Vec<GraphDiagnostic> {
    match kinds().get(&document.kind) {
        Some(kind) => validate(document, kind, level),
        None => vec![GraphDiagnostic::new(
            KIND_UNKNOWN,
            "/kind",
            format!(
                "no graph kind {:?}; registered kinds: {}",
                document.kind,
                kinds().names().collect::<Vec<_>>().join(", ")
            ),
        )],
    }
}

/// Converts graph diagnostics into a project error located in `file`.
pub fn into_error(diagnostics: Vec<GraphDiagnostic>, file: &Path) -> Error {
    Error {
        diagnostics: diagnostics
            .into_iter()
            .map(|d| Diagnostic {
                code: d.code,
                severity: "error".into(),
                file: file.to_owned(),
                path: d.path,
                message: d.message,
            })
            .collect(),
    }
}

/// Parses and fully validates graph document bytes read from `file`.
pub fn load_document(bytes: &[u8], file: &Path) -> Result<GraphDocument> {
    let document = parse_document(bytes).map_err(|d| into_error(vec![d], file))?;
    let diagnostics = validate_document(&document, Strictness::Complete);
    if diagnostics.is_empty() {
        Ok(document)
    } else {
        Err(into_error(diagnostics, file))
    }
}

/// Loads the validated document of a `graph` asset.
pub fn load_graph(project: &Project, id: &str) -> Result<GraphDocument> {
    let asset = project.require_asset(id, "graph")?;
    let file = project.resolve_asset(id)?;
    let bytes = crate::schema::read_bounded(&file, MAX_DOCUMENT_BYTES as u64)?;
    load_document(&bytes, &asset.path)
}

/// Project validation of a `graph` asset.
pub(crate) fn validate_asset(project: &Project, id: &str) -> Result<()> {
    load_graph(project, id).map(|_| ())
}
