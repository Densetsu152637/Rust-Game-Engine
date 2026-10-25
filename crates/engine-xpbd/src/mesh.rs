//! Mesh generators, validation and topology extraction (edges, hinges, surfaces).
use std::collections::{BTreeMap, BTreeSet};

use glam::Vec3;

use crate::{
    Error, MAX_OBJECT_PARTICLES, MAX_OBJECT_TETS, MAX_OBJECT_TRIANGLES, MIN_TET_VOLUME,
    MIN_TRIANGLE_AREA, Result, finite_bounded, particles::validate_positions,
};

/// Most segments per axis accepted by the generators.
const MAX_SEGMENTS: u32 = 1024;

/// Builds a planar grid of `(segments_u + 1) * (segments_v + 1)` vertices spanning
/// `origin + s * u + t * v` for `s, t` in `[0, 1]`.
///
/// Vertex `(i, j)` has index `j * (segments_u + 1) + i`. Cell diagonals alternate in a
/// checkerboard so the mesh has no preferred shear direction and is mirror symmetric for
/// even segment counts. Triangles wind so their normal is `u x v`.
pub fn grid_mesh(
    origin: Vec3,
    u: Vec3,
    v: Vec3,
    segments_u: u32,
    segments_v: u32,
) -> Result<(Vec<Vec3>, Vec<[u32; 3]>)> {
    if !(1..=MAX_SEGMENTS).contains(&segments_u) || !(1..=MAX_SEGMENTS).contains(&segments_v) {
        return Err(Error("grid segment count out of range"));
    }
    let count = (segments_u as usize + 1) * (segments_v as usize + 1);
    if count > MAX_OBJECT_PARTICLES {
        return Err(Error("grid exceeds the per-object particle limit"));
    }
    if !finite_bounded(origin, crate::MAX_COORDINATE)
        || !finite_bounded(u, crate::MAX_COORDINATE)
        || !finite_bounded(v, crate::MAX_COORDINATE)
        || u.cross(v).length() <= 0.0
    {
        return Err(Error("invalid grid origin or axes"));
    }
    let columns = segments_u + 1;
    let mut positions = Vec::with_capacity(count);
    for j in 0..=segments_v {
        for i in 0..=segments_u {
            let s = i as f32 / segments_u as f32;
            let t = j as f32 / segments_v as f32;
            positions.push(origin + u * s + v * t);
        }
    }
    let mut triangles = Vec::with_capacity(segments_u as usize * segments_v as usize * 2);
    for j in 0..segments_v {
        for i in 0..segments_u {
            let a = j * columns + i;
            let b = a + 1;
            let d = a + columns;
            let c = d + 1;
            if (i + j) % 2 == 0 {
                triangles.push([a, b, c]);
                triangles.push([a, c, d]);
            } else {
                triangles.push([a, b, d]);
                triangles.push([b, c, d]);
            }
        }
    }
    Ok((positions, triangles))
}

/// Builds an axis-aligned box from `min` to `max` split into `segments` cells per axis,
/// each cut into six positively oriented tetrahedra (Kuhn subdivision, conforming across
/// cells). Vertex `(i, j, k)` has index `(k * (ny + 1) + j) * (nx + 1) + i`.
pub fn box_tet_mesh(
    min: Vec3,
    max: Vec3,
    segments: [u32; 3],
) -> Result<(Vec<Vec3>, Vec<[u32; 4]>)> {
    if segments.iter().any(|s| !(1..=MAX_SEGMENTS).contains(s)) {
        return Err(Error("box segment count out of range"));
    }
    let [nx, ny, nz] = segments.map(|s| s as usize);
    let count = (nx + 1) * (ny + 1) * (nz + 1);
    if count > MAX_OBJECT_PARTICLES || nx * ny * nz * 6 > MAX_OBJECT_TETS {
        return Err(Error(
            "box exceeds the per-object particle or tetrahedron limit",
        ));
    }
    if !finite_bounded(min, crate::MAX_COORDINATE)
        || !finite_bounded(max, crate::MAX_COORDINATE)
        || !(max - min).cmpgt(Vec3::ZERO).all()
    {
        return Err(Error("invalid box bounds"));
    }
    let size = max - min;
    let index = |i: usize, j: usize, k: usize| ((k * (ny + 1) + j) * (nx + 1) + i) as u32;
    let mut positions = Vec::with_capacity(count);
    for k in 0..=nz {
        for j in 0..=ny {
            for i in 0..=nx {
                let t = Vec3::new(
                    i as f32 / nx as f32,
                    j as f32 / ny as f32,
                    k as f32 / nz as f32,
                );
                positions.push(min + size * t);
            }
        }
    }
    // Kuhn paths from corner 000 to 111, one per axis permutation.
    const PERMUTATIONS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut tets = Vec::with_capacity(nx * ny * nz * 6);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                for permutation in PERMUTATIONS {
                    let mut corner = [i, j, k];
                    let mut tet = [index(i, j, k); 4];
                    for (slot, axis) in permutation.into_iter().enumerate() {
                        corner[axis] += 1;
                        tet[slot + 1] = index(corner[0], corner[1], corner[2]);
                    }
                    if signed_volume(&positions, tet) < 0.0 {
                        tet.swap(1, 2);
                    }
                    tets.push(tet);
                }
            }
        }
    }
    Ok((positions, tets))
}

/// Outward-wound boundary triangles of a tetrahedral mesh: faces that belong to exactly
/// one tetrahedron, in ascending order of their sorted vertex triple. Tetrahedra of either
/// orientation are accepted; degenerate ones are rejected.
pub fn surface_triangles(positions: &[Vec3], tets: &[[u32; 4]]) -> Result<Vec<[u32; 3]>> {
    let oriented = validate_tets(positions, tets)?;
    Ok(boundary(&oriented))
}

pub(crate) fn signed_volume(positions: &[Vec3], tet: [u32; 4]) -> f32 {
    let [a, b, c, d] = tet.map(|i| positions[i as usize]);
    (b - a).dot((c - a).cross(d - a)) / 6.0
}

/// Validates indices and volumes and returns the tetrahedra positively oriented.
pub(crate) fn validate_tets(positions: &[Vec3], tets: &[[u32; 4]]) -> Result<Vec<[u32; 4]>> {
    validate_positions(positions, MAX_OBJECT_PARTICLES, 4)?;
    if tets.is_empty() || tets.len() > MAX_OBJECT_TETS {
        return Err(Error("tetrahedron count out of range"));
    }
    let mut referenced = vec![false; positions.len()];
    let mut seen = BTreeSet::new();
    let mut oriented = Vec::with_capacity(tets.len());
    for tet in tets {
        if tet.iter().any(|&i| i as usize >= positions.len()) {
            return Err(Error("tetrahedron index out of range"));
        }
        let mut key = *tet;
        key.sort_unstable();
        if key.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error("tetrahedron repeats a vertex"));
        }
        if !seen.insert(key) {
            return Err(Error("duplicate tetrahedron"));
        }
        let volume = signed_volume(positions, *tet);
        if !volume.is_finite() || volume.abs() < MIN_TET_VOLUME {
            return Err(Error("degenerate tetrahedron"));
        }
        let mut tet = *tet;
        if volume < 0.0 {
            tet.swap(1, 2);
        }
        for &i in &tet {
            referenced[i as usize] = true;
        }
        oriented.push(tet);
    }
    if referenced.contains(&false) {
        return Err(Error("particle not referenced by any tetrahedron"));
    }
    Ok(oriented)
}

/// Boundary faces of positively oriented tetrahedra, wound outward.
pub(crate) fn boundary(tets: &[[u32; 4]]) -> Vec<[u32; 3]> {
    let mut faces: BTreeMap<[u32; 3], (u32, [u32; 3])> = BTreeMap::new();
    for &[a, b, c, d] in tets {
        for face in [[a, c, b], [a, b, d], [a, d, c], [b, c, d]] {
            let mut key = face;
            key.sort_unstable();
            faces.entry(key).or_insert((0, face)).0 += 1;
        }
    }
    faces
        .into_values()
        .filter(|(count, _)| *count == 1)
        .map(|(_, face)| face)
        .collect()
}

/// Validates a triangle mesh (indices, duplicates, areas, unreferenced vertices).
pub(crate) fn validate_triangles(positions: &[Vec3], triangles: &[[u32; 3]]) -> Result<()> {
    validate_positions(positions, MAX_OBJECT_PARTICLES, 3)?;
    if triangles.is_empty() || triangles.len() > MAX_OBJECT_TRIANGLES {
        return Err(Error("triangle count out of range"));
    }
    let mut referenced = vec![false; positions.len()];
    let mut seen = BTreeSet::new();
    for triangle in triangles {
        if triangle.iter().any(|&i| i as usize >= positions.len()) {
            return Err(Error("triangle index out of range"));
        }
        let mut key = *triangle;
        key.sort_unstable();
        if key[0] == key[1] || key[1] == key[2] {
            return Err(Error("triangle repeats a vertex"));
        }
        if !seen.insert(key) {
            return Err(Error("duplicate triangle"));
        }
        let [a, b, c] = triangle.map(|i| positions[i as usize]);
        let area = 0.5 * (b - a).cross(c - a).length();
        if !area.is_finite() || area < MIN_TRIANGLE_AREA {
            return Err(Error("degenerate triangle"));
        }
        for &i in triangle {
            referenced[i as usize] = true;
        }
    }
    if referenced.contains(&false) {
        return Err(Error("particle not referenced by any triangle"));
    }
    Ok(())
}

/// Unique undirected edges `(low, high)` in ascending order.
pub(crate) fn unique_edges<const N: usize>(elements: &[[u32; N]]) -> Vec<(u32, u32)> {
    let mut edges = BTreeSet::new();
    for element in elements {
        for i in 0..N {
            for j in i + 1..N {
                let (a, b) = (element[i], element[j]);
                edges.insert((a.min(b), a.max(b)));
            }
        }
    }
    edges.into_iter().collect()
}

/// Interior edges shared by exactly two triangles, as `[wing0, wing1, edge0, edge1]`.
/// Boundary and non-manifold edges (more than two triangles) get no hinge.
pub(crate) fn hinges(triangles: &[[u32; 3]]) -> Vec<[u32; 4]> {
    let mut wings: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for &[a, b, c] in triangles {
        for (p, q, opposite) in [(a, b, c), (b, c, a), (c, a, b)] {
            wings
                .entry((p.min(q), p.max(q)))
                .or_default()
                .push(opposite);
        }
    }
    wings
        .into_iter()
        .filter(|(_, opposite)| opposite.len() == 2)
        .map(|((p, q), opposite)| [opposite[0], opposite[1], p, q])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_surface_is_closed_and_outward() {
        let (positions, tets) =
            box_tet_mesh(Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0), [2, 2, 2]).unwrap();
        assert_eq!(positions.len(), 27);
        assert_eq!(tets.len(), 48);
        let total: f32 = tets.iter().map(|t| signed_volume(&positions, *t)).sum();
        assert!((total - 6.0).abs() < 1e-4, "volume {total}");
        assert!(tets.iter().all(|t| signed_volume(&positions, *t) > 0.0));
        let surface = surface_triangles(&positions, &tets).unwrap();
        assert_eq!(surface.len(), 6 * 4 * 2);
        // Divergence theorem: outward faces enclose the box volume.
        let enclosed: f32 = surface
            .iter()
            .map(|f| {
                let [a, b, c] = f.map(|i| positions[i as usize]);
                a.dot(b.cross(c)) / 6.0
            })
            .sum();
        assert!((enclosed - 6.0).abs() < 1e-4, "enclosed {enclosed}");
        // Closed 2-manifold: every surface edge is shared by exactly two faces.
        let mut counts: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for &[a, b, c] in &surface {
            for (p, q) in [(a, b), (b, c), (c, a)] {
                *counts.entry((p.min(q), p.max(q))).or_default() += 1;
            }
        }
        assert!(counts.values().all(|&n| n == 2));
    }

    #[test]
    fn grid_counts_and_winding() {
        let (positions, triangles) = grid_mesh(Vec3::ZERO, Vec3::X, Vec3::Z, 4, 3).unwrap();
        assert_eq!(positions.len(), 20);
        assert_eq!(triangles.len(), 24);
        for &[a, b, c] in &triangles {
            let [a, b, c] = [a, b, c].map(|i| positions[i as usize]);
            assert!(
                (b - a)
                    .cross(c - a)
                    .normalize()
                    .abs_diff_eq(Vec3::X.cross(Vec3::Z), 1e-6)
            );
        }
        assert_eq!(
            hinges(&triangles).len(),
            unique_edges(&triangles).len() - 14
        );
    }
}
