//! Procedural content generation tools: author, edit, validate and run PCG graphs.
//!
//! Graphs are JSON files (project asset kind `pcg`) in the `engine-pcg` format. The tools only
//! use the built-in node library and a flat ground; they never execute game code. `pcg_run`
//! can bake the generated instances into a scene as ordinary entities.
use super::{Spec, navigation, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_pcg::{
    Aabb, Diagnostic, EditOp, FlatSurface, Graph, GraphBuilder, Registry, RunSettings, apply_edit,
};
use engine_project::{EditOperation, SceneDocument};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Most operations one `pcg_edit` call applies.
const MAX_OPERATIONS: usize = 256;
/// Most entities `pcg_run` writes into a scene in one call.
const MAX_WRITTEN_ENTITIES: usize = 8192;
/// Most instances echoed back as a sample.
const MAX_SAMPLE: u64 = 64;
const MAX_GRAPH_BYTES: usize = 1024 * 1024;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "pcg_create",
                title: "Create PCG graph",
                description: "Create a new procedural content generation graph JSON file (project asset kind `pcg`). template `empty` writes a graph with no nodes; `scatter` writes poisson_sampler -> transform_points -> spawner using the given mesh `assets`. Never overwrites. Returns the asset entry to add to project.json; edit it with pcg_edit and attach it to a scene with an engine.PcgVolume component.",
                input: schema::object(
                    vec![
                        (
                            "output",
                            schema::string(
                                "New project-relative .json file in an existing directory.",
                            ),
                        ),
                        (
                            "seed",
                            schema::integer("Graph seed.", 0, 9_007_199_254_740_991, Some(0)),
                        ),
                        (
                            "template",
                            schema::string_enum("Starting point.", &["empty", "scatter"]),
                        ),
                        (
                            "assets",
                            schema::array_of(
                                "Mesh asset ids spawned by the `scatter` template (equal weights).",
                                json!({"type": "string"}),
                                64,
                            ),
                        ),
                    ],
                    &["output"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type": "string"})),
                    ("hash", json!({"type": "string"})),
                    ("nodes", json!({"type": "integer"})),
                    ("asset", json!({"type": "object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            create,
        ),
        tool(
            Spec {
                name: "pcg_edit",
                title: "Edit PCG graph",
                description: "Atomically apply ordered edit operations to a PCG graph file: add_node {id,kind,params?}, remove_node {id}, connect {from:{node,port},to:{node,port}}, disconnect {from,to}, set_parameter {node,name,value}, clear_parameter {node,name}, set_seed {seed}. Each operation is checked as it is applied (types, ranges, cycles); any failure leaves the file unchanged. Incomplete graphs may be saved: the result lists remaining problems. Requires expected_hash from file_read, pcg_validate or the previous edit.",
                input: schema::object(
                    vec![
                        ("path", schema::string("Project-relative graph file.")),
                        (
                            "expected_hash",
                            schema::string("`hash` of the version being edited."),
                        ),
                        (
                            "operations",
                            json!({"type": "array", "maxItems": MAX_OPERATIONS, "items": {"type": "object"},
                                "description": "Ordered edit operations."}),
                        ),
                        (
                            "dry_run",
                            schema::boolean("Validate and report without saving.", false),
                        ),
                    ],
                    &["path", "expected_hash", "operations"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type": "string"})),
                    ("saved", json!({"type": "boolean"})),
                    ("previous_hash", json!({"type": "string"})),
                    ("hash", json!({"type": "string"})),
                    ("nodes", json!({"type": "integer"})),
                    ("edges", json!({"type": "integer"})),
                    ("problems", json!({"type": "array"})),
                ]),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            edit,
        ),
        tool(
            Spec {
                name: "pcg_validate",
                title: "Validate PCG graph",
                description: "Validate a PCG graph file against the built-in node library: unknown kinds, ports and parameters, parameter ranges, edge type mismatches, double inputs, missing required inputs, cycles and size limits. Optionally lists the node kinds (`kinds`) and/or performs a bounded read-only dry run (`run`) on flat ground returning counts, a bounding box, per-asset totals, a content hash and a sample of instances.",
                input: schema::object(
                    {
                        let mut properties = vec![
                            ("path", schema::string("Project-relative graph file.")),
                            (
                                "kinds",
                                schema::boolean(
                                    "Include the node kind catalog with ports and parameters.",
                                    false,
                                ),
                            ),
                            (
                                "run",
                                schema::boolean(
                                    "Also execute the graph and summarise the result.",
                                    false,
                                ),
                            ),
                        ];
                        properties.extend(run_properties());
                        properties
                    },
                    &["path"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type": "string"})),
                    ("hash", json!({"type": "string"})),
                    ("valid", json!({"type": "boolean"})),
                    ("nodes", json!({"type": "integer"})),
                    ("edges", json!({"type": "integer"})),
                    ("problems", json!({"type": "array"})),
                    ("kinds", json!({"type": "array"})),
                    ("run", json!({"type": "object"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            validate,
        ),
        tool(
            Spec {
                name: "pcg_run",
                title: "Run PCG graph",
                description: "Execute a PCG graph on flat ground inside a box and return a bounded summary (counts, bounding box, per-asset totals, content hash, sample). With `scene` and `expected_revision` the instances are also written into that scene as entities `pcg:<volume>:<n>` (mesh renderers, or a transform anchor plus the prefab's entities), replacing any earlier entities with that prefix, in one revisioned atomic edit. Use mode `baked` on the matching engine.PcgVolume so the runtime does not generate them again.",
                input: schema::object(
                    {
                        let mut properties = vec![
                            ("path", schema::string("Project-relative graph file.")),
                            (
                                "scene",
                                schema::string(
                                    "Scene to write the entities into (omit for a summary only).",
                                ),
                            ),
                            (
                                "expected_revision",
                                schema::string("Scene revision; required with `scene`."),
                            ),
                            (
                                "volume",
                                schema::string(
                                    "Id infix of the written entities (default `pcg`); 1-64 characters of [A-Za-z0-9_.-].",
                                ),
                            ),
                            (
                                "dry_run",
                                schema::boolean("Validate the scene edit without saving.", false),
                            ),
                        ];
                        properties.extend(run_properties());
                        properties
                    },
                    &["path"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type": "string"})),
                    ("run", json!({"type": "object"})),
                    ("scene", json!({"type": "string"})),
                    ("saved", json!({"type": "boolean"})),
                    ("revision", json!({"type": "string"})),
                    ("written", json!({"type": "integer"})),
                    ("removed", json!({"type": "integer"})),
                ]),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            run,
        ),
    ]
}

fn run_properties() -> Vec<(&'static str, Value)> {
    let vector = |description: &str| {
        json!({"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "number"},
            "description": description})
    };
    vec![
        (
            "seed",
            schema::integer(
                "Run seed (mixed with the graph seed).",
                0,
                9_007_199_254_740_991,
                Some(0),
            ),
        ),
        (
            "center",
            vector("Box centre [x, y, z] (default [0, 0, 0])."),
        ),
        (
            "size",
            vector("Box size [x, y, z] in metres (default [32, 64, 32])."),
        ),
        (
            "origin_offset",
            vector("Absolute position of the local origin; noise samples at offset + local."),
        ),
        (
            "ground_height",
            json!({"type": "number", "default": 0, "description": "Height of the flat ground."}),
        ),
        (
            "max_instances",
            schema::integer("Instance cap.", 1, 65536, Some(4096)),
        ),
        (
            "sample",
            schema::integer("Instances echoed back.", 0, MAX_SAMPLE as i64, Some(8)),
        ),
    ]
}

fn failure(code: &str, message: impl Into<String>) -> ToolFailure {
    ToolFailure::new(code, message)
}

fn problem(d: &Diagnostic) -> Value {
    serde_json::to_value(d).unwrap_or(Value::Null)
}

fn vec3(args: &Map<String, Value>, name: &str, default: [f64; 3]) -> Result<[f64; 3], ToolFailure> {
    let Some(value) = args.get(name).filter(|v| !v.is_null()) else {
        return Ok(default);
    };
    let items = value.as_array().filter(|a| a.len() == 3);
    let numbers: Option<Vec<f64>> = items
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .filter(|n: &Vec<f64>| n.len() == 3 && n.iter().all(|v| v.is_finite() && v.abs() <= 1.0e9));
    match numbers {
        Some(n) => Ok([n[0], n[1], n[2]]),
        None => Err(schema::invalid(name, "an array of three finite numbers")),
    }
}

fn load_graph(
    cx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<(std::path::PathBuf, String, Vec<u8>, Graph), ToolFailure> {
    let file = cx.existing_file("path", args.get("path").unwrap_or(&Value::Null))?;
    let bytes = paths::read_bounded(&file, MAX_GRAPH_BYTES)
        .map_err(|_| failure("file_too_large", "graph files are limited to 1 MiB"))?;
    let text = paths::as_text(&bytes).ok_or_else(|| failure("not_text", "the file is not text"))?;
    let graph = Graph::from_json(text).map_err(|d| {
        failure("pcg_parse", d.to_string()).with_hint("Create graphs with pcg_create.")
    })?;
    let relative = cx.relative(&file);
    Ok((file, relative, bytes, graph))
}

// ---------------------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------------------

fn create(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let output = a.string("output")?;
    let seed = a.integer("seed", 0, 9_007_199_254_740_991, 0)?;
    let template = a
        .one_of("template", &["empty", "scatter"])?
        .unwrap_or("empty");
    let assets = a.string_array("assets")?.unwrap_or_default();
    navigation::check_new_json_target(cx, output)?;
    let registry = Registry::builtin();
    let graph = if template == "scatter" {
        if assets.is_empty() {
            return Err(schema::invalid(
                "assets",
                "at least one mesh asset for `scatter`",
            ));
        }
        let project = cx.project()?;
        for asset in &assets {
            project
                .require_asset(asset, "mesh")
                .map_err(ToolFailure::from)?;
        }
        let entries: Vec<Value> = assets
            .iter()
            .map(|asset| json!({"asset": asset, "kind": "mesh", "weight": 1}))
            .collect();
        GraphBuilder::new(&registry, seed)
            .node("points", "poisson_sampler", [("min_distance", json!(3.0))])
            .node(
                "variation",
                "transform_points",
                [
                    ("yaw_max_degrees", json!(360.0)),
                    ("scale_min", json!(0.8)),
                    ("scale_max", json!(1.2)),
                ],
            )
            .node("spawn", "spawner", [("entries", Value::Array(entries))])
            .connect("points.points", "variation.points")
            .connect("variation.points", "spawn.points")
            .build()
            .map_err(|problems| {
                failure(
                    "pcg_template",
                    format!("{} problems building the template", problems.len()),
                )
            })?
    } else {
        Graph::new(seed)
    };
    let bytes = graph.to_json().into_bytes();
    navigation::publish_new_file(cx, output, &bytes, "pcg.publish")?;
    Ok(ToolResult::new(json!({
        "path": output, "hash": paths::content_hash(&bytes), "nodes": graph.nodes.len(),
        "asset": {"id": "choose-a-unique-pcg-id", "kind": "pcg", "path": output},
    })))
}

// ---------------------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------------------

fn edit(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let expected = a.string("expected_hash")?;
    let dry_run = a.boolean("dry_run", false)?;
    let operations = args
        .get("operations")
        .and_then(Value::as_array)
        .filter(|ops| ops.len() <= MAX_OPERATIONS)
        .ok_or_else(|| schema::invalid("operations", "an array of at most 256 operations"))?;
    let (file, relative, bytes, mut graph) = load_graph(cx, args)?;
    let hash = paths::content_hash(&bytes);
    if hash != expected {
        return Err(failure(
            "revision_conflict",
            "the file changed since the supplied expected_hash",
        )
        .with_hint("Re-read the graph with file_read or pcg_validate and retry."));
    }
    let registry = Registry::builtin();
    for (index, operation) in operations.iter().enumerate() {
        let op: EditOp = serde_json::from_value(operation.clone()).map_err(|e| {
            failure("pcg_operation_invalid", format!("operation {index}: {e}"))
                .with_details(json!({"operation_index": index}))
        })?;
        apply_edit(&mut graph, &registry, op).map_err(|d| {
            failure("pcg_edit_rejected", format!("operation {index}: {d}"))
                .with_details(json!({"operation_index": index, "problem": problem(&d)}))
                .with_hint("The file was not changed. Fix the operation and retry.")
        })?;
    }
    let new_bytes = graph.to_json().into_bytes();
    if new_bytes.len() > MAX_GRAPH_BYTES {
        return Err(failure(
            "content_too_large",
            "the edited graph exceeds 1 MiB",
        ));
    }
    let new_hash = paths::content_hash(&new_bytes);
    if !dry_run {
        paths::atomic_write(&file, &new_bytes).map_err(|e| {
            failure(
                "io_error",
                format!("the graph could not be written: {}", e.kind()),
            )
        })?;
    }
    let problems: Vec<Value> = engine_pcg::validate(&graph, &registry)
        .iter()
        .map(problem)
        .collect();
    Ok(ToolResult::new(json!({
        "path": relative, "saved": !dry_run, "previous_hash": hash, "hash": new_hash,
        "nodes": graph.nodes.len(), "edges": graph.edges.len(), "problems": problems,
    })))
}

// ---------------------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------------------

fn kind_catalog(registry: &Registry) -> Vec<Value> {
    registry
        .definitions()
        .map(|d| serde_json::to_value(d).unwrap_or(Value::Null))
        .collect()
}

fn validate(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let (_, relative, bytes, graph) = load_graph(cx, args)?;
    let registry = Registry::builtin();
    let problems = engine_pcg::validate(&graph, &registry);
    let mut result = json!({
        "path": relative, "hash": paths::content_hash(&bytes), "valid": problems.is_empty(),
        "nodes": graph.nodes.len(), "edges": graph.edges.len(),
        "problems": problems.iter().map(problem).collect::<Vec<_>>(),
    });
    if a.boolean("kinds", false)? {
        result["kinds"] = Value::Array(kind_catalog(&registry));
    }
    if a.boolean("run", false)? {
        if !problems.is_empty() {
            return Err(failure("pcg_invalid", "the graph has validation problems")
                .with_details(json!({"problems": result["problems"]}))
                .with_hint("Fix the problems listed by pcg_validate, then run again."));
        }
        let summary = execute(args, &graph, &registry)?;
        result["run"] = summary.json;
    }
    Ok(ToolResult::new(result))
}

// ---------------------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------------------

struct Summary {
    json: Value,
    instances: Vec<engine_pcg::Instance>,
}

fn execute(
    args: &Map<String, Value>,
    graph: &Graph,
    registry: &Registry,
) -> Result<Summary, ToolFailure> {
    let a = Args(args);
    let seed = a.integer("seed", 0, 9_007_199_254_740_991, 0)?;
    let center = vec3(args, "center", [0.0; 3])?;
    let size = vec3(args, "size", [32.0, 64.0, 32.0])?;
    if size.iter().any(|v| *v <= 0.0 || *v > 1.0e6) {
        return Err(schema::invalid("size", "components in (0, 1e6]"));
    }
    let origin = vec3(args, "origin_offset", [0.0; 3])?;
    let ground = args
        .get("ground_height")
        .filter(|v| !v.is_null())
        .map(|v| v.as_f64().filter(|h| h.is_finite() && h.abs() <= 1.0e6))
        .unwrap_or(Some(0.0))
        .ok_or_else(|| schema::invalid("ground_height", "a finite number"))?;
    let max_instances = a.integer("max_instances", 1, 65536, 4096)? as usize;
    let sample = a.integer("sample", 0, MAX_SAMPLE, 8)? as usize;
    let bounds = Aabb::new(
        std::array::from_fn(|i| (center[i] - size[i] * 0.5) as f32),
        std::array::from_fn(|i| (center[i] + size[i] * 0.5) as f32),
    );
    let mut settings = RunSettings::new(graph.seed_for(seed), bounds);
    settings.domain_offset = origin;
    settings.limits.max_instances = max_instances;
    let surface = FlatSurface::new(ground as f32);
    let result = engine_pcg::run(graph, registry, &surface, &settings).map_err(|e| {
        failure("pcg_run_failed", e.to_string()).with_details(json!({
            "problems": e.diagnostics.iter().map(problem).collect::<Vec<_>>(),
        }))
    })?;
    let mut by_asset: BTreeMap<&str, u64> = BTreeMap::new();
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for instance in &result.instances {
        *by_asset.entry(instance.asset.as_str()).or_default() += 1;
        for axis in 0..3 {
            lo[axis] = lo[axis].min(instance.position[axis]);
            hi[axis] = hi[axis].max(instance.position[axis]);
        }
    }
    let encoded = serde_json::to_vec(&result.instances).unwrap_or_default();
    let nodes: Vec<Value> = result
        .nodes
        .iter()
        .map(|n| json!({"id": n.id, "kind": n.kind, "points": n.points, "instances": n.instances}))
        .collect();
    let json = json!({
        "seed": graph.seed_for(seed),
        "instances": result.instances.len(),
        "by_asset": by_asset,
        "bounds": if result.instances.is_empty() { Value::Null } else { json!({"min": lo, "max": hi}) },
        "hash": paths::content_hash(&encoded),
        "nodes": nodes,
        "sample": super::clean(&result.instances.iter().take(sample).collect::<Vec<_>>()),
    });
    Ok(Summary {
        json,
        instances: result.instances,
    })
}

fn run(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let scene_requested = args.get("scene").is_some_and(|v| !v.is_null());
    let dry_run = a.boolean("dry_run", false)?;
    let volume = a.optional_string("volume")?.unwrap_or("pcg");
    if volume.is_empty()
        || volume.len() > 64
        || !volume
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(schema::invalid(
            "volume",
            "1-64 characters of [A-Za-z0-9_.-]",
        ));
    }
    let expected = a.optional_string("expected_revision")?;
    if scene_requested && expected.is_none() {
        return Err(schema::invalid(
            "expected_revision",
            "supplied together with `scene`",
        ));
    }
    let (_, relative, _, graph) = load_graph(cx, args)?;
    let registry = Registry::builtin();
    let problems = engine_pcg::validate(&graph, &registry);
    if !problems.is_empty() {
        return Err(failure("pcg_invalid", "the graph has validation problems")
            .with_details(json!({"problems": problems.iter().map(problem).collect::<Vec<_>>()}))
            .with_hint("Fix the problems listed by pcg_validate, then run again."));
    }
    let summary = execute(args, &graph, &registry)?;
    let mut result = json!({"path": relative, "run": summary.json});
    if !scene_requested {
        return Ok(ToolResult::new(result));
    }

    let loaded = super::scene::load(cx, args)?;
    let expected = expected.expect("checked above");
    if loaded.revision != expected {
        return Err(failure(
            "revision_conflict",
            "the scene changed since the supplied expected_revision",
        )
        .with_hint("Re-read the scene with scene_inspect and retry with its revision."));
    }
    let prefix = format!("pcg:{volume}:");
    let entities =
        engine_project::world::pcg::instance_entities(&loaded.project, volume, &summary.instances)
            .map_err(|message| failure("pcg_spawn_failed", message))?;
    if entities.len() > MAX_WRITTEN_ENTITIES {
        return Err(failure(
            "pcg_too_large",
            format!(
                "writing {} entities exceeds the limit of {MAX_WRITTEN_ENTITIES}; lower max_instances or enlarge the spacing",
                entities.len()
            ),
        ));
    }
    let existing: Vec<String> = loaded
        .scene
        .entities
        .iter()
        .filter(|e| e.id.starts_with(&prefix))
        .map(|e| e.id.clone())
        .collect();
    let mut operations: Vec<EditOperation> = existing
        .iter()
        .map(|id| EditOperation::DeleteEntity {
            entity_id: id.clone(),
        })
        .collect();
    let written = entities.len();
    operations.extend(
        entities
            .into_iter()
            .map(|entity| EditOperation::CreateEntity { entity }),
    );
    let mut document = SceneDocument::for_scene(&loaded.project, loaded.scene);
    let revision = document
        .apply(EditOperation::Batch { operations }, expected)
        .map_err(ToolFailure::from)?;
    if !dry_run {
        document.save(&revision).map_err(ToolFailure::from)?;
    }
    result["scene"] = json!(loaded.path);
    result["saved"] = json!(!dry_run);
    result["revision"] = json!(revision);
    result["written"] = json!(written);
    result["removed"] = json!(existing.len());
    Ok(ToolResult::new(result))
}
