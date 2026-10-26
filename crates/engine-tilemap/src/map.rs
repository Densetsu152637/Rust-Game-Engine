//! Sparse chunked tile layers, painting operations and the validated map document.
use crate::error::{Error, Result};
use crate::orient;
use crate::tileset::{MAX_TILE_ID, TileId, Tileset};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Cells per chunk edge.
pub const CHUNK_SIZE: i32 = 16;
const CHUNK_CELLS: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;
/// Format version written and accepted.
pub const TILEMAP_VERSION: u32 = 1;
/// Maximum layers in one map.
pub const MAX_LAYERS: usize = 32;
/// Maximum non-empty chunks in one layer (1,048,576 cells).
pub const MAX_CHUNKS_PER_LAYER: usize = 4096;
/// Largest edge of the cell bounds.
pub const MAX_BOUNDS_EDGE: i64 = 65_536;
/// Largest number of cells a single rectangle fill or flood fill may change.
pub const MAX_EDIT_CELLS: usize = 65_536;

/// A cell coordinate (x right, y up).
pub type CellPos = [i32; 2];
/// A chunk coordinate: `floor(cell / CHUNK_SIZE)`.
pub type ChunkKey = [i32; 2];

/// Chunk containing `cell`.
pub fn chunk_of(cell: CellPos) -> ChunkKey {
    [
        cell[0].div_euclid(CHUNK_SIZE),
        cell[1].div_euclid(CHUNK_SIZE),
    ]
}

/// Half-open cell rectangle `[min, max)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellRect {
    /// Inclusive minimum corner.
    pub min: CellPos,
    /// Exclusive maximum corner.
    pub max: CellPos,
}
impl CellRect {
    /// Rectangle from corners.
    pub fn new(min: CellPos, max: CellPos) -> Self {
        Self { min, max }
    }
    /// Whether `cell` lies inside.
    pub fn contains(&self, cell: CellPos) -> bool {
        (self.min[0]..self.max[0]).contains(&cell[0])
            && (self.min[1]..self.max[1]).contains(&cell[1])
    }
    /// Width and height in cells (zero when empty).
    pub fn size(&self) -> [i64; 2] {
        [
            (i64::from(self.max[0]) - i64::from(self.min[0])).max(0),
            (i64::from(self.max[1]) - i64::from(self.min[1])).max(0),
        ]
    }
    /// Number of cells.
    pub fn area(&self) -> i64 {
        let s = self.size();
        s[0] * s[1]
    }
    /// Overlap with `other` (possibly empty).
    pub fn intersect(&self, other: &CellRect) -> CellRect {
        CellRect {
            min: [self.min[0].max(other.min[0]), self.min[1].max(other.min[1])],
            max: [self.max[0].min(other.max[0]), self.max[1].min(other.max[1])],
        }
    }
}
impl Default for CellRect {
    /// Bounds of 1024 x 1024 cells around the origin.
    fn default() -> Self {
        Self {
            min: [-512, -512],
            max: [512, 512],
        }
    }
}

/// One map cell. `tile` is what the author painted (possibly a rule tile); `shown` is the
/// concrete tile that renders and collides after rule resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// Painted tile id, `0` when empty.
    pub tile: TileId,
    /// Painted orientation flags (see [`crate::orient`]).
    pub flags: u8,
    /// Concrete tile displayed.
    pub shown: TileId,
    /// Orientation of the displayed tile.
    pub shown_flags: u8,
}

struct Chunk {
    cells: Box<[Cell; CHUNK_CELLS]>,
    filled: u16,
    revision: u64,
}
fn slot(cell: CellPos) -> usize {
    (cell[1].rem_euclid(CHUNK_SIZE) * CHUNK_SIZE + cell[0].rem_euclid(CHUNK_SIZE)) as usize
}

/// What an editing operation touched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaintReport {
    /// Cells whose painted tile or flags changed.
    pub changed: usize,
    /// Cells whose displayed tile changed (includes `changed` and rule-resolved neighbours).
    pub refreshed: usize,
    /// Chunks whose content changed, ascending.
    pub dirty_chunks: Vec<ChunkKey>,
}

/// One layer of a map: sparse chunked cells plus presentation settings.
pub struct TileLayer {
    /// Unique name.
    pub name: String,
    /// Draw order; larger draws on top.
    pub z_order: i32,
    /// Whether the layer renders.
    pub visible: bool,
    /// Opacity in `0..=1`.
    pub opacity: f32,
    bounds: CellRect,
    chunks: BTreeMap<ChunkKey, Chunk>,
    counter: u64,
}

impl TileLayer {
    fn new(name: &str, bounds: CellRect) -> Self {
        Self {
            name: name.into(),
            z_order: 0,
            visible: true,
            opacity: 1.0,
            bounds,
            chunks: BTreeMap::new(),
            counter: 0,
        }
    }
    /// The cell bounds shared by the map.
    pub fn bounds(&self) -> CellRect {
        self.bounds
    }
    /// The cell at `cell`; empty outside the bounds.
    pub fn get(&self, cell: CellPos) -> Cell {
        self.chunks
            .get(&chunk_of(cell))
            .map(|c| c.cells[slot(cell)])
            .unwrap_or_default()
    }
    /// The painted tile at `cell` (`0` when empty).
    pub fn tile_at(&self, cell: CellPos) -> TileId {
        self.get(cell).tile
    }
    /// Number of non-empty cells.
    pub fn filled_cells(&self) -> usize {
        self.chunks.values().map(|c| c.filled as usize).sum()
    }
    /// Non-empty chunk keys, ascending.
    pub fn chunk_keys(&self) -> Vec<ChunkKey> {
        self.chunks.keys().copied().collect()
    }
    /// Change stamp of `key`; it increases whenever the chunk changes, and a chunk that is
    /// removed and recreated never reuses an older stamp.
    pub fn chunk_revision(&self, key: ChunkKey) -> Option<u64> {
        self.chunks.get(&key).map(|c| c.revision)
    }
    /// Non-empty cells of a chunk, row-major from the bottom.
    pub fn chunk_cells(&self, key: ChunkKey) -> Vec<(CellPos, Cell)> {
        let Some(chunk) = self.chunks.get(&key) else {
            return Vec::new();
        };
        let origin = [key[0] * CHUNK_SIZE, key[1] * CHUNK_SIZE];
        let mut out = Vec::with_capacity(chunk.filled as usize);
        for (i, cell) in chunk.cells.iter().enumerate() {
            if cell.tile != 0 {
                let i = i as i32;
                out.push((
                    [origin[0] + i % CHUNK_SIZE, origin[1] + i / CHUNK_SIZE],
                    *cell,
                ));
            }
        }
        out
    }
    /// All non-empty cells in chunk order.
    pub fn cells(&self) -> Vec<(CellPos, Cell)> {
        self.chunks
            .keys()
            .flat_map(|k| self.chunk_cells(*k))
            .collect()
    }

    fn check(&self, cell: CellPos, tile: TileId, flags: u8) -> Result<()> {
        if !self.bounds.contains(cell) {
            return Err(Error::new(
                "tilemap.bounds",
                format!("cell {cell:?} is outside the map bounds"),
            ));
        }
        if tile > MAX_TILE_ID || flags & !orient::MASK != 0 {
            return Err(Error::new("tilemap.tile", "tile id or flags out of range"));
        }
        Ok(())
    }
    fn write(&mut self, cell: CellPos, tile: TileId, flags: u8) -> Result<bool> {
        let key = chunk_of(cell);
        let old = self.get(cell);
        if old.tile == tile && (tile == 0 || old.flags == flags) {
            return Ok(false);
        }
        if tile != 0 && !self.chunks.contains_key(&key) && self.chunks.len() >= MAX_CHUNKS_PER_LAYER
        {
            return Err(Error::new(
                "tilemap.capacity",
                "layer chunk capacity exceeded",
            ));
        }
        self.counter += 1;
        let stamp = self.counter;
        let chunk = self.chunks.entry(key).or_insert_with(|| Chunk {
            cells: Box::new([Cell::default(); CHUNK_CELLS]),
            filled: 0,
            revision: 0,
        });
        let target = &mut chunk.cells[slot(cell)];
        if target.tile == 0 && tile != 0 {
            chunk.filled += 1;
        } else if target.tile != 0 && tile == 0 {
            chunk.filled -= 1;
        }
        *target = Cell {
            tile,
            flags: if tile == 0 { 0 } else { flags },
            shown: tile,
            shown_flags: if tile == 0 { 0 } else { flags },
        };
        chunk.revision = stamp;
        if chunk.filled == 0 {
            self.chunks.remove(&key);
        }
        Ok(true)
    }
    /// Re-picks the displayed tile of each rule-tile cell in `cells`. Plain tiles display as
    /// painted. Returns the cells whose display changed.
    fn refresh(&mut self, cells: &BTreeSet<CellPos>, tileset: Option<&Tileset>) -> Vec<CellPos> {
        let mut changed = Vec::new();
        for &cell in cells {
            let current = self.get(cell);
            if current.tile == 0 {
                continue;
            }
            let (shown, shown_flags) = match tileset {
                Some(ts) if ts.rule_tile(current.tile).is_some() => ts
                    .resolve_rule(current.tile, |o| {
                        ts.connects(current.tile, self.tile_at([cell[0] + o[0], cell[1] + o[1]]))
                    })
                    .expect("rule tile resolves"),
                _ => (current.tile, current.flags),
            };
            if (shown, shown_flags) != (current.shown, current.shown_flags) {
                self.counter += 1;
                let stamp = self.counter;
                let chunk = self.chunks.get_mut(&chunk_of(cell)).expect("chunk exists");
                let target = &mut chunk.cells[slot(cell)];
                target.shown = shown;
                target.shown_flags = shown_flags;
                chunk.revision = stamp;
                changed.push(cell);
            }
        }
        changed
    }
    fn finish(&mut self, written: Vec<CellPos>, tileset: Option<&Tileset>) -> PaintReport {
        let mut affected = BTreeSet::new();
        for cell in &written {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    affected.insert([cell[0] + dx, cell[1] + dy]);
                }
            }
        }
        let refreshed = self.refresh(&affected, tileset);
        let mut dirty: BTreeSet<ChunkKey> = written.iter().map(|c| chunk_of(*c)).collect();
        let mut shown_changed: BTreeSet<CellPos> = written.iter().copied().collect();
        for cell in refreshed {
            dirty.insert(chunk_of(cell));
            shown_changed.insert(cell);
        }
        PaintReport {
            changed: written.len(),
            refreshed: shown_changed.len(),
            dirty_chunks: dirty.into_iter().collect(),
        }
    }

    /// Paints one cell (`tile == 0` erases). Rule tiles resolve with `tileset`, and the eight
    /// neighbours re-resolve; no other cell is touched.
    pub fn set(
        &mut self,
        cell: CellPos,
        tile: TileId,
        flags: u8,
        tileset: Option<&Tileset>,
    ) -> Result<PaintReport> {
        self.check(cell, tile, flags)?;
        self.check_known(tile, tileset)?;
        let written = if self.write(cell, tile, flags)? {
            vec![cell]
        } else {
            Vec::new()
        };
        Ok(self.finish(written, tileset))
    }
    /// Erases one cell.
    pub fn erase(&mut self, cell: CellPos, tileset: Option<&Tileset>) -> Result<PaintReport> {
        self.set(cell, 0, 0, tileset)
    }
    fn check_known(&self, tile: TileId, tileset: Option<&Tileset>) -> Result<()> {
        if let Some(ts) = tileset
            && tile != 0
            && !ts.contains(tile)
        {
            return Err(Error::new(
                "tilemap.tile",
                format!("tile {tile} is not in the tileset"),
            ));
        }
        Ok(())
    }
    /// Fills `rect` (clipped nowhere: it must lie inside the bounds and hold at most
    /// [`MAX_EDIT_CELLS`] cells). `tile == 0` clears the rectangle.
    pub fn fill_rect(
        &mut self,
        rect: CellRect,
        tile: TileId,
        flags: u8,
        tileset: Option<&Tileset>,
    ) -> Result<PaintReport> {
        let area = rect.area();
        if rect.min[0] >= rect.max[0]
            || rect.min[1] >= rect.max[1]
            || rect.intersect(&self.bounds) != rect
            || area > MAX_EDIT_CELLS as i64
        {
            return Err(Error::new(
                "tilemap.fill",
                "rectangle must be non-empty, inside the bounds and at most 65536 cells",
            ));
        }
        self.check(rect.min, tile, flags)?;
        self.check_known(tile, tileset)?;
        // Writing may fail on capacity part-way; snapshot the touched chunks to stay atomic.
        let backup = self.snapshot_chunks(&rect);
        let mut written = Vec::new();
        for y in rect.min[1]..rect.max[1] {
            for x in rect.min[0]..rect.max[0] {
                match self.write([x, y], tile, flags) {
                    Ok(true) => written.push([x, y]),
                    Ok(false) => {}
                    Err(error) => {
                        self.restore_chunks(&rect, backup);
                        return Err(error);
                    }
                }
            }
        }
        Ok(self.finish(written, tileset))
    }
    /// Flood-fills the 4-connected region of cells that hold the same painted tile as `seed`,
    /// restricted to `limit` (or the whole bounds). The region may hold at most
    /// [`MAX_EDIT_CELLS`] cells; otherwise nothing changes and `tilemap.fill` is returned.
    pub fn flood_fill(
        &mut self,
        seed: CellPos,
        tile: TileId,
        flags: u8,
        limit: Option<CellRect>,
        tileset: Option<&Tileset>,
    ) -> Result<PaintReport> {
        self.check(seed, tile, flags)?;
        self.check_known(tile, tileset)?;
        let region = limit.map_or(self.bounds, |l| l.intersect(&self.bounds));
        if !region.contains(seed) {
            return Err(Error::new(
                "tilemap.fill",
                "seed lies outside the fill limit",
            ));
        }
        let target = self.tile_at(seed);
        if target == tile && (tile == 0 || self.get(seed).flags == flags) {
            return Ok(PaintReport::default());
        }
        let mut found = BTreeSet::from([seed]);
        let mut stack = vec![seed];
        while let Some(c) = stack.pop() {
            for n in [
                [c[0] + 1, c[1]],
                [c[0] - 1, c[1]],
                [c[0], c[1] + 1],
                [c[0], c[1] - 1],
            ] {
                if region.contains(n) && self.tile_at(n) == target && found.insert(n) {
                    if found.len() > MAX_EDIT_CELLS {
                        return Err(Error::new(
                            "tilemap.fill",
                            "flood region exceeds 65536 cells; pass a smaller limit",
                        ));
                    }
                    stack.push(n);
                }
            }
        }
        let bounds = found.iter().fold(CellRect::new(seed, seed), |r, c| {
            CellRect::new(
                [r.min[0].min(c[0]), r.min[1].min(c[1])],
                [r.max[0].max(c[0]), r.max[1].max(c[1])],
            )
        });
        let backup = self.snapshot_chunks(&CellRect::new(
            bounds.min,
            [bounds.max[0] + 1, bounds.max[1] + 1],
        ));
        let mut written = Vec::new();
        for &c in &found {
            match self.write(c, tile, flags) {
                Ok(true) => written.push(c),
                Ok(false) => {}
                Err(error) => {
                    self.restore_chunks(
                        &CellRect::new(bounds.min, [bounds.max[0] + 1, bounds.max[1] + 1]),
                        backup,
                    );
                    return Err(error);
                }
            }
        }
        Ok(self.finish(written, tileset))
    }
    /// Re-resolves every rule tile (call after loading or after editing the tileset rules).
    pub fn refresh_all(&mut self, tileset: Option<&Tileset>) -> PaintReport {
        let all: BTreeSet<CellPos> = self.cells().into_iter().map(|(c, _)| c).collect();
        let changed = self.refresh(&all, tileset);
        PaintReport {
            changed: 0,
            refreshed: changed.len(),
            dirty_chunks: changed
                .iter()
                .map(|c| chunk_of(*c))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
    #[allow(clippy::type_complexity)]
    fn snapshot_chunks(&self, rect: &CellRect) -> Vec<(ChunkKey, Option<(Vec<Cell>, u16, u64)>)> {
        let lo = chunk_of(rect.min);
        let hi = chunk_of([rect.max[0] - 1, rect.max[1] - 1]);
        let mut out = Vec::new();
        for y in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                out.push((
                    [x, y],
                    self.chunks
                        .get(&[x, y])
                        .map(|c| (c.cells.to_vec(), c.filled, c.revision)),
                ));
            }
        }
        out
    }
    #[allow(clippy::type_complexity)]
    fn restore_chunks(
        &mut self,
        _rect: &CellRect,
        backup: Vec<(ChunkKey, Option<(Vec<Cell>, u16, u64)>)>,
    ) {
        for (key, saved) in backup {
            match saved {
                None => {
                    self.chunks.remove(&key);
                }
                Some((cells, filled, revision)) => {
                    let mut array = Box::new([Cell::default(); CHUNK_CELLS]);
                    array.copy_from_slice(&cells);
                    self.chunks.insert(
                        key,
                        Chunk {
                            cells: array,
                            filled,
                            revision,
                        },
                    );
                }
            }
        }
    }
}

/// A multi-layer tile map. Cell `(x, y)` covers `[x, x+1) x [y, y+1)` cell units, y up.
pub struct Tilemap {
    bounds: CellRect,
    layers: Vec<TileLayer>,
}
impl Tilemap {
    /// A map with one layer named `ground`.
    pub fn new(bounds: CellRect) -> Result<Self> {
        let size = bounds.size();
        if size[0] == 0
            || size[1] == 0
            || size[0] > MAX_BOUNDS_EDGE
            || size[1] > MAX_BOUNDS_EDGE
            || bounds
                .min
                .iter()
                .chain(&bounds.max)
                .any(|v| v.abs() > 1_000_000)
        {
            return Err(Error::new(
                "tilemap.bounds",
                "bounds must be non-empty, at most 65536 cells per edge and within +-1,000,000",
            ));
        }
        let mut map = Self {
            bounds,
            layers: Vec::new(),
        };
        map.add_layer("ground")?;
        Ok(map)
    }
    /// The cell bounds (fixed at creation).
    pub fn bounds(&self) -> CellRect {
        self.bounds
    }
    /// Layers in creation order.
    pub fn layers(&self) -> &[TileLayer] {
        &self.layers
    }
    /// Mutable layers.
    pub fn layers_mut(&mut self) -> &mut [TileLayer] {
        &mut self.layers
    }
    /// Index of the layer called `name`.
    pub fn layer_index(&self, name: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.name == name)
    }
    /// The layer called `name`.
    pub fn layer(&self, name: &str) -> Option<&TileLayer> {
        self.layers.iter().find(|l| l.name == name)
    }
    /// The layer called `name`, mutably.
    pub fn layer_mut(&mut self, name: &str) -> Option<&mut TileLayer> {
        self.layers.iter_mut().find(|l| l.name == name)
    }
    /// Appends an empty layer; `z_order` defaults to the layer's index.
    pub fn add_layer(&mut self, name: &str) -> Result<&mut TileLayer> {
        if name.is_empty()
            || name.chars().count() > 64
            || self.layer_index(name).is_some()
            || self.layers.len() >= MAX_LAYERS
        {
            return Err(Error::new(
                "tilemap.layer",
                "layer names must be unique, 1-64 characters, at most 32 layers",
            ));
        }
        let mut layer = TileLayer::new(name, self.bounds);
        layer.z_order = self.layers.len() as i32;
        self.layers.push(layer);
        Ok(self.layers.last_mut().expect("just pushed"))
    }
    /// Removes the layer called `name`; returns whether it existed.
    pub fn remove_layer(&mut self, name: &str) -> bool {
        let before = self.layers.len();
        self.layers.retain(|l| l.name != name);
        self.layers.len() != before
    }
    /// Re-resolves rule tiles on every layer.
    pub fn refresh_all(&mut self, tileset: Option<&Tileset>) {
        for layer in &mut self.layers {
            layer.refresh_all(tileset);
        }
    }

    /// Serializable form.
    pub fn to_document(&self) -> TilemapDocument {
        TilemapDocument {
            version: TILEMAP_VERSION,
            bounds: self.bounds,
            layers: self
                .layers
                .iter()
                .map(|l| LayerDocument {
                    name: l.name.clone(),
                    z_order: l.z_order,
                    visible: l.visible,
                    opacity: l.opacity,
                    chunks: l
                        .chunks
                        .iter()
                        .map(|(key, chunk)| ChunkDocument {
                            x: key[0],
                            y: key[1],
                            tiles: chunk
                                .cells
                                .iter()
                                .map(|c| c.tile | (u32::from(c.flags) << 24))
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
    /// Validates `doc`; with a tileset, rule tiles resolve immediately and unknown tile ids are
    /// rejected.
    pub fn from_document(doc: &TilemapDocument, tileset: Option<&Tileset>) -> Result<Self> {
        let bad = |m: &str| Error::new("tilemap.invalid", m.to_string());
        if doc.version != TILEMAP_VERSION {
            return Err(bad("unsupported tilemap version"));
        }
        if doc.layers.len() > MAX_LAYERS {
            return Err(bad("too many layers"));
        }
        let mut map = Self::new(doc.bounds)?;
        map.layers.clear();
        for layer_doc in &doc.layers {
            if !(0.0..=1.0).contains(&layer_doc.opacity) {
                return Err(bad("opacity must be within 0..=1"));
            }
            let layer = map.add_layer(&layer_doc.name)?;
            layer.z_order = layer_doc.z_order;
            layer.visible = layer_doc.visible;
            layer.opacity = layer_doc.opacity;
            if layer_doc.chunks.len() > MAX_CHUNKS_PER_LAYER {
                return Err(bad("too many chunks"));
            }
            for chunk in &layer_doc.chunks {
                if chunk.tiles.len() != CHUNK_CELLS {
                    return Err(bad("each chunk needs exactly 256 tiles"));
                }
                for (i, packed) in chunk.tiles.iter().enumerate() {
                    let tile = packed & MAX_TILE_ID;
                    let flags = (packed >> 24) as u8;
                    if tile == 0 {
                        if *packed != 0 {
                            return Err(bad("empty cells must be zero"));
                        }
                        continue;
                    }
                    let i = i as i32;
                    let cell = [
                        chunk
                            .x
                            .checked_mul(CHUNK_SIZE)
                            .ok_or_else(|| bad("chunk overflow"))?
                            + i % CHUNK_SIZE,
                        chunk
                            .y
                            .checked_mul(CHUNK_SIZE)
                            .ok_or_else(|| bad("chunk overflow"))?
                            + i / CHUNK_SIZE,
                    ];
                    layer.check(cell, tile, flags)?;
                    layer.check_known(tile, tileset)?;
                    if layer.get(cell).tile != 0 {
                        return Err(bad("duplicate chunk"));
                    }
                    layer.write(cell, tile, flags)?;
                }
            }
            layer.refresh_all(tileset);
        }
        Ok(map)
    }
    /// Parses and validates a map JSON file (at most 16 MiB).
    pub fn from_json(bytes: &[u8], tileset: Option<&Tileset>) -> Result<Self> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::new("tilemap.invalid", "document exceeds 16 MiB"));
        }
        let doc: TilemapDocument = serde_json::from_slice(bytes)
            .map_err(|e| Error::new("tilemap.malformed", e.to_string()))?;
        Self::from_document(&doc, tileset)
    }
    /// Pretty JSON for the document.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.to_document())
            .expect("tilemap documents always serialize")
    }
}

fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}
/// Serialized layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerDocument {
    /// Unique layer name.
    pub name: String,
    /// Draw order.
    #[serde(default)]
    pub z_order: i32,
    /// Visibility.
    #[serde(default = "yes")]
    pub visible: bool,
    /// Opacity `0..=1`.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Non-empty chunks.
    #[serde(default)]
    pub chunks: Vec<ChunkDocument>,
}
/// Serialized chunk: 256 packed entries (`tile | flags << 24`), row-major from the bottom-left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkDocument {
    /// Chunk x.
    pub x: i32,
    /// Chunk y.
    pub y: i32,
    /// Packed cells.
    pub tiles: Vec<u32>,
}
/// Serialized tile map (`version` 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TilemapDocument {
    /// Format version; must be 1.
    pub version: u32,
    /// Cell bounds.
    #[serde(default)]
    pub bounds: CellRect,
    /// Layers in creation order.
    pub layers: Vec<LayerDocument>,
}
impl Default for TilemapDocument {
    fn default() -> Self {
        Tilemap::new(CellRect::default())
            .expect("default bounds are valid")
            .to_document()
    }
}
