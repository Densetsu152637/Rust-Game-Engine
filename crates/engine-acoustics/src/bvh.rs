//! Deterministic binned-SAH BVH builder over flat [`BvhNode`] arrays.
//!
//! Layout rules shared with the GPU traversal (`intersect_bvh.wgsl`):
//!
//! - Nodes of one tree are contiguous; the root is the first node of the tree.
//! - Interior nodes have `count == 0`; their children are `left_or_first` and
//!   `left_or_first + 1`, stored as absolute indices into the array the tree lives in
//!   (`GpuWorldView::nodes` for meshes, `GpuWorldView::top_nodes` for instances).
//!   Children always have larger indices than their parent.
//! - Mesh leaves have `count` in `1..=MESH_MAX_LEAF` and `left_or_first` is an
//!   absolute index into `GpuWorldView::triangles` (the mesh's triangles are stored in
//!   leaf order starting at `GpuInstance::triangle_offset`).
//! - Instance-level leaves always have `count == 1` and `left_or_first` is the index
//!   into `GpuWorldView::instances` (which is the `InstanceId`).
//!
//! The build is deterministic: 16 bins per axis, axes tried in x, y, z order, the
//! first strictly cheapest split wins, and partitioning is stable (ties keep the
//! input order, which is triangle or instance index order).

use crate::gpu_layout::BvhNode;
use crate::math::Aabb;

/// SAH bins per axis.
pub const SAH_BINS: usize = 16;
/// Largest mesh leaf.
pub const MESH_MAX_LEAF: usize = 4;
/// Cost of one traversal step relative to one primitive test.
const TRAVERSAL_COST: f32 = 1.0;
/// Deepest tree accepted (root at depth 0). A depth-first traversal that pushes both
/// children needs at most `MAX_BVH_DEPTH + 1` stack entries per tree, so one tree
/// fits a 64-entry stack and a combined instance-plus-mesh walk fits 128.
pub const MAX_BVH_DEPTH: usize = 48;

/// Why a build stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    Budget,
    Depth,
}

/// Builds one tree over `bounds` (one box per primitive) appended to `nodes`.
/// `order` receives the primitive permutation in leaf order. `leaf_base` is added to
/// leaf `left_or_first` values; `max_leaf == 1` forces single-primitive leaves.
/// `work` is charged per primitive per visited node; the build stops with
/// `BuildError::Budget` once `work` would exceed `budget`, and with
/// `BuildError::Depth` when a leaf would be deeper than [`MAX_BVH_DEPTH`].
pub fn build(
    bounds: &[Aabb],
    max_leaf: usize,
    leaf_base: u32,
    nodes: &mut Vec<BvhNode>,
    order: &mut Vec<u32>,
    work: &mut u64,
    budget: u64,
) -> Result<(), BuildError> {
    order.clear();
    if bounds.is_empty() {
        return Ok(());
    }
    order.extend(0..bounds.len() as u32);
    let centroids: Vec<[f32; 3]> = bounds.iter().map(Aabb::centroid).collect();
    let root = nodes.len();
    nodes.push(BvhNode::default());
    let mut scratch: Vec<u32> = Vec::with_capacity(bounds.len());
    // (node index, start, end, depth) into `order`.
    let mut stack = vec![(root, 0usize, bounds.len(), 0usize)];
    while let Some((node, start, end, depth)) = stack.pop() {
        let count = end - start;
        *work += count as u64 * 3;
        if *work > budget {
            return Err(BuildError::Budget);
        }
        if depth > MAX_BVH_DEPTH {
            return Err(BuildError::Depth);
        }
        let mut node_box = Aabb::EMPTY;
        let mut centroid_box = Aabb::EMPTY;
        for &p in &order[start..end] {
            node_box = node_box.union(&bounds[p as usize]);
            centroid_box.grow(centroids[p as usize]);
        }
        let split = if count <= 1 {
            None
        } else {
            choose_split(
                &order[start..end],
                bounds,
                &centroids,
                &centroid_box,
                &node_box,
                max_leaf,
            )
        };
        let mid = match split {
            Some(Split::Sah { axis, bin }) => {
                partition(&mut order[start..end], &mut scratch, |p| {
                    bin_index(centroids[p as usize][axis], &centroid_box, axis) <= bin
                }) + start
            }
            Some(Split::Median) => start + count / 2,
            None => {
                nodes[node] = BvhNode {
                    min: node_box.min,
                    left_or_first: leaf_base + start as u32,
                    max: node_box.max,
                    count: count as u32,
                };
                continue;
            }
        };
        let left = nodes.len();
        nodes.push(BvhNode::default());
        nodes.push(BvhNode::default());
        nodes[node] = BvhNode {
            min: node_box.min,
            left_or_first: left as u32,
            max: node_box.max,
            count: 0,
        };
        // Right first so the left subtree is built (and numbered) first.
        stack.push((left + 1, mid, end, depth + 1));
        stack.push((left, start, mid, depth + 1));
    }
    Ok(())
}

enum Split {
    Sah { axis: usize, bin: usize },
    Median,
}

fn bin_index(c: f32, centroid_box: &Aabb, axis: usize) -> usize {
    let extent = centroid_box.max[axis] - centroid_box.min[axis];
    let k = ((c - centroid_box.min[axis]) / extent * SAH_BINS as f32) as usize;
    k.min(SAH_BINS - 1)
}

#[allow(clippy::needless_range_loop)] // The axis indexes several arrays.
fn choose_split(
    prims: &[u32],
    bounds: &[Aabb],
    centroids: &[[f32; 3]],
    centroid_box: &Aabb,
    node_box: &Aabb,
    max_leaf: usize,
) -> Option<Split> {
    let count = prims.len();
    let mut best: Option<(f32, usize, usize)> = None;
    for axis in 0..3 {
        let extent = centroid_box.max[axis] - centroid_box.min[axis];
        if extent <= 0.0 || !extent.is_finite() {
            continue;
        }
        let mut bin_box = [Aabb::EMPTY; SAH_BINS];
        let mut bin_count = [0usize; SAH_BINS];
        for &p in prims {
            let k = bin_index(centroids[p as usize][axis], centroid_box, axis);
            bin_box[k] = bin_box[k].union(&bounds[p as usize]);
            bin_count[k] += 1;
        }
        // Right-to-left suffix areas.
        let mut right_area = [0.0f32; SAH_BINS];
        let mut right_count = [0usize; SAH_BINS];
        let mut acc = Aabb::EMPTY;
        let mut n = 0;
        for k in (1..SAH_BINS).rev() {
            acc = acc.union(&bin_box[k]);
            n += bin_count[k];
            right_area[k] = acc.half_area();
            right_count[k] = n;
        }
        let mut left = Aabb::EMPTY;
        let mut nl = 0;
        for k in 0..SAH_BINS - 1 {
            left = left.union(&bin_box[k]);
            nl += bin_count[k];
            let nr = right_count[k + 1];
            if nl == 0 || nr == 0 {
                continue;
            }
            let cost = left.half_area() * nl as f32 + right_area[k + 1] * nr as f32;
            if best.is_none_or(|(c, _, _)| cost < c) {
                best = Some((cost, axis, k));
            }
        }
    }
    let parent_area = node_box.half_area();
    match best {
        Some((cost, axis, bin)) => {
            let split_cost = if parent_area > 0.0 {
                TRAVERSAL_COST + cost / parent_area
            } else {
                TRAVERSAL_COST
            };
            if count <= max_leaf && count as f32 <= split_cost {
                None
            } else {
                Some(Split::Sah { axis, bin })
            }
        }
        // Coincident centroids: split by index order when too many for one leaf.
        None if count > max_leaf => Some(Split::Median),
        None => None,
    }
}

/// Stable partition; returns the number of items for which `left` holds.
fn partition(items: &mut [u32], scratch: &mut Vec<u32>, left: impl Fn(u32) -> bool) -> usize {
    scratch.clear();
    let mut l = 0;
    for i in 0..items.len() {
        let p = items[i];
        if left(p) {
            items[l] = p;
            l += 1;
        } else {
            scratch.push(p);
        }
    }
    items[l..].copy_from_slice(scratch);
    l
}

/// Recomputes interior boxes bottom-up after leaf boxes changed. Relies on
/// children having larger indices than parents.
pub fn refit(nodes: &mut [BvhNode], leaf_box: impl Fn(&BvhNode) -> Aabb) {
    for i in (0..nodes.len()).rev() {
        let node = nodes[i];
        let b = if node.count > 0 {
            leaf_box(&node)
        } else {
            let l = nodes[node.left_or_first as usize];
            let r = nodes[node.left_or_first as usize + 1];
            Aabb {
                min: l.min,
                max: l.max,
            }
            .union(&Aabb {
                min: r.min,
                max: r.max,
            })
        };
        nodes[i].min = b.min;
        nodes[i].max = b.max;
    }
}
