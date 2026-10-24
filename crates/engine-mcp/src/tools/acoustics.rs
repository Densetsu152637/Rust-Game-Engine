//! Acoustic world inspection, queries and baking. The logic lives in
//! `engine_project::acoustics`; these tools are thin wrappers that never execute
//! game code and always trace on the deterministic CPU tier.
use super::{Spec, navigation, scene, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::acoustics::{
    self, BakeInclude, MAX_BAKED_DOCUMENT_BYTES, MAX_QUERY_REFLECTIONS, MAX_QUERY_REVERB_RAYS,
    MAX_QUERY_SOURCES, QueryRequest,
};
use serde_json::{Map, Value, json};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let scene_arg = || {
        schema::string(
            "Project-relative scene path (see scene_list). Defaults to the startup scene.",
        )
    };
    vec![
        tool(
            Spec {
                name: "acoustics_inspect",
                title: "Inspect acoustic world",
                description: "Describe the acoustic world a scene builds from its meshes, 2D colliders and engine.Acoustic* components: world statistics and bounds, resolved materials, box rooms with Sabine and Eyring RT60, the 2D proxy summary, the contributing entities and findings such as ignored components or a closed room that leaks. Does not execute game code and does not start audio.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "include_entities",
                            schema::boolean(
                                "List up to 256 entities that contribute geometry.",
                                true,
                            ),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    ("scene", json!({"type":"string","description":"Scene ID."})),
                    (
                        "environment",
                        json!({"type":["object","null"],"description":"Summary of engine.AcousticEnvironment; null when the scene has no enabled one."}),
                    ),
                    (
                        "world",
                        json!({"type":"object","description":"Triangles, instances, materials, BVH nodes and bounds."}),
                    ),
                    (
                        "materials",
                        json!({"type":"array","items":{"type":"object"}}),
                    ),
                    ("rooms", json!({"type":"array","items":{"type":"object"}})),
                    ("proxy", json!({"type":"object"})),
                    (
                        "contributors",
                        json!({"type":"array","items":{"type":"object"}}),
                    ),
                    (
                        "warnings",
                        json!({"type":"array","items":{"type":"string"}}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            inspect,
        ),
        tool(
            Spec {
                name: "acoustics_query",
                title: "Query acoustic propagation",
                description: "Trace sound from up to 16 point sources to a listener in a scene on the CPU tier and return, per source, the visible fraction, direct gain in dB per band (125, 500, 2000 and 8000 Hz, occlusion only, excluding the distance law), distance and up to 16 early reflection paths, plus the late reverb (RT60 per band, level, mean free path, escaped fraction) and Sabine/Eyring estimates when the listener is inside a room volume. Positions are in scene units (a 2D proxy maps them). Deterministic: identical calls return identical results. Does not execute game code.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "listener",
                            described(
                                schema::object(
                                    vec![
                                        ("position", vector("Listener position [x, y, z].")),
                                        (
                                            "forward",
                                            vector(
                                                "Listener forward direction. Defaults to [0, 0, -1].",
                                            ),
                                        ),
                                        (
                                            "up",
                                            vector("Listener up direction. Defaults to [0, 1, 0]."),
                                        ),
                                    ],
                                    &["position"],
                                ),
                                "The listener pose in scene units.",
                            ),
                        ),
                        (
                            "sources",
                            json!({"type":"array","minItems":1,"maxItems":MAX_QUERY_SOURCES,
                            "description":"Point sources to trace to the listener.",
                            "items":schema::object(
                                vec![
                                    ("position", vector("Source position [x, y, z].")),
                                    ("radius_m", json!({"type":"number","minimum":0,"maximum":100,"description":"Source radius in metres for occlusion sampling. Default 0.25."})),
                                ],
                                &["position"],
                            )}),
                        ),
                        (
                            "quality",
                            described(
                                schema::object(
                                    vec![
                                        (
                                            "occlusion_rays",
                                            schema::integer(
                                                "Occlusion rays per source.",
                                                0,
                                                4096,
                                                None,
                                            ),
                                        ),
                                        (
                                            "reflection_order",
                                            schema::integer(
                                                "Image-source reflection order.",
                                                0,
                                                3,
                                                None,
                                            ),
                                        ),
                                        (
                                            "max_reflections",
                                            schema::integer(
                                                "Reflection paths kept per source.",
                                                0,
                                                16,
                                                None,
                                            ),
                                        ),
                                        (
                                            "er_rays",
                                            schema::integer(
                                                "Rays used to find reflecting planes.",
                                                0,
                                                4096,
                                                None,
                                            ),
                                        ),
                                        (
                                            "reverb_rays",
                                            schema::integer(
                                                "Late-field rays.",
                                                1,
                                                i64::from(MAX_QUERY_REVERB_RAYS),
                                                None,
                                            ),
                                        ),
                                        (
                                            "max_bounces",
                                            schema::integer(
                                                "Bounces per late-field ray.",
                                                1,
                                                256,
                                                None,
                                            ),
                                        ),
                                    ],
                                    &[],
                                ),
                                "Optional overrides of the environment's tracing quality.",
                            ),
                        ),
                    ],
                    &["listener", "sources"],
                ),
                output: schema::output(vec![
                    ("scene", json!({"type":"string","description":"Scene ID."})),
                    (
                        "backend",
                        json!({"type":"string","description":"Always cpu."}),
                    ),
                    (
                        "sources",
                        json!({"type":"array","description":format!("Per source, in request order; at most {MAX_QUERY_REFLECTIONS} reflections each."),"items":{"type":"object"}}),
                    ),
                    ("reverb", json!({"type":"object"})),
                    ("room_estimate", json!({"type":"object"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            query,
        ),
        tool(
            Spec {
                name: "acoustics_bake",
                title: "Bake acoustic world",
                description: "Write the scene's acoustic source geometry (meshes, instances, volumes and 2D proxy footprints, with resolved materials) to a new AcousticWorldDocument JSON file, sealed with a content hash and checked by replaying it through the world builder under the default limits. Existing files are never overwritten. Returns the `acoustics` asset entry to add to project.json; the runtime builds its world from live geometry, and the document is used by tools and native code. Does not execute Rust.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "output",
                            schema::string(
                                "New project-relative .json file in an existing directory.",
                            ),
                        ),
                        (
                            "include",
                            described(
                                schema::object(
                                    vec![
                                        (
                                            "meshes",
                                            schema::boolean("Include mesh instances.", true),
                                        ),
                                        (
                                            "proxy2d",
                                            schema::boolean(
                                                "Include the 2D proxy footprints and settings.",
                                                true,
                                            ),
                                        ),
                                        (
                                            "volumes",
                                            schema::boolean(
                                                "Include engine.AcousticVolume solids and rooms.",
                                                true,
                                            ),
                                        ),
                                    ],
                                    &[],
                                ),
                                "Which geometry families to keep; all default to true.",
                            ),
                        ),
                    ],
                    &["output"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("triangles", json!({"type":"integer"})),
                    (
                        "hash",
                        json!({"type":"string","description":"Content hash of the written file."}),
                    ),
                    (
                        "content_hash",
                        json!({"type":"string","description":"The document's own sealed hash."}),
                    ),
                    ("asset", json!({"type":"object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            bake,
        ),
    ]
}

fn described(mut schema: Value, description: &str) -> Value {
    schema["description"] = json!(description);
    schema
}
fn vector(description: &str) -> Value {
    json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number"},"description":description})
}

/// The scene argument, defaulting to the startup scene.
fn load(cx: &ToolContext<'_>, args: &Map<String, Value>) -> Result<scene::Loaded, ToolFailure> {
    if args.get("scene").is_some_and(|v| !v.is_null()) {
        return scene::load(cx, args);
    }
    let project = cx.project()?;
    let mut with_scene = args.clone();
    with_scene.insert("scene".into(), json!(super::slash(&project.startup_scene)));
    scene::load(cx, &with_scene)
}

fn failure(error: engine_project::Error) -> ToolFailure {
    ToolFailure::from(error)
}

fn inspect(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let include_entities = Args(args).boolean("include_entities", true)?;
    let loaded = load(cx, args)?;
    let value = acoustics::inspect_scene(&loaded.project, &loaded.scene, include_entities)
        .map_err(failure)?;
    Ok(ToolResult::new(value))
}

fn query(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let request: QueryRequest = serde_json::from_value(json!({
        "listener": args.get("listener").cloned().unwrap_or(Value::Null),
        "sources": args.get("sources").cloned().unwrap_or(Value::Null),
        "quality": args.get("quality").cloned().unwrap_or(json!({})),
    }))
    .map_err(|e| {
        schema::invalid(
            "listener/sources/quality",
            &format!("valid query data ({e})"),
        )
    })?;
    let loaded = load(cx, args)?;
    let value =
        acoustics::query_scene(&loaded.project, &loaded.scene, &request).map_err(failure)?;
    Ok(ToolResult::new(value))
}

fn bake(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let output = Args(args).string("output")?;
    let include = match Args(args).optional_object("include")? {
        Some(map) => {
            let flag = |name: &str| Args(map).boolean(name, true);
            BakeInclude {
                meshes: flag("meshes")?,
                proxy2d: flag("proxy2d")?,
                volumes: flag("volumes")?,
            }
        }
        None => BakeInclude::default(),
    };
    navigation::check_new_json_target(cx, output)?;
    let loaded = load(cx, args)?;
    let (document, triangles) =
        acoustics::bake_scene(&loaded.project, &loaded.scene, include).map_err(failure)?;
    let bytes = serde_json::to_vec(&document)
        .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    if bytes.len() > MAX_BAKED_DOCUMENT_BYTES {
        return Err(schema::invalid(
            "scene",
            "an acoustic world document below 64 MiB",
        ));
    }
    navigation::publish_new_file(cx, output, &bytes, "acoustics.publish")?;
    Ok(ToolResult::new(json!({
        "path": output,
        "triangles": triangles,
        "hash": paths::content_hash(&bytes),
        "content_hash": document.content_hash,
        "asset": {"id": "choose-a-unique-acoustics-id", "kind": "acoustics", "path": output},
    })))
}
