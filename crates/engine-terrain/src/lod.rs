//! Chunked quadtree level of detail (CDLOD-style) with per-vertex geomorphing.
//!
//! A node of level `L` covers `node_resolution << L` sample intervals per side with a regular
//! grid of `node_resolution` x `node_resolution` cells whose vertices are `1 << L` samples
//! apart (`step`). Level 0 is the finest. Selection starts from a grid of top-level roots and
//! subdivides a node of level `L > min_level` while its distance to the viewer is at most
//! `range[L - 1]`; otherwise the node is emitted whole.
//!
//! **Distance.** `d(p) = sqrt(|p.xz - c.xz|^2 + gap^2)`, where `c` is the viewer in terrain-local
//! coordinates and `gap` is how far the viewer is above the highest (or below the lowest)
//! sample. `d` depends only on the horizontal position, so it is the same for every node
//! sharing a vertex and is 1-Lipschitz in `xz`.
//!
//! **Morph.** A vertex of a level-`L` node with sample coordinates `p` moves its height from the
//! fine sample `h(p)` toward the level-`L+1` surface `hc(p)` by
//! `k = clamp((d(p) - start[L]) / (end[L] - start[L]), 0, 1)`, with `end[L] = range[L]`. The
//! coarse height is the coarse triangulation evaluated at the vertex: the sample itself for
//! vertices on the coarse grid, the mean of the two coarse neighbours along X or Z for edge
//! midpoints, and the mean of the coarse diagonal's endpoints `(x - step, z + step)` and
//! `(x + step, z - step)` for cell centres. At `k = 1` the fine node is exactly the coarse
//! surface (the fine diagonals subdivide the coarse ones), so a fully-morphed vertex lies on
//! the coarser surface.
//!
//! **Crack-freedom.** Let node `A` (level `a`) share an edge `E` with node `B` of a coarser level.
//! `B` was not subdivided, so every point of `E` is farther than `range[a]` and `A` is fully
//! morphed there (`k_a = 1`). `A`'s parent `P` was subdivided, so points of `P` are within
//! `range[a] + diag(P)` where `diag` is the horizontal diagonal of a level-`a+1` node; the
//! layout guarantees `start[a+1] >= range[a] + diag(a+1)`, so `B` is unmorphed on `E`
//! (`k_{a+1} = 0`). The same inequality rules out neighbours more than one level apart. Hence
//! `A`'s edge vertices lie on `B`'s edge segments (T-junctions on a straight edge, no gaps).
//! `range[0] = lod_distance * node_resolution * spacing`, `range[L] = range[0] * 2^L`; the
//! inequality needs `lod_distance > 2 * sqrt(2)`, so settings require `lod_distance >= 3`.
//! `tests/lod_cracks.rs` checks both properties numerically.
//!
//! **Grid alignment.** Sides `columns - 1` and `rows - 1` are multiples of the top-level step
//! (`levels` is reduced until they are), so every cell of every node is entirely inside or
//! entirely outside the grid. Sample coordinates are clamped to the grid before evaluation;
//! outside cells collapse onto the border and are dropped by the mesh builders.
//!
//! **Budget.** When the selection would exceed `max_nodes`, the finest level allowed is raised
//! until it fits; this keeps the crack-freedom argument (it only stops subdivision uniformly).
use crate::heightfield::{Heightfield, SampleRect};
use crate::{Error, Result};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Most quadtree levels.
pub const MAX_LEVELS: u32 = 12;
/// Most nodes a selection may hold.
pub const MAX_NODE_BUDGET: u32 = 4096;
/// Morph start used for the top level, which never morphs (`k = 0`).
pub const NO_MORPH: f32 = 1.0e30;

/// Authored level-of-detail settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LodSettings {
    /// Cells per node side: a power of two in 4..=128.
    pub node_resolution: u32,
    /// Quadtree levels; 0 picks enough levels for one root to cover the terrain. The
    /// effective count is reduced until both grid sides are multiples of the top-level step.
    pub levels: u32,
    /// `range[0]` in leaf-node sizes (`node_resolution * spacing`); 3..=1000.
    pub lod_distance: f32,
    /// Fraction of the admissible band below each range used for morphing; (0, 1].
    pub morph_fraction: f32,
    /// Most nodes selected for one view; 1..=4096.
    pub max_nodes: u32,
}
impl Default for LodSettings {
    fn default() -> Self {
        Self {
            node_resolution: 32,
            levels: 0,
            lod_distance: 4.0,
            morph_fraction: 0.5,
            max_nodes: 512,
        }
    }
}
impl LodSettings {
    /// Validates the ranges of every field.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("terrain.lod.invalid", m.to_string()));
        if !self.node_resolution.is_power_of_two() || !(4..=128).contains(&self.node_resolution) {
            return bad("node_resolution must be a power of two within 4..=128");
        }
        if self.levels > MAX_LEVELS {
            return bad("levels must be within 0..=12");
        }
        if !(self.lod_distance.is_finite() && (3.0..=1000.0).contains(&self.lod_distance)) {
            return bad("lod_distance must be within 3..=1000 (crack-free LOD needs > 2.83)");
        }
        if !(self.morph_fraction.is_finite()
            && self.morph_fraction > 0.0
            && self.morph_fraction <= 1.0)
        {
            return bad("morph_fraction must be within (0, 1]");
        }
        if self.max_nodes == 0 || self.max_nodes > MAX_NODE_BUDGET {
            return bad("max_nodes must be within 1..=4096");
        }
        Ok(())
    }
}

/// A quadtree node: level and node coordinate at that level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeKey {
    pub level: u32,
    pub x: u32,
    pub z: u32,
}

/// Derived, validated quadtree layout of one heightfield size.
#[derive(Clone, Debug, PartialEq)]
pub struct LodLayout {
    columns: u32,
    rows: u32,
    spacing: f32,
    node_resolution: u32,
    levels: u32,
    roots: [u32; 2],
    max_nodes: u32,
    ranges: Vec<f32>,
    morph: Vec<[f32; 2]>,
}

/// One node chosen for drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectedNode {
    pub key: NodeKey,
    /// Sample coordinate of the node's first vertex.
    pub origin: [u32; 2],
    /// Samples between neighbouring vertices (`1 << level`).
    pub step: u32,
    /// Morph `[start, end]` distances of the node's level.
    pub morph: [f32; 2],
}

/// The viewer as the morph function sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodViewer {
    /// Viewer position in terrain-local coordinates.
    pub position: Vec3,
    /// Vertical distance from the viewer to the terrain's height range (0 inside it).
    pub gap: f32,
}
impl LodViewer {
    /// A viewer at local `position` over a terrain whose heights span `height_range`.
    pub fn new(position: Vec3, height_range: [f32; 2]) -> Self {
        let gap = (position.y - height_range[1])
            .max(height_range[0] - position.y)
            .max(0.0);
        Self { position, gap }
    }
    /// `d(p)` of a local horizontal position.
    pub fn distance(&self, x: f32, z: f32) -> f32 {
        let dx = x - self.position.x;
        let dz = z - self.position.z;
        (dx * dx + dz * dz + self.gap * self.gap).sqrt()
    }
}

/// Result of [`LodLayout::select`].
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub nodes: Vec<SelectedNode>,
    pub viewer: LodViewer,
    /// Finest level the budget allowed (0 unless the budget was hit).
    pub min_level: u32,
    /// Whether `max_nodes` forced `min_level` above 0.
    pub budget_limited: bool,
}
impl Selection {
    /// Nodes per level, finest first.
    pub fn level_histogram(&self, levels: u32) -> Vec<usize> {
        let mut counts = vec![0; levels as usize];
        for node in &self.nodes {
            counts[node.key.level as usize] += 1;
        }
        counts
    }
}

impl LodLayout {
    /// Layout for a `columns` x `rows` grid with `spacing`.
    pub fn new(columns: u32, rows: u32, spacing: f32, settings: &LodSettings) -> Result<Self> {
        settings.validate()?;
        if columns < 2 || rows < 2 || !(spacing.is_finite() && spacing > 0.0) {
            return Err(Error::new("terrain.size", "invalid grid for a LOD layout"));
        }
        let (w, h) = (columns - 1, rows - 1);
        let n = settings.node_resolution;
        // Both sides must be multiples of the top-level step 2^(levels - 1).
        let aligned = 1 + w.trailing_zeros().min(h.trailing_zeros()).min(31);
        let mut cover = 1;
        while (n << (cover - 1)) < w.max(h) && cover < MAX_LEVELS {
            cover += 1;
        }
        let wanted = if settings.levels == 0 {
            cover
        } else {
            settings.levels
        };
        let levels = wanted.min(aligned).clamp(1, MAX_LEVELS);
        let root = n << (levels - 1);
        let roots = [w.div_ceil(root), h.div_ceil(root)];
        if roots[0] * roots[1] > settings.max_nodes {
            return Err(Error::new(
                "terrain.lod.budget",
                format!(
                    "{} root nodes exceed max_nodes {}; raise max_nodes or node_resolution, or \
                     use sides that are multiples of a larger power of two",
                    roots[0] * roots[1],
                    settings.max_nodes
                ),
            ));
        }
        let leaf = n as f32 * spacing;
        let base = settings.lod_distance * leaf;
        let mut ranges = Vec::with_capacity(levels as usize);
        let mut morph = Vec::with_capacity(levels as usize);
        for level in 0..levels {
            let range = base * (1u64 << level) as f32;
            if level + 1 == levels {
                ranges.push(f32::INFINITY);
                morph.push([NO_MORPH, 2.0 * NO_MORPH]);
                continue;
            }
            let lower = if level == 0 {
                0.0
            } else {
                let diagonal = std::f32::consts::SQRT_2 * leaf * (1u64 << level) as f32;
                ranges[level as usize - 1] + diagonal
            };
            let start = range - settings.morph_fraction * (range - lower);
            ranges.push(range);
            morph.push([start, range]);
        }
        Ok(Self {
            columns,
            rows,
            spacing,
            node_resolution: n,
            levels,
            roots,
            max_nodes: settings.max_nodes,
            ranges,
            morph,
        })
    }
    /// Layout matching `field`'s size.
    pub fn for_heightfield(field: &Heightfield, settings: &LodSettings) -> Result<Self> {
        Self::new(field.columns(), field.rows(), field.spacing(), settings)
    }
    /// Effective number of levels.
    pub fn levels(&self) -> u32 {
        self.levels
    }
    /// Cells per node side.
    pub fn node_resolution(&self) -> u32 {
        self.node_resolution
    }
    /// Top-level node counts `[x, z]`.
    pub fn roots(&self) -> [u32; 2] {
        self.roots
    }
    /// Sample spacing.
    pub fn spacing(&self) -> f32 {
        self.spacing
    }
    /// Samples along X and Z.
    pub fn dims(&self) -> [u32; 2] {
        [self.columns, self.rows]
    }
    /// Selection distance of each level (the top level's is infinite).
    pub fn ranges(&self) -> &[f32] {
        &self.ranges
    }
    /// Morph `[start, end]` of `level`.
    pub fn morph_range(&self, level: u32) -> [f32; 2] {
        self.morph[level as usize]
    }
    /// Samples between vertices at `level`.
    pub fn step(level: u32) -> u32 {
        1 << level
    }
    /// Morph factor of a vertex of `level` at distance `d`.
    pub fn morph_factor(&self, level: u32, distance: f32) -> f32 {
        morph_factor(self.morph[level as usize], distance)
    }
    /// Sample rectangle covered by `key`, clamped to the grid (`max` exclusive, i.e. the last
    /// sample index plus one).
    pub fn node_samples(&self, key: NodeKey) -> SampleRect {
        let size = self.node_resolution << key.level;
        let min = [key.x * size, key.z * size];
        SampleRect::new(
            min,
            [
                (min[0] + size).min(self.columns - 1) + 1,
                (min[1] + size).min(self.rows - 1) + 1,
            ],
        )
    }
    /// Whether any cell of `key` lies inside the grid.
    pub fn node_exists(&self, key: NodeKey) -> bool {
        let size = self.node_resolution << key.level;
        key.x * size < self.columns - 1 && key.z * size < self.rows - 1
    }
    /// The node of `level` containing sample `(column, row)`.
    pub fn node_at(&self, level: u32, column: u32, row: u32) -> NodeKey {
        let size = self.node_resolution << level;
        NodeKey {
            level,
            x: column / size,
            z: row / size,
        }
    }
    fn node_distance(&self, key: NodeKey, viewer: &LodViewer) -> f32 {
        let rect = self.node_samples(key);
        let s = self.spacing;
        let lo = Vec2::new(rect.min[0] as f32 * s, rect.min[1] as f32 * s);
        let hi = Vec2::new((rect.max[0] - 1) as f32 * s, (rect.max[1] - 1) as f32 * s);
        let p = Vec2::new(viewer.position.x, viewer.position.z);
        let d = (lo - p).max(p - hi).max(Vec2::ZERO);
        (d.length_squared() + viewer.gap * viewer.gap).sqrt()
    }
    fn selected(&self, key: NodeKey) -> SelectedNode {
        let size = self.node_resolution << key.level;
        SelectedNode {
            key,
            origin: [key.x * size, key.z * size],
            step: Self::step(key.level),
            morph: self.morph[key.level as usize],
        }
    }
    fn select_from(&self, viewer: &LodViewer, min_level: u32) -> Option<Vec<SelectedNode>> {
        let top = self.levels - 1;
        let mut out = Vec::new();
        let mut stack = Vec::new();
        for z in (0..self.roots[1]).rev() {
            for x in (0..self.roots[0]).rev() {
                stack.push(NodeKey { level: top, x, z });
            }
        }
        while let Some(key) = stack.pop() {
            if key.level <= min_level
                || self.node_distance(key, viewer) > self.ranges[key.level as usize - 1]
            {
                if out.len() as u32 >= self.max_nodes {
                    return None;
                }
                out.push(self.selected(key));
                continue;
            }
            let level = key.level - 1;
            for (dx, dz) in [(1, 1), (0, 1), (1, 0), (0, 0)] {
                let child = NodeKey {
                    level,
                    x: key.x * 2 + dx,
                    z: key.z * 2 + dz,
                };
                if self.node_exists(child) {
                    stack.push(child);
                }
            }
        }
        Some(out)
    }
    /// Selects the nodes to draw for a viewer at local `position` over a terrain whose sample
    /// heights span `height_range`. Deterministic (depth-first, fixed child order).
    pub fn select(&self, position: Vec3, height_range: [f32; 2]) -> Selection {
        let viewer = LodViewer::new(position, height_range);
        for min_level in 0..self.levels {
            if let Some(nodes) = self.select_from(&viewer, min_level) {
                return Selection {
                    nodes,
                    viewer,
                    min_level,
                    budget_limited: min_level > 0,
                };
            }
        }
        unreachable!("the root count is validated against max_nodes")
    }
}

/// `clamp((d - start) / (end - start), 0, 1)`; the GPU feature evaluates the same expression.
pub fn morph_factor(range: [f32; 2], distance: f32) -> f32 {
    ((distance - range[0]) / (range[1] - range[0])).clamp(0.0, 1.0)
}

/// Height and sample coordinate of vertex `(i, j)` of `node` with morph factor `k`. Used by the
/// CPU mesh builder; the WGSL vertex shader mirrors it.
pub fn morphed_height(field: &Heightfield, node: &SelectedNode, i: u32, j: u32, k: f32) -> f32 {
    let [x, z] = vertex_sample(field, node, i, j);
    let fine = field.height(x, z);
    let coarse = coarse_height(field, x, z, node.step);
    fine + k * (coarse - fine)
}

/// Sample coordinate of vertex `(i, j)` of `node`, clamped to the grid.
pub fn vertex_sample(field: &Heightfield, node: &SelectedNode, i: u32, j: u32) -> [u32; 2] {
    [
        (node.origin[0] + i * node.step).min(field.columns() - 1),
        (node.origin[1] + j * node.step).min(field.rows() - 1),
    ]
}

/// Height of the next-coarser surface (vertex spacing `2 * step`) at sample `(x, z)`, which is a
/// vertex of a node with vertex spacing `step`.
pub fn coarse_height(field: &Heightfield, x: u32, z: u32, step: u32) -> f32 {
    let (gx, gz) = (x / step, z / step);
    let (x, z, t) = (x as i64, z as i64, step as i64);
    let s = |a: i64, b: i64| field.height_clamped(a, b);
    match (gx % 2, gz % 2) {
        (0, 0) => s(x, z),
        (1, 0) => 0.5 * (s(x - t, z) + s(x + t, z)),
        (0, _) => 0.5 * (s(x, z - t) + s(x, z + t)),
        _ => 0.5 * (s(x - t, z + t) + s(x + t, z - t)),
    }
}

/// Local position of vertex `(i, j)` of `node` as drawn for `viewer`.
pub fn vertex_position(
    field: &Heightfield,
    layout: &LodLayout,
    node: &SelectedNode,
    viewer: &LodViewer,
    i: u32,
    j: u32,
) -> Vec3 {
    let [x, z] = vertex_sample(field, node, i, j);
    let (px, pz) = (x as f32 * layout.spacing, z as f32 * layout.spacing);
    let k = morph_factor(node.morph, viewer.distance(px, pz));
    Vec3::new(px, morphed_height(field, node, i, j, k), pz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(side: u32, settings: LodSettings) -> LodLayout {
        LodLayout::new(side, side, 1.0, &settings).unwrap()
    }

    #[test]
    fn levels_follow_coverage_alignment_and_settings() {
        let s = LodSettings {
            node_resolution: 16,
            ..LodSettings::default()
        };
        assert_eq!(layout(257, s).levels(), 5); // 16 * 2^4 = 256 covers the grid
        assert_eq!(layout(257, s).roots(), [1, 1]);
        assert_eq!(layout(101, s).levels(), 3); // 100 = 4 * 25: top step at most 4
        assert_eq!(layout(100, s).levels(), 1); // odd sides allow no LOD
        let fixed = LodSettings { levels: 2, ..s };
        assert_eq!(layout(257, fixed).levels(), 2);
        assert_eq!(layout(257, fixed).roots(), [8, 8]);
        let tiny = LodSettings {
            levels: 1,
            max_nodes: 4,
            ..s
        };
        assert!(LodLayout::new(257, 257, 1.0, &tiny).is_err());
        assert!(
            LodSettings {
                lod_distance: 2.5,
                ..s
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn morph_bands_leave_room_for_the_coarser_level() {
        let l = layout(1025, LodSettings::default());
        for level in 1..l.levels() - 1 {
            let [start, end] = l.morph_range(level);
            let diag = std::f32::consts::SQRT_2 * 32.0 * (1u32 << level) as f32;
            assert!(start >= l.ranges()[level as usize - 1] + diag - 1e-3);
            assert!(start < end && end == l.ranges()[level as usize]);
        }
        assert_eq!(l.morph_factor(l.levels() - 1, 1e9), 0.0);
    }

    #[test]
    fn selection_refines_near_the_viewer_and_respects_the_budget() {
        let field = Heightfield::new(513, 513, 1.0).unwrap();
        let settings = LodSettings {
            node_resolution: 16,
            ..LodSettings::default()
        };
        let l = LodLayout::for_heightfield(&field, &settings).unwrap();
        let near = l.select(Vec3::new(10.0, 2.0, 10.0), field.height_range());
        assert!(!near.budget_limited);
        let closest = near
            .nodes
            .iter()
            .find(|n| {
                let r = l.node_samples(n.key);
                (r.min[0]..r.max[0]).contains(&10) && (r.min[1]..r.max[1]).contains(&10)
            })
            .unwrap();
        assert_eq!(closest.key.level, 0);
        // Selected nodes tile the grid exactly once.
        let covered: u64 = near
            .nodes
            .iter()
            .map(|n| (16u64 << n.key.level).pow(2))
            .sum();
        assert_eq!(covered, 512 * 512);
        let high = l.select(Vec3::new(10.0, 5000.0, 10.0), field.height_range());
        assert!(high.nodes.len() < near.nodes.len());
        let tight = LodLayout::for_heightfield(
            &field,
            &LodSettings {
                max_nodes: 8,
                ..settings
            },
        )
        .unwrap();
        let limited = tight.select(Vec3::new(10.0, 2.0, 10.0), field.height_range());
        assert!(limited.budget_limited && limited.nodes.len() <= 8);
    }
}
