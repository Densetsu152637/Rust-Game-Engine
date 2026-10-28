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

/// A pure binding check run after a graph passes complete kind validation.
pub type ProjectCheck = fn(&Project, &GraphDocument) -> Vec<GraphDiagnostic>;

struct RegisteredKind {
    kind: GraphKind,
    project_check: Option<ProjectCheck>,
}

pub struct GraphKinds {
    kinds: BTreeMap<String, RegisteredKind>,
}

impl GraphKinds {
    pub fn get(&self, name: &str) -> Option<&GraphKind> {
        self.kinds.get(name).map(|entry| &entry.kind)
    }
    /// Kinds ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &GraphKind> {
        self.kinds.values().map(|entry| &entry.kind)
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.kinds.keys().map(String::as_str)
    }
    fn validate(&self, document: &GraphDocument, level: Strictness) -> Vec<GraphDiagnostic> {
        match self.get(&document.kind) {
            Some(kind) => validate(document, kind, level),
            None => vec![GraphDiagnostic::new(
                KIND_UNKNOWN,
                "/kind",
                format!(
                    "no graph kind {:?}; registered kinds: {}",
                    document.kind,
                    self.names().collect::<Vec<_>>().join(", ")
                ),
            )],
        }
    }
    /// Runs the pure project check registered for a fully valid graph kind.
    fn check_project(&self, project: &Project, document: &GraphDocument) -> Vec<GraphDiagnostic> {
        self.kinds
            .get(&document.kind)
            .and_then(|entry| entry.project_check)
            .map_or_else(Vec::new, |check| check(project, document))
            .into_iter()
            .take(engine_graph::MAX_DIAGNOSTICS)
            .collect()
    }

    fn validate_with_project(
        &self,
        project: &Project,
        document: &GraphDocument,
        level: Strictness,
    ) -> Vec<GraphDiagnostic> {
        let diagnostics = self.validate(document, level);
        if diagnostics.is_empty() && level == Strictness::Complete {
            self.check_project(project, document)
        } else {
            diagnostics
        }
    }
}

/// The kinds shipped with the engine. Append other domains' kinds here.
fn build() -> GraphKinds {
    let kinds: [(GraphKind, Option<ProjectCheck>); 7] = [
        (engine_ai::behavior_tree::kind(), None),
        (engine_animation::graph::kind(), None),
        (engine_audio::graph::kind(), None),
        (engine_ai::state_tree::kind(), None),
        (engine_material_graph::material_kind(), None),
        (engine_gameplay::dialogue::kind(), None),
        (
            engine_script::graph::kind(),
            Some(crate::scripting::project_check),
        ),
    ];
    GraphKinds {
        kinds: kinds
            .into_iter()
            .map(|(kind, project_check)| {
                (
                    kind.name.clone(),
                    RegisteredKind {
                        kind,
                        project_check,
                    },
                )
            })
            .collect(),
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
    kinds().validate(document, level)
}

/// Validates a document and, for a complete valid graph, its project bindings.
pub fn validate_document_with_project(
    project: &Project,
    document: &GraphDocument,
    level: Strictness,
) -> Vec<GraphDiagnostic> {
    kinds().validate_with_project(project, document, level)
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
    let document = parse_document(&bytes).map_err(|d| into_error(vec![d], &asset.path))?;
    let diagnostics = validate_document_with_project(project, &document, Strictness::Complete);
    if diagnostics.is_empty() {
        Ok(document)
    } else {
        Err(into_error(diagnostics, &asset.path))
    }
}

/// Project validation of a `graph` asset.
pub(crate) fn validate_asset(project: &Project, id: &str) -> Result<()> {
    let document = load_graph(project, id)?;
    if document.kind == engine_material_graph::KIND {
        engine_material_graph::compile_material(&document)
            .map_err(|diagnostics| into_error(diagnostics, &project.assets[id].path))?;
    }
    if document.kind == "animation" {
        let graph = engine_animation::graph::Graph::compile(&document)
            .map_err(|d| into_error(d, Path::new(id)))?;
        use engine_animation::graph::{GraphAssetKind, GraphRigs};
        let mut clips = std::collections::BTreeMap::new();
        let mut rigs = std::collections::BTreeMap::new();
        for dependency in graph.asset_dependencies() {
            match dependency.kind {
                GraphAssetKind::Animation => {
                    clips.insert(
                        dependency.id.to_owned(),
                        crate::simulation::load_clip(project, dependency.id)?,
                    );
                }
                GraphAssetKind::Model => {
                    rigs.insert(
                        dependency.id.to_owned(),
                        load_animation_rig(project, dependency.id)?,
                    );
                }
            }
        }
        if let Some(spec) = graph.retargets().next() {
            let sources = graph
                .source_rig_ids()
                .map(|id| (id.to_owned(), rigs[id].clone()))
                .collect();
            let bindings = GraphRigs::new(
                spec.target_rig.clone(),
                rigs[&spec.target_rig].clone(),
                sources,
            )
            .map_err(|e| Error::new("animation.retarget.invalid", e.to_string()))?;
            graph
                .validate_retarget_bindings(&bindings, &clips)
                .map_err(|e| Error::new("animation.retarget.invalid", e.to_string()))?;
        }
    }
    Ok(())
}

/// Shared bounded model dependency read used by animation graph validation and runtime binding.
pub(crate) fn load_animation_rig(
    project: &Project,
    id: &str,
) -> Result<std::sync::Arc<engine_animation::Skeleton>> {
    project.require_asset(id, "model")?;
    let model = crate::model_import::load_model(&project.resolve_asset(id)?)?;
    crate::simulation::imported_skeleton(&model.scene).map(std::sync::Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentCatalog, Scene};
    use engine_graph::{NodeTypeRegistry, PortType};
    use std::path::PathBuf;

    fn check(project: &Project, _: &GraphDocument) -> Vec<GraphDiagnostic> {
        vec![GraphDiagnostic::new(
            "test.project",
            "",
            project.name.clone(),
        )]
    }

    #[test]
    fn project_check_runs_only_after_complete_kind_validation() {
        let kind = GraphKind::new("test", "test", NodeTypeRegistry::new());
        let registry = GraphKinds {
            kinds: BTreeMap::from([(
                "test".into(),
                RegisteredKind {
                    kind,
                    project_check: Some(check),
                },
            )]),
        };
        let project = Project {
            component_catalog: ComponentCatalog::new(Vec::new()).unwrap(),
            schema: 1,
            name: "context".into(),
            root: PathBuf::new(),
            manifest: PathBuf::new(),
            startup_scene: PathBuf::new(),
            rust_manifest: PathBuf::new(),
            assets: BTreeMap::new(),
            input_actions: BTreeMap::new(),
            input_bindings: BTreeMap::new(),
            scene: Scene {
                schema: 1,
                id: "scene".into(),
                entities: Vec::new(),
                source: PathBuf::new(),
            },
        };
        let mut document = GraphDocument::new("test");
        assert!(
            registry
                .validate_with_project(&project, &document, Strictness::Structure)
                .is_empty()
        );
        let diagnostics = registry.validate_with_project(&project, &document, Strictness::Complete);
        assert_eq!(
            diagnostics,
            vec![GraphDiagnostic::new("test.project", "", "context")]
        );
        document.variables.push(engine_graph::GraphVariable {
            name: "bad".into(),
            ty: PortType::Flow,
            default: None,
            description: String::new(),
        });
        let diagnostics = registry.validate_with_project(&project, &document, Strictness::Complete);
        assert_eq!(diagnostics[0].code, engine_graph::codes::VARIABLE_TYPE);
        assert!(!diagnostics.iter().any(|d| d.code == "test.project"));
    }
}
