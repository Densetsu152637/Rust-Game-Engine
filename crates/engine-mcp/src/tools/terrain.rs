//! Terrain authoring and queries over binary `terrain` assets (the validated `engine-terrain`
//! `.terrain` format); never executes game code. Edits are all-or-nothing and rewrite the file
//! atomically; new files never replace existing ones.
use super::navigation::{check_new_json_target, publish_new_file};
use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::Project;
use engine_project::simulation::BakeSettings;
use engine_terrain::{
    Brush, EditReport, Heightfield, NavInputOptions, SampleRect, SculptOp, TerrainLayer, asset,
    noise::fbm,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

const MAX_OPS: usize = 256;
const MAX_SAMPLES_QUERIED: usize = 256;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "terrain_create",
                title: "Create terrain",
                description: "Write a new binary .terrain heightmap asset: `columns` x `rows` samples (2..=4097 each, at most 4198401 in total) `spacing` apart, flat at `height` or seeded fBm `noise`, with 1..=8 splat layers (the first starts at full weight). Sides that are multiples of a power of two (for example 257 or 513 samples) allow more LOD levels. Existing files are never overwritten. Returns the asset entry (kind terrain) and an `engine.Terrain` component snippet; does not execute Rust.",
                input: schema::object(
                    vec![
                        (
                            "output",
                            schema::string(
                                "New project-relative .terrain file in an existing directory.",
                            ),
                        ),
                        (
                            "columns",
                            schema::integer("Samples along X.", 2, 4097, None),
                        ),
                        ("rows", schema::integer("Samples along Z.", 2, 4097, None)),
                        (
                            "spacing",
                            json!({"type":"number","minimum":0.001,"maximum":1000,"default":1.0,"description":"Distance between samples in local units."}),
                        ),
                        (
                            "height",
                            json!({"type":"number","minimum":-100000,"maximum":100000,"default":0.0,"description":"Base height."}),
                        ),
                        (
                            "noise",
                            json!({"type":"object","additionalProperties":false,"description":"Optional fBm added to the base height.",
                                "properties":{"seed":{"type":"integer","minimum":0,"maximum":4294967295u64},
                                    "amplitude":{"type":"number","minimum":0,"maximum":10000},
                                    "frequency":{"type":"number","minimum":0.0001,"maximum":100,"description":"Cycles per local unit."}},
                                "required":["amplitude","frequency"]}),
                        ),
                        (
                            "layers",
                            schema::array_of(
                                "Splat layers {name, texture?, tint?:[r,g,b,a] 0..1, uv_tiling?}. Default one layer named base.",
                                json!({"type":"object"}),
                                8,
                            ),
                        ),
                    ],
                    &["output", "columns", "rows"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("samples", json!({"type":"array"})),
                    ("bytes", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                    ("asset", json!({"type":"object"})),
                    ("component", json!({"type":"object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            create,
        ),
        tool(
            Spec {
                name: "terrain_sculpt",
                title: "Sculpt terrain",
                description: "Apply up to 256 ordered sculpt brushes to an existing terrain asset, all or nothing: `raise`/`lower` (strength in local units), `smooth` (3x3 mean, strength 0..1), `flatten` (toward `height` or the height under the centre, strength 0..1), `set_height` (toward `height`, ignoring strength) and `noise` (seeded fBm of `frequency`, strength in local units). Brushes are circles in terrain-local [x, z] with `radius`, `strength` and `falloff` (0..1, smoothstep). Deterministic. Rewrites the file atomically; returns the changed samples and their bounding region.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Terrain asset id or project-relative .terrain file."),
                        ),
                        (
                            "expected_hash",
                            schema::string("Optional content hash the file must still have."),
                        ),
                        (
                            "ops",
                            schema::array_of(
                                "Brushes: {op: raise|lower|smooth|flatten|set_height|noise, center:[x,z], radius, strength?, falloff?, height?, seed?, frequency?}.",
                                json!({"type":"object"}),
                                MAX_OPS,
                            ),
                        ),
                    ],
                    &["path", "ops"],
                ),
                output: edit_output(),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            sculpt,
        ),
        tool(
            Spec {
                name: "terrain_paint",
                title: "Paint terrain layers",
                description: "Apply up to 256 ordered splat edits to an existing terrain asset, all or nothing: `paint` moves the named `layer`'s weight toward 1 by strength x brush weight while the other layers shrink proportionally (weights stay normalized, 0..255 summing to 255 per sample); `add_layer` appends a layer {name, texture?, tint?, uv_tiling?} at zero weight (at most 8 layers); `set_layer` replaces a layer's texture, tint or tiling. Rewrites the file atomically.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Terrain asset id or project-relative .terrain file."),
                        ),
                        (
                            "expected_hash",
                            schema::string("Optional content hash the file must still have."),
                        ),
                        (
                            "ops",
                            schema::array_of(
                                "Edits: {op: paint, layer, center:[x,z], radius, strength?, falloff?} | {op: add_layer|set_layer, name, texture?, tint?, uv_tiling?}.",
                                json!({"type":"object"}),
                                MAX_OPS,
                            ),
                        ),
                    ],
                    &["path", "ops"],
                ),
                output: edit_output(),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            paint,
        ),
        tool(
            Spec {
                name: "terrain_holes",
                title: "Paint terrain holes",
                description: "Apply up to 256 ordered hole edits to an existing terrain asset, all or nothing: every cell whose centre lies within `radius` of `center` (terrain-local [x, z]) becomes a hole (`hole: true`, default) or is filled. Holes are cut from rendering, physics (those collider tiles become triangle meshes) and navigation input. Rewrites the file atomically; the region is in cells.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Terrain asset id or project-relative .terrain file."),
                        ),
                        (
                            "expected_hash",
                            schema::string("Optional content hash the file must still have."),
                        ),
                        (
                            "ops",
                            schema::array_of(
                                "Edits: {center:[x,z], radius, hole?: bool}.",
                                json!({"type":"object"}),
                                MAX_OPS,
                            ),
                        ),
                    ],
                    &["path", "ops"],
                ),
                output: edit_output(),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            holes,
        ),
        tool(
            Spec {
                name: "terrain_navmesh",
                title: "Bake terrain navigation",
                description: "Bake a terrain asset placed at `origin` (the terrain entity's world translation) into a new navigation JSON document (kind navigation, the format navigation_bake writes and navigation_query reads). The input is a walkable raster-aligned soup: one planar quad per bake cell, holes and cells steeper than settings.max_slope_degrees left out; at most 16384 cells, so use `region` (world [[min_x, min_z], [max_x, max_z]]) or a larger settings.cell_size for big terrains. Existing files are never overwritten; does not execute Rust.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Terrain asset id or project-relative .terrain file."),
                        ),
                        (
                            "output",
                            schema::string(
                                "New project-relative .json file in an existing directory.",
                            ),
                        ),
                        (
                            "origin",
                            json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number","minimum":-100000,"maximum":100000},"description":"World position of the terrain's local origin."}),
                        ),
                        (
                            "region",
                            json!({"type":"array","minItems":2,"maxItems":2,"items":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}},"description":"Optional world-space [[min_x, min_z], [max_x, max_z]]."}),
                        ),
                        (
                            "settings",
                            json!({"type":"object","description":"Bake settings: cell_size, agent_radius, agent_height, max_slope_degrees, max_step (navigation_bake defaults)."}),
                        ),
                    ],
                    &["path", "output"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("cells", json!({"type":"integer"})),
                    ("hole_cells", json!({"type":"integer"})),
                    ("steep_cells", json!({"type":"integer"})),
                    ("polygons", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                    ("asset", json!({"type":"object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            navmesh,
        ),
        tool(
            Spec {
                name: "terrain_info",
                title: "Terrain info",
                description: "Summarize a terrain asset (samples, spacing, extent, height range, layers, hole count, content hash) and optionally sample up to 256 terrain-local [x, z] points: triangulated height (the rendered and physics surface), face normal, slope in degrees, hole flag and per-layer weights. Read-only.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Terrain asset id or project-relative .terrain file."),
                        ),
                        (
                            "samples",
                            schema::array_of(
                                "Terrain-local points [x, z] to sample.",
                                json!({"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}}),
                                MAX_SAMPLES_QUERIED,
                            ),
                        ),
                    ],
                    &["path"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("columns", json!({"type":"integer"})),
                    ("rows", json!({"type":"integer"})),
                    ("spacing", json!({"type":"number"})),
                    ("extent", json!({"type":"array"})),
                    ("height_range", json!({"type":"array"})),
                    ("layers", json!({"type":"array"})),
                    ("holes", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                    ("samples", json!({"type":"array"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            info,
        ),
    ]
}

fn edit_output() -> Value {
    schema::output(vec![
        ("path", json!({"type":"string"})),
        ("changed", json!({"type":"integer"})),
        (
            "region",
            json!({"type":["object","null"],"description":"{min:[x,z], max:[x,z]} exclusive max, sample (or cell) indices."}),
        ),
        ("hash", json!({"type":"string"})),
    ])
}

fn failure(error: engine_terrain::Error) -> ToolFailure {
    ToolFailure::new(error.code, error.message)
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value, field: &str) -> Result<T, ToolFailure> {
    serde_json::from_value(value.clone())
        .map_err(|e| schema::invalid(field, &format!("valid terrain data ({e})")))
}

/// Resolves an asset id or a project-relative `.terrain` file to an existing file.
fn target(cx: &ToolContext<'_>, project: &Project, value: &str) -> Result<PathBuf, ToolFailure> {
    let path = if project.assets.contains_key(value) {
        project.require_asset(value, "terrain")?;
        project.resolve_asset(value)?
    } else {
        cx.existing_file("path", &json!(value))?
    };
    if path.extension().and_then(|s| s.to_str()) != Some("terrain") {
        return Err(schema::invalid(
            "path",
            "a terrain asset id or .terrain file",
        ));
    }
    Ok(path)
}
fn load(path: &Path) -> Result<(Heightfield, Vec<u8>), ToolFailure> {
    let bytes = paths::read_bounded(path, asset::MAX_FILE_BYTES)
        .map_err(|_| ToolFailure::new("file_too_large", "terrain file exceeds 64 MiB"))?;
    let field = asset::decode(&bytes).map_err(failure)?;
    Ok((field, bytes))
}
fn open(
    cx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<(PathBuf, Heightfield), ToolFailure> {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, a.string("path")?)?;
    let (field, bytes) = load(&file)?;
    if let Some(expected) = a.optional_string("expected_hash")?
        && expected != paths::content_hash(&bytes)
    {
        return Err(ToolFailure::new(
            "stale_hash",
            "the terrain file changed since expected_hash was read",
        )
        .with_hint("Re-read it with terrain_info and retry."));
    }
    Ok((file, field))
}
fn ops(args: &Map<String, Value>) -> Result<Vec<Value>, ToolFailure> {
    let ops: Vec<Value> = decode(args.get("ops").unwrap_or(&Value::Null), "ops")?;
    if ops.is_empty() || ops.len() > MAX_OPS {
        return Err(schema::invalid("ops", "1 to 256 edits"));
    }
    Ok(ops)
}
fn finish(
    cx: &ToolContext<'_>,
    file: &Path,
    field: &Heightfield,
    report: EditReport,
) -> ToolOutcome {
    let bytes = asset::encode(field);
    if report.changed > 0 {
        paths::atomic_write(file, &bytes)
            .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    }
    Ok(ToolResult::new(json!({
        "path": cx.relative(file),
        "changed": report.changed,
        "region": report.region.map(|r| json!({"min": r.min, "max": r.max})),
        "hash": paths::content_hash(&bytes),
    })))
}
fn indexed(index: usize, mut error: ToolFailure) -> ToolFailure {
    error.message = format!("ops[{index}]: {}", error.message);
    error
}

// ---- terrain_create ------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoiseArg {
    #[serde(default)]
    seed: u32,
    amplitude: f32,
    frequency: f32,
}

fn create(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let output = a.string("output")?;
    let checked = paths::resolve_write_target(cx.root, output, "output")?;
    if checked.exists
        || checked.needs_directories
        || checked.path.extension().and_then(|s| s.to_str()) != Some("terrain")
    {
        return Err(schema::invalid(
            "output",
            "a new .terrain file in an existing project directory",
        ));
    }
    let columns = a.integer("columns", 2, 4097, 0)? as u32;
    let rows = a.integer("rows", 2, 4097, 0)? as u32;
    let number = |name: &str, default: f32| -> Result<f32, ToolFailure> {
        match args.get(name).filter(|v| !v.is_null()) {
            None => Ok(default),
            Some(v) => v
                .as_f64()
                .map(|v| v as f32)
                .filter(|v| v.is_finite())
                .ok_or_else(|| schema::invalid(name, "a number")),
        }
    };
    let spacing = number("spacing", 1.0)?;
    let base = number("height", 0.0)?;
    let noise: Option<NoiseArg> = match args.get("noise").filter(|v| !v.is_null()) {
        Some(value) => Some(decode(value, "noise")?),
        None => None,
    };
    if let Some(n) = &noise
        && !(n.amplitude.is_finite()
            && (0.0..=10_000.0).contains(&n.amplitude)
            && n.frequency.is_finite()
            && n.frequency > 0.0
            && n.frequency <= 100.0)
    {
        return Err(schema::invalid(
            "noise",
            "amplitude 0..=10000 and frequency (0, 100]",
        ));
    }
    let mut field = Heightfield::new(columns, rows, spacing).map_err(failure)?;
    let heights = (0..rows)
        .flat_map(|r| (0..columns).map(move |c| (c, r)))
        .map(|(c, r)| {
            let mut h = base;
            if let Some(n) = &noise {
                let (x, z) = (c as f32 * spacing, r as f32 * spacing);
                h += n.amplitude * fbm(x * n.frequency, z * n.frequency, n.seed);
            }
            h
        })
        .collect();
    field.set_heights(heights).map_err(failure)?;
    if let Some(layers) = args.get("layers").filter(|v| !v.is_null()) {
        let layers: Vec<TerrainLayer> = decode(layers, "layers")?;
        if layers.is_empty() {
            return Err(schema::invalid("layers", "1 to 8 layers"));
        }
        let mut layers = layers.into_iter();
        field
            .set_layer(0, layers.next().expect("non-empty"))
            .map_err(failure)?;
        for layer in layers {
            field.add_layer(layer).map_err(failure)?;
        }
    }
    let bytes = asset::encode(&field);
    publish_new_file(cx, output, &bytes, "terrain.publish")?;
    Ok(ToolResult::new(json!({
        "path": output,
        "samples": [columns, rows],
        "bytes": bytes.len(),
        "hash": paths::content_hash(&bytes),
        "asset": {"id":"choose-a-unique-terrain-id","kind":"terrain","path":output},
        "component": {"engine.Terrain":{"version":1,"data":{"asset":"choose-a-unique-terrain-id"}}},
    })))
}

// ---- terrain_sculpt / terrain_paint / terrain_holes ---------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SculptArg {
    op: String,
    center: [f32; 2],
    radius: f32,
    strength: Option<f32>,
    falloff: Option<f32>,
    height: Option<f32>,
    seed: Option<u32>,
    frequency: Option<f32>,
}
fn brush(center: [f32; 2], radius: f32, strength: Option<f32>, falloff: Option<f32>) -> Brush {
    Brush {
        center,
        radius,
        strength: strength.unwrap_or(1.0),
        falloff: falloff.unwrap_or(0.5),
    }
}

fn sculpt(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let (file, mut field) = open(cx, args)?;
    let mut report = EditReport::default();
    for (index, op) in ops(args)?.iter().enumerate() {
        let arg: SculptArg = decode(op, "ops").map_err(|e| indexed(index, e))?;
        let need_height = || {
            arg.height
                .ok_or_else(|| indexed(index, schema::invalid("ops", "a height for set_height")))
        };
        let kind = match arg.op.as_str() {
            "raise" => SculptOp::Raise,
            "lower" => SculptOp::Lower,
            "smooth" => SculptOp::Smooth,
            "flatten" => SculptOp::Flatten { height: arg.height },
            "set_height" => SculptOp::SetHeight {
                height: need_height()?,
            },
            "noise" => SculptOp::Noise {
                seed: arg.seed.unwrap_or(0),
                frequency: arg.frequency.unwrap_or(0.1),
            },
            _ => {
                return Err(indexed(
                    index,
                    schema::invalid(
                        "ops",
                        "op raise, lower, smooth, flatten, set_height or noise",
                    ),
                ));
            }
        };
        let b = brush(arg.center, arg.radius, arg.strength, arg.falloff);
        let applied =
            engine_terrain::sculpt(&mut field, &b, kind).map_err(|e| indexed(index, failure(e)))?;
        report = report.merge(applied);
    }
    finish(cx, &file, &field, report)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaintArg {
    op: String,
    layer: Option<String>,
    center: Option<[f32; 2]>,
    radius: Option<f32>,
    strength: Option<f32>,
    falloff: Option<f32>,
    name: Option<String>,
    texture: Option<String>,
    tint: Option<[f32; 4]>,
    uv_tiling: Option<f32>,
}

fn paint(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let (file, mut field) = open(cx, args)?;
    let mut report = EditReport::default();
    let all = SampleRect::new([0, 0], [field.columns(), field.rows()]);
    for (index, op) in ops(args)?.iter().enumerate() {
        let arg: PaintArg = decode(op, "ops").map_err(|e| indexed(index, e))?;
        let missing = |what: &str| indexed(index, schema::invalid("ops", what));
        match arg.op.as_str() {
            "paint" => {
                let name = arg
                    .layer
                    .as_deref()
                    .ok_or_else(|| missing("a layer to paint"))?;
                let layer = field.layer_index(name).ok_or_else(|| {
                    indexed(
                        index,
                        ToolFailure::new("terrain.layer.unknown", format!("no layer {name}"))
                            .with_hint("Add it with an add_layer op first."),
                    )
                })?;
                let center = arg.center.ok_or_else(|| missing("a center [x, z]"))?;
                let radius = arg.radius.ok_or_else(|| missing("a radius"))?;
                let b = brush(center, radius, arg.strength, arg.falloff);
                let applied = engine_terrain::paint_layer(&mut field, &b, layer)
                    .map_err(|e| indexed(index, failure(e)))?;
                report = report.merge(applied);
            }
            "add_layer" | "set_layer" => {
                let name = arg.name.clone().ok_or_else(|| missing("a layer name"))?;
                let mut layer = field
                    .layer_index(&name)
                    .map(|i| field.layers()[i].clone())
                    .unwrap_or_else(|| TerrainLayer {
                        name: name.clone(),
                        ..TerrainLayer::default()
                    });
                if arg.texture.is_some() {
                    layer.texture = arg.texture.clone();
                }
                if let Some(tint) = arg.tint {
                    layer.tint = tint;
                }
                if let Some(tiling) = arg.uv_tiling {
                    layer.uv_tiling = tiling;
                }
                let result = if arg.op == "add_layer" {
                    field.add_layer(layer).map(|_| ())
                } else {
                    let i = field.layer_index(&name).ok_or_else(|| {
                        indexed(
                            index,
                            ToolFailure::new("terrain.layer.unknown", format!("no layer {name}")),
                        )
                    })?;
                    field.set_layer(i, layer)
                };
                result.map_err(|e| indexed(index, failure(e)))?;
                report = report.merge(EditReport {
                    region: Some(all),
                    changed: 1,
                });
            }
            _ => {
                return Err(indexed(
                    index,
                    schema::invalid("ops", "op paint, add_layer or set_layer"),
                ));
            }
        }
    }
    finish(cx, &file, &field, report)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HoleArg {
    center: [f32; 2],
    radius: f32,
    #[serde(default = "yes")]
    hole: bool,
}
fn yes() -> bool {
    true
}

fn holes(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let (file, mut field) = open(cx, args)?;
    let mut report = EditReport::default();
    for (index, op) in ops(args)?.iter().enumerate() {
        let arg: HoleArg = decode(op, "ops").map_err(|e| indexed(index, e))?;
        let applied =
            engine_terrain::paint_holes(&mut field, &Brush::new(arg.center, arg.radius), arg.hole)
                .map_err(|e| indexed(index, failure(e)))?;
        report = report.merge(applied);
    }
    finish(cx, &file, &field, report)
}

// ---- terrain_navmesh ------------------------------------------------------------------------

fn navmesh(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, a.string("path")?)?;
    let output = a.string("output")?;
    check_new_json_target(cx, output)?;
    let (field, _) = load(&file)?;
    let origin: [f32; 3] = match args.get("origin").filter(|v| !v.is_null()) {
        Some(v) => decode(v, "origin")?,
        None => [0.0; 3],
    };
    let region: Option<[[f32; 2]; 2]> = match args.get("region").filter(|v| !v.is_null()) {
        Some(v) => Some(decode(v, "region")?),
        None => None,
    };
    let settings: BakeSettings = decode(
        args.get("settings")
            .filter(|v| !v.is_null())
            .unwrap_or(&json!({})),
        "settings",
    )?;
    let options = NavInputOptions {
        origin,
        region,
        ..NavInputOptions::default()
    };
    let soup = engine_terrain::navigation_soup(
        &field,
        &NavInputOptions {
            cell_size: settings.cell_size,
            max_slope_degrees: settings.max_slope_degrees,
            ..options
        },
    )
    .map_err(failure)?;
    let document = engine_project::world::terrain::navigation_document(
        &field,
        &options,
        settings,
        Vec::new(),
    )?;
    let polygons = document.bake()?.polygon_count();
    let bytes = serde_json::to_vec(&document)
        .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(schema::invalid(
            "region",
            "a navigation document below 16 MiB",
        ));
    }
    publish_new_file(cx, output, &bytes, "terrain.navmesh.publish")?;
    Ok(ToolResult::new(json!({
        "path": output,
        "cells": soup.cells,
        "hole_cells": soup.hole_cells,
        "steep_cells": soup.steep_cells,
        "polygons": polygons,
        "hash": paths::content_hash(&bytes),
        "asset": {"id":"choose-a-unique-navigation-id","kind":"navigation","path":output},
    })))
}

// ---- terrain_info ---------------------------------------------------------------------------

fn info(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, a.string("path")?)?;
    let (field, bytes) = load(&file)?;
    let points: Vec<[f32; 2]> = match args.get("samples").filter(|v| !v.is_null()) {
        Some(v) => decode(v, "samples")?,
        None => Vec::new(),
    };
    if points.len() > MAX_SAMPLES_QUERIED || points.iter().flatten().any(|v| !v.is_finite()) {
        return Err(schema::invalid(
            "samples",
            "at most 256 finite [x, z] points",
        ));
    }
    let samples: Vec<Value> = points
        .iter()
        .map(|&[x, z]| {
            let weights: Map<String, Value> = field
                .layers()
                .iter()
                .enumerate()
                .map(|(i, l)| (l.name.clone(), json!(field.layer_weight_at(i, x, z))))
                .collect();
            json!({
                "point": [x, z],
                "inside": field.contains(x, z),
                "height": field.height_at(x, z),
                "normal": field.normal_at(x, z).to_array(),
                "slope_degrees": field.slope_degrees_at(x, z),
                "hole": field.is_hole_at(x, z),
                "weights": weights,
            })
        })
        .collect();
    let layers: Vec<Value> = field
        .layers()
        .iter()
        .map(|l| json!({"name":l.name,"texture":l.texture,"tint":l.tint,"uv_tiling":l.uv_tiling}))
        .collect();
    Ok(ToolResult::new(super::clean(&json!({
        "path": cx.relative(&file),
        "columns": field.columns(),
        "rows": field.rows(),
        "spacing": field.spacing(),
        "extent": field.extent(),
        "height_range": field.height_range(),
        "layers": layers,
        "holes": field.hole_count(),
        "hash": paths::content_hash(&bytes),
        "samples": samples,
    }))))
}
