//! Project-aware validation: the graph kind's [`ProjectCheck`](crate::graphs::ProjectCheck)
//! and the scene check of `engine.ScriptGraph` components. Nothing here runs game
//! code or touches a world.
use super::{
    component::{SCRIPT_GRAPH, ScriptGraph, TEXT_SCRIPT, TextScript},
    functions::signature_table,
    runtime::compile_program,
    text::compile_text_program,
};
use crate::{Error, Project, Result, Scene};
use engine_graph::{GraphDiagnostic, GraphDocument, GraphIndex};
use engine_script::{ScriptError, codes, graph::node};

/// The `script_graph` project check: host calls against the declared functions
/// and constant component names against the catalog.
pub(crate) fn project_check(project: &Project, document: &GraphDocument) -> Vec<GraphDiagnostic> {
    let signatures = signature_table(project);
    let mut diagnostics = match engine_script::graph::compile(document, &signatures) {
        Ok(_) => Vec::new(),
        Err(found) => found,
    };
    let index = GraphIndex::new(document);
    for call in &document.nodes {
        if call.node_type != node::CALL {
            continue;
        }
        let function = call.params.get("function").and_then(|v| v.as_str());
        if !matches!(
            function,
            Some("engine.component_get" | "engine.component_set")
        ) {
            continue;
        }
        let Some(edge) = index.incoming(call.id.as_str(), "name").first() else {
            continue;
        };
        let Some(source) = index.node(edge.from.node.as_str()) else {
            continue;
        };
        if source.node_type != node::CONST {
            continue;
        }
        if let Some(name) = source.params.get("value").and_then(|v| v.as_str())
            && !project.component_catalog.0.contains_key(name)
        {
            diagnostics.push(GraphDiagnostic::new(
                codes::CAPABILITY_UNKNOWN_COMPONENT,
                format!("/nodes/{}/ports/name", call.id),
                format!("component {name:?} is not in the project's component catalog"),
            ));
        }
    }
    diagnostics.truncate(engine_graph::MAX_DIAGNOSTICS);
    diagnostics
}

/// Scene-level checks of every `engine.ScriptGraph`: shape, the graph asset
/// compiles against the declared functions, variable overrides fit the graph and
/// capability lists name catalog components.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(SCRIPT_GRAPH) else {
            continue;
        };
        let component: ScriptGraph = serde_json::from_value(value.data.clone())?;
        component.check_shape()?;
        project.require_asset(&component.graph, "graph")?;
        for name in component
            .capabilities
            .read
            .iter()
            .chain(&component.capabilities.write)
        {
            if !project.component_catalog.0.contains_key(name) {
                return Err(Error::new(
                    codes::CAPABILITY_UNKNOWN_COMPONENT,
                    format!(
                        "entity {}: capability names component {name:?}, which is not in the catalog",
                        entity.id
                    ),
                ));
            }
        }
        let signatures = signature_table(project);
        let program = compile_program(project, &signatures, &component.graph)?;
        program
            .instantiate(&component.variables)
            .map_err(|e| Error::new(e.code, format!("entity {}: {}", entity.id, e.message)))?;
    }
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(TEXT_SCRIPT) else {
            continue;
        };
        let component: TextScript = serde_json::from_value(value.data.clone())?;
        component.check_shape()?;
        project.require_asset(&component.script, "text_script")?;
        for name in component
            .capabilities
            .read
            .iter()
            .chain(&component.capabilities.write)
        {
            if !project.component_catalog.0.contains_key(name) {
                return Err(Error::new(
                    codes::CAPABILITY_UNKNOWN_COMPONENT,
                    format!(
                        "entity {}: capability names component {name:?}, which is not in the catalog",
                        entity.id
                    ),
                ));
            }
        }
        compile_text_program(project, &component.script)?;
    }
    Ok(())
}

/// Parse and structurally validate a text asset without invoking any handler.
pub fn check_text_asset(project: &Project, id: &str) -> Result<()> {
    compile_text_program(project, id).map(|_| ())
}

/// Largest `.rhai` source accepted, in bytes (same bound as the text backend).
pub const MAX_TEXT_SOURCE_BYTES: usize = 256 * 1024;

#[cfg(feature = "text-scripting")]
const _: () = assert!(MAX_TEXT_SOURCE_BYTES == engine_script::text::MAX_SOURCE_BYTES);

/// What a successful structural check of one `.rhai` source found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextReport {
    /// Every function the source defines as `(name, parameter count)`, sorted.
    pub functions: Vec<(String, usize)>,
    /// Statically declared event subscriptions (`const events = [...]`), sorted.
    pub events: Vec<String>,
    pub source_bytes: usize,
}

/// Parse and structurally validate bounded `.rhai` source bytes. Nothing is run, no
/// top-level statement is evaluated and no project is needed.
#[cfg(feature = "text-scripting")]
pub fn check_text_source(bytes: &[u8]) -> std::result::Result<TextReport, ScriptError> {
    let program = engine_script::text::TextProgram::compile(bytes)?;
    Ok(TextReport {
        functions: program
            .functions()
            .map(|(name, arity)| (name.to_owned(), arity))
            .collect(),
        events: program.subscriptions().to_vec(),
        source_bytes: bytes.len(),
    })
}

/// Parse and structurally validate bounded `.rhai` source bytes.
#[cfg(not(feature = "text-scripting"))]
pub fn check_text_source(_bytes: &[u8]) -> std::result::Result<TextReport, ScriptError> {
    Err(ScriptError::new(
        codes::TEXT_DISABLED,
        "this build was compiled without the text-scripting feature",
    ))
}

/// Whether this build can parse and run Rhai text scripts.
pub const TEXT_SCRIPTING: bool = cfg!(feature = "text-scripting");

/// The enforced limits of the text backend as JSON; `None` without the feature.
#[cfg(feature = "text-scripting")]
pub fn text_limits() -> Option<serde_json::Value> {
    use engine_script::text as t;
    Some(serde_json::json!({
        "max_source_bytes": t::MAX_SOURCE_BYTES,
        "max_events": t::MAX_EVENTS,
        "max_call_levels": t::MAX_CALL_LEVELS,
        "max_expr_depth": t::MAX_EXPR_DEPTHS.0,
        "max_function_expr_depth": t::MAX_EXPR_DEPTHS.1,
        "max_string_bytes": engine_script::MAX_STRING_BYTES,
        "max_array_size": t::MAX_ARRAY_SIZE,
        "max_map_size": t::MAX_MAP_SIZE,
        "max_variables": t::MAX_VARIABLES,
        "max_functions": t::MAX_FUNCTIONS,
        "max_pending_timers": engine_script::trap::MAX_PENDING,
        "max_state_bytes": engine_script::trap::MAX_STATE_BYTES,
        "worker_stack_bytes": t::WORKER_STACK_BYTES,
    }))
}

/// The enforced limits of the text backend as JSON; `None` without the feature.
#[cfg(not(feature = "text-scripting"))]
pub fn text_limits() -> Option<serde_json::Value> {
    None
}
