//! Graph authoring tools: `graph_schema`, `graph_validate` and `graph_edit`.
//!
//! Graph assets are JSON documents of the shared `engine-graph` model; the
//! registered graph kinds (behavior trees today) define their node types. These
//! tools read, validate and edit those documents. They never execute game code,
//! and `graph_edit` writes only documents that pass full validation, because an
//! invalid graph asset would stop the whole project from loading.
use super::{Spec, clean, navigation::publish_new_file, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_graph::{
    EditFailure, GRAPH_VERSION, GraphDiagnostic, GraphDocument, GraphEdit, GraphKind,
    MAX_DOCUMENT_BYTES, MAX_EDITS, Strictness, apply_edits, codes,
};
use engine_project::graphs;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "graph_schema",
                title: "Graph kinds and node types",
                description: "List the graph kinds the engine can run (for example behavior_tree) and, \
                    for one kind, every node type with its ports (name, direction, type, multiplicity, \
                    required), parameter schema (type, default, bounds, choices), category and \
                    description. Also lists the graph_edit operations and the stable diagnostic \
                    codes. Read this before authoring a graph asset. Pure metadata; no game execution.",
                input: schema::object(
                    vec![
                        (
                            "kind",
                            schema::string(
                                "Graph kind to describe in full, for example behavior_tree. Omit to list every kind briefly.",
                            ),
                        ),
                        (
                            "node_type",
                            schema::string("With `kind`: describe only this node type."),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    ("document_version", json!({"type": "integer"})),
                    (
                        "kinds",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    (
                        "edit_operations",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    (
                        "diagnostic_codes",
                        json!({"type": "array", "items": {"type": "string"}}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            graph_schema,
        ),
        tool(
            Spec {
                name: "graph_validate",
                title: "Validate a graph",
                description: "Validate a graph document against its kind and return coded diagnostics \
                    (code, path, message). Give exactly one of `asset` (a graph asset id from \
                    project.json), `path` (a project-relative .json file) or `document` (an inline \
                    graph object). `level` complete (default) applies every rule; structure skips the \
                    rules a half-built graph cannot meet yet (required ports and params, the root \
                    rule, reachability). Works on a graph whose asset is currently invalid. \
                    Paths in diagnostics name nodes by id, for example /nodes/seq/params/count.",
                input: schema::object(
                    vec![
                        (
                            "asset",
                            schema::string("Graph asset id registered in project.json."),
                        ),
                        ("path", schema::string("Project-relative graph .json file.")),
                        (
                            "document",
                            json!({"type": "object", "description": "An inline graph document."}),
                        ),
                        (
                            "level",
                            schema::string_enum(
                                "complete (default) or structure.",
                                &["complete", "structure"],
                            ),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    ("valid", json!({"type": "boolean"})),
                    ("kind", json!({"type": "string"})),
                    ("level", json!({"type": "string"})),
                    ("nodes", json!({"type": "integer"})),
                    ("edges", json!({"type": "integer"})),
                    ("variables", json!({"type": "integer"})),
                    (
                        "path",
                        schema::string("Project-relative file, for asset and path inputs."),
                    ),
                    (
                        "revision",
                        schema::string("Content hash to pass to graph_edit as expected_revision."),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            graph_validate,
        ),
        tool(
            Spec {
                name: "graph_edit",
                title: "Edit a graph",
                description: "Apply a list of edit operations to a graph asset atomically. Every \
                    operation is validated as it is applied and the final document must pass full \
                    validation, otherwise nothing is written and the failure names the operation \
                    (details.op_index) with coded diagnostics. Operations (see graph_schema): \
                    add_node, remove_node, connect (optional index = position among the edges \
                    leaving the port, which is child order), disconnect, set_param (null removes), \
                    set_position, set_ports (extensible nodes), add_variable, remove_variable, set_variable, set_metadata. \
                    Target an existing graph with `asset` (id) or `path`, and pass the \
                    `expected_revision` from graph_validate or the previous graph_edit; a stale \
                    revision fails with revision_conflict. To create a graph give a new `path` and \
                    its `kind`; the first batch must build a complete graph (for a behavior_tree: \
                    root, a child and its connection). Returns the new revision, and for a new \
                    file the asset entry to add to project.json. Does not execute game code.",
                input: schema::object(
                    vec![
                        (
                            "asset",
                            schema::string("Existing graph asset id registered in project.json."),
                        ),
                        (
                            "path",
                            schema::string(
                                "Project-relative .json graph file; may be a new file in an existing directory when `kind` is given.",
                            ),
                        ),
                        (
                            "kind",
                            schema::string("Graph kind of a new file, for example behavior_tree."),
                        ),
                        (
                            "expected_revision",
                            schema::string(
                                "Revision (content hash) of the graph you are editing. Required when the file exists.",
                            ),
                        ),
                        (
                            "edits",
                            schema::array_of(
                                "Edit operations applied in order, each an object with an `op` field.",
                                json!({"type": "object", "required": ["op"]}),
                                MAX_EDITS,
                            ),
                        ),
                    ],
                    &["edits"],
                ),
                output: schema::output(vec![
                    ("path", schema::string("Project-relative graph file.")),
                    ("created", json!({"type": "boolean"})),
                    ("applied", json!({"type": "integer"})),
                    ("revision", schema::string("New content hash.")),
                    ("previous_revision", schema::string("Hash before the edit.")),
                    ("kind", json!({"type": "string"})),
                    ("nodes", json!({"type": "integer"})),
                    ("edges", json!({"type": "integer"})),
                    ("variables", json!({"type": "integer"})),
                    (
                        "asset",
                        json!({"type": "object", "description": "Asset entry to add to project.json for a new file."}),
                    ),
                ]),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            graph_edit,
        ),
    ]
}

fn unknown_kind(name: &str) -> ToolFailure {
    ToolFailure::new(
        graphs::KIND_UNKNOWN,
        format!("no graph kind named {name:?}"),
    )
    .with_hint(format!(
        "Registered kinds: {}. Call graph_schema without arguments to list them.",
        graphs::kinds().names().collect::<Vec<_>>().join(", ")
    ))
}

fn edit_operations() -> Value {
    json!([
        {"op": "add_node", "fields": {"id": "string, unique", "node_type": "string", "params": "object, optional", "position": "[x, y], optional", "ports": "array of {name, direction, type, multiple?}, optional for extensible node types"},
         "description": "Adds a node; omitted ports are copied from the node type. Authored ports must preserve every declared port."},
        {"op": "remove_node", "fields": {"id": "string"},
         "description": "Removes a node and every edge attached to it."},
        {"op": "connect", "fields": {"from": "{node, port}", "to": "{node, port}", "index": "integer, optional"},
         "description": "Adds an edge from an out port to an in port; index is the position among the edges leaving `from`."},
        {"op": "disconnect", "fields": {"from": "{node, port}", "to": "{node, port}"},
         "description": "Removes one edge."},
        {"op": "set_param", "fields": {"node": "string", "name": "string", "value": "any; null removes the param"},
         "description": "Sets one node parameter."},
        {"op": "set_position", "fields": {"node": "string", "position": "[x, y] or null"},
         "description": "Moves a node on the editor canvas."},
        {"op": "set_ports", "fields": {"node": "string", "ports": "array of {name, direction, type, multiple?}"},
         "description": "Replaces ports on an extensible node; every declared port must remain unchanged."},
        {"op": "add_variable", "fields": {"variable": "{name, type, default?, description?}"},
         "description": "Declares a variable (a blackboard key for behavior trees)."},
        {"op": "remove_variable", "fields": {"name": "string"},
         "description": "Removes a variable that no node parameter references."},
        {"op": "set_variable", "fields": {"variable": "{name, type, default?, description?}"},
         "description": "Replaces the variable of the same name."},
        {"op": "set_metadata", "fields": {"key": "string", "value": "any; null removes the entry"},
         "description": "Sets a document metadata entry."},
    ])
}

fn graph_schema(_cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let wanted = a.optional_string("kind")?;
    let node_type = a.optional_string("node_type")?;
    if node_type.is_some() && wanted.is_none() {
        return Err(schema::invalid("node_type", "used together with `kind`"));
    }
    let kinds = graphs::kinds();
    let mut listed = Vec::new();
    match wanted {
        Some(name) => {
            let kind = kinds.get(name).ok_or_else(|| unknown_kind(name))?;
            let mut info = clean(&kind.info());
            if let Some(wanted_type) = node_type {
                let found: Vec<Value> = info["node_types"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| t["name"] == wanted_type)
                    .cloned()
                    .collect();
                if found.is_empty() {
                    return Err(ToolFailure::new(
                        codes::NODE_UNKNOWN_TYPE,
                        format!("kind {name} has no node type {wanted_type:?}"),
                    )
                    .with_hint("Call graph_schema with only `kind` to list its node types."));
                }
                info["node_types"] = Value::Array(found);
            }
            listed.push(info);
        }
        None => {
            for kind in kinds.iter() {
                let mut info = clean(&kind.info());
                info["node_types"] = Value::Array(
                    kind.registry
                        .iter()
                        .map(|t| json!({"name": t.name, "category": t.category}))
                        .collect(),
                );
                listed.push(info);
            }
        }
    }
    Ok(ToolResult::new(json!({
        "document_version": GRAPH_VERSION,
        "kinds": listed,
        "edit_operations": edit_operations(),
        "diagnostic_codes": codes::ALL,
    })))
}

/// A graph file read from the project.
struct Loaded {
    relative: String,
    bytes: Vec<u8>,
    revision: String,
}

fn read_graph_file(cx: &ToolContext<'_>, file: &Path) -> Result<Loaded, ToolFailure> {
    let bytes = paths::read_bounded(file, MAX_DOCUMENT_BYTES).map_err(|error| match error {
        paths::ReadError::TooLarge => ToolFailure::new(
            codes::DOCUMENT_TOO_LARGE,
            format!("the graph file exceeds {MAX_DOCUMENT_BYTES} bytes"),
        ),
        paths::ReadError::Io => ToolFailure::new("io_error", "the graph file could not be read"),
    })?;
    Ok(Loaded {
        relative: cx.relative(file),
        revision: paths::content_hash(&bytes),
        bytes,
    })
}

/// Resolves a graph asset id through the manifest without loading the project:
/// an invalid graph must stay inspectable and repairable.
fn asset_file(cx: &ToolContext<'_>, id: &str) -> Result<PathBuf, ToolFailure> {
    let manifest =
        engine_project::confined_file(cx.root, Path::new("project.json")).map_err(|_| {
            ToolFailure::new(
                "project_missing",
                "opened project manifest project.json is missing or outside the project root",
            )
        })?;
    let bytes = paths::read_bounded(&manifest, 16 * 1024 * 1024)
        .map_err(|_| ToolFailure::new("io_error", "project.json could not be read"))?;
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|e| {
        ToolFailure::new(
            "project_invalid",
            format!("project.json is not valid JSON: {e}"),
        )
    })?;
    let entry = manifest["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["id"] == id)
        .ok_or_else(|| {
            ToolFailure::new(
                "asset_not_found",
                format!("the project catalog has no asset with ID {id}"),
            )
            .with_hint("List graph assets with asset_list kind graph.")
        })?;
    if entry["kind"] != "graph" {
        return Err(ToolFailure::new(
            "asset_kind",
            format!("asset {id} is not a graph asset"),
        ));
    }
    let relative = entry["path"].as_str().unwrap_or_default();
    paths::resolve_existing_file(cx.root, relative).ok_or_else(|| {
        ToolFailure::new(
            "asset_missing",
            format!("the file of asset {id} is missing or outside the project"),
        )
    })
}

fn diagnostic_json(file: &str, d: &GraphDiagnostic) -> Value {
    json!({"code": d.code, "severity": "error", "file": file, "path": d.path, "message": d.message})
}

/// Project context is parsed with the normal bounds and path confinement, while
/// asset validation is deferred so a broken graph can still be inspected.
fn authoring_project(cx: &ToolContext<'_>) -> Option<engine_project::Project> {
    let manifest = engine_project::confined_file(cx.root, Path::new("project.json")).ok()?;
    engine_project::Project::load_for_graph_authoring(manifest).ok()
}

fn graph_validate(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let sources = ["asset", "path", "document"]
        .into_iter()
        .filter(|name| a.raw(name).is_some_and(|v| !v.is_null()))
        .count();
    if sources != 1 {
        return Err(schema::invalid(
            "asset",
            "given together with exactly one of asset, path and document",
        ));
    }
    let level = match a.one_of("level", &["complete", "structure"])? {
        Some("structure") => Strictness::Structure,
        _ => Strictness::Complete,
    };
    let mut loaded = None;
    let parsed = if let Some(document) = a.optional_object("document")? {
        GraphDocument::from_value(Value::Object(document.clone()))
    } else {
        let file = match a.optional_string("asset")? {
            Some(id) => asset_file(cx, id)?,
            None => cx.existing_file("path", a.raw("path").unwrap_or(&Value::Null))?,
        };
        let file = read_graph_file(cx, &file)?;
        let parsed = GraphDocument::from_json(&file.bytes);
        loaded = Some(file);
        parsed
    };
    let file = loaded.as_ref().map_or("", |l| l.relative.as_str());
    let project = authoring_project(cx);
    let (diagnostics, document) = match parsed {
        Ok(document) => {
            let diagnostics = match &project {
                Some(project) => graphs::validate_document_with_project(project, &document, level),
                None => graphs::validate_document(&document, level),
            };
            (diagnostics, Some(document))
        }
        Err(diagnostic) => (vec![diagnostic], None),
    };
    let mut result = json!({
        "valid": diagnostics.is_empty(),
        "project_context_loaded": project.is_some(),
        "level": if level == Strictness::Complete { "complete" } else { "structure" },
        "diagnostics": diagnostics.iter().map(|d| diagnostic_json(file, d)).collect::<Vec<_>>(),
    });
    if let Some(document) = &document {
        result["kind"] = json!(document.kind);
        result["nodes"] = json!(document.nodes.len());
        result["edges"] = json!(document.edges.len());
        result["variables"] = json!(document.variables.len());
    }
    if let Some(loaded) = &loaded {
        result["path"] = json!(loaded.relative);
        result["revision"] = json!(loaded.revision);
    }
    Ok(ToolResult::new(result))
}

fn parse_edits(raw: Option<&Value>) -> Result<Vec<GraphEdit>, ToolFailure> {
    let items = raw
        .and_then(Value::as_array)
        .ok_or_else(|| schema::invalid("edits", "an array of edit operations"))?;
    if items.is_empty() {
        return Err(schema::invalid("edits", "a non-empty array"));
    }
    if items.len() > MAX_EDITS {
        return Err(schema::invalid(
            "edits",
            &format!("an array of at most {MAX_EDITS} operations"),
        ));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            serde_json::from_value::<GraphEdit>(item.clone()).map_err(|error| {
                ToolFailure::new(
                    "invalid_argument",
                    format!("edits[{index}] is not a valid operation: {error}"),
                )
                .with_hint("Call graph_schema for the operations and their fields.")
            })
        })
        .collect()
}

fn edit_failure(failure: EditFailure, file: &str) -> ToolFailure {
    let first = &failure.diagnostics[0];
    let mut message = first.message.clone();
    if let Some(index) = failure.op_index {
        message = format!("edits[{index}]: {message}");
    }
    if failure.diagnostics.len() > 1 {
        message.push_str(&format!(
            " (and {} more diagnostics)",
            failure.diagnostics.len() - 1
        ));
    }
    ToolFailure::new(first.code.clone(), message)
        .with_hint(
            "Nothing was written. Fix the reported node, port or parameter and send the whole \
             batch again; graph_schema lists node types, ports and parameter bounds.",
        )
        .with_details(json!({"op_index": failure.op_index, "written": false}))
        .with_diagnostics(
            failure
                .diagnostics
                .iter()
                .map(|d| diagnostic_json(file, d))
                .collect(),
        )
}

fn graph_edit(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let edits = parse_edits(a.raw("edits"))?;
    let expected = a.optional_string("expected_revision")?;
    let kind_name = a.optional_string("kind")?;
    let (asset, path) = (a.optional_string("asset")?, a.optional_string("path")?);
    if asset.is_some() == path.is_some() {
        return Err(schema::invalid(
            "asset",
            "given together with exactly one of asset and path",
        ));
    }

    // Resolve the target: an existing file, or a new one inside the project.
    let (file, relative, existing) = match (asset, path) {
        (Some(id), _) => {
            let file = asset_file(cx, id)?;
            let loaded = read_graph_file(cx, &file)?;
            (file, loaded.relative.clone(), Some(loaded))
        }
        (None, Some(path)) => {
            let target = paths::resolve_write_target(cx.root, path, "path")?;
            if target.needs_directories
                || target.path.extension().and_then(|s| s.to_str()) != Some("json")
            {
                return Err(schema::invalid(
                    "path",
                    "a .json file in an existing project directory",
                ));
            }
            let relative = paths::display_relative(cx.root, &target.path);
            let existing = if target.exists {
                Some(read_graph_file(cx, &target.path)?)
            } else {
                None
            };
            (target.path, relative, existing)
        }
        (None, None) => unreachable!("one of asset and path is present"),
    };

    let (mut document, previous) = match existing {
        Some(loaded) => {
            match expected {
                None => {
                    return Err(ToolFailure::new(
                        "hash_required",
                        "the graph already exists; editing needs expected_revision",
                    )
                    .with_hint(
                        "Call graph_validate and pass its `revision` as expected_revision.",
                    ));
                }
                Some(expected) if expected != loaded.revision => {
                    return Err(ToolFailure::new(
                        "revision_conflict",
                        "the graph changed since the supplied expected_revision",
                    )
                    .with_hint(
                        "Re-read it with graph_validate, rebuild your edits against the current \
                         document and retry with the returned revision.",
                    ));
                }
                Some(_) => {}
            }
            let document = GraphDocument::from_json(&loaded.bytes)
                .map_err(|d| edit_failure(failure_of(d), &relative))?;
            if kind_name.is_some_and(|k| k != document.kind) {
                return Err(schema::invalid(
                    "kind",
                    &format!(
                        "omitted or {:?}, the kind of the existing graph",
                        document.kind
                    ),
                ));
            }
            (document, Some(loaded.revision))
        }
        None => {
            if expected.is_some() {
                return Err(ToolFailure::new(
                    "file_not_found",
                    "expected_revision was supplied but the graph does not exist",
                )
                .with_hint("Omit expected_revision to create a new graph."));
            }
            let kind = kind_name.ok_or_else(|| {
                schema::invalid("kind", "given when the graph file does not exist yet")
            })?;
            (GraphDocument::new(kind), None)
        }
    };
    let kind: &GraphKind = graphs::kinds()
        .get(&document.kind)
        .ok_or_else(|| unknown_kind(&document.kind))?;

    apply_edits(&mut document, &edits, kind, Strictness::Complete)
        .map_err(|failure| edit_failure(failure, &relative))?;

    if let Some(project) = authoring_project(cx) {
        let diagnostics =
            graphs::validate_document_with_project(&project, &document, Strictness::Complete);
        if !diagnostics.is_empty() {
            return Err(edit_failure(
                EditFailure {
                    op_index: None,
                    diagnostics,
                },
                &relative,
            ));
        }
    }

    let text = document.to_json_pretty();
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(ToolFailure::new(
            codes::DOCUMENT_TOO_LARGE,
            format!("the edited graph would exceed {MAX_DOCUMENT_BYTES} bytes"),
        ));
    }
    let created = previous.is_none();
    if created {
        publish_new_file(cx, &relative, text.as_bytes(), "graph.publish")?;
    } else {
        // Recheck the file just before replacing it, to narrow the race window.
        let current = read_graph_file(cx, &file)?;
        if Some(&current.revision) != previous.as_ref() {
            return Err(ToolFailure::new(
                "revision_conflict",
                "the graph changed while the edit was being applied",
            )
            .with_hint("Re-read it with graph_validate and retry."));
        }
        paths::atomic_write(&file, text.as_bytes()).map_err(|e| {
            ToolFailure::new(
                "io_error",
                format!("the graph could not be written: {}", e.kind()),
            )
        })?;
    }

    let mut result = json!({
        "path": relative,
        "created": created,
        "applied": edits.len(),
        "revision": paths::content_hash(text.as_bytes()),
        "kind": document.kind,
        "nodes": document.nodes.len(),
        "edges": document.edges.len(),
        "variables": document.variables.len(),
    });
    if let Some(previous) = previous {
        result["previous_revision"] = json!(previous);
    }
    if created {
        result["asset"] =
            json!({"id": "choose-a-unique-graph-id", "kind": "graph", "path": relative});
    }
    Ok(ToolResult::new(result))
}

fn failure_of(diagnostic: GraphDiagnostic) -> EditFailure {
    EditFailure {
        op_index: None,
        diagnostics: vec![diagnostic],
    }
}
