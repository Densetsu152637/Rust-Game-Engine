//! Navigation-mesh input: a walkable triangle soup of the terrain that excludes holes.
//!
//! engine-navigation's baker keeps a raster cell only when one plane covers it completely, so
//! the raw terrain triangles (two planes per cell, creases everywhere) would bake to islands.
//! The soup is therefore raster-aligned: one planar quad (two coplanar triangles) per bake
//! cell, with the cell's world-space corners exactly `cell * cell_size`, the plane through the
//! surface height at the cell centre with the surface's mean slope across the cell. Neighbouring
//! quads meet within the surface curvature over one cell, which the bake's `max_step` bridges.
//! Cells overlapping a hole cell and cells steeper than `max_slope_degrees` are left out.
use crate::heightfield::{Heightfield, SampleRect};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Most quads in one soup (engine-navigation accepts 32,768 triangles and 65,536 vertices).
pub const MAX_NAV_CELLS: usize = 16_384;

/// Where and how finely to generate the soup.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NavInputOptions {
    /// Raster cell size; must equal the bake's `cell_size`. 0.01..=100.
    pub cell_size: f32,
    /// World position of the terrain's local origin (sample (0, 0) at height 0).
    pub origin: [f32; 3],
    /// Optional world-space `[[min_x, min_z], [max_x, max_z]]` restriction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<[[f32; 2]; 2]>,
    /// Steeper quads are left out (the bake would reject them too). 0..=80.
    pub max_slope_degrees: f32,
}
impl Default for NavInputOptions {
    fn default() -> Self {
        Self {
            cell_size: 0.25,
            origin: [0.0; 3],
            region: None,
            max_slope_degrees: 45.0,
        }
    }
}

/// The generated soup.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NavSoup {
    /// World-space vertices, four per quad.
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// Quads emitted.
    pub cells: usize,
    /// Cells skipped because they overlap a hole.
    pub hole_cells: usize,
    /// Cells skipped as too steep.
    pub steep_cells: usize,
}

/// Builds the walkable soup of `field` placed at `options.origin`.
pub fn navigation_soup(field: &Heightfield, options: &NavInputOptions) -> Result<NavSoup> {
    let c = options.cell_size;
    if !(c.is_finite() && (0.01..=100.0).contains(&c)) {
        return Err(Error::new(
            "terrain.navigation.invalid",
            "cell_size must be within 0.01..=100",
        ));
    }
    if !(options.max_slope_degrees.is_finite() && (0.0..=80.0).contains(&options.max_slope_degrees))
    {
        return Err(Error::new(
            "terrain.navigation.invalid",
            "max_slope_degrees must be within 0..=80",
        ));
    }
    if !options
        .origin
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 100_000.0)
    {
        return Err(Error::new(
            "terrain.navigation.invalid",
            "origin must be finite within 100000",
        ));
    }
    let [ox, oy, oz] = options.origin;
    let extent = field.extent();
    let mut lo = [ox, oz];
    let mut hi = [ox + extent[0], oz + extent[1]];
    if let Some([rmin, rmax]) = options.region {
        if !rmin.iter().chain(&rmax).all(|v| v.is_finite()) {
            return Err(Error::new(
                "terrain.navigation.invalid",
                "region must be finite",
            ));
        }
        lo = [lo[0].max(rmin[0]), lo[1].max(rmin[1])];
        hi = [hi[0].min(rmax[0]), hi[1].min(rmax[1])];
    }
    // Cells entirely inside [lo, hi].
    let first = [(lo[0] / c).ceil() as i64, (lo[1] / c).ceil() as i64];
    let last = [(hi[0] / c).floor() as i64, (hi[1] / c).floor() as i64];
    let counts = [(last[0] - first[0]).max(0), (last[1] - first[1]).max(0)];
    if (counts[0] as u128 * counts[1] as u128) > MAX_NAV_CELLS as u128 {
        return Err(Error::new(
            "terrain.navigation.capacity",
            format!(
                "{} x {} bake cells exceed {MAX_NAV_CELLS}; use a region or a larger cell_size",
                counts[0], counts[1]
            ),
        ));
    }
    let max_slope = options.max_slope_degrees.to_radians().tan();
    let s = field.spacing();
    let mut soup = NavSoup::default();
    for i in first[1]..last[1] {
        for j in first[0]..last[0] {
            let x0 = j as f32 * c;
            let z0 = i as f32 * c;
            let (x1, z1) = (x0 + c, z0 + c);
            // Local coordinates of the cell.
            let (lx0, lz0, lx1, lz1) = (x0 - ox, z0 - oz, x1 - ox, z1 - oz);
            let cells = SampleRect::new(
                [
                    (lx0 / s).floor().max(0.0) as u32,
                    (lz0 / s).floor().max(0.0) as u32,
                ],
                [(lx1 / s).ceil() as u32, (lz1 / s).ceil() as u32],
            );
            if field.any_hole(cells) {
                soup.hole_cells += 1;
                continue;
            }
            let (cx, cz) = ((lx0 + lx1) * 0.5, (lz0 + lz1) * 0.5);
            let h = field.height_at(cx, cz);
            let dx = (field.height_at(lx1, cz) - field.height_at(lx0, cz)) / c;
            let dz = (field.height_at(cx, lz1) - field.height_at(cx, lz0)) / c;
            if (dx * dx + dz * dz).sqrt() > max_slope {
                soup.steep_cells += 1;
                continue;
            }
            let y = |px: f32, pz: f32| oy + h + dx * (px - ox - cx) + dz * (pz - oz - cz);
            let base = soup.vertices.len() as u32;
            soup.vertices.extend_from_slice(&[
                [x0, y(x0, z0), z0],
                [x1, y(x1, z0), z0],
                [x1, y(x1, z1), z1],
                [x0, y(x0, z1), z1],
            ]);
            soup.triangles
                .extend_from_slice(&[[base, base + 3, base + 1], [base + 3, base + 2, base + 1]]);
            soup.cells += 1;
        }
    }
    Ok(soup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soup_is_raster_aligned_planar_and_skips_holes_and_steep_cells() {
        // h = 0.1 x, with a steep cliff column and one hole.
        let mut heights: Vec<f32> = (0..17 * 17).map(|i| 0.1 * (i % 17) as f32).collect();
        for r in 0..17 {
            heights[r * 17 + 12] += 20.0;
        }
        let mut f = Heightfield::from_heights(17, 17, 1.0, heights).unwrap();
        f.set_hole(3, 3, true).unwrap();
        let options = NavInputOptions {
            cell_size: 1.0,
            origin: [10.0, 2.0, -4.0],
            ..NavInputOptions::default()
        };
        let soup = navigation_soup(&f, &options).unwrap();
        assert_eq!(soup.cells + soup.hole_cells + soup.steep_cells, 16 * 16);
        assert_eq!(soup.hole_cells, 1);
        assert_eq!(soup.steep_cells, 2 * 16);
        assert_eq!(soup.triangles.len(), 2 * soup.cells);
        // Corners sit on the bake raster and on the surface for planar regions.
        let v = soup.vertices[0];
        assert_eq!([v[0], v[2]], [10.0, -4.0]);
        assert!((v[1] - 2.0).abs() < 1e-5);
        let region = NavInputOptions {
            region: Some([[10.0, -4.0], [14.0, 0.0]]),
            ..options
        };
        assert_eq!(navigation_soup(&f, &region).unwrap().cells, 15);
        let fine = NavInputOptions {
            cell_size: 0.1,
            ..options
        };
        assert!(navigation_soup(&f, &fine).is_err());
    }
}
