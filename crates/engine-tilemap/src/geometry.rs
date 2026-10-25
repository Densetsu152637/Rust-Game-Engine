//! Render data for one chunk: textured quads in tilemap-local space.
use crate::error::{Error, Result};
use crate::map::{CellPos, ChunkKey, TileLayer};
use crate::orient;
use crate::tileset::Tileset;

/// Cell origin (bottom-left corner) in tilemap-local units.
pub fn cell_origin(cell: CellPos, cell_size: [f32; 2]) -> [f32; 2] {
    [cell[0] as f32 * cell_size[0], cell[1] as f32 * cell_size[1]]
}
/// The cell containing tilemap-local `position` (floor, so cell `(0,0)` is `[0, size)`).
pub fn position_to_cell(position: [f32; 2], cell_size: [f32; 2]) -> CellPos {
    [
        (position[0] / cell_size[0]).floor() as i32,
        (position[1] / cell_size[1]).floor() as i32,
    ]
}

/// Quad data for one chunk of one layer. Each quad has four vertices in the order
/// bottom-left, bottom-right, top-right, top-left (counter-clockwise, y up) and six indices
/// `0,1,2, 0,2,3`. UVs are normalised to the atlas texture with the origin at its top-left, so
/// `v` grows downward; orientation flags are already baked into the UVs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChunkGeometry {
    /// Vertex positions, tilemap-local.
    pub positions: Vec<[f32; 2]>,
    /// Vertex texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle list indices.
    pub indices: Vec<u32>,
}
impl ChunkGeometry {
    /// Number of quads.
    pub fn quads(&self) -> usize {
        self.positions.len() / 4
    }
}

/// Builds the quads of `key`. Cells whose displayed tile is unknown are skipped.
pub fn chunk_geometry(
    layer: &TileLayer,
    key: ChunkKey,
    tileset: &Tileset,
    cell_size: [f32; 2],
    texture_size: [u32; 2],
) -> Result<ChunkGeometry> {
    if texture_size.contains(&0) || !cell_size.iter().all(|v| v.is_finite() && *v > 0.0) {
        return Err(Error::new(
            "tilemap.geometry",
            "texture size and cell size must be positive",
        ));
    }
    let mut out = ChunkGeometry::default();
    let (tw, th) = (texture_size[0] as f32, texture_size[1] as f32);
    for (cell, data) in layer.chunk_cells(key) {
        let Some(def) = tileset.concrete(data.shown) else {
            continue;
        };
        let [sx, sy, sw, sh] = def.source;
        let origin = cell_origin(cell, cell_size);
        let base = out.positions.len() as u32;
        for corner in [[-0.5f32, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]] {
            out.positions.push([
                origin[0] + (corner[0] + 0.5) * cell_size[0],
                origin[1] + (corner[1] + 0.5) * cell_size[1],
            ]);
            let art = orient::invert(data.shown_flags, corner);
            out.uvs.push([
                (sx as f32 + (art[0] + 0.5) * sw as f32) / tw,
                (sy as f32 + (0.5 - art[1]) * sh as f32) / th,
            ]);
        }
        out.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Ok(out)
}
