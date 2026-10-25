//! Convex polyhedron as planar polygon faces, clipped by half-spaces.
//!
//! Faces stay polygons (never triangulated until output), so repeated clipping adds vertices
//! only where a plane crosses an edge. Edge crossings are computed from a canonical endpoint
//! order, so neighbouring faces produce bit-identical points and the output welds exactly.
use crate::mesh::Welded;
use crate::triangulate;
use glam::DVec3;
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct Face {
    /// Counter-clockwise seen from outside.
    pub(crate) pts: Vec<DVec3>,
    /// Fracture (cap) surface rather than original surface.
    pub(crate) interior: bool,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Convex {
    pub(crate) faces: Vec<Face>,
}
fn key(p: DVec3) -> [u64; 3] {
    [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]
}
fn order(a: DVec3, b: DVec3) -> Ordering {
    a.x.total_cmp(&b.x)
        .then(a.y.total_cmp(&b.y))
        .then(a.z.total_cmp(&b.z))
}
/// Where the segment `a`-`b` meets the plane, computed identically from either end.
fn crossing(a: DVec3, da: f64, b: DVec3, db: f64) -> DVec3 {
    let (p, dp, q, dq) = if order(a, b) == Ordering::Greater {
        (b, db, a, da)
    } else {
        (a, da, b, db)
    };
    p + (q - p) * (dp / (dp - dq))
}
fn dedup(poly: &mut Vec<DVec3>) {
    poly.dedup_by(|a, b| key(*a) == key(*b));
    while poly.len() > 1 && key(poly[0]) == key(poly[poly.len() - 1]) {
        poly.pop();
    }
}
impl Convex {
    pub(crate) fn from_triangles(
        verts: &[DVec3],
        tris: impl Iterator<Item = ([u32; 3], bool)>,
    ) -> Self {
        Self {
            faces: tris
                .map(|(t, interior)| Face {
                    pts: t.map(|i| verts[i as usize]).to_vec(),
                    interior,
                })
                .collect(),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
    pub(crate) fn max_distance(&self, point: DVec3) -> f64 {
        self.faces
            .iter()
            .flat_map(|f| f.pts.iter())
            .map(|p| (*p - point).length())
            .fold(0., f64::max)
    }
    /// Keeps `normal . p <= offset` (`normal` is unit length) and caps the cut. Points within
    /// `eps` of the plane count as on it.
    pub(crate) fn clip(&mut self, normal: DVec3, offset: f64, eps: f64) {
        let dist = |p: DVec3| {
            let d = normal.dot(p) - offset;
            if d.abs() <= eps { 0. } else { d }
        };
        let (mut outside, mut inside) = (false, false);
        for p in self.faces.iter().flat_map(|f| f.pts.iter()) {
            let d = dist(*p);
            outside |= d > 0.;
            inside |= d < 0.;
        }
        if !outside {
            return;
        }
        if !inside {
            self.faces.clear();
            return;
        }
        let mut cap: Vec<DVec3> = Vec::new();
        let mut kept = Vec::with_capacity(self.faces.len() + 1);
        for face in std::mem::take(&mut self.faces) {
            let d: Vec<f64> = face.pts.iter().map(|p| dist(*p)).collect();
            if d.iter().all(|x| *x >= 0.) {
                continue;
            }
            if d.iter().all(|x| *x <= 0.) {
                cap.extend(
                    face.pts
                        .iter()
                        .zip(&d)
                        .filter(|(_, x)| **x == 0.)
                        .map(|(p, _)| *p),
                );
                kept.push(face);
                continue;
            }
            let n = face.pts.len();
            let mut poly = Vec::with_capacity(n + 2);
            for k in 0..n {
                let (a, b) = (face.pts[k], face.pts[(k + 1) % n]);
                let (da, db) = (d[k], d[(k + 1) % n]);
                if da <= 0. {
                    poly.push(a);
                    if da == 0. {
                        cap.push(a);
                    }
                }
                if da < 0. && db > 0. || da > 0. && db < 0. {
                    let x = crossing(a, da, b, db);
                    poly.push(x);
                    cap.push(x);
                }
            }
            dedup(&mut poly);
            if poly.len() >= 3 {
                kept.push(Face {
                    pts: poly,
                    interior: face.interior,
                });
            }
        }
        self.faces = kept;
        if let Some(face) = cap_face(cap, normal) {
            self.faces.push(face);
        }
    }
    /// Welded vertices, triangles and per-triangle fracture flags.
    pub(crate) fn welded(&self, scale: f64) -> Welded {
        let mut index: BTreeMap<[u64; 3], u32> = BTreeMap::new();
        let mut out = Welded::default();
        for face in &self.faces {
            let ids: Vec<u32> = face
                .pts
                .iter()
                .map(|p| {
                    *index.entry(key(*p)).or_insert_with(|| {
                        out.verts.push(*p);
                        (out.verts.len() - 1) as u32
                    })
                })
                .collect();
            let start = out.tris.len();
            if ids.len() == 3 {
                out.tris.push([ids[0], ids[1], ids[2]]);
            } else {
                let newell = face
                    .pts
                    .iter()
                    .zip(face.pts.iter().cycle().skip(1))
                    .fold(DVec3::ZERO, |sum, (a, b)| sum + a.cross(*b));
                let tris = if newell.length() > 0. {
                    triangulate::triangulate(
                        std::slice::from_ref(&ids),
                        &out.verts,
                        newell.normalize(),
                        scale,
                    )
                    .unwrap_or_default()
                } else {
                    vec![]
                };
                if tris.is_empty() {
                    out.tris
                        .extend((1..ids.len() - 1).map(|k| [ids[0], ids[k], ids[k + 1]]));
                } else {
                    out.tris.extend(tris);
                }
            }
            out.interior
                .extend(std::iter::repeat_n(face.interior, out.tris.len() - start));
        }
        out
    }
}
/// The cross-section polygon (counter-clockwise about `normal`) of on-plane points.
fn cap_face(mut points: Vec<DVec3>, normal: DVec3) -> Option<Face> {
    points.sort_by(|a, b| order(*a, *b));
    points.dedup_by(|a, b| key(*a) == key(*b));
    if points.len() < 3 {
        return None;
    }
    let centre = points.iter().copied().sum::<DVec3>() / points.len() as f64;
    let helper = if normal.x.abs() < 0.6 {
        DVec3::X
    } else {
        DVec3::Y
    };
    let u = normal.cross(helper).normalize();
    let v = normal.cross(u);
    let angle = |p: DVec3| (v.dot(p - centre)).atan2(u.dot(p - centre));
    points.sort_by(|a, b| angle(*a).total_cmp(&angle(*b)).then(order(*a, *b)));
    let area: f64 = (0..points.len())
        .map(|k| normal.dot(points[k].cross(points[(k + 1) % points.len()])))
        .sum();
    (area > 0.).then_some(Face {
        pts: points,
        interior: true,
    })
}
