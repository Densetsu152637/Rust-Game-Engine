//! CPU chunk meshes of selected LOD nodes: the mesh fallback renderer's geometry.
use crate::heightfield::{Heightfield, SampleRect};
use crate::lod::{LodLayout, LodViewer, SelectedNode, vertex_position, vertex_sample};

/// Geometry of one node in terrain-local coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChunkMesh {
    pub positions: Vec<[f32; 3]>,
    /// Unit sample normals (central differences of the full-resolution heights), so shading
    /// does not change with the level of detail.
    pub normals: Vec<[f32; 3]>,
    /// `(x, z) / extent`: 0..1 across the whole terrain.
    pub uvs: Vec<[f32; 2]>,
    /// Splat colour `sum(weight_i * tint_i)` of the vertex's sample.
    pub colors: Vec<[f32; 4]>,
    /// Triangle list; hole cells and cells outside the grid are omitted.
    pub indices: Vec<u32>,
}
impl ChunkMesh {
    /// Number of triangles.
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Builds `node`'s mesh as seen from `viewer` (the morph factors depend on it).
///
/// Vertices form a `(n + 1) x (n + 1)` grid, `n = layout.node_resolution()`, indexed
/// `j * (n + 1) + i` (`i` along X). Each cell is split along the shared diagonal from its
/// `(x0, z1)` corner to its `(x1, z0)` corner, wound counter-clockwise seen from +Y. A cell of
/// a level-`L` node covers `2^L x 2^L` heightfield cells and is dropped when any of them is a
/// hole, so holes never close at a distance (they may look up to one LOD cell larger).
pub fn build_chunk_mesh(
    field: &Heightfield,
    layout: &LodLayout,
    node: &SelectedNode,
    viewer: &LodViewer,
) -> ChunkMesh {
    let n = layout.node_resolution();
    let side = (n + 1) as usize;
    let extent = field.extent();
    let mut mesh = ChunkMesh {
        positions: Vec::with_capacity(side * side),
        normals: Vec::with_capacity(side * side),
        uvs: Vec::with_capacity(side * side),
        colors: Vec::with_capacity(side * side),
        indices: Vec::with_capacity(n as usize * n as usize * 6),
    };
    for j in 0..=n {
        for i in 0..=n {
            let [x, z] = vertex_sample(field, node, i, j);
            let p = vertex_position(field, layout, node, viewer, i, j);
            mesh.positions.push(p.to_array());
            mesh.normals.push(field.sample_normal(x, z).to_array());
            mesh.uvs.push([p.x / extent[0], p.z / extent[1]]);
            mesh.colors.push(field.sample_color(x, z));
        }
    }
    let (w, h) = (field.columns() - 1, field.rows() - 1);
    for j in 0..n {
        for i in 0..n {
            let x0 = node.origin[0] + i * node.step;
            let z0 = node.origin[1] + j * node.step;
            if x0 >= w || z0 >= h {
                continue;
            }
            if field.any_hole(SampleRect::new([x0, z0], [x0 + node.step, z0 + node.step])) {
                continue;
            }
            let a = (j as usize * side + i as usize) as u32;
            let b = a + side as u32; // (x0, z1)
            let c = a + 1; // (x1, z0)
            let d = b + 1; // (x1, z1)
            mesh.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lod::LodSettings;
    use glam::Vec3;

    #[test]
    fn a_leaf_node_reproduces_the_heightfield_and_skips_holes() {
        let heights = (0..17 * 17)
            .map(|i| ((i % 17) as f32 * 0.3).sin())
            .collect();
        let mut field = Heightfield::from_heights(17, 17, 0.5, heights).unwrap();
        field.set_hole(3, 4, true).unwrap();
        let settings = LodSettings {
            node_resolution: 16,
            ..LodSettings::default()
        };
        let layout = LodLayout::for_heightfield(&field, &settings).unwrap();
        assert_eq!(layout.levels(), 1);
        let selection = layout.select(Vec3::ZERO, field.height_range());
        assert_eq!(selection.nodes.len(), 1);
        let mesh = build_chunk_mesh(&field, &layout, &selection.nodes[0], &selection.viewer);
        assert_eq!(mesh.positions.len(), 17 * 17);
        assert_eq!(mesh.triangles(), 16 * 16 * 2 - 2);
        for (index, p) in mesh.positions.iter().enumerate() {
            let (c, r) = ((index % 17) as u32, (index / 17) as u32);
            assert_eq!(p[1], field.height(c, r));
            assert_eq!([p[0], p[2]], [c as f32 * 0.5, r as f32 * 0.5]);
        }
        // Every triangle faces up.
        for t in mesh.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(mesh.positions[i as usize]));
            assert!((b - a).cross(c - a).y > 0.0);
        }
    }
}
