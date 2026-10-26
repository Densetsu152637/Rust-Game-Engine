//! The heightfield: a bounded grid of `f32` heights with a holes mask, a splat map of up to
//! eight normalized layer weights and the layer definitions.
//!
//! Coordinates are terrain-local and Y-up. Sample `(column, row)` sits at local
//! `(column * spacing, height, row * spacing)`: columns advance along +X, rows along +Z, and
//! storage is row-major (`row * columns + column`), the same layout as the engine-physics 3D
//! heightfield. Cell `(column, row)` spans samples `column..=column + 1` and `row..=row + 1`.
//!
//! **Triangulation.** Every cell is split along the diagonal from its `(x0, z1)` corner to its
//! `(x1, z0)` corner, which is Rapier/Parry's default heightfield triangulation: with cell-local
//! `u = (x - x0) / spacing`, `v = (z - z0) / spacing`, the first triangle `(x0,z0), (x0,z1),
//! (x1,z0)` covers `u + v <= 1` and the second `(x0,z1), (x1,z1), (x1,z0)` covers `u + v >= 1`.
//! [`Heightfield::height_at`], the chunk meshes, the GPU feature and the physics collider all
//! use this split, so rendered, sampled and simulated surfaces agree.
use crate::{Error, Result};
use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Largest number of samples along one side.
pub const MAX_SIDE: u32 = 4097;
/// Largest number of samples (`columns * rows`), 2049 x 2049.
pub const MAX_SAMPLES: usize = 2049 * 2049;
/// Largest number of splat layers.
pub const MAX_LAYERS: usize = 8;
/// Largest absolute height in local units.
pub const MAX_ABS_HEIGHT: f32 = 100_000.0;
/// Largest sample spacing in local units.
pub const MAX_SPACING: f32 = 1_000.0;
/// Sum of every sample's splat weights.
pub const WEIGHT_SUM: u32 = 255;

/// One splat layer: how the layer looks where its weight is painted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerrainLayer {
    /// Unique name (1..=64 bytes), used by paint tools and `layer_weight` queries.
    pub name: String,
    /// Texture asset id tiled over the layer, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture: Option<String>,
    /// Linear RGBA multiplied into the layer colour (each 0..=1).
    pub tint: [f32; 4],
    /// World units covered by one repeat of the layer texture.
    pub uv_tiling: f32,
}
impl Default for TerrainLayer {
    fn default() -> Self {
        Self {
            name: "base".into(),
            texture: None,
            tint: [1.0; 4],
            uv_tiling: 8.0,
        }
    }
}
impl TerrainLayer {
    /// A named layer with a tint and the default tiling.
    pub fn new(name: impl Into<String>, tint: [f32; 4]) -> Self {
        Self {
            name: name.into(),
            tint,
            ..Self::default()
        }
    }
    /// Validates the name, texture id, tint and tiling.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("terrain.layer.invalid", m.to_string()));
        if self.name.is_empty() || self.name.len() > 64 {
            return bad("layer names need 1..=64 bytes");
        }
        if self
            .texture
            .as_ref()
            .is_some_and(|t| t.is_empty() || t.len() > 128)
        {
            return bad("layer texture ids need 1..=128 bytes");
        }
        if !self
            .tint
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        {
            return bad("layer tint components must be within 0..=1");
        }
        if !(self.uv_tiling.is_finite() && self.uv_tiling > 0.0 && self.uv_tiling <= 100_000.0) {
            return bad("layer uv_tiling must be within (0, 100000]");
        }
        Ok(())
    }
}

/// A half-open rectangle of sample (or cell) indices: `min` inclusive, `max` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SampleRect {
    pub min: [u32; 2],
    pub max: [u32; 2],
}
impl SampleRect {
    /// A rectangle from inclusive `min` to exclusive `max`.
    pub fn new(min: [u32; 2], max: [u32; 2]) -> Self {
        Self { min, max }
    }
    /// Whether it contains no index.
    pub fn is_empty(&self) -> bool {
        self.min[0] >= self.max[0] || self.min[1] >= self.max[1]
    }
    /// Number of indices covered.
    pub fn area(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            (self.max[0] - self.min[0]) as usize * (self.max[1] - self.min[1]) as usize
        }
    }
    /// Smallest rectangle covering both (an empty side is ignored).
    pub fn union(self, other: SampleRect) -> SampleRect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        SampleRect {
            min: [self.min[0].min(other.min[0]), self.min[1].min(other.min[1])],
            max: [self.max[0].max(other.max[0]), self.max[1].max(other.max[1])],
        }
    }
    /// Whether the two rectangles share at least one index.
    pub fn intersects(&self, other: &SampleRect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.min[0] < other.max[0]
            && other.min[0] < self.max[0]
            && self.min[1] < other.max[1]
            && other.min[1] < self.max[1]
    }
}

/// The terrain grid. See the module documentation for coordinates and triangulation.
#[derive(Clone, Debug, PartialEq)]
pub struct Heightfield {
    columns: u32,
    rows: u32,
    spacing: f32,
    heights: Vec<f32>,
    /// One bit per cell, row-major over `(columns - 1) x (rows - 1)` cells.
    holes: Vec<u64>,
    hole_count: usize,
    layers: Vec<TerrainLayer>,
    /// `samples * layers.len()` weights; each sample's weights sum to [`WEIGHT_SUM`].
    splat: Vec<u8>,
    bounds: [f32; 2],
    revision: u64,
}

pub(crate) fn check_dims(columns: u32, rows: u32, spacing: f32) -> Result<()> {
    if !(2..=MAX_SIDE).contains(&columns) || !(2..=MAX_SIDE).contains(&rows) {
        return Err(Error::new(
            "terrain.size",
            format!("columns and rows must be within 2..={MAX_SIDE}"),
        ));
    }
    if columns as usize * rows as usize > MAX_SAMPLES {
        return Err(Error::new(
            "terrain.size",
            format!("at most {MAX_SAMPLES} samples (columns x rows)"),
        ));
    }
    if !(spacing.is_finite() && spacing > 0.0 && spacing <= MAX_SPACING) {
        return Err(Error::new(
            "terrain.spacing",
            format!("spacing must be within (0, {MAX_SPACING}]"),
        ));
    }
    Ok(())
}
fn check_height(h: f32) -> Result<()> {
    if h.is_finite() && h.abs() <= MAX_ABS_HEIGHT {
        Ok(())
    } else {
        Err(Error::new(
            "terrain.height",
            format!("heights must be finite with magnitude <= {MAX_ABS_HEIGHT}"),
        ))
    }
}
pub(crate) fn check_layers(layers: &[TerrainLayer]) -> Result<()> {
    if layers.is_empty() || layers.len() > MAX_LAYERS {
        return Err(Error::new(
            "terrain.layer.count",
            format!("1..={MAX_LAYERS} layers are required"),
        ));
    }
    for (i, layer) in layers.iter().enumerate() {
        layer.validate()?;
        if layers[..i].iter().any(|l| l.name == layer.name) {
            return Err(Error::new(
                "terrain.layer.duplicate",
                format!("duplicate layer name {}", layer.name),
            ));
        }
    }
    Ok(())
}

impl Heightfield {
    /// A flat heightfield at height 0 with one default layer named `base`.
    pub fn new(columns: u32, rows: u32, spacing: f32) -> Result<Self> {
        check_dims(columns, rows, spacing)?;
        let samples = columns as usize * rows as usize;
        Self::from_parts(
            columns,
            rows,
            spacing,
            vec![0.0; samples],
            vec![TerrainLayer::default()],
            vec![WEIGHT_SUM as u8; samples],
            None,
        )
    }
    /// A heightfield from row-major heights, with one default layer.
    pub fn from_heights(columns: u32, rows: u32, spacing: f32, heights: Vec<f32>) -> Result<Self> {
        check_dims(columns, rows, spacing)?;
        let samples = columns as usize * rows as usize;
        Self::from_parts(
            columns,
            rows,
            spacing,
            heights,
            vec![TerrainLayer::default()],
            vec![WEIGHT_SUM as u8; samples],
            None,
        )
    }
    /// Validates and assembles every part. `splat` holds `layers.len()` weights per sample
    /// (summing to 255); `holes` is one flag per cell, row-major, or `None` for no holes.
    pub fn from_parts(
        columns: u32,
        rows: u32,
        spacing: f32,
        heights: Vec<f32>,
        layers: Vec<TerrainLayer>,
        splat: Vec<u8>,
        holes: Option<Vec<bool>>,
    ) -> Result<Self> {
        check_dims(columns, rows, spacing)?;
        let samples = columns as usize * rows as usize;
        if heights.len() != samples {
            return Err(Error::new(
                "terrain.size",
                format!("{} heights given for {samples} samples", heights.len()),
            ));
        }
        for &h in &heights {
            check_height(h)?;
        }
        check_layers(&layers)?;
        if splat.len() != samples * layers.len() {
            return Err(Error::new(
                "terrain.splat.size",
                "splat needs one weight per layer per sample",
            ));
        }
        for weights in splat.chunks_exact(layers.len()) {
            if weights.iter().map(|&w| w as u32).sum::<u32>() != WEIGHT_SUM {
                return Err(Error::new(
                    "terrain.splat.sum",
                    "each sample's layer weights must sum to 255",
                ));
            }
        }
        let cells = (columns as usize - 1) * (rows as usize - 1);
        let mut field = Self {
            columns,
            rows,
            spacing,
            heights,
            holes: vec![0; cells.div_ceil(64)],
            hole_count: 0,
            layers,
            splat,
            bounds: [0.0; 2],
            revision: 0,
        };
        if let Some(flags) = holes {
            if flags.len() != cells {
                return Err(Error::new(
                    "terrain.holes.size",
                    "the holes mask needs one flag per cell",
                ));
            }
            for (index, &hole) in flags.iter().enumerate() {
                if hole {
                    field.holes[index / 64] |= 1 << (index % 64);
                    field.hole_count += 1;
                }
            }
        }
        field.refresh_bounds();
        Ok(field)
    }

    /// Samples along X.
    pub fn columns(&self) -> u32 {
        self.columns
    }
    /// Samples along Z.
    pub fn rows(&self) -> u32 {
        self.rows
    }
    /// Distance between neighbouring samples.
    pub fn spacing(&self) -> f32 {
        self.spacing
    }
    /// Local extent `[x, z]`: `(columns - 1) * spacing` by `(rows - 1) * spacing`.
    pub fn extent(&self) -> [f32; 2] {
        [
            (self.columns - 1) as f32 * self.spacing,
            (self.rows - 1) as f32 * self.spacing,
        ]
    }
    /// Number of samples.
    pub fn sample_count(&self) -> usize {
        self.heights.len()
    }
    /// Number of cells, `(columns - 1) * (rows - 1)`.
    pub fn cell_count(&self) -> usize {
        (self.columns as usize - 1) * (self.rows as usize - 1)
    }
    /// Row-major heights.
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }
    /// Incremented by every mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Lowest and highest sample height.
    pub fn height_range(&self) -> [f32; 2] {
        self.bounds
    }
    /// Local axis-aligned bounds `(min, max)`.
    pub fn local_bounds(&self) -> (Vec3, Vec3) {
        let e = self.extent();
        (
            Vec3::new(0.0, self.bounds[0], 0.0),
            Vec3::new(e[0], self.bounds[1], e[1]),
        )
    }

    fn index(&self, column: u32, row: u32) -> usize {
        row as usize * self.columns as usize + column as usize
    }
    /// Height of sample `(column, row)`; indices are clamped to the grid.
    pub fn height(&self, column: u32, row: u32) -> f32 {
        self.heights[self.index(column.min(self.columns - 1), row.min(self.rows - 1))]
    }
    /// Height of a signed sample index, clamped to the grid (the edge samples repeat).
    pub fn height_clamped(&self, column: i64, row: i64) -> f32 {
        let c = column.clamp(0, self.columns as i64 - 1) as u32;
        let r = row.clamp(0, self.rows as i64 - 1) as u32;
        self.heights[self.index(c, r)]
    }
    /// Sets one sample (bounds, finiteness and magnitude checked).
    pub fn set_height(&mut self, column: u32, row: u32, height: f32) -> Result<()> {
        self.check_sample(column, row)?;
        check_height(height)?;
        let index = self.index(column, row);
        self.heights[index] = height;
        self.touch();
        Ok(())
    }
    /// Replaces every height (length, finiteness and magnitude checked).
    pub fn set_heights(&mut self, heights: Vec<f32>) -> Result<()> {
        if heights.len() != self.heights.len() {
            return Err(Error::new("terrain.size", "height count mismatch"));
        }
        for &h in &heights {
            check_height(h)?;
        }
        self.heights = heights;
        self.touch();
        Ok(())
    }
    fn check_sample(&self, column: u32, row: u32) -> Result<()> {
        if column >= self.columns || row >= self.rows {
            return Err(Error::new(
                "terrain.sample",
                format!("sample ({column}, {row}) outside the grid"),
            ));
        }
        Ok(())
    }
    /// Records a mutation: bumps the revision and refreshes the height range.
    pub(crate) fn touch(&mut self) {
        self.revision += 1;
        self.refresh_bounds();
    }
    fn refresh_bounds(&mut self) {
        let (lo, hi) = self
            .heights
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &h| {
                (lo.min(h), hi.max(h))
            });
        self.bounds = [lo, hi];
    }
    pub(crate) fn heights_mut(&mut self) -> &mut [f32] {
        &mut self.heights
    }

    /// Cell coordinate and in-cell fractions `(u, v)` of local `(x, z)`, clamped to the grid.
    fn locate(&self, x: f32, z: f32) -> ([u32; 2], f32, f32) {
        let e = self.extent();
        let fx = (x.clamp(0.0, e[0]) / self.spacing).min((self.columns - 1) as f32);
        let fz = (z.clamp(0.0, e[1]) / self.spacing).min((self.rows - 1) as f32);
        let cx = (fx.floor() as u32).min(self.columns - 2);
        let cz = (fz.floor() as u32).min(self.rows - 2);
        ([cx, cz], fx - cx as f32, fz - cz as f32)
    }
    /// Whether local `(x, z)` lies inside the grid's extent.
    pub fn contains(&self, x: f32, z: f32) -> bool {
        let e = self.extent();
        x >= 0.0 && z >= 0.0 && x <= e[0] && z <= e[1]
    }
    /// Cell containing local `(x, z)`, or `None` outside the extent.
    pub fn cell_at(&self, x: f32, z: f32) -> Option<[u32; 2]> {
        self.contains(x, z).then(|| self.locate(x, z).0)
    }
    /// Height of the triangulated surface at local `(x, z)` (clamped to the extent). Matches
    /// the rendered mesh at full detail and the physics heightfield exactly in exact
    /// arithmetic; see the module documentation for the diagonal.
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let ([c, r], u, v) = self.locate(x, z);
        let h00 = self.height(c, r);
        let h10 = self.height(c + 1, r);
        let h01 = self.height(c, r + 1);
        let h11 = self.height(c + 1, r + 1);
        triangulated(h00, h10, h01, h11, u, v)
    }
    /// Bilinear interpolation of the four samples around local `(x, z)` (clamped). Smoother
    /// than [`height_at`](Self::height_at) but not on the rendered surface between samples.
    pub fn bilinear_height_at(&self, x: f32, z: f32) -> f32 {
        let ([c, r], u, v) = self.locate(x, z);
        let a = self.height(c, r) + (self.height(c + 1, r) - self.height(c, r)) * u;
        let b = self.height(c, r + 1) + (self.height(c + 1, r + 1) - self.height(c, r + 1)) * u;
        a + (b - a) * v
    }
    /// Unit normal of the surface triangle under local `(x, z)` (faceted, matches physics).
    pub fn normal_at(&self, x: f32, z: f32) -> Vec3 {
        let ([c, r], u, v) = self.locate(x, z);
        let s = self.spacing;
        let h00 = self.height(c, r);
        let h10 = self.height(c + 1, r);
        let h01 = self.height(c, r + 1);
        let h11 = self.height(c + 1, r + 1);
        let (dx, dz) = if u + v <= 1.0 {
            (h10 - h00, h01 - h00)
        } else {
            (h11 - h01, h11 - h10)
        };
        Vec3::new(-dx / s, 1.0, -dz / s).normalize()
    }
    /// Unit normal at sample `(column, row)` by central differences (one-sided at edges).
    pub fn sample_normal(&self, column: u32, row: u32) -> Vec3 {
        let (c, r) = (column as i64, row as i64);
        let left = (c - 1).max(0);
        let right = (c + 1).min(self.columns as i64 - 1);
        let down = (r - 1).max(0);
        let up = (r + 1).min(self.rows as i64 - 1);
        let dx = (self.height_clamped(right, r) - self.height_clamped(left, r))
            / ((right - left) as f32 * self.spacing);
        let dz = (self.height_clamped(c, up) - self.height_clamped(c, down))
            / ((up - down) as f32 * self.spacing);
        Vec3::new(-dx, 1.0, -dz).normalize()
    }
    /// Bilinear blend of the four surrounding sample normals (smooth shading normal).
    pub fn smooth_normal_at(&self, x: f32, z: f32) -> Vec3 {
        let ([c, r], u, v) = self.locate(x, z);
        let a = self
            .sample_normal(c, r)
            .lerp(self.sample_normal(c + 1, r), u);
        let b = self
            .sample_normal(c, r + 1)
            .lerp(self.sample_normal(c + 1, r + 1), u);
        a.lerp(b, v).normalize()
    }
    /// Slope in degrees of the surface triangle under local `(x, z)`.
    pub fn slope_degrees_at(&self, x: f32, z: f32) -> f32 {
        self.normal_at(x, z).y.clamp(-1.0, 1.0).acos().to_degrees()
    }

    // ---- holes -------------------------------------------------------------------------

    fn cell_index(&self, column: u32, row: u32) -> usize {
        row as usize * (self.columns as usize - 1) + column as usize
    }
    /// Whether cell `(column, row)` is a hole (false outside the grid).
    pub fn is_hole(&self, column: u32, row: u32) -> bool {
        if column + 1 >= self.columns || row + 1 >= self.rows {
            return false;
        }
        let i = self.cell_index(column, row);
        self.holes[i / 64] & (1 << (i % 64)) != 0
    }
    /// Whether local `(x, z)` falls into a hole cell.
    pub fn is_hole_at(&self, x: f32, z: f32) -> bool {
        self.cell_at(x, z).is_some_and(|[c, r]| self.is_hole(c, r))
    }
    /// Marks or clears one cell; returns whether it changed.
    pub fn set_hole(&mut self, column: u32, row: u32, hole: bool) -> Result<bool> {
        if column + 1 >= self.columns || row + 1 >= self.rows {
            return Err(Error::new(
                "terrain.cell",
                format!("cell ({column}, {row}) outside the grid"),
            ));
        }
        let changed = self.set_hole_unchecked(column, row, hole);
        if changed {
            self.touch();
        }
        Ok(changed)
    }
    pub(crate) fn set_hole_unchecked(&mut self, column: u32, row: u32, hole: bool) -> bool {
        let i = self.cell_index(column, row);
        let bit = 1u64 << (i % 64);
        let was = self.holes[i / 64] & bit != 0;
        if was == hole {
            return false;
        }
        if hole {
            self.holes[i / 64] |= bit;
            self.hole_count += 1;
        } else {
            self.holes[i / 64] &= !bit;
            self.hole_count -= 1;
        }
        true
    }
    /// Number of hole cells.
    pub fn hole_count(&self) -> usize {
        self.hole_count
    }
    /// Whether any cell of the half-open cell rectangle is a hole.
    pub fn any_hole(&self, cells: SampleRect) -> bool {
        if self.hole_count == 0 {
            return false;
        }
        let max_c = cells.max[0].min(self.columns - 1);
        let max_r = cells.max[1].min(self.rows - 1);
        (cells.min[1]..max_r).any(|r| (cells.min[0]..max_c).any(|c| self.is_hole(c, r)))
    }
    /// One flag per cell, row-major.
    pub fn hole_flags(&self) -> Vec<bool> {
        (0..self.rows - 1)
            .flat_map(|r| (0..self.columns - 1).map(move |c| (c, r)))
            .map(|(c, r)| self.is_hole(c, r))
            .collect()
    }

    // ---- layers and splat --------------------------------------------------------------

    /// The splat layers.
    pub fn layers(&self) -> &[TerrainLayer] {
        &self.layers
    }
    /// Index of the layer called `name`.
    pub fn layer_index(&self, name: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.name == name)
    }
    /// Replaces a layer's definition (its name must stay unique).
    pub fn set_layer(&mut self, index: usize, layer: TerrainLayer) -> Result<()> {
        if index >= self.layers.len() {
            return Err(Error::new("terrain.layer.unknown", "no such layer index"));
        }
        let mut layers = self.layers.clone();
        layers[index] = layer;
        check_layers(&layers)?;
        self.layers = layers;
        self.revision += 1;
        Ok(())
    }
    /// Appends a layer with zero weight everywhere; returns its index.
    pub fn add_layer(&mut self, layer: TerrainLayer) -> Result<usize> {
        let mut layers = self.layers.clone();
        layers.push(layer);
        check_layers(&layers)?;
        let old = self.layers.len();
        let mut splat = Vec::with_capacity(self.sample_count() * layers.len());
        for weights in self.splat.chunks_exact(old) {
            splat.extend_from_slice(weights);
            splat.push(0);
        }
        self.layers = layers;
        self.splat = splat;
        self.revision += 1;
        Ok(old)
    }
    /// All weights, `layers().len()` per sample in sample order.
    pub fn splat(&self) -> &[u8] {
        &self.splat
    }
    /// The weights (0..=255, summing to 255) of sample `(column, row)`.
    pub fn sample_weights(&self, column: u32, row: u32) -> &[u8] {
        let n = self.layers.len();
        let i = self.index(column.min(self.columns - 1), row.min(self.rows - 1)) * n;
        &self.splat[i..i + n]
    }
    /// Sets one sample's weights; they must number `layers().len()` and sum to 255.
    pub fn set_sample_weights(&mut self, column: u32, row: u32, weights: &[u8]) -> Result<()> {
        self.check_sample(column, row)?;
        if weights.len() != self.layers.len()
            || weights.iter().map(|&w| w as u32).sum::<u32>() != WEIGHT_SUM
        {
            return Err(Error::new(
                "terrain.splat.sum",
                "one weight per layer, summing to 255",
            ));
        }
        let n = self.layers.len();
        let i = self.index(column, row) * n;
        self.splat[i..i + n].copy_from_slice(weights);
        self.revision += 1;
        Ok(())
    }
    pub(crate) fn splat_mut(&mut self) -> &mut [u8] {
        &mut self.splat
    }
    pub(crate) fn bump(&mut self) {
        self.revision += 1;
    }
    /// Normalized weight (0..=1) of layer `layer` at local `(x, z)`, bilinear between the four
    /// surrounding samples; `None` for an unknown layer index. Weights of all layers at a point
    /// sum to 1. Foliage density maps and PCG `layer` fields read this.
    pub fn layer_weight_at(&self, layer: usize, x: f32, z: f32) -> Option<f32> {
        if layer >= self.layers.len() {
            return None;
        }
        let ([c, r], u, v) = self.locate(x, z);
        let w = |c, r| self.sample_weights(c, r)[layer] as f32 / WEIGHT_SUM as f32;
        let a = w(c, r) + (w(c + 1, r) - w(c, r)) * u;
        let b = w(c, r + 1) + (w(c + 1, r + 1) - w(c, r + 1)) * u;
        Some(a + (b - a) * v)
    }
    /// [`layer_weight_at`](Self::layer_weight_at) by layer name.
    pub fn layer_weight_named(&self, layer: &str, x: f32, z: f32) -> Option<f32> {
        self.layer_weight_at(self.layer_index(layer)?, x, z)
    }
    /// Splat colour of sample `(column, row)`: `sum(weight_i * tint_i)`, weights normalized.
    pub fn sample_color(&self, column: u32, row: u32) -> [f32; 4] {
        let mut color = [0.0f32; 4];
        for (weight, layer) in self.sample_weights(column, row).iter().zip(&self.layers) {
            let w = *weight as f32 / WEIGHT_SUM as f32;
            for (out, tint) in color.iter_mut().zip(layer.tint) {
                *out += w * tint;
            }
        }
        color
    }
}

/// Height inside one cell from its corner heights and fractions, on the shared diagonal.
pub fn triangulated(h00: f32, h10: f32, h01: f32, h11: f32, u: f32, v: f32) -> f32 {
    if u + v <= 1.0 {
        h00 + u * (h10 - h00) + v * (h01 - h00)
    } else {
        h11 + (1.0 - u) * (h01 - h11) + (1.0 - v) * (h10 - h11)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Heightfield {
        // h = x + 2 z on a 5x4 grid with spacing 2: planar, so every sampler is exact.
        let heights = (0..4)
            .flat_map(|r| (0..5).map(move |c| (c as f32 * 2.0) + 2.0 * (r as f32 * 2.0)))
            .collect();
        Heightfield::from_heights(5, 4, 2.0, heights).unwrap()
    }

    #[test]
    fn planar_surface_samples_exactly_with_both_samplers() {
        let f = ramp();
        assert_eq!(f.extent(), [8.0, 6.0]);
        for &(x, z) in &[(0.3, 0.7), (3.9, 1.1), (7.99, 5.99), (1.0, 3.0)] {
            assert!((f.height_at(x, z) - (x + 2.0 * z)).abs() < 1e-4);
            assert!((f.bilinear_height_at(x, z) - (x + 2.0 * z)).abs() < 1e-4);
            let n = f.normal_at(x, z);
            let expected = Vec3::new(-1.0, 1.0, -2.0).normalize();
            assert!(n.abs_diff_eq(expected, 1e-5), "{n}");
        }
        assert_eq!(f.height_range(), [0.0, 8.0 + 12.0]);
        // Clamped outside the extent.
        assert_eq!(f.height_at(-5.0, -5.0), 0.0);
    }

    #[test]
    fn triangulation_uses_the_x0z1_x1z0_diagonal() {
        // One cell: only (x1, z1) is raised. On the diagonal u + v = 1 the height is 0 because
        // the diagonal joins (x0, z1) and (x1, z0); the opposite split would give 0.5.
        let f = Heightfield::from_heights(2, 2, 1.0, vec![0.0, 0.0, 0.0, 1.0]).unwrap();
        assert!(f.height_at(0.5, 0.5).abs() < 1e-6);
        assert!((f.height_at(0.75, 0.75) - 0.5).abs() < 1e-6);
        assert!((f.bilinear_height_at(0.5, 0.5) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn holes_and_layers_are_validated_and_counted() {
        let mut f = Heightfield::new(4, 4, 1.0).unwrap();
        assert!(f.set_hole(1, 1, true).unwrap());
        assert!(!f.set_hole(1, 1, true).unwrap());
        assert!(f.set_hole(3, 0, true).is_err());
        assert_eq!(f.hole_count(), 1);
        assert!(f.is_hole_at(1.5, 1.5));
        assert!(!f.is_hole_at(0.5, 1.5));
        assert!(f.any_hole(SampleRect::new([0, 0], [2, 2])));
        let grass = f
            .add_layer(TerrainLayer::new("grass", [0., 1., 0., 1.]))
            .unwrap();
        assert_eq!(f.layer_weight_at(grass, 1.0, 1.0), Some(0.0));
        assert!(f.add_layer(TerrainLayer::new("grass", [1.; 4])).is_err());
        f.set_sample_weights(0, 0, &[55, 200]).unwrap();
        assert!(f.set_sample_weights(0, 0, &[55, 199]).is_err());
        let w = f.layer_weight_at(grass, 0.0, 0.0).unwrap();
        assert!((w - 200.0 / 255.0).abs() < 1e-6);
        assert_eq!(f.sample_color(0, 0)[0], 55.0 / 255.0);
        assert!(Heightfield::new(1, 4, 1.0).is_err());
        assert!(Heightfield::new(4, 4, 0.0).is_err());
        assert!(Heightfield::from_heights(2, 2, 1.0, vec![0., f32::NAN, 0., 0.]).is_err());
    }
}
