//! The 3D frame: per-view state, stage execution and host hooks.

use crate::builtin::mesh::{MeshRenderer, MeshViewState, SceneMesh};
use crate::builtin::passes::{OutputPass, UpscalePass};
use crate::context::{
    FeatureContext, FeatureSetup, FrameIo, HdrSource, OutputTarget, PipelineLayouts,
};
use crate::feature::{
    Activity, FeatureBody, FeatureDescriptor, FeatureRegistry, PipelineFeature, ResolvedFeatures,
    Stage,
};
use crate::instancing::DrawStats;
use crate::lights::LightList;
use crate::shader::ShaderComposer;
use crate::targets::{DEPTH_FORMAT, FrameTargets, Target};
use crate::view::{VIEW_UNIFORM_BYTES, ViewData, ViewHistory, ViewId};
use anyhow::{Context, Result, bail, ensure};
use bevy_ecs::world::World;
use engine_render_wgpu::GpuContext;
use std::collections::HashMap;

/// Stable names of the built-in features.
pub mod names {
    /// Shadow stage: shadow casters into the view's shadow atlas.
    pub const SHADOWS: &str = "engine.shadows";
    /// Prepass stage: world normals, when a running feature needs them.
    pub const NORMALS: &str = "engine.prepass.normals";
    /// Prepass stage: motion vectors, when a feature or the host needs them.
    pub const MOTION: &str = "engine.prepass.motion";
    /// Opaque stage: opaque engine meshes (instanced).
    pub const OPAQUE: &str = "engine.opaque";
    /// Transparent stage: blended engine meshes, back to front.
    pub const TRANSPARENT: &str = "engine.transparent";
    /// Upscale stage: bilinear resample when render and output sizes differ.
    pub const UPSCALE: &str = "engine.upscale";
    /// Tonemap stage: the default output operator (linear clamp). Required.
    pub const OUTPUT: &str = "engine.output";
    /// Overlay stage: the host's 2D sprites/UI (a host-executed node).
    pub const SPRITES: &str = "engine.sprites";
}

/// Host-side work interleaved with the pipeline's stages.
pub trait HostStages {
    /// Called after the Transparent stage, once render-size scene colour,
    /// depth and (when needed) motion are complete and submitted. Return
    /// [`HostUpscale::Upscaled`] with an output-size HDR view to replace the
    /// Upscale stage (DLSS), [`HostUpscale::NotHandled`] to run it, or
    /// [`HostUpscale::Abort`] to stop this view's frame without output.
    fn upscale(&mut self, _gpu: &GpuContext, _scene: &SceneTextures<'_>) -> Result<HostUpscale> {
        Ok(HostUpscale::NotHandled)
    }

    /// Execute a host overlay node (for example [`names::SPRITES`]) into
    /// `target`. Earlier stage work is already submitted.
    fn overlay(&mut self, _node: &str, _gpu: &GpuContext, _target: &OutputTarget) -> Result<()> {
        Ok(())
    }
}

/// A host without stage hooks.
pub struct NoHost;
impl HostStages for NoHost {}

/// What the host's upscale hook did.
pub enum HostUpscale {
    NotHandled,
    /// An output-size `Rgba16Float`-compatible HDR view the rest of the
    /// frame reads.
    Upscaled(wgpu::TextureView),
    Abort,
}

/// Render-size scene textures handed to [`HostStages::upscale`].
pub struct SceneTextures<'a> {
    pub hdr: &'a Target,
    pub depth: &'a Target,
    pub motion: Option<&'a Target>,
    pub render_size: [u32; 2],
    pub output_size: [u32; 2],
}

/// Everything one view renders this frame.
pub struct SceneFrame<'a> {
    pub view: &'a ViewData,
    pub meshes: &'a [SceneMesh<'a>],
    pub lights: &'a LightList,
    pub world: Option<&'a World>,
}

/// What one view's frame did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewReport {
    pub view: Option<ViewId>,
    /// Mesh draws submitted to the built-in path.
    pub draws: usize,
    /// Instanced draw calls and instances (built-ins and features that
    /// called [`FeatureContext::record_draws`]).
    pub draw_calls: usize,
    pub instances: usize,
    pub features: ResolvedFeatures,
    /// The host aborted the frame in its upscale hook.
    pub aborted: bool,
}

struct Atlas {
    resolution: u32,
    layers: u32,
    array_view: wgpu::TextureView,
    layer_views: Vec<wgpu::TextureView>,
}

impl Atlas {
    fn new(device: &wgpu::Device, resolution: u32, layers: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("engine shadow atlas"),
            size: wgpu::Extent3d {
                width: resolution,
                height: resolution,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layer_views = (0..layers)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self {
            resolution,
            layers,
            array_view,
            layer_views,
        }
    }
}

struct GrowBuffer {
    buffer: wgpu::Buffer,
    capacity: u64,
}

impl GrowBuffer {
    fn new(device: &wgpu::Device, label: &str, capacity: u64, usage: wgpu::BufferUsages) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: capacity,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
        }
    }

    /// Write `bytes`, growing first; `true` when the buffer was replaced.
    fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        bytes: &[u8],
    ) -> bool {
        let grown = self.capacity < bytes.len() as u64;
        if grown {
            *self = Self::new(
                device,
                label,
                (bytes.len() as u64).next_power_of_two(),
                wgpu::BufferUsages::STORAGE,
            );
        }
        queue.write_buffer(&self.buffer, 0, bytes);
        grown
    }
}

struct ViewState {
    targets: Option<FrameTargets>,
    history: ViewHistory,
    mesh: MeshViewState,
    uniform: wgpu::Buffer,
    lights: GrowBuffer,
    shadows: GrowBuffer,
    atlas: Atlas,
    bind_group: wgpu::BindGroup,
    /// Placeholder atlas of `shadow_group`.
    no_atlas: Atlas,
    /// The view group with a placeholder atlas: bound while the Shadow stage
    /// renders into the real atlas (a texture cannot be sampled and written
    /// in one pass).
    shadow_group: wgpu::BindGroup,
    report: ViewReport,
    final_hdr: Option<wgpu::Texture>,
}

impl ViewState {
    fn new(device: &wgpu::Device, layouts: &PipelineLayouts) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine view"),
            size: VIEW_UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lights = GrowBuffer::new(device, "engine lights", 256, wgpu::BufferUsages::STORAGE);
        let shadows = GrowBuffer::new(device, "engine shadows", 256, wgpu::BufferUsages::STORAGE);
        let atlas = Atlas::new(device, 64, 1);
        let bind_group = view_bind_group(device, layouts, &uniform, &lights, &shadows, &atlas);
        let no_atlas = Atlas::new(device, 1, 1);
        let shadow_group = view_bind_group(device, layouts, &uniform, &lights, &shadows, &no_atlas);
        Self {
            targets: None,
            history: ViewHistory::default(),
            mesh: MeshViewState::default(),
            uniform,
            lights,
            shadows,
            atlas,
            bind_group,
            no_atlas,
            shadow_group,
            report: ViewReport::default(),
            final_hdr: None,
        }
    }
}

fn view_bind_group(
    device: &wgpu::Device,
    layouts: &PipelineLayouts,
    uniform: &wgpu::Buffer,
    lights: &GrowBuffer,
    shadows: &GrowBuffer,
    atlas: &Atlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("engine view"),
        layout: &layouts.view,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: lights.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: shadows.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&atlas.array_view),
            },
        ],
    })
}

fn layouts(device: &wgpu::Device) -> PipelineLayouts {
    let all = wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE;
    let storage = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: all,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let texture = |binding, sample_type, view_dimension| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: all,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension,
            multisampled: false,
        },
        count: None,
    };
    let dynamic_uniform = |binding, size| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    };
    let float = wgpu::TextureSampleType::Float { filterable: true };
    let d2 = wgpu::TextureViewDimension::D2;
    PipelineLayouts {
        view: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine::view"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: all,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(VIEW_UNIFORM_BYTES),
                    },
                    count: None,
                },
                storage(1),
                storage(2),
                texture(
                    3,
                    wgpu::TextureSampleType::Depth,
                    wgpu::TextureViewDimension::D2Array,
                ),
            ],
        }),
        instances: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine::mesh instances"),
            entries: &[storage(0)],
        }),
        material_textures: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine::material textures"),
            entries: &[
                texture(0, float, d2),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: all,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(2, float, d2),
                texture(3, float, d2),
            ],
        }),
        material_params: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine::material uniforms"),
            entries: &[dynamic_uniform(0, 32), dynamic_uniform(2, 16)],
        }),
    }
}

/// The built-in features in registration order (no device needed).
pub fn builtin_registry() -> Result<FeatureRegistry> {
    let mut registry = FeatureRegistry::default();
    for descriptor in [
        FeatureDescriptor::new(names::SHADOWS, Stage::Shadow),
        FeatureDescriptor::new(names::NORMALS, Stage::Prepass),
        FeatureDescriptor::new(names::MOTION, Stage::Prepass),
        FeatureDescriptor::new(names::OPAQUE, Stage::Opaque),
        FeatureDescriptor::new(names::TRANSPARENT, Stage::Transparent),
        FeatureDescriptor::new(names::UPSCALE, Stage::Upscale),
        FeatureDescriptor::new(names::OUTPUT, Stage::Tonemap).required(),
    ] {
        registry.insert(descriptor, FeatureBody::Builtin)?;
    }
    registry.insert(
        FeatureDescriptor::new(names::SPRITES, Stage::Overlay),
        FeatureBody::Host,
    )?;
    Ok(registry)
}

/// View id of the overlay-only state used for frames without a 3D view.
const OVERLAY_ONLY: ViewId = ViewId(u64::MAX);

/// The 3D renderer: registered features plus built-ins, per-view state,
/// and stage execution. Built on a shared [`GpuContext`].
pub struct ScenePipeline {
    gpu: GpuContext,
    layouts: PipelineLayouts,
    composer: ShaderComposer,
    registry: FeatureRegistry,
    mesh: MeshRenderer,
    output: OutputPass,
    upscale: UpscalePass,
    views: HashMap<ViewId, ViewState>,
    shadow_alignment: u32,
}

impl ScenePipeline {
    /// Create the pipeline and its built-in features on `gpu`.
    pub fn new(gpu: GpuContext) -> Result<Self> {
        let device = &gpu.device;
        let layouts = layouts(device);
        let mesh = MeshRenderer::new(&gpu, &layouts).context("creating the built-in mesh path")?;
        let output = OutputPass::new(device)?;
        let upscale = UpscalePass::new(device)?;
        let registry = builtin_registry()?;
        let shadow_alignment = device.limits().min_uniform_buffer_offset_alignment.max(16);
        Ok(Self {
            gpu,
            layouts,
            composer: ShaderComposer::new(),
            registry,
            mesh,
            output,
            upscale,
            views: HashMap::new(),
            shadow_alignment,
        })
    }

    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    pub fn layouts(&self) -> &PipelineLayouts {
        &self.layouts
    }

    /// Engine WGSL modules; register feature modules here before
    /// registering the features that compose against them.
    pub fn composer_mut(&mut self) -> &mut ShaderComposer {
        &mut self.composer
    }

    pub fn composer(&self) -> &ShaderComposer {
        &self.composer
    }

    pub fn registry(&self) -> &FeatureRegistry {
        &self.registry
    }

    /// Register and prepare one feature.
    pub fn register(&mut self, feature: impl PipelineFeature) -> Result<()> {
        let mut registry = FeatureRegistry::default();
        registry.register(feature)?;
        self.add_features(registry)
    }

    /// Prepare every feature of `features` on this device, then add them.
    /// Fails without changes on a preparation error or a name conflict.
    pub fn add_features(&mut self, mut features: FeatureRegistry) -> Result<()> {
        let setup = FeatureSetup {
            gpu: &self.gpu,
            layouts: &self.layouts,
            composer: &self.composer,
        };
        for registered in &mut features.features {
            if let FeatureBody::Owned(feature) = &mut registered.body {
                let name = registered.descriptor.name.clone();
                crate::scoped(&self.gpu.device, || feature.prepare(&setup))
                    .with_context(|| format!("preparing render feature {name}"))?;
                registered.prepared = true;
            }
        }
        self.registry.absorb(features)
    }

    /// The report of the last frame of `view`.
    pub fn view_report(&self, view: ViewId) -> Option<&ViewReport> {
        self.views.get(&view).map(|s| &s.report)
    }

    /// Last frame's unjittered view-projection of `view` as the next frame
    /// will see it (`current` on the first frame or with `reset`).
    pub fn previous_clip_from_world(
        &self,
        view: ViewId,
        current: glam::Mat4,
        reset: bool,
    ) -> glam::Mat4 {
        self.views
            .get(&view)
            .map_or(current, |state| state.history.previous(current, reset))
    }

    /// Drop a view's targets and history (for example a closed viewport).
    pub fn forget_view(&mut self, view: ViewId) {
        self.views.remove(&view);
    }

    /// Render-size targets of `view`'s last frame.
    pub fn targets(&self, view: ViewId) -> Option<&FrameTargets> {
        self.views.get(&view)?.targets.as_ref()
    }

    /// Render one view: scene stages at the render size, upscale, HDR post,
    /// the output operator into `target`, then overlays. Invalid view, light
    /// or mesh data fails before anything is submitted.
    pub fn render_view(
        &mut self,
        frame: &SceneFrame<'_>,
        target: &OutputTarget,
        host: &mut dyn HostStages,
    ) -> Result<ViewReport> {
        let view = frame.view;
        view.validate()?;
        frame.lights.validate()?;
        ensure!(
            view.id != OVERLAY_ONLY,
            "view id {} is reserved",
            OVERLAY_ONLY.0
        );
        let v = target.viewport;
        ensure!(
            v.width == view.output_size[0]
                && v.height == view.output_size[1]
                && v.x + v.width <= target.size[0]
                && v.y + v.height <= target.size[1],
            "the target viewport must lie inside the target and match the view's output size"
        );
        let mut resolved = self.registry.resolve(&view.features);
        let needs = self.registry.needs(&resolved).union(view.host_needs);
        let device = self.gpu.device.clone();
        let queue = self.gpu.queue.clone();
        let layouts = &self.layouts;
        let state = self
            .views
            .entry(view.id)
            .or_insert_with(|| ViewState::new(&device, layouts));

        // CPU validation and uploads first: nothing is encoded on failure.
        self.mesh
            .prepare(
                &self.gpu,
                layouts,
                &mut state.mesh,
                frame.meshes,
                view.camera_position(),
                view.reset_history,
            )
            .context("preparing scene meshes")?;
        let mut targets = match state.targets.take() {
            Some(t) if t.render_size == view.render_size && t.output_size == view.output_size => t,
            _ => FrameTargets::new(&device, view.render_size, view.output_size),
        };
        if needs.normals {
            targets.ensure_normals(&device);
        }
        if needs.motion {
            targets.ensure_motion(&device);
        }
        let shadows_run = resolved.is_enabled(names::SHADOWS) && !frame.lights.shadows.is_empty();
        let mut lights = frame.lights.clone();
        if !shadows_run {
            for light in &mut lights.lights {
                light.shadow_index = -1;
            }
        }
        let (resolution, layers) = lights.atlas();
        let mut rebind =
            state
                .lights
                .write(&device, &queue, "engine lights", &lights.light_bytes());
        rebind |= state
            .shadows
            .write(&device, &queue, "engine shadows", &lights.shadow_bytes());
        if state.atlas.resolution != resolution || state.atlas.layers != layers {
            state.atlas = Atlas::new(&device, resolution, layers);
            rebind = true;
        }
        if rebind {
            state.bind_group = view_bind_group(
                &device,
                layouts,
                &state.uniform,
                &state.lights,
                &state.shadows,
                &state.atlas,
            );
            state.shadow_group = view_bind_group(
                &device,
                layouts,
                &state.uniform,
                &state.lights,
                &state.shadows,
                &state.no_atlas,
            );
        }
        let current = view.clip_from_world();
        let previous = state.history.previous(current, view.reset_history);
        let uniform = view.uniform(
            previous,
            state.history.frame_index(),
            lights.lights.len() as u32,
        );
        queue.write_buffer(&state.uniform, 0, bytemuck::bytes_of(&uniform));

        let mut stats = DrawStats::default();
        let mut io = FrameIo {
            targets: Some(targets),
            current_hdr: None,
            source: HdrSource::Scene,
            output: Some(target.clone()),
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("engine scene stages"),
        });
        clear_scene(
            &mut encoder,
            io.targets.as_ref().expect("targets set above"),
        );
        let result = (|| -> Result<Option<HostUpscale>> {
            for stage in [
                Stage::Prepare,
                Stage::Shadow,
                Stage::Prepass,
                Stage::Opaque,
                Stage::Sky,
                Stage::Transparent,
            ] {
                run_stage(
                    StageRun {
                        gpu: &self.gpu,
                        layouts,
                        registry: &mut self.registry,
                        mesh: &self.mesh,
                        output: &mut self.output,
                        upscale: &self.upscale,
                        shadow_alignment: self.shadow_alignment,
                        state,
                        view,
                        frame,
                        lights: &lights,
                        needs,
                    },
                    stage,
                    &mut encoder,
                    &mut io,
                    &mut resolved,
                    &mut stats,
                )?;
            }
            let finished = std::mem::replace(
                &mut encoder,
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("engine output stages"),
                }),
            );
            queue.submit([finished.finish()]);
            let targets = io.targets.as_ref().expect("targets set above");
            let textures = SceneTextures {
                hdr: &targets.hdr,
                depth: &targets.depth,
                motion: targets.motion.as_ref(),
                render_size: view.render_size,
                output_size: view.output_size,
            };
            let decision = host.upscale(&self.gpu, &textures)?;
            Ok(Some(decision))
        })();
        let decision = match result {
            Ok(decision) => decision,
            Err(error) => {
                state.targets = io.targets.take();
                return Err(error);
            }
        };
        if let Some(HostUpscale::Abort) = decision {
            state.targets = io.targets.take();
            let report = ViewReport {
                view: Some(view.id),
                aborted: true,
                features: resolved,
                ..Default::default()
            };
            state.report = report.clone();
            return Ok(report);
        }
        io.current_hdr = Some(io.targets.as_ref().expect("targets").hdr.view.clone());
        if let Some(HostUpscale::Upscaled(upscaled)) = decision {
            io.current_hdr = Some(upscaled);
            io.source = HdrSource::Host;
            for state in resolved
                .states
                .iter_mut()
                .filter(|s| s.stage == Stage::Upscale && s.enabled)
            {
                state.activity = Activity::ReplacedByHost;
            }
        }
        let result = (|| -> Result<()> {
            let stages: &[Stage] = if io.source == HdrSource::Host {
                &[Stage::PostProcess, Stage::Tonemap, Stage::Overlay]
            } else {
                &[
                    Stage::Upscale,
                    Stage::PostProcess,
                    Stage::Tonemap,
                    Stage::Overlay,
                ]
            };
            for &stage in stages {
                if stage == Stage::Overlay {
                    run_overlay(
                        &self.gpu,
                        layouts,
                        &mut self.registry,
                        &state.bind_group,
                        view,
                        frame.world,
                        &mut encoder,
                        &mut io,
                        &mut resolved,
                        &mut stats,
                        host,
                    )?;
                    continue;
                }
                run_stage(
                    StageRun {
                        gpu: &self.gpu,
                        layouts,
                        registry: &mut self.registry,
                        mesh: &self.mesh,
                        output: &mut self.output,
                        upscale: &self.upscale,
                        shadow_alignment: self.shadow_alignment,
                        state,
                        view,
                        frame,
                        lights: &lights,
                        needs,
                    },
                    stage,
                    &mut encoder,
                    &mut io,
                    &mut resolved,
                    &mut stats,
                )?;
            }
            Ok(())
        })();
        queue.submit([encoder.finish()]);
        let targets = io.targets.take().expect("targets set above");
        state.final_hdr = match io.source {
            HdrSource::Scene => Some(targets.hdr.texture.clone()),
            HdrSource::Upscaled => targets.upscaled.as_ref().map(|t| t.texture.clone()),
            HdrSource::Post(i) => targets.post[i].as_ref().map(|t| t.texture.clone()),
            HdrSource::Host => None,
        };
        state.targets = Some(targets);
        result?;
        state.history.commit(current);
        for feature in &mut resolved.states {
            if feature.activity == Activity::Pending {
                feature.activity = Activity::Ran;
            }
        }
        let report = ViewReport {
            view: Some(view.id),
            draws: state.mesh.draws,
            draw_calls: stats.draw_calls,
            instances: stats.instances,
            features: resolved,
            aborted: false,
        };
        state.report = report.clone();
        Ok(report)
    }

    /// Run only the Overlay stage into `target` (frames without a 3D view):
    /// host nodes and overlay features in order.
    pub fn render_overlay_only(
        &mut self,
        target: &OutputTarget,
        world: Option<&World>,
        overrides: &crate::feature::FeatureOverrides,
        host: &mut dyn HostStages,
    ) -> Result<ViewReport> {
        let size = [target.size[0].max(1), target.size[1].max(1)];
        let mut view = ViewData::new(
            OVERLAY_ONLY,
            glam::Mat4::IDENTITY,
            glam::Mat4::IDENTITY,
            size,
            size,
        )?;
        view.features = overrides.clone();
        let device = self.gpu.device.clone();
        let layouts = &self.layouts;
        let state = self
            .views
            .entry(OVERLAY_ONLY)
            .or_insert_with(|| ViewState::new(&device, layouts));
        let uniform = view.uniform(view.clip_from_world(), 0, 0);
        self.gpu
            .queue
            .write_buffer(&state.uniform, 0, bytemuck::bytes_of(&uniform));
        let mut resolved = self.registry.resolve(&view.features);
        for feature in resolved
            .states
            .iter_mut()
            .filter(|s| s.stage != Stage::Overlay && s.enabled)
        {
            feature.activity = Activity::Idle;
        }
        let mut stats = DrawStats::default();
        let mut io = FrameIo {
            output: Some(target.clone()),
            ..Default::default()
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("engine overlay"),
        });
        let result = run_overlay(
            &self.gpu,
            layouts,
            &mut self.registry,
            &state.bind_group,
            &view,
            world,
            &mut encoder,
            &mut io,
            &mut resolved,
            &mut stats,
            host,
        );
        self.gpu.queue.submit([encoder.finish()]);
        result?;
        for feature in &mut resolved.states {
            if feature.activity == Activity::Pending {
                feature.activity = Activity::Ran;
            }
        }
        let report = ViewReport {
            view: None,
            draw_calls: stats.draw_calls,
            instances: stats.instances,
            features: resolved,
            ..Default::default()
        };
        state.report = report.clone();
        Ok(report)
    }

    /// Read back the final HDR colour (after post-processing, output size)
    /// of `view`'s last frame as linear RGBA `f32`, top row first.
    pub fn read_hdr(&self, view: ViewId) -> Result<HdrImage> {
        let state = self
            .views
            .get(&view)
            .context("no frame was rendered for this view")?;
        let texture = state
            .final_hdr
            .as_ref()
            .context("the final HDR colour of this view is host-owned (upscaled by the host)")?;
        crate::readback::read_rgba16f(&self.gpu, texture)
    }

    /// Read back the motion vectors (`xy`, render pixels) of `view`'s last
    /// frame; an error when no feature or host needed motion.
    pub fn read_motion(&self, view: ViewId) -> Result<HdrImage> {
        let targets = self
            .targets(view)
            .context("no frame was rendered for this view")?;
        let motion = targets
            .motion
            .as_ref()
            .context("this view produced no motion vectors")?;
        crate::readback::read_rgba16f(&self.gpu, &motion.texture)
    }

    /// Read back the world normals of `view`'s last frame; an error when no
    /// running feature needed normals.
    pub fn read_normals(&self, view: ViewId) -> Result<HdrImage> {
        let targets = self
            .targets(view)
            .context("no frame was rendered for this view")?;
        let normals = targets
            .normals
            .as_ref()
            .context("this view produced no normals")?;
        crate::readback::read_rgba16f(&self.gpu, &normals.texture)
    }

    /// Read back render-size scene HDR colour (before upscale/post).
    pub fn read_scene_hdr(&self, view: ViewId) -> Result<HdrImage> {
        let targets = self
            .targets(view)
            .context("no frame was rendered for this view")?;
        crate::readback::read_rgba16f(&self.gpu, &targets.hdr.texture)
    }
}

/// Linear RGBA `f32` pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct HdrImage {
    pub size: [u32; 2],
    pub pixels: Vec<[f32; 4]>,
}

impl HdrImage {
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        self.pixels[(y * self.size[0] + x) as usize]
    }
}

fn clear_scene(encoder: &mut wgpu::CommandEncoder, targets: &FrameTargets) {
    fn clear(
        view: &wgpu::TextureView,
        color: wgpu::Color,
    ) -> Option<wgpu::RenderPassColorAttachment<'_>> {
        Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
                store: wgpu::StoreOp::Store,
            },
        })
    }
    let mut colors = vec![clear(&targets.hdr.view, wgpu::Color::BLACK)];
    if let Some(normals) = &targets.normals {
        colors.push(clear(&normals.view, wgpu::Color::TRANSPARENT));
    }
    if let Some(motion) = &targets.motion {
        colors.push(clear(&motion.view, wgpu::Color::TRANSPARENT));
    }
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("engine clear"),
        color_attachments: &colors,
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &targets.depth.view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
}

struct StageRun<'a, 'f> {
    gpu: &'a GpuContext,
    layouts: &'a PipelineLayouts,
    registry: &'a mut FeatureRegistry,
    mesh: &'a MeshRenderer,
    output: &'a mut OutputPass,
    upscale: &'a UpscalePass,
    shadow_alignment: u32,
    state: &'a mut ViewState,
    view: &'a ViewData,
    frame: &'a SceneFrame<'f>,
    lights: &'a LightList,
    needs: crate::feature::FeatureNeeds,
}

fn run_stage(
    mut run: StageRun<'_, '_>,
    stage: Stage,
    encoder: &mut wgpu::CommandEncoder,
    io: &mut FrameIo,
    resolved: &mut ResolvedFeatures,
    stats: &mut DrawStats,
) -> Result<()> {
    let order: Vec<usize> = run.registry.ordered().to_vec();
    for index in order {
        let registered = &run.registry.features[index];
        if registered.descriptor.stage != stage {
            continue;
        }
        let name = registered.descriptor.name.clone();
        if !resolved.is_enabled(&name) {
            continue;
        }
        let builtin = matches!(registered.body, FeatureBody::Builtin);
        if builtin {
            let ran = run_builtin(&mut run, &name, encoder, io, stats)
                .with_context(|| format!("render feature {name}"))?;
            if !ran {
                resolved.set_activity(&name, Activity::Idle);
            }
            continue;
        }
        let registered = &mut run.registry.features[index];
        match &mut registered.body {
            FeatureBody::Builtin | FeatureBody::Host | FeatureBody::Descriptor => {}
            FeatureBody::Owned(feature) => {
                ensure!(
                    registered.prepared,
                    "render feature {name} was not prepared"
                );
                let mut context = FeatureContext {
                    gpu: run.gpu,
                    encoder,
                    stage,
                    view: run.view,
                    view_bind_group: if stage == Stage::Shadow {
                        &run.state.shadow_group
                    } else {
                        &run.state.bind_group
                    },
                    layouts: run.layouts,
                    world: run.frame.world,
                    io,
                    stats,
                    name: &name,
                };
                feature
                    .encode(&mut context)
                    .with_context(|| format!("render feature {name}"))?;
            }
        }
    }
    Ok(())
}

/// Built-in work; `Ok(false)` when there was nothing to do.
fn run_builtin(
    run: &mut StageRun<'_, '_>,
    name: &str,
    encoder: &mut wgpu::CommandEncoder,
    io: &mut FrameIo,
    stats: &mut DrawStats,
) -> Result<bool> {
    let state = &*run.state;
    let view_group = &state.bind_group;
    let device = &run.gpu.device;
    let targets = io.targets.as_mut().context("no scene targets")?;
    let scene_pass = |encoder: &'_ mut wgpu::CommandEncoder, targets: &FrameTargets, label| {
        encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.hdr.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            })
            .forget_lifetime()
    };
    match name {
        names::SHADOWS => {
            if run.lights.shadows.is_empty() {
                return Ok(false);
            }
            for (index, layer) in state.atlas.layer_views.iter().enumerate() {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("engine shadow layer"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: layer,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                if index < run.lights.shadows.len() {
                    *stats += run.mesh.draw_shadow(
                        &mut pass,
                        &state.shadow_group,
                        &state.mesh,
                        index as u32,
                        run.shadow_alignment,
                    );
                }
            }
            Ok(true)
        }
        names::NORMALS | names::MOTION => {
            let normals = name == names::NORMALS;
            if (normals && !run.needs.normals) || (!normals && !run.needs.motion) {
                return Ok(false);
            }
            let (color, depth, depth_load) = if normals {
                let target = targets.normals.as_ref().context("normals target")?;
                (&target.view, &targets.depth.view, wgpu::LoadOp::Load)
            } else {
                let target = targets.motion.as_ref().context("motion target")?;
                let depth = targets.motion_depth.as_ref().context("motion depth")?;
                (&target.view, &depth.view, wgpu::LoadOp::Clear(1.0))
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(name),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: depth_load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            *stats += run
                .mesh
                .draw_prepass(&mut pass, view_group, &state.mesh, normals);
            Ok(true)
        }
        names::OPAQUE => {
            let mut pass = scene_pass(encoder, targets, "engine opaque");
            *stats += run.mesh.draw_opaque(&mut pass, view_group, &state.mesh);
            Ok(true)
        }
        names::TRANSPARENT => {
            let mut pass = scene_pass(encoder, targets, "engine transparent");
            *stats += run
                .mesh
                .draw_transparent(&mut pass, view_group, &state.mesh);
            Ok(true)
        }
        names::UPSCALE => {
            if targets.render_size == targets.output_size {
                io.current_hdr = Some(targets.hdr.view.clone());
                io.source = HdrSource::Scene;
                return Ok(false);
            }
            let source = targets.hdr.view.clone();
            let destination = targets.ensure_upscaled(device).view.clone();
            run.upscale.encode(
                device,
                &run.gpu.queue,
                encoder,
                &source,
                &destination,
                run.view.output_size,
            );
            io.current_hdr = Some(destination);
            io.source = HdrSource::Upscaled;
            Ok(true)
        }
        names::OUTPUT => {
            let hdr = io.current_hdr.clone().context("no HDR colour to output")?;
            let target = io.output.as_ref().context("no output target")?;
            run.output.encode(
                device,
                &run.gpu.queue,
                encoder,
                &hdr,
                &target.view,
                target.format,
                target.viewport,
            )?;
            Ok(true)
        }
        other => bail!("unknown built-in feature {other}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_overlay(
    gpu: &GpuContext,
    layouts: &PipelineLayouts,
    registry: &mut FeatureRegistry,
    view_bind_group: &wgpu::BindGroup,
    view: &ViewData,
    world: Option<&World>,
    encoder: &mut wgpu::CommandEncoder,
    io: &mut FrameIo,
    resolved: &mut ResolvedFeatures,
    stats: &mut DrawStats,
    host: &mut dyn HostStages,
) -> Result<()> {
    let order: Vec<usize> = registry.ordered().to_vec();
    for index in order {
        let registered = &mut registry.features[index];
        if registered.descriptor.stage != Stage::Overlay {
            continue;
        }
        let name = registered.descriptor.name.clone();
        if !resolved.is_enabled(&name) {
            continue;
        }
        match &mut registered.body {
            FeatureBody::Host => {
                let finished = std::mem::replace(
                    encoder,
                    gpu.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("engine overlay"),
                        }),
                );
                gpu.queue.submit([finished.finish()]);
                let target = io.output.as_ref().context("no output target")?;
                host.overlay(&name, gpu, target)
                    .with_context(|| format!("host overlay {name}"))?;
            }
            FeatureBody::Builtin | FeatureBody::Descriptor => {}
            FeatureBody::Owned(feature) => {
                let mut context = FeatureContext {
                    gpu,
                    encoder,
                    stage: Stage::Overlay,
                    view,
                    view_bind_group,
                    layouts,
                    world,
                    io,
                    stats,
                    name: &name,
                };
                feature
                    .encode(&mut context)
                    .with_context(|| format!("render feature {name}"))?;
            }
        }
    }
    Ok(())
}
