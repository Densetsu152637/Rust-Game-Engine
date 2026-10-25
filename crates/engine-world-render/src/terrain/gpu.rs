//! GPU state of the terrain features: pipelines, per-terrain textures and bind groups, the
//! shared patch meshes, and per-view node selection.
use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use engine_project::world::terrain::{ResolvedLayer, TerrainRenderItem, TerrainRenderSet};
use engine_render_pipeline::{
    DEPTH_FORMAT, DrawStats, ENGINE_VERTEX_LAYOUT, EngineVertex, FeatureContext, FeatureSetup,
    GpuContext, GpuMesh, HDR_FORMAT, InstanceBatcher, MAX_CASCADES, MOTION_FORMAT, NORMALS_FORMAT,
    ViewId, frustum_planes, sphere_in_frustum,
};
use engine_terrain::{Heightfield, LodLayout, LodSettings, LodViewer, MAX_LAYERS};
use glam::{Mat4, Vec3};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

const SHADER: &str = include_str!("terrain.wgsl");
/// Edge of every layer albedo texture in the array.
pub const ALBEDO_SIZE: u32 = 256;
const ALBEDO_MIPS: u32 = 9;
/// Edge in texels of every layer of a view's terrain shadow map (one `Depth32Float` layer per
/// cascade of the first directional light, [`MAX_CASCADES`] layers: 16 MiB per view).
pub const SHADOW_MAP_SIZE: u32 = 1024;

/// Per-node instance data (`@location(4..=6)` of the terrain vertex shader).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct TerrainInstance {
    /// Origin sample x, origin sample z, step, level.
    pub node: [u32; 4],
    /// Morph start, morph end, viewer gap, 0.
    pub morph: [f32; 4],
    /// Terrain-local viewer position, 0.
    pub viewer: [f32; 4],
    /// Terrain-local viewer position and viewer gap of the view's previous frame (the
    /// geomorph of the motion pass); equal to `viewer` / the gap after a history reset.
    pub previous_viewer: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TerrainUniform {
    world_from_local: [[f32; 4]; 4],
    normal_from_local: [[f32; 4]; 4],
    previous_world_from_local: [[f32; 4]; 4],
    dims: [u32; 4],
    params: [f32; 4],
    tints: [[f32; 4]; MAX_LAYERS],
    layers: [[f32; 4]; MAX_LAYERS],
}

struct Prepared {
    layout: wgpu::BindGroupLayout,
    shadow_layout: wgpu::BindGroupLayout,
    color: wgpu::RenderPipeline,
    normals: wgpu::RenderPipeline,
    shadow: wgpu::RenderPipeline,
    motion: wgpu::RenderPipeline,
    clamp: wgpu::Sampler,
    repeat: wgpu::Sampler,
    compare: wgpu::Sampler,
    /// Receiver group of views without a terrain shadow map this frame (disabled).
    no_shadow: wgpu::BindGroup,
    /// The shadow pass's cascade index (dynamic offset `cascade * cascade_stride`).
    cascades: wgpu::BindGroup,
    cascade_stride: u32,
    _resources: (wgpu::Texture, wgpu::Buffer, wgpu::Buffer),
}

/// Which pass a node collection is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassKind {
    /// Terrain depth into the terrain shadow map: no camera frustum culling (terrain outside
    /// the view casts into it).
    Shadow,
    Normals,
    /// The colour pass; advances the view's motion history.
    Color,
    Motion,
}

/// Terrain-local viewer (xyz, gap) and transform of one view in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewerState {
    viewer: [f32; 4],
    world_from_local: [[f32; 4]; 4],
}

/// Motion history of one terrain in one view.
#[derive(Clone, Copy, Debug)]
struct History {
    previous: ViewerState,
    current: ViewerState,
}

/// The terrain shadow map of one view.
struct ViewShadow {
    layers: Vec<wgpu::TextureView>,
    bind: wgpu::BindGroup,
    /// Rendered in this frame's Shadow stage and not yet used by the colour pass.
    fresh: bool,
    _resources: (wgpu::Texture, wgpu::Buffer),
}

#[derive(Clone, PartialEq)]
struct LayerKey {
    texture: Option<String>,
    image: usize,
}
fn layer_keys(layers: &[ResolvedLayer]) -> Vec<LayerKey> {
    layers
        .iter()
        .map(|l| LayerKey {
            texture: l.texture.clone(),
            image: l.image.as_ref().map_or(0, |i| Arc::as_ptr(i) as usize),
        })
        .collect()
}

struct GpuTerrain {
    field: Arc<Heightfield>,
    revision: u64,
    layers: Vec<LayerKey>,
    lod: LodSettings,
    layout: LodLayout,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    _textures: Vec<wgpu::Texture>,
}

/// Counts of one collection pass.
pub(crate) struct FrameDraw {
    pub terrains: usize,
    pub nodes: usize,
    pub culled: usize,
    pub uploads: u64,
    pub notes: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Shared {
    prepared: Option<Prepared>,
    terrains: BTreeMap<String, GpuTerrain>,
    patches: HashMap<u32, Arc<GpuMesh>>,
    order: Vec<String>,
    shadows: HashMap<ViewId, ViewShadow>,
    history: HashMap<(ViewId, String), History>,
    /// Views whose colour pass drew terrain this frame (the motion pass runs only then).
    colored: HashSet<ViewId>,
}

fn texture_entry(
    binding: u32,
    filterable: bool,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// One terrain pipeline: the patch vertex plus the per-node instance layout.
struct PipelineSpec<'a> {
    label: &'a str,
    vertex: &'a str,
    fragment: &'a str,
    /// Colour target, `None` for depth-only passes.
    format: Option<wgpu::TextureFormat>,
    depth_write: bool,
    cull: Option<wgpu::Face>,
    bias: wgpu::DepthBiasState,
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    spec: PipelineSpec<'_>,
) -> wgpu::RenderPipeline {
    let instance = wgpu::vertex_attr_array![
        4 => Uint32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4
    ];
    let targets = [spec.format.map(wgpu::ColorTargetState::from)];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(spec.vertex),
            compilation_options: Default::default(),
            buffers: &[
                Some(ENGINE_VERTEX_LAYOUT),
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TerrainInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &instance,
                }),
            ],
        },
        primitive: wgpu::PrimitiveState {
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: spec.cull,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(spec.depth_write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: Default::default(),
            bias: spec.bias,
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(spec.fragment),
            compilation_options: Default::default(),
            targets: if spec.format.is_some() { &targets } else { &[] },
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn uniform_entry(binding: u32, dynamic: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: wgpu::BufferSize::new(16),
        },
        count: None,
    }
}

/// A `layers`-layer `Depth32Float` array of `size`^2 texels.
fn depth_array(gpu: &GpuContext, label: &str, size: u32, layers: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// The receiver group (`@group(2)` bindings 0-2 of the colour pass).
fn shadow_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    map: &wgpu::Texture,
    compare: &wgpu::Sampler,
    params: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let view = map.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("world.terrain shadow receiver"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(compare),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
        ],
    })
}

fn params_buffer(gpu: &GpuContext, label: &str, values: [f32; 4]) -> wgpu::Buffer {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue
        .write_buffer(&buffer, 0, bytemuck::cast_slice(&values));
    buffer
}

fn create_texture(
    gpu: &GpuContext,
    label: &str,
    size: [u32; 2],
    layers: u32,
    mips: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: layers,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    mip: u32,
    layer: u32,
    size: [u32; 2],
    texel_bytes: u32,
    data: &[u8],
) {
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * texel_bytes),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
}

/// Nearest-resampled `ALBEDO_SIZE`^2 RGBA8 of a layer image (white without one).
fn albedo_base(layer: &ResolvedLayer) -> Vec<u8> {
    let n = ALBEDO_SIZE as usize;
    let mut out = vec![255u8; n * n * 4];
    let Some(image) = &layer.image else {
        return out;
    };
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 || image.pixels.len() < w * h * 4 {
        return out;
    }
    for y in 0..n {
        for x in 0..n {
            let (sx, sy) = (x * w / n, y * h / n);
            let from = (sy * w + sx) * 4;
            out[(y * n + x) * 4..(y * n + x) * 4 + 4]
                .copy_from_slice(&image.pixels[from..from + 4]);
        }
    }
    out
}
fn downsample(level: &[u8], size: usize) -> Vec<u8> {
    let half = size / 2;
    let mut out = vec![0u8; half * half * 4];
    for y in 0..half {
        for x in 0..half {
            for c in 0..4 {
                let at = |xx: usize, yy: usize| level[(yy * size + xx) * 4 + c] as u32;
                let sum = at(2 * x, 2 * y)
                    + at(2 * x + 1, 2 * y)
                    + at(2 * x, 2 * y + 1)
                    + at(2 * x + 1, 2 * y + 1);
                out[(y * half + x) * 4 + c] = sum.div_ceil(4).min(255) as u8;
            }
        }
    }
    out
}

impl Shared {
    pub(crate) fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let source = setup.composer.compose(SHADER)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world.terrain"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.terrain"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: both,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1, false, both),
                texture_entry(2, true, fragment),
                texture_entry(3, true, fragment),
                texture_entry(4, false, fragment),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let comparison = wgpu::ShaderStages::FRAGMENT;
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.terrain shadow receiver"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: comparison,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: comparison,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                uniform_entry(2, false),
            ],
        });
        let cascade_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.terrain shadow cascade"),
            entries: &[uniform_entry(3, true)],
        });
        let layout_of = |groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("world.terrain"),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let base = layout_of(&[Some(&setup.layouts.view), Some(&layout)]);
        let receiving = layout_of(&[
            Some(&setup.layouts.view),
            Some(&layout),
            Some(&shadow_layout),
        ]);
        let casting = layout_of(&[
            Some(&setup.layouts.view),
            Some(&layout),
            Some(&cascade_layout),
        ]);
        let spec = |label, vertex, fragment, format| PipelineSpec {
            label,
            vertex,
            fragment,
            format,
            depth_write: true,
            cull: Some(wgpu::Face::Back),
            bias: Default::default(),
        };
        let color = pipeline(
            device,
            &receiving,
            &module,
            spec("world.terrain", "vs_main", "fs_main", Some(HDR_FORMAT)),
        );
        let normals = pipeline(
            device,
            &base,
            &module,
            spec(
                "world.terrain.normals",
                "vs_main",
                "fs_normals",
                Some(NORMALS_FORMAT),
            ),
        );
        let shadow = pipeline(
            device,
            &casting,
            &module,
            PipelineSpec {
                // Both faces: grazing light sees ridges from the side.
                cull: None,
                bias: wgpu::DepthBiasState {
                    constant: 0,
                    slope_scale: 1.0,
                    clamp: 0.0,
                },
                ..spec("world.terrain.shadow", "vs_shadow", "fs_shadow", None)
            },
        );
        let motion = pipeline(
            device,
            &base,
            &module,
            PipelineSpec {
                // The colour pass wrote this surface's depth: test against it, write none.
                depth_write: false,
                ..spec(
                    "world.terrain.motion",
                    "vs_motion",
                    "fs_motion",
                    Some(MOTION_FORMAT),
                )
            },
        );
        let clamp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world.terrain clamp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let repeat = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world.terrain repeat"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let compare = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world.terrain shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let placeholder = depth_array(setup.gpu, "world.terrain no shadow", 1, 1);
        let disabled = params_buffer(setup.gpu, "world.terrain no shadow", [0.0, 1.0, 0.0, 0.0]);
        let no_shadow = shadow_group(device, &shadow_layout, &placeholder, &compare, &disabled);
        let cascade_stride = device.limits().min_uniform_buffer_offset_alignment.max(16);
        let cascade_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("world.terrain shadow cascades"),
            size: u64::from(cascade_stride * MAX_CASCADES),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for cascade in 0..MAX_CASCADES {
            setup.gpu.queue.write_buffer(
                &cascade_buffer,
                u64::from(cascade * cascade_stride),
                bytemuck::cast_slice(&[cascade, 0, 0, 0]),
            );
        }
        let cascades = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world.terrain shadow cascades"),
            layout: &cascade_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &cascade_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(16),
                }),
            }],
        });
        self.prepared = Some(Prepared {
            layout,
            shadow_layout,
            color,
            normals,
            shadow,
            motion,
            clamp,
            repeat,
            compare,
            no_shadow,
            cascades,
            cascade_stride,
            _resources: (placeholder, disabled, cascade_buffer),
        });
        self.terrains.clear();
        self.patches.clear();
        self.order.clear();
        self.shadows.clear();
        self.history.clear();
        self.colored.clear();
        Ok(())
    }

    fn patch(&mut self, gpu: &GpuContext, n: u32) -> Result<Arc<GpuMesh>> {
        if let Some(mesh) = self.patches.get(&n) {
            return Ok(mesh.clone());
        }
        let side = n + 1;
        let mut vertices = Vec::with_capacity((side * side) as usize);
        for j in 0..side {
            for i in 0..side {
                vertices.push(EngineVertex {
                    position: [i as f32, 0.0, j as f32],
                    uv: [0.0; 2],
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
            GpuMesh::from_engine_vertices(&gpu.device, "world.terrain patch", &vertices, &indices)?;
        self.patches.insert(n, mesh.clone());
        Ok(mesh)
    }

    /// Uploads `item`'s textures when its heightfield or layers changed (returns whether it
    /// did) and writes its uniform with the view's previous transform.
    fn sync_terrain(
        &mut self,
        gpu: &GpuContext,
        item: &TerrainRenderItem,
        previous_world_from_local: [[f32; 4]; 4],
    ) -> Result<bool> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.terrain is not prepared")?;
        let keys = layer_keys(&item.layers);
        let fresh = self.terrains.get(&item.id).is_none_or(|t| {
            !Arc::ptr_eq(&t.field, &item.field)
                || t.revision != item.field.revision()
                || t.layers != keys
        });
        if fresh {
            let field = &item.field;
            let size = [field.columns(), field.rows()];
            let heights = create_texture(
                gpu,
                "world.terrain heights",
                size,
                1,
                1,
                wgpu::TextureFormat::R32Float,
            );
            write(
                gpu,
                &heights,
                0,
                0,
                size,
                4,
                bytemuck::cast_slice(field.heights()),
            );
            let n = field.layers().len();
            let mut splat = [
                vec![0u8; field.sample_count() * 4],
                vec![0u8; field.sample_count() * 4],
            ];
            for (sample, weights) in field.splat().chunks_exact(n).enumerate() {
                for (layer, &w) in weights.iter().enumerate() {
                    splat[layer / 4][sample * 4 + layer % 4] = w;
                }
            }
            let mut splats = Vec::new();
            for data in &splat {
                let texture = create_texture(
                    gpu,
                    "world.terrain splat",
                    size,
                    1,
                    1,
                    wgpu::TextureFormat::Rgba8Unorm,
                );
                write(gpu, &texture, 0, 0, size, 4, data);
                splats.push(texture);
            }
            let cells = [size[0] - 1, size[1] - 1];
            let hole_bytes: Vec<u8> = field
                .hole_flags()
                .iter()
                .map(|&h| if h { 255 } else { 0 })
                .collect();
            let holes = create_texture(
                gpu,
                "world.terrain holes",
                cells,
                1,
                1,
                wgpu::TextureFormat::R8Unorm,
            );
            write(gpu, &holes, 0, 0, cells, 1, &hole_bytes);
            let albedo = create_texture(
                gpu,
                "world.terrain albedo",
                [ALBEDO_SIZE; 2],
                MAX_LAYERS as u32,
                ALBEDO_MIPS,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            );
            for layer in 0..MAX_LAYERS {
                let mut level = item.layers.get(layer).map_or_else(
                    || vec![255u8; (ALBEDO_SIZE * ALBEDO_SIZE * 4) as usize],
                    albedo_base,
                );
                let mut edge = ALBEDO_SIZE as usize;
                for mip in 0..ALBEDO_MIPS {
                    write(gpu, &albedo, mip, layer as u32, [edge as u32; 2], 4, &level);
                    if edge > 1 {
                        level = downsample(&level, edge);
                        edge /= 2;
                    }
                }
            }
            let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("world.terrain uniform"),
                size: std::mem::size_of::<TerrainUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let view = |t: &wgpu::Texture| t.create_view(&Default::default());
            let albedo_view = albedo.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            let views = [
                view(&heights),
                view(&splats[0]),
                view(&splats[1]),
                view(&holes),
            ];
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("world.terrain"),
                layout: &prepared.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&views[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&views[1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&views[2]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&views[3]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&albedo_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::Sampler(&prepared.clamp),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::Sampler(&prepared.repeat),
                    },
                ],
            });
            let layout = LodLayout::for_heightfield(field, &item.lod)
                .map_err(|e| anyhow::anyhow!("{}: {e}", item.id))?;
            let mut textures = vec![heights, holes, albedo];
            textures.extend(splats);
            self.terrains.insert(
                item.id.clone(),
                GpuTerrain {
                    field: field.clone(),
                    revision: field.revision(),
                    layers: keys,
                    lod: item.lod,
                    layout,
                    uniform,
                    bind,
                    _textures: textures,
                },
            );
        }
        let terrain = self.terrains.get_mut(&item.id).expect("synced");
        if terrain.lod != item.lod {
            terrain.layout = LodLayout::for_heightfield(&item.field, &item.lod)
                .map_err(|e| anyhow::anyhow!("{}: {e}", item.id))?;
            terrain.lod = item.lod;
        }
        let world_from_local = Mat4::from_cols_array_2d(&item.world_from_local);
        let mut uniform = TerrainUniform {
            world_from_local: item.world_from_local,
            normal_from_local: world_from_local.inverse().transpose().to_cols_array_2d(),
            previous_world_from_local,
            dims: [
                item.field.columns(),
                item.field.rows(),
                item.layers.len().min(MAX_LAYERS) as u32,
                0,
            ],
            params: [item.field.spacing(), 0.0, 0.0, 0.0],
            tints: [[0.0; 4]; MAX_LAYERS],
            layers: [[1.0, 0.0, 0.0, 0.0]; MAX_LAYERS],
        };
        for (i, layer) in item.layers.iter().take(MAX_LAYERS).enumerate() {
            uniform.tints[i] = layer.tint;
            uniform.layers[i] = [
                layer.uv_tiling,
                f32::from(u8::from(layer.image.is_some())),
                0.0,
                0.0,
            ];
        }
        gpu.queue
            .write_buffer(&terrain.uniform, 0, bytemuck::bytes_of(&uniform));
        Ok(fresh)
    }

    /// Selects and culls the nodes of every GPU terrain for this view and pushes them into
    /// `batcher` (key = terrain order index). `kind` decides the culling (none for the shadow
    /// map) and whether the view's motion history advances (the colour pass).
    pub(crate) fn collect(
        &mut self,
        ctx: &FeatureContext<'_>,
        batcher: &mut InstanceBatcher<TerrainInstance>,
        kind: PassKind,
    ) -> Result<FrameDraw> {
        if self.prepared.is_none() {
            bail!("world.terrain is not prepared");
        }
        let mut frame = FrameDraw {
            terrains: 0,
            nodes: 0,
            culled: 0,
            uploads: 0,
            notes: Vec::new(),
        };
        self.order.clear();
        let Some(world) = ctx.world else {
            frame
                .notes
                .push("the host supplied no extracted world".into());
            return Ok(frame);
        };
        let Some(set) = world.get_resource::<TerrainRenderSet>() else {
            frame.notes.push(
                "no TerrainRenderSet in the extracted world (call engine_world_render::install)"
                    .into(),
            );
            return Ok(frame);
        };
        // Drop GPU data and motion history of terrains no longer in the set.
        let live: Vec<&str> = set.items.iter().map(|i| i.id.as_str()).collect();
        self.terrains.retain(|id, _| live.contains(&id.as_str()));
        self.history
            .retain(|(_, id), _| live.contains(&id.as_str()));
        let planes = frustum_planes(ctx.view.clip_from_world());
        let camera = ctx.view.camera_position();
        let view_id = ctx.view.id;
        for item in &set.items {
            let world_from_local = Mat4::from_cols_array_2d(&item.world_from_local);
            let local_from_world = world_from_local.inverse();
            let viewer = local_from_world.transform_point3(camera);
            let range = item.field.height_range();
            let gap = LodViewer::new(viewer, range).gap;
            let now = ViewerState {
                viewer: [viewer.x, viewer.y, viewer.z, gap],
                world_from_local: item.world_from_local,
            };
            let key = (view_id, item.id.clone());
            let previous = match kind {
                PassKind::Color => {
                    let previous = match self.history.get(&key) {
                        Some(h) if !ctx.view.reset_history => h.current,
                        _ => now,
                    };
                    self.history.insert(
                        key,
                        History {
                            previous,
                            current: now,
                        },
                    );
                    previous
                }
                PassKind::Motion => self.history.get(&key).map_or(now, |h| h.previous),
                PassKind::Shadow | PassKind::Normals => now,
            };
            match self.sync_terrain(ctx.gpu, item, previous.world_from_local) {
                Ok(true) => frame.uploads += 1,
                Ok(false) => {}
                Err(error) => {
                    frame.notes.push(error.to_string());
                    continue;
                }
            }
            let patch = self.patch(ctx.gpu, item.lod.node_resolution)?;
            let terrain = &self.terrains[&item.id];
            let selection = terrain.layout.select(viewer, range);
            let scale = world_from_local
                .x_axis
                .truncate()
                .length()
                .max(world_from_local.y_axis.truncate().length())
                .max(world_from_local.z_axis.truncate().length());
            let order = self.order.len() as u64;
            let s = item.field.spacing();
            let mut drawn = 0;
            for node in &selection.nodes {
                if kind != PassKind::Shadow {
                    let rect = terrain.layout.node_samples(node.key);
                    let lo = Vec3::new(rect.min[0] as f32 * s, range[0], rect.min[1] as f32 * s);
                    let hi = Vec3::new(
                        (rect.max[0] - 1) as f32 * s,
                        range[1],
                        (rect.max[1] - 1) as f32 * s,
                    );
                    let center = world_from_local.transform_point3((lo + hi) * 0.5);
                    let radius = (hi - lo).length() * 0.5 * scale;
                    if !sphere_in_frustum(&planes, center, radius) {
                        frame.culled += 1;
                        continue;
                    }
                }
                batcher.push(
                    &patch,
                    order,
                    TerrainInstance {
                        node: [node.origin[0], node.origin[1], node.step, node.key.level],
                        morph: [node.morph[0], node.morph[1], selection.viewer.gap, 0.0],
                        viewer: [viewer.x, viewer.y, viewer.z, 0.0],
                        previous_viewer: previous.viewer,
                    },
                );
                drawn += 1;
            }
            self.order.push(item.id.clone());
            if drawn > 0 {
                frame.terrains += 1;
                frame.nodes += drawn;
            }
        }
        Ok(frame)
    }

    /// Draws the batches `collect` produced with the pipeline of `kind`; `group2` is the
    /// receiver (colour) or cascade (shadow) group with its dynamic offsets.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view_group: &wgpu::BindGroup,
        batcher: &InstanceBatcher<TerrainInstance>,
        kind: PassKind,
        group2: Option<(&wgpu::BindGroup, &[u32])>,
    ) -> Result<DrawStats> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.terrain is not prepared")?;
        pass.set_pipeline(match kind {
            PassKind::Shadow => &prepared.shadow,
            PassKind::Normals => &prepared.normals,
            PassKind::Color => &prepared.color,
            PassKind::Motion => &prepared.motion,
        });
        pass.set_bind_group(0, view_group, &[]);
        if let Some((group, offsets)) = group2 {
            pass.set_bind_group(2, group, offsets);
        }
        let stats = batcher.draw(pass, Some(1), |pass, key| {
            if let Some(terrain) = self
                .order
                .get(key as usize)
                .and_then(|id| self.terrains.get(id))
            {
                pass.set_bind_group(1, &terrain.bind, &[]);
            }
        });
        Ok(stats)
    }

    /// The layer views of `view`'s terrain shadow map (created on first use), marked as
    /// holding this frame's terrain depth.
    pub(crate) fn shadow_layers(
        &mut self,
        gpu: &GpuContext,
        view: ViewId,
    ) -> Result<Vec<wgpu::TextureView>> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.terrain is not prepared")?;
        let shadow = self.shadows.entry(view).or_insert_with(|| {
            let texture = depth_array(
                gpu,
                "world.terrain shadow map",
                SHADOW_MAP_SIZE,
                MAX_CASCADES,
            );
            let params = params_buffer(
                gpu,
                "world.terrain shadow",
                [1.0, SHADOW_MAP_SIZE as f32, 0.0, 0.0],
            );
            let bind = shadow_group(
                &gpu.device,
                &prepared.shadow_layout,
                &texture,
                &prepared.compare,
                &params,
            );
            let layers = (0..MAX_CASCADES)
                .map(|layer| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("world.terrain shadow layer"),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_array_layer: layer,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect();
            ViewShadow {
                layers,
                bind,
                fresh: false,
                _resources: (texture, params),
            }
        });
        shadow.fresh = true;
        Ok(shadow.layers.clone())
    }

    /// The cascade group and the dynamic offset of each cascade.
    pub(crate) fn cascades(&self) -> Result<(wgpu::BindGroup, Vec<u32>)> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.terrain is not prepared")?;
        Ok((
            prepared.cascades.clone(),
            (0..MAX_CASCADES)
                .map(|c| c * prepared.cascade_stride)
                .collect(),
        ))
    }

    /// The colour pass's receiver group: `view`'s terrain shadow map when the Shadow stage
    /// rendered it this frame (consumed), otherwise the disabled placeholder. Returns whether
    /// the map is used.
    pub(crate) fn take_receiver(&mut self, view: ViewId) -> Result<(wgpu::BindGroup, bool)> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.terrain is not prepared")?;
        Ok(match self.shadows.get_mut(&view) {
            Some(shadow) if shadow.fresh => {
                shadow.fresh = false;
                (shadow.bind.clone(), true)
            }
            _ => (prepared.no_shadow.clone(), false),
        })
    }

    /// Records that `view`'s colour pass drew terrain this frame.
    pub(crate) fn mark_colored(&mut self, view: ViewId) {
        self.colored.insert(view);
    }

    /// Whether `view`'s colour pass drew terrain this frame (consumed).
    pub(crate) fn take_colored(&mut self, view: ViewId) -> bool {
        self.colored.remove(&view)
    }
}
