//! GPU meshes and textures shared by the built-in mesh path and features.

use anyhow::{Result, ensure};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use wgpu::util::DeviceExt;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The engine vertex: `engine::mesh`'s `EngineVertex` (48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EngineVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// Zero when the mesh has no authored normals.
    pub normal: [f32; 3],
}

const ENGINE_ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x2,
    2 => Float32x4,
    3 => Float32x3,
];

/// Vertex buffer layout of [`EngineVertex`] at buffer slot 0.
pub const ENGINE_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<EngineVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &ENGINE_ATTRIBUTES,
};

/// An uploaded triangle-list mesh. Cheap to share through `Arc`; its `id` is
/// unique for the process and keys instancing batches.
#[derive(Debug)]
pub struct GpuMesh {
    id: u64,
    pub vertex: wgpu::Buffer,
    pub index: Option<wgpu::Buffer>,
    /// Index count when indexed, else vertex count.
    pub count: u32,
    pub vertex_stride: u64,
}

impl GpuMesh {
    /// Upload engine vertices and optional `u32` indices (a triangle list).
    pub fn from_engine_vertices(
        device: &wgpu::Device,
        label: &str,
        vertices: &[EngineVertex],
        indices: &[u32],
    ) -> Result<Arc<Self>> {
        Self::from_bytes(
            device,
            label,
            bytemuck::cast_slice(vertices),
            std::mem::size_of::<EngineVertex>() as u64,
            indices,
        )
    }

    /// Upload raw vertex bytes of any layout (`stride` bytes per vertex).
    pub fn from_bytes(
        device: &wgpu::Device,
        label: &str,
        vertex_bytes: &[u8],
        stride: u64,
        indices: &[u32],
    ) -> Result<Arc<Self>> {
        ensure!(stride > 0, "vertex stride must be positive");
        ensure!(
            !vertex_bytes.is_empty() && vertex_bytes.len() as u64 % stride == 0,
            "mesh vertex data must be a nonzero whole number of vertices"
        );
        let vertices = vertex_bytes.len() as u64 / stride;
        ensure!(
            indices.iter().all(|&i| u64::from(i) < vertices),
            "mesh index exceeds vertex count"
        );
        let count = if indices.is_empty() {
            vertices
        } else {
            indices.len() as u64
        };
        ensure!(
            count % 3 == 0 && count <= u64::from(u32::MAX),
            "meshes are complete triangle lists"
        );
        let vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index = (!indices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        });
        Ok(Arc::new(Self {
            id: next_id(),
            vertex,
            index,
            count: count as u32,
            vertex_stride: stride,
        }))
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Bind the buffers at vertex slot 0 and draw `instances`.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, instances: std::ops::Range<u32>) {
        pass.set_vertex_buffer(0, self.vertex.slice(..));
        match &self.index {
            Some(index) => {
                pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.count, 0, instances);
            }
            None => pass.draw(0..self.count, instances),
        }
    }
}

/// A sampled RGBA8 texture with an sRGB view (colour) and a linear view
/// (data such as normal and metallic-roughness maps).
#[derive(Debug)]
pub struct GpuTexture {
    id: u64,
    pub texture: wgpu::Texture,
    /// `Rgba8UnormSrgb` view: colour sampling decodes sRGB.
    pub view: wgpu::TextureView,
    /// `Rgba8Unorm` view of the same texels: data sampling stays linear.
    pub linear_view: wgpu::TextureView,
    pub size: [u32; 2],
}

/// Usages of every [`GpuTexture`]: sampled, written by uploads and copies,
/// and renderable (stream textures callers draw into).
pub const TEXTURE_USAGES: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::RENDER_ATTACHMENT);

impl GpuTexture {
    /// An empty texture of `size` (contents undefined until written).
    pub fn new(device: &wgpu::Device, label: &str, size: [u32; 2]) -> Result<Arc<Self>> {
        ensure!(size[0] > 0 && size[1] > 0, "texture size must be nonzero");
        let max = device.limits().max_texture_dimension_2d;
        ensure!(
            size[0] <= max && size[1] <= max,
            "texture exceeds the device limit of {max}"
        );
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: TEXTURE_USAGES,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });
        let view = texture.create_view(&Default::default());
        let linear_view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        Ok(Arc::new(Self {
            id: next_id(),
            texture,
            view,
            linear_view,
            size,
        }))
    }

    /// Upload tightly packed sRGB RGBA8 rows (top row first).
    pub fn rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        size: [u32; 2],
        pixels: &[u8],
    ) -> Result<Arc<Self>> {
        let texture = Self::new(device, label, size)?;
        texture.write(queue, pixels)?;
        Ok(texture)
    }

    /// Replace every texel with tightly packed RGBA8 rows.
    pub fn write(&self, queue: &wgpu::Queue, pixels: &[u8]) -> Result<()> {
        let expected = self.size[0] as usize * self.size[1] as usize * 4;
        ensure!(
            pixels.len() == expected,
            "texture upload has {} bytes; {}x{} RGBA8 needs {expected}",
            pixels.len(),
            self.size[0],
            self.size[1]
        );
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.size[0] * 4),
                rows_per_image: Some(self.size[1]),
            },
            wgpu::Extent3d {
                width: self.size[0],
                height: self.size[1],
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}
