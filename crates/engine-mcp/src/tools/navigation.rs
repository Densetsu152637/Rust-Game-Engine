//! Shared validated navigation authoring and queries; never executes game code.
use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::simulation::{BakeSettings, Obstacle, QuerySettings};
use serde_json::{Map, Value, json};
use std::{io::Write, path::Path};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "navigation_bake",
                title: "Bake navigation mesh",
                description: "Bake explicit world-space floor mesh assets and optional solid box obstacles into a new navigation JSON file. Uses bounded layered surface rasterization, slope/step limits and conservative agent clearance. Existing files are never overwritten. Returns the navigation asset entry to add to project.json; does not execute Rust.",
                input: schema::object(
                    vec![
                        (
                            "meshes",
                            json!({"type":"array","minItems":1,"maxItems":128,"items":{"type":"string"},"description":"Mesh asset ids or project-relative mesh files forming the walkable floor."}),
                        ),
                        (
                            "output",
                            schema::string(
                                "New project-relative .json file in an existing directory.",
                            ),
                        ),
                        (
                            "settings",
                            described(
                                bake_settings(),
                                "Optional bake limits: cell size, agent radius/height, slope and step.",
                            ),
                        ),
                        (
                            "obstacles",
                            json!({"type":"array","maxItems":4096,"items":schema::object(vec![("min",vector("Minimum world-space corner [x, y, z]")),("max",vector("Maximum world-space corner [x, y, z]"))],&["min","max"]),"description":"Optional solid axis-aligned box obstacles in world space."}),
                        ),
                    ],
                    &["meshes", "output"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("polygons", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                    ("asset", json!({"type":"object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            bake,
        ),
        tool(
            Spec {
                name: "navigation_query",
                title: "Query navigation path",
                description: "Find a bounded continuous path through a saved navigation document. Endpoints project onto the nearest floor within max_distance. Returns reached, unreachable, or budget_exceeded. Uses the same bake/query API as native agents; no game execution.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Project-relative navigation JSON document."),
                        ),
                        ("start", vector("World-space path start [x, y, z].")),
                        ("goal", vector("World-space path goal [x, y, z].")),
                        (
                            "query",
                            described(
                                query_settings(),
                                "Optional query limits: snap distance and search budgets.",
                            ),
                        ),
                    ],
                    &["path", "start", "goal"],
                ),
                output: schema::output(vec![
                    ("status", json!({"type":"string"})),
                    ("points", json!({"type":"array"})),
                    ("polygons", json!({"type":"integer"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            query,
        ),
    ]
}
fn vector(description: &str) -> Value {
    json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number"},"description":description})
}
fn described(mut schema: Value, description: &str) -> Value {
    schema["description"] = json!(description);
    schema
}
fn bake_settings() -> Value {
    schema::object(
        vec![
            (
                "cell_size",
                json!({"type":"number","minimum":0.01,"maximum":100,"default":0.25}),
            ),
            (
                "agent_radius",
                json!({"type":"number","minimum":0,"maximum":100,"default":0.25}),
            ),
            (
                "agent_height",
                json!({"type":"number","minimum":0.01,"maximum":100,"default":1.8}),
            ),
            (
                "max_slope_degrees",
                json!({"type":"number","minimum":0,"maximum":80,"default":45}),
            ),
            (
                "max_step",
                json!({"type":"number","minimum":0,"maximum":100,"default":0.3}),
            ),
        ],
        &[],
    )
}
fn query_settings() -> Value {
    schema::object(
        vec![
            (
                "max_distance",
                json!({"type":"number","minimum":0,"maximum":1000,"default":2}),
            ),
            (
                "max_polygons",
                schema::integer("Maximum nearest-polygon scan.", 1, 65536, Some(65536)),
            ),
            (
                "max_expansions",
                schema::integer("Maximum A* polygon expansions.", 0, 65536, Some(16384)),
            ),
            (
                "max_points",
                schema::integer(
                    "Maximum intermediate/returned path points.",
                    2,
                    65536,
                    Some(4096),
                ),
            ),
        ],
        &[],
    )
}
fn decode<T: serde::de::DeserializeOwned>(value: Value, field: &str) -> Result<T, ToolFailure> {
    serde_json::from_value(value).map_err(|_| schema::invalid(field, "valid navigation data"))
}
fn bake(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let meshes: Vec<String> = decode(args.get("meshes").cloned().unwrap_or(Value::Null), "meshes")?;
    let settings: BakeSettings = decode(
        args.get("settings").cloned().unwrap_or(json!({})),
        "settings",
    )?;
    let obstacles: Vec<Obstacle> = decode(
        args.get("obstacles").cloned().unwrap_or(json!([])),
        "obstacles",
    )?;
    let output = Args(args).string("output")?;
    check_new_json_target(cx, output)?;
    let document = engine_project::simulation::navigation_from_meshes(
        &cx.project()?,
        &meshes,
        settings,
        obstacles,
    )
    .map_err(ToolFailure::from)?;
    let polygons = document.bake().map_err(ToolFailure::from)?.polygon_count();
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(schema::invalid(
            "meshes",
            "a navigation document below 16 MiB",
        ));
    }
    publish_new_file(cx, output, &bytes, "navigation.publish")?;
    Ok(ToolResult::new(
        json!({"path":output,"polygons":polygons,"hash":paths::content_hash(&bytes),"asset":{"id":"choose-a-unique-navigation-id","kind":"navigation","path":output}}),
    ))
}
/// Rejects an `output` that exists, needs new directories or is not `.json`.
pub(super) fn check_new_json_target(cx: &ToolContext<'_>, output: &str) -> Result<(), ToolFailure> {
    let target = paths::resolve_write_target(cx.root, output, "output")?;
    if target.exists
        || target.needs_directories
        || target.path.extension().and_then(|s| s.to_str()) != Some("json")
    {
        return Err(schema::invalid(
            "output",
            "a new .json file in an existing project directory",
        ));
    }
    Ok(())
}
/// Atomically creates `output` with `bytes`; never replaces an existing file.
/// Confinement is rechecked after any expensive work and again before the rename.
pub(super) fn publish_new_file(
    cx: &ToolContext<'_>,
    output: &str,
    bytes: &[u8],
    error_code: &str,
) -> Result<(), ToolFailure> {
    // Recheck confinement after potentially expensive work; create_new never clobbers.
    let target = paths::resolve_write_target(cx.root, output, "output")?;
    let parent = target
        .path
        .parent()
        .ok_or_else(|| schema::invalid("output", "a project file"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    let target = paths::resolve_write_target(cx.root, output, "output")?;
    // Match the project's bounded Windows rename retry: antivirus/indexers can
    // briefly retain a handle. Every attempt still refuses to replace a file.
    let mut attempt = 0u64;
    loop {
        match file.persist_noclobber(&target.path) {
            Ok(_) => return Ok(()),
            Err(error)
                if error.error.kind() == std::io::ErrorKind::PermissionDenied && attempt < 8 =>
            {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(5 * attempt));
                file = error.file;
            }
            Err(error) => {
                return Err(ToolFailure::from(engine_project::Error::new(
                    error_code,
                    error.error.to_string(),
                )));
            }
        }
    }
}
fn query(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let path = args
        .get("path")
        .ok_or_else(|| schema::invalid("path", "a navigation document"))?;
    cx.existing_file("path", path)?;
    let start: [f32; 3] = decode(args.get("start").cloned().unwrap_or(Value::Null), "start")?;
    let goal: [f32; 3] = decode(args.get("goal").cloned().unwrap_or(Value::Null), "goal")?;
    let query: QuerySettings = decode(args.get("query").cloned().unwrap_or(json!({})), "query")?;
    let mesh = engine_project::simulation::load_navmesh_file(
        cx.root,
        Path::new(Args(args).string("path")?),
    )
    .map_err(ToolFailure::from)?;
    let result = mesh.find_path(start, goal, query).map_err(|e| {
        ToolFailure::from(engine_project::Error::new(
            "navigation.query",
            e.to_string(),
        ))
    })?;
    Ok(ToolResult::new(super::clean(&result)))
}
