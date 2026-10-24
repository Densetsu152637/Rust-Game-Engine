//! The built-in mesh path: material programs, instanced opaque/transparent
//! draws, the shadow, motion and normal passes of engine meshes.

use crate::context::PipelineLayouts;
use crate::instancing::{DrawStats, Grouping, group_keys};
use crate::resources::{ENGINE_VERTEX_LAYOUT, GpuMesh, GpuTexture};
use crate::scoped;
use crate::shader::{
    LIT_WGSL, MOTION_WGSL, MaterialTarget, NORMALS_WGSL, SHADOW_WGSL, ShaderComposer,
    compose_material,
};
use crate::targets::{DEPTH_FORMAT, HDR_FORMAT, MOTION_FORMAT, NORMALS_FORMAT};
use anyhow::{Context, Result, ensure};
use engine_render_wgpu::GpuContext;
use glam::Mat4;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Most meshes one view draws per frame.
pub const MAX_SCENE_MESHES: usize = 1 << 20;

/// Material values of the built-in contract (`EngineMaterial` + `maps`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialParams {
    pub tint: [f32; 4],
    pub unlit: bool,
    pub metallic: f32,
    pub roughness: f32,
    pub normal_scale: f32,
    pub receive_shadows: bool,
    pub cast_shadows: bool,
    pub transparent: bool,
}

impl Default for MaterialParams {
    fn default() -> Self {
        Self {
            tint: [1.0; 4],
            unlit: false,
            metallic: 0.0,
            roughness: 0.5,
            normal_scale: 1.0,
            receive_shadows: true,
            cast_shadows: true,
            transparent: false,
        }
    }
}

/// One mesh draw of the built-in path.
#[derive(Clone, Debug)]
pub struct SceneMesh<'a> {
    /// Stable unique id: orders draws and keys motion history.
    pub id: &'a str,
    pub mesh: Arc<GpuMesh>,
    pub world_from_local: Mat4,
    /// Custom material WGSL (the engine material contract); `None` selects
    /// [`LIT_WGSL`].
    pub shader: Option<&'a str>,
    pub params: MaterialParams,
    pub base_color: Option<Arc<GpuTexture>>,
    pub metallic_roughness: Option<Arc<GpuTexture>>,
    pub normal_map: Option<Arc<GpuTexture>>,
}

/// `EngineInstance` (208 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuInstance {
    pub world_from_local: [[f32; 4]; 4],
    pub normal_from_local: [[f32; 4]; 4],
    pub previous_world_from_local: [[f32; 4]; 4],
    pub params: [u32; 4],
}

/// Stride of one material's uniforms in the per-frame arena: `EngineMaterial`
/// at 0, `maps` at 256 (both dynamic offsets).
const MATERIAL_STRIDE: u64 = 512;

struct Program {
    id: u64,
    opaque: wgpu::RenderPipeline,
    transparent: wgpu::RenderPipeline,
    used: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TextureKey([u64; 3]);

struct Batch {
    mesh: Arc<GpuMesh>,
    program: u64,
    material: u32,
    textures: TextureKey,
    instances: std::ops::Range<u32>,
    casts: bool,
    /// Index of the batch's first mesh in the frame's mesh slice.
    source: usize,
}

/// Per-view GPU state of the mesh path.
#[derive(Default)]
pub(crate) struct MeshViewState {
    instances: Option<(wgpu::Buffer, u64, wgpu::BindGroup)>,
    params: Option<(wgpu::Buffer, u64, wgpu::BindGroup)>,
    texture_groups: HashMap<TextureKey, wgpu::BindGroup>,
    previous_models: HashMap<String, Mat4>,
    opaque: Vec<Batch>,
    transparent: Vec<Batch>,
    pub draws: usize,
}

/// Shared programs and pipelines of the mesh path.
pub(crate) struct MeshRenderer {
    pipeline_layout: wgpu::PipelineLayout,
    programs: HashMap<String, Program>,
    next_program: u64,
    frame: u64,
    shadow: wgpu::RenderPipeline,
    shadow_layout: wgpu::BindGroupLayout,
    shadow_index: (wgpu::Buffer, wgpu::BindGroup),
    motion: wgpu::RenderPipeline,
    normals: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    white: Arc<GpuTexture>,
}

fn module(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn depth_state(write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: Default::default(),
        bias: Default::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn mesh_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    target: Option<(wgpu::TextureFormat, Option<wgpu::BlendState>)>,
    depth_write: bool,
) -> wgpu::RenderPipeline {
    let targets = target.map(|(format, blend)| wgpu::ColorTargetState {
        format,
        blend,
        write_mask: wgpu::ColorWrites::ALL,
    });
    let targets = [targets];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(ENGINE_VERTEX_LAYOUT)],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(depth_state(depth_write)),
        multisample: Default::default(),
        fragment: target.is_some().then(|| wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl MeshRenderer {
    pub fn new(gpu: &GpuContext, layouts: &PipelineLayouts) -> Result<Self> {
        let device = &gpu.device;
        let composer = ShaderComposer::new();
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine material layout"),
            bind_group_layouts: &[
                Some(&layouts.view),
                Some(&layouts.material_textures),
                Some(&layouts.material_params),
                Some(&layouts.instances),
            ],
            immediate_size: 0,
        });
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine shadow pass"),
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
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(16);
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine shadow indices"),
            size: alignment * crate::lights::MAX_SHADOWS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut indices = vec![0u8; (alignment * crate::lights::MAX_SHADOWS as u64) as usize];
        for i in 0..crate::lights::MAX_SHADOWS {
            let at = i * alignment as usize;
            indices[at..at + 4].copy_from_slice(&(i as u32).to_ne_bytes());
        }
        gpu.queue.write_buffer(&index_buffer, 0, &indices);
        let index_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine shadow indices"),
            layout: &shadow_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &index_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(16),
                }),
            }],
        });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("engine shadow layout"),
                bind_group_layouts: &[
                    Some(&layouts.view),
                    Some(&shadow_layout),
                    None,
                    Some(&layouts.instances),
                ],
                immediate_size: 0,
            });
        let prepass_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine prepass layout"),
            bind_group_layouts: &[Some(&layouts.view), None, None, Some(&layouts.instances)],
            immediate_size: 0,
        });
        let (shadow, motion, normals) = scoped(device, || {
            let shadow_module = module(device, "engine shadow", &composer.compose(SHADOW_WGSL)?);
            let motion_module = module(device, "engine motion", &composer.compose(MOTION_WGSL)?);
            let normals_module = module(device, "engine normals", &composer.compose(NORMALS_WGSL)?);
            Ok((
                mesh_pipeline(
                    device,
                    "engine shadow",
                    &shadow_pipeline_layout,
                    &shadow_module,
                    None,
                    true,
                ),
                mesh_pipeline(
                    device,
                    "engine motion",
                    &prepass_layout,
                    &motion_module,
                    Some((MOTION_FORMAT, None)),
                    true,
                ),
                mesh_pipeline(
                    device,
                    "engine normals",
                    &prepass_layout,
                    &normals_module,
                    Some((NORMALS_FORMAT, None)),
                    true,
                ),
            ))
        })?;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("engine nearest clamp sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let white = GpuTexture::rgba8(device, &gpu.queue, "engine white", [1, 1], &[255; 4])?;
        Ok(Self {
            pipeline_layout,
            programs: HashMap::new(),
            next_program: 1,
            frame: 0,
            shadow,
            shadow_layout,
            shadow_index: (index_buffer, index_group),
            motion,
            normals,
            sampler,
            white,
        })
    }

    /// Compile (or reuse) the program of a material source. Invalid shaders
    /// fail here, before anything is submitted.
    fn program(&mut self, gpu: &GpuContext, source: Option<&str>) -> Result<u64> {
        let key = source.unwrap_or(LIT_WGSL);
        let frame = self.frame;
        if let Some(program) = self.programs.get_mut(key) {
            program.used = frame;
            return Ok(program.id);
        }
        let composed = compose_material(key, MaterialTarget::Mesh)?;
        let device = &gpu.device;
        let layout = &self.pipeline_layout;
        let (opaque, transparent) = scoped(device, || {
            let module = module(device, "engine material", &composed);
            Ok((
                mesh_pipeline(
                    device,
                    "engine opaque material",
                    layout,
                    &module,
                    Some((HDR_FORMAT, None)),
                    true,
                ),
                mesh_pipeline(
                    device,
                    "engine transparent material",
                    layout,
                    &module,
                    Some((HDR_FORMAT, Some(wgpu::BlendState::ALPHA_BLENDING))),
                    false,
                ),
            ))
        })
        .context("creating the material pipeline")?;
        let id = self.next_program;
        self.next_program += 1;
        self.programs.insert(
            key.to_owned(),
            Program {
                id,
                opaque,
                transparent,
                used: frame,
            },
        );
        Ok(id)
    }

    fn pipeline(&self, program: u64, transparent: bool) -> Option<&wgpu::RenderPipeline> {
        self.programs.values().find(|p| p.id == program).map(|p| {
            if transparent {
                &p.transparent
            } else {
                &p.opaque
            }
        })
    }

    /// Validate every mesh, compile programs, sort, instance and upload one
    /// view's meshes. Nothing is encoded; errors leave `state` drawable from
    /// the previous frame's plan only if it is not used.
    pub fn prepare(
        &mut self,
        gpu: &GpuContext,
        layouts: &PipelineLayouts,
        state: &mut MeshViewState,
        meshes: &[SceneMesh<'_>],
        camera: glam::Vec3,
        reset_history: bool,
    ) -> Result<()> {
        ensure!(
            meshes.len() <= MAX_SCENE_MESHES,
            "at most {MAX_SCENE_MESHES} meshes per view"
        );
        self.frame += 1;
        let mut ids = HashSet::with_capacity(meshes.len());
        let mut programs = Vec::with_capacity(meshes.len());
        for mesh in meshes {
            ensure!(
                !mesh.id.is_empty() && ids.insert(mesh.id),
                "empty or duplicate mesh draw id: {}",
                mesh.id
            );
            ensure!(
                mesh.world_from_local.is_finite()
                    && mesh.world_from_local.determinant().abs() > 1e-8,
                "nonfinite model transform: {}",
                mesh.id
            );
            validate_params(&mesh.params).with_context(|| format!("mesh {}", mesh.id))?;
            programs.push(
                self.program(gpu, mesh.shader)
                    .with_context(|| format!("material of mesh {}", mesh.id))?,
            );
        }
        // Programs no draw referenced this frame are released.
        let frame = self.frame;
        self.programs.retain(|_, p| p.used == frame);

        // Draw order: opaque by id, then transparent back to front.
        let mut order: Vec<usize> = (0..meshes.len()).collect();
        let distance = |i: usize| {
            meshes[i]
                .world_from_local
                .w_axis
                .truncate()
                .distance_squared(camera)
        };
        order.sort_by(|&a, &b| {
            let (ma, mb) = (&meshes[a], &meshes[b]);
            ma.params
                .transparent
                .cmp(&mb.params.transparent)
                .then_with(|| {
                    if ma.params.transparent {
                        distance(b).total_cmp(&distance(a))
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| ma.id.cmp(mb.id))
        });

        // Distinct materials (program, textures, values) in first-use order.
        let mut material_index: HashMap<(u64, TextureKey, [u32; 12]), u32> = HashMap::new();
        let mut material_bytes: Vec<u8> = Vec::new();
        let mut keys = Vec::with_capacity(meshes.len());
        for &i in &order {
            let mesh = &meshes[i];
            let textures = TextureKey([
                mesh.base_color.as_ref().map_or(0, |t| t.id()),
                mesh.metallic_roughness.as_ref().map_or(0, |t| t.id()),
                mesh.normal_map.as_ref().map_or(0, |t| t.id()),
            ]);
            let (material, maps) = material_values(mesh);
            let mut bits = [0u32; 12];
            for (slot, value) in bits.iter_mut().zip(material.iter().chain(&maps)) {
                *slot = value.to_bits();
            }
            let next = material_index.len() as u32;
            let index = *material_index
                .entry((programs[i], textures, bits))
                .or_insert_with(|| {
                    let start = material_bytes.len();
                    material_bytes.resize(start + MATERIAL_STRIDE as usize, 0);
                    material_bytes[start..start + 32]
                        .copy_from_slice(bytemuck::cast_slice(&material));
                    material_bytes[start + 256..start + 272]
                        .copy_from_slice(bytemuck::cast_slice(&maps));
                    next
                });
            keys.push((
                mesh.mesh.id(),
                u64::from(index),
                mesh.params.transparent,
                textures,
            ));
        }

        // Instances: opaque groups by (mesh, material); transparent keeps
        // its sorted order and merges only neighbours.
        let split = order
            .iter()
            .position(|&i| meshes[i].params.transparent)
            .unwrap_or(order.len());
        let mut instances: Vec<GpuInstance> = Vec::with_capacity(meshes.len());
        let mut current_models = HashMap::with_capacity(meshes.len());
        state.opaque.clear();
        state.transparent.clear();
        for (range, grouping, transparent) in [
            (0..split, Grouping::ByKey, false),
            (split..order.len(), Grouping::Consecutive, true),
        ] {
            let group_input: Vec<(u64, u64)> =
                range.clone().map(|k| (keys[k].0, keys[k].1)).collect();
            for (members, _) in group_keys(&group_input, grouping) {
                let first = members[0] + range.start;
                let start = instances.len() as u32;
                for &member in &members {
                    let mesh = &meshes[order[member + range.start]];
                    let model = mesh.world_from_local;
                    let previous = if reset_history {
                        model
                    } else {
                        state.previous_models.get(mesh.id).copied().unwrap_or(model)
                    };
                    current_models.insert(mesh.id.to_owned(), model);
                    instances.push(GpuInstance {
                        world_from_local: model.to_cols_array_2d(),
                        normal_from_local: model.inverse().transpose().to_cols_array_2d(),
                        previous_world_from_local: previous.to_cols_array_2d(),
                        params: [id_hash(mesh.id), 0, 0, 0],
                    });
                }
                let mesh = &meshes[order[first]];
                let batch = Batch {
                    mesh: mesh.mesh.clone(),
                    program: programs[order[first]],
                    material: keys[first].1 as u32,
                    textures: keys[first].3,
                    instances: start..instances.len() as u32,
                    casts: mesh.params.cast_shadows && !transparent,
                    source: order[first],
                };
                if transparent {
                    state.transparent.push(batch);
                } else {
                    state.opaque.push(batch);
                }
            }
        }
        state.previous_models = current_models;
        state.draws = meshes.len();

        // Uploads.
        let device = &gpu.device;
        if instances.is_empty() {
            instances.push(GpuInstance::zeroed_instance());
        }
        let bytes: &[u8] = bytemuck::cast_slice(&instances);
        if state
            .instances
            .as_ref()
            .is_none_or(|(_, cap, _)| *cap < bytes.len() as u64)
        {
            let capacity = (bytes.len() as u64).next_power_of_two();
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("engine instances"),
                size: capacity,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine instances"),
                layout: &layouts.instances,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            state.instances = Some((buffer, capacity, group));
        }
        if let Some((buffer, _, _)) = &state.instances {
            gpu.queue.write_buffer(buffer, 0, bytes);
        }
        if material_bytes.is_empty() {
            material_bytes.resize(MATERIAL_STRIDE as usize, 0);
        }
        if state
            .params
            .as_ref()
            .is_none_or(|(_, cap, _)| *cap < material_bytes.len() as u64)
        {
            let capacity = (material_bytes.len() as u64).next_power_of_two();
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("engine material uniforms"),
                size: capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let binding = |size| {
                wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(size),
                })
            };
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine material uniforms"),
                layout: &layouts.material_params,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: binding(32),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: binding(16),
                    },
                ],
            });
            state.params = Some((buffer, capacity, group));
        }
        if let Some((buffer, _, _)) = &state.params {
            gpu.queue.write_buffer(buffer, 0, &material_bytes);
        }
        // Texture bind groups referenced this frame; others are released.
        let mut live = HashSet::new();
        for batch in state.opaque.iter().chain(&state.transparent) {
            live.insert(batch.textures);
            if state.texture_groups.contains_key(&batch.textures) {
                continue;
            }
            let mesh = &meshes[batch.source];
            let white = &self.white;
            let base = mesh.base_color.as_ref().unwrap_or(white);
            let mr = mesh.metallic_roughness.as_ref().unwrap_or(white);
            let normal = mesh.normal_map.as_ref().unwrap_or(white);
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine material textures"),
                layout: &layouts.material_textures,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&base.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&mr.linear_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&normal.linear_view),
                    },
                ],
            });
            state.texture_groups.insert(batch.textures, group);
        }
        state.texture_groups.retain(|key, _| live.contains(key));
        Ok(())
    }

    fn draw_batches(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view: &wgpu::BindGroup,
        state: &MeshViewState,
        batches: &[Batch],
        transparent: bool,
    ) -> DrawStats {
        let mut stats = DrawStats::default();
        let (Some((_, _, instances)), Some((_, _, params))) = (&state.instances, &state.params)
        else {
            return stats;
        };
        pass.set_bind_group(0, view, &[]);
        pass.set_bind_group(3, instances, &[]);
        let mut current = None;
        for batch in batches {
            let Some(pipeline) = self.pipeline(batch.program, transparent) else {
                continue;
            };
            if current != Some(batch.program) {
                pass.set_pipeline(pipeline);
                current = Some(batch.program);
            }
            let offset = u64::from(batch.material) * MATERIAL_STRIDE;
            pass.set_bind_group(1, &state.texture_groups[&batch.textures], &[]);
            pass.set_bind_group(2, params, &[offset as u32, (offset + 256) as u32]);
            batch.mesh.draw(pass, batch.instances.clone());
            stats.draw_calls += 1;
            stats.instances += batch.instances.len();
        }
        stats
    }

    pub fn draw_opaque(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view: &wgpu::BindGroup,
        state: &MeshViewState,
    ) -> DrawStats {
        self.draw_batches(pass, view, state, &state.opaque, false)
    }

    pub fn draw_transparent(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view: &wgpu::BindGroup,
        state: &MeshViewState,
    ) -> DrawStats {
        self.draw_batches(pass, view, state, &state.transparent, true)
    }

    /// Depth-only casters into atlas layer `shadow` (the shadow list index).
    pub fn draw_shadow(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view: &wgpu::BindGroup,
        state: &MeshViewState,
        shadow: u32,
        alignment: u32,
    ) -> DrawStats {
        let mut stats = DrawStats::default();
        let Some((_, _, instances)) = &state.instances else {
            return stats;
        };
        pass.set_pipeline(&self.shadow);
        pass.set_bind_group(0, view, &[]);
        pass.set_bind_group(1, &self.shadow_index.1, &[shadow * alignment]);
        pass.set_bind_group(3, instances, &[]);
        for batch in state.opaque.iter().filter(|b| b.casts) {
            batch.mesh.draw(pass, batch.instances.clone());
            stats.draw_calls += 1;
            stats.instances += batch.instances.len();
        }
        stats
    }

    /// Opaque meshes with an engine prepass pipeline (motion or normals).
    pub fn draw_prepass(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        view: &wgpu::BindGroup,
        state: &MeshViewState,
        normals: bool,
    ) -> DrawStats {
        let mut stats = DrawStats::default();
        let Some((_, _, instances)) = &state.instances else {
            return stats;
        };
        pass.set_pipeline(if normals { &self.normals } else { &self.motion });
        pass.set_bind_group(0, view, &[]);
        pass.set_bind_group(3, instances, &[]);
        for batch in &state.opaque {
            batch.mesh.draw(pass, batch.instances.clone());
            stats.draw_calls += 1;
            stats.instances += batch.instances.len();
        }
        stats
    }

    #[allow(dead_code)]
    pub fn shadow_layout(&self) -> &wgpu::BindGroupLayout {
        &self.shadow_layout
    }
}

impl GpuInstance {
    fn zeroed_instance() -> Self {
        bytemuck::Zeroable::zeroed()
    }
}

/// `EngineMaterial` (tint, options) and `maps` of a mesh.
fn material_values(mesh: &SceneMesh<'_>) -> ([f32; 8], [f32; 4]) {
    let p = &mesh.params;
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    (
        [
            p.tint[0],
            p.tint[1],
            p.tint[2],
            p.tint[3],
            flag(p.unlit),
            p.metallic,
            p.roughness,
            flag(p.receive_shadows),
        ],
        [
            flag(mesh.normal_map.is_some()),
            p.normal_scale,
            flag(mesh.metallic_roughness.is_some()),
            0.0,
        ],
    )
}

/// Range checks of the built-in material values.
pub fn validate_params(params: &MaterialParams) -> Result<()> {
    ensure!(
        params
            .tint
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && (0.0..=1.0).contains(&params.metallic)
            && (0.0..=1.0).contains(&params.roughness)
            && params.normal_scale.is_finite()
            && (0.0..=10.0).contains(&params.normal_scale),
        "material values must be finite: tint, metallic and roughness in 0..1, normal scale in 0..10"
    );
    Ok(())
}

/// FNV-1a of a draw id (the `EngineInstance.params.x` picking id).
pub fn id_hash(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}
