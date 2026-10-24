//! Quadric-error-metric edge-collapse simplification (Garland-Heckbert) with
//! attribute-aware costs, boundary and seam preservation, and flip rejection.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::{DMat3, DVec2, DVec3, Vec2, Vec3};

use crate::distance::mesh_error;
use crate::error::{MeshLodError, Result};
use crate::mesh::MeshData;

/// Smallest accepted ratio of triangle double-area to squared longest edge.
const SLIVER_RATIO: f64 = 1e-5;

/// Controls for [`simplify`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimplifyOptions {
    /// Stop once at most this many triangles remain (must be >= 1).
    pub target_triangles: Option<usize>,
    /// Never perform a collapse whose quadric error (object units) exceeds this.
    pub max_error: Option<f32>,
    /// Weight of the normal-deviation penalty, relative to local surface area.
    pub normal_weight: f32,
    /// Weight of the UV-distance penalty, relative to local surface area.
    pub uv_weight: f32,
    /// Weight of the boundary-preserving quadric planes.
    pub boundary_weight: f32,
    /// Forbid any collapse touching an open-boundary vertex.
    pub lock_boundary: bool,
}

impl Default for SimplifyOptions {
    fn default() -> Self {
        Self {
            target_triangles: None,
            max_error: None,
            normal_weight: 1.0,
            uv_weight: 1.0,
            boundary_weight: 1000.0,
            lock_boundary: false,
        }
    }
}

impl SimplifyOptions {
    /// Options that simplify down to `count` triangles.
    pub fn to_triangle_count(count: usize) -> Self {
        Self {
            target_triangles: Some(count),
            ..Self::default()
        }
    }

    /// Options that simplify as far as possible within `error` object units.
    pub fn to_max_error(error: f32) -> Self {
        Self {
            max_error: Some(error),
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<()> {
        if self.target_triangles.is_none() && self.max_error.is_none() {
            return Err(invalid("set target_triangles and/or max_error"));
        }
        if self.target_triangles == Some(0) {
            return Err(invalid("target_triangles must be >= 1"));
        }
        if let Some(e) = self.max_error
            && !(e.is_finite() && e >= 0.0)
        {
            return Err(invalid("max_error must be finite and >= 0"));
        }
        for (name, w) in [
            ("normal_weight", self.normal_weight),
            ("uv_weight", self.uv_weight),
            ("boundary_weight", self.boundary_weight),
        ] {
            if !(w.is_finite() && w >= 0.0) {
                return Err(invalid(&format!("{name} must be finite and >= 0")));
            }
        }
        Ok(())
    }
}

fn invalid(reason: &str) -> MeshLodError {
    MeshLodError::InvalidOption {
        reason: reason.to_string(),
    }
}

/// Outcome of [`simplify`].
#[derive(Debug, Clone, PartialEq)]
pub struct SimplifyResult {
    /// The simplified mesh (compacted, unreferenced vertices removed).
    pub mesh: MeshData,
    /// Approximate symmetric Hausdorff distance to the input, in object units
    /// (see [`mesh_error`]).
    pub achieved_error: f32,
    /// Triangles in the input.
    pub original_triangles: usize,
    /// Triangles in the output.
    pub final_triangles: usize,
    /// `true` when the triangle target (if any) was reached.
    pub reached_target: bool,
}

#[derive(Clone, Copy, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    fn from_plane(n: DVec3, d: f64, w: f64) -> Self {
        Self([
            w * n.x * n.x,
            w * n.x * n.y,
            w * n.x * n.z,
            w * n.x * d,
            w * n.y * n.y,
            w * n.y * n.z,
            w * n.y * d,
            w * n.z * n.z,
            w * n.z * d,
            w * d * d,
        ])
    }

    fn add(&mut self, o: &Quadric) {
        for (a, b) in self.0.iter_mut().zip(o.0) {
            *a += b;
        }
    }

    fn sum(a: &Quadric, b: &Quadric) -> Quadric {
        let mut r = *a;
        r.add(b);
        r
    }

    fn eval(&self, p: DVec3) -> f64 {
        let q = &self.0;
        (q[0] * p.x * p.x
            + 2.0 * q[1] * p.x * p.y
            + 2.0 * q[2] * p.x * p.z
            + 2.0 * q[3] * p.x
            + q[4] * p.y * p.y
            + 2.0 * q[5] * p.y * p.z
            + 2.0 * q[6] * p.y
            + q[7] * p.z * p.z
            + 2.0 * q[8] * p.z
            + q[9])
            .max(0.0)
    }

    fn optimal(&self) -> Option<DVec3> {
        let q = &self.0;
        let m = DMat3::from_cols(
            DVec3::new(q[0], q[1], q[2]),
            DVec3::new(q[1], q[4], q[5]),
            DVec3::new(q[2], q[5], q[7]),
        );
        let trace = q[0] + q[4] + q[7];
        let det = m.determinant();
        if !det.is_finite() || det.abs() <= 1e-10 * trace * trace * trace {
            return None;
        }
        let x = -(m.inverse() * DVec3::new(q[3], q[6], q[8]));
        x.is_finite().then_some(x)
    }
}

#[derive(Clone, Copy)]
struct Collapse {
    keep: u32,
    remove: u32,
    pos: DVec3,
    /// Interpolation parameter from `keep` (0) to `remove` (1) for attributes.
    t: f64,
    /// Pure geometric quadric error (squared length).
    geo: f64,
    /// Ordering cost: geometric error plus attribute penalties.
    total: f64,
}

struct HeapEntry {
    collapse: Collapse,
    ver_keep: u32,
    ver_remove: u32,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    /// Reversed so that `BinaryHeap` pops the cheapest, lowest-index edge first.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .collapse
            .total
            .total_cmp(&self.collapse.total)
            .then_with(|| other.collapse.keep.cmp(&self.collapse.keep))
            .then_with(|| other.collapse.remove.cmp(&self.collapse.remove))
    }
}

struct State {
    pos: Vec<DVec3>,
    normals: Option<Vec<DVec3>>,
    uvs: Option<Vec<DVec2>>,
    quadric: Vec<Quadric>,
    tris: Vec<[u32; 3]>,
    tri_alive: Vec<bool>,
    vert_tris: Vec<Vec<u32>>,
    vert_alive: Vec<bool>,
    boundary: Vec<bool>,
    locked: Vec<bool>,
    version: Vec<u32>,
    live_tris: usize,
    normal_weight: f64,
    uv_weight: f64,
}

fn tri_cross(p: [DVec3; 3]) -> DVec3 {
    (p[1] - p[0]).cross(p[2] - p[0])
}

impl State {
    fn neighbors(&self, v: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for &t in &self.vert_tris[v as usize] {
            for &c in &self.tris[t as usize] {
                if c != v {
                    out.push(c);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn link_ok(&self, a: u32, b: u32, shared: usize, closed: bool) -> bool {
        let na = self.neighbors(a);
        let nb = self.neighbors(b);
        let common = na.iter().filter(|v| nb.binary_search(v).is_ok()).count();
        if common != shared {
            return false;
        }
        // Union of both stars including a and b: forbid collapsing a minimal
        // tetrahedron or single triangle into a degenerate sheet.
        let mut union: Vec<u32> = na.iter().chain(nb.iter()).copied().collect();
        union.push(a);
        union.push(b);
        union.sort_unstable();
        union.dedup();
        union.len() > if closed { 4 } else { 3 }
    }

    fn corner_pos(&self, c: u32, a: u32, b: u32, p: DVec3) -> DVec3 {
        if c == a || c == b {
            p
        } else {
            self.pos[c as usize]
        }
    }

    /// Rejects collapses that flip or degenerate any surviving triangle.
    fn collapse_valid(&self, a: u32, b: u32, p: DVec3) -> bool {
        for v in [a, b] {
            for &t in &self.vert_tris[v as usize] {
                let tri = self.tris[t as usize];
                if tri.contains(&a) && tri.contains(&b) {
                    continue;
                }
                let old = tri_cross(tri.map(|c| self.pos[c as usize]));
                let new_pts = tri.map(|c| self.corner_pos(c, a, b, p));
                let new = tri_cross(new_pts);
                let old_len = old.length();
                let new_len = new.length();
                if old_len <= 0.0 {
                    continue; // already degenerate input, nothing to preserve
                }
                if new.dot(old) <= 0.05 * old_len * new_len {
                    return false;
                }
                let longest = (new_pts[1] - new_pts[0])
                    .length_squared()
                    .max((new_pts[2] - new_pts[1]).length_squared())
                    .max((new_pts[0] - new_pts[2]).length_squared());
                if new_len < SLIVER_RATIO * longest {
                    return false;
                }
            }
        }
        true
    }

    fn mean_area(&self, a: u32, b: u32) -> f64 {
        let mut sum = 0.0;
        let mut n = 0usize;
        for v in [a, b] {
            for &t in &self.vert_tris[v as usize] {
                let tri = self.tris[t as usize];
                if v == b && tri.contains(&a) {
                    continue;
                }
                sum += 0.5 * tri_cross(tri.map(|c| self.pos[c as usize])).length();
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { sum / n as f64 }
    }

    fn evaluate(&self, a: u32, b: u32) -> Option<Collapse> {
        let (ai, bi) = (a as usize, b as usize);
        let (la, lb) = (self.locked[ai], self.locked[bi]);
        if la && lb {
            return None;
        }
        let shared = self.vert_tris[ai]
            .iter()
            .filter(|&&t| self.tris[t as usize].contains(&b))
            .count();
        if shared == 0 || shared > 2 {
            return None;
        }
        let (ba, bb) = (self.boundary[ai], self.boundary[bi]);
        if ba && bb && shared != 1 {
            return None; // chord between boundary vertices would pinch the mesh
        }
        if !self.link_ok(a, b, shared, !ba && !bb) {
            return None;
        }
        let (pa, pb) = (self.pos[ai], self.pos[bi]);
        let mid = (pa + pb) * 0.5;
        let q = Quadric::sum(&self.quadric[ai], &self.quadric[bi]);

        let mut candidates: Vec<(DVec3, f64)> = Vec::with_capacity(4);
        if la {
            candidates.push((pa, 0.0));
        } else if lb {
            candidates.push((pb, 1.0));
        } else if ba && bb {
            candidates.extend([(pa, 0.0), (pb, 1.0), (mid, 0.5)]);
        } else if ba {
            candidates.push((pa, 0.0));
        } else if bb {
            candidates.push((pb, 1.0));
        } else {
            candidates.extend([(pa, 0.0), (pb, 1.0), (mid, 0.5)]);
            if let Some(opt) = q.optimal() {
                let reach = (pb - pa).length() * 3.0;
                if (opt - mid).length() <= reach {
                    let ab = pb - pa;
                    let len2 = ab.length_squared();
                    let t = if len2 > 0.0 {
                        ((opt - pa).dot(ab) / len2).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    candidates.push((opt, t));
                }
            }
        }
        let mut scored: Vec<(f64, usize)> = candidates
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (q.eval(*p), i))
            .collect();
        scored.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        let (geo, idx) = scored
            .into_iter()
            .find(|&(_, i)| self.collapse_valid(a, b, candidates[i].0))?;
        let (pos, t) = candidates[idx];

        let mut penalty = 0.0;
        let area = self.mean_area(a, b);
        if let Some(n) = &self.normals {
            penalty += self.normal_weight * 0.5 * (1.0 - n[ai].dot(n[bi])).max(0.0);
        }
        if let Some(uv) = &self.uvs {
            penalty += self.uv_weight * uv[ai].distance_squared(uv[bi]);
        }
        Some(Collapse {
            keep: a,
            remove: b,
            pos,
            t,
            geo,
            total: geo + penalty * area,
        })
    }

    /// Applies the collapse and returns the number of triangles removed.
    fn apply(&mut self, c: &Collapse) -> usize {
        let (a, b) = (c.keep, c.remove);
        let mut killed = 0;
        let b_tris = std::mem::take(&mut self.vert_tris[b as usize]);
        for t in b_tris {
            if !self.tri_alive[t as usize] {
                continue;
            }
            let tri = self.tris[t as usize];
            if tri.contains(&a) {
                self.tri_alive[t as usize] = false;
                killed += 1;
                for v in tri {
                    self.vert_tris[v as usize].retain(|&x| x != t);
                }
            } else {
                for corner in &mut self.tris[t as usize] {
                    if *corner == b {
                        *corner = a;
                    }
                }
                self.vert_tris[a as usize].push(t);
            }
        }
        let (ai, bi) = (a as usize, b as usize);
        if let Some(n) = &mut self.normals {
            let blended = n[ai] * (1.0 - c.t) + n[bi] * c.t;
            n[ai] = if blended.length_squared() > 1e-18 {
                blended.normalize()
            } else {
                n[ai]
            };
        }
        if let Some(uv) = &mut self.uvs {
            uv[ai] = uv[ai] * (1.0 - c.t) + uv[bi] * c.t;
        }
        self.pos[ai] = c.pos;
        let qb = self.quadric[bi];
        self.quadric[ai].add(&qb);
        self.boundary[ai] |= self.boundary[bi];
        self.locked[ai] |= self.locked[bi];
        self.vert_alive[bi] = false;
        self.version[bi] += 1;
        self.live_tris -= killed;
        killed
    }
}

/// Simplifies `mesh` with quadric edge collapse.
///
/// Guarantees: output triangles never flip relative to their pre-collapse
/// orientation, none are degenerate, open boundaries stay on the boundary,
/// attribute-seam vertices (same position, different normal/UV) are never
/// removed, and identical input gives identical output.
pub fn simplify(mesh: &MeshData, options: &SimplifyOptions) -> Result<SimplifyResult> {
    mesh.validate()?;
    options.validate()?;
    let original_triangles = mesh.triangle_count();

    // Weld bit-identical vertices (sign of zero ignored) and drop degenerate triangles.
    let mut remap = vec![0u32; mesh.positions.len()];
    let mut weld: HashMap<[u32; 8], u32> = HashMap::new();
    let mut src_of: Vec<usize> = Vec::new();
    let bits = |f: f32| (f + 0.0).to_bits();
    for (i, p) in mesh.positions.iter().enumerate() {
        let n = mesh.normals.as_ref().map_or(Vec3::ZERO, |n| n[i]);
        let uv = mesh.uvs.as_ref().map_or(Vec2::ZERO, |u| u[i]);
        let key = [
            bits(p.x),
            bits(p.y),
            bits(p.z),
            bits(n.x),
            bits(n.y),
            bits(n.z),
            bits(uv.x),
            bits(uv.y),
        ];
        let next = src_of.len() as u32;
        let id = *weld.entry(key).or_insert_with(|| {
            src_of.push(i);
            next
        });
        remap[i] = id;
    }
    let tris: Vec<[u32; 3]> = mesh
        .indices
        .chunks_exact(3)
        .map(|t| [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]])
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    if tris.is_empty() {
        return Err(MeshLodError::EmptyMesh);
    }

    // Work relative to the bounding-box centre for numerical robustness.
    let center = mesh.bounds().map_or(Vec3::ZERO, |b| b.center()).as_dvec3();
    let nv = src_of.len();
    let pos: Vec<DVec3> = src_of
        .iter()
        .map(|&i| mesh.positions[i].as_dvec3() - center)
        .collect();
    let normals = mesh.normals.as_ref().map(|n| {
        src_of
            .iter()
            .map(|&i| n[i].as_dvec3().normalize_or_zero())
            .collect::<Vec<_>>()
    });
    let uvs = mesh
        .uvs
        .as_ref()
        .map(|u| src_of.iter().map(|&i| u[i].as_dvec2()).collect::<Vec<_>>());

    let mut vert_tris = vec![Vec::new(); nv];
    for (t, tri) in tris.iter().enumerate() {
        for &v in tri {
            vert_tris[v as usize].push(t as u32);
        }
    }

    // Edge table: (min, max, tri) sorted, grouped to find boundary / non-manifold edges.
    let mut edges: Vec<(u32, u32, u32)> = Vec::with_capacity(tris.len() * 3);
    for (t, tri) in tris.iter().enumerate() {
        for k in 0..3 {
            let (u, v) = (tri[k], tri[(k + 1) % 3]);
            edges.push((u.min(v), u.max(v), t as u32));
        }
    }
    edges.sort_unstable();

    let mut boundary = vec![false; nv];
    let mut locked = vec![false; nv];
    let mut quadric = vec![Quadric::default(); nv];
    let mut unique_edges: Vec<(u32, u32)> = Vec::new();

    for tri in &tris {
        let n = tri_cross(tri.map(|c| pos[c as usize]));
        let len = n.length();
        if len <= 0.0 {
            continue;
        }
        let n = n / len;
        let d = -n.dot(pos[tri[0] as usize]);
        let q = Quadric::from_plane(n, d, 1.0);
        for &v in tri {
            quadric[v as usize].add(&q);
        }
    }

    let mut i = 0;
    while i < edges.len() {
        let (u, v, _) = edges[i];
        let mut j = i;
        while j < edges.len() && edges[j].0 == u && edges[j].1 == v {
            j += 1;
        }
        unique_edges.push((u, v));
        match j - i {
            1 => {
                boundary[u as usize] = true;
                boundary[v as usize] = true;
                let tri = tris[edges[i].2 as usize];
                let n = tri_cross(tri.map(|c| pos[c as usize])).normalize_or_zero();
                let e = pos[v as usize] - pos[u as usize];
                let m = e.cross(n).normalize_or_zero();
                if m != DVec3::ZERO {
                    let d = -m.dot(pos[u as usize]);
                    let q = Quadric::from_plane(m, d, f64::from(options.boundary_weight));
                    quadric[u as usize].add(&q);
                    quadric[v as usize].add(&q);
                }
            }
            2 => {}
            _ => {
                locked[u as usize] = true;
                locked[v as usize] = true;
            }
        }
        i = j;
    }

    // Seam vertices: referenced vertices that share a position with another vertex.
    let mut referenced = vec![false; nv];
    for tri in &tris {
        for &v in tri {
            referenced[v as usize] = true;
        }
    }
    let mut position_count: HashMap<[u32; 3], u32> = HashMap::new();
    let pkey = |v: usize| {
        let p = mesh.positions[src_of[v]];
        [bits(p.x), bits(p.y), bits(p.z)]
    };
    for v in (0..nv).filter(|&v| referenced[v]) {
        *position_count.entry(pkey(v)).or_insert(0) += 1;
    }
    for v in (0..nv).filter(|&v| referenced[v]) {
        if position_count[&pkey(v)] > 1 {
            locked[v] = true;
        }
    }
    if options.lock_boundary {
        for v in 0..nv {
            if boundary[v] {
                locked[v] = true;
            }
        }
    }

    let live = tris.len();
    let mut state = State {
        pos,
        normals,
        uvs,
        quadric,
        tri_alive: vec![true; tris.len()],
        tris,
        vert_tris,
        vert_alive: vec![true; nv],
        boundary,
        locked,
        version: vec![0; nv],
        live_tris: live,
        normal_weight: f64::from(options.normal_weight),
        uv_weight: f64::from(options.uv_weight),
    };

    let max_geo = options
        .max_error
        .map(|e| f64::from(e) * f64::from(e))
        .unwrap_or(f64::INFINITY);
    let target = options.target_triangles.unwrap_or(0);

    let mut heap = BinaryHeap::new();
    for &(u, v) in &unique_edges {
        if let Some(c) = state.evaluate(u, v) {
            heap.push(HeapEntry {
                collapse: c,
                ver_keep: 0,
                ver_remove: 0,
            });
        }
    }

    while state.live_tris > target {
        let Some(entry) = heap.pop() else { break };
        let c = entry.collapse;
        let (a, b) = (c.keep as usize, c.remove as usize);
        if !state.vert_alive[a]
            || !state.vert_alive[b]
            || state.version[a] != entry.ver_keep
            || state.version[b] != entry.ver_remove
            || c.geo > max_geo
        {
            continue;
        }
        state.apply(&c);

        // Refresh every edge whose validity or cost could have changed.
        let mut ring = state.neighbors(c.keep);
        ring.push(c.keep);
        ring.sort_unstable();
        ring.dedup();
        for &v in &ring {
            state.version[v as usize] += 1;
        }
        let mut refresh: Vec<(u32, u32)> = Vec::new();
        for &v in &ring {
            for w in state.neighbors(v) {
                refresh.push((v.min(w), v.max(w)));
            }
        }
        refresh.sort_unstable();
        refresh.dedup();
        for (u, v) in refresh {
            if let Some(nc) = state.evaluate(u, v) {
                heap.push(HeapEntry {
                    ver_keep: state.version[nc.keep as usize],
                    ver_remove: state.version[nc.remove as usize],
                    collapse: nc,
                });
            }
        }
    }

    // Compact output in triangle order (deterministic).
    let mut new_id = vec![u32::MAX; nv];
    let mut out = MeshData::default();
    let mut out_normals = state.normals.as_ref().map(|_| Vec::new());
    let mut out_uvs = state.uvs.as_ref().map(|_| Vec::new());
    for (t, tri) in state.tris.iter().enumerate() {
        if !state.tri_alive[t] {
            continue;
        }
        for &v in tri {
            let vi = v as usize;
            if new_id[vi] == u32::MAX {
                new_id[vi] = out.positions.len() as u32;
                out.positions.push((state.pos[vi] + center).as_vec3());
                if let (Some(dst), Some(src)) = (out_normals.as_mut(), state.normals.as_ref()) {
                    dst.push(src[vi].as_vec3());
                }
                if let (Some(dst), Some(src)) = (out_uvs.as_mut(), state.uvs.as_ref()) {
                    dst.push(src[vi].as_vec2());
                }
            }
            out.indices.push(new_id[vi]);
        }
    }
    out.normals = out_normals;
    out.uvs = out_uvs;

    let final_triangles = out.triangle_count();
    let achieved_error = mesh_error(mesh, &out)?;
    Ok(SimplifyResult {
        achieved_error,
        original_triangles,
        final_triangles,
        reached_target: options.target_triangles.is_none_or(|t| final_triangles <= t),
        mesh: out,
    })
}
