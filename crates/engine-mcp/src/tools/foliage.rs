//! Foliage authoring and queries over binary `foliage` assets (the validated `engine-foliage`
//! `.foliage` format); never executes game code. Edits are all-or-nothing and rewrite the
//! file atomically; new files never replace existing ones.
use super::navigation::publish_new_file;
use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_foliage::{
    Brush, ExclusionZone, FoliageAsset, FoliageField, FoliageType, PaintContext, PaintOp,
    ScatterParams, asset, paint, scatter,
};
use engine_pcg::{Aabb, FlatSurface, Surface};
use engine_project::Project;
use engine_project::world::foliage::TerrainGround;
use glam::{Mat4, Vec3};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_OPS: usize = 256;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let ground = json!({"type":"object","additionalProperties":false,
        "description":"The ground: {height} flat at that y (default 0), or {terrain, origin?} a terrain asset id or .terrain file placed at `origin` [x,y,z] (translation only); terrain holes stay empty and terrain paint layers can drive density.",
        "properties":{"height":{"type":"number"},"terrain":{"type":"string"},
            "origin":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3}}});
    let bounds = |s: &str| json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3,"description":s});
    vec![
        tool(
            Spec {
                name: "foliage_scatter",
                title: "Scatter foliage",
                description: "Write a new binary .foliage asset: deterministically scatter up to 32 foliage `types` (engine.Foliage type objects: name, mesh, density per m2, density_map, scale, slope_degrees, height, altitude, align_to_normal, min_spacing, excludes, LOD distances ...) over a flat or terrain `ground` inside the box `center` / `size` (y range sets the altitude floor). Placement limits are exact; types are placed in list order. Existing files are never overwritten. Returns per-type counts and rejection reasons, the content hash, the asset entry (kind foliage) and an `engine.Foliage` snippet (mode asset); does not execute Rust.",
                input: schema::object(
                    vec![
                        (
                            "output",
                            schema::string(
                                "New project-relative .foliage file in an existing directory.",
                            ),
                        ),
                        (
                            "types",
                            schema::array_of(
                                "Foliage type objects (see engine.Foliage `types`).",
                                json!({"type":"object"}),
                                32,
                            ),
                        ),
                        (
                            "seed",
                            schema::integer("Scatter seed.", 0, 9_007_199_254_740_991, Some(0)),
                        ),
                        (
                            "center",
                            bounds("Box centre [x, y, z] (default [0,128,0])."),
                        ),
                        ("size", bounds("Box size [x, y, z] (default [64,256,64]).")),
                        (
                            "cell_size",
                            json!({"type":"number","minimum":1,"maximum":4096,"default":32.0,"description":"Storage cell edge in metres."}),
                        ),
                        ("ground", ground.clone()),
                        (
                            "zones",
                            schema::array_of(
                                "Exclusion zones: {shape: circle, center:[x,z], radius, types?} | {shape: rect, min:[x,z], max:[x,z], types?}.",
                                json!({"type":"object"}),
                                64,
                            ),
                        ),
                        (
                            "max_instances",
                            schema::integer(
                                "Instance cap; exceeding it fails.",
                                1,
                                262_144,
                                Some(65_536),
                            ),
                        ),
                    ],
                    &["output", "types"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("instances", json!({"type":"integer"})),
                    ("per_type", json!({"type":"array"})),
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
                name: "foliage_paint",
                title: "Paint foliage",
                description: "Apply up to 256 ordered brush strokes to an existing foliage asset, all or nothing: `add` places instances of the named type exactly as a scatter would (same limits, minimum spacing against existing instances) with density scaled by brush weight x strength; `erase` removes instances of the named type (all types when omitted) whose brush weight x strength exceeds a per-instance hash. Brushes are circles in world [x, z] with `radius`, `strength` and `falloff` (0..1); `seed` makes strokes deterministic (a stroke with the same seed is idempotent). Rewrites the file atomically.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Foliage asset id or project-relative .foliage file."),
                        ),
                        (
                            "expected_hash",
                            schema::string("Optional content hash the file must still have."),
                        ),
                        ("ground", ground.clone()),
                        (
                            "center",
                            bounds("Placement box centre for `add` (default [0,128,0])."),
                        ),
                        (
                            "size",
                            bounds("Placement box size for `add` (default [100000,256,100000])."),
                        ),
                        (
                            "ops",
                            schema::array_of(
                                "Strokes: {op: add|erase, type?: name, center:[x,z], radius, strength?, falloff?, seed?}.",
                                json!({"type":"object"}),
                                MAX_OPS,
                            ),
                        ),
                    ],
                    &["path", "ops"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("added", json!({"type":"integer"})),
                    ("removed", json!({"type":"integer"})),
                    ("instances", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                ]),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            paint_tool,
        ),
        tool(
            Spec {
                name: "foliage_info",
                title: "Foliage info",
                description: "Summarise a foliage asset: types, instance counts per type and per cell statistics, bounds, instances per square metre over the bounds, the content hash and, with `area` {center:[x,z], radius}, the instances inside that circle (counts per type, density per m2).",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Foliage asset id or project-relative .foliage file."),
                        ),
                        (
                            "area",
                            json!({"type":"object","description":"{center:[x,z], radius}"}),
                        ),
                    ],
                    &["path"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("instances", json!({"type":"integer"})),
                    ("types", json!({"type":"array"})),
                    ("cells", json!({"type":"object"})),
                    ("hash", json!({"type":"string"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            info,
        ),
    ]
}

fn failure(error: engine_foliage::Error) -> ToolFailure {
    ToolFailure::new(error.code, error.message)
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value, field: &str) -> Result<T, ToolFailure> {
    serde_json::from_value(value.clone())
        .map_err(|e| schema::invalid(field, &format!("valid foliage data ({e})")))
}
fn optional<T: serde::de::DeserializeOwned>(
    args: &Map<String, Value>,
    field: &str,
) -> Result<Option<T>, ToolFailure> {
    match args.get(field).filter(|v| !v.is_null()) {
        Some(v) => decode(v, field).map(Some),
        None => Ok(None),
    }
}
fn indexed(index: usize, mut error: ToolFailure) -> ToolFailure {
    error.message = format!("ops[{index}]: {}", error.message);
    error
}

/// Resolves an asset id or a project-relative `.foliage` file to an existing file.
fn target(cx: &ToolContext<'_>, project: &Project, value: &str) -> Result<PathBuf, ToolFailure> {
    let path = if project.assets.contains_key(value) {
        project.require_asset(value, "foliage")?;
        project.resolve_asset(value)?
    } else {
        cx.existing_file("path", &json!(value))?
    };
    if path.extension().and_then(|s| s.to_str()) != Some("foliage") {
        return Err(schema::invalid(
            "path",
            "a foliage asset id or .foliage file",
        ));
    }
    Ok(path)
}
fn load(path: &Path) -> Result<(FoliageAsset, Vec<u8>), ToolFailure> {
    let bytes = paths::read_bounded(path, asset::MAX_FILE_BYTES)
        .map_err(|_| ToolFailure::new("file_too_large", "foliage file exceeds its size limit"))?;
    Ok((asset::decode(&bytes).map_err(failure)?, bytes))
}

enum Ground {
    Flat(FlatSurface),
    Terrain(Box<TerrainGround>),
}
impl Ground {
    fn surface(&self) -> &dyn Surface {
        match self {
            Self::Flat(s) => s,
            Self::Terrain(t) => &**t,
        }
    }
    fn holes(&self) -> Option<&dyn engine_foliage::HoleMask> {
        match self {
            Self::Flat(_) => None,
            Self::Terrain(t) => Some(&**t),
        }
    }
}

fn ground(cx: &ToolContext<'_>, args: &Map<String, Value>) -> Result<Ground, ToolFailure> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct G {
        height: Option<f32>,
        terrain: Option<String>,
        origin: Option<[f32; 3]>,
    }
    let g: G = optional(args, "ground")?.unwrap_or(G {
        height: None,
        terrain: None,
        origin: None,
    });
    let Some(terrain) = g.terrain else {
        let height = g.height.unwrap_or(0.0);
        if !height.is_finite() {
            return Err(schema::invalid("ground", "a finite height"));
        }
        return Ok(Ground::Flat(FlatSurface::new(height)));
    };
    let project = cx.project()?;
    let file = if project.assets.contains_key(&terrain) {
        project.require_asset(&terrain, "terrain")?;
        project.resolve_asset(&terrain)?
    } else {
        cx.existing_file("ground", &json!(terrain))?
    };
    if file.extension().and_then(|s| s.to_str()) != Some("terrain") {
        return Err(schema::invalid(
            "ground",
            "a terrain asset id or .terrain file",
        ));
    }
    let bytes = paths::read_bounded(&file, engine_terrain::asset::MAX_FILE_BYTES)
        .map_err(|_| ToolFailure::new("file_too_large", "terrain file exceeds 64 MiB"))?;
    let field =
        engine_terrain::asset::decode(&bytes).map_err(|e| ToolFailure::new(e.code, e.message))?;
    let origin = g.origin.unwrap_or([0.0; 3]);
    if origin.iter().any(|v| !v.is_finite()) {
        return Err(schema::invalid("ground", "a finite origin"));
    }
    let ground = TerrainGround::new(Arc::new(field), Mat4::from_translation(Vec3::from(origin)))
        .ok_or_else(|| schema::invalid("ground", "a usable terrain"))?;
    Ok(Ground::Terrain(Box::new(ground)))
}

fn placement_box(args: &Map<String, Value>, default_size: [f32; 3]) -> Result<Aabb, ToolFailure> {
    let center: [f32; 3] = optional(args, "center")?.unwrap_or([0.0, 128.0, 0.0]);
    let size: [f32; 3] = optional(args, "size")?.unwrap_or(default_size);
    if center.iter().any(|v| !v.is_finite() || v.abs() > 1.0e6)
        || size.iter().any(|v| !(*v > 0.0 && *v <= 1.0e6))
    {
        return Err(schema::invalid(
            "size",
            "finite center within 1e6 and size components within (0, 1e6]",
        ));
    }
    let half = size.map(|v| v * 0.5);
    Ok(Aabb::new(
        std::array::from_fn(|i| center[i] - half[i]),
        std::array::from_fn(|i| center[i] + half[i]),
    ))
}

// ---- foliage_scatter -------------------------------------------------------------------------

fn create(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let output = a.string("output")?;
    let checked = paths::resolve_write_target(cx.root, output, "output")?;
    if checked.exists
        || checked.needs_directories
        || checked.path.extension().and_then(|s| s.to_str()) != Some("foliage")
    {
        return Err(schema::invalid(
            "output",
            "a new .foliage file in an existing project directory",
        ));
    }
    let types: Vec<FoliageType> = decode(args.get("types").unwrap_or(&Value::Null), "types")?;
    engine_foliage::validate_types(&types).map_err(failure)?;
    let zones: Vec<ExclusionZone> = optional(args, "zones")?.unwrap_or_default();
    if zones.len() > 64 {
        return Err(schema::invalid("zones", "at most 64 zones"));
    }
    let seed = a.integer("seed", 0, 9_007_199_254_740_991, 0)?;
    let cell_size: f32 = optional(args, "cell_size")?.unwrap_or(32.0);
    let max_instances = a.integer("max_instances", 1, 262_144, 65_536)? as usize;
    let bounds = placement_box(args, [64.0, 256.0, 64.0])?;
    let ground = ground(cx, args)?;
    let params = ScatterParams {
        seed,
        bounds,
        cell_size,
        zones: &zones,
        holes: ground.holes(),
        max_instances,
    };
    let result = scatter(&types, ground.surface(), &params).map_err(failure)?;
    let bytes = asset::encode(&types, &result.field).map_err(failure)?;
    publish_new_file(cx, output, &bytes, "foliage.publish")?;
    Ok(ToolResult::new(super::clean(&json!({
        "path": output,
        "instances": result.field.len(),
        "per_type": result.types,
        "bytes": bytes.len(),
        "hash": paths::content_hash(&bytes),
        "asset": {"id":"choose-a-unique-foliage-id","kind":"foliage","path":output},
        "component": {"engine.Foliage":{"version":1,"data":{
            "mode":"asset","instances":"choose-a-unique-foliage-id","terrain":""}}},
    }))))
}

// ---- foliage_paint ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrokeArg {
    op: String,
    #[serde(rename = "type")]
    type_name: Option<String>,
    center: [f32; 2],
    radius: f32,
    strength: Option<f32>,
    falloff: Option<f32>,
    seed: Option<u64>,
}

fn paint_tool(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, a.string("path")?)?;
    let (mut loaded, bytes) = load(&file)?;
    if let Some(expected) = a.optional_string("expected_hash")?
        && expected != paths::content_hash(&bytes)
    {
        return Err(ToolFailure::new(
            "stale_hash",
            "the foliage file changed since expected_hash was read",
        )
        .with_hint("Re-read it with foliage_info and retry."));
    }
    let ops: Vec<Value> = decode(args.get("ops").unwrap_or(&Value::Null), "ops")?;
    if ops.is_empty() || ops.len() > MAX_OPS {
        return Err(schema::invalid("ops", "1 to 256 strokes"));
    }
    let bounds = placement_box(args, [100_000.0, 256.0, 100_000.0])?;
    let ground = ground(cx, args)?;
    let ctx = PaintContext {
        surface: ground.surface(),
        bounds,
        zones: &[],
        holes: ground.holes(),
        max_instances: engine_foliage::MAX_INSTANCES,
    };
    let (mut added, mut removed) = (0usize, 0usize);
    for (index, op) in ops.iter().enumerate() {
        let stroke: StrokeArg = decode(op, "ops").map_err(|e| indexed(index, e))?;
        let type_index = match &stroke.type_name {
            Some(name) => Some(
                loaded
                    .types
                    .iter()
                    .position(|t| &t.name == name)
                    .ok_or_else(|| {
                        indexed(
                            index,
                            schema::invalid("ops", &format!("a type of the asset, not `{name}`")),
                        )
                    })?,
            ),
            None => None,
        };
        let brush = Brush {
            center: stroke.center,
            radius: stroke.radius,
            strength: stroke.strength.unwrap_or(1.0),
            falloff: stroke.falloff.unwrap_or(0.5),
        };
        let paint_op = match stroke.op.as_str() {
            "add" => PaintOp::Add {
                type_index: type_index
                    .ok_or_else(|| indexed(index, schema::invalid("ops", "a type name for add")))?,
            },
            "erase" => PaintOp::Erase { type_index },
            _ => return Err(indexed(index, schema::invalid("ops", "op add or erase"))),
        };
        let report = paint(
            &mut loaded.field,
            &loaded.types,
            &ctx,
            &brush,
            paint_op,
            stroke.seed.unwrap_or(index as u64),
        )
        .map_err(|e| indexed(index, failure(e)))?;
        added += report.added;
        removed += report.removed;
    }
    let out = asset::encode(&loaded.types, &loaded.field).map_err(failure)?;
    if added + removed > 0 {
        paths::atomic_write(&file, &out)
            .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    }
    Ok(ToolResult::new(json!({
        "path": cx.relative(&file),
        "added": added,
        "removed": removed,
        "instances": loaded.field.len(),
        "hash": paths::content_hash(&out),
    })))
}

// ---- foliage_info ----------------------------------------------------------------------------

fn info(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Area {
        center: [f32; 2],
        radius: f32,
    }
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, a.string("path")?)?;
    let (loaded, bytes) = load(&file)?;
    let field: &FoliageField = &loaded.field;
    let counts = field.counts_per_type(loaded.types.len());
    let types: Vec<Value> = loaded
        .types
        .iter()
        .zip(&counts)
        .map(|(t, n)| json!({"name": t.name, "mesh": t.mesh, "density": t.density, "instances": n}))
        .collect();
    let cell_counts: Vec<usize> = field.cells().map(|(_, c)| c.instances.len()).collect();
    let bounds = field.bounds();
    let extent = bounds.map(|(lo, hi)| ((hi.x - lo.x).max(0.0), (hi.z - lo.z).max(0.0)));
    let density = extent
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)
        .map(|(w, h)| field.len() as f32 / (w * h));
    let area: Option<Area> = optional(args, "area")?;
    let area_report = match area {
        Some(area) => {
            if !(area.radius.is_finite()
                && area.radius > 0.0
                && area.radius <= 1.0e5
                && area.center.iter().all(|v| v.is_finite()))
            {
                return Err(schema::invalid(
                    "area",
                    "a finite center and radius within (0, 100000]",
                ));
            }
            let hits = field.query_disc(area.center[0], area.center[1], area.radius);
            let mut per = vec![0usize; loaded.types.len()];
            for (_, _, i) in &hits {
                if let Some(c) = per.get_mut(usize::from(i.type_index)) {
                    *c += 1;
                }
            }
            let disc = std::f32::consts::PI * area.radius * area.radius;
            Some(json!({
                "instances": hits.len(),
                "per_type": loaded.types.iter().zip(&per).map(|(t, n)| json!({"name": t.name, "instances": n})).collect::<Vec<_>>(),
                "density_per_m2": hits.len() as f32 / disc,
            }))
        }
        None => None,
    };
    Ok(ToolResult::new(super::clean(&json!({
        "path": cx.relative(&file),
        "instances": field.len(),
        "types": types,
        "cell_size": field.cell_size(),
        "cells": {
            "count": cell_counts.len(),
            "max_instances": cell_counts.iter().copied().max().unwrap_or(0),
            "mean_instances": if cell_counts.is_empty() { 0.0 } else { field.len() as f32 / cell_counts.len() as f32 },
        },
        "bounds": bounds.map(|(lo, hi)| json!({"min": lo.to_array(), "max": hi.to_array()})),
        "density_per_m2": density,
        "area": area_report,
        "hash": paths::content_hash(&bytes),
    }))))
}
