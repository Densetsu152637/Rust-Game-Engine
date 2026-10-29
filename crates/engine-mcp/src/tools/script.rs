//! `script_schema`: the contract for authoring `script_graph` scripts.
//!
//! A read tool. It lists the script graph kind, the host functions a graph may
//! call (the engine's `engine.*` built-ins plus the project's `scriptFunctions`
//! declarations), the `engine.ScriptGraph` component, the run limits and the
//! stable codes. `script_check` parses and structurally validates one `.rhai`
//! text script. Neither tool executes game code or scripts.
use super::{Spec, clean, graph::asset_file_of_kind, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{Project, scripting};
use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![script_schema_tool(), script_check_tool()]
}

fn script_schema_tool() -> Box<dyn Tool> {
    tool(
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
                ("text", json!({"type": "object"})),
            ]),
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        },
        script_schema,
    )
}

/// Largest `.rhai` source `script_check` reads (the runtime's bound).
const MAX_SOURCE: usize = scripting::MAX_TEXT_SOURCE_BYTES;

fn script_check_tool() -> Box<dyn Tool> {
    tool(
        Spec {
            name: "script_check",
            title: "Check a Rhai text script",
            description: "Parse and structurally validate one Rhai text script (a text_script \
                asset, a .rhai file in the project, or inline source) WITHOUT running it: no \
                top-level statement and no handler is evaluated and no host function is called. \
                Reports a coded diagnostic with a line:col site (script.text.parse for syntax \
                errors, oversize or non-UTF-8 source and the disabled `import` and `eval`; \
                script.text.handler_missing for a handler with the wrong parameter count or \
                `events` without on_event; script.text.events for a malformed `const events = \
                [...]`), the functions the source defines and its static event subscriptions. \
                Handlers: on_start(), on_update(dt), on_event(name, payload, source), on_stop(); \
                timers: engine::after(seconds, \"fn_name\"). Call script_schema for the host \
                functions, limits and codes. Host calls and capabilities are checked only when \
                the script runs.",
            input: schema::object(
                vec![
                    (
                        "asset",
                        schema::string("Catalog ID of a text_script asset."),
                    ),
                    (
                        "path",
                        schema::string("Project-relative path of a .rhai file."),
                    ),
                    (
                        "source",
                        schema::string(
                            "Inline Rhai source, at most 262144 bytes. Give exactly one of asset, path and source.",
                        ),
                    ),
                ],
                &[],
            ),
            output: schema::output(vec![
                ("valid", json!({"type": "boolean"})),
                ("diagnostics", schema::diagnostics_schema()),
                (
                    "functions",
                    json!({"type": "array", "items": {"type": "object"}}),
                ),
                (
                    "events",
                    json!({"type": "array", "items": {"type": "string"}}),
                ),
                ("source_bytes", json!({"type": "integer"})),
                ("path", json!({"type": "string"})),
                ("revision", json!({"type": "string"})),
                ("limits", json!({"type": "object"})),
            ]),
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        },
        script_check,
    )
}

fn script_check(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let given = ["asset", "path", "source"]
        .into_iter()
        .filter(|name| a.raw(name).is_some_and(|v| !v.is_null()))
        .count();
    if given != 1 {
        return Err(schema::invalid(
            "asset",
            "given together with exactly one of asset, path and source",
        ));
    }
    let mut location = None;
    let bytes = if let Some(source) = a.optional_string("source")? {
        if source.len() > MAX_SOURCE {
            return Err(ToolFailure::new(
                engine_script::codes::TEXT_PARSE,
                format!("inline source exceeds {MAX_SOURCE} bytes"),
            ));
        }
        source.as_bytes().to_vec()
    } else {
        let file = match a.optional_string("asset")? {
            Some(id) => asset_file_of_kind(cx, id, "text_script")?,
            None => {
                let file = cx.existing_file("path", a.raw("path").unwrap_or(&Value::Null))?;
                if file.extension().and_then(|e| e.to_str()) != Some("rhai") {
                    return Err(schema::invalid("path", "a project-relative .rhai file"));
                }
                file
            }
        };
        let bytes = paths::read_bounded(&file, MAX_SOURCE).map_err(|error| match error {
            paths::ReadError::TooLarge => ToolFailure::new(
                engine_script::codes::TEXT_PARSE,
                format!("the script file exceeds {MAX_SOURCE} bytes"),
            ),
            paths::ReadError::Io => {
                ToolFailure::new("io_error", "the script file could not be read")
            }
        })?;
        location = Some((cx.relative(&file), paths::content_hash(&bytes)));
        bytes
    };
    let mut result = json!({
        "limits": {
            "max_source_bytes": MAX_SOURCE,
            "handlers": ["on_start()", "on_update(dt)", "on_event(name, payload, source)", "on_stop()"],
        },
        "source_bytes": bytes.len(),
    });
    match scripting::check_text_source(&bytes) {
        Ok(report) => {
            result["valid"] = json!(true);
            result["diagnostics"] = json!([]);
            result["functions"] = report
                .functions
                .iter()
                .map(|(name, parameters)| json!({"name": name, "parameters": parameters}))
                .collect();
            result["events"] = json!(report.events);
        }
        Err(error) => {
            let file = location.as_ref().map_or("", |(path, _)| path.as_str());
            let mut diagnostic = json!({
                "code": error.code,
                "severity": "error",
                "file": file,
                "message": error.message,
            });
            if let Some(site) = &error.site {
                diagnostic["path"] = json!(site.to_string());
            }
            result["valid"] = json!(false);
            result["diagnostics"] = json!([diagnostic]);
            result["functions"] = json!([]);
            result["events"] = json!([]);
        }
    }
    if let Some((path, revision)) = location {
        result["path"] = json!(path);
        result["revision"] = json!(revision);
    }
    Ok(ToolResult::new(result))
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
        "text": text_contract(),
    })))
}

/// The Rhai text backend: component, handlers, calling convention and limits.
fn text_contract() -> Value {
    let descriptor = scripting::text_descriptor();
    json!({
        "enabled": scripting::TEXT_SCRIPTING,
        "asset_kind": "text_script",
        "extension": ".rhai",
        "component": {
            "name": descriptor.name,
            "version": descriptor.version,
            "schema": descriptor.schema,
            "defaults": descriptor.defaults,
        },
        "handlers": ["on_start()", "on_update(dt)", "on_event(name, payload, source)", "on_stop()"],
        "events": "Declare subscriptions with a literal top-level `const events = [\"name\", ...];` (requires on_event).",
        "timers": "engine::after(seconds, \"fn_name\") runs a zero-parameter script function after the delay (0..3600 s).",
        "host_calls": "A host function name maps its dots to namespaces: engine.emit is engine::emit(..), game.damage is game::damage(..). Script state lives in `this` (an object map) and persists between handlers.",
        "unavailable": ["import", "eval", "modules", "file, network, process, clock and ambient randomness", "print and debug other than to the runtime log"],
        "limits": scripting::text_limits(),
        "check": "script_check parses and validates a script without running it.",
    })
}
