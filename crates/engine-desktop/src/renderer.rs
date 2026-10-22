use crate::{CameraView, GpuContext, NativeRenderPasses, RenderTargetInfo};
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use engine_core::bevy_ecs::world::World;
use engine_project::{
    content::{Mesh, Texture},
    runtime::{BlendMode, DrawSpace, RenderFrame},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use wgpu::util::DeviceExt;

#[derive(Clone, Copy, Debug)]
pub struct RenderLimits {
    pub max_draws: usize,
    pub max_mesh_bytes: usize,
    pub max_texture_bytes: usize,
    pub max_frames_in_flight: usize,
}
impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            max_draws: 65_536,
            max_mesh_bytes: 256 * 1024 * 1024,
            max_texture_bytes: 512 * 1024 * 1024,
            max_frames_in_flight: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub draws: usize,
    /// Consecutive compatible pipeline/texture/camera groups; painter order is
    /// never changed merely to reduce binding changes.
    pub batches: usize,
    pub mesh_uploads: usize,
    pub texture_uploads: usize,
    pub cached_meshes: usize,
    pub cached_textures: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}
struct CachedMesh {
    source: Arc<Mesh>,
    vertices: wgpu::Buffer,
    indices: Option<wgpu::Buffer>,
    count: u32,
}
struct CachedTexture {
    _source: Option<Arc<Texture>>,
    bind: wgpu::BindGroup,
}

/// A renderer usable by a game window, editor viewport or offscreen test. CPU
/// assets are immutable Arc snapshots: retaining the same Arc means unchanged;
/// publish a new Arc for edits. GPU resources retain the Arc to prevent address
/// reuse and are evicted after their last frame reference.
pub struct GpuRenderer {
    pub gpu: GpuContext,
    info: RenderTargetInfo,
    limits: RenderLimits,
    camera_buffers: [wgpu::Buffer; 2],
    camera_binds: [wgpu::BindGroup; 2],
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipelines: Vec<wgpu::RenderPipeline>,
    depth: wgpu::TextureView,
    meshes: HashMap<usize, CachedMesh>,
    textures: HashMap<usize, CachedTexture>,
    white: CachedTexture,
    passes: NativeRenderPasses,
    submissions: VecDeque<wgpu::SubmissionIndex>,
}

impl GpuRenderer {
    pub fn new(gpu: GpuContext, info: RenderTargetInfo, limits: RenderLimits) -> Result<Self> {
        ensure!(
            info.width > 0 && info.height > 0,
            "render target must be nonzero"
        );
        ensure!(
            (1..=8).contains(&limits.max_frames_in_flight),
            "frame limit must be 1..=8"
        );
        ensure!(limits.max_draws > 0, "draw limit must be positive");
        let device = &gpu.device;
        ensure!(
            info.width <= device.limits().max_texture_dimension_2d
                && info.height <= device.limits().max_texture_dimension_2d,
            "render target exceeds device limit"
        );
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let camera_buffers = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("camera"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let camera_binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("camera"),
                layout: &camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffers[i].as_entire_binding(),
                }],
            })
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest atlas sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("textured mesh layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured mesh"),
            source: wgpu::ShaderSource::Wgsl(include_str!("textured.wgsl").into()),
        });
        let pipelines = (0..8)
            .map(|key| create_pipeline(device, &layout, &shader, info.format, key))
            .collect();
        let depth = create_depth(device, info);
        let white = upload_texture(&gpu, &texture_layout, &sampler, None, 1, 1, &[255; 4]);
        gpu.check_errors()?;
        Ok(Self {
            gpu,
            info,
            limits,
            camera_buffers,
            camera_binds,
            texture_layout,
            sampler,
            pipelines,
            depth,
            meshes: HashMap::new(),
            textures: HashMap::new(),
            white,
            passes: NativeRenderPasses::default(),
            submissions: VecDeque::new(),
        })
    }

    pub fn target_info(&self) -> RenderTargetInfo {
        self.info
    }

    pub fn set_passes(&mut self, mut passes: NativeRenderPasses) -> Result<()> {
        for (name, pass) in &mut passes.passes {
            pass.prepare(&self.gpu, self.info)
                .with_context(|| format!("preparing render pass {name}"))?;
        }
        self.gpu.check_errors()?;
        self.passes = passes;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        ensure!(
            width > 0 && height > 0,
            "zero size is a suspended surface, not a render target"
        );
        ensure!(
            width <= self.gpu.device.limits().max_texture_dimension_2d
                && height <= self.gpu.device.limits().max_texture_dimension_2d,
            "render target exceeds device limit"
        );
        self.gpu.wait_idle()?;
        self.submissions.clear();
        self.info.width = width;
        self.info.height = height;
        self.depth = create_depth(&self.gpu.device, self.info);
        for (name, pass) in &mut self.passes.passes {
            pass.prepare(&self.gpu, self.info)
                .with_context(|| format!("resizing render pass {name}"))?;
        }
        self.gpu.check_errors()
    }

    /// Encode and submit one frame. At most `max_frames_in_flight` submissions
    /// are retained; backpressure waits on the oldest submission. Invalid CPU
    /// data fails before changing cached resources or submitting partial draws.
    pub fn render(
        &mut self,
        frame: &RenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
    ) -> Result<RenderStats> {
        self.gpu.check_errors()?;
        let camera = CameraView::new(&frame.camera, self.info.width, self.info.height)?;
        self.validate(frame, &camera)?;
        if self.submissions.len() >= self.limits.max_frames_in_flight
            && let Some(index) = self.submissions.pop_front()
        {
            self.gpu
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(index),
                    timeout: Some(std::time::Duration::from_secs(30)),
                })
                .context("GPU frame backpressure")?;
        }
        let mut stats = RenderStats::default();
        let mut live_meshes = HashSet::new();
        let mut live_textures = HashSet::new();
        for draw in &frame.draws {
            let key = Arc::as_ptr(&draw.mesh) as usize;
            live_meshes.insert(key);
            if !self.meshes.contains_key(&key) {
                self.meshes
                    .insert(key, upload_mesh(&self.gpu.device, draw.mesh.clone()));
                stats.mesh_uploads += 1;
            }
            if let Some(texture) = &draw.texture {
                let key = Arc::as_ptr(texture) as usize;
                live_textures.insert(key);
                if !self.textures.contains_key(&key) {
                    self.textures.insert(
                        key,
                        upload_texture(
                            &self.gpu,
                            &self.texture_layout,
                            &self.sampler,
                            Some(texture.clone()),
                            texture.width,
                            texture.height,
                            &texture.pixels,
                        ),
                    );
                    stats.texture_uploads += 1;
                }
            }
        }
        self.meshes.retain(|key, _| live_meshes.contains(key));
        self.textures.retain(|key, _| live_textures.contains(key));
        self.gpu.queue.write_buffer(
            &self.camera_buffers[0],
            0,
            bytemuck::cast_slice(&camera.world),
        );
        self.gpu
            .queue
            .write_buffer(&self.camera_buffers[1], 0, bytemuck::cast_slice(&camera.ui));
        let mut draws: Vec<_> = frame.draws.iter().collect();
        // Stable IDs make equal-layer draws independent of ECS iteration order.
        draws.sort_by(|a, b| a.layer.cmp(&b.layer).then_with(|| a.id.cmp(&b.id)));
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("engine frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("engine textured draws"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            let v = camera.viewport;
            if v.width > 0 && v.height > 0 {
                pass.set_viewport(
                    v.x as f32,
                    v.y as f32,
                    v.width as f32,
                    v.height as f32,
                    0.0,
                    1.0,
                );
            }
            let mut previous = None;
            for draw in draws {
                let Some(scissor) = camera.scissor(draw.clip)? else {
                    continue;
                };
                let mesh = &self.meshes[&(Arc::as_ptr(&draw.mesh) as usize)];
                if mesh.count == 0 {
                    continue;
                }
                let texture_key = draw.texture.as_ref().map(|t| Arc::as_ptr(t) as usize);
                let texture = texture_key.map_or(&self.white, |key| &self.textures[&key]);
                let camera_index = match draw.space {
                    DrawSpace::World => 0,
                    DrawSpace::Ui => 1,
                };
                let pipeline_key = (matches!(draw.state.blend, BlendMode::StraightAlpha) as usize)
                    | ((draw.state.depth_test as usize) << 1)
                    | ((draw.state.depth_write as usize) << 2);
                let key = (pipeline_key, texture_key, camera_index, scissor);
                if previous != Some(key) {
                    pass.set_pipeline(&self.pipelines[pipeline_key]);
                    pass.set_bind_group(0, &self.camera_binds[camera_index], &[]);
                    pass.set_bind_group(1, &texture.bind, &[]);
                    pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
                    stats.batches += 1;
                    previous = Some(key);
                }
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                if let Some(indices) = &mesh.indices {
                    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.count, 0, 0..1);
                } else {
                    pass.draw(0..mesh.count, 0..1);
                }
                stats.draws += 1;
            }
        }
        for (name, pass) in &mut self.passes.passes {
            pass.encode(&self.gpu, &mut encoder, target, self.info, world)
                .with_context(|| format!("encoding render pass {name}"))?;
        }
        self.gpu.check_errors()?;
        self.submissions
            .push_back(self.gpu.queue.submit([encoder.finish()]));
        self.gpu
            .device
            .poll(wgpu::PollType::Poll)
            .context("polling GPU frame")?;
        self.gpu.check_errors()?;
        stats.cached_meshes = self.meshes.len();
        stats.cached_textures = self.textures.len();
        Ok(stats)
    }

    fn validate(&self, frame: &RenderFrame, camera: &CameraView) -> Result<()> {
        ensure!(
            frame.draws.len() <= self.limits.max_draws,
            "frame draw limit exceeded"
        );
        let mut ids = HashSet::new();
        let mut meshes = HashSet::new();
        let mut textures = HashSet::new();
        let mut mesh_bytes = 0usize;
        let mut texture_bytes = 0usize;
        for draw in &frame.draws {
            ensure!(
                !draw.id.is_empty() && ids.insert(&draw.id),
                "empty or duplicate draw ID: {}",
                draw.id
            );
            camera.scissor(draw.clip)?;
            let mesh = &draw.mesh;
            if meshes.insert(Arc::as_ptr(mesh) as usize) {
                let bytes = mesh
                    .vertices
                    .len()
                    .checked_mul(std::mem::size_of::<GpuVertex>())
                    .and_then(|n| {
                        mesh.indices
                            .len()
                            .checked_mul(4)
                            .and_then(|i| n.checked_add(i))
                    })
                    .context("mesh byte size overflow")?;
                mesh_bytes = mesh_bytes
                    .checked_add(bytes)
                    .context("mesh byte total overflow")?;
                ensure!(
                    mesh_bytes <= self.limits.max_mesh_bytes,
                    "frame mesh byte limit exceeded"
                );
                ensure!(
                    bytes as u64 <= self.gpu.device.limits().max_buffer_size,
                    "mesh exceeds GPU buffer limit"
                );
                ensure!(
                    mesh.vertices.len() <= u32::MAX as usize
                        && mesh.indices.len() <= u32::MAX as usize,
                    "mesh draw count overflow"
                );
                if !self
                    .meshes
                    .get(&(Arc::as_ptr(mesh) as usize))
                    .is_some_and(|old| Arc::ptr_eq(&old.source, mesh))
                {
                    ensure!(
                        mesh.vertices.iter().all(|v| v
                            .position
                            .iter()
                            .chain(v.uv.iter())
                            .chain(v.color.iter())
                            .all(|x| x.is_finite())),
                        "mesh contains nonfinite vertex data"
                    );
                    ensure!(
                        mesh.indices
                            .iter()
                            .all(|i| (*i as usize) < mesh.vertices.len()),
                        "mesh index exceeds vertex count"
                    );
                    let count = if mesh.indices.is_empty() {
                        mesh.vertices.len()
                    } else {
                        mesh.indices.len()
                    };
                    ensure!(count % 3 == 0, "mesh must contain complete triangles");
                }
            }
            if let Some(texture) = &draw.texture
                && textures.insert(Arc::as_ptr(texture) as usize)
            {
                let expected = (texture.width as usize)
                    .checked_mul(texture.height as usize)
                    .and_then(|n| n.checked_mul(4))
                    .context("texture size overflow")?;
                ensure!(
                    texture.width > 0 && texture.height > 0 && expected == texture.pixels.len(),
                    "invalid RGBA texture dimensions/data"
                );
                ensure!(
                    texture.width <= self.gpu.device.limits().max_texture_dimension_2d
                        && texture.height <= self.gpu.device.limits().max_texture_dimension_2d,
                    "texture exceeds device limit"
                );
                texture_bytes = texture_bytes
                    .checked_add(expected)
                    .context("texture byte total overflow")?;
                ensure!(
                    texture_bytes <= self.limits.max_texture_bytes,
                    "frame texture byte limit exceeded"
                );
            }
        }
        Ok(())
    }
}

fn create_depth(device: &wgpu::Device, info: RenderTargetInfo) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: info.width,
                height: info.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    key: usize,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("textured mesh pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GpuVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x2,2=>Float32x4],
            })],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(key & 4 != 0),
            depth_compare: Some(if key & 2 != 0 {
                wgpu::CompareFunction::LessEqual
            } else {
                wgpu::CompareFunction::Always
            }),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: if key & 1 != 0 {
                    Some(wgpu::BlendState::ALPHA_BLENDING)
                } else {
                    None
                },
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn upload_mesh(device: &wgpu::Device, source: Arc<Mesh>) -> CachedMesh {
    let vertices: Vec<_> = source
        .vertices
        .iter()
        .map(|v| GpuVertex {
            position: v.position,
            uv: v.uv,
            color: v.color,
        })
        .collect();
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let indices = (!source.indices.is_empty()).then(|| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh indices"),
            contents: bytemuck::cast_slice(&source.indices),
            usage: wgpu::BufferUsages::INDEX,
        })
    });
    let count = if indices.is_some() {
        source.indices.len()
    } else {
        source.vertices.len()
    } as u32;
    CachedMesh {
        source,
        vertices: buffer,
        indices,
        count,
    }
}

fn upload_texture(
    gpu: &GpuContext,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    source: Option<Arc<Texture>>,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> CachedTexture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("RGBA texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );
    let view = texture.create_view(&Default::default());
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("texture"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    CachedTexture {
        _source: source,
        bind,
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_vertex_is_a_tightly_packed_36_byte_record() {
        assert_eq!(size_of::<GpuVertex>(), 36);
        assert_eq!(align_of::<GpuVertex>(), 4);
    }

    #[test]
    fn gpu_vertex_fields_sit_at_the_offsets_the_pipeline_assumes() {
        // vertex_attr_array![0=>Float32x3, 1=>Float32x2, 2=>Float32x4] packs 12, 8, 16 bytes.
        assert_eq!(offset_of!(GpuVertex, position), 0);
        assert_eq!(offset_of!(GpuVertex, uv), 12);
        assert_eq!(offset_of!(GpuVertex, color), 20);
        assert_eq!(20 + size_of::<[f32; 4]>(), size_of::<GpuVertex>());
    }

    #[test]
    fn gpu_vertex_bytes_are_little_endian_floats_in_declaration_order() {
        let vertex = GpuVertex {
            position: [1.0, 2.0, 3.0],
            uv: [0.5, 0.25],
            color: [0.0, 1.0, -1.0, 8.0],
        };
        let bytes = bytemuck::bytes_of(&vertex);
        let floats: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(floats, [1.0, 2.0, 3.0, 0.5, 0.25, 0.0, 1.0, -1.0, 8.0]);
    }

    #[test]
    fn vertex_slices_cast_without_padding_between_elements() {
        let vertices = [GpuVertex::zeroed(); 5];
        assert_eq!(bytemuck::cast_slice::<_, u8>(&vertices).len(), 5 * 36);
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
        };
        let floats: &[f32] = bytemuck::cast_slice(bytemuck::bytes_of(&gpu));
        assert_eq!(floats, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn shader_vertex_inputs_match_the_gpu_vertex_attributes() {
        let module = naga::front::wgsl::parse_str(include_str!("textured.wgsl")).unwrap();
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
        assert_eq!(stride as usize, size_of::<GpuVertex>());
    }

    #[test]
    fn camera_buffer_size_matches_a_column_major_4x4() {
        // The pipeline layout declares min_binding_size 64 and each camera buffer is 64 bytes.
        assert_eq!(size_of::<[f32; 16]>(), 64);
        let camera =
            crate::CameraView::new(&engine_project::Camera2D::default(), 320, 180).unwrap();
        assert_eq!(bytemuck::cast_slice::<f32, u8>(&camera.world).len(), 64);
        assert_eq!(bytemuck::cast_slice::<f32, u8>(&camera.ui).len(), 64);
    }

    #[test]
    fn render_limits_default_to_documented_bounds() {
        let limits = RenderLimits::default();
        assert_eq!(limits.max_draws, 65_536);
        assert_eq!(limits.max_mesh_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.max_texture_bytes, 512 * 1024 * 1024);
        assert_eq!(limits.max_frames_in_flight, 2);
        assert!((1..=8).contains(&limits.max_frames_in_flight));
    }

    #[test]
    fn render_stats_default_to_zero_and_compare_by_value() {
        let zero = RenderStats::default();
        assert_eq!(
            zero,
            RenderStats {
                draws: 0,
                batches: 0,
                mesh_uploads: 0,
                texture_uploads: 0,
                cached_meshes: 0,
                cached_textures: 0
            }
        );
        assert_ne!(zero, RenderStats { batches: 1, ..zero });
    }
}
