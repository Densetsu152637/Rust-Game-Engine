//! Initialized device state: resource tables and their creation.

use crate::context::GpuContext;
use crate::error::{check_gpu, wgpu_error};
use crate::shader::{BindKey, PipelineKey, ShaderEntry};
use crate::surface::SurfaceState;
use crate::target::read_texture_rgba;
use crate::translate::{COLOR_FORMAT, DEPTH_FORMAT, backend_label, target_format};
use engine_render::{
    BackendKind, DeviceLimits, MeshHandle, MeshResource, OffscreenDesc, ReadbackImage,
    RenderCapabilities, RenderError, RenderFeature, ResourceId, Result, ShaderHandle,
    ShaderResource, TargetHandle, Texture2D, TextureHandle, VertexLayout,
};
use std::collections::{HashMap, VecDeque};
use wgpu::util::DeviceExt;

pub(crate) struct MeshEntry {
    pub vertex: wgpu::Buffer,
    pub index: Option<wgpu::Buffer>,
    pub draw_count: u32,
    pub layout: VertexLayout,
}

#[derive(Clone)]
pub(crate) struct TextureEntry {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub linear_view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct TargetEntry {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

/// Uniform arena shared by every draw of a frame.
pub(crate) struct Arena {
    pub buffer: wgpu::Buffer,
    pub capacity: u64,
}

pub(crate) struct Core {
    pub gpu: GpuContext,
    pub caps: RenderCapabilities,
    pub frames_in_flight: u32,
    pub submissions: VecDeque<wgpu::SubmissionIndex>,
    pub shaders: HashMap<u64, ShaderEntry>,
    pub meshes: HashMap<u64, MeshEntry>,
    pub textures: HashMap<u64, TextureEntry>,
    pub targets: HashMap<u64, TargetEntry>,
    pub pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    pub bind_groups: HashMap<BindKey, wgpu::BindGroup>,
    pub arena: Option<Arena>,
    pub depth_cache: HashMap<(u32, u32), wgpu::TextureView>,
    pub sampler: wgpu::Sampler,
    pub white: TextureEntry,
    pub surface: Option<SurfaceState>,
    pub shadow: crate::shadow::ShadowResources,
}

impl Core {
    pub fn new(
        gpu: GpuContext,
        surface: Option<SurfaceState>,
        external_surface: bool,
        frames_in_flight: u32,
    ) -> Result<Self> {
        let info = gpu.adapter.get_info();
        let limits = gpu.device.limits();
        let frames_in_flight = frames_in_flight.clamp(1, 8);
        let mut caps = RenderCapabilities::full(BackendKind::Wgpu, backend_label(info.backend));
        caps.device_name = Some(info.name.clone());
        caps.max_frames_in_flight = frames_in_flight;
        caps.max_texture_dimension = limits.max_texture_dimension_2d;
        caps.limits = DeviceLimits {
            max_bind_groups: limits.max_bind_groups,
            max_bindings_per_bind_group: limits.max_bindings_per_bind_group,
            max_vertex_buffer_array_stride: limits.max_vertex_buffer_array_stride,
            max_vertex_attributes: limits.max_vertex_attributes,
            max_uniform_buffer_binding_size: u32::try_from(limits.max_uniform_buffer_binding_size)
                .unwrap_or(u32::MAX),
        };
        caps.surface_presentation = surface.is_some() || external_surface;
        if !caps.surface_presentation {
            caps.unsupported_reasons.insert(
                RenderFeature::SurfacePresentation,
                "the backend was created without a window (offscreen only)".into(),
            );
        }
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest clamp sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let white = gpu.validated(|| Ok(upload_texture(&gpu, &Texture2D::white())))?;
        let shadow = crate::shadow::ShadowResources::new(&gpu.device);
        check_gpu(&gpu)?;
        Ok(Self {
            gpu,
            caps,
            frames_in_flight,
            submissions: VecDeque::new(),
            shaders: HashMap::new(),
            meshes: HashMap::new(),
            textures: HashMap::new(),
            targets: HashMap::new(),
            pipelines: HashMap::new(),
            bind_groups: HashMap::new(),
            arena: None,
            depth_cache: HashMap::new(),
            sampler,
            white,
            surface,
            shadow,
        })
    }

    pub fn create_shader(&mut self, handle: ShaderHandle, resource: &ShaderResource) -> Result<()> {
        let entry = self
            .gpu
            .validated(|| ShaderEntry::new(&self.gpu, resource))?;
        check_gpu(&self.gpu)?;
        self.shaders.insert(handle.id(), entry);
        Ok(())
    }

    /// Reject meshes whose buffers exceed the device limit instead of letting wgpu
    /// raise a sticky validation error.
    fn check_mesh_size(&self, resource: &MeshResource) -> Result<()> {
        let limit = self.gpu.device.limits().max_buffer_size;
        let vertex = resource.vertices.len() as u64;
        let index = resource.indices.len() as u64 * 4;
        if vertex > limit || index > limit {
            return Err(RenderError::InvalidResource(format!(
                "mesh buffers of {vertex} and {index} bytes exceed the device buffer limit {limit}"
            )));
        }
        Ok(())
    }

    pub fn create_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        self.check_mesh_size(resource)?;
        let entry = self
            .gpu
            .validated(|| Ok(upload_mesh(&self.gpu, resource)))?;
        check_gpu(&self.gpu)?;
        self.meshes.insert(handle.id(), entry);
        Ok(())
    }

    pub fn update_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        if !self.meshes.contains_key(&handle.id()) {
            return Err(RenderError::StaleHandle(handle.into()));
        }
        // New buffers: frames already submitted keep the old ones alive.
        self.create_mesh(handle, resource)
    }

    pub fn create_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        let entry = self
            .gpu
            .validated(|| Ok(upload_texture(&self.gpu, texture)))?;
        check_gpu(&self.gpu)?;
        self.textures.insert(handle.id(), entry);
        Ok(())
    }

    pub fn update_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        let Some(existing) = self.textures.get(&handle.id()) else {
            return Err(RenderError::StaleHandle(handle.into()));
        };
        if existing.width == texture.width && existing.height == texture.height {
            // Queue order: frames submitted earlier still see the old texels.
            self.gpu.validated(|| {
                write_texture(&self.gpu, &existing.texture, texture);
                Ok(())
            })?;
            return check_gpu(&self.gpu);
        }
        self.drop_texture_bind_groups(handle.id());
        self.create_texture(handle, texture)
    }

    pub fn create_target(&mut self, handle: TargetHandle, desc: &OffscreenDesc) -> Result<()> {
        let format = target_format(desc.format);
        let size = wgpu::Extent3d {
            width: desc.width,
            height: desc.height,
            depth_or_array_layers: 1,
        };
        let (texture, view, depth) = self.gpu.validated(|| {
            let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("offscreen target"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let depth = create_depth(&self.gpu.device, desc.width, desc.height);
            Ok((texture, view, depth))
        })?;
        check_gpu(&self.gpu)?;
        self.targets.insert(
            handle.id(),
            TargetEntry {
                texture,
                view,
                depth,
                width: desc.width,
                height: desc.height,
            },
        );
        Ok(())
    }

    pub fn destroy_resource(&mut self, id: ResourceId) -> Result<()> {
        match id {
            ResourceId::Shader(handle) => {
                self.shaders.remove(&handle.id());
                self.pipelines.retain(|key, _| key.shader != handle.id());
                self.bind_groups.retain(|key, _| key.shader != handle.id());
            }
            ResourceId::Mesh(handle) => {
                self.meshes.remove(&handle.id());
            }
            ResourceId::Texture(handle) => {
                self.textures.remove(&handle.id());
                self.drop_texture_bind_groups(handle.id());
            }
            ResourceId::Target(handle) => {
                self.targets.remove(&handle.id());
            }
        }
        Ok(())
    }

    fn drop_texture_bind_groups(&mut self, texture: u64) {
        self.bind_groups.retain(|key, _| {
            key.texture != Some(texture) && !key.data_textures.contains(&Some(texture))
        });
    }

    pub fn read_target(&mut self, handle: TargetHandle) -> Result<ReadbackImage> {
        let target = self
            .targets
            .get(&handle.id())
            .ok_or(RenderError::StaleHandle(handle.into()))?;
        let rgba8 = self.gpu.validated(|| {
            read_texture_rgba(&self.gpu, &target.texture, target.width, target.height)
                .map_err(|error| wgpu_error(&self.gpu, &error))
        })?;
        Ok(ReadbackImage {
            width: target.width,
            height: target.height,
            rgba8,
        })
    }

    pub fn wait_idle(&mut self) -> Result<()> {
        self.submissions.clear();
        self.gpu
            .wait_idle()
            .map_err(|error| wgpu_error(&self.gpu, &error))
    }
}

pub(crate) fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn upload_mesh(gpu: &GpuContext, resource: &MeshResource) -> MeshEntry {
    let vertex = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh vertices"),
            contents: &resource.vertices,
            usage: wgpu::BufferUsages::VERTEX,
        });
    let index = resource.is_indexed().then(|| {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh indices"),
                contents: bytemuck::cast_slice(&resource.indices),
                usage: wgpu::BufferUsages::INDEX,
            })
    });
    MeshEntry {
        vertex,
        index,
        draw_count: resource.draw_count(),
        layout: resource.layout.clone(),
    }
}

fn upload_texture(gpu: &GpuContext, texture: &Texture2D) -> TextureEntry {
    let size = wgpu::Extent3d {
        width: texture.width,
        height: texture.height,
        depth_or_array_layers: 1,
    };
    let created = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("RGBA texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    write_texture(gpu, &created, texture);
    let view = created.create_view(&Default::default());
    let linear_view = created.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    TextureEntry {
        linear_view,
        texture: created,
        view,
        width: texture.width,
        height: texture.height,
    }
}

fn write_texture(gpu: &GpuContext, target: &wgpu::Texture, texture: &Texture2D) {
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &texture.rgba8,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(texture.width * 4),
            rows_per_image: Some(texture.height),
        },
        wgpu::Extent3d {
            width: texture.width,
            height: texture.height,
            depth_or_array_layers: 1,
        },
    );
}
