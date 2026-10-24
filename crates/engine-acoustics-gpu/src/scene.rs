//! Owned copy of an `AcousticWorld`'s flat GPU layouts, validated before any of
//! it reaches a shader.
//!
//! Index conventions (engine-acoustics `gpu_layout`, mirrored by
//! `shaders/intersect_bvh.wgsl`):
//!
//! - BVH children are absolute indices into the array the tree lives in
//!   (`nodes` for meshes, `top_nodes` for instances) and always follow their
//!   parent. A mesh's root is `nodes[instance.node_offset]`; `u32::MAX` marks a
//!   mesh without triangles, whose instances are not in `top_nodes`.
//! - Mesh leaves index `triangles` absolutely; a mesh's triangles are contiguous
//!   from `instance.triangle_offset`, in leaf order. Instance leaves index
//!   `instances`.
//! - Triangles are stored in object space; `GpuTriangle::v0[3]` holds the
//!   material id as `f32` bits.

use crate::error;
use engine_acoustics::gpu_layout::{BvhNode, GpuInstance, GpuMaterial, GpuTriangle};
use engine_acoustics::{BackendError, GpuWorldView};

/// One distinct mesh referenced by at least one instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeshRange {
    pub node_offset: u32,
    pub triangle_offset: u32,
    pub triangle_count: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FlatScene {
    pub nodes: Vec<BvhNode>,
    pub top_nodes: Vec<BvhNode>,
    pub triangles: Vec<GpuTriangle>,
    pub instances: Vec<GpuInstance>,
    pub materials: Vec<GpuMaterial>,
}

impl FlatScene {
    pub fn from_view(view: &GpuWorldView<'_>) -> Self {
        Self {
            nodes: view.nodes.to_vec(),
            top_nodes: view.top_nodes.to_vec(),
            triangles: view.triangles.to_vec(),
            instances: view.instances.to_vec(),
            materials: view.materials.to_vec(),
        }
    }

    /// Nothing can be hit: no instances, or only instances of empty meshes
    /// (which are not in the instance BVH).
    pub fn is_empty(&self) -> bool {
        self.top_nodes.is_empty()
    }

    /// Checks every index a shader will follow, so out-of-range data is an error
    /// here rather than undefined traversal on the GPU. Returns the distinct
    /// meshes, ordered by first use; `mesh_of_instance[i]` indexes that list.
    pub fn validate(&self) -> Result<(Vec<MeshRange>, Vec<u32>), BackendError> {
        if self.is_empty() {
            return Ok((Vec::new(), vec![NO_MESH; self.instances.len()]));
        }
        if self.instances.len() > (1 << 24) {
            return Err(error::invalid(
                "more than 2^24 instances (TLAS custom data is 24 bits)",
            ));
        }
        for (i, material) in self.materials.iter().enumerate() {
            let values = material
                .reflect
                .iter()
                .chain(&material.transmission)
                .chain(std::iter::once(&material.scattering));
            if values.into_iter().any(|v| !(0.0..=1.0).contains(v)) {
                return Err(error::invalid(format!("material {i} is outside [0, 1]")));
            }
        }
        for (i, triangle) in self.triangles.iter().enumerate() {
            let material = triangle.v0[3].to_bits() as usize;
            if material >= self.materials.len() {
                return Err(error::invalid(format!(
                    "triangle {i} references material {material} of {}",
                    self.materials.len()
                )));
            }
            let coordinates = triangle.v0[..3]
                .iter()
                .chain(&triangle.v1[..3])
                .chain(&triangle.v2[..3]);
            if coordinates.into_iter().any(|v| !v.is_finite()) {
                return Err(error::invalid(format!("triangle {i} is not finite")));
            }
        }
        check_bvh(&self.top_nodes, 0, 0..self.instances.len(), "instance BVH")?;
        let mut meshes: Vec<MeshRange> = Vec::new();
        let mut mesh_of_instance = Vec::with_capacity(self.instances.len());
        for (i, instance) in self.instances.iter().enumerate() {
            if instance
                .world_from_object
                .iter()
                .chain(&instance.object_from_world)
                .any(|v| !v.is_finite())
            {
                return Err(error::invalid(format!(
                    "instance {i} transform is not finite"
                )));
            }
            if instance.node_offset == NO_MESH {
                mesh_of_instance.push(NO_MESH);
                continue;
            }
            let key = (instance.node_offset, instance.triangle_offset);
            let index = match meshes
                .iter()
                .position(|m| (m.node_offset, m.triangle_offset) == key)
            {
                Some(index) => index,
                None => {
                    let first = instance.triangle_offset as usize;
                    let end = check_bvh(
                        &self.nodes,
                        instance.node_offset as usize,
                        first..self.triangles.len(),
                        "mesh BVH",
                    )?;
                    meshes.push(MeshRange {
                        node_offset: instance.node_offset,
                        triangle_offset: instance.triangle_offset,
                        triangle_count: (end - first) as u32,
                    });
                    meshes.len() - 1
                }
            };
            mesh_of_instance.push(index as u32);
        }
        Ok((meshes, mesh_of_instance))
    }
}

/// `GpuInstance::node_offset` of a mesh without triangles.
pub(crate) const NO_MESH: u32 = u32::MAX;

/// Walks the tree rooted at `nodes[root]` (absolute child indices, each larger
/// than its parent, so the walk terminates) and checks that every leaf stays in
/// `primitives`. Returns one past the largest primitive a leaf references.
fn check_bvh(
    nodes: &[BvhNode],
    root: usize,
    primitives: std::ops::Range<usize>,
    what: &str,
) -> Result<usize, BackendError> {
    let mut end = primitives.start;
    let mut stack = vec![root];
    while let Some(index) = stack.pop() {
        let node = nodes
            .get(index)
            .ok_or_else(|| error::invalid(format!("{what} node {index} is out of range")))?;
        if node.min.iter().chain(&node.max).any(|v| v.is_nan()) {
            return Err(error::invalid(format!(
                "{what} node {index} has NaN bounds"
            )));
        }
        if node.count == 0 {
            let left = node.left_or_first as usize;
            if left <= index || left + 1 >= nodes.len() {
                return Err(error::invalid(format!(
                    "{what} node {index} has invalid children {left}"
                )));
            }
            stack.push(left);
            stack.push(left + 1);
        } else {
            let first = node.left_or_first as usize;
            let last = first + node.count as usize;
            if first < primitives.start || last > primitives.end {
                return Err(error::invalid(format!(
                    "{what} node {index} references primitives {first}..{last} outside {primitives:?}"
                )));
            }
            end = end.max(last);
        }
    }
    Ok(end)
}

/// Single-leaf BVH node bounding `triangles` in object space; used by the
/// self-tests, which build their scenes without the CPU builder.
pub(crate) fn leaf_over(triangles: &[GpuTriangle]) -> BvhNode {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for t in triangles {
        for v in [t.v0, t.v1, t.v2] {
            for axis in 0..3 {
                min[axis] = min[axis].min(v[axis]);
                max[axis] = max[axis].max(v[axis]);
            }
        }
    }
    BvhNode {
        min,
        left_or_first: 0,
        max,
        count: triangles.len() as u32,
    }
}
