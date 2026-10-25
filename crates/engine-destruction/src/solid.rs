//! Indexed closed solid with half-space clipping and cap generation.
use crate::Result;
use crate::triangulate;
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Tri {
    pub(crate) v: [u32; 3],
    /// Fracture (cap) surface rather than original surface.
    pub(crate) interior: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Solid {
    pub(crate) verts: Vec<DVec3>,
    pub(crate) tris: Vec<Tri>,
}
fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}
impl Solid {
    pub(crate) fn new(verts: Vec<DVec3>, tris: &[[u32; 3]]) -> Self {
        Self {
            verts,
            tris: tris.iter().map(|&v| Tri { v, interior: false }).collect(),
        }
    }
    /// Drops vertices no triangle uses.
    pub(crate) fn compact(&mut self) {
        let mut remap = vec![u32::MAX; self.verts.len()];
        let mut verts = Vec::new();
        for tri in &mut self.tris {
            for slot in &mut tri.v {
                if remap[*slot as usize] == u32::MAX {
                    remap[*slot as usize] = verts.len() as u32;
                    verts.push(self.verts[*slot as usize]);
                }
                *slot = remap[*slot as usize];
            }
        }
        self.verts = verts;
    }
    /// Keeps the part with `normal . p <= offset` (`normal` is unit length) and closes the cut
    /// with a cap whose outward normal is `normal`. Vertices within `eps` of the plane lie on it.
    pub(crate) fn clip(&mut self, normal: DVec3, offset: f64, eps: f64, scale: f64) -> Result<()> {
        let mut dist: Vec<f64> = self
            .verts
            .iter()
            .map(|p| {
                let d = normal.dot(*p) - offset;
                if d.abs() <= eps { 0. } else { d }
            })
            .collect();
        if dist.iter().all(|d| *d <= 0.) {
            return Ok(());
        }
        if dist.iter().all(|d| *d >= 0.) {
            self.tris.clear();
            return Ok(());
        }
        let mut cuts: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        let mut out: Vec<Tri> = Vec::with_capacity(self.tris.len());
        let tris = std::mem::take(&mut self.tris);
        for tri in &tris {
            let d = tri.v.map(|i| dist[i as usize]);
            if d.iter().all(|x| *x == 0.) {
                // A face lying in the cut plane bounds the kept part only when it faces the
                // same way as the cut; otherwise the solid is on the discarded side of it.
                let [a, b, c] = tri.v.map(|i| self.verts[i as usize]);
                if (b - a).cross(c - a).dot(normal) > 0. {
                    out.push(*tri);
                }
                continue;
            }
            if d.iter().all(|x| *x <= 0.) {
                out.push(*tri);
                continue;
            }
            if d.iter().all(|x| *x >= 0.) {
                continue;
            }
            let mut poly: Vec<u32> = Vec::with_capacity(4);
            for k in 0..3 {
                let (a, b) = (tri.v[k], tri.v[(k + 1) % 3]);
                let (da, db) = (dist[a as usize], dist[b as usize]);
                if da <= 0. {
                    poly.push(a);
                }
                if da < 0. && db > 0. || da > 0. && db < 0. {
                    let key = (a.min(b), a.max(b));
                    let index = match cuts.get(&key) {
                        Some(&index) => index,
                        None => {
                            let (p, q) = (self.verts[key.0 as usize], self.verts[key.1 as usize]);
                            let (dp, dq) = (dist[key.0 as usize], dist[key.1 as usize]);
                            let t = dp / (dp - dq);
                            self.verts.push(p + (q - p) * t);
                            dist.push(0.);
                            let index = (self.verts.len() - 1) as u32;
                            cuts.insert(key, index);
                            index
                        }
                    };
                    poly.push(index);
                }
            }
            poly.dedup();
            while poly.len() > 1 && poly.first() == poly.last() {
                poly.pop();
            }
            for k in 1..poly.len().saturating_sub(1) {
                out.push(Tri {
                    v: [poly[0], poly[k], poly[k + 1]],
                    interior: tri.interior,
                });
            }
        }
        // Open boundary edges of the clipped surface are exactly the cut outline.
        let edges: BTreeSet<(u32, u32)> = out
            .iter()
            .flat_map(|t| (0..3).map(|k| (t.v[k], t.v[(k + 1) % 3])))
            .collect();
        let boundary: Vec<(u32, u32)> = edges
            .iter()
            .filter(|(a, b)| !edges.contains(&(*b, *a)))
            .map(|&(a, b)| (b, a))
            .collect();
        self.tris = out;
        if !boundary.is_empty() {
            let rings = triangulate::loops(&boundary)?;
            for tri in triangulate::triangulate(&rings, &self.verts, normal, scale)? {
                self.tris.push(Tri {
                    v: tri,
                    interior: true,
                });
            }
        }
        self.compact();
        Ok(())
    }
    /// Splits into edge-connected pieces, ordered by their first triangle.
    pub(crate) fn components(&self) -> Vec<Solid> {
        let n = self.tris.len();
        let mut parent: Vec<usize> = (0..n).collect();
        let mut owner: BTreeMap<(u32, u32), usize> = BTreeMap::new();
        for (index, tri) in self.tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (tri.v[k], tri.v[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                match owner.get(&key) {
                    Some(&other) => {
                        let (x, y) = (find(&mut parent, index), find(&mut parent, other));
                        parent[x.max(y)] = x.min(y);
                    }
                    None => {
                        owner.insert(key, index);
                    }
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<Tri>> = BTreeMap::new();
        for index in 0..n {
            let root = find(&mut parent, index);
            groups.entry(root).or_default().push(self.tris[index]);
        }
        groups
            .into_values()
            .map(|tris| {
                let mut solid = Solid {
                    verts: self.verts.clone(),
                    tris,
                };
                solid.compact();
                solid
            })
            .collect()
    }
}
