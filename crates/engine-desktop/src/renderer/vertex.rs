//! The 48-byte sprite vertex record and its backend-neutral layout.

use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use engine_project::content::Mesh;
use engine_render::{MeshResource, VertexAttribute, VertexFormat, VertexLayout};

/// `position: vec3`, `uv: vec2`, `color: vec4`, tightly packed.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GpuVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub normal: [f32; 3],
}

/// Bytes per vertex.
pub(crate) const VERTEX_STRIDE: usize = std::mem::size_of::<GpuVertex>();

/// The layout the textured shader's `vs_main` inputs expect.
pub(crate) fn vertex_layout() -> VertexLayout {
    VertexLayout::new(
        VERTEX_STRIDE as u32,
        [
            VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: std::mem::offset_of!(GpuVertex, position) as u32,
            },
            VertexAttribute {
                location: 1,
                format: VertexFormat::Float32x2,
                offset: std::mem::offset_of!(GpuVertex, uv) as u32,
            },
            VertexAttribute {
                location: 2,
                format: VertexFormat::Float32x4,
                offset: std::mem::offset_of!(GpuVertex, color) as u32,
            },
            VertexAttribute {
                location: 3,
                format: VertexFormat::Float32x3,
                offset: 36,
            },
        ],
    )
}

/// Convert a CPU mesh into the neutral resource description. The mesh must not
/// be empty (the adapter rejects empty meshes).
pub(crate) fn mesh_resource(mesh: &Mesh) -> Result<MeshResource> {
    ensure!(
        (mesh.normals.is_empty() || mesh.normals.len() == mesh.vertices.len())
            && mesh.normals.iter().flatten().all(|n| n.is_finite()),
        "invalid mesh normals"
    );
    let vertices: Vec<GpuVertex> = mesh
        .vertices
        .iter()
        .enumerate()
        .map(|(index, v)| GpuVertex {
            position: v.position,
            uv: v.uv,
            color: v.color,
            normal: mesh.normals.get(index).copied().unwrap_or([0.0; 3]),
        })
        .collect();
    Ok(MeshResource::new(
        bytemuck::cast_slice(&vertices).to_vec(),
        vertex_layout(),
        mesh.indices.clone(),
    )?)
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_vertex_is_a_tightly_packed_48_byte_record() {
        assert_eq!(size_of::<GpuVertex>(), 48);
        assert_eq!(align_of::<GpuVertex>(), 4);
    }

    #[test]
    fn gpu_vertex_fields_sit_at_the_offsets_the_pipeline_assumes() {
        // Float32x3, Float32x2, Float32x4 pack 12, 8, 16 bytes.
        assert_eq!(offset_of!(GpuVertex, position), 0);
        assert_eq!(offset_of!(GpuVertex, uv), 12);
        assert_eq!(offset_of!(GpuVertex, color), 20);
        assert_eq!(36 + size_of::<[f32; 3]>(), size_of::<GpuVertex>());
    }

    #[test]
    fn gpu_vertex_bytes_are_little_endian_floats_in_declaration_order() {
        let vertex = GpuVertex {
            position: [1.0, 2.0, 3.0],
            uv: [0.5, 0.25],
            color: [0.0, 1.0, -1.0, 8.0],
            normal: [0.0; 3],
        };
        let bytes = bytemuck::bytes_of(&vertex);
        let floats: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(
            floats,
            [1.0, 2.0, 3.0, 0.5, 0.25, 0.0, 1.0, -1.0, 8.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn vertex_slices_cast_without_padding_between_elements() {
        let vertices = [GpuVertex::zeroed(); 5];
        assert_eq!(bytemuck::cast_slice::<_, u8>(&vertices).len(), 5 * 48);
        assert!(
            bytemuck::cast_slice::<_, u8>(&vertices)
                .iter()
                .all(|b| *b == 0)
        );
    }

    #[test]
    fn project_vertex_layout_matches_the_gpu_vertex_field_for_field() {
        let source = engine_project::content::Vertex {
            position: [1.0, 2.0, 3.0],
            uv: [4.0, 5.0],
            color: [6.0, 7.0, 8.0, 9.0],
        };
        let gpu = GpuVertex {
            position: source.position,
            uv: source.uv,
            color: source.color,
            normal: [0.0; 3],
        };
        let floats: &[f32] = bytemuck::cast_slice(bytemuck::bytes_of(&gpu));
        assert_eq!(
            floats,
            [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn shader_vertex_inputs_match_the_gpu_vertex_attributes() {
        let module = naga::front::wgsl::parse_str(include_str!("../textured.wgsl")).unwrap();
        let vertex = module
            .entry_points
            .iter()
            .find(|e| e.name == "vs_main")
            .unwrap();
        let inputs: Vec<(u32, u32, naga::ScalarKind)> = vertex
            .function
            .arguments
            .iter()
            .map(|argument| {
                let Some(naga::Binding::Location { location, .. }) = argument.binding else {
                    panic!("vertex input without a location");
                };
                let naga::TypeInner::Vector { size, scalar } = module.types[argument.ty].inner
                else {
                    panic!("vertex input must be a vector");
                };
                assert_eq!(scalar.width, 4, "attributes are 32-bit");
                (location, size as u32, scalar.kind)
            })
            .collect();
        assert_eq!(
            inputs,
            [
                (0, 3, naga::ScalarKind::Float),
                (1, 2, naga::ScalarKind::Float),
                (2, 4, naga::ScalarKind::Float),
            ]
        );
        let stride: u32 = inputs.iter().map(|(_, components, _)| components * 4).sum();
        assert_eq!(
            stride as usize, 36,
            "legacy shader inputs retain their offsets"
        );
    }

    #[test]
    fn neutral_layout_is_valid_and_matches_the_shader_interface() {
        let layout = vertex_layout();
        layout.validate().unwrap();
        assert_eq!(layout.stride as usize, size_of::<GpuVertex>());
        let offsets: Vec<(u32, u32)> = layout
            .attributes
            .iter()
            .map(|a| (a.location, a.offset))
            .collect();
        assert_eq!(offsets, [(0, 0), (1, 12), (2, 20), (3, 36)]);
        let interface =
            engine_render::ShaderInterface::reflect(&engine_render::ShaderResource::new(
                "textured",
                engine_render::ShaderSource::wgsl(include_str!("../textured.wgsl")),
            ))
            .unwrap();
        interface.check_vertex_layout(&layout).unwrap();
    }

    #[test]
    fn mesh_resource_carries_vertices_and_indices_verbatim() {
        let mesh = Mesh {
            normals: vec![],
            vertices: vec![
                engine_project::content::Vertex {
                    position: [0.0, 0.0, 0.0],
                    uv: [0.0, 0.0],
                    color: [1.0; 4],
                };
                3
            ],
            indices: vec![0, 1, 2],
        };
        let resource = mesh_resource(&mesh).unwrap();
        assert_eq!(resource.vertex_count, 3);
        assert_eq!(resource.vertices.len(), 3 * 48);
        assert_eq!(resource.indices, [0, 1, 2]);
        assert_eq!(resource.draw_count(), 3);
        assert!(
            mesh_resource(&Mesh::default()).is_err(),
            "empty meshes are rejected"
        );
    }
}
