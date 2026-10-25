//! Tilemap authoring and queries over `tileset` and `tilemap` JSON assets; never executes game
//! code. Files are validated by the shared `engine-tilemap` model before they are written.
use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{Project, world::tilemap as host};
use engine_tilemap::{
    CellRect, CollisionKind, Rule, RuleTile, TileDef, TileId, Tilemap, Tileset, TilesetDocument,
    collision,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_OPS: usize = 256;
const MAX_QUERY_CELLS: usize = 4096;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "tilemap_create_tileset",
                title: "Create tileset",
                description: "Write a new tileset JSON file for a project texture atlas: concrete tiles with atlas rectangles, collision (none, full, rect or polygon as cell fractions) and properties. Give explicit `tiles`, a regular `grid` that numbers tiles row-major from `first_id`, or both. Existing files are never overwritten. Returns the asset entry (kind tileset) to add to project.json; does not execute Rust.",
                input: schema::object(
                    vec![
                        (
                            "output",
                            schema::string(
                                "New project-relative .json file in an existing directory.",
                            ),
                        ),
                        (
                            "texture",
                            schema::string("Texture asset id holding the atlas."),
                        ),
                        ("name", schema::string("Display name of the tileset.")),
                        (
                            "tile_size",
                            int_vector(2, 1, 4096, "Nominal tile size [width, height] in pixels."),
                        ),
                        (
                            "tiles",
                            schema::array_of(
                                "Explicit tiles: {id, name?, source:[x,y,w,h], collision?, properties?}. collision is \"none\", \"full\", {\"rect\":{\"min\":[x,y],\"max\":[x,y]}} or {\"polygon\":{\"points\":[[x,y],...]}} in cell fractions 0..1.",
                                json!({"type":"object"}),
                                4096,
                            ),
                        ),
                        (
                            "grid",
                            described(
                                schema::object(
                                    vec![
                                        (
                                            "first_id",
                                            schema::integer(
                                                "Id of the first tile.",
                                                1,
                                                16_777_215,
                                                Some(1),
                                            ),
                                        ),
                                        (
                                            "columns",
                                            schema::integer("Tiles per row.", 1, 256, None),
                                        ),
                                        ("rows", schema::integer("Rows of tiles.", 1, 256, None)),
                                        (
                                            "offset",
                                            int_vector_unsigned(
                                                2,
                                                "Atlas offset [x, y] of the first tile in pixels.",
                                            ),
                                        ),
                                        (
                                            "spacing",
                                            int_vector_unsigned(
                                                2,
                                                "Gap [x, y] between tiles in pixels.",
                                            ),
                                        ),
                                        (
                                            "collision",
                                            schema::string_enum(
                                                "Collision of every grid tile.",
                                                &["none", "full"],
                                            ),
                                        ),
                                    ],
                                    &["columns", "rows"],
                                ),
                                "Regular grid of equal tiles of `tile_size`.",
                            ),
                        ),
                    ],
                    &["output", "texture", "tile_size"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("tiles", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                    ("asset", json!({"type":"object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            create_tileset,
        ),
        tool(
            Spec {
                name: "tilemap_create",
                title: "Create tilemap",
                description: "Write a new, empty tilemap JSON file with bounded cell bounds and named layers (z order, visibility, opacity). The tileset is only checked to exist. Existing files are never overwritten. Returns the asset entry (kind tilemap) and an `engine.Tilemap` component snippet to place on an entity; does not execute Rust.",
                input: schema::object(
                    vec![
                        (
                            "output",
                            schema::string(
                                "New project-relative .json file in an existing directory.",
                            ),
                        ),
                        (
                            "tileset",
                            schema::string(
                                "Tileset asset id (or project-relative tileset file) this map will use.",
                            ),
                        ),
                        (
                            "bounds",
                            rect_schema(
                                "Cell bounds, min inclusive and max exclusive; at most 65536 cells per edge. Default -512..512 on both axes.",
                            ),
                        ),
                        (
                            "layers",
                            schema::array_of(
                                "Layers in order: {name, z_order?, visible?, opacity?}. Default one layer named ground.",
                                json!({"type":"object"}),
                                32,
                            ),
                        ),
                    ],
                    &["output", "tileset"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("layers", json!({"type":"array"})),
                    ("hash", json!({"type":"string"})),
                    ("asset", json!({"type":"object"})),
                    ("component", json!({"type":"object"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            create_tilemap,
        ),
        tool(
            Spec {
                name: "tilemap_define_rules",
                title: "Define rule tile",
                description: "Add or replace a rule tile (autotiling) in an existing tileset file. Use preset `blob47` (47 tiles, ids first_tile..first_tile+46 in ascending reduced-mask order) or `four_neighbour16` (16 tiles, first_tile + N1/E2/S4/W8 mask), and/or custom `rules`: eight neighbour requirements [NW,N,NE,W,E,SW,S,SE] of any/this/not_this, an output tile and a transform (none, rotated, mirror_x, mirror_y, mirror_xy). Custom rules are tried before preset rules; the first match wins. The result is validated before the file is rewritten atomically.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Existing tileset asset id or project-relative file."),
                        ),
                        (
                            "rule_tile",
                            json!({"type":"object","description":"{id, name?, preset?, first_tile?, default_tile?, connects?:[tile ids], rules?:[{neighbours:[8], output, transform?}]}. default_tile is the tile shown when nothing matches (preset: first_tile)."}),
                        ),
                    ],
                    &["path", "rule_tile"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("rule_tile", json!({"type":"integer"})),
                    ("rules", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                ]),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            define_rules,
        ),
        tool(
            Spec {
                name: "tilemap_paint",
                title: "Paint tilemap",
                description: "Apply up to 256 ordered edits to one layer of an existing tilemap file, all or nothing: `set` a cell, `erase` a cell, `rect` fill (min inclusive, max exclusive, tile 0 clears), `flood` fill the connected same-tile region (bounded to 65536 cells and the optional limit rectangle), or `layer` to create or update a layer. Rule tiles are validated against the tileset and re-resolve only the affected neighbours when queried or rendered. The file is rewritten atomically.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Existing tilemap asset id or project-relative file."),
                        ),
                        (
                            "tileset",
                            schema::string(
                                "Tileset asset id (or file) the map uses; tile ids are checked against it.",
                            ),
                        ),
                        (
                            "layer",
                            schema::string("Layer name for set/erase/rect/flood edits."),
                        ),
                        (
                            "ops",
                            schema::array_of(
                                "Edits: {op:set|erase|rect|flood|layer, cell?:[x,y], rect?:{min,max}, tile?, flags?, limit?:{min,max}, name?, z_order?, visible?, opacity?}. flags are orientation bits flip_x=1, flip_y=2, transpose=4.",
                                json!({"type":"object"}),
                                MAX_OPS,
                            ),
                        ),
                    ],
                    &["path", "tileset", "ops"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("changed", json!({"type":"integer"})),
                    ("refreshed", json!({"type":"integer"})),
                    ("dirty_chunks", json!({"type":"integer"})),
                    ("filled_cells", json!({"type":"integer"})),
                    ("hash", json!({"type":"string"})),
                ]),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            paint,
        ),
        tool(
            Spec {
                name: "tilemap_info",
                title: "Tilemap info",
                description: "Summarize a tileset or tilemap file (asset id or project-relative path): for a tileset its texture, tile and rule-tile counts, collision kinds and rule tiles; for a tilemap its bounds and per-layer z order, visibility, opacity, chunk and cell counts. Read-only.",
                input: schema::object(
                    vec![(
                        "path",
                        schema::string("Tileset or tilemap asset id, or project-relative file."),
                    )],
                    &["path"],
                ),
                output: schema::output(vec![
                    ("kind", json!({"type":"string"})),
                    ("details", json!({"type":"object"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            info,
        ),
        tool(
            Spec {
                name: "tilemap_query",
                title: "Query tilemap",
                description: "Query one layer of a saved tilemap against its tileset using the same rule resolution as the runtime. mode `cell` returns the painted and displayed tile, orientation and properties at a cell; `rect` lists up to 4096 non-empty cells in a rectangle; `collision` returns the merged collision boxes (cell units) and outline loops the runtime would generate. Read-only.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            schema::string("Tilemap asset id or project-relative file."),
                        ),
                        (
                            "tileset",
                            schema::string("Tileset asset id (or file) the map uses."),
                        ),
                        ("layer", schema::string("Layer name.")),
                        (
                            "mode",
                            schema::string_enum("What to return.", &["cell", "rect", "collision"]),
                        ),
                        (
                            "cell",
                            int_vector(2, -1_000_000, 1_000_000, "Cell [x, y] for mode cell."),
                        ),
                        (
                            "rect",
                            rect_schema(
                                "Cell rectangle for mode rect: min inclusive, max exclusive.",
                            ),
                        ),
                    ],
                    &["path", "tileset", "layer", "mode"],
                ),
                output: schema::output(vec![
                    ("mode", json!({"type":"string"})),
                    ("cell", json!({"type":"object"})),
                    ("cells", json!({"type":"array"})),
                    ("boxes", json!({"type":"array"})),
                    ("outlines", json!({"type":"array"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            query,
        ),
    ]
}

fn described(mut schema: Value, description: &str) -> Value {
    schema["description"] = json!(description);
    schema
}
fn int_vector(n: usize, min: i64, max: i64, description: &str) -> Value {
    json!({"type":"array","minItems":n,"maxItems":n,"items":{"type":"integer","minimum":min,"maximum":max},"description":description})
}
fn int_vector_unsigned(n: usize, description: &str) -> Value {
    int_vector(n, 0, 1_000_000, description)
}
fn rect_schema(description: &str) -> Value {
    json!({"type":"object","description":description,"additionalProperties":false,
        "properties":{"min":int_vector(2,-1_000_000,1_000_000,"Inclusive minimum cell [x, y]."),
                      "max":int_vector(2,-1_000_000,1_000_000,"Exclusive maximum cell [x, y].")},
        "required":["min","max"]})
}

fn failure(error: engine_tilemap::Error) -> ToolFailure {
    ToolFailure::new(error.code, error.message)
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value, field: &str) -> Result<T, ToolFailure> {
    serde_json::from_value(value.clone())
        .map_err(|e| schema::invalid(field, &format!("valid tilemap data ({e})")))
}
fn host_failure(error: engine_project::Error) -> ToolFailure {
    ToolFailure::from(error)
}

/// Resolves an asset id or a project-relative file to an existing canonical file.
fn target(
    cx: &ToolContext<'_>,
    project: &Project,
    field: &str,
    value: &str,
) -> Result<PathBuf, ToolFailure> {
    if project.assets.contains_key(value) {
        return project.resolve_asset(value).map_err(host_failure);
    }
    cx.existing_file(field, &json!(value))
}
fn read_target(path: &Path) -> Result<Vec<u8>, ToolFailure> {
    let length = std::fs::metadata(path)
        .map_err(|e| host_failure(e.into()))?
        .len();
    if length > MAX_FILE {
        return Err(ToolFailure::new(
            "file_too_large",
            "document exceeds 16 MiB",
        ));
    }
    std::fs::read(path).map_err(|e| host_failure(e.into()))
}
fn load_tileset_arg(
    cx: &ToolContext<'_>,
    project: &Project,
    field: &str,
    value: &str,
) -> Result<Tileset, ToolFailure> {
    if project.assets.contains_key(value) {
        return host::load_tileset(project, value)
            .map(|(tileset, _)| tileset)
            .map_err(host_failure);
    }
    let path = target(cx, project, field, value)?;
    Tileset::from_json(&read_target(&path)?).map_err(failure)
}
fn load_map_arg(path: &Path, tileset: &Tileset) -> Result<Tilemap, ToolFailure> {
    Tilemap::from_json(&read_target(path)?, Some(tileset)).map_err(failure)
}

/// Writes a new file next to existing directories; never replaces anything.
fn publish_new(cx: &ToolContext<'_>, output: &str, bytes: &[u8]) -> Result<(), ToolFailure> {
    let checked = paths::resolve_write_target(cx.root, output, "output")?;
    if checked.exists
        || checked.needs_directories
        || checked.path.extension().and_then(|s| s.to_str()) != Some("json")
    {
        return Err(schema::invalid(
            "output",
            "a new .json file in an existing project directory",
        ));
    }
    if bytes.len() as u64 > MAX_FILE {
        return Err(ToolFailure::new(
            "file_too_large",
            "document exceeds 16 MiB",
        ));
    }
    let parent = checked
        .path
        .parent()
        .ok_or_else(|| schema::invalid("output", "a project file"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| host_failure(e.into()))?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| host_failure(e.into()))?;
    let target = paths::resolve_write_target(cx.root, output, "output")?;
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
                return Err(host_failure(engine_project::Error::new(
                    "tilemap.publish",
                    error.error.to_string(),
                )));
            }
        }
    }
}
fn rewrite(path: &Path, bytes: &[u8]) -> Result<(), ToolFailure> {
    paths::atomic_write(path, bytes).map_err(|e| host_failure(e.into()))
}

// ---- tilemap_create_tileset ---------------------------------------------------------------

fn create_tileset(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let output = a.string("output")?;
    let texture = a.string("texture")?;
    let tile_size: [u32; 2] = decode(args.get("tile_size").unwrap_or(&Value::Null), "tile_size")?;
    let project = cx.project()?;
    let size = host::texture_size(&project, texture).map_err(host_failure)?;
    let mut doc =
        TilesetDocument::new(a.optional_string("name")?.unwrap_or(""), texture, tile_size);
    if let Some(tiles) = args.get("tiles").filter(|v| !v.is_null()) {
        doc.tiles = decode::<Vec<TileDef>>(tiles, "tiles")?;
    }
    if let Some(grid) = a.optional_object("grid")? {
        let grid_args = Args(grid);
        let first = grid_args.integer("first_id", 1, 16_777_215, 1)? as u32;
        let columns = grid_args.integer("columns", 1, 256, 0)? as u32;
        let rows = grid_args.integer("rows", 1, 256, 0)? as u32;
        if columns == 0 || rows == 0 {
            return Err(schema::invalid("grid", "columns and rows of at least 1"));
        }
        let pair = |name: &str| -> Result<[u32; 2], ToolFailure> {
            grid.get(name)
                .map_or(Ok([0, 0]), |v| decode(v, &format!("grid.{name}")))
        };
        let (offset, spacing) = (pair("offset")?, pair("spacing")?);
        let kind = match grid_args.one_of("collision", &["none", "full"])? {
            Some("full") => CollisionKind::Full,
            _ => CollisionKind::None,
        };
        for index in 0..columns * rows {
            let (column, row) = (index % columns, index / columns);
            doc.tiles.push(TileDef {
                id: first + index,
                name: String::new(),
                source: [
                    offset[0] + column * (tile_size[0] + spacing[0]),
                    offset[1] + row * (tile_size[1] + spacing[1]),
                    tile_size[0],
                    tile_size[1],
                ],
                collision: kind.clone(),
                properties: BTreeMap::new(),
            });
        }
    }
    if doc.tiles.is_empty() {
        return Err(schema::invalid("tiles", "tiles or a grid"));
    }
    let tileset = Tileset::from_document(doc).map_err(failure)?;
    tileset
        .validate_texture(size[0], size[1])
        .map_err(failure)?;
    let bytes = tileset.to_json().into_bytes();
    publish_new(cx, output, &bytes)?;
    Ok(ToolResult::new(json!({
        "path": output,
        "tiles": tileset.document().tiles.len(),
        "hash": paths::content_hash(&bytes),
        "asset": {"id":"choose-a-unique-tileset-id","kind":"tileset","path":output},
    })))
}

// ---- tilemap_create -----------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerArg {
    name: String,
    z_order: Option<i32>,
    visible: Option<bool>,
    opacity: Option<f32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RectArg {
    min: [i32; 2],
    max: [i32; 2],
}
impl From<RectArg> for CellRect {
    fn from(rect: RectArg) -> Self {
        CellRect::new(rect.min, rect.max)
    }
}

fn create_tilemap(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let output = a.string("output")?;
    let tileset_ref = a.string("tileset")?;
    let project = cx.project()?;
    let _ = load_tileset_arg(cx, &project, "tileset", tileset_ref)?;
    let bounds = match args.get("bounds").filter(|v| !v.is_null()) {
        Some(value) => CellRect::from(decode::<RectArg>(value, "bounds")?),
        None => CellRect::default(),
    };
    let mut map = Tilemap::new(bounds).map_err(failure)?;
    if let Some(layers) = args.get("layers").filter(|v| !v.is_null()) {
        let layers: Vec<LayerArg> = decode(layers, "layers")?;
        if layers.is_empty() {
            return Err(schema::invalid("layers", "at least one layer"));
        }
        map.remove_layer("ground");
        for layer in layers {
            let target = map.add_layer(&layer.name).map_err(failure)?;
            apply_layer(target, layer.z_order, layer.visible, layer.opacity)?;
        }
    }
    let bytes = map.to_json().into_bytes();
    publish_new(cx, output, &bytes)?;
    let names: Vec<_> = map.layers().iter().map(|l| l.name.clone()).collect();
    Ok(ToolResult::new(json!({
        "path": output,
        "layers": names,
        "hash": paths::content_hash(&bytes),
        "asset": {"id":"choose-a-unique-tilemap-id","kind":"tilemap","path":output},
        "component": {"engine.Tilemap":{"version":1,"data":{"tileset":tileset_ref,"map":"choose-a-unique-tilemap-id"}}},
    })))
}
fn apply_layer(
    layer: &mut engine_tilemap::TileLayer,
    z_order: Option<i32>,
    visible: Option<bool>,
    opacity: Option<f32>,
) -> Result<(), ToolFailure> {
    if let Some(z) = z_order {
        layer.z_order = z;
    }
    if let Some(v) = visible {
        layer.visible = v;
    }
    if let Some(o) = opacity {
        if !(0.0..=1.0).contains(&o) {
            return Err(schema::invalid("opacity", "a number from 0 to 1"));
        }
        layer.opacity = o;
    }
    Ok(())
}

// ---- tilemap_define_rules -----------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleArg {
    id: TileId,
    name: Option<String>,
    preset: Option<String>,
    first_tile: Option<TileId>,
    default_tile: Option<TileId>,
    #[serde(default)]
    connects: Vec<TileId>,
    #[serde(default)]
    rules: Vec<Rule>,
}

fn define_rules(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let reference = a.string("path")?;
    let project = cx.project()?;
    let file = target(cx, &project, "path", reference)?;
    let tileset = Tileset::from_json(&read_target(&file)?).map_err(failure)?;
    let arg: RuleArg = decode(args.get("rule_tile").unwrap_or(&Value::Null), "rule_tile")?;
    let name = arg.name.as_deref().unwrap_or("");
    let mut rule = match (arg.preset.as_deref(), arg.first_tile) {
        (Some("blob47"), Some(first)) => RuleTile::blob47(arg.id, name, first),
        (Some("four_neighbour16"), Some(first)) => RuleTile::four_neighbour16(arg.id, name, first),
        (Some(_), None) => return Err(schema::invalid("rule_tile", "first_tile with a preset")),
        (Some(_), Some(_)) => {
            return Err(schema::invalid(
                "rule_tile",
                "preset blob47 or four_neighbour16",
            ));
        }
        (None, _) => {
            let default = arg
                .default_tile
                .ok_or_else(|| schema::invalid("rule_tile", "default_tile or a preset"))?;
            RuleTile::new(arg.id, name, default)
        }
    };
    if let Some(default) = arg.default_tile {
        rule.default_tile = default;
    }
    rule.connects = arg.connects;
    if !arg.rules.is_empty() {
        let mut combined = arg.rules;
        combined.append(&mut rule.rules);
        rule.rules = combined;
    }
    let (id, rules) = (rule.id, rule.rules.len());
    if rules == 0 {
        return Err(schema::invalid(
            "rule_tile",
            "a preset or at least one rule",
        ));
    }
    let mut updated = tileset;
    updated.set_rule_tile(rule).map_err(failure)?;
    let bytes = updated.to_json().into_bytes();
    rewrite(&file, &bytes)?;
    Ok(ToolResult::new(json!({
        "path": cx.relative(&file),
        "rule_tile": id,
        "rules": rules,
        "hash": paths::content_hash(&bytes),
    })))
}

// ---- tilemap_paint ------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaintOp {
    op: String,
    cell: Option<[i32; 2]>,
    rect: Option<RectArg>,
    tile: Option<TileId>,
    flags: Option<u8>,
    limit: Option<RectArg>,
    name: Option<String>,
    z_order: Option<i32>,
    visible: Option<bool>,
    opacity: Option<f32>,
}

fn paint(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, "path", a.string("path")?)?;
    let tileset = load_tileset_arg(cx, &project, "tileset", a.string("tileset")?)?;
    let mut map = load_map_arg(&file, &tileset)?;
    let ops: Vec<PaintOp> = decode(args.get("ops").unwrap_or(&Value::Null), "ops")?;
    if ops.is_empty() || ops.len() > MAX_OPS {
        return Err(schema::invalid("ops", "1 to 256 edits"));
    }
    let default_layer = a.optional_string("layer")?;
    let (mut changed, mut refreshed) = (0usize, 0usize);
    let mut dirty = std::collections::BTreeSet::new();
    let mut touched = std::collections::BTreeSet::new();
    for (index, op) in ops.into_iter().enumerate() {
        let wrap = |e: engine_tilemap::Error| {
            ToolFailure::new(e.code, format!("ops[{index}] ({}): {}", op.op, e.message))
        };
        if op.op == "layer" {
            let name = op
                .name
                .as_deref()
                .ok_or_else(|| schema::invalid("ops", "a layer op with name"))?;
            if map.layer(name).is_none() {
                map.add_layer(name).map_err(wrap)?;
            }
            let layer = map.layer_mut(name).expect("exists");
            apply_layer(layer, op.z_order, op.visible, op.opacity)?;
            continue;
        }
        let layer_name = default_layer.ok_or_else(|| schema::invalid("layer", "a layer name"))?;
        let layer = map.layer_mut(layer_name).ok_or_else(|| {
            ToolFailure::new(
                "unknown_layer",
                format!("layer {layer_name} does not exist"),
            )
            .with_hint("Create it with a `layer` op or tilemap_create.")
        })?;
        let flags = op.flags.unwrap_or(0);
        let need_cell = || {
            op.cell
                .ok_or_else(|| schema::invalid("ops", "a cell [x, y] for this op"))
        };
        let report = match op.op.as_str() {
            "set" => {
                let tile = op
                    .tile
                    .ok_or_else(|| schema::invalid("ops", "a tile for set"))?;
                layer
                    .set(need_cell()?, tile, flags, Some(&tileset))
                    .map_err(wrap)?
            }
            "erase" => layer.erase(need_cell()?, Some(&tileset)).map_err(wrap)?,
            "rect" => {
                let rect = op
                    .rect
                    .ok_or_else(|| schema::invalid("ops", "a rect for rect"))?;
                let tile = op.tile.unwrap_or(0);
                layer
                    .fill_rect(rect.into(), tile, flags, Some(&tileset))
                    .map_err(wrap)?
            }
            "flood" => {
                let tile = op
                    .tile
                    .ok_or_else(|| schema::invalid("ops", "a tile for flood"))?;
                layer
                    .flood_fill(
                        need_cell()?,
                        tile,
                        flags,
                        op.limit.map(CellRect::from),
                        Some(&tileset),
                    )
                    .map_err(wrap)?
            }
            _ => {
                return Err(schema::invalid(
                    "ops",
                    "op one of set, erase, rect, flood, layer",
                ));
            }
        };
        changed += report.changed;
        refreshed += report.refreshed;
        dirty.extend(
            report
                .dirty_chunks
                .iter()
                .map(|c| (layer_name.to_owned(), *c)),
        );
        touched.insert(layer_name.to_owned());
    }
    let bytes = map.to_json().into_bytes();
    if bytes.len() as u64 > MAX_FILE {
        return Err(ToolFailure::new(
            "file_too_large",
            "document exceeds 16 MiB",
        ));
    }
    rewrite(&file, &bytes)?;
    let filled = default_layer
        .and_then(|name| map.layer(name))
        .map_or(0, |l| l.filled_cells());
    Ok(ToolResult::new(json!({
        "path": cx.relative(&file),
        "changed": changed,
        "refreshed": refreshed,
        "dirty_chunks": dirty.len(),
        "filled_cells": filled,
        "hash": paths::content_hash(&bytes),
    })))
}

// ---- tilemap_info -------------------------------------------------------------------------

fn info(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let project = cx.project()?;
    let file = target(cx, &project, "path", Args(args).string("path")?)?;
    let bytes = read_target(&file)?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ToolFailure::new("tilemap.malformed", e.to_string()))?;
    if value.get("tiles").is_some() {
        let tileset = Tileset::from_json(&bytes).map_err(failure)?;
        let doc = tileset.document();
        let mut kinds = BTreeMap::<&str, usize>::new();
        for tile in &doc.tiles {
            let kind = match tile.collision {
                CollisionKind::None => "none",
                CollisionKind::Full => "full",
                CollisionKind::Rect { .. } => "rect",
                CollisionKind::Polygon { .. } => "polygon",
            };
            *kinds.entry(kind).or_default() += 1;
        }
        let rule_tiles: Vec<_> = doc
            .rule_tiles
            .iter()
            .map(|r| {
                json!({"id":r.id,"name":r.name,"default_tile":r.default_tile,
                    "rules":r.rules.len(),"connects":r.connects})
            })
            .collect();
        return Ok(ToolResult::new(json!({"kind":"tileset","details":{
            "name":doc.name,"texture":doc.texture,"tile_size":doc.tile_size,
            "tiles":doc.tiles.len(),"collision":kinds,"rule_tiles":rule_tiles,
            "hash":paths::content_hash(&bytes)}})));
    }
    let map = Tilemap::from_json(&bytes, None).map_err(failure)?;
    let layers: Vec<_> = map
        .layers()
        .iter()
        .map(|l| {
            json!({"name":l.name,"z_order":l.z_order,"visible":l.visible,"opacity":l.opacity,
                "chunks":l.chunk_keys().len(),"cells":l.filled_cells()})
        })
        .collect();
    Ok(ToolResult::new(json!({"kind":"tilemap","details":{
        "bounds":{"min":map.bounds().min,"max":map.bounds().max},
        "layers":layers,"hash":paths::content_hash(&bytes)}})))
}

// ---- tilemap_query ------------------------------------------------------------------------

fn query(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let project = cx.project()?;
    let file = target(cx, &project, "path", a.string("path")?)?;
    let tileset = load_tileset_arg(cx, &project, "tileset", a.string("tileset")?)?;
    let map = load_map_arg(&file, &tileset)?;
    let layer_name = a.string("layer")?;
    let layer = map.layer(layer_name).ok_or_else(|| {
        ToolFailure::new(
            "unknown_layer",
            format!("layer {layer_name} does not exist"),
        )
    })?;
    let describe = |cell: engine_tilemap::Cell| {
        let concrete = tileset.concrete(cell.shown);
        json!({"tile":cell.tile,"flags":cell.flags,"shown":cell.shown,"shown_flags":cell.shown_flags,
            "name":concrete.map(|t| t.name.clone()),
            "collision":concrete.map(|t| &t.collision),
            "properties":concrete.map(|t| &t.properties)})
    };
    match a.one_of("mode", &["cell", "rect", "collision"])? {
        Some("cell") => {
            let cell: [i32; 2] = decode(args.get("cell").unwrap_or(&Value::Null), "cell")?;
            let mut result = describe(layer.get(cell));
            result["cell"] = json!(cell);
            Ok(ToolResult::new(json!({"mode":"cell","cell":result})))
        }
        Some("rect") => {
            let rect: RectArg = decode(args.get("rect").unwrap_or(&Value::Null), "rect")?;
            let rect = CellRect::from(rect);
            let mut cells = Vec::new();
            'outer: for key in layer.chunk_keys() {
                for (pos, cell) in layer.chunk_cells(key) {
                    if rect.contains(pos) {
                        if cells.len() >= MAX_QUERY_CELLS {
                            break 'outer;
                        }
                        let mut entry = describe(cell);
                        entry["cell"] = json!(pos);
                        cells.push(entry);
                    }
                }
            }
            cells.sort_by_key(|c| (c["cell"][1].as_i64(), c["cell"][0].as_i64()));
            Ok(ToolResult::new(json!({"mode":"rect","cells":cells})))
        }
        Some("collision") => {
            let boxes: Vec<_> = collision::collision_boxes(layer, &tileset)
                .into_iter()
                .map(|b| json!({"min":b.min,"max":b.max}))
                .collect();
            let outlines = collision::collision_outlines(layer, &tileset);
            Ok(ToolResult::new(
                json!({"mode":"collision","boxes":boxes,"outlines":outlines}),
            ))
        }
        _ => Err(schema::invalid("mode", "one of cell, rect, collision")),
    }
}
