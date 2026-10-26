//! GPU state of `world.foliage`: pipelines, per-type bind groups, uploaded meshes and textures,
//! and the per-view cell / frustum / distance / LOD selection that feeds the instance batcher.
use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use engine_foliage::{CellKey, LodLevel, LodTracker};
use engine_project::content::{Mesh, Texture};
use engine_project::world::foliage::{FoliageRenderItem, FoliageRenderSet, FoliageRenderType};
use engine_render_pipeline::{
    DEPTH_FORMAT, DrawStats, ENGINE_VERTEX_LAYOUT, EngineVertex, FeatureContext, FeatureSetup,
    GpuContext, GpuMesh, GpuTexture, HDR_FORMAT, InstanceBatcher, MAX_SHADOWS, ViewId,
    frustum_planes, sphere_in_frustum,
};
use glam::{Mat4, Vec3};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const SHADER: &str = include_str!("foliage.wgsl");

/// Per-instance vertex data (`@location(4..=6)` of the foliage vertex shaders).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct FoliageInstance {
    /// Rotation quaternion `(x, y, z, w)`.
    pub rotation: [f32; 4],
    /// Position `xyz`, wind phase `w`.
    pub position_phase: [f32; 4],
    /// Scale `xyz`, 0.
    pub scale: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TypeUniform {
    tint: [f32; 4],
    wind: [f32; 4],
    impostor: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WindUniform {
    a: [f32; 4],
    b: [f32; 4],
}

struct Prepared {
    type_layout: wgpu::BindGroupLayout,
    wind_layout: wgpu::BindGroupLayout,
    mesh: wgpu::RenderPipeline,
    billboard: wgpu::RenderPipeline,
    shadow_mesh: wgpu::RenderPipeline,
    shadow_billboard: wgpu::RenderPipeline,
    shadow_indices: wgpu::BindGroup,
    shadow_index_stride: u32,
    _shadow_index_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    white: Arc<GpuTexture>,
    quad: Arc<GpuMesh>,
}

struct GpuType {
    meshes: Vec<Arc<GpuMesh>>,
    mesh_bind: wgpu::BindGroup,
    billboard_bind: Option<wgpu::BindGroup>,
    /// Conservative radius around the instance origin at scale 1 (meshes and impostor).
    radius: f32,
    _buffers: Vec<wgpu::Buffer>,
}

/// Facts about a cell, rebuilt when its revision changes.
struct CellFacts {
    revision: u64,
    y: [f32; 2],
    /// Largest `radius * scale` of its instances.
    reach: f32,
}

struct GpuSet {
    signature: Vec<usize>,
    types: Vec<GpuType>,
    wind_buffer: wgpu::Buffer,
    wind_bind: wgpu::BindGroup,
    cells: HashMap<CellKey, CellFacts>,
    trackers: HashMap<ViewId, LodTracker>,
}

/// One drawn batch (a type at one level).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchInfo {
    pub set: String,
    pub type_name: String,
    /// `Mesh(n)` or `Impostor`.
    pub level: LodLevel,
    pub instances: usize,
}

/// Counts of one collection pass.
#[derive(Default)]
pub(crate) struct FrameDraw {
    pub sets: usize,
    pub instances: usize,
    pub impostors: usize,
    pub cells_visited: usize,
    pub cells_culled: usize,
    pub culled_distance: usize,
    pub culled_frustum: usize,
    pub uploads: u64,
    pub batches: Vec<BatchInfo>,
    pub notes: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Shared {
    prepared: Option<Prepared>,
    sets: BTreeMap<String, GpuSet>,
    /// Keyed by the address of the source `Arc` (kept alive in the value).
    meshes: HashMap<usize, (Arc<Mesh>, Arc<GpuMesh>)>,
    textures: HashMap<usize, (Arc<Texture>, Arc<GpuTexture>)>,
    order: Vec<String>,
}

fn smooth_normals(mesh: &Mesh) -> Vec<[f32; 3]> {
    let mut sums = vec![Vec3::ZERO; mesh.vertices.len()];
    for tri in mesh.indices.as_chunks::<3>().0 {
        let p = |i: u32| {
            mesh.vertices
                .get(i as usize)
                .map(|v| Vec3::from(v.position))
        };
        if let (Some(a), Some(b), Some(c)) = (p(tri[0]), p(tri[1]), p(tri[2])) {
            let n = (b - a).cross(c - a);
            for &i in tri {
                if let Some(s) = sums.get_mut(i as usize) {
                    *s += n;
                }
            }
        }
    }
    sums.into_iter()
        .map(|n| n.normalize_or(Vec3::Y).to_array())
        .collect()
}

fn engine_vertices(mesh: &Mesh) -> Vec<EngineVertex> {
    let normals = if mesh.normals.len() == mesh.vertices.len() {
        mesh.normals.clone()
    } else {
        smooth_normals(mesh)
    };
    mesh.vertices
        .iter()
        .zip(normals)
        .map(|(v, normal)| EngineVertex {
            position: v.position,
            uv: v.uv,
            color: v.color,
            normal,
        })
        .collect()
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    vertex: &str,
    shadow: bool,
) -> wgpu::RenderPipeline {
    let instance = wgpu::vertex_attr_array![4 => Float32x4, 5 => Float32x4, 6 => Float32x4];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(vertex),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vertex),
            compilation_options: Default::default(),
            buffers: &[
                Some(ENGINE_VERTEX_LAYOUT),
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<FoliageInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &instance,
                }),
            ],
        },
        primitive: wgpu::PrimitiveState {
            front_face: wgpu::FrontFace::Ccw,
            // Leaves and cards are seen from both sides.
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
            module,
            entry_point: Some(if shadow { "fs_shadow" } else { "fs_main" }),
            compilation_options: Default::default(),
            targets: if shadow {
                &[]
            } else {
                &[Some(HDR_FORMAT.into())]
            },
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Local bounding data of a mesh: (min y, max y, largest vertex distance from the origin).
fn mesh_extent(mesh: &Mesh) -> (f32, f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut radius = 0.0f32;
    for v in &mesh.vertices {
        lo = lo.min(v.position[1]);
        hi = hi.max(v.position[1]);
        radius = radius.max(Vec3::from(v.position).length());
    }
    if lo > hi {
        (0.0, 0.0, 0.0)
    } else {
        (lo, hi, radius)
    }
}

impl Shared {
    pub(crate) fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let source = setup.composer.compose(SHADER)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world.foliage"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let type_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.foliage type"),
            entries: &[
                uniform_entry(0, both),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let wind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world.foliage wind"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world.foliage"),
            bind_group_layouts: &[
                Some(&setup.layouts.view),
                Some(&type_layout),
                Some(&wind_layout),
            ],
            immediate_size: 0,
        });
        let shadow_index_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("world.foliage shadow index"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                }],
            });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world.foliage.shadow"),
            bind_group_layouts: &[
                Some(&setup.layouts.view),
                Some(&type_layout),
                Some(&wind_layout),
                Some(&shadow_index_layout),
            ],
            immediate_size: 0,
        });
        let mesh = pipeline(device, &pipeline_layout, &module, "vs_mesh", false);
        let billboard = pipeline(device, &pipeline_layout, &module, "vs_billboard", false);
        let shadow_mesh = pipeline(device, &shadow_layout, &module, "vs_shadow_mesh", true);
        let shadow_billboard =
            pipeline(device, &shadow_layout, &module, "vs_shadow_billboard", true);
        let shadow_index_stride = device.limits().min_uniform_buffer_offset_alignment.max(16);
        let shadow_index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("world.foliage shadow indices"),
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
            label: Some("world.foliage shadow indices"),
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world.foliage"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white = GpuTexture::rgba8(
            device,
            &setup.gpu.queue,
            "world.foliage white",
            [1, 1],
            &[255; 4],
        )?;
        let vertex = |x: f32, y: f32| EngineVertex {
            position: [x, y, 0.0],
            uv: [(x + 1.0) * 0.5, 1.0 - y],
            color: [1.0; 4],
            normal: [0.0, 0.0, 1.0],
        };
        let quad = GpuMesh::from_engine_vertices(
            device,
            "world.foliage impostor quad",
            &[
                vertex(-1.0, 0.0),
                vertex(1.0, 0.0),
                vertex(1.0, 1.0),
                vertex(-1.0, 1.0),
            ],
            &[0, 1, 2, 0, 2, 3],
        )?;
        self.prepared = Some(Prepared {
            type_layout,
            wind_layout,
            mesh,
            billboard,
            shadow_mesh,
            shadow_billboard,
            shadow_indices,
            shadow_index_stride,
            _shadow_index_buffer: shadow_index_buffer,
            sampler,
            white,
            quad,
        });
        self.sets.clear();
        self.meshes.clear();
        self.textures.clear();
        self.order.clear();
        Ok(())
    }

    fn mesh(&mut self, gpu: &GpuContext, mesh: &Arc<Mesh>) -> Result<Arc<GpuMesh>> {
        let key = Arc::as_ptr(mesh) as usize;
        if let Some((_, uploaded)) = self.meshes.get(&key) {
            return Ok(uploaded.clone());
        }
        let vertices = engine_vertices(mesh);
        let uploaded = GpuMesh::from_engine_vertices(
            &gpu.device,
            "world.foliage mesh",
            &vertices,
            &mesh.indices,
        )?;
        self.meshes.insert(key, (mesh.clone(), uploaded.clone()));
        Ok(uploaded)
    }

    fn texture(&mut self, gpu: &GpuContext, texture: &Arc<Texture>) -> Result<Arc<GpuTexture>> {
        let key = Arc::as_ptr(texture) as usize;
        if let Some((_, uploaded)) = self.textures.get(&key) {
            return Ok(uploaded.clone());
        }
        let uploaded = GpuTexture::rgba8(
            &gpu.device,
            &gpu.queue,
            "world.foliage texture",
            [texture.width, texture.height],
            &texture.pixels,
        )?;
        self.textures
            .insert(key, (texture.clone(), uploaded.clone()));
        Ok(uploaded)
    }

    fn build_type(&mut self, gpu: &GpuContext, t: &FoliageRenderType) -> Result<GpuType> {
        let (layout, sampler, white) = {
            let p = self
                .prepared
                .as_ref()
                .context("world.foliage is not prepared")?;
            (p.type_layout.clone(), p.sampler.clone(), p.white.clone())
        };
        let mut meshes = Vec::new();
        for m in &t.meshes {
            meshes.push(self.mesh(gpu, m)?);
        }
        let (base, top, mut radius) = t.meshes.first().map_or((0.0, 0.0, 0.0), |m| mesh_extent(m));
        let extent = top - base;
        let albedo = match &t.texture {
            Some(texture) => self.texture(gpu, texture)?,
            None => white,
        };
        let impostor_size = t.impostor.as_ref().map_or([0.0; 2], |i| i.size);
        if t.impostor.is_some() {
            radius = radius.max((impostor_size[0] * 0.5).hypot(impostor_size[1]));
        }
        let uniform = TypeUniform {
            tint: t.tint,
            wind: [
                base,
                if extent > 1e-6 { 1.0 / extent } else { 0.0 },
                t.wind_weight,
                0.0,
            ],
            impostor: [impostor_size[0], impostor_size[1], 0.0, 0.0],
        };
        let make = |label: &str, uniform: &TypeUniform, texture: &GpuTexture| {
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<TypeUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue
                .write_buffer(&buffer, 0, bytemuck::bytes_of(uniform));
            (buffer, texture.view.clone())
        };
        let bind = |buffer: &wgpu::Buffer, view: &wgpu::TextureView| {
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("world.foliage type"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let (mesh_buffer, mesh_view) = make("world.foliage type", &uniform, &albedo);
        let mesh_bind = bind(&mesh_buffer, &mesh_view);
        let mut buffers = vec![mesh_buffer];
        let billboard_bind = match &t.impostor {
            Some(i) => {
                let texture = self.texture(gpu, &i.texture)?;
                let (buffer, view) = make("world.foliage impostor", &uniform, &texture);
                let bind_group = bind(&buffer, &view);
                buffers.push(buffer);
                Some(bind_group)
            }
            None => None,
        };
        Ok(GpuType {
            meshes,
            mesh_bind,
            billboard_bind,
            radius,
            _buffers: buffers,
        })
    }

    /// Builds (or rebuilds when the source assets changed) the GPU data of `item`; returns
    /// whether it did.
    fn sync_set(&mut self, gpu: &GpuContext, item: &FoliageRenderItem) -> Result<bool> {
        let signature: Vec<usize> = item
            .types
            .iter()
            .flat_map(|t| {
                t.meshes
                    .iter()
                    .map(|m| Arc::as_ptr(m) as usize)
                    .chain(t.texture.iter().map(|x| Arc::as_ptr(x) as usize))
                    .chain(t.impostor.iter().map(|i| Arc::as_ptr(&i.texture) as usize))
                    .chain([
                        t.tint[0].to_bits() as usize,
                        t.wind_weight.to_bits() as usize,
                    ])
                    .chain(t.impostor.iter().map(|i| i.size[0].to_bits() as usize))
                    .chain(t.impostor.iter().map(|i| i.size[1].to_bits() as usize))
                    .chain([usize::MAX])
            })
            .collect();
        let fresh = self
            .sets
            .get(&item.id)
            .is_none_or(|s| s.signature != signature);
        if fresh {
            let mut types = Vec::with_capacity(item.types.len());
            for t in &item.types {
                types.push(self.build_type(gpu, t)?);
            }
            let prepared = self
                .prepared
                .as_ref()
                .context("world.foliage is not prepared")?;
            let wind_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("world.foliage wind"),
                size: std::mem::size_of::<WindUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let wind_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("world.foliage wind"),
                layout: &prepared.wind_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wind_buffer.as_entire_binding(),
                }],
            });
            self.sets.insert(
                item.id.clone(),
                GpuSet {
                    signature,
                    types,
                    wind_buffer,
                    wind_bind,
                    cells: HashMap::new(),
                    trackers: HashMap::new(),
                },
            );
        }
        let set = self.sets.get(&item.id).expect("synced");
        let block = item.wind.gpu_block();
        gpu.queue.write_buffer(
            &set.wind_buffer,
            0,
            bytemuck::bytes_of(&WindUniform {
                a: block[0],
                b: block[1],
            }),
        );
        Ok(fresh)
    }

    /// Culls and selects levels for every GPU foliage set in this view and pushes the surviving
    /// instances into `batcher` (key = set index, type index and the impostor bit).
    pub(crate) fn collect(
        &mut self,
        ctx: &FeatureContext<'_>,
        batcher: &mut InstanceBatcher<FoliageInstance>,
    ) -> Result<FrameDraw> {
        if self.prepared.is_none() {
            bail!("world.foliage is not prepared");
        }
        let mut frame = FrameDraw::default();
        self.order.clear();
        let Some(world) = ctx.world else {
            frame
                .notes
                .push("the host supplied no extracted world".into());
            return Ok(frame);
        };
        let Some(render_set) = world.get_resource::<FoliageRenderSet>() else {
            frame.notes.push(
                "no FoliageRenderSet in the extracted world (call engine_world_render::install)"
                    .into(),
            );
            return Ok(frame);
        };
        let live: Vec<&str> = render_set.items.iter().map(|i| i.id.as_str()).collect();
        self.sets.retain(|id, _| live.contains(&id.as_str()));
        let planes = frustum_planes(ctx.view.clip_from_world());
        let camera = ctx.view.camera_position();
        let view_id = ctx.view.id;
        let mut counts: BTreeMap<(usize, usize, u8), usize> = BTreeMap::new();
        for item in &render_set.items {
            match self.sync_set(ctx.gpu, item) {
                Ok(true) => frame.uploads += 1,
                Ok(false) => {}
                Err(error) => {
                    frame.notes.push(format!("{}: {error}", item.id));
                    continue;
                }
            }
            let set_index = self.order.len();
            self.order.push(item.id.clone());
            let quad = self.prepared.as_ref().expect("prepared").quad.clone();
            let set = self.sets.get_mut(&item.id).expect("synced");
            frame.sets += 1;
            set.cells.retain(|k, _| item.field.cell(*k).is_some());
            let tracker = set.trackers.entry(view_id).or_default();
            tracker.retain(|k| item.field.cell(k).is_some());
            let widest = item
                .types
                .iter()
                .map(|t| t.bands.cull_distance())
                .fold(0.0f32, f32::max);
            for (key, cell) in item.field.cells() {
                frame.cells_visited += 1;
                let facts = set.cells.entry(key).or_insert(CellFacts {
                    revision: u64::MAX,
                    y: [0.0; 2],
                    reach: 0.0,
                });
                if facts.revision != cell.revision {
                    let mut y = [f32::INFINITY, f32::NEG_INFINITY];
                    let mut reach = 0.0f32;
                    for i in &cell.instances {
                        y = [y[0].min(i.position[1]), y[1].max(i.position[1])];
                        let radius = set
                            .types
                            .get(usize::from(i.type_index))
                            .map_or(0.0, |t| t.radius);
                        reach = reach.max(radius * i.scale.iter().copied().fold(0.0, f32::max));
                    }
                    *facts = CellFacts {
                        revision: cell.revision,
                        y,
                        reach,
                    };
                }
                // Cell bounding sphere and distance test.
                let rect = item.field.cell_rect(key);
                let lo = Vec3::new(rect[0][0], facts.y[0] - facts.reach, rect[0][1]);
                let hi = Vec3::new(rect[1][0], facts.y[1] + facts.reach, rect[1][1]);
                let nearest = camera.clamp(lo, hi);
                if nearest.distance(camera) > widest {
                    frame.cells_culled += 1;
                    continue;
                }
                let centre = (lo + hi) * 0.5;
                if !sphere_in_frustum(&planes, centre, (hi - lo).length() * 0.5) {
                    frame.cells_culled += 1;
                    continue;
                }
                let levels = tracker.levels_mut(key, cell.revision, cell.instances.len());
                for (index, instance) in cell.instances.iter().enumerate() {
                    let t = usize::from(instance.type_index);
                    let (Some(render_type), Some(gpu_type)) = (item.types.get(t), set.types.get(t))
                    else {
                        continue;
                    };
                    let position = Vec3::from(instance.position);
                    let distance = position.distance(camera);
                    let previous = LodLevel::from_code(levels[index]);
                    let level = render_type.bands.select(distance, previous);
                    levels[index] = level.to_code();
                    if level == LodLevel::Culled {
                        frame.culled_distance += 1;
                        continue;
                    }
                    let scale = instance.scale.iter().copied().fold(0.0, f32::max);
                    if !sphere_in_frustum(&planes, position, gpu_type.radius * scale) {
                        frame.culled_frustum += 1;
                        continue;
                    }
                    let data = FoliageInstance {
                        rotation: instance.rotation,
                        position_phase: [
                            instance.position[0],
                            instance.position[1],
                            instance.position[2],
                            instance.phase,
                        ],
                        scale: [instance.scale[0], instance.scale[1], instance.scale[2], 0.0],
                    };
                    match level {
                        LodLevel::Mesh(n) => {
                            let n = usize::from(n).min(gpu_type.meshes.len() - 1);
                            batcher.push(
                                &gpu_type.meshes[n],
                                ((set_index << 20) | (t << 1)) as u64,
                                data,
                            );
                            *counts.entry((set_index, t, n as u8)).or_default() += 1;
                        }
                        LodLevel::Impostor => {
                            if gpu_type.billboard_bind.is_none() {
                                continue;
                            }
                            batcher.push(&quad, ((set_index << 20) | (t << 1) | 1) as u64, data);
                            *counts.entry((set_index, t, u8::MAX)).or_default() += 1;
                            frame.impostors += 1;
                        }
                        LodLevel::Culled => {}
                    }
                    frame.instances += 1;
                }
            }
        }
        for ((set_index, t, level), instances) in counts {
            let id = &self.order[set_index];
            let name = render_set
                .items
                .iter()
                .find(|i| &i.id == id)
                .and_then(|i| i.types.get(t))
                .map(|t| t.name.clone())
                .unwrap_or_default();
            frame.batches.push(BatchInfo {
                set: id.clone(),
                type_name: name,
                level: if level == u8::MAX {
                    LodLevel::Impostor
                } else {
                    LodLevel::Mesh(level)
                },
                instances,
            });
        }
        Ok(frame)
    }

    /// Select casters against this light's tile, independently of colour frustum culling.
    /// Beyond the camera's visible distance, use the coarsest real mesh instead of
    /// suppressing a caster that can still project into the tile.
    pub(crate) fn collect_shadow(
        &mut self,
        ctx: &FeatureContext<'_>,
        batcher: &mut InstanceBatcher<FoliageInstance>,
        clip: Mat4,
    ) -> Result<FrameDraw> {
        if self.prepared.is_none() {
            bail!("world.foliage is not prepared");
        }
        let mut frame = FrameDraw::default();
        self.order.clear();
        let Some(render_set) = ctx.world.and_then(|w| w.get_resource::<FoliageRenderSet>()) else {
            return Ok(frame);
        };
        let live: Vec<&str> = render_set.items.iter().map(|i| i.id.as_str()).collect();
        self.sets.retain(|id, _| live.contains(&id.as_str()));
        let planes = frustum_planes(clip);
        let camera = ctx.view.camera_position();
        let quad = self.prepared.as_ref().expect("prepared").quad.clone();
        for item in &render_set.items {
            match self.sync_set(ctx.gpu, item) {
                Ok(true) => frame.uploads += 1,
                Ok(false) => {}
                Err(error) => {
                    frame.notes.push(format!("{}: {error}", item.id));
                    continue;
                }
            }
            let set_index = self.order.len();
            self.order.push(item.id.clone());
            let set = self.sets.get(&item.id).expect("synced");
            for (_, cell) in item.field.cells() {
                for instance in &cell.instances {
                    let t = usize::from(instance.type_index);
                    let (Some(render_type), Some(gpu_type)) = (item.types.get(t), set.types.get(t))
                    else {
                        continue;
                    };
                    let position = Vec3::from(instance.position);
                    let scale = instance.scale.iter().copied().fold(0.0, f32::max);
                    // Wind displaces vertices horizontally; bound the maximum gust/side sway.
                    let wind_reach = item.wind.strength.abs()
                        * (1.0 + item.wind.gust_strength.abs())
                        * render_type.wind_weight.abs()
                        * 1.5;
                    if !sphere_in_frustum(&planes, position, gpu_type.radius * scale + wind_reach) {
                        continue;
                    }
                    let distance = position.distance(camera);
                    let level = match render_type.bands.select(distance, LodLevel::Culled) {
                        LodLevel::Culled => LodLevel::Mesh((gpu_type.meshes.len() - 1) as u8),
                        level => level,
                    };
                    let data = FoliageInstance {
                        rotation: instance.rotation,
                        position_phase: [
                            instance.position[0],
                            instance.position[1],
                            instance.position[2],
                            instance.phase,
                        ],
                        scale: [instance.scale[0], instance.scale[1], instance.scale[2], 0.0],
                    };
                    match level {
                        LodLevel::Mesh(n) => batcher.push(
                            &gpu_type.meshes[usize::from(n).min(gpu_type.meshes.len() - 1)],
                            ((set_index << 20) | (t << 1)) as u64,
                            data,
                        ),
                        LodLevel::Impostor if gpu_type.billboard_bind.is_some() => {
                            batcher.push(&quad, ((set_index << 20) | (t << 1) | 1) as u64, data)
                        }
                        _ => continue,
                    }
                    frame.instances += 1;
                }
            }
        }
        Ok(frame)
    }

    pub(crate) fn shadow_index(&self, index: u32) -> Result<(wgpu::BindGroup, u32)> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.foliage is not prepared")?;
        Ok((
            prepared.shadow_indices.clone(),
            index * prepared.shadow_index_stride,
        ))
    }

    /// Draws the batches `collect` produced.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view_group: &wgpu::BindGroup,
        batcher: &InstanceBatcher<FoliageInstance>,
        shadow: bool,
    ) -> Result<DrawStats> {
        let prepared = self
            .prepared
            .as_ref()
            .context("world.foliage is not prepared")?;
        pass.set_bind_group(0, view_group, &[]);
        let stats = batcher.draw(pass, Some(1), |pass, key| {
            let set_index = (key >> 20) as usize;
            let t = ((key >> 1) & 0x7_FFFF) as usize;
            let billboard = key & 1 == 1;
            let Some(set) = self.order.get(set_index).and_then(|id| self.sets.get(id)) else {
                return;
            };
            let Some(gpu_type) = set.types.get(t) else {
                return;
            };
            let bind = if billboard {
                gpu_type.billboard_bind.as_ref()
            } else {
                Some(&gpu_type.mesh_bind)
            };
            let Some(bind) = bind else { return };
            pass.set_pipeline(match (shadow, billboard) {
                (true, true) => &prepared.shadow_billboard,
                (true, false) => &prepared.shadow_mesh,
                (false, true) => &prepared.billboard,
                (false, false) => &prepared.mesh,
            });
            pass.set_bind_group(1, bind, &[]);
            pass.set_bind_group(2, &set.wind_bind, &[]);
        });
        Ok(stats)
    }
}
