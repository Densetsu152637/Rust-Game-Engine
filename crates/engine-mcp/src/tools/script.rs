//! `script_schema`: the contract for authoring `script_graph` scripts.
//!
//! A read tool. It lists the script graph kind, the host functions a graph may
//! call (the engine's `engine.*` built-ins plus the project's `scriptFunctions`
//! declarations), the `engine.ScriptGraph` component, the run limits and the
//! stable codes. It never executes game code or scripts.
use super::{Spec, clean, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolOutcome, ToolResult},
    schema,
};
use engine_project::{Project, scripting};
use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![tool(
        Spec {
            name: "script_schema",
            title: "Script authoring schema",
            description: "Describe what a script_graph (visual script) may do: the graph kind with \
                every node type, the host functions a `call` node can use (built-in engine.* \
                functions and the project's scriptFunctions declarations, each with its typed \
                parameters, return type, purity, fuel cost and effects), the engine.ScriptGraph \
                component fields and capability model, the run limits (fuel, call depth, state \
                memory, emits) and the stable diagnostic and trap codes. Read this before \
                writing a script graph; then author it with graph_edit (use set_ports for the \
                call/function nodes) and check it with graph_validate, which applies the \
                project's declared functions when a project is open. Pure metadata; no game or \
                script execution.",
            input: schema::object(
                vec![(
                    "node_type",
                    schema::string("Describe only this script_graph node type."),
                )],
                &[],
            ),
            output: schema::output(vec![
                ("kind", json!({"type": "object"})),
                (
                    "functions",
                    json!({"type": "array", "items": {"type": "object"}}),
                ),
                ("component", json!({"type": "object"})),
                ("limits", json!({"type": "object"})),
                (
                    "codes",
                    json!({"type": "array", "items": {"type": "string"}}),
                ),
                ("project_functions_loaded", json!({"type": "boolean"})),
            ]),
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        },
        script_schema,
    )]
}

/// The project without validating its assets, so the contract stays readable
/// while a script graph is still invalid. `None` when no project opens.
pub(super) fn open_project(cx: &ToolContext<'_>) -> Option<Project> {
    let manifest = engine_project::confined_file(cx.root, Path::new("project.json")).ok()?;
    Project::load_for_graph_authoring(manifest).ok()
}

fn function_json(name: &str, sig: &engine_script::FnSig, builtin: bool) -> Value {
    let mut value = clean(sig);
    value["name"] = json!(name);
    value["builtin"] = json!(builtin);
    value
}

fn script_schema(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let node_type = args.get("node_type").and_then(Value::as_str);
    let kind = engine_script::graph::shared_kind();
    let mut info = clean(&kind.info());
    if let Some(wanted) = node_type {
        let found: Vec<Value> = info["node_types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["name"] == wanted)
            .cloned()
            .collect();
        if found.is_empty() {
            return Err(crate::registry::ToolFailure::new(
                engine_graph::codes::NODE_UNKNOWN_TYPE,
                format!("script_graph has no node type {wanted:?}"),
            )
            .with_hint("Call script_schema without node_type to list them."));
        }
        info["node_types"] = Value::Array(found);
    }
    let mut functions: Vec<Value> = scripting::builtin_signatures()
        .iter()
        .map(|(name, sig)| function_json(name, sig, true))
        .collect();
    let project = open_project(cx);
    let loaded = project.is_some();
    if let Some(project) = &project {
        functions.extend(
            project
                .script_functions
                .iter()
                .map(|d| function_json(&d.name, &d.sig(), false)),
        );
    }
    let descriptor = scripting::descriptor();
    Ok(ToolResult::new(json!({
        "kind": info,
        "functions": functions,
        "component": {
            "name": descriptor.name,
            "version": descriptor.version,
            "schema": descriptor.schema,
            "defaults": descriptor.defaults,
        },
        "limits": {
            "fuel_per_tick": {"default": engine_script::trap::DEFAULT_FUEL, "min": engine_script::trap::FUEL_RANGE.0, "max": engine_script::trap::FUEL_RANGE.1},
            "global_fuel_per_tick": scripting::GLOBAL_FUEL_PER_TICK,
            "max_call_depth": engine_script::trap::MAX_CALL_DEPTH,
            "max_state_bytes": engine_script::trap::MAX_STATE_BYTES,
            "max_string_bytes": engine_script::MAX_STRING_BYTES,
            "max_pending_waits": engine_script::trap::MAX_PENDING,
            "max_emits_per_instance_per_tick": scripting::MAX_EMITS_PER_INSTANCE,
            "max_emits_global_per_tick": scripting::MAX_EMITS_GLOBAL,
            "max_instances": scripting::MAX_INSTANCES,
            "spawn": "not shipped: capabilities.spawn maximum is 0",
        },
        "codes": engine_script::codes::ALL,
        "project_functions_loaded": loaded,
    })))
}
