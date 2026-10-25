//! Asset catalog tools: `asset_list` and `asset_info`.

use super::{Spec, scene::scene_paths, slash, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{Asset, Project, content, load_scene};
use serde_json::{Map, Value, json};
use std::path::Path;

const KINDS: [&str; 16] = [
    "script",
    "mesh",
    "texture",
    "font",
    "audio",
    "scene",
    "shader",
    "prefab",
    "model",
    "animation",
    "navigation",
    "acoustics",
    "lut",
    "tileset",
    "tilemap",
    "pcg",
];
const MAX_USES: usize = 200;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "asset_list",
                title: "List assets",
                description: "List the project's asset catalog: stable asset ID, kind and \
                    project-relative path (plus file size). Entities reference assets by these \
                    IDs, never by path. Filter by kind and page with offset/limit.",
                input: schema::object(
                    vec![
                        (
                            "kind",
                            schema::string_enum("Only assets of this kind.", &KINDS),
                        ),
                        (
                            "offset",
                            schema::integer("Assets to skip (paging).", 0, 10_000_000, Some(0)),
                        ),
                        (
                            "limit",
                            schema::integer("Maximum assets to return.", 1, 1000, Some(200)),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    (
                        "count",
                        json!({"type": "integer", "description": "Total matching assets."}),
                    ),
                    (
                        "assets",
                        json!({"type": "array", "items": {"type": "object", "properties": {
                        "id": {"type": "string"}, "kind": {"type": "string"},
                        "path": {"type": "string"}, "size_bytes": {"type": "integer"}}}}),
                    ),
                    ("returned", json!({"type": "integer"})),
                    ("offset", json!({"type": "integer"})),
                    ("truncated", json!({"type": "boolean"})),
                    ("next_offset", json!({"type": "integer"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            asset_list,
        ),
        tool(
            Spec {
                name: "asset_info",
                title: "Asset details",
                description: "Describe one catalog asset by ID: kind, path, size and \
                    kind-specific details (texture width/height/format, mesh vertex count and \
                    bounds, font glyph count, script line count, scene entity count) plus where \
                    it is used (scene, entity, component).",
                input: schema::object(
                    vec![(
                        "asset",
                        schema::string("Asset ID, for example `asset:player-texture`."),
                    )],
                    &["asset"],
                ),
                output: schema::output(vec![
                    ("asset", json!({"type": "object"})),
                    ("details", json!({"type": "object"})),
                    (
                        "used_by",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    ("used_by_truncated", json!({"type": "boolean"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            asset_info,
        ),
    ]
}

fn file_size(cx: &ToolContext<'_>, asset: &Asset) -> Option<u64> {
    std::fs::metadata(cx.root.join(&asset.path))
        .ok()
        .map(|m| m.len())
}

fn asset_json(cx: &ToolContext<'_>, asset: &Asset) -> Value {
    let mut value = json!({"id": asset.id, "kind": asset.kind, "path": slash(&asset.path)});
    if let Some(size) = file_size(cx, asset) {
        value["size_bytes"] = json!(size);
    }
    value
}

fn asset_list(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let kind = a.one_of("kind", &KINDS)?;
    let offset = a.integer("offset", 0, 10_000_000, 0)? as usize;
    let limit = a.integer("limit", 1, cx.limits.max_page_size as u64, 200)? as usize;
    let project = cx.project()?;
    let matching: Vec<&Asset> = project
        .assets
        .values()
        .filter(|asset| kind.is_none_or(|k| asset.kind == k))
        .collect();
    let page: Vec<Value> = matching
        .iter()
        .skip(offset)
        .take(limit)
        .map(|asset| asset_json(cx, asset))
        .collect();
    let end = offset + page.len();
    let mut result = json!({"count": matching.len(), "returned": page.len(), "offset": offset,
        "truncated": end < matching.len(), "assets": page});
    if end < matching.len() {
        result["next_offset"] = json!(end);
    }
    Ok(ToolResult::new(result))
}

fn details(cx: &ToolContext<'_>, project: &Project, asset: &Asset) -> Result<Value, ToolFailure> {
    let file = project.resolve_asset(&asset.id)?;
    Ok(match asset.kind.as_str() {
        "texture" => {
            let texture = content::load_texture(&file)?;
            let mut magic = [0_u8; 8];
            let png = std::fs::File::open(&file)
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
                .is_ok()
                && magic == *b"\x89PNG\r\n\x1a\n";
            json!({"width": texture.width, "height": texture.height,
                "format": if png { "png" } else { "ppm" }, "channels": "rgba8"})
        }
        "mesh" => {
            let mesh = content::load_mesh(&file)?;
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            for vertex in &mesh.vertices {
                for axis in 0..3 {
                    min[axis] = min[axis].min(vertex.position[axis]);
                    max[axis] = max[axis].max(vertex.position[axis]);
                }
            }
            json!({"vertices": mesh.vertices.len(), "triangles": mesh.indices.len() / 3,
                "bounds": {"min": min, "max": max}})
        }
        "font" => {
            let font = content::load_font(&file)?;
            json!({"glyphs": font.glyphs.len(), "line_height": font.line_height,
                "fallback_codepoint": font.fallback, "texture_asset": font.texture})
        }
        "script" => {
            let bytes = paths::read_bounded(&file, cx.limits.max_file_scan_bytes)
                .map_err(|_| ToolFailure::new("file_too_large", "script source is too large"))?;
            let lines = String::from_utf8_lossy(&bytes).lines().count();
            json!({"language": "rust", "lines": lines, "bytes": bytes.len()})
        }
        "model" => {
            let model = engine_project::model_import::load_model(&file)?;
            json!({"version":model.version,"nodes":model.scene.nodes.len(),"meshes":model.scene.meshes.len(),
                "materials":model.scene.materials.len(),"images":model.scene.images.len(),"skins":model.scene.skins.len(),
                "clips":model.scene.clips.len(),"fingerprint":blake3::Hash::from(model.scene.fingerprint.0).to_hex().to_string()})
        }
        "navigation" => {
            let mesh = engine_project::simulation::load_navmesh(project, &asset.id)?;
            json!({"polygons":mesh.polygon_count(),"settings":mesh.settings()})
        }
        "acoustics" => {
            let bytes =
                paths::read_bounded(&file, engine_project::acoustics::MAX_BAKED_DOCUMENT_BYTES)
                    .map_err(|_| {
                        ToolFailure::new("file_too_large", "acoustics document is too large")
                    })?;
            let document = engine_project::acoustics::validate_document(&asset.id, &bytes)?;
            json!({"version":document.version,"source_scene":document.source_scene,
                "materials":document.materials.len(),"meshes":document.meshes.len(),
                "instances":document.instances.len(),"volumes":document.volumes.len(),
                "proxy":document.proxy.is_some(),"content_hash":document.content_hash})
        }
        "tileset" => {
            let (tileset, size) = engine_project::world::tilemap::load_tileset(project, &asset.id)?;
            let document = tileset.document();
            json!({"texture":document.texture,"texture_size":size,"tiles":document.tiles.len(),
                "rule_tiles":document.rule_tiles.len()})
        }
        "tilemap" => {
            let map = engine_tilemap::Tilemap::from_json(
                &paths::read_bounded(&file, 16 * 1024 * 1024)
                    .map_err(|_| ToolFailure::new("file_too_large", "tilemap is too large"))?,
                None,
            )
            .map_err(|e| ToolFailure::new(e.code, e.message))?;
            let layers: Vec<_> = map
                .layers()
                .iter()
                .map(|l| json!({"name":l.name,"cells":l.filled_cells()}))
                .collect();
            json!({"bounds":{"min":map.bounds().min,"max":map.bounds().max},"layers":layers})
        }
        "pcg" => {
            let graph = engine_project::world::pcg::load_graph(project, &asset.id)?;
            json!({"version": graph.version, "seed": graph.seed,
                "nodes": graph.nodes.len(), "edges": graph.edges.len()})
        }
        "scene" | "prefab" => {
            let relative = file.strip_prefix(cx.root).expect("confined file");
            let scene = load_scene(cx.root, relative)?;
            json!({"scene_id": scene.id, "entities": scene.entities.len(),
                "revision": scene.revision()})
        }
        _ => json!({}),
    })
}

/// Scenes and fonts that reference `asset`.
fn used_by(cx: &ToolContext<'_>, project: &Project, asset: &Asset) -> (Vec<Value>, bool) {
    let mut uses = Vec::new();
    let prefabs = project
        .assets
        .values()
        .filter(|a| a.kind == "prefab")
        .map(|a| (slash(&a.path), Some(a.id.clone()), false));
    for (path, _, _) in scene_paths(project).into_iter().chain(prefabs) {
        let Ok(scene) = load_scene(cx.root, Path::new(&path)) else {
            continue;
        };
        for entity in &scene.entities {
            let c = &entity.components;
            let mut fields: Vec<(&str, &str, &str)> = Vec::new();
            if let Some(s) = &c.script {
                fields.push(("Script", "asset", &s.asset));
            }
            if let Some(m) = &c.mesh_renderer {
                fields.push(("MeshRenderer", "mesh", &m.mesh));
                if let Some(t) = &m.texture {
                    fields.push(("MeshRenderer", "texture", t));
                }
            }
            if let Some(material) = &c.material {
                for (field, reference) in [
                    ("shader", &material.shader),
                    ("texture", &material.texture),
                    ("normal_texture", &material.normal_texture),
                    (
                        "metallic_roughness_texture",
                        &material.metallic_roughness_texture,
                    ),
                ] {
                    if let Some(reference) = reference {
                        fields.push(("MeshMaterial", field, reference));
                    }
                }
            }
            if let Some(s) = &c.sprite_renderer {
                fields.push(("SpriteRenderer", "texture", &s.texture));
            }
            if let Some(clip) = c
                .custom
                .get("engine.AnimationPlayer")
                .and_then(|v| v.data.get("clip"))
                .and_then(Value::as_str)
            {
                fields.push(("engine.AnimationPlayer", "clip", clip));
            }
            if let Some(model) = c
                .custom
                .get("engine.SkinBinding")
                .and_then(|v| v.data.get("model"))
                .and_then(Value::as_str)
            {
                fields.push(("engine.SkinBinding", "model", model));
            }
            if let Some(navmesh) = c
                .custom
                .get("engine.NavigationAgent")
                .and_then(|v| v.data.get("navmesh"))
                .and_then(Value::as_str)
            {
                fields.push(("engine.NavigationAgent", "navmesh", navmesh));
            }
            for (component, field, id) in fields {
                if id == asset.id {
                    uses.push(json!({"scene": path, "entity": entity.id,
                        "component": component, "field": field}));
                }
            }
        }
        if uses.len() > MAX_USES {
            break;
        }
    }
    if asset.kind == "texture" {
        for font in project.assets.values().filter(|a| a.kind == "font") {
            if let Ok(file) = project.resolve_asset(&font.id)
                && content::load_font(&file).is_ok_and(|f| f.texture == asset.id)
            {
                uses.push(json!({"asset": font.id, "field": "texture"}));
            }
        }
    }
    let truncated = uses.len() > MAX_USES;
    uses.truncate(MAX_USES);
    (uses, truncated)
}

fn asset_info(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let id = Args(args).string("asset")?;
    let project = cx.project()?;
    let Some(asset) = project.assets.get(id) else {
        let needle = id.to_lowercase();
        let suggestions: Vec<&str> = project
            .assets
            .keys()
            .map(String::as_str)
            .filter(|candidate| candidate.to_lowercase().contains(&needle))
            .take(10)
            .collect();
        return Err(ToolFailure::new(
            "asset_not_found",
            format!("the project catalog has no asset with ID {id}"),
        )
        .with_hint("List asset IDs with asset_list; IDs are exact and case-sensitive.")
        .with_details(json!({"suggestions": suggestions})));
    };
    let (uses, truncated) = used_by(cx, &project, asset);
    Ok(ToolResult::new(json!({
        "asset": asset_json(cx, asset),
        "details": details(cx, &project, asset)?,
        "used_by": uses,
        "used_by_truncated": truncated,
    })))
}
