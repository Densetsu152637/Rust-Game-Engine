//! Project-aware validation: the graph kind's [`ProjectCheck`](crate::graphs::ProjectCheck)
//! and the scene check of `engine.ScriptGraph` components. Nothing here runs game
//! code or touches a world.
use super::{
    component::{SCRIPT_GRAPH, ScriptGraph},
    functions::signature_table,
    runtime::compile_program,
};
use crate::{Error, Project, Result, Scene};
use engine_graph::{GraphDiagnostic, GraphDocument, GraphIndex};
use engine_script::{codes, graph::node};

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
    Ok(())
}
