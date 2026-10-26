//! The 3D frame: per-view state, stage execution and host hooks.

use crate::builtin::mesh::{MeshRenderer, MeshViewState, SceneMesh};
use crate::builtin::passes::{OutputPass, UpscalePass};
use crate::context::{
    FeatureContext, FeatureSetup, FrameIo, HdrSource, OutputTarget, PipelineLayouts,
    PresentationInput,
};
use crate::feature::{
    Activity, FeatureBody, FeatureDescriptor, FeatureRegistry, PipelineFeature, ResolvedFeatures,
    Stage,
};
use crate::instancing::DrawStats;
use crate::lighting::{LightingFrame, LightingRenderer, LightingReport, ViewLighting};
use crate::lights::LightList;
use crate::overlay::run_presentation;
use crate::shader::ShaderComposer;
use crate::shadows::ShadowAtlasConfig;
use crate::targets::{CompositeTargets, FrameTargets, Target};
use crate::view::{Presentation, VIEW_UNIFORM_BYTES, ViewData, ViewHistory, ViewId, Viewport};
use anyhow::{Context, Result, bail, ensure};
use bevy_ecs::world::World;
use engine_render_wgpu::GpuContext;
use std::collections::HashMap;

/// Stable names of the built-in features.
pub mod names {
    /// Prepare stage: the light cluster grid (compute). Disabled, every
    /// fragment loops over all uploaded local lights.
    pub const CLUSTERS: &str = "engine.lights.clusters";
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
    /// PostProcess stage: exposure (manual or auto), bloom and HDR colour
    /// grading. Runs for views with [`crate::ViewData::post`] set.
    pub const POST: &str = "engine.post";
    /// Tonemap stage: tonemapping, vignette, grain and the target encode,
    /// superseding `engine.output`. Runs for views with
    /// [`crate::ViewData::post`] set.
    pub const TONEMAP: &str = "engine.tonemap";
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

    /// Ordered painter items of a host node. None retains whole-node execution.
    fn overlay_items(
        &mut self,
        _node: &str,
        _view: &ViewData,
    ) -> Result<Option<Vec<crate::OverlayItem>>> {
        Ok(None)
    }

    /// Initialize the host's shared painter stream before any producer draws.
    fn begin_overlay(&mut self, _gpu: &GpuContext, _target: &OutputTarget) -> Result<()> {
        Ok(())
    }

    fn overlay_batch(
        &mut self,
        _node: &str,
        _gpu: &GpuContext,
        _target: &OutputTarget,
        _batch: crate::OverlayBatch<'_>,
    ) -> Result<()> {
        anyhow::bail!("ordered host overlay did not implement overlay_batch")
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
    /// Light culling, clustering and shadow allocation of the frame.
    pub lighting: LightingReport,
    /// The host aborted the frame in its upscale hook.
    pub aborted: bool,
}

struct ViewState {
    targets: Option<FrameTargets>,
    history: ViewHistory,
    mesh: MeshViewState,
    uniform: wgpu::Buffer,
    /// Light list, shadows, atlas, clusters and the `engine::view` groups.
    lighting: ViewLighting,
    report: ViewReport,
    final_hdr: Option<wgpu::Texture>,
    composite: Option<CompositeTargets>,
}

impl ViewState {
    fn new(gpu: &GpuContext, layouts: &PipelineLayouts, lighting: &LightingRenderer) -> Self {
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine view"),
            size: VIEW_UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lighting = ViewLighting::new(gpu, layouts, lighting, &uniform);
        Self {
            targets: None,
            history: ViewHistory::default(),
            mesh: MeshViewState::default(),
            uniform,
            lighting,
            report: ViewReport::default(),
            final_hdr: None,
            composite: None,
        }
    }
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
    let [
        clusters,
        cluster_lights,
        shadow_sampler,
        screen_shadow,
        screen_ao,
        screen_indirect,
        screen_reflection,
    ] = crate::lighting::view_layout_entries(all);
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
                clusters,
                cluster_lights,
                shadow_sampler,
                screen_shadow,
                screen_ao,
                screen_indirect,
                screen_reflection,
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
        FeatureDescriptor::new(names::CLUSTERS, Stage::Prepare),
        FeatureDescriptor::new(names::SHADOWS, Stage::Shadow),
        FeatureDescriptor::new(names::NORMALS, Stage::Prepass),
        FeatureDescriptor::new(names::MOTION, Stage::Prepass),
        FeatureDescriptor::new(names::OPAQUE, Stage::Opaque),
        FeatureDescriptor::new(names::TRANSPARENT, Stage::Transparent),
        FeatureDescriptor::new(names::UPSCALE, Stage::Upscale),
        FeatureDescriptor::new(names::POST, Stage::PostProcess).disabled_by_default(),
        FeatureDescriptor::new(names::OUTPUT, Stage::Tonemap).required(),
        FeatureDescriptor::new(names::TONEMAP, Stage::Tonemap)
            .after(names::OUTPUT)
            .disabled_by_default(),
    ] {
        registry.insert(descriptor, FeatureBody::Builtin)?;
    }
    registry.insert(
        FeatureDescriptor::new(names::SPRITES, Stage::Overlay).replayable(),
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
    post: std::sync::Arc<crate::post::PostShared>,
    mesh: MeshRenderer,
    output: OutputPass,
    composite_copy: OutputPass,
    upscale: UpscalePass,
    views: HashMap<ViewId, ViewState>,
    shadow_alignment: u32,
    lighting: LightingRenderer,
    atlas: ShadowAtlasConfig,
    presentation_sequence: bool,
    presentation_sequence_features: Vec<usize>,
}

impl ScenePipeline {
    /// Create the pipeline and its built-in features on `gpu`.
    pub fn new(gpu: GpuContext) -> Result<Self> {
        let device = &gpu.device;
        let layouts = layouts(device);
        let mesh = MeshRenderer::new(&gpu, &layouts).context("creating the built-in mesh path")?;
        let output = OutputPass::new(device)?;
        let composite_copy = OutputPass::new(device)?;
        let upscale = UpscalePass::new(device)?;
        let mut registry = builtin_registry()?;
        let composer = ShaderComposer::new();
        let post = std::sync::Arc::new(crate::post::PostShared::default());
        crate::post::install(
            &mut registry,
            &FeatureSetup {
                gpu: &gpu,
                layouts: &layouts,
                composer: &composer,
            },
            post.clone(),
        )
        .context("creating the post-processing stack")?;
        let lighting = LightingRenderer::new(&gpu).context("creating the lighting passes")?;
        let shadow_alignment = device.limits().min_uniform_buffer_offset_alignment.max(16);
        Ok(Self {
            gpu,
            layouts,
            composer,
            registry,
            post,
            mesh,
            output,
            composite_copy,
            upscale,
            views: HashMap::new(),
            shadow_alignment,
            lighting,
            atlas: ShadowAtlasConfig::default(),
            presentation_sequence: false,
            presentation_sequence_features: Vec::new(),
        })
    }

    /// Shadow atlas page size and page budget of every view.
    pub fn shadow_atlas(&self) -> ShadowAtlasConfig {
        self.atlas
    }

    pub fn set_shadow_atlas(&mut self, config: ShadowAtlasConfig) -> Result<()> {
        config.validate()?;
        self.atlas = config;
        Ok(())
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
        ensure!(
            !self.presentation_sequence,
            "cannot register features during a presentation sequence"
        );
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
        if let Some(post) = &view.post {
            post.validate()?;
        }
        // Post settings turn the post stack on unless the view says otherwise.
        let mut overrides = view.features.clone();
        if view.post.is_some() {
            for name in [names::POST, names::TONEMAP] {
                overrides.0.entry(name.to_owned()).or_insert(true);
            }
        }
        let mut resolved = self.registry.resolve(&overrides);
        let needs = self.registry.needs(&resolved).union(view.host_needs);
        let device = self.gpu.device.clone();
        let queue = self.gpu.queue.clone();
        let layouts = &self.layouts;
        let gpu = &self.gpu;
        let lighting = &self.lighting;
        let state = self
            .views
            .entry(view.id)
            .or_insert_with(|| ViewState::new(gpu, layouts, lighting));

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
        if needs.screen_lighting {
            targets.ensure_screen_lighting(&device);
        }
        state.lighting.set_screen(
            &device,
            layouts,
            &self.lighting,
            targets
                .screen_lighting
                .as_ref()
                .filter(|_| needs.screen_lighting),
        );
        let light_count = state
            .lighting
            .prepare(
                &self.gpu,
                layouts,
                &self.lighting,
                &state.uniform,
                &LightingFrame {
                    view,
                    lights: frame.lights,
                    shadows: resolved.is_enabled(names::SHADOWS),
                    clusters: resolved.is_enabled(names::CLUSTERS),
                    atlas: self.atlas,
                },
            )
            .context("preparing lights")?;
        let current = view.clip_from_world();
        let previous = state.history.previous(current, view.reset_history);
        let uniform = view.uniform(previous, state.history.frame_index(), light_count);
        queue.write_buffer(&state.uniform, 0, bytemuck::bytes_of(&uniform));

        let mut stats = DrawStats::default();
        let composite = view.presentation == Presentation::Enabled
            && composite_needed(&self.registry, &resolved, view);
        let mut io = presentation_io(&device, target, state.composite.take(), composite);
        io.targets = Some(targets);
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
                        lighting: &self.lighting,
                        state,
                        view,
                        frame,
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
            state.lighting.after_submit();
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
                state.composite = io.composite.take();
                return Err(error);
            }
        };
        if let Some(HostUpscale::Abort) = decision {
            state.targets = io.targets.take();
            state.composite = io.composite.take();
            let report = ViewReport {
                view: Some(view.id),
                aborted: true,
                features: resolved,
                lighting: state.lighting.report.clone(),
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
                &[
                    Stage::PostProcess,
                    Stage::Tonemap,
                    Stage::Overlay,
                    Stage::Composite,
                ]
            } else {
                &[
                    Stage::Upscale,
                    Stage::PostProcess,
                    Stage::Tonemap,
                    Stage::Overlay,
                    Stage::Composite,
                ]
            };
            for &stage in stages {
                if matches!(stage, Stage::Overlay | Stage::Composite) {
                    if view.presentation == Presentation::SceneOnly {
                        for feature in resolved
                            .states
                            .iter_mut()
                            .filter(|f| f.stage == stage && f.enabled)
                        {
                            feature.activity = Activity::Idle;
                        }
                        continue;
                    }
                    run_presentation(
                        stage,
                        &self.gpu,
                        layouts,
                        &mut self.registry,
                        state.lighting.bind_group(),
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
                        lighting: &self.lighting,
                        state,
                        view,
                        frame,
                        needs,
                    },
                    stage,
                    &mut encoder,
                    &mut io,
                    &mut resolved,
                    &mut stats,
                )?;
            }
            finish_presentation(&mut self.composite_copy, &self.gpu, &mut encoder, &io)?;
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
        state.composite = io.composite.take();
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
            lighting: state.lighting.report.clone(),
            aborted: false,
        };
        state.report = report.clone();
        Ok(report)
    }

    /// Run overlays and final display filters without a 3D scene.
    pub fn render_overlay_only(
        &mut self,
        target: &OutputTarget,
        world: Option<&World>,
        overrides: &crate::FeatureOverrides,
        host: &mut dyn HostStages,
    ) -> Result<ViewReport> {
        self.render_presentation(None, target, world, overrides, host)
    }

    /// Composite overlays and final filters over an already tonemapped,
    /// UI-free image. Source and target have equal size. The source may alias
    /// the target: it is sampled into an owned scratch image before any writes
    /// to the caller's target. Scene temporal histories are not advanced.
    pub fn render_overlay_over(
        &mut self,
        source: PresentationInput<'_>,
        target: &OutputTarget,
        world: Option<&World>,
        overrides: &crate::FeatureOverrides,
        host: &mut dyn HostStages,
    ) -> Result<ViewReport> {
        ensure!(
            source.size == target.size,
            "presentation source and target sizes must match"
        );
        self.render_presentation(Some(source), target, world, overrides, host)
    }

    fn render_presentation(
        &mut self,
        source: Option<PresentationInput<'_>>,
        target: &OutputTarget,
        world: Option<&World>,
        overrides: &crate::FeatureOverrides,
        host: &mut dyn HostStages,
    ) -> Result<ViewReport> {
        ensure!(
            target.size[0] > 0 && target.size[1] > 0,
            "presentation target must be nonzero"
        );
        let size = target.size;
        let mut view = ViewData::new(
            OVERLAY_ONLY,
            glam::Mat4::IDENTITY,
            glam::Mat4::IDENTITY,
            size,
            size,
        )?;
        view.features = overrides.clone();
        let device = self.gpu.device.clone();
        let gpu = &self.gpu;
        let layouts = &self.layouts;
        let state = self
            .views
            .entry(OVERLAY_ONLY)
            .or_insert_with(|| ViewState::new(gpu, layouts, &self.lighting));
        let uniform = view.uniform(view.clip_from_world(), 0, 0);
        gpu.queue
            .write_buffer(&state.uniform, 0, bytemuck::bytes_of(&uniform));
        let mut resolved = self.registry.resolve(&view.features);
        for feature in resolved
            .states
            .iter_mut()
            .filter(|f| !matches!(f.stage, Stage::Overlay | Stage::Composite) && f.enabled)
        {
            feature.activity = Activity::Idle;
        }
        let needs_composite =
            source.is_some() || composite_needed(&self.registry, &resolved, &view);
        let mut io = presentation_io(&device, target, state.composite.take(), needs_composite);
        let mut stats = DrawStats::default();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("engine late presentation"),
        });
        let result = (|| -> Result<()> {
            if let Some(source) = source {
                let output = io.output.as_ref().context("no presentation output")?;
                self.composite_copy.encode(
                    &device,
                    &gpu.queue,
                    &mut encoder,
                    source.view,
                    &output.view,
                    output.format,
                    Viewport::full(size),
                )?;
            } else if io.final_output.is_some() {
                let output = io.output.as_ref().context("no presentation output")?;
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("engine clear presentation"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &output.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
            }
            for stage in [Stage::Overlay, Stage::Composite] {
                run_presentation(
                    stage,
                    gpu,
                    layouts,
                    &mut self.registry,
                    state.lighting.bind_group(),
                    &view,
                    world,
                    &mut encoder,
                    &mut io,
                    &mut resolved,
                    &mut stats,
                    host,
                )?;
            }
            finish_presentation(&mut self.composite_copy, gpu, &mut encoder, &io)
        })();
        gpu.queue.submit([encoder.finish()]);
        state.composite = io.composite.take();
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

    /// Pin presentation data before producing a UI-free scene or generated
    /// frames. The host must end the sequence on every Result path.
    pub fn begin_presentation_sequence(&mut self) -> Result<()> {
        ensure!(
            !self.presentation_sequence,
            "presentation sequence already active"
        );
        self.presentation_sequence = true;
        self.presentation_sequence_features.clear();
        let result = (|| {
            for (index, entry) in self.registry.features.iter_mut().enumerate() {
                if matches!(entry.descriptor.stage, Stage::Overlay | Stage::Composite)
                    && let FeatureBody::Owned(feature) = &mut entry.body
                {
                    // Include the attempted feature so an error can release its
                    // partially acquired snapshot; never end an unstarted one.
                    self.presentation_sequence_features.push(index);
                    feature.begin_presentation_sequence()?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.end_presentation_sequence();
        }
        result
    }

    pub fn end_presentation_sequence(&mut self) {
        if !self.presentation_sequence {
            return;
        }
        for index in std::mem::take(&mut self.presentation_sequence_features)
            .into_iter()
            .rev()
        {
            if let FeatureBody::Owned(feature) = &mut self.registry.features[index].body {
                feature.end_presentation_sequence();
            }
        }
        self.presentation_sequence = false;
    }

    /// Share a snapshot across all outputs, or join the host's existing
    /// sequence. Releases a locally created sequence on success and error.
    pub fn with_presentation_sequence<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if self.presentation_sequence {
            return body(self);
        }
        self.begin_presentation_sequence()?;
        let result = body(self);
        self.end_presentation_sequence();
        result
    }

    /// Names of running presentation extensions that have not opted into
    /// replay on generated frames. Disabled features do not block generation.
    pub fn presentation_blockers(&self, overrides: &crate::FeatureOverrides) -> Vec<String> {
        self.registry.presentation_blockers(overrides)
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

    /// The auto-exposure state of `view`'s last frame: the adapted and the
    /// target exposure value (log2 of the exposure scale) and the scale.
    /// `None` when the view never ran auto-exposure. Blocks on the GPU.
    pub fn auto_exposure(&self, view: ViewId) -> Result<Option<AutoExposureState>> {
        let Some(buffer) = self
            .post
            .readbacks
            .lock()
            .expect("post readbacks lock")
            .get(&view)
            .cloned()
        else {
            return Ok(None);
        };
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.gpu.wait_idle()?;
        receiver
            .recv()
            .context("auto exposure readback was not mapped")?
            .context("mapping the auto exposure readback")?;
        let values: [f32; 4] = {
            let mapped = slice
                .get_mapped_range()
                .map_err(|error| anyhow::anyhow!("reading auto exposure: {error:?}"))?;
            bytemuck::pod_read_unaligned(&mapped[..16])
        };
        buffer.unmap();
        Ok((values[3] > 0.5).then_some(AutoExposureState {
            scale: values[0],
            ev: values[1],
            target_ev: values[2],
        }))
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

/// Auto-exposure of one view: `scale = 2^ev`, adapted toward `target_ev`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoExposureState {
    pub scale: f32,
    pub ev: f32,
    pub target_ev: f32,
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
    if let Some(screen) = &targets.screen_lighting {
        // Two passes: four Rgba16Float attachments alone reach the 32-byte
        // per-sample colour attachment limit.
        let [shadow, ao, indirect, reflection] = screen.targets();
        let color = |v: [f32; 4]| wgpu::Color {
            r: f64::from(v[0]),
            g: f64::from(v[1]),
            b: f64::from(v[2]),
            a: f64::from(v[3]),
        };
        let d = crate::targets::SCREEN_LIGHTING_DEFAULTS;
        for pair in [
            [(shadow, d[0]), (ao, d[1])],
            [(indirect, d[2]), (reflection, d[3])],
        ] {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("engine clear screen lighting"),
                color_attachments: &[
                    clear(&pair[0].0.view, color(pair[0].1)),
                    clear(&pair[1].0.view, color(pair[1].1)),
                ],
                ..Default::default()
            });
        }
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
    lighting: &'a LightingRenderer,
    state: &'a mut ViewState,
    view: &'a ViewData,
    frame: &'a SceneFrame<'f>,
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
    if stage == Stage::Shadow {
        run.state.lighting.clear_shadows(encoder);
    }
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
                        run.state.lighting.shadow_group()
                    } else {
                        run.state.lighting.bind_group()
                    },
                    layouts: run.layouts,
                    world: run.frame.world,
                    shadows: Some(&run.state.lighting),
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
    if name == names::CLUSTERS {
        if !run.state.lighting.dispatch {
            return Ok(false);
        }
        run.lighting
            .encode_clusters(encoder, &mut run.state.lighting);
        return Ok(true);
    }
    let state = &*run.state;
    let view_group = state.lighting.bind_group();
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
            let draws = &state.lighting.draws;
            if draws.is_empty() {
                return Ok(false);
            }
            let atlas = state.lighting.atlas();
            for (layer, target) in atlas.layer_views.iter().enumerate() {
                if !draws.iter().any(|d| d.layer == layer as u32) {
                    continue;
                }
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("engine shadow atlas page"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: target,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                for draw in draws.iter().filter(|d| d.layer == layer as u32) {
                    let size = draw.size as f32;
                    pass.set_viewport(draw.x as f32, draw.y as f32, size, size, 0.0, 1.0);
                    pass.set_scissor_rect(draw.x, draw.y, draw.size, draw.size);
                    *stats += run.mesh.draw_shadow(
                        &mut pass,
                        state.lighting.shadow_group(),
                        &state.mesh,
                        draw.index,
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

fn composite_needed(
    registry: &FeatureRegistry,
    resolved: &ResolvedFeatures,
    view: &ViewData,
) -> bool {
    registry.features.iter().any(|entry| {
        entry.descriptor.stage == Stage::Composite
            && resolved.is_enabled(&entry.descriptor.name)
            && match &entry.body {
                FeatureBody::Owned(feature) => feature.composite_active(view),
                _ => false,
            }
    })
}

fn presentation_io(
    device: &wgpu::Device,
    target: &OutputTarget,
    cached: Option<CompositeTargets>,
    active: bool,
) -> FrameIo {
    let mut io = FrameIo {
        output: Some(target.clone()),
        composite: cached,
        ..Default::default()
    };
    if active {
        let targets = match io.composite.take() {
            Some(targets) if targets.size == target.size && targets.format == target.format => {
                targets
            }
            _ => CompositeTargets::new(device, target.size, target.format),
        };
        io.output.as_mut().expect("set above").view = targets.colors[0].view.clone();
        io.final_output = Some(target.clone());
        io.composite = Some(targets);
    }
    io
}

fn finish_presentation(
    copy: &mut OutputPass,
    gpu: &GpuContext,
    encoder: &mut wgpu::CommandEncoder,
    io: &FrameIo,
) -> Result<()> {
    if let Some(target) = &io.final_output {
        let source = &io.output.as_ref().context("no composite source")?.view;
        copy.encode(
            &gpu.device,
            &gpu.queue,
            encoder,
            source,
            &target.view,
            target.format,
            Viewport::full(target.size),
        )?;
    }
    Ok(())
}
