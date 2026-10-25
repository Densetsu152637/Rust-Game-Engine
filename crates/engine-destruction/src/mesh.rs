//! Input meshes and mesh topology checks.
use crate::hull::convex_hull;
use crate::{Error, Limits, Result};
use glam::{DVec3, Vec3};
use std::collections::{BTreeMap, BTreeSet};

/// An indexed triangle mesh. Closed meshes wind counter-clockwise seen from outside.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
}
/// Vertex, edge and face counts plus closure of a triangle list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    pub vertices: usize,
    pub edges: usize,
    pub faces: usize,
    /// Every edge is used exactly once in each direction (closed and consistently wound).
    pub watertight: bool,
}
impl Topology {
    /// `V - E + F`; 2 for any closed surface of genus 0.
    pub fn euler_characteristic(&self) -> i64 {
        self.vertices as i64 - self.edges as i64 + self.faces as i64
    }
}
/// Topology of `triangles`, counting only referenced vertices.
pub fn topology(triangles: &[[u32; 3]]) -> Topology {
    let mut directed: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    let mut vertices = BTreeSet::new();
    for t in triangles {
        for k in 0..3 {
            vertices.insert(t[k]);
            *directed.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
        }
    }
    let watertight = directed
        .iter()
        .all(|(&(a, b), &n)| n == 1 && directed.get(&(b, a)) == Some(&1));
    let edges: BTreeSet<(u32, u32)> = directed
        .keys()
        .map(|&(a, b)| (a.min(b), a.max(b)))
        .collect();
    Topology {
        vertices: vertices.len(),
        edges: edges.len(),
        faces: triangles.len(),
        watertight,
    }
}
impl Mesh {
    pub fn new(positions: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> Self {
        Self {
            positions,
            triangles,
        }
    }
    /// Axis-aligned box centred on the origin.
    pub fn cuboid(half_extents: Vec3) -> Self {
        let positions = (0..8)
            .map(|i| {
                Vec3::new(
                    if i & 1 == 0 { -1. } else { 1. },
                    if i & 2 == 0 { -1. } else { 1. },
                    if i & 4 == 0 { -1. } else { 1. },
                ) * half_extents
            })
            .collect();
        let triangles = vec![
            [1, 3, 7],
            [1, 7, 5],
            [0, 4, 6],
            [0, 6, 2],
            [2, 6, 7],
            [2, 7, 3],
            [0, 1, 5],
            [0, 5, 4],
            [4, 5, 7],
            [4, 7, 6],
            [0, 2, 3],
            [0, 3, 1],
        ];
        Self {
            positions,
            triangles,
        }
    }
    /// Convex hull of `points` as a closed mesh.
    pub fn from_convex_points(points: &[Vec3]) -> Result<Self> {
        let wide: Vec<DVec3> = points.iter().map(|p| p.as_dvec3()).collect();
        let hull = convex_hull(&wide).ok_or(Error::Degenerate("points do not span a volume"))?;
        Ok(Self {
            positions: hull.points.iter().map(|p| p.as_vec3()).collect(),
            triangles: hull.tris,
        })
    }
    pub fn topology(&self) -> Topology {
        topology(&self.triangles)
    }
    pub fn is_watertight(&self) -> bool {
        self.topology().watertight
    }
    /// Signed volume (positive for outward counter-clockwise winding).
    pub fn volume(&self) -> f32 {
        let p = |i: u32| self.positions[i as usize].as_dvec3();
        let sum: f64 = self
            .triangles
            .iter()
            .map(|t| p(t[0]).dot(p(t[1]).cross(p(t[2]))) / 6.)
            .sum();
        sum as f32
    }
    /// Welds positions that are bit-identical, drops degenerate triangles, orients the mesh
    /// outward and verifies that it is closed and manifold.
    pub(crate) fn prepare(&self, limits: &Limits) -> Result<Prepared> {
        if self.triangles.len() < 4 || self.triangles.len() > limits.max_input_triangles {
            return Err(Error::InvalidMesh("triangle count out of range"));
        }
        if self.positions.len() > limits.max_vertices
            || self
                .positions
                .iter()
                .any(|p| !p.is_finite() || p.abs().max_element() > 1e7)
        {
            return Err(Error::InvalidMesh(
                "vertex count or coordinates out of range",
            ));
        }
        let mut welded: BTreeMap<[u32; 3], u32> = BTreeMap::new();
        let mut verts: Vec<DVec3> = Vec::new();
        let mut map = Vec::with_capacity(self.positions.len());
        for p in &self.positions {
            let key = [
                (p.x + 0.).to_bits(),
                (p.y + 0.).to_bits(),
                (p.z + 0.).to_bits(),
            ];
            let index = *welded.entry(key).or_insert_with(|| {
                verts.push(p.as_dvec3());
                (verts.len() - 1) as u32
            });
            map.push(index);
        }
        let mut tris = Vec::with_capacity(self.triangles.len());
        for t in &self.triangles {
            if t.iter().any(|&i| i as usize >= self.positions.len()) {
                return Err(Error::InvalidMesh("triangle index out of range"));
            }
            let t = t.map(|i| map[i as usize]);
            if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                tris.push(t);
            }
        }
        if !topology(&tris).watertight {
            return Err(Error::InvalidMesh(
                "mesh must be closed: every edge shared by exactly two consistently wound triangles",
            ));
        }
        let volume: f64 = tris
            .iter()
            .map(|t| {
                let p = |i: u32| verts[i as usize];
                p(t[0]).dot(p(t[1]).cross(p(t[2]))) / 6.
            })
            .sum();
        if volume < 0. {
            for t in &mut tris {
                t.swap(1, 2);
            }
        }
        let (min, max) = verts
            .iter()
            .fold((DVec3::MAX, DVec3::MIN), |(l, h), p| (l.min(*p), h.max(*p)));
        let scale = (max - min).length();
        if volume.abs() <= 1e-12 * scale.powi(3) || scale <= 0. {
            return Err(Error::InvalidMesh("mesh encloses no volume"));
        }
        Ok(Prepared {
            verts,
            tris,
            min,
            max,
            volume: volume.abs(),
        })
    }
}
/// Welded polygon-soup output: shared vertices, triangles and per-triangle fracture flags.
#[derive(Default)]
pub(crate) struct Welded {
    pub(crate) verts: Vec<DVec3>,
    pub(crate) tris: Vec<[u32; 3]>,
    pub(crate) interior: Vec<bool>,
}
/// A validated, welded, outward-oriented mesh in double precision.
pub(crate) struct Prepared {
    pub(crate) verts: Vec<DVec3>,
    pub(crate) tris: Vec<[u32; 3]>,
    pub(crate) min: DVec3,
    pub(crate) max: DVec3,
    pub(crate) volume: f64,
}
impl Prepared {
    pub(crate) fn scale(&self) -> f64 {
        (self.max - self.min).length()
    }
    /// Odd crossing parity of three skew rays (majority vote).
    pub(crate) fn contains(&self, point: DVec3) -> bool {
        if point.cmplt(self.min).any() || point.cmpgt(self.max).any() {
            return false;
        }
        const RAYS: [DVec3; 3] = [
            DVec3::new(0.577_350_269_19, 0.371_390_676_35, 0.731_124_412_8),
            DVec3::new(-0.282_094_791_77, 0.912_870_929_18, 0.293_093_251_6),
            DVec3::new(0.127_000_127, -0.451_776_695_3, 0.883_163_8),
        ];
        let votes = RAYS
            .iter()
            .filter(|dir| {
                let mut crossings = 0;
                for t in &self.tris {
                    let (a, b, c) = (
                        self.verts[t[0] as usize],
                        self.verts[t[1] as usize],
                        self.verts[t[2] as usize],
                    );
                    let (e1, e2) = (b - a, c - a);
                    let h = dir.cross(e2);
                    let det = e1.dot(h);
                    if det.abs() < 1e-18 {
                        continue;
                    }
                    let s = point - a;
                    let u = s.dot(h) / det;
                    if !(0. ..=1.).contains(&u) {
                        continue;
                    }
                    let q = s.cross(e1);
                    let v = dir.dot(q) / det;
                    if v < 0. || u + v > 1. {
                        continue;
                    }
                    if e2.dot(q) / det > 1e-12 {
                        crossings += 1;
                    }
                }
                crossings % 2 == 1
            })
            .count();
        votes >= 2
    }
}
