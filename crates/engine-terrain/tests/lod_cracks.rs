//! Numerical proof of the LOD contract: for many random viewers, the shared edges of adjacent
//! selected nodes agree, neighbours differ by at most one level, and fully-morphed vertices lie
//! on the next-coarser triangulated surface. Tolerance: 1e-4 local units on heights of +-20.
use engine_terrain::lod::{morphed_height, vertex_position, vertex_sample};
use engine_terrain::noise::fbm;
use engine_terrain::{Heightfield, LodLayout, LodSettings, SelectedNode, triangulated};
use glam::Vec3;

const TOLERANCE: f32 = 1e-4;

fn noisy(side: u32, spacing: f32) -> Heightfield {
    let heights = (0..side * side)
        .map(|i| {
            let (c, r) = ((i % side) as f32, (i / side) as f32);
            20.0 * fbm(c * 0.031, r * 0.027, 11) + 3.0 * fbm(c * 0.31, r * 0.29, 5)
        })
        .collect();
    Heightfield::from_heights(side, side, spacing, heights).unwrap()
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Sample rectangle (inclusive corners) of a node, clamped to the grid.
fn corners(field: &Heightfield, layout: &LodLayout, node: &SelectedNode) -> ([u32; 2], [u32; 2]) {
    let size = layout.node_resolution() * node.step;
    (
        node.origin,
        [
            (node.origin[0] + size).min(field.columns() - 1),
            (node.origin[1] + size).min(field.rows() - 1),
        ],
    )
}

/// Height of `node`'s drawn surface at sample `p` on its boundary (linear along the edge).
fn edge_height(
    field: &Heightfield,
    layout: &LodLayout,
    node: &SelectedNode,
    viewer: &engine_terrain::LodViewer,
    p: [u32; 2],
) -> f32 {
    let t = node.step;
    let (lo, hi) = corners(field, layout, node);
    let local = [p[0] - node.origin[0], p[1] - node.origin[1]];
    let along_z = p[0] == lo[0] || p[0] == hi[0];
    let (i, j) = (local[0] / t, local[1] / t);
    let vertex = |i, j| vertex_position(field, layout, node, viewer, i, j).y;
    if local[0].is_multiple_of(t) && local[1].is_multiple_of(t) {
        return vertex(i, j);
    }
    if along_z {
        let f = (local[1] % t) as f32 / t as f32;
        vertex(i, j) + (vertex(i, j + 1) - vertex(i, j)) * f
    } else {
        let f = (local[0] % t) as f32 / t as f32;
        vertex(i, j) + (vertex(i + 1, j) - vertex(i, j)) * f
    }
}

#[derive(Default, Debug)]
struct Stats {
    selections: usize,
    comparisons: usize,
    coarse_comparisons: usize,
    max_error: f32,
    levels_seen: usize,
    /// Coarse-boundary vertices that would crack without geomorphing (control).
    unmorphed_cracks: usize,
}

fn check_cracks(field: &Heightfield, settings: LodSettings, viewers: usize, seed: u64) -> Stats {
    let layout = LodLayout::for_heightfield(field, &settings).unwrap();
    assert!(layout.levels() >= 4, "levels {}", layout.levels());
    let extent = field.extent();
    let mut rng = Lcg(seed);
    let mut stats = Stats::default();
    for _ in 0..viewers {
        let position = Vec3::new(
            (rng.next() * 1.4 - 0.2) * extent[0],
            rng.next() * 60.0 - 25.0,
            (rng.next() * 1.4 - 0.2) * extent[1],
        );
        let selection = layout.select(position, field.height_range());
        stats.selections += 1;
        let used = selection
            .level_histogram(layout.levels())
            .iter()
            .filter(|&&n| n > 0)
            .count();
        stats.levels_seen = stats.levels_seen.max(used);
        let boxes: Vec<_> = selection
            .nodes
            .iter()
            .map(|n| corners(field, &layout, n))
            .collect();
        let mut buckets: std::collections::HashMap<(u32, u32), Vec<usize>> =
            std::collections::HashMap::new();
        for (index, (lo, hi)) in boxes.iter().enumerate() {
            for bz in lo[1] / 32..=hi[1] / 32 {
                for bx in lo[0] / 32..=hi[0] / 32 {
                    buckets.entry((bx, bz)).or_default().push(index);
                }
            }
        }
        let n = layout.node_resolution();
        for (a, node) in selection.nodes.iter().enumerate() {
            for j in 0..=n {
                for i in 0..=n {
                    if i != 0 && i != n && j != 0 && j != n {
                        continue;
                    }
                    let p = vertex_sample(field, node, i, j);
                    let height = vertex_position(field, &layout, node, &selection.viewer, i, j).y;
                    for &b in &buckets[&(p[0] / 32, p[1] / 32)] {
                        let other = &selection.nodes[b];
                        if a == b {
                            continue;
                        }
                        let (lo, hi) = boxes[b];
                        let inside =
                            (lo[0]..=hi[0]).contains(&p[0]) && (lo[1]..=hi[1]).contains(&p[1]);
                        if !inside {
                            continue;
                        }
                        // Nodes sharing more than a corner differ by at most one level.
                        let (alo, ahi) = boxes[a];
                        let overlap_x = ahi[0].min(hi[0]) as i64 - alo[0].max(lo[0]) as i64;
                        let overlap_z = ahi[1].min(hi[1]) as i64 - alo[1].max(lo[1]) as i64;
                        if overlap_x > 0 || overlap_z > 0 {
                            assert!(
                                node.key.level.abs_diff(other.key.level) <= 1,
                                "levels {:?} and {:?} are adjacent",
                                node.key,
                                other.key
                            );
                        }
                        let expected = edge_height(field, &layout, other, &selection.viewer, p);
                        let error = (height - expected).abs();
                        stats.max_error = stats.max_error.max(error);
                        stats.comparisons += 1;
                        if other.key.level > node.key.level {
                            stats.coarse_comparisons += 1;
                            let unmorphed = morphed_height(field, node, i, j, 0.0);
                            if (unmorphed - expected).abs() > TOLERANCE {
                                stats.unmorphed_cracks += 1;
                            }
                        }
                        assert!(
                            error <= TOLERANCE,
                            "crack {error} at sample {p:?} between {:?} and {:?} (viewer {position})",
                            node.key,
                            other.key
                        );
                    }
                }
            }
        }
    }
    stats
}

#[test]
fn shared_edges_of_adjacent_nodes_agree_for_random_viewers() {
    let field = noisy(257, 1.0);
    for (settings, seed) in [
        (
            LodSettings {
                node_resolution: 8,
                lod_distance: 3.0,
                morph_fraction: 1.0,
                max_nodes: 4096,
                levels: 0,
            },
            1,
        ),
        (
            LodSettings {
                node_resolution: 8,
                lod_distance: 4.0,
                morph_fraction: 0.5,
                max_nodes: 4096,
                levels: 0,
            },
            2,
        ),
        (
            LodSettings {
                node_resolution: 4,
                lod_distance: 3.5,
                morph_fraction: 0.8,
                max_nodes: 4096,
                levels: 0,
            },
            3,
        ),
    ] {
        let stats = check_cracks(&field, settings, 60, seed);
        eprintln!("{settings:?}: {stats:?}");
        assert_eq!(stats.selections, 60);
        assert!(stats.coarse_comparisons > 100, "{stats:?}");
        assert!(stats.levels_seen >= 3, "{stats:?}");
        // Without the morph the same boundaries crack: the check is not vacuous.
        assert!(stats.unmorphed_cracks > 100, "{stats:?}");
    }
}

#[test]
fn budget_limited_selections_stay_crack_free() {
    let field = noisy(257, 0.5);
    let settings = LodSettings {
        node_resolution: 8,
        lod_distance: 3.0,
        morph_fraction: 1.0,
        max_nodes: 40,
        levels: 0,
    };
    let stats = check_cracks(&field, settings, 30, 9);
    eprintln!("budget: {stats:?}");
    assert!(stats.comparisons > 0);
}

#[test]
fn fully_morphed_vertices_lie_on_the_coarser_surface() {
    let field = noisy(257, 1.0);
    let settings = LodSettings {
        node_resolution: 8,
        ..LodSettings::default()
    };
    let layout = LodLayout::for_heightfield(&field, &settings).unwrap();
    let (w, h) = (field.columns() - 1, field.rows() - 1);
    let mut checked = 0;
    let mut max_error = 0.0f32;
    for level in 0..layout.levels() - 1 {
        let step = 1u32 << level;
        let size = 8 * step;
        let coarse = 2 * step;
        for z in 0..(h / size).min(6) {
            for x in 0..(w / size).min(6) {
                let node = SelectedNode {
                    key: engine_terrain::NodeKey { level, x, z },
                    origin: [x * size, z * size],
                    step,
                    morph: layout.morph_range(level),
                };
                for j in 0..=8 {
                    for i in 0..=8 {
                        let [px, pz] = vertex_sample(&field, &node, i, j);
                        let morphed = morphed_height(&field, &node, i, j, 1.0);
                        // Independent evaluation of the coarse triangulation.
                        let cx = ((px / coarse) * coarse).min(w - coarse);
                        let cz = ((pz / coarse) * coarse).min(h - coarse);
                        let u = (px - cx) as f32 / coarse as f32;
                        let v = (pz - cz) as f32 / coarse as f32;
                        let expected = triangulated(
                            field.height(cx, cz),
                            field.height(cx + coarse, cz),
                            field.height(cx, cz + coarse),
                            field.height(cx + coarse, cz + coarse),
                            u,
                            v,
                        );
                        let error = (morphed - expected).abs();
                        max_error = max_error.max(error);
                        assert!(
                            error <= TOLERANCE,
                            "level {level} vertex ({i},{j}): {error}"
                        );
                        // k = 0 reproduces the fine sample exactly.
                        assert_eq!(
                            morphed_height(&field, &node, i, j, 0.0),
                            field.height(px, pz)
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    eprintln!("morph-to-coarse: {checked} vertices, max error {max_error}");
    assert!(checked > 1000);
}
