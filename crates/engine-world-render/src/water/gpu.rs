//! GPU state of the water features: pipelines, per-body uniforms, meshes and textures, and the
//! per-view copies of scene colour and depth.
use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use engine_project::world::water::{WaterRenderItem, WaterRenderSet};
use engine_render_pipeline::{
    DEPTH_FORMAT, ENGINE_VERTEX_LAYOUT, EngineVertex, FeatureContext, FeatureSetup, GpuContext,
    GpuMesh, HDR_FORMAT, MAX_SHADOWS,
};
use engine_water::{NoGround, Underwater, WaterShape, WaterSurface, Waves};
use glam::{Mat4, Vec3};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const WATER_SHADER: &str = include_str!("water.wgsl");
const UNDERWATER_SHADER: &str = include_str!("underwater.wgsl");
/// Gerstner waves the shader evaluates (the model's maximum).
pub const GPU_WAVES: usize = engine_water::MAX_GERSTNER_WAVES;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterUniform {
    world_from_local: [[f32; 4]; 4],
    params: [f32; 4],
    params2: [f32; 4],
    shallow: [f32; 4],
    deep: [f32; 4],
    absorption: [f32; 4],
    foam: [f32; 4],
    fog: [f32; 4],
    shore: [f32; 4],
    waves_a: [[f32; 4]; GPU_WAVES],
    waves_b: [[f32; 4]; GPU_WAVES],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FogUniform {
    color_density: [f32; 4],
    absorption: [f32; 4],
}

struct Prepared {
    layout: wgpu::BindGroupLayout,
    fog_layout: wgpu::BindGroupLayout,
    water: wgpu::RenderPipeline,
    shadow: wgpu::RenderPipeline,
    shadow_indices: wgpu::BindGroup,
    shadow_index_stride: u32,
    _shadow_index_buffer: wgpu::Buffer,
    underwater: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    /// 1 x 1 placeholders for bodies without an ocean frame or shore grid.
    blank_rgba: wgpu::Texture,
    blank_rg: wgpu::Texture,
    blank_color: wgpu::Texture,
    blank_depth: wgpu::Texture,
    fog_uniform: wgpu::Buffer,
}

/// Per-view copies of the HDR colour and depth (render size).
struct SceneCopies {
    size: [u32; 2],
    color: wgpu::Texture,
    depth: wgpu::Texture,
}

struct GpuBody {
    revision: u64,
    uniform: wgpu::Buffer,
    mesh: Arc<GpuMesh>,
    /// Grid side, displacement and normal textures, and the time bits of the uploaded frame.
    ocean: Option<(usize, wgpu::Texture, wgpu::Texture, usize)>,
    /// Shore texture and the FNV-1a hash of the uploaded grid.
    shore: Option<(wgpu::Texture, usize)>,
}

/// Counts of one water pass.
#[derive(Default)]
pub(crate) struct FrameDraw {
    pub bodies: usize,
    pub uploads: u64,
    pub notes: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Shared {
    prepared: Option<Prepared>,
    bodies: BTreeMap<String, GpuBody>,
    grids: HashMap<u32, Arc<GpuMesh>>,
    copies: Option<SceneCopies>,
}

fn texture(
    gpu: &GpuContext,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload(gpu: &GpuContext, texture: &wgpu::Texture, size: [u32; 2], texel: u32, data: &[u8]) {
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * texel),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
}

fn entry(
    binding: u32,
    ty: wgpu::BindingType,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty,
        count: None,
    }
}
fn float_texture(filterable: bool) -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable },
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    }
}
fn depth_texture() -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Depth,
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    }
}
fn uniform() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

/// The underwater state of a camera at world `camera`: the body whose surface is above it
/// (the highest surface when several are), from the surfaces' CPU queries.
pub fn camera_underwater(
    set: &WaterRenderSet,
    camera: Vec3,
) -> Option<(String, Underwater, Arc<WaterSurface>)> {
    let mut best: Option<(f32, String, Underwater, Arc<WaterSurface>)> = None;
    for item in &set.items {
        let m = Mat4::from_cols_array_2d(&item.world_from_local);
        let local = m.inverse().transform_point3(camera);
        if let Some(under) = item.surface.underwater(local, &NoGround)
            && best.as_ref().is_none_or(|b| under.depth > b.0)
        {
            best = Some((under.depth, item.id.clone(), under, item.surface.clone()));
        }
    }
    best.map(|(_, id, under, surface)| (id, under, surface))
}

impl Shared {
    pub(crate) fn is_prepared(&self) -> bool {
        self.prepared.is_some()
    }
    pub(crate) fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.water"),
            entries: &[
                entry(0, uniform(), both),
                entry(1, float_texture(false), both),
                entry(2, float_texture(false), both),
                entry(3, float_texture(false), both),
                entry(4, float_texture(true), fragment),
                entry(5, depth_texture(), fragment),
                entry(
                    6,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    fragment,
                ),
            ],
        });
        let fog_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.water.underwater"),
            entries: &[
                entry(0, uniform(), fragment),
                entry(1, float_texture(false), fragment),
                entry(2, depth_texture(), fragment),
            ],
        });
        let module = |label: &str, source: &str| -> Result<wgpu::ShaderModule> {
            let source = setup.composer.compose(source)?;
            Ok(device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            }))
        };
        let water_module = module("world.water", WATER_SHADER)?;
        let fog_module = module("world.water.underwater", UNDERWATER_SHADER)?;
        let water_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world.water"),
            bind_group_layouts: &[Some(&setup.layouts.view), Some(&layout)],
            immediate_size: 0,
        });
        let water = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world.water"),
            layout: Some(&water_layout),
            vertex: wgpu::VertexState {
                module: &water_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(ENGINE_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &water_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let shadow_index_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("world.water shadow index"),
                entries: &[entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    wgpu::ShaderStages::VERTEX,
                )],
            });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("world.water.shadow"),
                bind_group_layouts: &[
                    Some(&setup.layouts.view),
                    Some(&layout),
                    Some(&shadow_index_layout),
                ],
                immediate_size: 0,
            });
        let shadow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world.water.shadow"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &water_module,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[Some(ENGINE_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &water_module,
                entry_point: Some("fs_shadow"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        });
        let shadow_index_stride = device.limits().min_uniform_buffer_offset_alignment.max(16);
        let shadow_index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("world.water shadow indices"),
            size: u64::from(shadow_index_stride) * MAX_SHADOWS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for index in 0..MAX_SHADOWS as u32 {
            setup.gpu.queue.write_buffer(
                &shadow_index_buffer,
                u64::from(index * shadow_index_stride),
                bytemuck::cast_slice(&[index, 0u32, 0, 0]),
            );
        }
        let shadow_indices = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world.water shadow indices"),
            layout: &shadow_index_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &shadow_index_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(16),
                }),
            }],
        });
        let fog_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world.water.underwater"),
            bind_group_layouts: &[Some(&setup.layouts.view), Some(&fog_layout)],
            immediate_size: 0,
        });
        let underwater = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world.water.underwater"),
            layout: Some(&fog_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &fog_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &fog_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world.water"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank_rgba = texture(
            setup.gpu,
            "world.water blank",
            [1, 1],
            wgpu::TextureFormat::Rgba32Float,
        );
        upload(
            setup.gpu,
            &blank_rgba,
            [1, 1],
            16,
            bytemuck::cast_slice(&[0.0f32, 0.0, 0.0, 1.0]),
        );
        let blank_rg = texture(
            setup.gpu,
            "world.water blank shore",
            [1, 1],
            wgpu::TextureFormat::Rg32Float,
        );
        upload(
            setup.gpu,
            &blank_rg,
            [1, 1],
            8,
            bytemuck::cast_slice(&[1.0f32, 0.0]),
        );
        let blank_depth = texture(setup.gpu, "world.water blank depth", [1, 1], DEPTH_FORMAT);
        let blank_color = texture(
            setup.gpu,
            "world.water blank color",
            [1, 1],
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let fog_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("world.water.underwater"),
            size: std::mem::size_of::<FogUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.prepared = Some(Prepared {
            layout,
            fog_layout,
            water,
            shadow,
            shadow_indices,
            shadow_index_stride,
            _shadow_index_buffer: shadow_index_buffer,
            underwater,
            sampler,
            blank_rgba,
            blank_rg,
            blank_color,
            blank_depth,
            fog_uniform,
        });
        self.bodies.clear();
        self.grids.clear();
        self.copies = None;
        Ok(())
    }

    fn grid(&mut self, gpu: &GpuContext, n: u32) -> Result<Arc<GpuMesh>> {
        if let Some(mesh) = self.grids.get(&n) {
            return Ok(mesh.clone());
        }
        let side = n + 1;
        let mut vertices = Vec::with_capacity((side * side) as usize);
        for j in 0..side {
            for i in 0..side {
                let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                vertices.push(EngineVertex {
                    position: [u, 0.0, v],
                    uv: [u, v],
                    color: [1.0; 4],
                    normal: [0.0, 1.0, 0.0],
                });
            }
        }
        let mut indices = Vec::with_capacity((n * n * 6) as usize);
        for j in 0..n {
            for i in 0..n {
                let a = j * side + i;
                let (b, c) = (a + side, a + 1);
                indices.extend_from_slice(&[a, b, c, b, b + 1, c]);
            }
        }
        let mesh =
            GpuMesh::from_engine_vertices(&gpu.device, "world.water grid", &vertices, &indices)?;
        self.grids.insert(n, mesh.clone());
        Ok(mesh)
    }

    /// Creates or refreshes the GPU data of `item`; returns the number of texture uploads.
    fn sync_body(&mut self, gpu: &GpuContext, item: &WaterRenderItem) -> Result<u64> {
        let fresh = self
            .bodies
            .get(&item.id)
            .is_none_or(|b| b.revision != item.revision);
        if fresh {
            let mesh = match &item.surface.shape {
                WaterShape::River(river) => {
                    let across = (item.grid_resolution / 8).clamp(1, 32);
                    let (positions, uvs, indices) = river.ribbon(across);
                    let vertices: Vec<EngineVertex> = positions
                        .iter()
                        .zip(&uvs)
                        .map(|(p, uv)| EngineVertex {
                            position: p.to_array(),
                            uv: *uv,
                            color: [1.0; 4],
                            normal: [0.0, 1.0, 0.0],
                        })
                        .collect();
                    GpuMesh::from_engine_vertices(
                        &gpu.device,
                        "world.water river",
                        &vertices,
                        &indices,
                    )?
                }
                _ => self.grid(gpu, item.grid_resolution)?,
            };
            let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("world.water uniform"),
                size: std::mem::size_of::<WaterUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.bodies.insert(
                item.id.clone(),
                GpuBody {
                    revision: item.revision,
                    uniform,
                    mesh,
                    ocean: None,
                    shore: None,
                },
            );
        }
        let body = self.bodies.get_mut(&item.id).expect("synced");
        let mut uploads = 0;
        if let Waves::Ocean(frame) = &item.surface.waves {
            let n = frame.resolution();
            let key = frame.time().to_bits() as usize + 1;
            if body.ocean.as_ref().is_none_or(|o| o.0 != n) {
                let size = [n as u32; 2];
                body.ocean = Some((
                    n,
                    texture(
                        gpu,
                        "world.water ocean displacement",
                        size,
                        wgpu::TextureFormat::Rgba32Float,
                    ),
                    texture(
                        gpu,
                        "world.water ocean normal",
                        size,
                        wgpu::TextureFormat::Rgba32Float,
                    ),
                    0,
                ));
            }
            let ocean = body.ocean.as_mut().expect("created");
            if ocean.3 != key {
                let (displacement, normal) = frame.gpu_textures();
                let size = [n as u32; 2];
                upload(gpu, &ocean.1, size, 16, bytemuck::cast_slice(&displacement));
                upload(gpu, &ocean.2, size, 16, bytemuck::cast_slice(&normal));
                ocean.3 = key;
                uploads += 1;
            }
        } else {
            body.ocean = None;
        }
        match &item.shore {
            Some(grid) => {
                let key = grid.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| {
                    (h ^ (u64::from(v[0].to_bits()) << 32 | u64::from(v[1].to_bits())))
                        .wrapping_mul(0x0100_0000_01b3)
                }) as usize;
                let side = item.grid_resolution + 1;
                if body.shore.as_ref().is_none_or(|s| s.1 != key) {
                    let shore = texture(
                        gpu,
                        "world.water shore",
                        [side; 2],
                        wgpu::TextureFormat::Rg32Float,
                    );
                    upload(
                        gpu,
                        &shore,
                        [side; 2],
                        8,
                        bytemuck::cast_slice(grid.as_slice()),
                    );
                    body.shore = Some((shore, key));
                    uploads += 1;
                }
            }
            None => body.shore = None,
        }
        // Uniform.
        let optics = &item.surface.optics;
        let (mode, waves, patch, ocean_n) = match (&item.surface.shape, &item.surface.waves) {
            (WaterShape::River(_), _) => (3.0, Vec::new(), 1.0, 1.0),
            (_, Waves::Flat) => (0.0, Vec::new(), 1.0, 1.0),
            (_, Waves::Gerstner(set)) => (1.0, set.gpu_params(), 1.0, 1.0),
            (_, Waves::Ocean(frame)) => (
                2.0,
                Vec::new(),
                frame.patch_length(),
                frame.resolution() as f32,
            ),
        };
        let mut u = WaterUniform {
            world_from_local: item.world_from_local,
            params: [
                item.half_extent[0],
                item.half_extent[1],
                item.grid_resolution as f32,
                mode,
            ],
            params2: [
                item.surface.time,
                waves.len().min(GPU_WAVES) as f32,
                patch,
                ocean_n,
            ],
            shallow: [
                optics.shallow_color[0],
                optics.shallow_color[1],
                optics.shallow_color[2],
                optics.refraction,
            ],
            deep: [
                optics.deep_color[0],
                optics.deep_color[1],
                optics.deep_color[2],
                optics.roughness,
            ],
            absorption: [
                optics.absorption[0],
                optics.absorption[1],
                optics.absorption[2],
                optics.foam_threshold,
            ],
            foam: [
                optics.foam_color[0],
                optics.foam_color[1],
                optics.foam_color[2],
                optics.foam_intensity,
            ],
            fog: [
                optics.fog_color[0],
                optics.fog_color[1],
                optics.fog_color[2],
                optics.fog_density,
            ],
            shore: [
                if item.surface.shoreline.enabled {
                    item.surface.shoreline.foam_depth
                } else {
                    0.0
                },
                f32::from(u8::from(body.shore.is_some())),
                0.0,
                0.0,
            ],
            waves_a: [[0.0; 4]; GPU_WAVES],
            waves_b: [[0.0; 4]; GPU_WAVES],
        };
        for (i, (a, b)) in waves.iter().take(GPU_WAVES).enumerate() {
            u.waves_a[i] = *a;
            u.waves_b[i] = *b;
        }
        gpu.queue
            .write_buffer(&body.uniform, 0, bytemuck::bytes_of(&u));
        Ok(uploads)
    }

    /// Copies the current HDR colour and depth into the per-view copies (resized on demand).
    fn copy_scene(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let size = ctx.render_size();
        if self.copies.as_ref().is_none_or(|c| c.size != size) {
            let make = |label: &str, format: wgpu::TextureFormat| {
                ctx.gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };
            self.copies = Some(SceneCopies {
                size,
                color: make("world.water scene colour", HDR_FORMAT),
                depth: make("world.water scene depth", DEPTH_FORMAT),
            });
        }
        let copies = self.copies.as_ref().expect("created");
        let hdr = ctx.hdr()?.texture.clone();
        let depth = ctx.depth()?.texture.clone();
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        for (from, to) in [(&hdr, &copies.color), (&depth, &copies.depth)] {
            ctx.encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: from,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: to,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                extent,
            );
        }
        Ok(())
    }

    /// Draws every body of the render set into the view (`world.water`).
    pub(crate) fn draw_water(&mut self, ctx: &mut FeatureContext<'_>) -> Result<FrameDraw> {
        if self.prepared.is_none() {
            bail!("world.water is not prepared");
        }
        let mut frame = FrameDraw::default();
        let Some(world) = ctx.world else {
            frame
                .notes
                .push("the host supplied no extracted world".into());
            return Ok(frame);
        };
        let Some(set) = world.get_resource::<WaterRenderSet>() else {
            frame.notes.push(
                "no WaterRenderSet in the extracted world (call engine_world_render::install)"
                    .into(),
            );
            return Ok(frame);
        };
        let live: Vec<&str> = set.items.iter().map(|i| i.id.as_str()).collect();
        self.bodies.retain(|id, _| live.contains(&id.as_str()));
        let mut drawn = Vec::new();
        for item in &set.items {
            match self.sync_body(ctx.gpu, item) {
                Ok(n) => {
                    frame.uploads += n;
                    drawn.push(item.id.clone());
                }
                Err(error) => frame.notes.push(format!("{}: {error}", item.id)),
            }
        }
        if drawn.is_empty() {
            return Ok(frame);
        }
        self.copy_scene(ctx)?;
        let prepared = self
            .prepared
            .as_ref()
            .context("world.water is not prepared")?;
        let copies = self.copies.as_ref().expect("copied");
        let color_view = copies.color.create_view(&Default::default());
        let depth_view = copies.depth.create_view(&Default::default());
        let blank = prepared.blank_rgba.create_view(&Default::default());
        let blank_rg = prepared.blank_rg.create_view(&Default::default());
        let mut groups = Vec::new();
        for id in &drawn {
            let body = &self.bodies[id];
            let (displacement, normal) = match &body.ocean {
                Some((_, d, n, _)) => (
                    d.create_view(&Default::default()),
                    n.create_view(&Default::default()),
                ),
                None => (blank.clone(), blank.clone()),
            };
            let shore = body.shore.as_ref().map_or(blank_rg.clone(), |(t, _)| {
                t.create_view(&Default::default())
            });
            let views = [&displacement, &normal, &shore, &color_view, &depth_view];
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: body.uniform.as_entire_binding(),
            }];
            for (i, view) in views.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: i as u32 + 1,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            entries.push(wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(&prepared.sampler),
            });
            let group = ctx
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("world.water"),
                    layout: &prepared.layout,
                    entries: &entries,
                });
            groups.push((group, body.mesh.clone()));
        }
        let view_group = ctx.view_bind_group;
        let mut pass = ctx.scene_pass("world.water")?;
        pass.set_pipeline(&prepared.water);
        pass.set_bind_group(0, view_group, &[]);
        for (group, mesh) in &groups {
            pass.set_bind_group(1, group, &[]);
            pass.set_vertex_buffer(0, mesh.vertex.slice(..));
            match &mesh.index {
                Some(index) => {
                    pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.count, 0, 0..1);
                }
                None => pass.draw(0..mesh.count, 0..1),
            }
        }
        drop(pass);
        frame.bodies = groups.len();
        Ok(frame)
    }

    /// Opted-in water draws opaque depth from the same displaced mesh and uniforms as colour.
    pub(crate) fn draw_shadow(
        &mut self,
        ctx: &mut FeatureContext<'_>,
    ) -> Result<(FrameDraw, engine_render_pipeline::DrawStats)> {
        if self.prepared.is_none() {
            bail!("world.water is not prepared");
        }
        let mut frame = FrameDraw::default();
        let views = ctx.shadow_views()?.to_vec();
        if views.is_empty() {
            return Ok((frame, Default::default()));
        }
        let Some(set) = ctx.world.and_then(|w| w.get_resource::<WaterRenderSet>()) else {
            return Ok((frame, Default::default()));
        };
        let live: Vec<&str> = set.items.iter().map(|i| i.id.as_str()).collect();
        self.bodies.retain(|id, _| live.contains(&id.as_str()));
        let mut drawn = Vec::new();
        for item in set.items.iter().filter(|i| i.cast_shadows) {
            match self.sync_body(ctx.gpu, item) {
                Ok(n) => {
                    frame.uploads += n;
                    drawn.push(item.id.clone());
                }
                Err(error) => frame.notes.push(format!("{}: {error}", item.id)),
            }
        }
        if drawn.is_empty() {
            return Ok((frame, Default::default()));
        }
        let prepared = self.prepared.as_ref().expect("prepared");
        let blank = prepared.blank_rgba.create_view(&Default::default());
        let blank_rg = prepared.blank_rg.create_view(&Default::default());
        let blank_color = prepared.blank_color.create_view(&Default::default());
        let blank_depth = prepared.blank_depth.create_view(&Default::default());
        let mut groups = Vec::new();
        for id in &drawn {
            let body = &self.bodies[id];
            let displacement = body
                .ocean
                .as_ref()
                .map_or(blank.clone(), |o| o.1.create_view(&Default::default()));
            let normal = body
                .ocean
                .as_ref()
                .map_or(blank.clone(), |o| o.2.create_view(&Default::default()));
            let shore = body
                .shore
                .as_ref()
                .map_or(blank_rg.clone(), |s| s.0.create_view(&Default::default()));
            let views = [&displacement, &normal, &shore, &blank_color, &blank_depth];
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: body.uniform.as_entire_binding(),
            }];
            for (index, view) in views.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: index as u32 + 1,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            entries.push(wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(&prepared.sampler),
            });
            let bind = ctx
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("world.water.shadow"),
                    layout: &prepared.layout,
                    entries: &entries,
                });
            groups.push((bind, body.mesh.clone()));
        }
        let mut stats = engine_render_pipeline::DrawStats::default();
        let view_group = ctx.view_bind_group;
        for (tile, view) in views.iter().enumerate() {
            let mut pass = ctx.shadow_pass("world.water.shadow", tile)?;
            pass.set_pipeline(&prepared.shadow);
            pass.set_bind_group(0, view_group, &[]);
            pass.set_bind_group(
                2,
                &prepared.shadow_indices,
                &[view.index * prepared.shadow_index_stride],
            );
            for (bind, mesh) in &groups {
                pass.set_bind_group(1, bind, &[]);
                pass.set_vertex_buffer(0, mesh.vertex.slice(..));
                match &mesh.index {
                    Some(index) => {
                        pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..mesh.count, 0, 0..1);
                    }
                    None => pass.draw(0..mesh.count, 0..1),
                }
                stats.draw_calls += 1;
                stats.instances += 1;
            }
        }
        frame.bodies = groups.len();
        Ok((frame, stats))
    }

    /// Fogs the view when its camera is under a water surface (`world.water.underwater`);
    /// returns the body id when it did.
    pub(crate) fn draw_underwater(
        &mut self,
        ctx: &mut FeatureContext<'_>,
    ) -> Result<Option<String>> {
        if self.prepared.is_none() {
            bail!("world.water.underwater is not prepared");
        }
        let Some(set) = ctx.world.and_then(|w| w.get_resource::<WaterRenderSet>()) else {
            return Ok(None);
        };
        let Some((id, _, surface)) = camera_underwater(set, ctx.view.camera_position()) else {
            return Ok(None);
        };
        self.copy_scene(ctx)?;
        let prepared = self.prepared.as_ref().expect("prepared");
        let optics = &surface.optics;
        let fog = FogUniform {
            color_density: [
                optics.fog_color[0],
                optics.fog_color[1],
                optics.fog_color[2],
                optics.fog_density,
            ],
            absorption: [
                optics.absorption[0],
                optics.absorption[1],
                optics.absorption[2],
                0.0,
            ],
        };
        ctx.gpu
            .queue
            .write_buffer(&prepared.fog_uniform, 0, bytemuck::bytes_of(&fog));
        let copies = self.copies.as_ref().expect("copied");
        let color_view = copies.color.create_view(&Default::default());
        let depth_view = copies.depth.create_view(&Default::default());
        let group = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("world.water.underwater"),
                layout: &prepared.fog_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: prepared.fog_uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&depth_view),
                    },
                ],
            });
        let hdr = ctx.hdr()?.view.clone();
        let view_group = ctx.view_bind_group;
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("world.water.underwater"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &hdr,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&prepared.underwater);
        pass.set_bind_group(0, view_group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        Ok(Some(id))
    }
}
