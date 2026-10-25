//! GPU helpers shared by the features: the UI vertex, grow-on-demand buffers
//! and the upload cache of CPU texture snapshots.
use anyhow::{Result, ensure};
use engine_project::content::{Mesh, Texture};
use engine_render_pipeline::{GpuContext, GpuTexture};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use wgpu::util::DeviceExt;

/// Vertex of every UI draw: position, uv and straight-alpha colour
/// (`engine_project::content::Vertex`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

pub const UI_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<UiVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4],
};

/// Most vertices one frame's UI meshes may total; larger frames fail with a
/// named error instead of allocating without bound.
pub const MAX_UI_VERTICES: usize = 1 << 21;

/// Geometry of many meshes in one vertex and one index buffer.
#[derive(Debug, Default)]
pub struct Geometry {
    pub vertices: Vec<UiVertex>,
    pub indices: Vec<u32>,
}

/// Where a mesh landed in a [`Geometry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshRange {
    pub first_index: u32,
    pub index_count: u32,
    pub base_vertex: i32,
}

impl Geometry {
    /// Append `mesh`; `None` for an empty or inconsistent mesh (an index out of
    /// range would fault the draw, so the mesh is skipped).
    pub fn push(&mut self, mesh: &Mesh) -> Result<Option<MeshRange>> {
        ensure!(
            self.vertices.len() + mesh.vertices.len() <= MAX_UI_VERTICES,
            "UI draws exceed {MAX_UI_VERTICES} vertices in one frame"
        );
        let count = mesh.vertices.len() as u32;
        if mesh.indices.is_empty() || mesh.indices.iter().any(|&i| i >= count) {
            return Ok(None);
        }
        let range = MeshRange {
            first_index: self.indices.len() as u32,
            index_count: mesh.indices.len() as u32,
            base_vertex: self.vertices.len() as i32,
        };
        self.vertices.extend(mesh.vertices.iter().map(|v| UiVertex {
            position: v.position,
            uv: v.uv,
            color: v.color,
        }));
        self.indices.extend_from_slice(&mesh.indices);
        Ok(Some(range))
    }
}

/// A buffer that grows (doubling) to hold what is written to it.
#[derive(Debug)]
pub struct GrowBuffer {
    label: &'static str,
    usage: wgpu::BufferUsages,
    buffer: Option<wgpu::Buffer>,
    capacity: u64,
}

impl GrowBuffer {
    pub fn new(label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Self {
            label,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            buffer: None,
            capacity: 0,
        }
    }

    /// Replace the contents with `bytes` (at least 4 bytes are allocated).
    pub fn write(&mut self, gpu: &GpuContext, bytes: &[u8]) -> &wgpu::Buffer {
        let needed = (bytes.len() as u64).max(4).next_multiple_of(4);
        if self.buffer.is_none() || needed > self.capacity {
            self.capacity = needed.next_power_of_two().max(256);
            self.buffer = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: self.capacity,
                usage: self.usage,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.buffer.as_ref().expect("allocated above");
        if !bytes.is_empty() {
            let padded = bytes.len().next_multiple_of(4);
            if padded == bytes.len() {
                gpu.queue.write_buffer(buffer, 0, bytes);
            } else {
                let mut copy = bytes.to_vec();
                copy.resize(padded, 0);
                gpu.queue.write_buffer(buffer, 0, &copy);
            }
        }
        buffer
    }

    pub fn get(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }
}

/// Uploads of CPU texture snapshots, keyed by `Arc` identity (the text
/// backend publishes a new `Arc` only when a page changed). The cache holds
/// each source `Arc`, so a pointer is never reused while it is cached.
#[derive(Default)]
pub struct TextureUploads {
    entries: HashMap<usize, (Arc<Texture>, Arc<GpuTexture>)>,
    white: Option<Arc<GpuTexture>>,
}

impl TextureUploads {
    /// The GPU texture of `texture`, uploading it on first use.
    pub fn get(&mut self, gpu: &GpuContext, texture: &Arc<Texture>) -> Result<Arc<GpuTexture>> {
        let key = Arc::as_ptr(texture) as usize;
        if let Some((_, uploaded)) = self.entries.get(&key) {
            return Ok(uploaded.clone());
        }
        let uploaded = GpuTexture::rgba8(
            &gpu.device,
            &gpu.queue,
            "ui texture",
            [texture.width, texture.height],
            &texture.pixels,
        )?;
        self.entries.insert(key, (texture.clone(), uploaded.clone()));
        Ok(uploaded)
    }

    /// A 1x1 opaque white texture for solid-colour meshes.
    pub fn white(&mut self, gpu: &GpuContext) -> Result<Arc<GpuTexture>> {
        if let Some(white) = &self.white {
            return Ok(white.clone());
        }
        let white = GpuTexture::rgba8(
            &gpu.device,
            &gpu.queue,
            "ui white",
            [1, 1],
            &[255, 255, 255, 255],
        )?;
        self.white = Some(white.clone());
        Ok(white)
    }

    /// Drop the uploads of textures not in `live` (pointer keys).
    pub fn retain(&mut self, live: &HashSet<usize>) {
        self.entries.retain(|key, _| live.contains(key));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn texture_key(texture: &Arc<Texture>) -> usize {
    Arc::as_ptr(texture) as usize
}

/// A linear-filtering clamped sampler.
pub fn linear_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ui sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

/// A uniform buffer initialised with `value`.
pub fn uniform_buffer<T: bytemuck::Pod>(
    device: &wgpu::Device,
    label: &str,
    value: &T,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::bytes_of(value),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::content::Vertex;

    fn mesh(vertices: usize, indices: &[u32]) -> Mesh {
        Mesh {
            normals: vec![],
            vertices: (0..vertices)
                .map(|i| Vertex {
                    position: [i as f32, 0.0, 0.0],
                    uv: [0.0; 2],
                    color: [1.0; 4],
                })
                .collect(),
            indices: indices.to_vec(),
        }
    }

    #[test]
    fn geometry_ranges_follow_the_pushed_meshes() {
        let mut geometry = Geometry::default();
        let a = geometry.push(&mesh(4, &[0, 1, 2, 0, 2, 3])).unwrap().unwrap();
        let b = geometry.push(&mesh(3, &[0, 1, 2])).unwrap().unwrap();
        assert_eq!((a.first_index, a.index_count, a.base_vertex), (0, 6, 0));
        assert_eq!((b.first_index, b.index_count, b.base_vertex), (6, 3, 4));
        assert_eq!(geometry.vertices.len(), 7);
        assert_eq!(geometry.indices.len(), 9);
    }

    #[test]
    fn empty_and_out_of_range_meshes_are_skipped() {
        let mut geometry = Geometry::default();
        assert!(geometry.push(&mesh(3, &[])).unwrap().is_none());
        assert!(geometry.push(&mesh(3, &[0, 1, 3])).unwrap().is_none());
        assert!(geometry.vertices.is_empty() && geometry.indices.is_empty());
    }

    #[test]
    fn the_vertex_matches_the_engine_content_vertex() {
        assert_eq!(std::mem::size_of::<UiVertex>(), 36);
        assert_eq!(UI_VERTEX_LAYOUT.array_stride, 36);
    }
}
