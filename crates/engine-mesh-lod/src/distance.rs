//! Surface distance measurement (approximate symmetric Hausdorff distance).

use glam::{DVec3, Vec3};

use crate::error::Result;
use crate::mesh::MeshData;

/// Closest point on triangle `abc` to `p` (Ericson, Real-Time Collision Detection).
pub(crate) fn closest_point_on_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let denom = d1 - d3;
        let v = if denom != 0.0 { d1 / denom } else { 0.0 };
        return a + ab * v;
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let denom = d2 - d6;
        let w = if denom != 0.0 { d2 / denom } else { 0.0 };
        return a + ac * w;
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let denom = (d4 - d3) + (d5 - d6);
        let w = if denom != 0.0 { (d4 - d3) / denom } else { 0.0 };
        return b + (c - b) * w;
    }
    let sum = va + vb + vc;
    if sum == 0.0 {
        return a;
    }
    let denom = 1.0 / sum;
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// Uniform grid over triangles for nearest-surface queries.
struct TriangleGrid {
    tris: Vec<[DVec3; 3]>,
    origin: DVec3,
    cell: f64,
    dims: [usize; 3],
    cells: Vec<Vec<u32>>,
}

impl TriangleGrid {
    fn new(mesh: &MeshData) -> Self {
        let tris: Vec<[DVec3; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|t| t.map(|i| mesh.positions[i as usize].as_dvec3()))
            .collect();
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in &tris {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        let extent = hi - lo;
        let max_extent = extent.max_element().max(1e-12);
        let per_axis = (tris.len() as f64).cbrt().ceil().clamp(1.0, 256.0);
        let cell = max_extent / per_axis;
        let dims = [0, 1, 2].map(|a| ((extent[a] / cell).ceil() as usize).clamp(1, 256));
        let mut cells = vec![Vec::new(); dims[0] * dims[1] * dims[2]];
        let idx = |c: [usize; 3]| (c[2] * dims[1] + c[1]) * dims[0] + c[0];
        for (ti, t) in tris.iter().enumerate() {
            let tlo = t[0].min(t[1]).min(t[2]);
            let thi = t[0].max(t[1]).max(t[2]);
            let clo = [0, 1, 2].map(|a| (((tlo[a] - lo[a]) / cell).floor().max(0.0) as usize).min(dims[a] - 1));
            let chi = [0, 1, 2].map(|a| (((thi[a] - lo[a]) / cell).floor().max(0.0) as usize).min(dims[a] - 1));
            for z in clo[2]..=chi[2] {
                for y in clo[1]..=chi[1] {
                    for x in clo[0]..=chi[0] {
                        cells[idx([x, y, z])].push(ti as u32);
                    }
                }
            }
        }
        Self {
            tris,
            origin: lo,
            cell,
            dims,
            cells,
        }
    }

    /// Distance from `p` to the nearest triangle.
    fn distance(&self, p: DVec3) -> f64 {
        let c0 = [0, 1, 2].map(|a| (((p[a] - self.origin[a]) / self.cell).floor().max(0.0) as usize).min(self.dims[a] - 1));
        let max_ring = self.dims.iter().copied().max().unwrap_or(1);
        let mut best = f64::INFINITY;
        for r in 0..=max_ring {
            let ri = r as isize;
            for dz in -ri..=ri {
                for dy in -ri..=ri {
                    for dx in -ri..=ri {
                        if dx.abs().max(dy.abs()).max(dz.abs()) != ri {
                            continue;
                        }
                        let x = c0[0] as isize + dx;
                        let y = c0[1] as isize + dy;
                        let z = c0[2] as isize + dz;
                        if x < 0
                            || y < 0
                            || z < 0
                            || x >= self.dims[0] as isize
                            || y >= self.dims[1] as isize
                            || z >= self.dims[2] as isize
                        {
                            continue;
                        }
                        let cell = &self.cells
                            [(z as usize * self.dims[1] + y as usize) * self.dims[0] + x as usize];
                        for &t in cell {
                            let [a, b, c] = self.tris[t as usize];
                            let q = closest_point_on_triangle(p, a, b, c);
                            best = best.min(q.distance(p));
                        }
                    }
                }
            }
            if best <= r as f64 * self.cell {
                break;
            }
        }
        best
    }
}

fn one_sided(from: &MeshData, to: &TriangleGrid) -> f64 {
    let mut worst = 0.0f64;
    for p in &from.positions {
        worst = worst.max(to.distance(p.as_dvec3()));
    }
    for t in from.indices.chunks_exact(3) {
        let c = (from.positions[t[0] as usize].as_dvec3()
            + from.positions[t[1] as usize].as_dvec3()
            + from.positions[t[2] as usize].as_dvec3())
            / 3.0;
        worst = worst.max(to.distance(c));
    }
    worst
}

/// Approximate symmetric Hausdorff distance between two meshes, in object units.
///
/// Samples the vertices and triangle centroids of each mesh and takes the
/// largest distance to the other surface (exact point-to-triangle distance,
/// accelerated by a uniform grid). It is a lower bound of the true Hausdorff
/// distance: interior surface points that are not samples are not examined.
pub fn mesh_error(reference: &MeshData, simplified: &MeshData) -> Result<f32> {
    reference.validate()?;
    simplified.validate()?;
    let grid_ref = TriangleGrid::new(reference);
    let grid_simp = TriangleGrid::new(simplified);
    let a = one_sided(reference, &grid_simp);
    let b = one_sided(simplified, &grid_ref);
    Ok(a.max(b) as f32)
}

/// Distance helper for tests and callers: nearest distance from a point to a mesh surface.
pub fn distance_to_surface(mesh: &MeshData, point: Vec3) -> Result<f32> {
    mesh.validate()?;
    Ok(TriangleGrid::new(mesh).distance(point.as_dvec3()) as f32)
}
