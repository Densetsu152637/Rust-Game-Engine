//! Headless physics queries and debug-view summaries. The logic lives in
//! `engine_project::simulation::physics` (`query_scene`, `debug_scene`); these tools are
//! thin wrappers that load a scene, advance it a bounded number of fixed ticks and never
//! write files. Native scripts are not registered, so no game code runs.
use super::{Spec, scene, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::simulation::physics::{
    self, DebugViewOptions, MAX_QUERY_LIMIT, MAX_QUERY_TICKS, QueryRequest,
};
use serde_json::{Map, Value, json};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "physics_query",
                title: "Query physics scene",
                description: "Load a project scene, start its physics simulation headless, optionally advance `ticks` fixed ticks, then run one spatial query against the simulated bodies: `ray` (closest hit, or every hit with all=true), `shape_cast` (sweep a ball, box, capsule, cylinder or convex hull along a direction; returns distance of impact, contact point and normal), `overlap` (every collider intersecting a shape) or `point` (nearest collider to a point, or with contains=true every collider containing it). 3D uses XYZ and quaternions [x,y,z,w]; 2D (`dimension` \"2d\") uses XY and a rotation angle in radians. Filters select named layers or a raw mask, exclude entities by authored id, include/exclude sensors and restrict body kinds. Hits are sorted by distance then entity and carry authored entity ids. Does not execute game code and never writes files.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "kind",
                            schema::string_enum(
                                "Query type.",
                                &["ray", "shape_cast", "overlap", "point"],
                            ),
                        ),
                        (
                            "dimension",
                            schema::string_enum(
                                "Physics world to query; default 3d.",
                                &["2d", "3d"],
                            ),
                        ),
                        (
                            "ticks",
                            schema::integer(
                                "Fixed ticks to simulate before querying.",
                                0,
                                i64::from(MAX_QUERY_TICKS),
                                Some(0),
                            ),
                        ),
                        (
                            "dt",
                            json!({"type":"number","minimum":0.0001,"maximum":0.1,"default":0.016666668,"description":"Seconds per simulated tick."}),
                        ),
                        ("origin", vector("Ray origin (3 numbers in 3D, 2 in 2D).")),
                        (
                            "direction",
                            vector(
                                "Ray or sweep direction; normalized, so distances are world units.",
                            ),
                        ),
                        ("position", vector("Query shape position.")),
                        ("point", vector("Point to project or test.")),
                        (
                            "rotation",
                            json!({"type":"array","minItems":1,"maxItems":4,"items":{"type":"number"},"description":"Query shape rotation: quaternion [x,y,z,w] (3D) or a one-element angle in radians [angle] (2D)."}),
                        ),
                        (
                            "max_distance",
                            json!({"type":"number","minimum":0,"maximum":100000,"default":1000,"description":"Longest ray or sweep distance."}),
                        ),
                        (
                            "all",
                            schema::boolean(
                                "Ray only: return every hit instead of the closest.",
                                false,
                            ),
                        ),
                        (
                            "contains",
                            schema::boolean(
                                "Point only: list every collider containing the point.",
                                false,
                            ),
                        ),
                        ("shape", shape_schema()),
                        (
                            "limit",
                            schema::integer(
                                "Most hits for multi-hit queries; the nearest are kept.",
                                1,
                                MAX_QUERY_LIMIT as i64,
                                Some(64),
                            ),
                        ),
                        ("filter", filter_schema()),
                    ],
                    &["kind"],
                ),
                output: schema::output(vec![
                    ("dimension", json!({"type":"string"})),
                    ("ticks", json!({"type":"integer"})),
                    ("count", json!({"type":"integer"})),
                    (
                        "truncated",
                        json!({"type":"boolean","description":"More hits existed than `limit`."}),
                    ),
                    (
                        "hits",
                        json!({"type":"array","items":{"type":"object","description":"entity (authored id or null), entity_bits, distance, plus point/normal (ray, shape_cast, point), shape_point/penetrating (shape_cast) or inside (point)."}}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            query,
        ),
        tool(
            Spec {
                name: "physics_debug",
                title: "Summarize physics debug view",
                description: "Load a project scene, start its physics simulation headless, optionally advance `ticks` fixed ticks, and return a bounded summary of the physics debug view: uncapped totals of colliders, contact points and joint anchors, a count of colliders by state colour key (fixed, dynamic, kinematic, sleeping, sensor), the size of the full world-space line list a renderer would draw, and up to `max_items` colliders (entity, state, shape, pose), contacts (points, normals, depth) and joint anchors. `truncated` reports whether any list was cut. Works whether or not engine.PhysicsSettings.debug is set. Does not execute game code and never writes files.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "ticks",
                            schema::integer(
                                "Fixed ticks to simulate before summarizing.",
                                0,
                                i64::from(MAX_QUERY_TICKS),
                                Some(0),
                            ),
                        ),
                        (
                            "dt",
                            json!({"type":"number","minimum":0.0001,"maximum":0.1,"default":0.016666668,"description":"Seconds per simulated tick."}),
                        ),
                        (
                            "max_items",
                            schema::integer(
                                "Cap on listed colliders, contacts and anchors.",
                                1,
                                1024,
                                Some(64),
                            ),
                        ),
                        (
                            "aabbs",
                            schema::boolean("Include collider AABBs in the line count.", false),
                        ),
                        ("contacts", schema::boolean("Include contact points.", true)),
                        ("joints", schema::boolean("Include joint anchors.", true)),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    ("ticks", json!({"type":"integer"})),
                    (
                        "totals",
                        json!({"type":"object","description":"Uncapped colliders, contacts and anchors."}),
                    ),
                    (
                        "states",
                        json!({"type":"object","description":"Collider count per state key."}),
                    ),
                    (
                        "line_segments",
                        json!({"type":"integer","description":"Segments in the full wireframe."}),
                    ),
                    ("truncated", json!({"type":"boolean"})),
                    ("max_items", json!({"type":"integer"})),
                    (
                        "colliders",
                        json!({"type":"array","items":{"type":"object"}}),
                    ),
                    (
                        "contacts",
                        json!({"type":"array","items":{"type":"object"}}),
                    ),
                    ("anchors", json!({"type":"array","items":{"type":"object"}})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            debug,
        ),
    ]
}

fn scene_arg() -> Value {
    schema::string("Project-relative scene path (see scene_list). Defaults to the startup scene.")
}
fn vector(description: &str) -> Value {
    json!({"type":"array","minItems":2,"maxItems":3,"items":{"type":"number"},"description":description})
}
fn shape_schema() -> Value {
    json!({"type":"object","description":"Query shape for shape_cast and overlap: {type:\"ball\",radius}, {type:\"box\",half_extents:[..]}, {type:\"capsule\",half_height,radius} (Y axis), {type:\"cylinder\",half_height,radius} (3D, Y axis) or {type:\"convex_hull\",points:[[..],..]}.",
        "properties":{"type":{"type":"string","enum":["ball","box","capsule","cylinder","convex_hull"]},
            "radius":{"type":"number"},"half_extents":{"type":"array","items":{"type":"number"}},
            "half_height":{"type":"number"},
            "points":{"type":"array","maxItems":4096,"items":{"type":"array","items":{"type":"number"}}}},
        "required":["type"],"additionalProperties":false})
}
fn filter_schema() -> Value {
    json!({"type":"object","description":"Which colliders may be reported.","additionalProperties":false,"properties":{
        "layers":{"type":"array","maxItems":32,"items":{"type":"string"},"description":"Named engine.PhysicsSettings layers to include."},
        "mask":{"type":"integer","minimum":0,"maximum":4294967295u64,"description":"Raw layer membership mask; combined with layers by OR."},
        "exclude":{"type":"array","maxItems":1024,"items":{"type":"string"},"description":"Authored entity ids never reported."},
        "sensors":{"type":"string","enum":["include","exclude","only"]},
        "kinds":{"type":"array","maxItems":3,"items":{"type":"string","enum":["fixed","dynamic","kinematic"]}}}})
}

/// The scene argument, defaulting to the startup scene, as a project whose active scene it is.
fn project_for(
    cx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<engine_project::Project, ToolFailure> {
    let mut with_scene = args.clone();
    if args.get("scene").is_none_or(Value::is_null) {
        let project = cx.project()?;
        with_scene.insert("scene".into(), json!(super::slash(&project.startup_scene)));
    }
    let loaded = scene::load(cx, &with_scene)?;
    let mut project = loaded.project;
    project.scene = loaded.scene;
    Ok(project)
}
fn ticks_and_dt(args: &Map<String, Value>) -> Result<(u32, f32), ToolFailure> {
    let ticks = Args(args).integer("ticks", 0, u64::from(MAX_QUERY_TICKS), 0)? as u32;
    let dt = match Args(args).raw("dt").filter(|v| !v.is_null()) {
        None => 1.0 / 60.0,
        Some(value) => value
            .as_f64()
            .filter(|dt| (0.0001..=0.1).contains(dt))
            .ok_or_else(|| schema::invalid("dt", "a number from 0.0001 to 0.1"))?
            as f32,
    };
    Ok((ticks, dt))
}
fn failure(error: engine_project::Error) -> ToolFailure {
    ToolFailure::from(error)
}

fn query(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let kind = Args(args)
        .one_of("kind", &["ray", "shape_cast", "overlap", "point"])?
        .ok_or_else(|| schema::invalid("kind", "ray, shape_cast, overlap or point"))?;
    let (ticks, dt) = ticks_and_dt(args)?;
    let mut request = Map::new();
    request.insert("kind".into(), json!(kind));
    for key in [
        "dimension",
        "origin",
        "direction",
        "position",
        "point",
        "rotation",
        "max_distance",
        "all",
        "contains",
        "limit",
        "filter",
    ] {
        if let Some(value) = args.get(key).filter(|v| !v.is_null()) {
            request.insert(key.into(), value.clone());
        }
    }
    // A one-element array is a 2D angle; the request type takes a bare number.
    if let Some(Value::Array(angle)) = request.get("rotation")
        && let [single] = angle.as_slice()
    {
        let single = single.clone();
        request.insert("rotation".into(), single);
    }
    // The shape object is flattened into the request, its `type` becoming `shape`.
    if let Some(shape) = Args(args).optional_object("shape")? {
        for (key, value) in shape {
            request.insert(
                if key == "type" { "shape" } else { key }.into(),
                value.clone(),
            );
        }
    }
    // The request type rejects unknown fields per kind, so drop members of other kinds.
    let wanted: &[&str] = match kind {
        "ray" => &["origin", "direction", "max_distance", "all"],
        "shape_cast" => &["position", "rotation", "direction", "max_distance"],
        "overlap" => &["position", "rotation"],
        _ => &["point", "contains"],
    };
    let shaped = matches!(kind, "shape_cast" | "overlap");
    request.retain(|key, _| {
        matches!(key.as_str(), "kind" | "dimension" | "filter" | "limit")
            || wanted.contains(&key.as_str())
            || shaped
                && matches!(
                    key.as_str(),
                    "shape" | "radius" | "half_extents" | "half_height" | "points"
                )
    });
    let request: QueryRequest = serde_json::from_value(Value::Object(request))
        .map_err(|e| schema::invalid("query", &format!("a valid {kind} query ({e})")))?;
    let value =
        physics::query_scene(project_for(cx, args)?, ticks, dt, &request).map_err(failure)?;
    Ok(ToolResult::new(value))
}

fn debug(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let (ticks, dt) = ticks_and_dt(args)?;
    let options = DebugViewOptions {
        max_items: Args(args).integer("max_items", 1, 1024, 64)? as usize,
        aabbs: Args(args).boolean("aabbs", false)?,
        contacts: Args(args).boolean("contacts", true)?,
        joints: Args(args).boolean("joints", true)?,
    };
    let value =
        physics::debug_scene(project_for(cx, args)?, ticks, dt, &options).map_err(failure)?;
    Ok(ToolResult::new(value))
}
