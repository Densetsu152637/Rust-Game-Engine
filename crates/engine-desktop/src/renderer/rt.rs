//! The hardware ray-traced lighting tier in the frame (docs/rendering-raytracing.md,
//! "Frame integration").
//!
//! Four Prepass-stage features, `engine.rt.shadows`, `engine.rt.ao`,
//! `engine.rt.reflections` and `engine.rt.gi`, run after the normal and
//! motion prepasses. The first one of a frame reconstructs the RT G-buffer
//! (world positions, `Rgba32Float`, from the prepass depth; normals are the
//! C1 normal target). Each then traces its `engine-render-rt` kernel, denoises
//! the signal with its own [`Denoiser`] (C1 motion vectors, camera cuts from
//! `ViewData.reset_history`) and writes the pipeline's screen-lighting input,
//! which `engine::lighting` applies in the Opaque stage. Without a
//! hardware tier (or on any kernel error) the effect runs its raster
//! fallback instead and reports why: shadows keep the shadow maps, AO runs a
//! screen-space hemisphere SSAO, reflections add nothing and GI keeps the
//! constant ambient.
//!
//! [`RtHost`] owns the shared state: the [`RtDevice`] (created once, when a
//! frame first wants an RT feature, on the renderer's own device), the
//! [`RtScene`] synced incrementally from the frame's meshes, the material
//! table, per-view textures and denoisers, and the per-effect outcome that
//! becomes the [`RayTracingReport`].
use anyhow::{Context, Result};
use engine_project::content::Mesh;
use engine_project::raytracing::{RayTracing, RayTracingReport, RtEffectReport};
use engine_project::rendering3d::{RenderPipelineState, SceneRenderFrame};
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureNeeds, FeatureRegistry, FeatureSetup, GpuContext,
    LightList, LightShape, PipelineFeature, ScenePipeline, Stage, ViewData, ViewId, ViewReport,
    names,
};
use engine_render_rt::{
    AmbientOcclusionKernel, AoInputs, Denoiser, DenoiserInputs, DenoiserSignal, DiffuseGiKernel,
    GiInputs, InstanceId, MeshId, RayTracingStatus, RayTracingTier, ReflectionInputs,
    ReflectionKernel, RtCommitStats, RtDevice, RtEffect, RtEffectMode, RtEffectStatus,
    RtEffectsSettings, RtEffectsStatus, RtFeatureRequest, RtGBuffer, RtIndices, RtInstanceDesc,
    RtLighting, RtMaterial, RtMaterialTable, RtMeshData, RtScalarFormat, RtScene, RtSceneLimits,
    RtUnavailableCause, ShadowMaskKernel, ShadowMaskParams, ShadowMaskTargets,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use wgpu::util::DeviceExt;

const SHADER: &str = include_str!("rt.wgsl");

/// Feature names of the RT lighting tier (Prepass stage, disabled by
/// default; an enabled `RayTracing` effect turns its feature on).
pub mod rt_names {
    pub const SHADOWS: &str = "engine.rt.shadows";
    pub const AO: &str = "engine.rt.ao";
    pub const REFLECTIONS: &str = "engine.rt.reflections";
    pub const GI: &str = "engine.rt.gi";
    pub const ALL: [&str; 4] = [SHADOWS, AO, REFLECTIONS, GI];
}

/// Instance mask bit of shadow casters (`MeshMaterial.cast_shadows`); every
/// opaque instance also carries bit 1.
const CASTER_MASK: u8 = 0x01;
const VISIBLE_MASK: u8 = 0x02;
/// SSAO samples per pixel of the raster fallback.
const SSAO_SAMPLES: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Shadows = 0,
    Ao = 1,
    Reflections = 2,
    Gi = 3,
}

impl Effect {
    const ALL: [Effect; 4] = [Effect::Shadows, Effect::Ao, Effect::Reflections, Effect::Gi];

    fn feature(self) -> &'static str {
        rt_names::ALL[self as usize]
    }

    fn rt(self) -> RtEffect {
        match self {
            Effect::Shadows => RtEffect::Shadows,
            Effect::Ao => RtEffect::AmbientOcclusion,
            Effect::Reflections => RtEffect::Reflections,
            Effect::Gi => RtEffect::DiffuseGi,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Effect::Shadows => "shadows",
            Effect::Ao => "ambient_occlusion",
            Effect::Reflections => "reflections",
            Effect::Gi => "diffuse_gi",
        }
    }

    /// What runs when the effect cannot be ray traced.
    fn fallback(self) -> &'static str {
        match self {
            Effect::Shadows => "shadow_maps",
            Effect::Ao => "ssao",
            Effect::Reflections => "none",
            Effect::Gi => "constant_ambient",
        }
    }

    fn enabled(self, settings: &RtEffectsSettings) -> bool {
        match self {
            Effect::Shadows => settings.shadows.enabled,
            Effect::Ao => settings.ambient_occlusion.enabled,
            Effect::Reflections => settings.reflections.enabled,
            Effect::Gi => settings.diffuse_gi.enabled,
        }
    }

    fn set_enabled(self, settings: &mut RtEffectsSettings, on: bool) {
        match self {
            Effect::Shadows => settings.shadows.enabled = on,
            Effect::Ao => settings.ambient_occlusion.enabled = on,
            Effect::Reflections => settings.reflections.enabled = on,
            Effect::Gi => settings.diffuse_gi.enabled = on,
        }
    }
}

/// `RayTracing` (authored) as the RT crate's settings: the same fields.
pub fn effects_settings(authored: &RayTracing) -> RtEffectsSettings {
    let mut s = RtEffectsSettings::default();
    let a = authored;
    s.shadows.enabled = a.shadows.enabled;
    s.shadows.sun_angular_radius = a.shadows.sun_angular_radius;
    s.shadows.samples_per_pixel = a.shadows.samples_per_pixel;
    s.shadows.normal_bias = a.shadows.normal_bias;
    s.shadows.max_distance = a.shadows.max_distance;
    s.shadows.denoise = a.shadows.denoise;
    s.ambient_occlusion.enabled = a.ambient_occlusion.enabled;
    s.ambient_occlusion.radius = a.ambient_occlusion.radius;
    s.ambient_occlusion.rays_per_pixel = a.ambient_occlusion.rays_per_pixel;
    s.ambient_occlusion.normal_bias = a.ambient_occlusion.normal_bias;
    s.ambient_occlusion.denoise = a.ambient_occlusion.denoise;
    s.reflections.enabled = a.reflections.enabled;
    s.reflections.roughness_cutoff = a.reflections.roughness_cutoff;
    s.reflections.max_distance = a.reflections.max_distance;
    s.reflections.normal_bias = a.reflections.normal_bias;
    s.reflections.denoise = a.reflections.denoise;
    s.diffuse_gi.enabled = a.diffuse_gi.enabled;
    s.diffuse_gi.bounces = a.diffuse_gi.bounces;
    s.diffuse_gi.rays_per_pixel = a.diffuse_gi.rays_per_pixel;
    s.diffuse_gi.max_distance = a.diffuse_gi.max_distance;
    s.diffuse_gi.normal_bias = a.diffuse_gi.normal_bias;
    s.diffuse_gi.denoise = a.diffuse_gi.denoise;
    let d = &a.denoiser;
    s.denoiser.atrous_iterations = d.atrous_iterations;
    s.denoiser.max_history_length = d.max_history_length;
    s.denoiser.reprojection_depth_tolerance = d.reprojection_depth_tolerance;
    s.denoiser.reprojection_normal_threshold = d.reprojection_normal_threshold;
    s.denoiser.phi_depth = d.phi_depth;
    s.denoiser.phi_normal = d.phi_normal;
    s.denoiser.phi_luminance = d.phi_luminance;
    s.denoiser.min_history_for_temporal_variance = d.min_history_for_temporal_variance;
    s
}

/// Add the `engine-render-rt` request to `options` for `adapter` (the device
/// hook of docs/rendering-architecture.md). `Err` says why the adapter
/// cannot run the hardware tier; the device is then created without it and
/// the effects report that reason when they fall back.
pub fn request_ray_tracing(
    options: &mut engine_render_wgpu::WgpuOptions,
    adapter: &wgpu::Adapter,
) -> std::result::Result<(), RayTracingStatus> {
    let request = RtFeatureRequest::for_adapter(adapter)?;
    let mut limits = wgpu::Limits::default();
    request.merge_limits(&mut limits);
    options
        .requirements
        .request(
            engine_render_pipeline::DeviceFeatureRequest::new("engine-render-rt")
                .require(request.features)
                .with_limits(limits)
                .with_experimental(request.experimental),
        )
        .map_err(|reason| unavailable(RtUnavailableCause::DeviceRequestFailed, reason))
}

/// When `frame`'s camera enables a `RayTracing` effect, add the RT request
/// for the adapter `instance` would pick (for `surface`) to `options` before
/// the device is created. `None` when nothing requests ray tracing; pass the
/// result to [`crate::GpuRenderer::note_ray_tracing_request`].
pub async fn prepare_ray_tracing(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    options: &mut engine_render_wgpu::WgpuOptions,
    frame: &SceneRenderFrame,
) -> Option<std::result::Result<(), RayTracingStatus>> {
    let camera_wants = frame
        .camera3d
        .as_ref()
        .and(frame.ray_tracing.as_ref())
        .is_some_and(RayTracing::any_enabled);
    if !camera_wants {
        return None;
    }
    let adapter = match GpuContext::request_adapter(instance, surface, options).await {
        Ok(adapter) => adapter,
        Err(error) => {
            return Some(Err(unavailable(
                RtUnavailableCause::NoAdapter,
                format!("{error:#}"),
            )));
        }
    };
    Some(request_ray_tracing(options, &adapter))
}

fn unavailable(cause: RtUnavailableCause, reason: impl Into<String>) -> RayTracingStatus {
    RayTracingStatus {
        tier: RayTracingTier::Unavailable,
        cause: Some(cause),
        reason: Some(engine_render_rt::bounded_reason(&reason.into())),
        adapter: None,
    }
}

struct Tex {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

fn texture(device: &wgpu::Device, label: &str, size: [u32; 2], format: wgpu::TextureFormat) -> Tex {
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
        format,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    Tex {
        _texture: texture,
        view,
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    view_from_world: [[f32; 4]; 4],
    size: [u32; 4],
    values: [f32; 4],
}

/// The non-ray-query compute passes (created on any device).
struct Passes {
    gbuffer: (wgpu::ComputePipeline, wgpu::BindGroupLayout),
    ssao: (wgpu::ComputePipeline, wgpu::BindGroupLayout),
    blit: (wgpu::ComputePipeline, wgpu::BindGroupLayout),
}

impl Passes {
    fn new(device: &wgpu::Device) -> Result<Self> {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("engine rt integration"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let tex = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let storage = |binding, format| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let pipeline = |label: &str, entry: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
            (pipeline, layout)
        };
        Ok(Self {
            gbuffer: pipeline(
                "engine rt gbuffer",
                "gbuffer",
                &[
                    tex(0, wgpu::TextureSampleType::Depth),
                    tex(1, float),
                    storage(2, wgpu::TextureFormat::Rgba32Float),
                    uniform(3),
                ],
            ),
            ssao: pipeline(
                "engine rt ssao fallback",
                "ssao",
                &[
                    tex(4, float),
                    tex(5, float),
                    storage(6, wgpu::TextureFormat::Rgba16Float),
                    uniform(7),
                ],
            ),
            blit: pipeline(
                "engine rt blit",
                "blit",
                &[
                    tex(8, float),
                    storage(9, wgpu::TextureFormat::Rgba16Float),
                    uniform(10),
                ],
            ),
        })
    }
}

fn dispatch(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pass: &(wgpu::ComputePipeline, wgpu::BindGroupLayout),
    views: &[(u32, &wgpu::TextureView)],
    params: &Params,
    size: [u32; 2],
) {
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("engine rt pass params"),
        contents: bytemuck::bytes_of(params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let uniform_binding = views.iter().map(|(b, _)| *b).max().unwrap_or(0) + 1;
    let mut entries: Vec<wgpu::BindGroupEntry<'_>> = views
        .iter()
        .map(|(binding, view)| wgpu::BindGroupEntry {
            binding: *binding,
            resource: wgpu::BindingResource::TextureView(view),
        })
        .collect();
    entries.push(wgpu::BindGroupEntry {
        binding: uniform_binding,
        resource: buffer.as_entire_binding(),
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("engine rt pass"),
        layout: &pass.1,
        entries: &entries,
    });
    let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("engine rt pass"),
        timestamp_writes: None,
    });
    compute.set_pipeline(&pass.0);
    compute.set_bind_group(0, &group, &[]);
    compute.dispatch_workgroups(size[0].div_ceil(8), size[1].div_ceil(8), 1);
}

struct Kernels {
    shadow: ShadowMaskKernel,
    ao: AmbientOcclusionKernel,
    reflections: ReflectionKernel,
    gi: DiffuseGiKernel,
}

/// Per-view RT textures and denoisers (render size).
struct ViewTargets {
    size: [u32; 2],
    position: Tex,
    /// Constant zero roughness: reflections trace the mirror direction and
    /// the lit shader applies the material roughness (cutoff and fade).
    roughness: Tex,
    /// Kernel outputs: shadow and AO `R32Float`, reflection and GI `Rgba16Float`.
    outputs: [Tex; 4],
    denoisers: [Option<Denoiser>; 4],
    gbuffer_serial: Option<u64>,
}

impl ViewTargets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let scalar = wgpu::TextureFormat::R32Float;
        let color = wgpu::TextureFormat::Rgba16Float;
        Self {
            size,
            position: texture(
                device,
                "engine rt world positions",
                size,
                wgpu::TextureFormat::Rgba32Float,
            ),
            roughness: texture(device, "engine rt roughness", size, scalar),
            outputs: [
                texture(device, "engine rt shadow mask", size, scalar),
                texture(device, "engine rt ambient occlusion", size, scalar),
                texture(device, "engine rt reflections", size, color),
                texture(device, "engine rt diffuse gi", size, color),
            ],
            denoisers: [None, None, None, None],
            gbuffer_serial: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Outcome {
    ran: bool,
    traced: bool,
    denoised: bool,
    reason: Option<String>,
}

#[derive(Clone)]
struct FrameInputs {
    settings: RtEffectsSettings,
    lighting: RtLighting,
    sun: Option<[f32; 3]>,
    serial: u64,
}

struct Shared {
    passes: Option<Passes>,
    rt: Option<Arc<RtDevice>>,
    status: RayTracingStatus,
    kernels: Option<std::result::Result<Kernels, String>>,
    scene: Option<RtScene>,
    meshes: HashMap<usize, (Arc<Mesh>, MeshId)>,
    instances: Vec<InstanceId>,
    material_values: Vec<RtMaterial>,
    materials: Option<RtMaterialTable>,
    sync_error: Option<String>,
    last_commit: RtCommitStats,
    frame: Option<FrameInputs>,
    views: HashMap<ViewId, ViewTargets>,
    outcomes: [Outcome; 4],
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    fn new() -> Self {
        Self {
            passes: None,
            rt: None,
            status: unavailable(
                RtUnavailableCause::DeviceMissingFeature,
                "no frame has requested a ray-traced effect yet",
            ),
            kernels: None,
            scene: None,
            meshes: HashMap::new(),
            instances: Vec::new(),
            material_values: Vec::new(),
            materials: None,
            sync_error: None,
            last_commit: RtCommitStats::default(),
            frame: None,
            views: HashMap::new(),
            outcomes: Default::default(),
        }
    }

    /// Why the hardware path cannot run this frame, if it cannot.
    fn hardware_blocker(&mut self) -> Option<String> {
        let Some(rt) = &self.rt else {
            return Some(self.status.to_string());
        };
        let status = rt.status();
        if !status.is_hardware() {
            self.status = status.clone();
            return Some(status.to_string());
        }
        if let Some(error) = &self.sync_error {
            return Some(format!("RT scene sync failed: {error}"));
        }
        if self.scene.is_none() || self.materials.is_none() {
            return Some("the RT scene is not built".into());
        }
        if self.kernels.is_none() {
            self.kernels = Some(
                (|| -> std::result::Result<Kernels, engine_render_rt::RtError> {
                    Ok(Kernels {
                        shadow: ShadowMaskKernel::new(rt, RtScalarFormat::R32Float)?,
                        ao: AmbientOcclusionKernel::new(rt, RtScalarFormat::R32Float)?,
                        reflections: ReflectionKernel::new(rt)?,
                        gi: DiffuseGiKernel::new(rt)?,
                    })
                })()
                .map_err(|e| e.to_string()),
            );
        }
        match &self.kernels {
            Some(Err(error)) => Some(format!("creating the RT kernels failed: {error}")),
            _ => None,
        }
    }

    fn sync_scene(&mut self, frame: &SceneRenderFrame) {
        let Some(rt) = self.rt.clone() else {
            return;
        };
        if self.scene.is_none() {
            match RtScene::new(&rt, RtSceneLimits::for_device(rt.limits())) {
                Ok(scene) => self.scene = Some(scene),
                Err(error) => {
                    self.sync_error = Some(error.to_string());
                    return;
                }
            }
        }
        self.sync_error = None;
        self.last_commit = RtCommitStats::default();
        let scene = self.scene.as_mut().expect("created above");
        let mut live = HashSet::new();
        let mut descs = Vec::new();
        let mut materials = Vec::new();
        for draw in frame.meshes.iter().filter(|d| {
            !d.material.transparent && !d.mesh.vertices.is_empty() && !d.mesh.indices.is_empty()
        }) {
            let key = Arc::as_ptr(&draw.mesh) as usize;
            let id = match self.meshes.get(&key) {
                Some((_, id)) => *id,
                None => {
                    let positions: Vec<[f32; 3]> =
                        draw.mesh.vertices.iter().map(|v| v.position).collect();
                    let normals = (draw.mesh.normals.len() == positions.len())
                        .then_some(draw.mesh.normals.as_slice());
                    match scene.add_mesh(&RtMeshData {
                        positions: &positions,
                        indices: RtIndices::U32(&draw.mesh.indices),
                        normals,
                    }) {
                        Ok(id) => {
                            self.meshes.insert(key, (draw.mesh.clone(), id));
                            id
                        }
                        Err(error) => {
                            self.sync_error = Some(format!("mesh of draw {}: {error}", draw.id));
                            continue;
                        }
                    }
                }
            };
            live.insert(key);
            let tint = [
                draw.material.tint[0],
                draw.material.tint[1],
                draw.material.tint[2],
            ];
            materials.push(RtMaterial {
                albedo: tint,
                // Unlit materials show their colour regardless of light.
                emissive: if draw.material.unlit { tint } else { [0.0; 3] },
            });
            let m = draw.model;
            descs.push(RtInstanceDesc {
                mesh: id,
                transform: [
                    m[0][0], m[1][0], m[2][0], m[3][0], //
                    m[0][1], m[1][1], m[2][1], m[3][1], //
                    m[0][2], m[1][2], m[2][2], m[3][2],
                ],
                mask: VISIBLE_MASK
                    | if draw.material.cast_shadows {
                        CASTER_MASK
                    } else {
                        0
                    },
                custom_index: 0,
                material_index: (materials.len() - 1) as u32,
            });
        }
        for (index, desc) in descs.iter().enumerate() {
            let result = match self.instances.get(index) {
                Some(&id) if scene.instance(id) == Some(desc) => Ok(()),
                Some(&id) => scene.set_instance(id, desc),
                None => scene.add_instance(desc).map(|id| self.instances.push(id)),
            };
            if let Err(error) = result {
                self.sync_error = Some(format!("instance {index}: {error}"));
            }
        }
        while self.instances.len() > descs.len() {
            let id = self.instances.pop().expect("longer than descs");
            if let Err(error) = scene.remove_instance(id) {
                self.sync_error = Some(error.to_string());
            }
        }
        let dead: Vec<usize> = self
            .meshes
            .keys()
            .filter(|key| !live.contains(key))
            .copied()
            .collect();
        for key in dead {
            if let Some((_, id)) = self.meshes.get(&key)
                && scene.remove_mesh(*id).is_ok()
            {
                self.meshes.remove(&key);
            }
        }
        if materials.is_empty() {
            materials.push(RtMaterial {
                albedo: [0.0; 3],
                emissive: [0.0; 3],
            });
        }
        if self.materials.is_none() || materials != self.material_values {
            match RtMaterialTable::new(&rt, &materials) {
                Ok(table) => {
                    self.materials = Some(table);
                    self.material_values = materials;
                }
                Err(error) => self.sync_error = Some(format!("materials: {error}")),
            }
        }
        if scene.is_dirty() {
            match scene.commit() {
                Ok(stats) => self.last_commit = stats,
                Err(error) => self.sync_error = Some(format!("commit: {error}")),
            }
        }
    }

    fn encode(&mut self, effect: Effect, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let index = effect as usize;
        self.outcomes[index].ran = true;
        let Some(frame) = self.frame.clone() else {
            self.outcomes[index].reason = Some("the host supplied no RT frame inputs".into());
            return Ok(());
        };
        let device = ctx.gpu.device.clone();
        let size = ctx.render_size();
        let view = ctx.view.clone();
        let depth = ctx.depth()?.view.clone();
        let normals = ctx.normals().map(|t| t.view.clone());
        let motion = ctx.motion().map(|t| t.view.clone());
        let screen = ctx
            .screen_lighting()
            .map(|s| s.targets().map(|t| t.view.clone()));
        let (Some(normals), Some(motion), Some(screen)) = (normals, motion, screen) else {
            self.outcomes[index].reason =
                Some("the pipeline produced no normals, motion or screen-lighting targets".into());
            return Ok(());
        };
        let passes = self.passes.as_ref().context("RT passes not prepared")?;
        let targets = self
            .views
            .entry(view.id)
            .or_insert_with(|| ViewTargets::new(&device, size));
        if targets.size != size {
            *targets = ViewTargets::new(&device, size);
        }
        // The RT G-buffer, once per view and frame.
        if targets.gbuffer_serial != Some(frame.serial) {
            targets.gbuffer_serial = Some(frame.serial);
            let params = Params {
                matrix: view.jittered_clip_from_world().inverse().to_cols_array_2d(),
                view_from_world: view.view_from_world().to_cols_array_2d(),
                size: [size[0], size[1], 0, 0],
                values: [0.0; 4],
            };
            dispatch(
                &device,
                ctx.encoder,
                &passes.gbuffer,
                &[(0, &depth), (1, &normals), (2, &targets.position.view)],
                &params,
                size,
            );
        }
        let blocker = self.hardware_blocker();
        let traced = match blocker {
            Some(reason) => Err(reason),
            None => self.trace(
                effect,
                ctx.encoder,
                &frame,
                &view,
                &normals,
                &motion,
                &screen,
            ),
        };
        let outcome = &mut self.outcomes[index];
        match traced {
            Ok(denoised) => {
                outcome.traced = true;
                outcome.denoised = denoised;
            }
            Err(reason) => {
                outcome.reason = Some(format!("{reason}; using the raster fallback"));
                if effect == Effect::Ao {
                    let passes = self.passes.as_ref().expect("checked above");
                    let targets = &self.views[&view.id];
                    let s = &frame.settings.ambient_occlusion;
                    let params = Params {
                        matrix: view.jittered_clip_from_world().to_cols_array_2d(),
                        view_from_world: view.view_from_world().to_cols_array_2d(),
                        size: [size[0], size[1], SSAO_SAMPLES, frame.serial as u32],
                        values: [s.radius, (0.02 * s.radius).max(s.normal_bias), 0.0, 0.0],
                    };
                    dispatch(
                        &device,
                        ctx.encoder,
                        &passes.ssao,
                        &[(4, &targets.position.view), (5, &normals), (6, &screen[1])],
                        &params,
                        size,
                    );
                }
            }
        }
        Ok(())
    }

    /// Trace, denoise and blit `effect`; `Ok(denoised)`.
    #[allow(clippy::too_many_arguments)]
    fn trace(
        &mut self,
        effect: Effect,
        encoder: &mut wgpu::CommandEncoder,
        frame: &FrameInputs,
        view: &ViewData,
        normals: &wgpu::TextureView,
        motion: &wgpu::TextureView,
        screen: &[wgpu::TextureView; 4],
    ) -> std::result::Result<bool, String> {
        let rt = self.rt.clone().expect("checked by hardware_blocker");
        let Some(Ok(kernels)) = &self.kernels else {
            return Err("RT kernels missing".into());
        };
        let scene = self.scene.as_ref().expect("checked by hardware_blocker");
        let materials = self
            .materials
            .as_ref()
            .expect("checked by hardware_blocker");
        let passes = self.passes.as_ref().expect("prepared");
        let targets = self.views.get_mut(&view.id).expect("created by encode");
        let size = targets.size;
        let s = &frame.settings;
        let gbuffer = RtGBuffer {
            world_position: &targets.position.view,
            world_normal: normals,
            width: size[0],
            height: size[1],
        };
        let index = effect as usize;
        let output = &targets.outputs[index].view;
        let frame_index = frame.serial as u32;
        let eye = view.camera_position().to_array();
        let (denoise, signal, mode, cutoff) = match effect {
            Effect::Shadows => {
                let sun = frame
                    .sun
                    .ok_or("the scene has no directional light to trace shadows for")?;
                let mut params = ShadowMaskParams::from_settings(&s.shadows, sun, frame_index);
                params.mask = CASTER_MASK;
                kernels
                    .shadow
                    .record(
                        encoder,
                        scene,
                        &ShadowMaskTargets {
                            world_position: gbuffer.world_position,
                            world_normal: normals,
                            output,
                            width: size[0],
                            height: size[1],
                        },
                        &params,
                    )
                    .map_err(|e| e.to_string())?;
                (
                    s.shadows.is_soft() && s.shadows.denoise,
                    DenoiserSignal::Scalar,
                    0,
                    0.0,
                )
            }
            Effect::Ao => {
                kernels
                    .ao
                    .record(
                        encoder,
                        scene,
                        &AoInputs {
                            gbuffer,
                            output,
                            frame_index,
                        },
                        &s.ambient_occlusion,
                    )
                    .map_err(|e| e.to_string())?;
                (s.ambient_occlusion.denoise, DenoiserSignal::Scalar, 1, 0.0)
            }
            Effect::Reflections => {
                kernels
                    .reflections
                    .record(
                        encoder,
                        scene,
                        &ReflectionInputs {
                            gbuffer,
                            roughness: &targets.roughness.view,
                            output,
                            materials,
                            lighting: &frame.lighting,
                            eye,
                            frame_index,
                        },
                        &s.reflections,
                    )
                    .map_err(|e| e.to_string())?;
                (
                    s.reflections.denoise,
                    DenoiserSignal::RgbHitDistance,
                    3,
                    s.reflections.roughness_cutoff,
                )
            }
            Effect::Gi => {
                kernels
                    .gi
                    .record(
                        encoder,
                        scene,
                        &GiInputs {
                            gbuffer,
                            output,
                            materials,
                            lighting: &frame.lighting,
                            frame_index,
                        },
                        &s.diffuse_gi,
                    )
                    .map_err(|e| e.to_string())?;
                (s.diffuse_gi.denoise, DenoiserSignal::Rgb, 2, 0.0)
            }
        };
        let mut source = &targets.outputs[index].view;
        let mut denoised = false;
        if denoise {
            let slot = &mut targets.denoisers[index];
            if slot.as_ref().is_none_or(|d| d.size() != (size[0], size[1])) {
                *slot = Denoiser::new(&rt, size[0], size[1], signal, &s.denoiser).ok();
            }
            if let Some(denoiser) = slot.as_mut() {
                if denoiser.settings() != &s.denoiser {
                    let _ = denoiser.set_settings(&s.denoiser);
                }
                let recorded = denoiser.record(
                    encoder,
                    &DenoiserInputs {
                        gbuffer: RtGBuffer {
                            world_position: &targets.position.view,
                            world_normal: normals,
                            width: size[0],
                            height: size[1],
                        },
                        signal: &targets.outputs[index].view,
                        motion,
                        eye,
                        camera_cut: view.reset_history,
                    },
                );
                if recorded.is_ok() {
                    source = denoiser.output_view();
                    denoised = true;
                }
            }
        }
        let params = Params {
            matrix: [[0.0; 4]; 4],
            view_from_world: [[0.0; 4]; 4],
            size: [size[0], size[1], mode, 0],
            values: [0.0, 0.0, cutoff, 0.0],
        };
        dispatch(
            rt.device(),
            encoder,
            &passes.blit,
            &[(8, source), (9, &screen[screen_slot(effect)])],
            &params,
            size,
        );
        Ok(denoised)
    }
}

/// The screen-lighting input (binding order shadow, AO, indirect,
/// reflection) an effect writes.
fn screen_slot(effect: Effect) -> usize {
    match effect {
        Effect::Shadows => 0,
        Effect::Ao => 1,
        Effect::Gi => 2,
        Effect::Reflections => 3,
    }
}

/// One `engine.rt.*` feature.
struct RtFeature {
    effect: Effect,
    shared: Arc<Mutex<Shared>>,
}

impl PipelineFeature for RtFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(self.effect.feature(), Stage::Prepass)
            .after(names::NORMALS)
            .after(names::MOTION)
            .disabled_by_default()
            .needs(FeatureNeeds {
                normals: true,
                motion: true,
                screen_lighting: true,
            })
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let mut shared = lock(&self.shared);
        if shared.passes.is_none() {
            shared.passes = Some(Passes::new(&setup.gpu.device)?);
        }
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        lock(&self.shared).encode(self.effect, ctx)
    }
}

/// The renderer's RT tier: device, scene sync, features and status.
pub(crate) struct RtHost {
    shared: Arc<Mutex<Shared>>,
    registered: bool,
    attempted: bool,
    serial: u64,
    /// Authored settings of the frame being rendered.
    authored: Option<RtEffectsSettings>,
    report: Option<RayTracingReport>,
}

impl Default for RtHost {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::new())),
            registered: false,
            attempted: false,
            serial: 0,
            authored: None,
            report: None,
        }
    }
}

impl RtHost {
    /// Record why the device was created without the RT request: the
    /// effects then fall back with this reason instead of probing the device.
    pub fn set_unavailable(&mut self, status: RayTracingStatus) {
        self.attempted = true;
        lock(&self.shared).status = status;
    }

    /// The last camera view's report; `None` while no frame used RT.
    pub fn report(&self) -> Option<&RayTracingReport> {
        self.report.as_ref()
    }

    /// Before `render_view`: map the authored settings, enable the features
    /// of enabled effects in `view`, create the RT device once, sync the
    /// scene and publish the frame inputs.
    pub fn prepare(
        &mut self,
        gpu: &GpuContext,
        pipeline: &mut ScenePipeline,
        frame: &SceneRenderFrame,
        lights: &LightList,
        view: &mut ViewData,
    ) -> Result<()> {
        let authored = frame.ray_tracing.as_ref().map(effects_settings);
        if let Some(settings) = &authored {
            for effect in Effect::ALL {
                if effect.enabled(settings) {
                    view.features
                        .0
                        .entry(effect.feature().to_owned())
                        .or_insert(true);
                }
            }
        }
        let wanted: Vec<Effect> = Effect::ALL
            .into_iter()
            .filter(|e| view.features.get(e.feature()) == Some(true))
            .collect();
        self.authored = authored.clone();
        if wanted.is_empty() && !self.registered {
            return Ok(());
        }
        if !self.registered {
            let mut registry = FeatureRegistry::default();
            for effect in Effect::ALL {
                registry.register_engine(RtFeature {
                    effect,
                    shared: self.shared.clone(),
                })?;
            }
            pipeline
                .add_features(registry)
                .context("registering the engine.rt features")?;
            self.registered = true;
        }
        let mut shared = lock(&self.shared);
        if !wanted.is_empty() && !self.attempted {
            self.attempted = true;
            match RtDevice::new(&gpu.adapter, &gpu.device, &gpu.queue) {
                Ok(rt) => {
                    let rt = Arc::new(rt);
                    shared.status = rt.status();
                    let lost = rt.clone();
                    gpu.on_device_lost(move |message| lost.report_device_lost(message));
                    shared.rt = Some(rt);
                }
                Err(status) => shared.status = status,
            }
        }
        if !wanted.is_empty() {
            shared.sync_scene(frame);
        }
        let mut settings = authored.unwrap_or_default();
        for effect in Effect::ALL {
            effect.set_enabled(&mut settings, wanted.contains(&effect));
        }
        let sun = lights
            .lights
            .iter()
            .find(|l| matches!(l.shape, LightShape::Directional));
        let ambient = view.ambient.to_array();
        let lighting = RtLighting {
            sun_direction: sun.map_or([0.0, 1.0, 0.0], |l| (-l.direction).to_array()),
            sun_illuminance: sun.map_or([0.0; 3], |l| (l.color * l.intensity).to_array()),
            sky_zenith: ambient,
            sky_horizon: ambient,
        };
        self.serial += 1;
        shared.frame = Some(FrameInputs {
            settings,
            lighting,
            sun: sun.map(|l| (-l.direction).to_array()),
            serial: self.serial,
        });
        shared.outcomes = Default::default();
        Ok(())
    }

    /// After a camera view rendered: the per-effect report.
    pub fn finish(&mut self, report: &ViewReport) {
        if !self.registered {
            self.report = None;
            return;
        }
        let shared = lock(&self.shared);
        let authored = self.authored.clone().unwrap_or_default();
        let base = authored.status(&shared.status);
        let effect_status = |effect: Effect| -> RtEffectStatus {
            let outcome = &shared.outcomes[effect as usize];
            let base = match effect {
                Effect::Shadows => &base.shadows,
                Effect::Ao => &base.ambient_occlusion,
                Effect::Reflections => &base.reflections,
                Effect::Gi => &base.diffuse_gi,
            };
            let running = report
                .features
                .get(effect.feature())
                .is_some_and(|state| state.runs());
            let (mode, denoised, reason) = if outcome.traced {
                (
                    RtEffectMode::RayTraced,
                    outcome.denoised,
                    base.reason.clone(),
                )
            } else if outcome.ran || running {
                let reason = outcome
                    .reason
                    .clone()
                    .or_else(|| base.reason.clone())
                    .unwrap_or_else(|| "the effect did not run; using the raster fallback".into());
                (RtEffectMode::Fallback, false, Some(reason))
            } else if effect.enabled(&authored) {
                (
                    RtEffectMode::Disabled,
                    false,
                    Some(format!(
                        "{} is disabled by Camera3D.render_features",
                        effect.feature()
                    )),
                )
            } else {
                (
                    RtEffectMode::Disabled,
                    false,
                    Some("disabled in settings".into()),
                )
            };
            RtEffectStatus {
                effect: effect.rt(),
                mode,
                denoised,
                reason: reason.map(|r| engine_render_rt::bounded_reason(&r)),
            }
        };
        let status = RtEffectsStatus {
            shadows: effect_status(Effect::Shadows),
            ambient_occlusion: effect_status(Effect::Ao),
            reflections: effect_status(Effect::Reflections),
            diffuse_gi: effect_status(Effect::Gi),
        };
        let stats = shared
            .scene
            .as_ref()
            .map(RtScene::stats)
            .unwrap_or_default();
        let effects = [
            (Effect::Shadows, &status.shadows),
            (Effect::Ao, &status.ambient_occlusion),
            (Effect::Reflections, &status.reflections),
            (Effect::Gi, &status.diffuse_gi),
        ]
        .into_iter()
        .map(|(effect, s)| RtEffectReport {
            effect: effect.key().into(),
            mode: match s.mode {
                RtEffectMode::RayTraced => "ray_traced",
                RtEffectMode::Fallback => "fallback",
                RtEffectMode::Disabled => "disabled",
            }
            .into(),
            denoised: s.denoised,
            fallback: (s.mode == RtEffectMode::Fallback).then(|| effect.fallback().into()),
            reason: s.reason.clone(),
        })
        .collect();
        self.report = Some(RayTracingReport {
            state: RenderPipelineState::Rendered,
            tier: match shared.status.tier {
                RayTracingTier::Hardware => "hardware",
                RayTracingTier::Unavailable => "unavailable",
            }
            .into(),
            reason: shared.status.reason.clone(),
            effects,
            blas_count: stats.blas_count,
            instance_count: stats.instance_count,
            triangle_count: stats.triangle_count,
            last_blas_built: shared.last_commit.blas_built,
        });
    }

    /// The effects status in the RT crate's types (for native callers).
    pub fn effects_status(&self) -> Option<RtEffectsStatus> {
        let report = self.report.as_ref()?;
        let effect = |key: &str, effect: RtEffect| {
            let r = report.effects.iter().find(|e| e.effect == key)?;
            Some(RtEffectStatus {
                effect,
                mode: match r.mode.as_str() {
                    "ray_traced" => RtEffectMode::RayTraced,
                    "fallback" => RtEffectMode::Fallback,
                    _ => RtEffectMode::Disabled,
                },
                denoised: r.denoised,
                reason: r.reason.clone(),
            })
        };
        Some(RtEffectsStatus {
            shadows: effect("shadows", RtEffect::Shadows)?,
            ambient_occlusion: effect("ambient_occlusion", RtEffect::AmbientOcclusion)?,
            reflections: effect("reflections", RtEffect::Reflections)?,
            diffuse_gi: effect("diffuse_gi", RtEffect::DiffuseGi)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_integration_shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("validates");
    }

    #[test]
    fn authored_settings_map_field_by_field() {
        let mut authored = RayTracing::default();
        authored.ambient_occlusion.enabled = true;
        authored.ambient_occlusion.radius = 2.5;
        authored.diffuse_gi.bounces = 3;
        authored.denoiser.phi_normal = 64.0;
        let s = effects_settings(&authored);
        assert!(s.ambient_occlusion.enabled && !s.shadows.enabled);
        assert_eq!(s.ambient_occlusion.radius, 2.5);
        assert_eq!(s.diffuse_gi.bounces, 3);
        assert_eq!(s.denoiser.phi_normal, 64.0);
        // Defaults agree between the authored component and the RT crate.
        let defaults = effects_settings(&RayTracing::default());
        assert_eq!(defaults, RtEffectsSettings::default());
        s.validate().unwrap();
    }
}
