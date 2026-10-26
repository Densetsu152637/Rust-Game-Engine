//! Level streaming tools: split a scene into cell documents, build HLOD proxies and inspect a
//! partition manifest.
//!
//! * `streaming_partition` assigns a scene's entities to grid cells by position and writes one
//!   cell document per (cell, data layer) plus the `partition` manifest. Positions are stored
//!   relative to the cell corner so they stay precise far from the origin.
//! * `streaming_hlod` merges each cell's static meshes into one simplified proxy mesh
//!   (`engine-mesh-lod`) and records it in the manifest.
//! * `streaming_info` summarises a manifest and answers "what would load here".
//!
//! The tools only read and write project files; they never execute game code.
use super::{Spec, navigation, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_mesh_lod::{HlodInstance, HlodOptions, MeshData, build_hlod};
use engine_project::content::{Mesh, Vertex};
use engine_project::world::streaming::{LAYER_NAME, PARTITION_NAME, SOURCE_NAME};
use engine_project::{EditOperation, Scene, SceneDocument, SceneEntity};
use engine_streaming::{CellKey, Grid, GridPlane, HlodProxy, ManifestCell, PartitionManifest};
use glam::{Mat4, Quat, Vec2, Vec3};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_CELL_FILES: usize = 8192;
const MAX_HLOD_CELLS: usize = 2048;
const MAX_LISTED: usize = 64;
const MAX_SKIPPED: usize = 32;
const STRIP_CHUNK: usize = 900;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "streaming_partition",
                title: "Partition scene into cells",
                description: "Split a scene into world-partition cells. Entities (with their children) are assigned to grid cells by the position of their root entity (absolute position = origin_offset + local position) and written as one scene document per cell and data layer (layer = the root's engine.DataLayer component, default none) into output_dir, plus partition.json (project asset kind `partition`) listing the cells. Root positions are stored relative to the cell corner. Cameras, engine.WorldPartition / engine.StreamingSource entities, `exclude`d ids and anything without a Transform stay in the scene. Never overwrites: every target must be new. With strip=true the moved entities are removed from the scene (requires expected_revision). Attach the result with an engine.WorldPartition component whose manifest is the new asset id.",
                input: schema::object(
                    vec![
                        ("scene", schema::string("Project-relative scene to split.")),
                        (
                            "output_dir",
                            schema::string(
                                "Project-relative directory for the cell files and partition.json (created when missing, one level).",
                            ),
                        ),
                        (
                            "cell_size",
                            json!({"type": "number", "minimum": 1.0, "maximum": 10_000_000.0,
                                "description": "Cell edge length in metres."}),
                        ),
                        (
                            "plane",
                            schema::string_enum(
                                "`xz` (default): vertical columns; `xyz`: also divide height.",
                                &["xz", "xyz"],
                            ),
                        ),
                        (
                            "origin_offset",
                            json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3,
                                "description": "Absolute position (f64 metres) of the scene origin; default [0,0,0]."}),
                        ),
                        (
                            "exclude",
                            schema::array_of(
                                "Entity ids (and their children) that stay in the scene.",
                                json!({"type": "string"}),
                                4096,
                            ),
                        ),
                        (
                            "strip",
                            schema::boolean("Remove the moved entities from the scene.", false),
                        ),
                        (
                            "expected_revision",
                            schema::string(
                                "Scene revision; required with strip, checked when given.",
                            ),
                        ),
                        (
                            "dry_run",
                            schema::boolean("Plan and report without writing anything.", false),
                        ),
                    ],
                    &["scene", "output_dir", "cell_size"],
                ),
                output: schema::output(vec![
                    ("manifest", json!({"type": "string"})),
                    ("hash", json!({"type": "string"})),
                    ("written", json!({"type": "boolean"})),
                    ("cells", json!({"type": "integer"})),
                    ("moved_entities", json!({"type": "integer"})),
                    ("kept_entities", json!({"type": "integer"})),
                    ("skipped", json!({"type": "array"})),
                    ("sample", json!({"type": "array"})),
                    ("asset", json!({"type": "object"})),
                    ("revision", json!({"type": "string"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            partition,
        ),
        tool(
            Spec {
                name: "streaming_hlod",
                title: "Build cell HLOD proxies",
                description: "Build a merged, simplified HLOD proxy mesh for every cell of a partition manifest that contains static meshes (MeshRenderer entities, transformed by their parent chain) and record it in the manifest. Proxies are written to <manifest dir>/hlod/ (project JSON mesh format, cell-local space) and shown by the runtime while the cell is not loaded. Existing proxy files named by the manifest are replaced; other existing files are never touched. proxy_ratio is the kept triangle fraction (0, 1]; max_proxy_triangles caps each proxy. Supply expected_hash (from streaming_info or a previous call) to guard concurrent edits.",
                input: schema::object(
                    vec![
                        (
                            "manifest",
                            schema::string("Project-relative partition manifest."),
                        ),
                        (
                            "proxy_ratio",
                            json!({"type": "number", "exclusiveMinimum": 0.0, "maximum": 1.0, "default": 0.1,
                                "description": "Fraction of merged triangles kept."}),
                        ),
                        (
                            "max_proxy_triangles",
                            schema::integer("Triangle cap per proxy.", 1, 65_536, Some(2048)),
                        ),
                        (
                            "expected_hash",
                            schema::string("`hash` of the manifest being updated."),
                        ),
                        (
                            "dry_run",
                            schema::boolean("Build and report without writing.", false),
                        ),
                    ],
                    &["manifest"],
                ),
                output: schema::output(vec![
                    ("manifest", json!({"type": "string"})),
                    ("previous_hash", json!({"type": "string"})),
                    ("hash", json!({"type": "string"})),
                    ("saved", json!({"type": "boolean"})),
                    ("built", json!({"type": "integer"})),
                    ("skipped", json!({"type": "array"})),
                    ("source_triangles", json!({"type": "integer"})),
                    ("proxy_triangles", json!({"type": "integer"})),
                    ("proxies", json!({"type": "array"})),
                ]),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            hlod,
        ),
        tool(
            Spec {
                name: "streaming_info",
                title: "Inspect partition manifest",
                description: "Summarise a partition manifest: grid, cell and entity counts, bytes, data layers, HLOD coverage, key bounds and missing cell files. Page through `cells` with offset/limit. With position (absolute metres) and radius it also lists the cells whose box is within radius of the point, nearest first - the cells a streaming source there would load. Reads files only; the live state of a running world is `streaming` in the runtime snapshot.",
                input: schema::object(
                    vec![
                        (
                            "manifest",
                            schema::string("Project-relative partition manifest."),
                        ),
                        (
                            "offset",
                            schema::integer("Cells to skip (paging).", 0, 10_000_000, Some(0)),
                        ),
                        (
                            "limit",
                            schema::integer("Most cells to list.", 1, 256, Some(64)),
                        ),
                        (
                            "position",
                            json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3,
                                "description": "Absolute position to query."}),
                        ),
                        (
                            "radius",
                            json!({"type": "number", "minimum": 0.0, "maximum": 1.0e9,
                                "description": "With position: list cells within this distance."}),
                        ),
                    ],
                    &["manifest"],
                ),
                output: schema::output(vec![
                    ("manifest", json!({"type": "string"})),
                    ("hash", json!({"type": "string"})),
                    ("cell_size", json!({"type": "number"})),
                    ("plane", json!({"type": "string"})),
                    ("cell_count", json!({"type": "integer"})),
                    ("entities", json!({"type": "integer"})),
                    ("bytes", json!({"type": "integer"})),
                    ("layers", json!({"type": "array"})),
                    ("hlod_cells", json!({"type": "integer"})),
                    ("bounds", json!({"type": "object"})),
                    ("missing_files", json!({"type": "array"})),
                    ("cells", json!({"type": "array"})),
                    ("truncated", json!({"type": "boolean"})),
                    ("within", json!({"type": "array"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            info,
        ),
    ]
}

// ---------------------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------------------

fn failure(code: &str, message: impl Into<String>) -> ToolFailure {
    ToolFailure::new(code, message)
}

fn number(
    args: &Map<String, Value>,
    name: &str,
    min: f64,
    max: f64,
    default: Option<f64>,
) -> Result<f64, ToolFailure> {
    match args.get(name).filter(|v| !v.is_null()) {
        None => default.ok_or_else(|| schema::invalid(name, "a number")),
        Some(value) => value
            .as_f64()
            .filter(|v| v.is_finite() && *v >= min && *v <= max)
            .ok_or_else(|| schema::invalid(name, &format!("a number from {min} to {max}"))),
    }
}

fn vec3(args: &Map<String, Value>, name: &str) -> Result<Option<[f64; 3]>, ToolFailure> {
    let Some(value) = args.get(name).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let numbers: Option<Vec<f64>> = value
        .as_array()
        .filter(|a| a.len() == 3)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .filter(|n: &Vec<f64>| {
            n.len() == 3 && n.iter().all(|v| v.is_finite() && v.abs() <= 1.0e12)
        });
    match numbers {
        Some(n) => Ok(Some([n[0], n[1], n[2]])),
        None => Err(schema::invalid(name, "an array of three finite numbers")),
    }
}

struct LoadedManifest {
    file: PathBuf,
    relative: String,
    bytes: Vec<u8>,
    manifest: PartitionManifest,
}

fn load_manifest(
    cx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<LoadedManifest, ToolFailure> {
    let file = cx.existing_file("manifest", args.get("manifest").unwrap_or(&Value::Null))?;
    let bytes = paths::read_bounded(&file, MAX_MANIFEST_BYTES)
        .map_err(|_| failure("file_too_large", "manifests are limited to 16 MiB"))?;
    let manifest = PartitionManifest::from_json(&bytes).map_err(|e| {
        failure("streaming_manifest_invalid", e.to_string())
            .with_hint("Create manifests with streaming_partition.")
    })?;
    let relative = cx.relative(&file);
    Ok(LoadedManifest {
        file,
        relative,
        bytes,
        manifest,
    })
}

fn plane_of(name: Option<&str>) -> GridPlane {
    if name == Some("xyz") {
        GridPlane::Xyz
    } else {
        GridPlane::Xz
    }
}

fn layer_suffix(layer: &str) -> String {
    if layer.is_empty() {
        String::new()
    } else {
        format!(".{layer}")
    }
}

fn cell_stem(key: CellKey, layer: &str) -> String {
    format!(
        "cell_{}_{}_{}{}",
        key.0[0],
        key.0[1],
        key.0[2],
        layer_suffix(layer)
    )
}

fn valid_layer(layer: &str) -> bool {
    !layer.is_empty()
        && layer.len() <= 64
        && layer
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

// ---------------------------------------------------------------------------------------
// streaming_partition
// ---------------------------------------------------------------------------------------

struct Skipped {
    id: String,
    reason: &'static str,
}

fn is_anchor(entity: &SceneEntity) -> bool {
    let c = &entity.components;
    c.camera3d.is_some()
        || c.camera_2d.is_some()
        || c.custom.contains_key(PARTITION_NAME)
        || c.custom.contains_key(SOURCE_NAME)
}

fn layer_of(entity: &SceneEntity) -> Result<String, ToolFailure> {
    let Some(value) = entity.components.custom.get(LAYER_NAME) else {
        return Ok(String::new());
    };
    let layer = value
        .data
        .get("layer")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if layer.is_empty() || valid_layer(layer) {
        Ok(layer.to_owned())
    } else {
        Err(failure(
            "streaming_layer_invalid",
            format!(
                "entity `{}` has layer `{layer}`; layer names are 1-64 characters of [A-Za-z0-9_-]",
                entity.id
            ),
        ))
    }
}

fn partition(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let cell_size = number(args, "cell_size", 1.0, 1.0e7, None)?;
    let plane = plane_of(a.one_of("plane", &["xz", "xyz"])?);
    let origin = vec3(args, "origin_offset")?.unwrap_or([0.0; 3]);
    let exclude: BTreeSet<&str> = a
        .string_array("exclude")?
        .unwrap_or_default()
        .into_iter()
        .collect();
    let strip = a.boolean("strip", false)?;
    let dry_run = a.boolean("dry_run", false)?;
    let expected = a.optional_string("expected_revision")?;
    if strip && expected.is_none() {
        return Err(schema::invalid(
            "expected_revision",
            "supplied together with `strip`",
        ));
    }
    let output_dir = a.string("output_dir")?.trim_end_matches('/');
    let grid = Grid::new(cell_size, plane).map_err(|e| schema::invalid("cell_size", &e.0))?;
    // Reject unusable destinations before any work.
    paths::resolve_write_target(
        cx.root,
        &format!("{output_dir}/partition.json"),
        "output_dir",
    )?;

    let loaded = super::scene::load(cx, args)?;
    if let Some(expected) = expected
        && loaded.revision != expected
    {
        return Err(failure(
            "revision_conflict",
            "the scene changed since the supplied expected_revision",
        )
        .with_hint("Re-read the scene with scene_inspect and retry with its revision."));
    }
    let scene = &loaded.scene;

    // Group every entity under the root of its parent chain.
    let index: BTreeMap<&str, usize> = scene
        .entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.as_str(), i))
        .collect();
    let root_of = |start: usize| -> Option<usize> {
        let mut current = start;
        for _ in 0..=scene.entities.len() {
            match &scene.entities[current].parent {
                None => return Some(current),
                Some(parent) => current = *index.get(parent.as_str())?,
            }
        }
        None
    };
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut unplaced: BTreeSet<usize> = BTreeSet::new();
    for (i, _) in scene.entities.iter().enumerate() {
        match root_of(i) {
            Some(root) => members.entry(root).or_default().push(i),
            None => {
                unplaced.insert(i);
            }
        }
    }

    let mut skipped: Vec<Skipped> = unplaced
        .iter()
        .map(|i| Skipped {
            id: scene.entities[*i].id.clone(),
            reason: "broken parent chain",
        })
        .collect();
    let mut cells: BTreeMap<(CellKey, String), Vec<usize>> = BTreeMap::new();
    let mut kept = unplaced.len();
    for (root, group) in &members {
        let root_entity = &scene.entities[*root];
        let reason = if exclude.contains(root_entity.id.as_str())
            || group
                .iter()
                .any(|i| exclude.contains(scene.entities[*i].id.as_str()))
        {
            Some("excluded")
        } else if group.iter().any(|i| is_anchor(&scene.entities[*i])) {
            Some("camera, partition or streaming source")
        } else if root_entity.components.transform.is_none() {
            Some("no Transform")
        } else {
            None
        };
        if let Some(reason) = reason {
            kept += group.len();
            skipped.extend(group.iter().map(|i| Skipped {
                id: scene.entities[*i].id.clone(),
                reason,
            }));
            continue;
        }
        let position = root_entity
            .components
            .transform
            .as_ref()
            .map(|t| t.position)
            .unwrap_or_default();
        let absolute: [f64; 3] = std::array::from_fn(|i| origin[i] + f64::from(position[i]));
        let layer = layer_of(root_entity)?;
        cells
            .entry((grid.key_of(absolute), layer))
            .or_default()
            .extend(group.iter().copied());
    }
    for group in cells.values_mut() {
        group.sort_unstable();
    }
    if cells.len() > MAX_CELL_FILES {
        return Err(failure(
            "streaming_too_many_cells",
            format!(
                "the scene spans {} cells; the limit is {MAX_CELL_FILES} per call. Use a larger cell_size",
                cells.len()
            ),
        ));
    }
    let moved: usize = cells.values().map(Vec::len).sum();

    // Build the documents.
    let dir_manifest = format!("{output_dir}/partition.json");
    let mut manifest = PartitionManifest::new(cell_size, plane);
    let mut documents: Vec<(String, Vec<u8>)> = Vec::new();
    for ((key, layer), group) in &cells {
        let cell_origin = grid.origin(*key);
        let entities: Vec<SceneEntity> = group
            .iter()
            .map(|i| {
                let mut entity = scene.entities[*i].clone();
                if entity.parent.is_none()
                    && let Some(transform) = entity.components.transform.as_mut()
                {
                    for (axis, position) in transform.position.iter_mut().enumerate() {
                        *position =
                            (origin[axis] + f64::from(*position) - cell_origin[axis]) as f32;
                    }
                }
                entity
            })
            .collect();
        let stem = cell_stem(*key, layer);
        let document = Scene {
            schema: 1,
            id: stem.clone(),
            entities,
            source: PathBuf::new(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
        let file = format!("{output_dir}/{stem}.json");
        manifest.cells.push(ManifestCell {
            key: *key,
            layer: layer.clone(),
            file: file.clone(),
            entities: group.len() as u32,
            bytes: bytes.len() as u64,
            hlod: None,
        });
        documents.push((file, bytes));
    }
    manifest
        .validate()
        .map_err(|e| failure("streaming_manifest_invalid", e.to_string()))?;
    let manifest_bytes = manifest.to_json().into_bytes();
    let hash = paths::content_hash(&manifest_bytes);

    // Every target must be new.
    for path in documents
        .iter()
        .map(|(p, _)| p.as_str())
        .chain([dir_manifest.as_str()])
    {
        let target = paths::resolve_write_target(cx.root, path, "output_dir")?;
        if target.exists {
            return Err(failure(
                "streaming_target_exists",
                format!("`{path}` already exists; choose an empty output_dir"),
            ));
        }
    }

    let sample: Vec<Value> = manifest
        .cells
        .iter()
        .take(MAX_LISTED)
        .map(|c| json!({"key": c.key, "layer": c.layer, "file": c.file, "entities": c.entities, "bytes": c.bytes}))
        .collect();
    let mut result = json!({
        "manifest": dir_manifest, "hash": hash, "written": !dry_run,
        "cells": manifest.cells.len(), "moved_entities": moved, "kept_entities": kept,
        "skipped": skipped.iter().take(MAX_SKIPPED)
            .map(|s| json!({"id": s.id, "reason": s.reason})).collect::<Vec<_>>(),
        "sample": sample,
        "asset": {"id": "choose-a-unique-partition-id", "kind": "partition", "path": dir_manifest},
    });

    // Prepare the stripped scene before touching the disk so an error leaves nothing behind.
    let mut stripped = None;
    if strip && !dry_run {
        let mut document = SceneDocument::for_scene(&loaded.project, loaded.scene.clone());
        let mut revision = loaded.revision.clone();
        let ids: Vec<String> = cells
            .values()
            .flatten()
            .map(|i| scene.entities[*i].id.clone())
            .collect();
        for chunk in ids.chunks(STRIP_CHUNK) {
            let operations = chunk
                .iter()
                .map(|id| EditOperation::DeleteEntity {
                    entity_id: id.clone(),
                })
                .collect();
            revision = document
                .apply(EditOperation::Batch { operations }, &revision)
                .map_err(ToolFailure::from)?;
        }
        stripped = Some((document, revision));
    }

    if dry_run {
        return Ok(ToolResult::new(result));
    }
    ensure_directory(cx, output_dir, &dir_manifest)?;
    let mut written: Vec<&str> = Vec::new();
    let outcome = (|| -> Result<(), ToolFailure> {
        for (file, bytes) in &documents {
            navigation::publish_new_file(cx, file, bytes, "streaming.publish")?;
            written.push(file);
        }
        navigation::publish_new_file(cx, &dir_manifest, &manifest_bytes, "streaming.publish")
    })();
    if let Err(error) = outcome {
        for file in written {
            if let Ok(target) = paths::resolve_write_target(cx.root, file, "output_dir") {
                let _ = std::fs::remove_file(target.path);
            }
        }
        return Err(error);
    }
    if let Some((mut document, revision)) = stripped {
        let saved = document.save(&revision).map_err(ToolFailure::from)?;
        result["revision"] = json!(saved);
        result["stripped"] = json!(moved);
    }
    Ok(ToolResult::new(result))
}

/// Creates the (single, validated) directory that will hold the manifest.
fn ensure_directory(cx: &ToolContext<'_>, dir: &str, manifest: &str) -> Result<(), ToolFailure> {
    let target = paths::resolve_write_target(cx.root, manifest, "output_dir")?;
    if target.needs_directories {
        let parent = target
            .path
            .parent()
            .ok_or_else(|| schema::invalid("output_dir", "a project directory"))?;
        // `resolve_write_target` proved every existing component is a real directory inside
        // the root; only the missing tail is created.
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
        if paths::resolve_existing_dir(cx.root, dir).is_none() {
            return Err(schema::invalid("output_dir", "a project directory"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// streaming_hlod
// ---------------------------------------------------------------------------------------

fn to_mesh_data(mesh: &Mesh) -> Result<MeshData, ToolFailure> {
    let positions: Vec<Vec3> = mesh
        .vertices
        .iter()
        .map(|v| Vec3::from(v.position))
        .collect();
    let mut data = MeshData::new(positions, mesh.indices.clone());
    if mesh.normals.len() == mesh.vertices.len() && !mesh.normals.is_empty() {
        data = data.with_normals(mesh.normals.iter().map(|n| Vec3::from(*n)).collect());
    }
    data = data.with_uvs(mesh.vertices.iter().map(|v| Vec2::from(v.uv)).collect());
    data.validate()
        .map_err(|e| failure("streaming_mesh_invalid", e.to_string()))?;
    Ok(data)
}

fn from_mesh_data(data: &MeshData) -> Mesh {
    let uv = |i: usize| data.uvs.as_ref().map_or([0.0; 2], |u| u[i].to_array());
    Mesh {
        normals: data
            .normals
            .as_ref()
            .map(|n| n.iter().map(|v| v.to_array()).collect())
            .unwrap_or_default(),
        vertices: data
            .positions
            .iter()
            .enumerate()
            .map(|(i, p)| Vertex {
                position: p.to_array(),
                uv: uv(i),
                color: [1.0; 4],
            })
            .collect(),
        indices: data.indices.clone(),
    }
}

/// Matrix of an entity within its cell: its own transform composed with its parents'.
fn local_matrix(
    entities: &[SceneEntity],
    index: &BTreeMap<&str, usize>,
    start: usize,
) -> Option<Mat4> {
    let mut matrix = Mat4::IDENTITY;
    let mut current = start;
    for _ in 0..=entities.len() {
        let entity = &entities[current];
        let t = entity.components.transform.clone().unwrap_or_default();
        let local = Mat4::from_scale_rotation_translation(
            Vec3::from(t.scale),
            Quat::from_array(t.rotation).normalize(),
            Vec3::from(t.position),
        );
        matrix = local * matrix;
        match &entity.parent {
            None => return Some(matrix),
            Some(parent) => current = *index.get(parent.as_str())?,
        }
    }
    None
}

fn hlod(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let ratio = number(args, "proxy_ratio", 1.0e-6, 1.0, Some(0.1))?;
    let max_triangles = a.integer("max_proxy_triangles", 1, 65_536, 2048)? as usize;
    let dry_run = a.boolean("dry_run", false)?;
    let expected = a.optional_string("expected_hash")?;
    let LoadedManifest {
        file,
        relative,
        bytes,
        mut manifest,
    } = load_manifest(cx, args)?;
    let previous_hash = paths::content_hash(&bytes);
    if let Some(expected) = expected
        && expected != previous_hash
    {
        return Err(failure(
            "revision_conflict",
            "the manifest changed since the supplied expected_hash",
        )
        .with_hint("Re-read it with streaming_info and retry."));
    }
    if manifest.cells.len() > MAX_HLOD_CELLS {
        return Err(failure(
            "streaming_too_many_cells",
            format!("at most {MAX_HLOD_CELLS} cells per call"),
        ));
    }
    let project = cx.project()?;
    let directory = Path::new(&relative)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let proxy_dir = if directory.is_empty() {
        "hlod".to_owned()
    } else {
        format!("{directory}/hlod")
    };
    let options = HlodOptions {
        max_cluster_extent: f32::MAX / 4.0,
        max_cluster_instances: usize::MAX,
        proxy_ratio: ratio as f32,
        max_proxy_triangles: max_triangles,
        ..HlodOptions::default()
    };

    let mut mesh_cache: BTreeMap<String, Option<MeshData>> = BTreeMap::new();
    let mut summaries = Vec::new();
    let mut skipped = Vec::new();
    let (mut total_source, mut total_proxy) = (0usize, 0usize);
    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut updated: Vec<(usize, HlodProxy)> = Vec::new();
    for (position, cell) in manifest.cells.iter().enumerate() {
        let name = cell_stem(cell.key, &cell.layer);
        let scene = engine_project::load_scene(cx.root, Path::new(&cell.file))
            .map_err(ToolFailure::from)?;
        let index: BTreeMap<&str, usize> = scene
            .entities
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.as_str(), i))
            .collect();
        let mut meshes: Vec<MeshData> = Vec::new();
        let mut mesh_slot: BTreeMap<String, usize> = BTreeMap::new();
        let mut instances = Vec::new();
        for (i, entity) in scene.entities.iter().enumerate() {
            let Some(renderer) = &entity.components.mesh_renderer else {
                continue;
            };
            if renderer.mesh.is_empty() {
                continue;
            }
            let Some(matrix) = local_matrix(&scene.entities, &index, i) else {
                continue;
            };
            if !mesh_cache.contains_key(&renderer.mesh) {
                let data = project
                    .require_asset(&renderer.mesh, "mesh")
                    .ok()
                    .and_then(|asset| {
                        engine_project::confined_file(&project.root, &asset.path).ok()
                    })
                    .and_then(|path| engine_project::content::load_mesh(&path).ok())
                    .and_then(|mesh| to_mesh_data(&mesh).ok());
                mesh_cache.insert(renderer.mesh.clone(), data);
            }
            let Some(Some(data)) = mesh_cache.get(&renderer.mesh) else {
                continue;
            };
            let slot = *mesh_slot.entry(renderer.mesh.clone()).or_insert_with(|| {
                meshes.push(data.clone());
                meshes.len() - 1
            });
            instances.push(HlodInstance {
                mesh: slot,
                transform: matrix,
            });
        }
        if instances.is_empty() {
            skipped.push(json!({"cell": name, "reason": "no static meshes"}));
            continue;
        }
        let clusters = build_hlod(&meshes, &instances, &options)
            .map_err(|e| failure("streaming_hlod_failed", format!("cell {name}: {e}")))?;
        let mut merged = MeshData::new(Vec::new(), Vec::new());
        let mut source_triangles = 0usize;
        for cluster in &clusters {
            source_triangles += cluster.merged_triangles;
            let base = merged.positions.len() as u32;
            merged.positions.extend_from_slice(&cluster.proxy.positions);
            merged
                .indices
                .extend(cluster.proxy.indices.iter().map(|i| i + base));
        }
        let triangles = merged.triangle_count();
        if triangles == 0 {
            skipped.push(json!({"cell": name, "reason": "empty proxy"}));
            continue;
        }
        let mesh = from_mesh_data(&merged);
        let output = serde_json::to_vec(&mesh)
            .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
        let path = format!("{proxy_dir}/{name}.mesh.json");
        total_source += source_triangles;
        total_proxy += triangles;
        summaries.push(
            json!({"cell": name, "file": path, "source_triangles": source_triangles,
            "triangles": triangles, "bytes": output.len()}),
        );
        updated.push((
            position,
            HlodProxy {
                file: path.clone(),
                triangles: triangles as u32,
                source_triangles: source_triangles as u32,
            },
        ));
        outputs.push((path, output));
    }
    for (position, proxy) in &updated {
        manifest.cells[*position].hlod = Some(proxy.clone());
    }
    let new_bytes = manifest.to_json().into_bytes();
    let new_hash = paths::content_hash(&new_bytes);
    let mut result = json!({
        "manifest": relative, "previous_hash": previous_hash, "hash": new_hash, "saved": !dry_run,
        "built": outputs.len(), "skipped": skipped.iter().take(MAX_SKIPPED).collect::<Vec<_>>(),
        "source_triangles": total_source, "proxy_triangles": total_proxy,
        "proxies": summaries.iter().take(MAX_LISTED).collect::<Vec<_>>(),
    });
    if dry_run {
        return Ok(ToolResult::new(result));
    }

    // Proxy files named by the current manifest may be replaced; anything else must be new.
    let owned: BTreeSet<String> = bytes_owned_proxies(&PartitionManifest::from_json(&bytes).ok());
    for (path, _) in &outputs {
        let target = paths::resolve_write_target(cx.root, path, "manifest")?;
        if target.exists && !owned.contains(path) {
            return Err(failure(
                "streaming_target_exists",
                format!("`{path}` exists and is not a proxy of this manifest"),
            ));
        }
    }
    if outputs.iter().any(|(path, _)| {
        paths::resolve_write_target(cx.root, path, "manifest")
            .map(|t| t.needs_directories)
            .unwrap_or(false)
    }) {
        let first = &outputs[0].0;
        let target = paths::resolve_write_target(cx.root, first, "manifest")?;
        let parent = target
            .path
            .parent()
            .ok_or_else(|| schema::invalid("manifest", "a project file"))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))?;
    }
    for (path, output) in &outputs {
        let target = paths::resolve_write_target(cx.root, path, "manifest")?;
        paths::atomic_write(&target.path, output).map_err(|e| {
            failure(
                "io_error",
                format!("`{path}` could not be written: {}", e.kind()),
            )
        })?;
    }
    paths::atomic_write(&file, &new_bytes).map_err(|e| {
        failure(
            "io_error",
            format!("the manifest could not be written: {}", e.kind()),
        )
    })?;
    result["saved"] = json!(true);
    Ok(ToolResult::new(result))
}

fn bytes_owned_proxies(manifest: &Option<PartitionManifest>) -> BTreeSet<String> {
    manifest
        .iter()
        .flat_map(|m| m.cells.iter())
        .filter_map(|c| c.hlod.as_ref().map(|h| h.file.clone()))
        .collect()
}

// ---------------------------------------------------------------------------------------
// streaming_info
// ---------------------------------------------------------------------------------------

fn info(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let offset = a.integer("offset", 0, 10_000_000, 0)? as usize;
    let limit = a.integer("limit", 1, 256, MAX_LISTED as u64)? as usize;
    let position = vec3(args, "position")?;
    let radius = number(args, "radius", 0.0, 1.0e9, Some(0.0))?;
    let LoadedManifest {
        relative,
        bytes,
        manifest,
        ..
    } = load_manifest(cx, args)?;
    let grid = manifest
        .grid()
        .map_err(|e| failure("streaming_manifest_invalid", e.to_string()))?;

    let mut layers: BTreeMap<&str, (usize, u64, u64)> = BTreeMap::new();
    let (mut entities, mut total_bytes, mut hlod_cells) = (0u64, 0u64, 0usize);
    let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
    let mut missing = Vec::new();
    for cell in &manifest.cells {
        let layer = layers.entry(cell.layer.as_str()).or_default();
        layer.0 += 1;
        layer.1 += u64::from(cell.entities);
        layer.2 += cell.bytes;
        entities += u64::from(cell.entities);
        total_bytes += cell.bytes;
        hlod_cells += usize::from(cell.hlod.is_some());
        for (axis, key) in cell.key.0.iter().enumerate() {
            lo[axis] = lo[axis].min(*key);
            hi[axis] = hi[axis].max(*key);
        }
        let present = paths::resolve_existing_file(cx.root, &cell.file).is_some();
        let proxy_present = cell
            .hlod
            .as_ref()
            .is_none_or(|h| paths::resolve_existing_file(cx.root, &h.file).is_some());
        if (!present || !proxy_present) && missing.len() < MAX_LISTED {
            missing.push(json!({"key": cell.key, "layer": cell.layer,
                "file": if present { cell.hlod.as_ref().map(|h| h.file.clone()).unwrap_or_default() } else { cell.file.clone() }}));
        }
    }
    let cell_json = |cell: &ManifestCell, distance: Option<f64>| {
        let mut value = json!({"key": cell.key, "layer": cell.layer, "file": cell.file,
            "entities": cell.entities, "bytes": cell.bytes,
            "hlod": cell.hlod.as_ref().map(|h| json!({"file": h.file, "triangles": h.triangles, "source_triangles": h.source_triangles}))});
        if let Some(d) = distance {
            value["distance"] = json!((d * 1000.0).round() / 1000.0);
        }
        value
    };
    let mut sorted: Vec<&ManifestCell> = manifest.cells.iter().collect();
    sorted.sort_by(|x, y| (x.key, &x.layer).cmp(&(y.key, &y.layer)));
    let page: Vec<Value> = sorted
        .iter()
        .skip(offset)
        .take(limit)
        .map(|c| cell_json(c, position.map(|p| grid.distance(p, c.key))))
        .collect();
    let mut result = json!({
        "manifest": relative, "hash": paths::content_hash(&bytes),
        "cell_size": manifest.cell_size,
        "plane": if manifest.plane == GridPlane::Xyz { "xyz" } else { "xz" },
        "cell_count": manifest.cells.len(), "entities": entities, "bytes": total_bytes,
        "layers": layers.iter().map(|(name, (cells, ents, b))|
            json!({"name": name, "cells": cells, "entities": ents, "bytes": b})).collect::<Vec<_>>(),
        "hlod_cells": hlod_cells,
        "bounds": if manifest.cells.is_empty() { Value::Null } else { json!({"min": lo, "max": hi}) },
        "missing_files": missing,
        "cells": page, "offset": offset,
        "truncated": offset + limit < sorted.len(),
    });
    if let Some(p) = position {
        let mut within: Vec<(f64, &ManifestCell)> = manifest
            .cells
            .iter()
            .map(|c| (grid.distance(p, c.key), c))
            .filter(|(d, _)| *d <= radius)
            .collect();
        within.sort_by(|x, y| {
            x.0.total_cmp(&y.0)
                .then((x.1.key, &x.1.layer).cmp(&(y.1.key, &y.1.layer)))
        });
        result["within_count"] = json!(within.len());
        result["within"] = Value::Array(
            within
                .iter()
                .take(limit)
                .map(|(d, c)| cell_json(c, Some(*d)))
                .collect(),
        );
    }
    Ok(ToolResult::new(result))
}
