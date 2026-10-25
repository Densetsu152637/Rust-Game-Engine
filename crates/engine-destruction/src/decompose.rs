//! Splits a closed mesh into convex parts along the planes of its reflex faces.
use crate::convex::Convex;
use crate::mass::mass_properties;
use crate::mesh::Prepared;
use crate::solid::Solid;
use crate::{Error, Result};
use glam::DVec3;
use std::collections::BTreeMap;

/// Most splitting planes tried before the mesh is declared too concave.
pub(crate) const MAX_SPLITS: usize = 32;

/// The face plane `(normal, offset)` most violated by a reflex edge, if any edge is reflex
/// by more than `tol`.
fn worst_reflex(solid: &Solid, tol: f64) -> Option<(DVec3, f64)> {
    let mut apex: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for t in &solid.tris {
        for k in 0..3 {
            apex.insert((t.v[k], t.v[(k + 1) % 3]), t.v[(k + 2) % 3]);
        }
    }
    let p = |i: u32| solid.verts[i as usize];
    let mut worst: Option<(f64, DVec3, f64)> = None;
    for t in &solid.tris {
        let normal = (p(t.v[1]) - p(t.v[0]))
            .cross(p(t.v[2]) - p(t.v[0]))
            .normalize_or_zero();
        if normal == DVec3::ZERO {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
            let Some(&other) = apex.get(&(b, a)) else {
                continue;
            };
            let violation = normal.dot(p(other) - p(a));
            if violation > tol && worst.is_none_or(|(best, _, _)| violation > best) {
                worst = Some((violation, normal, normal.dot(p(a))));
            }
        }
    }
    worst.map(|(_, normal, offset)| (normal, offset))
}
/// Convex parts of the mesh. A convex mesh is one part; otherwise the mesh is cut along the
/// plane of its most reflex face, recursively, until every part is convex. Cut faces are
/// flagged as fracture surface.
pub(crate) fn pieces(prep: &Prepared) -> Result<Vec<Convex>> {
    let scale = prep.scale();
    let (eps, tol) = (1e-10 * scale, 1e-9 * scale);
    let mut queue = vec![Solid::new(prep.verts.clone(), &prep.tris)];
    let mut done: Vec<Solid> = Vec::new();
    let mut splits = 0;
    while let Some(piece) = queue.pop() {
        let Some((normal, offset)) = worst_reflex(&piece, tol) else {
            done.push(piece);
            continue;
        };
        splits += 1;
        if splits > MAX_SPLITS {
            return Err(Error::NonRobust(
                "mesh is too concave: more than 32 splits are needed to make it convex parts",
            ));
        }
        let mut behind = piece.clone();
        behind.clip(normal, offset, eps, scale)?;
        let mut front = piece;
        front.clip(-normal, -offset, eps, scale)?;
        for part in [behind, front] {
            for component in part.components() {
                let tris: Vec<[u32; 3]> = component.tris.iter().map(|t| t.v).collect();
                if mass_properties(&component.verts, &tris).volume > 1e-9 * prep.volume {
                    queue.push(component);
                }
            }
        }
    }
    Ok(done
        .iter()
        .map(|s| Convex::from_triangles(&s.verts, s.tris.iter().map(|t| (t.v, t.interior))))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, Mesh};
    use glam::Vec3;

    fn l_prism() -> Mesh {
        let outline = [(0., 0.), (2., 0.), (2., 1.), (1., 1.), (1., 2.), (0., 2.)];
        let n = outline.len() as u32;
        let mut positions: Vec<Vec3> = outline.iter().map(|p| Vec3::new(p.0, p.1, 0.)).collect();
        positions.extend(outline.iter().map(|p| Vec3::new(p.0, p.1, 1.)));
        let mut triangles = Vec::new();
        for t in [[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5]] {
            triangles.push([t[0] + n, t[1] + n, t[2] + n]);
            triangles.push([t[0], t[2], t[1]]);
        }
        for k in 0..n {
            let next = (k + 1) % n;
            triangles.push([k, next, next + n]);
            triangles.push([k, next + n, k + n]);
        }
        Mesh::new(positions, triangles)
    }
    #[test]
    fn an_l_prism_splits_into_two_convex_parts_that_stay_inside_the_l() {
        let prep = l_prism().prepare(&Limits::default()).unwrap();
        let parts = pieces(&prep).unwrap();
        assert_eq!(parts.len(), 2);
        let mut volumes = Vec::new();
        for part in &parts {
            let welded = part.welded(prep.scale());
            assert!(crate::topology(&welded.tris).watertight);
            volumes.push(mass_properties(&welded.verts, &welded.tris).volume);
            for p in part.faces.iter().flat_map(|f| f.pts.iter()) {
                // Nothing may stick into the notch x > 1, y > 1.
                assert!(!(p.x > 1. + 1e-9 && p.y > 1. + 1e-9), "{p:?}");
            }
        }
        volumes.sort_by(f64::total_cmp);
        assert!((volumes[0] - 1.).abs() < 1e-9 && (volumes[1] - 2.).abs() < 1e-9);
    }
}
