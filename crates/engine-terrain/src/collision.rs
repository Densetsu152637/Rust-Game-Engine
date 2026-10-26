//! Collision tiles for a physics engine: the grid is cut into square tiles of `tile_cells`
//! cells, each a heightfield (rows along Z, columns along X, centred on the tile centre, the
//! layout of engine-physics `dim3::Shape::HeightField`) whose hole cells are listed for
//! removal. Tiles keep edits local: only tiles intersecting an edit's region are rebuilt.
use crate::heightfield::{Heightfield, SampleRect};
use crate::{Error, Result};

/// Smallest tile edge in cells.
pub const MIN_TILE_CELLS: u32 = 8;
/// Largest tile edge in cells (a 256 x 256 sample heightfield).
pub const MAX_TILE_CELLS: u32 = 255;

/// Geometry of one tile in tile-local coordinates: x and z relative to the tile centre,
/// heights unchanged (relative to the terrain origin).
#[derive(Clone, Debug, PartialEq)]
pub enum TileShape {
    /// `rows` x `columns` row-major heights spanning `size` local units. `holes` lists the
    /// tile's hole cells by row-major index over `(rows - 1) x (columns - 1)`, ascending.
    HeightField {
        rows: u32,
        columns: u32,
        heights: Vec<f32>,
        size: [f32; 2],
        holes: Vec<u32>,
    },
    /// Every cell is a hole.
    Empty,
}

/// One collision tile.
#[derive(Clone, Debug, PartialEq)]
pub struct CollisionTile {
    /// Tile coordinate.
    pub key: [u32; 2],
    /// Cells covered (`max` exclusive).
    pub cells: SampleRect,
    /// Terrain-local `[x, z]` of the tile centre (the collider's body origin).
    pub center: [f32; 2],
    pub shape: TileShape,
}

fn check(tile_cells: u32) -> Result<()> {
    if !(MIN_TILE_CELLS..=MAX_TILE_CELLS).contains(&tile_cells) {
        return Err(Error::new(
            "terrain.collision.tile",
            format!("tile_cells must be within {MIN_TILE_CELLS}..={MAX_TILE_CELLS}"),
        ));
    }
    Ok(())
}

/// Number of tiles `[x, z]`.
pub fn tile_counts(field: &Heightfield, tile_cells: u32) -> Result<[u32; 2]> {
    check(tile_cells)?;
    Ok([
        (field.columns() - 1).div_ceil(tile_cells),
        (field.rows() - 1).div_ceil(tile_cells),
    ])
}

/// Tiles whose cells intersect `region` (sample or cell indices; a sample edit affects the
/// cells on both sides of it, so the region is widened by one).
pub fn tiles_touching(
    field: &Heightfield,
    tile_cells: u32,
    region: SampleRect,
) -> Result<Vec<[u32; 2]>> {
    let [nx, nz] = tile_counts(field, tile_cells)?;
    let lo = |v: u32| v.saturating_sub(1) / tile_cells;
    let hi = |v: u32, n: u32| (v / tile_cells + 1).min(n);
    let mut out = Vec::new();
    for z in lo(region.min[1])..hi(region.max[1], nz) {
        for x in lo(region.min[0])..hi(region.max[0], nx) {
            out.push([x, z]);
        }
    }
    Ok(out)
}

/// Builds tile `key`.
pub fn collision_tile(
    field: &Heightfield,
    tile_cells: u32,
    key: [u32; 2],
) -> Result<CollisionTile> {
    let [nx, nz] = tile_counts(field, tile_cells)?;
    if key[0] >= nx || key[1] >= nz {
        return Err(Error::new(
            "terrain.collision.tile",
            "tile outside the grid",
        ));
    }
    let min = [key[0] * tile_cells, key[1] * tile_cells];
    let max = [
        (min[0] + tile_cells).min(field.columns() - 1),
        (min[1] + tile_cells).min(field.rows() - 1),
    ];
    let cells = SampleRect::new(min, max);
    let s = field.spacing();
    let size = [(max[0] - min[0]) as f32 * s, (max[1] - min[1]) as f32 * s];
    let center = [
        min[0] as f32 * s + size[0] * 0.5,
        min[1] as f32 * s + size[1] * 0.5,
    ];
    let (columns, rows) = (max[0] - min[0] + 1, max[1] - min[1] + 1);
    let mut holes = Vec::new();
    if field.any_hole(cells) {
        for r in min[1]..max[1] {
            for c in min[0]..max[0] {
                if field.is_hole(c, r) {
                    holes.push((r - min[1]) * (columns - 1) + (c - min[0]));
                }
            }
        }
    }
    let shape = if holes.len() == ((columns - 1) * (rows - 1)) as usize {
        TileShape::Empty
    } else {
        let mut heights = Vec::with_capacity((columns * rows) as usize);
        for r in min[1]..=max[1] {
            for c in min[0]..=max[0] {
                heights.push(field.height(c, r));
            }
        }
        TileShape::HeightField {
            rows,
            columns,
            heights,
            size,
            holes,
        }
    };
    Ok(CollisionTile {
        key,
        cells,
        center,
        shape,
    })
}

/// Every tile, row-major.
pub fn collision_tiles(field: &Heightfield, tile_cells: u32) -> Result<Vec<CollisionTile>> {
    let [nx, nz] = tile_counts(field, tile_cells)?;
    (0..nz)
        .flat_map(|z| (0..nx).map(move |x| [x, z]))
        .map(|key| collision_tile(field, tile_cells, key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_grid_and_list_hole_cells() {
        let heights = (0..21 * 13).map(|i| i as f32 * 0.01).collect();
        let mut f = Heightfield::from_heights(21, 13, 2.0, heights).unwrap();
        assert_eq!(tile_counts(&f, 8).unwrap(), [3, 2]);
        f.set_hole(17, 9, true).unwrap();
        let tiles = collision_tiles(&f, 8).unwrap();
        assert_eq!(tiles.len(), 6);
        let last = &tiles[5];
        assert_eq!(last.cells, SampleRect::new([16, 8], [20, 12]));
        assert_eq!(last.center, [36.0, 20.0]);
        match &last.shape {
            TileShape::HeightField {
                rows,
                columns,
                holes,
                ..
            } => {
                assert_eq!((*rows, *columns), (5, 5));
                // Cell (17, 9) is local column 1, row 1 of the 4 x 4 cell tile.
                assert_eq!(holes, &vec![5]);
            }
            other => panic!("expected a holed heightfield, got {other:?}"),
        }
        match &tiles[0].shape {
            TileShape::HeightField {
                rows,
                columns,
                heights,
                size,
                holes,
            } => {
                assert!(holes.is_empty());
                assert_eq!((*rows, *columns), (9, 9));
                assert_eq!(heights[9], f.height(0, 1));
                assert_eq!(*size, [16.0, 16.0]);
            }
            other => panic!("expected a heightfield, got {other:?}"),
        }
        let touched = tiles_touching(&f, 8, SampleRect::new([8, 0], [9, 1])).unwrap();
        assert_eq!(touched, vec![[0, 0], [1, 0]]);
        assert!(tile_counts(&f, 4).is_err());
    }
}
