//! Tilesets: atlas rectangles, per-tile collision and properties, plus rule tiles.
use crate::error::{Error, Result};
use crate::orient;
use crate::rules::{CompiledRule, RuleTile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Tile identifier. `0` is the empty cell; usable ids are `1..=MAX_TILE_ID`.
pub type TileId = u32;
/// Largest tile id (ids share a `u32` with orientation flags in the file format).
pub const MAX_TILE_ID: TileId = (1 << 24) - 1;
/// Format version written and accepted.
pub const TILESET_VERSION: u32 = 1;
/// Maximum concrete tiles in one tileset.
pub const MAX_TILES: usize = 4096;
/// Maximum rule tiles in one tileset.
pub const MAX_RULE_TILES: usize = 256;
/// Maximum rules (before rotation/mirror expansion) in one rule tile.
pub const MAX_RULES: usize = 256;
const MAX_POLYGON_POINTS: usize = 64;
const MAX_PROPERTIES: usize = 64;

/// How a tile blocks movement. Rectangle and polygon coordinates are fractions of the cell,
/// origin at the bottom-left, y up, within `0..=1`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionKind {
    /// Does not block.
    #[default]
    None,
    /// Blocks the whole cell; adjacent full cells merge into larger rectangles.
    Full,
    /// Blocks an axis-aligned sub-rectangle of the cell.
    Rect {
        /// Bottom-left corner.
        min: [f32; 2],
        /// Top-right corner.
        max: [f32; 2],
    },
    /// Blocks a polygon. The axis-aligned solver uses its bounding box; outlines keep the polygon.
    Polygon {
        /// Vertices (3-64).
        points: Vec<[f32; 2]>,
    },
}

/// A free-form tile property value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    /// Boolean flag.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Real number.
    Float(f64),
    /// Text.
    Text(String),
}

/// One concrete tile: where it sits in the atlas and how it collides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TileDef {
    /// Unique non-zero id.
    pub id: TileId,
    /// Display name (up to 64 characters).
    #[serde(default)]
    pub name: String,
    /// Atlas rectangle `[x, y, width, height]` in pixels, top-left origin.
    pub source: [u32; 4],
    /// Collision kind.
    #[serde(default)]
    pub collision: CollisionKind,
    /// Author properties (up to 64).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, PropertyValue>,
}

/// The plain serialized form of a tileset (`version` 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TilesetDocument {
    /// Format version; must be 1.
    pub version: u32,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Texture asset id that holds the atlas.
    pub texture: String,
    /// Nominal tile size in pixels (informational; each tile has its own source rectangle).
    pub tile_size: [u32; 2],
    /// Concrete tiles.
    #[serde(default)]
    pub tiles: Vec<TileDef>,
    /// Rule tiles that pick one of the concrete tiles from their neighbours.
    #[serde(default)]
    pub rule_tiles: Vec<RuleTile>,
}
impl TilesetDocument {
    /// An empty document for `texture`.
    pub fn new(name: &str, texture: &str, tile_size: [u32; 2]) -> Self {
        Self {
            version: TILESET_VERSION,
            name: name.into(),
            texture: texture.into(),
            tile_size,
            tiles: Vec::new(),
            rule_tiles: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Slot {
    Tile(usize),
    Rule(usize),
}

/// A validated tileset with id lookup tables and compiled rules.
#[derive(Clone, Debug)]
pub struct Tileset {
    doc: TilesetDocument,
    index: BTreeMap<TileId, Slot>,
    compiled: Vec<Vec<CompiledRule>>,
}
impl Tileset {
    /// Validates `doc` and builds lookups.
    pub fn from_document(doc: TilesetDocument) -> Result<Self> {
        let bad = |m: String| Error::new("tileset.invalid", m);
        if doc.version != TILESET_VERSION {
            return Err(bad(format!("unsupported tileset version {}", doc.version)));
        }
        if doc.name.chars().count() > 128 || doc.texture.is_empty() || doc.texture.len() > 128 {
            return Err(bad("name or texture id out of range".into()));
        }
        if doc.tile_size.contains(&0) || doc.tile_size.iter().any(|v| *v > 4096) {
            return Err(bad("tile_size must be 1..=4096".into()));
        }
        if doc.tiles.len() > MAX_TILES || doc.rule_tiles.len() > MAX_RULE_TILES {
            return Err(bad("too many tiles or rule tiles".into()));
        }
        let mut index = BTreeMap::new();
        for (i, tile) in doc.tiles.iter().enumerate() {
            if tile.id == 0
                || tile.id > MAX_TILE_ID
                || index.insert(tile.id, Slot::Tile(i)).is_some()
            {
                return Err(bad(format!(
                    "tile id {} is zero, too large or duplicated",
                    tile.id
                )));
            }
            validate_tile(tile)?;
        }
        for (i, rule) in doc.rule_tiles.iter().enumerate() {
            if rule.id == 0
                || rule.id > MAX_TILE_ID
                || index.insert(rule.id, Slot::Rule(i)).is_some()
            {
                return Err(bad(format!(
                    "rule tile id {} is zero, too large or duplicated",
                    rule.id
                )));
            }
        }
        let mut compiled = Vec::new();
        for rule in &doc.rule_tiles {
            compiled.push(rule.compile(|id| matches!(index.get(&id), Some(Slot::Tile(_))))?);
        }
        Ok(Self {
            doc,
            index,
            compiled,
        })
    }
    /// Parses and validates a tileset JSON file (at most 16 MiB).
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::new("tileset.invalid", "document exceeds 16 MiB"));
        }
        let doc = serde_json::from_slice(bytes)
            .map_err(|e| Error::new("tileset.malformed", e.to_string()))?;
        Self::from_document(doc)
    }
    /// Pretty JSON for the document.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.doc).expect("tileset documents always serialize")
    }
    /// The serializable document.
    pub fn document(&self) -> &TilesetDocument {
        &self.doc
    }
    /// Atlas texture asset id.
    pub fn texture(&self) -> &str {
        &self.doc.texture
    }
    /// Adds a concrete tile (revalidates everything).
    pub fn add_tile(&mut self, tile: TileDef) -> Result<()> {
        let mut doc = self.doc.clone();
        doc.tiles.push(tile);
        *self = Self::from_document(doc)?;
        Ok(())
    }
    /// Adds or replaces a rule tile with the same id (revalidates everything).
    pub fn set_rule_tile(&mut self, rule: RuleTile) -> Result<()> {
        let mut doc = self.doc.clone();
        doc.rule_tiles.retain(|r| r.id != rule.id);
        doc.rule_tiles.push(rule);
        *self = Self::from_document(doc)?;
        Ok(())
    }
    /// The concrete tile `id`, if it is one.
    pub fn tile(&self, id: TileId) -> Option<&TileDef> {
        match self.index.get(&id)? {
            Slot::Tile(i) => self.doc.tiles.get(*i),
            Slot::Rule(_) => None,
        }
    }
    /// The rule tile `id`, if it is one.
    pub fn rule_tile(&self, id: TileId) -> Option<&RuleTile> {
        match self.index.get(&id)? {
            Slot::Rule(i) => self.doc.rule_tiles.get(*i),
            Slot::Tile(_) => None,
        }
    }
    /// The concrete tile displayed for `id`: the tile itself, or a rule tile's default.
    pub fn concrete(&self, id: TileId) -> Option<&TileDef> {
        self.tile(id)
            .or_else(|| self.rule_tile(id).and_then(|r| self.tile(r.default_tile)))
    }
    /// Whether `id` is a known tile or rule tile.
    pub fn contains(&self, id: TileId) -> bool {
        self.index.contains_key(&id)
    }
    /// Picks the tile and orientation flags for rule tile `id`. `is_same(offset)` answers whether
    /// the neighbour at `offset` (y-up) connects to this rule tile. Returns `None` for non-rule ids.
    pub fn resolve_rule(
        &self,
        id: TileId,
        mut is_same: impl FnMut([i32; 2]) -> bool,
    ) -> Option<(TileId, u8)> {
        let Some(Slot::Rule(i)) = self.index.get(&id) else {
            return None;
        };
        let rule = &self.doc.rule_tiles[*i];
        let mut neighbours = [false; 8];
        for (k, offset) in crate::rules::OFFSETS.iter().enumerate() {
            neighbours[k] = is_same(*offset);
        }
        for compiled in &self.compiled[*i] {
            if compiled.matches(&neighbours) {
                return Some((compiled.output, compiled.flags));
            }
        }
        Some((rule.default_tile, 0))
    }
    /// Whether neighbour tile `other` connects to rule tile `rule_id`.
    pub fn connects(&self, rule_id: TileId, other: TileId) -> bool {
        other != 0
            && (other == rule_id
                || self
                    .rule_tile(rule_id)
                    .is_some_and(|r| r.connects.contains(&other)))
    }
    /// All tile ids (concrete and rule), ascending.
    pub fn ids(&self) -> Vec<TileId> {
        self.index.keys().copied().collect()
    }
    /// Checks every atlas rectangle fits a texture of `width` x `height` pixels.
    pub fn validate_texture(&self, width: u32, height: u32) -> Result<()> {
        for tile in &self.doc.tiles {
            let [x, y, w, h] = tile.source;
            if x.checked_add(w).is_none_or(|v| v > width)
                || y.checked_add(h).is_none_or(|v| v > height)
            {
                return Err(Error::new(
                    "tileset.texture",
                    format!(
                        "tile {} source exceeds the {width}x{height} texture",
                        tile.id
                    ),
                ));
            }
        }
        Ok(())
    }
}
fn validate_tile(tile: &TileDef) -> Result<()> {
    let bad = |m: &str| Error::new("tileset.invalid", format!("tile {}: {m}", tile.id));
    if tile.name.chars().count() > 64 || tile.source[2] == 0 || tile.source[3] == 0 {
        return Err(bad("name too long or empty source rectangle"));
    }
    if tile.properties.len() > MAX_PROPERTIES
        || tile.properties.keys().any(|k| k.is_empty() || k.len() > 64)
        || tile.properties.values().any(|v| match v {
            PropertyValue::Text(t) => t.len() > 256,
            PropertyValue::Float(f) => !f.is_finite(),
            _ => false,
        })
    {
        return Err(bad("invalid properties"));
    }
    let unit = |v: f32| (0.0..=1.0).contains(&v);
    let invalid_shape = match &tile.collision {
        CollisionKind::Rect { min, max } => {
            !min.iter().chain(max).all(|v| unit(*v)) || min[0] >= max[0] || min[1] >= max[1]
        }
        CollisionKind::Polygon { points } => {
            !(3..=MAX_POLYGON_POINTS).contains(&points.len())
                || !points.iter().flatten().all(|v| unit(*v))
        }
        _ => false,
    };
    if invalid_shape {
        return Err(bad(
            "custom collision must be non-empty, within 0..=1, polygons 3-64 points",
        ));
    }
    Ok(())
}
impl CollisionKind {
    /// Cell-fraction polygon of a custom (`Rect` or `Polygon`) shape after applying orientation
    /// `flags`; `None` for `None` and `Full`.
    pub fn oriented_points(&self, flags: u8) -> Option<Vec<[f32; 2]>> {
        let raw: Vec<[f32; 2]> = match self {
            CollisionKind::Rect { min, max } => {
                vec![*min, [max[0], min[1]], *max, [min[0], max[1]]]
            }
            CollisionKind::Polygon { points } => points.clone(),
            _ => return None,
        };
        Some(
            raw.into_iter()
                .map(|p| {
                    let o = orient::apply(flags, [p[0] - 0.5, p[1] - 0.5]);
                    [o[0] + 0.5, o[1] + 0.5]
                })
                .collect(),
        )
    }
}
