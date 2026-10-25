//! Incremental 3D convex hull in double precision.
use glam::DVec3;
use std::collections::{BTreeSet, HashSet};

pub(crate) struct Hull {
    pub(crate) points: Vec<DVec3>,
    /// Outward counter-clockwise triangles indexing `points`.
    pub(crate) tris: Vec<[u32; 3]>,
}
struct Face {
    v: [usize; 3],
    normal: DVec3,
}
fn face(points: &[DVec3], v: [usize; 3]) -> Face {
    let n = (points[v[1]] - points[v[0]]).cross(points[v[2]] - points[v[0]]);
    Face {
        v,
        normal: n.normalize_or_zero(),
    }
}
/// Hull of `input`; `None` when fewer than four affinely independent points exist.
pub(crate) fn convex_hull(input: &[DVec3]) -> Option<Hull> {
    let mut seen = BTreeSet::new();
    let pts: Vec<DVec3> = input
        .iter()
        .copied()
        .filter(|p| seen.insert([p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]))
        .collect();
    if pts.len() < 4 {
        return None;
    }
    let (lo, hi) = pts
        .iter()
        .fold((DVec3::MAX, DVec3::MIN), |(l, h), p| (l.min(*p), h.max(*p)));
    let scale = (hi - lo).length();
    if scale <= 0. {
        return None;
    }
    let eps = 1e-10 * scale;
    let i0 = (0..pts.len()).min_by(|&a, &b| pts[a].x.total_cmp(&pts[b].x))?;
    let i1 = (0..pts.len()).max_by(|&a, &b| {
        (pts[a] - pts[i0])
            .length_squared()
            .total_cmp(&(pts[b] - pts[i0]).length_squared())
    })?;
    let axis = pts[i1] - pts[i0];
    if axis.length() <= eps {
        return None;
    }
    let i2 = (0..pts.len()).max_by(|&a, &b| {
        axis.cross(pts[a] - pts[i0])
            .length_squared()
            .total_cmp(&axis.cross(pts[b] - pts[i0]).length_squared())
    })?;
    let plane = axis.cross(pts[i2] - pts[i0]);
    if plane.length() <= eps * scale {
        return None;
    }
    let plane = plane.normalize();
    let i3 = (0..pts.len()).max_by(|&a, &b| {
        plane
            .dot(pts[a] - pts[i0])
            .abs()
            .total_cmp(&plane.dot(pts[b] - pts[i0]).abs())
    })?;
    if plane.dot(pts[i3] - pts[i0]).abs() <= eps {
        return None;
    }
    let simplex = [i0, i1, i2, i3];
    let mut faces: Vec<Face> = Vec::new();
    for (tri, opposite) in [
        ([i0, i1, i2], i3),
        ([i0, i1, i3], i2),
        ([i0, i2, i3], i1),
        ([i1, i2, i3], i0),
    ] {
        let mut f = face(&pts, tri);
        if f.normal.dot(pts[opposite] - pts[tri[0]]) > 0. {
            f = face(&pts, [tri[0], tri[2], tri[1]]);
        }
        faces.push(f);
    }
    for (index, p) in pts.iter().enumerate() {
        if simplex.contains(&index) {
            continue;
        }
        let visible: Vec<usize> = (0..faces.len())
            .filter(|&f| faces[f].normal.dot(*p - pts[faces[f].v[0]]) > eps)
            .collect();
        if visible.is_empty() {
            continue;
        }
        let edges: HashSet<(usize, usize)> = visible
            .iter()
            .flat_map(|&f| {
                let v = faces[f].v;
                [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])]
            })
            .collect();
        let mut horizon: Vec<(usize, usize)> = edges
            .iter()
            .copied()
            .filter(|&(a, b)| !edges.contains(&(b, a)))
            .collect();
        horizon.sort_unstable();
        let mut keep = vec![true; faces.len()];
        for &f in &visible {
            keep[f] = false;
        }
        let mut next = 0;
        faces.retain(|_| {
            next += 1;
            keep[next - 1]
        });
        for (a, b) in horizon {
            faces.push(face(&pts, [a, b, index]));
        }
    }
    let mut remap = vec![u32::MAX; pts.len()];
    let mut points = Vec::new();
    let mut tris = Vec::new();
    for f in &faces {
        let mut tri = [0u32; 3];
        for (slot, &v) in tri.iter_mut().zip(&f.v) {
            if remap[v] == u32::MAX {
                remap[v] = points.len() as u32;
                points.push(pts[v]);
            }
            *slot = remap[v];
        }
        tris.push(tri);
    }
    Some(Hull { points, tris })
}
