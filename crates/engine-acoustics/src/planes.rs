//! Plane clusters: coplanar triangles of static instances merged per (quantized
//! world plane, material). They are the mirrors of the image-source method.

use crate::math;
use crate::world::{AcousticWorld, limit};
use crate::{AcousticsError, MaterialId, Mobility, Result};
use std::collections::HashMap;

/// Normal quantum for merging (about 0.006 degrees).
const NORMAL_QUANTUM: f64 = 1e-4;
/// Plane-distance quantum for merging, metres.
const DISTANCE_QUANTUM_M: f64 = 1e-3;

/// One mirror plane: `dot(normal, p) == distance` for points on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneCluster {
    /// Unit normal, sign-canonicalised (largest-magnitude component positive).
    pub normal: [f32; 3],
    pub distance: f32,
    pub material: MaterialId,
    /// Total triangle area in the cluster, m^2.
    pub area_m2: f32,
    pub triangles: u32,
}
impl PlaneCluster {
    /// Mirror image of `p` in this plane.
    pub fn reflect_point(&self, p: [f32; 3]) -> [f32; 3] {
        let n = self.normal.map(f64::from);
        let pd = p.map(f64::from);
        let s = 2.0 * (n[0] * pd[0] + n[1] * pd[1] + n[2] * pd[2] - f64::from(self.distance));
        [
            (pd[0] - s * n[0]) as f32,
            (pd[1] - s * n[1]) as f32,
            (pd[2] - s * n[2]) as f32,
        ]
    }
    /// Signed distance of `p` from the plane.
    pub fn signed_distance(&self, p: [f32; 3]) -> f32 {
        math::dot(self.normal, p) - self.distance
    }
}

pub(crate) struct Discovered {
    pub clusters: Vec<PlaneCluster>,
    /// Per instance, the start of its entries in `tri_cluster` (`u32::MAX` for
    /// dynamic instances).
    pub offsets: Vec<u32>,
    /// Cluster index per (static instance, triangle in leaf order).
    pub tri_cluster: Vec<u32>,
}

pub(crate) fn discover(
    world: &AcousticWorld,
    max_clusters: u32,
    work: &mut u64,
    budget: u64,
) -> Result<Discovered> {
    let mut keys: HashMap<(i64, i64, i64, i64, u16), u32> = HashMap::new();
    let mut clusters: Vec<PlaneCluster> = Vec::new();
    let mut offsets = Vec::with_capacity(world.instances.len());
    let mut tri_cluster = Vec::new();
    for (i, inst) in world.instances.iter().enumerate() {
        if world.records[i].mobility != Mobility::Static || inst.node_offset == u32::MAX {
            offsets.push(u32::MAX);
            continue;
        }
        offsets.push(tri_cluster.len() as u32);
        let mesh = world.meshes[world.records[i].mesh as usize];
        *work += u64::from(mesh.triangle_count);
        if *work > budget {
            return Err(AcousticsError::new(
                "acoustics.world.budget",
                format!("plane clustering exceeded the {budget} operation budget"),
            ));
        }
        let m = &inst.world_from_object;
        for tri in mesh.triangle_offset..mesh.triangle_offset + mesh.triangle_count {
            let g = &world.triangles[tri as usize];
            let a = math::transform_point(m, [g.v0[0], g.v0[1], g.v0[2]]).map(f64::from);
            let b = math::transform_point(m, [g.v1[0], g.v1[1], g.v1[2]]).map(f64::from);
            let c = math::transform_point(m, [g.v2[0], g.v2[1], g.v2[2]]).map(f64::from);
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let mut n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let material = MaterialId(g.v0[3].to_bits() as u16);
            if len <= 0.0 || !len.is_finite() {
                tri_cluster.push(u32::MAX);
                continue;
            }
            n = n.map(|v| v / len);
            // Canonical sign: the largest-magnitude component (first on ties) is
            // positive, so both faces of a double-sided wall share one plane.
            let mut k = 0;
            for j in 1..3 {
                if n[j].abs() > n[k].abs() {
                    k = j;
                }
            }
            if n[k] < 0.0 {
                n = n.map(|v| -v);
            }
            // Snap tiny components so axis-aligned planes merge exactly.
            n = n.map(|v| if v.abs() < 1e-7 { 0.0 } else { v });
            let d = n[0] * a[0] + n[1] * a[1] + n[2] * a[2];
            let key = (
                (n[0] / NORMAL_QUANTUM).round() as i64,
                (n[1] / NORMAL_QUANTUM).round() as i64,
                (n[2] / NORMAL_QUANTUM).round() as i64,
                (d / DISTANCE_QUANTUM_M).round() as i64,
                material.0,
            );
            let area = (0.5 * len) as f32;
            let id = match keys.get(&key) {
                Some(&id) => {
                    let cl = &mut clusters[id as usize];
                    cl.area_m2 += area;
                    cl.triangles += 1;
                    id
                }
                None => {
                    if clusters.len() as u64 >= u64::from(max_clusters) {
                        return Err(limit(format!("more than {max_clusters} plane clusters")));
                    }
                    let id = clusters.len() as u32;
                    clusters.push(PlaneCluster {
                        normal: n.map(|v| v as f32),
                        distance: d as f32,
                        material,
                        area_m2: area,
                        triangles: 1,
                    });
                    keys.insert(key, id);
                    id
                }
            };
            tri_cluster.push(id);
        }
    }
    Ok(Discovered {
        clusters,
        offsets,
        tri_cluster,
    })
}
