//! Native rendering for games, editor viewports and offscreen tests.
//!
//! * [`GpuRenderer`] (this file): 3D scenes through `engine-render-pipeline`
//!   (HDR stages, render features, DLSS host hooks) and 2D sprites/UI
//!   through a [`SpriteRenderer`] sharing the caller's [`GpuContext`]. The
//!   sprites run as the pipeline's `engine.sprites` overlay node.
//! * [`SpriteRenderer`] (`sprite.rs`): backend-neutral conversion of a
//!   [`RenderFrame`] into `engine-render` draw commands.
//! * `scene3d.rs`: engine scene data to pipeline inputs and CPU validation.
//! * `material2d.rs`: material shaders on sprites.
//! * `cache.rs`: Arc-keyed mesh/texture handle caches with eviction.
//! * `vertex.rs`: the 48-byte vertex record and its neutral layout.

mod cache;
mod framegen;
mod material2d;
mod post;
mod post_effects;
mod rt;
mod scene3d;
mod sprite;
mod temporal;
mod upscale;
mod vertex;

pub use framegen::{MAX_GENERATION_GAP, MAX_PRESENT_SPACING, PresentPacer, presentation_offsets};
pub use rt::{effects_settings, prepare_ray_tracing, request_ray_tracing, rt_names};
pub use scene3d::{LIT_WGSL, camera_matrix};
pub use sprite::{SpriteRenderer, TEXTURED_WGSL};
pub use temporal::{halton, jitter_matrix, jitter_offset, jitter_phase_count, pixel_motion};

use crate::{GpuContext, RenderTargetInfo, UpscalerDevice};
use anyhow::{Context, Result, ensure};
use engine_core::bevy_ecs::world::World;
use engine_project::lights::LightingStatus;
use engine_project::post::PostStatus;
use engine_project::rendering3d::{
    FrameGenerationMode, FrameGenerationSettings, FrameGenerationStatus, RenderFeatureView,
    RenderPipelineState, RenderPipelineStatus, SceneRenderFrame, TextureStreamMode, UpscalerMode,
    UpscalerSettings, UpscalerStatus,
};
use engine_project::runtime::RenderFrame;
use engine_render::{DrawCommand, RenderCommandBuffer, RenderTarget};
#[cfg(any(feature = "dlss", test))]
use engine_render_pipeline::Presentation;
use engine_render_pipeline::{
    FeatureOverrides, GpuTexture, HostStages, HostUpscale, LightingReport, OutputTarget,
    OverlayBatch, OverlayItem, PipelineFeatures, PresentationInput, SceneFrame, ScenePipeline,
    SceneTextures, Stage, ViewData, ViewId, ViewKind, ViewReport, Viewport, names,
};
use engine_render_post_effects::{
    DepthOfFieldFeature, MotionBlurFeature, PostEffects, SpatialAaFeature,
};
use engine_render_wgpu::{WgpuBackend, WgpuOptions};
use glam::Mat4;
use std::sync::Arc;

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
    /// Consecutive compatible pipeline/texture/camera groups of sprites plus
    /// instanced 3D draw calls; painter order is never changed merely to
    /// reduce binding changes.
    pub batches: usize,
    pub mesh_uploads: usize,
    pub texture_uploads: usize,
    pub cached_meshes: usize,
    pub cached_textures: usize,
}

/// One XR eye (or any externally owned view target): a caller-supplied
/// camera transform and projection rendered into `target`.
pub struct EyeTarget<'a> {
    /// Stable id of this eye's history (`ViewId::LEFT_EYE`/`RIGHT_EYE`).
    pub view: ViewId,
    pub world_from_view: Mat4,
    /// View-to-clip projection (WebGPU depth 0..1), for example an OpenXR
    /// asymmetric field of view.
    pub clip_from_view: Mat4,
    pub target: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub size: [u32; 2],
}

/// One UI-free presentation image and its final output target. Source and
/// target may be the same image; late composition snapshots the source first.
pub struct PresentationOutput<'a> {
    pub source: PresentationInput<'a>,
    pub target: &'a OutputTarget,
}

/// A renderer usable by a game window, editor viewport or offscreen test. CPU
/// assets are immutable Arc snapshots: retaining the same Arc means unchanged;
/// publish a new Arc for edits. GPU resources retain the Arc to prevent address
/// reuse and are evicted after their last frame reference.
///
/// A frame with a 3D camera renders through the [`ScenePipeline`]: HDR
/// scene colour, registered render features ([`Self::set_features`]), the
/// output operator into the target, then 2D sprites/UI (the
/// `engine.sprites` overlay node) and overlay features. Frames without a 3D
/// camera draw sprites directly (plus overlay features when registered).
///
/// A 3D camera whose `upscaler` requests DLSS Super Resolution renders through
/// the temporal stage (see [`Self::upscaler_status`]) when the device was
/// prepared by [`crate::request_upscaler_gpu`]; otherwise the frame renders
/// natively and the status reports why.
pub struct GpuRenderer {
    pub gpu: GpuContext,
    info: RenderTargetInfo,
    sprites: SpriteRenderer<WgpuBackend>,
    scene: Option<ScenePipeline>,
    uploads: scene3d::SceneUploads,
    #[cfg_attr(not(feature = "dlss"), allow(dead_code))]
    depth_export: Option<Arc<engine_render_pipeline::DepthExport>>,
    status: RenderPipelineStatus,
    lighting: LightingStatus,
    post: PostStatus,
    post_host: post::PostHost,
    post_effects: PostEffects,
    rt: rt::RtHost,
    /// A camera cut requested for the next native view (`reset_history`).
    native_reset: bool,
    upscaler: upscale::Upscaler,
    framegen: framegen::FrameGeneration,
}

/// How the `engine.sprites` overlay node draws a frame's 2D sprites/UI.
enum SpriteMode {
    /// Over the composed 3D output (preserving it).
    Over(OrderedSprites),
    /// Into an empty target (2D-only frames; clears it).
    Alone(OrderedSprites),
    /// Not at all (HUD-less frame generation input, XR eyes).
    Skip,
}

struct OrderedSprites {
    draws: Vec<DrawCommand>,
    painter: Vec<OverlayItem>,
    viewport: engine_render::PixelRect,
    stats: RenderStats,
}
impl From<sprite::BuiltFrame> for OrderedSprites {
    fn from(frame: sprite::BuiltFrame) -> Self {
        Self {
            draws: frame.commands.into_ordered(),
            painter: frame.painter,
            viewport: frame.viewport,
            stats: frame.stats,
        }
    }
}

type UpscaleHook<'a> = dyn FnMut(&GpuContext, &SceneTextures<'_>) -> Result<HostUpscale> + 'a;

/// The desktop side of the pipeline's host hooks.
struct DesktopHost<'a, 'h> {
    sprites: &'a mut SpriteRenderer<WgpuBackend>,
    mode: SpriteMode,
    stats: RenderStats,
    /// External stream textures to copy into the sprite textures standing in
    /// for them before the sprites draw.
    stream_copies: Vec<(Arc<GpuTexture>, Arc<engine_project::content::Texture>)>,
    upscale: Option<&'h mut UpscaleHook<'h>>,
}

impl HostStages for DesktopHost<'_, '_> {
    fn upscale(&mut self, gpu: &GpuContext, scene: &SceneTextures<'_>) -> Result<HostUpscale> {
        match &mut self.upscale {
            Some(hook) => hook(gpu, scene),
            None => Ok(HostUpscale::NotHandled),
        }
    }

    fn overlay_items(&mut self, node: &str, _view: &ViewData) -> Result<Option<Vec<OverlayItem>>> {
        if node != names::SPRITES {
            return Ok(None);
        }
        Ok(Some(match &self.mode {
            SpriteMode::Over(built) | SpriteMode::Alone(built) => built.painter.clone(),
            SpriteMode::Skip => vec![],
        }))
    }

    fn begin_overlay(&mut self, gpu: &GpuContext, target: &OutputTarget) -> Result<()> {
        copy_streams(self.sprites, gpu, &self.stream_copies);
        if let SpriteMode::Alone(built) = &self.mode {
            let mut desc = engine_render::FrameDesc::new(RenderTarget::Surface);
            desc.viewport = Some(built.viewport);
            submit_to(
                self.sprites,
                &target.view,
                RenderTargetInfo {
                    width: target.size[0],
                    height: target.size[1],
                    format: target.format,
                },
                desc,
                RenderCommandBuffer::new(),
            )?;
        }
        Ok(())
    }

    fn overlay_batch(
        &mut self,
        node: &str,
        _gpu: &GpuContext,
        target: &OutputTarget,
        batch: OverlayBatch<'_>,
    ) -> Result<()> {
        if node != names::SPRITES {
            return Ok(());
        }
        let built = match &self.mode {
            SpriteMode::Over(built) | SpriteMode::Alone(built) => built,
            SpriteMode::Skip => return Ok(()),
        };
        let mut commands = RenderCommandBuffer::new();
        for &token in batch.tokens {
            commands.record(
                built
                    .draws
                    .get(token as usize)
                    .context("sprite painter token is not in the retained frame")?
                    .clone(),
            )?;
        }
        let mut desc = engine_render::FrameDesc::new(RenderTarget::Surface);
        desc.viewport = Some(built.viewport);
        desc.preserve_color = true;
        submit_to(
            self.sprites,
            &target.view,
            RenderTargetInfo {
                width: target.size[0],
                height: target.size[1],
                format: target.format,
            },
            desc,
            commands,
        )?;
        if batch.first {
            sprite::add_stats(&mut self.stats, built.stats);
        }
        Ok(())
    }

    fn overlay(&mut self, node: &str, gpu: &GpuContext, target: &OutputTarget) -> Result<()> {
        self.begin_overlay(gpu, target)?;
        let items = self
            .overlay_items(
                node,
                &ViewData::new(
                    ViewId::MAIN,
                    Mat4::IDENTITY,
                    Mat4::IDENTITY,
                    target.size,
                    target.size,
                )?,
            )?
            .unwrap_or_default();
        let tokens: Vec<_> = items.iter().map(|item| item.token).collect();
        self.overlay_batch(
            node,
            gpu,
            target,
            OverlayBatch {
                tokens: &tokens,
                first: true,
                last: true,
            },
        )
    }
}

/// Copy external stream textures into the sprite textures uploaded from
/// their placeholders, so sprites sample the caller-written texels.
fn copy_streams(
    sprites: &SpriteRenderer<WgpuBackend>,
    gpu: &GpuContext,
    copies: &[(Arc<GpuTexture>, Arc<engine_project::content::Texture>)],
) {
    if copies.is_empty() {
        return;
    }
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    for (stream, placeholder) in copies {
        let destination = sprites
            .texture_handle(placeholder)
            .and_then(|handle| sprites.device().backend().texture(handle));
        if let Some(destination) = destination
            && destination.size() == stream.texture.size()
        {
            encoder.copy_texture_to_texture(
                stream.texture.as_image_copy(),
                destination.as_image_copy(),
                stream.texture.size(),
            );
        }
    }
    gpu.queue.submit([encoder.finish()]);
}

impl GpuRenderer {
    pub fn new(gpu: GpuContext, info: RenderTargetInfo, limits: RenderLimits) -> Result<Self> {
        ensure!(
            info.width > 0 && info.height > 0,
            "render target must be nonzero"
        );
        sprite::validate_limits(&limits)?;
        let max = gpu.device.limits().max_texture_dimension_2d;
        ensure!(
            info.width <= max && info.height <= max,
            "render target exceeds device limit"
        );
        let backend = WgpuBackend::from_context_with(
            gpu.clone(),
            WgpuOptions {
                frames_in_flight: limits.max_frames_in_flight as u32,
                ..Default::default()
            },
        );
        let mut sprites = SpriteRenderer::new(backend, limits)?;
        sprites.set_lighting_limits(gpu.device.limits().clone());
        sprites.set_surface_size((info.width, info.height));
        gpu.check_errors()?;
        Ok(Self {
            gpu,
            info,
            sprites,
            scene: None,
            uploads: Default::default(),
            depth_export: None,
            status: RenderPipelineStatus::default(),
            lighting: LightingStatus::default(),
            post: PostStatus::default(),
            post_host: post::PostHost::default(),
            post_effects: PostEffects::default(),
            rt: rt::RtHost::default(),
            native_reset: false,
            upscaler: upscale::Upscaler::new(UpscalerDevice::default()),
            framegen: framegen::FrameGeneration::new(),
        })
    }

    /// [`Self::new`] on a device from [`crate::request_upscaler_gpu`], which
    /// records whether DLSS can run on it.
    pub fn with_upscaler_device(
        gpu: GpuContext,
        info: RenderTargetInfo,
        limits: RenderLimits,
        device: UpscalerDevice,
    ) -> Result<Self> {
        let mut renderer = Self::new(gpu, info, limits)?;
        renderer.framegen.clear();
        renderer.upscaler.set_device(device);
        Ok(renderer)
    }

    /// What the upscaler did for the last rendered 3D frame.
    pub fn upscaler_status(&self) -> &UpscalerStatus {
        self.upscaler.status()
    }

    /// What frame generation did for the last rendered 3D frame.
    pub fn frame_generation_status(&self) -> &FrameGenerationStatus {
        self.framegen.status()
    }

    /// Registered render features and their state in the last camera view
    /// (or overlay-only frame).
    pub fn pipeline_status(&self) -> &RenderPipelineStatus {
        &self.status
    }

    /// Light culling, clustering and shadow allocation of the last camera
    /// view.
    pub fn lighting_status(&self) -> &LightingStatus {
        &self.lighting
    }

    /// The effective post-processing of the last camera view: influencing
    /// volumes, the settings summary and any LUT error.
    pub fn post_status(&self) -> &PostStatus {
        &self.post
    }

    /// Ray-traced lighting of the last camera view: the tier and, per
    /// effect, `ray_traced` / `fallback` / `disabled` with the reason; `None`
    /// while no frame enabled an `engine.rt.*` feature.
    pub fn ray_tracing_status(&self) -> Option<&engine_project::raytracing::RayTracingReport> {
        self.rt.report()
    }

    /// [`Self::ray_tracing_status`] in `engine-render-rt`'s types.
    pub fn rt_effects_status(&self) -> Option<engine_render_rt::RtEffectsStatus> {
        self.rt.effects_status()
    }

    /// Record the outcome of [`crate::prepare_ray_tracing`] for this
    /// renderer's device: an `Err` (the adapter cannot run the hardware tier)
    /// becomes the fallback reason of every ray-traced effect.
    pub fn note_ray_tracing_request(
        &mut self,
        request: Option<std::result::Result<(), engine_render_rt::RayTracingStatus>>,
    ) {
        if let Some(Err(status)) = request {
            self.rt.set_unavailable(status);
        }
    }

    /// Discard upscaler and frame generation history before the next frame
    /// (camera cut, teleport, scene transition, resume after a pause). Resizes
    /// and quality changes reset automatically.
    pub fn reset_temporal_history(&mut self) {
        self.native_reset = true;
        self.upscaler.request_reset();
        self.framegen.request_reset();
    }

    pub fn target_info(&self) -> RenderTargetInfo {
        self.info
    }

    /// The 3D render pipeline, created on first use.
    pub fn pipeline(&mut self) -> Result<&mut ScenePipeline> {
        if self.scene.is_none() {
            let mut scene =
                ScenePipeline::new(self.gpu.clone()).context("creating the 3D render pipeline")?;
            scene.register(DepthOfFieldFeature::new(self.post_effects.clone()))?;
            scene.register(MotionBlurFeature::new(self.post_effects.clone()))?;
            scene.register(SpatialAaFeature::new(self.post_effects.clone()))?;
            self.scene = Some(scene);
        }
        Ok(self.scene.as_mut().expect("created above"))
    }

    /// Prepare and add render features (for example from a
    /// [`PipelineFeatures`] resource). Names must not clash with registered
    /// features; nothing is added on failure.
    pub fn set_features(&mut self, features: PipelineFeatures) -> Result<()> {
        if features.0.is_empty() {
            return Ok(());
        }
        self.pipeline()?.add_features(features.0)?;
        self.gpu.check_errors()
    }

    /// Whether registered (non-built-in) overlay features exist.
    fn has_overlay_features(&self) -> bool {
        self.scene.as_ref().is_some_and(|scene| {
            scene.registry().descriptors().any(|d| {
                matches!(d.stage, Stage::Overlay | Stage::Composite) && d.name != names::SPRITES
            })
        })
    }

    fn presentation_blocker(&self, frame: &SceneRenderFrame) -> Option<String> {
        let overrides = FeatureOverrides(
            frame
                .camera3d
                .as_ref()
                .map(|c| c.camera.effective_render_features())
                .unwrap_or_default(),
        );
        let blocked = self.scene.as_ref()?.presentation_blockers(&overrides);
        (!blocked.is_empty()).then(|| {
            format!(
                "presentation features do not support replay on generated frames: {}",
                blocked.join(", ")
            )
        })
    }

    /// Apply the same retained UI/overlay/filter snapshot to a sequence of
    /// externally produced display images. This is also the late Frame
    /// Generation path, and can be used with synthetic generated images.
    pub fn composite_presentations(
        &mut self,
        frame: &SceneRenderFrame,
        outputs: &[PresentationOutput<'_>],
        world: Option<&World>,
    ) -> Result<RenderStats> {
        let overrides = FeatureOverrides(
            frame
                .camera3d
                .as_ref()
                .map(|c| c.camera.effective_render_features())
                .unwrap_or_default(),
        );
        for output in outputs {
            ensure!(
                output.source.size == output.target.size,
                "presentation input/output sizes must match"
            );
            self.sprites
                .validate_scene(frame, (output.target.size[0], output.target.size[1]))?;
        }
        self.pipeline()?;
        let copies = stream_copies(frame, &self.uploads.streams);
        let scene = self.scene.as_mut().expect("created above");
        let sprites = &mut self.sprites;
        let mut last_report = None;
        let stats = scene.with_presentation_sequence(&overrides, |scene| {
            let mut stats = RenderStats::default();
            for output in outputs {
                let built =
                    sprites.build_overlay(frame, (output.target.size[0], output.target.size[1]))?;
                let mut host = DesktopHost {
                    sprites: &mut *sprites,
                    mode: SpriteMode::Over(built.into()),
                    stats: RenderStats::default(),
                    stream_copies: copies.clone(),
                    upscale: None,
                };
                last_report = Some(scene.render_overlay_over(
                    PresentationInput {
                        view: output.source.view,
                        size: output.source.size,
                    },
                    output.target,
                    world,
                    &overrides,
                    &mut host,
                )?);
                sprite::add_stats(&mut stats, host.stats);
            }
            Ok(stats)
        })?;
        if let Some(report) = last_report {
            let late = status_of(&report, RenderPipelineState::Rendered, &overrides.0);
            if self.status.state == RenderPipelineState::Rendered {
                for feature in late
                    .features
                    .into_iter()
                    .filter(|f| matches!(f.stage.as_str(), "overlay" | "composite"))
                {
                    if let Some(current) = self
                        .status
                        .features
                        .iter_mut()
                        .find(|f| f.name == feature.name)
                    {
                        *current = feature;
                    } else {
                        self.status.features.push(feature);
                    }
                }
            } else {
                self.status = late;
            }
        }
        self.gpu.check_errors()?;
        Ok(stats)
    }

    /// The persistent GPU texture of the external stream `name` (created at
    /// `size` on first use and when the size changes). Write into it (copy,
    /// `queue.write_texture` or render) between frames; materials and
    /// sprites referencing `stream:<name>` sample it.
    pub fn stream_texture(&mut self, name: &str, size: [u32; 2]) -> Result<Arc<GpuTexture>> {
        self.uploads.streams.external(&self.gpu.device, name, size)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        ensure!(
            width > 0 && height > 0,
            "zero size is a suspended surface, not a render target"
        );
        let max = self.gpu.device.limits().max_texture_dimension_2d;
        ensure!(
            width <= max && height <= max,
            "render target exceeds device limit"
        );
        self.gpu.wait_idle()?;
        self.info.width = width;
        self.info.height = height;
        self.sprites.set_surface_size((width, height));
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
        self.render_scene(&frame.clone().into(), target, world)
    }

    /// Render one frame into `target`. With frame generation active the real
    /// frame is drawn and the generated frames stay available through
    /// [`Self::capture_generated`]; windowed hosts present them in order with
    /// [`Self::render_generated`] instead.
    pub fn render_scene(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
    ) -> Result<RenderStats> {
        if let Some(stats) = self.render_generated_with_world(frame, world)? {
            self.present_real(target)?;
            return Ok(stats);
        }
        self.render_scene_without_generation(frame, target, world)
    }

    /// [`Self::render_scene`] for a frame [`Self::render_generated`] declined:
    /// Super Resolution or native rendering.
    pub fn render_scene_without_generation(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
    ) -> Result<RenderStats> {
        self.gpu.check_errors()?;
        let request = frame.camera3d.as_ref().map(|c| c.camera.upscaler);
        let stats = match request {
            None => {
                let stats = self.render_2d(frame, target, world)?;
                self.upscaler.native(UpscalerSettings::default());
                stats
            }
            Some(request) if request.mode == UpscalerMode::DlssSr => {
                match self.render_upscaled(frame, target, world, request)? {
                    Some(stats) => stats,
                    None => self.render_native(frame, target, world)?,
                }
            }
            Some(request) => {
                let stats = self.render_native(frame, target, world)?;
                self.upscaler.native(request);
                stats
            }
        };
        self.gpu.check_errors()?;
        Ok(stats)
    }

    fn output_target(
        &self,
        target: &wgpu::TextureView,
        viewport: crate::PixelRect,
    ) -> OutputTarget {
        OutputTarget {
            view: target.clone(),
            format: self.info.format,
            size: [self.info.width, self.info.height],
            viewport: Viewport {
                x: viewport.x,
                y: viewport.y,
                width: viewport.width,
                height: viewport.height,
            },
        }
    }

    /// Sprites only (no 3D camera), plus registered overlay features.
    fn render_2d(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
    ) -> Result<RenderStats> {
        self.post_effects.set_dof(ViewId::MAIN, None)?;
        self.post_effects.set_motion_blur(ViewId::MAIN, None)?;
        self.post_effects.set_spatial_aa(ViewId::MAIN, None)?;
        let size = (self.info.width, self.info.height);
        self.sprites.validate_scene(frame, size)?;
        let built = self.sprites.build_overlay(frame, size)?;
        if !self.has_overlay_features() {
            copy_streams(
                &self.sprites,
                &self.gpu,
                &stream_copies(frame, &self.uploads.streams),
            );
            let desc = built.desc(RenderTarget::Surface);
            submit_to(&mut self.sprites, target, self.info, desc, built.commands)?;
            self.status = RenderPipelineStatus::default();
            return Ok(built.stats);
        }
        let output = OutputTarget {
            view: target.clone(),
            format: self.info.format,
            size: [size.0, size.1],
            viewport: Viewport::full([size.0, size.1]),
        };
        let copies = stream_copies(frame, &self.uploads.streams);
        let mut host = DesktopHost {
            sprites: &mut self.sprites,
            mode: SpriteMode::Alone(built.into()),
            stats: RenderStats::default(),
            stream_copies: copies,
            upscale: None,
        };
        let scene = self
            .scene
            .as_mut()
            .context("overlay features require the render pipeline")?;
        let report =
            scene.render_overlay_only(&output, world, &FeatureOverrides::default(), &mut host)?;
        let stats = host.stats;
        self.status = status_of(&report, RenderPipelineState::Rendered, &Default::default());
        Ok(stats)
    }

    /// The 3D scene at output resolution, then sprites/UI.
    fn render_native(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
    ) -> Result<RenderStats> {
        let camera = frame
            .camera3d
            .as_ref()
            .context("a 3D frame needs a Camera3D")?;
        let viewport = self
            .sprites
            .scene_viewport(frame, (self.info.width, self.info.height))?;
        let size = [viewport.width, viewport.height];
        let mut view = scene3d::camera_view(camera, ViewId::MAIN, size, size)?;
        // A requested camera cut discards the view's temporal history
        // (motion, RT denoisers).
        view.reset_history = std::mem::take(&mut self.native_reset);
        let output = self.output_target(target, viewport);
        let (_, stats) = self.draw_3d(frame, view, &output, world, true, None)?;
        Ok(stats)
    }

    /// Validate, upload and render one 3D view into `output`; the sprites
    /// overlay too when `sprites`. `upscale` is the host upscale hook.
    fn draw_3d<'h>(
        &mut self,
        frame: &SceneRenderFrame,
        view: ViewData,
        output: &OutputTarget,
        world: Option<&World>,
        sprites: bool,
        upscale: Option<&'h mut UpscaleHook<'h>>,
    ) -> Result<(ViewReport, RenderStats)> {
        let size = (output.size[0], output.size[1]);
        // Every check runs before any upload or submission.
        self.sprites.validate_scene(frame, size)?;
        let lights = scene3d::light_list(frame)?;
        let mode = if sprites {
            SpriteMode::Over(self.sprites.build_overlay(frame, size)?.into())
        } else {
            SpriteMode::Skip
        };
        self.pipeline()?;
        let mut view = view;
        let post_status = self.post_host.prepare(frame, &mut view)?;
        post_effects::prepare(&self.post_effects, frame, &mut view)?;
        self.rt.prepare(
            &self.gpu,
            self.scene.as_mut().expect("created above"),
            frame,
            &lights,
            &mut view,
        )?;
        let Self {
            gpu,
            sprites: sprite_renderer,
            scene,
            uploads,
            status,
            lighting,
            post,
            rt,
            ..
        } = self;
        let scene = scene.as_mut().expect("created above");
        let inputs = uploads.sync(gpu, frame)?;
        let copies = stream_copies(frame, &uploads.streams);
        let mut host = DesktopHost {
            sprites: sprite_renderer,
            mode,
            stats: RenderStats::default(),
            stream_copies: copies,
            upscale,
        };
        let report = scene.render_view(
            &SceneFrame {
                view: &view,
                meshes: &inputs.meshes,
                lights: &lights,
                world,
            },
            output,
            &mut host,
        )?;
        let mut stats = inputs.stats;
        stats.draws += report.draws;
        stats.batches += report.draw_calls;
        sprite::add_stats(&mut stats, host.stats);
        let overrides = frame
            .camera3d
            .as_ref()
            .map(|c| c.camera.effective_render_features())
            .unwrap_or_default();
        if view.kind == ViewKind::Camera {
            *status = status_of(&report, RenderPipelineState::Rendered, &overrides);
            *lighting = lighting_status_of(&report.lighting, frame.lights.len());
            *post = post_status;
            rt.finish(&report);
        }
        Ok((report, stats))
    }

    /// Render `frame`'s 3D scene once per eye into externally owned targets
    /// (OpenXR swapchain images, or any texture), each with its own view and
    /// projection and its own history. 2D sprites/UI are not drawn into the
    /// eyes (an XR host composes UI as its own layer); overlay features run.
    /// Lighting and feature overrides come from the frame's 3D camera.
    pub fn render_eyes(
        &mut self,
        frame: &SceneRenderFrame,
        eyes: &[EyeTarget<'_>],
        world: Option<&World>,
    ) -> Result<Vec<RenderStats>> {
        self.gpu.check_errors()?;
        let mut stats = Vec::with_capacity(eyes.len());
        for eye in eyes {
            ensure!(
                eye.view != ViewId::MAIN,
                "eye views need their own ids (ViewId::LEFT_EYE, ViewId::RIGHT_EYE)"
            );
            let mut view = ViewData::new(
                eye.view,
                eye.world_from_view,
                eye.clip_from_view,
                eye.size,
                eye.size,
            )?;
            view.kind = ViewKind::XrEye;
            if let Some(camera) = &frame.camera3d {
                view.ambient = glam::Vec3::splat(camera.camera.lighting.ambient);
                view.features = FeatureOverrides(camera.camera.effective_render_features());
            }
            let output = OutputTarget {
                view: eye.target.clone(),
                format: eye.format,
                size: eye.size,
                viewport: Viewport::full(eye.size),
            };
            let (_, eye_stats) = self.draw_3d(frame, view, &output, world, false, None)?;
            stats.push(eye_stats);
        }
        self.gpu.check_errors()?;
        Ok(stats)
    }

    /// `Ok(None)`: the request could not run; the status holds the fallback
    /// reason and the caller renders natively. Invalid frame data is an error,
    /// exactly as on the native path.
    #[cfg(not(feature = "dlss"))]
    fn render_upscaled(
        &mut self,
        _: &SceneRenderFrame,
        _: &wgpu::TextureView,
        _: Option<&World>,
        request: UpscalerSettings,
    ) -> Result<Option<RenderStats>> {
        let reason = self.upscaler.precheck(request, [0, 0]).unwrap_or_default();
        self.upscaler.fallback(request, &reason);
        Ok(None)
    }

    #[cfg(feature = "dlss")]
    fn render_upscaled(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
        request: UpscalerSettings,
    ) -> Result<Option<RenderStats>> {
        let viewport = self
            .sprites
            .scene_viewport(frame, (self.info.width, self.info.height))?;
        let output = [viewport.width, viewport.height];
        if let Some(reason) = self.upscaler.precheck(request, output) {
            self.upscaler.fallback(request, &reason);
            return Ok(None);
        }
        let mut stage = self.take_stage();
        let output_target = self.output_target(target, viewport);
        let result = self.super_resolve(
            &mut stage,
            frame,
            request,
            &output_target,
            world,
            true,
            None,
        );
        self.upscaler.dlss = Some(stage);
        result.map(|scene| scene.map(|scene| scene.stats))
    }

    /// The shared NGX stage, created on first use (callers checked the device).
    #[cfg(feature = "dlss")]
    fn take_stage(&mut self) -> upscale::dlss_stage::DlssStage {
        use crate::dlss::DeviceSupport;
        match (self.upscaler.dlss.take(), &self.upscaler.device.0) {
            (Some(stage), _) => stage,
            (None, DeviceSupport::Ready(paths)) => {
                upscale::dlss_stage::DlssStage::new(&self.gpu.device, paths.clone())
            }
            (None, _) => unreachable!("prechecks accept only a ready device"),
        }
    }

    /// The depth exporter, created on first use.
    #[cfg(feature = "dlss")]
    fn depth_export(&mut self) -> Result<Arc<engine_render_pipeline::DepthExport>> {
        if self.depth_export.is_none() {
            self.depth_export = Some(Arc::new(engine_render_pipeline::DepthExport::new(
                &self.gpu.device,
            )?));
        }
        Ok(self.depth_export.clone().expect("created above"))
    }

    /// Render the 3D scene at the NGX render size with jitter, evaluate
    /// Super Resolution on the HDR colour, depth and motion, and finish the
    /// frame (post, output, overlays) from the upscaled HDR into `output`.
    /// `Ok(None)` records the SR fallback reason (session setup or a failed
    /// evaluation, which drops the poisoned session); the caller then renders
    /// natively. `fg_inputs` also receives this frame's render-size depth
    /// (`R32Float` view) and motion (`Rg16Float` texture) for frame generation.
    #[cfg(feature = "dlss")]
    // Without NGX_SDK_DIR at build time engine-dlss session types are
    // uninhabited, so code after them is unreachable in that configuration.
    #[allow(unreachable_code, unused_variables, clippy::too_many_arguments)]
    fn super_resolve(
        &mut self,
        stage: &mut upscale::dlss_stage::DlssStage,
        frame: &SceneRenderFrame,
        request: UpscalerSettings,
        output_target: &OutputTarget,
        world: Option<&World>,
        sprites: bool,
        fg_inputs: Option<(wgpu::TextureView, wgpu::Texture)>,
    ) -> Result<Option<SrScene>> {
        let camera = frame
            .camera3d
            .as_ref()
            .context("an upscaled frame requires a Camera3D")?;
        let output = output_target.viewport.size();
        let new_session = match stage.ensure_session(
            &self.gpu.device,
            &self.gpu.queue,
            request.quality,
            output,
        ) {
            Ok(new) => new,
            Err(reason) => {
                self.upscaler.remember_failure(request, output, &reason);
                self.upscaler.fallback(request, &reason);
                return Ok(None);
            }
        };
        if new_session {
            self.upscaler.history.request_reset();
        }
        let (key, mut session) = stage.session.take().expect("session ensured above");
        let render = session.status().render_size;
        let reset = self.upscaler.history.needs_reset(camera);
        let jitter = jitter_offset(
            self.upscaler.history.frame(),
            jitter_phase_count(render, output),
        );
        let mut view = scene3d::camera_view(camera, ViewId::MAIN, render, output)?;
        view.jitter = jitter;
        view.reset_history = reset;
        view.host_needs.motion = true;
        if fg_inputs.is_some() {
            view.presentation = Presentation::SceneOnly;
        }
        let depth_export = self.depth_export()?;
        let now = std::time::Instant::now();
        let delta_ms = stage.last_evaluation.map_or(1000.0 / 60.0, |last| {
            (now.duration_since(last).as_secs_f32() * 1000.0).clamp(0.1, 1000.0)
        });
        let mut failure = None;
        let mut hook = |gpu: &GpuContext, scene: &SceneTextures<'_>| -> Result<HostUpscale> {
            let motion = scene
                .motion
                .context("the SR view requests motion vectors")?;
            let [color, depth, motion_input] = session.inputs().map(Clone::clone);
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_texture(
                scene.hdr.texture.as_image_copy(),
                color.texture().as_image_copy(),
                scene.hdr.texture.size(),
            );
            depth_export.encode(&gpu.device, &mut encoder, &scene.depth.view, &depth);
            encoder.copy_texture_to_texture(
                motion.texture.as_image_copy(),
                motion_input.texture().as_image_copy(),
                motion.texture.size(),
            );
            if let Some((fg_depth, fg_motion)) = &fg_inputs {
                depth_export.encode(&gpu.device, &mut encoder, &scene.depth.view, fg_depth);
                encoder.copy_texture_to_texture(
                    motion.texture.as_image_copy(),
                    fg_motion.as_image_copy(),
                    motion.texture.size(),
                );
            }
            gpu.queue.submit([encoder.finish()]);
            if let Err(error) = session.evaluate(engine_dlss::Frame {
                jitter,
                motion_scale: [1.0, 1.0],
                reset,
                delta_ms,
            }) {
                failure = Some(format!("DLSS evaluation failed: {error}"));
                return Ok(HostUpscale::Abort);
            }
            Ok(HostUpscale::Upscaled(session.output().clone()))
        };
        let drawn = self.draw_3d(frame, view, output_target, world, sprites, Some(&mut hook));
        let (report, stats) = match drawn {
            Ok(drawn) => drawn,
            Err(error) => {
                stage.session = Some((key, session));
                return Err(error);
            }
        };
        if let Some(reason) = failure {
            // A failed evaluation poisons the session (dropped here); retry only
            // after the request changes, and present this frame natively.
            self.upscaler.remember_failure(request, output, &reason);
            self.upscaler.fallback(request, &reason);
            return Ok(None);
        }
        ensure!(
            !report.aborted,
            "the frame was aborted without an SR failure"
        );
        let adapter = session.status().device_name.clone();
        stage.last_evaluation = Some(now);
        stage.session = Some((key, session));
        self.upscaler.evaluated += 1;
        if reset {
            self.upscaler.resets += 1;
        }
        self.upscaler.history.commit(camera);
        self.upscaler.active(request, render, output, adapter);
        Ok(Some(SrScene { stats, jitter }))
    }

    /// Render the real frame through DLSS Frame Generation when requested and
    /// supported: the scene (optionally super-resolved) into the engine's
    /// HUD-less color, plus 2D/UI into its backbuffer, then NGX generates the
    /// frames between the previous and this real frame. Hosts then present
    /// [`Self::present_generated`] for `0..generated_frame_count()` followed by
    /// [`Self::present_real`]. `Ok(None)`: not requested or unsupported (see
    /// [`Self::frame_generation_status`]); render with [`Self::render_scene`].
    pub fn render_generated(&mut self, frame: &SceneRenderFrame) -> Result<Option<RenderStats>> {
        self.render_generated_with_world(frame, None)
    }

    /// World-aware Frame Generation entry point; late replayable extensions
    /// retain the same extracted-world access as ordinary presentation.
    pub fn render_generated_with_world(
        &mut self,
        frame: &SceneRenderFrame,
        world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        self.gpu.check_errors()?;
        let request = frame
            .camera3d
            .as_ref()
            .map(|c| c.camera.frame_generation)
            .unwrap_or_default();
        if request.mode == FrameGenerationMode::Off {
            self.framegen.off(request);
            return Ok(None);
        }
        let viewport = self
            .sprites
            .scene_viewport(frame, (self.info.width, self.info.height))?;
        self.framegen.discard_frames();
        let output = [viewport.width, viewport.height];
        #[cfg(feature = "dlss")]
        let prior = self
            .framegen
            .stage
            .as_ref()
            .map_or((output, output), |s| (s.output, s.render));
        #[cfg(not(feature = "dlss"))]
        let prior = (output, output);
        // Known capability/blocker failures must fall back before acquiring any
        // presentation snapshots. In particular, a disabled extension's hook
        // cannot prevent native rendering on a device without FG support.
        if let Some(reason) = self.framegen.precheck(
            request,
            &self.upscaler.device,
            self.presentation_blocker(frame).as_deref(),
            prior,
        ) {
            self.framegen.fallback(request, &reason);
            return Ok(None);
        }
        let overrides = FeatureOverrides(
            frame
                .camera3d
                .as_ref()
                .map(|c| c.camera.effective_render_features())
                .unwrap_or_default(),
        );
        self.pipeline()?.begin_presentation_sequence(&overrides)?;
        let result = self.render_generated_frame(frame, request, viewport, world);
        self.scene
            .as_mut()
            .expect("created above")
            .end_presentation_sequence();
        let stats = result?;
        if stats.is_some() {
            self.framegen.viewport = [viewport.x, viewport.y, viewport.width, viewport.height];
            self.framegen.real_ready = true;
        }
        self.gpu.check_errors()?;
        Ok(stats)
    }

    #[cfg(not(feature = "dlss"))]
    fn render_generated_frame(
        &mut self,
        frame: &SceneRenderFrame,
        request: FrameGenerationSettings,
        viewport: crate::PixelRect,
        _world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        let output = [viewport.width, viewport.height];
        let reason = self
            .framegen
            .precheck(
                request,
                &self.upscaler.device,
                self.presentation_blocker(frame).as_deref(),
                (output, output),
            )
            .unwrap_or_default();
        self.framegen.fallback(request, &reason);
        Ok(None)
    }

    #[cfg(feature = "dlss")]
    fn render_generated_frame(
        &mut self,
        frame: &SceneRenderFrame,
        request: FrameGenerationSettings,
        viewport: crate::PixelRect,
        world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        let mut stage = self.take_stage();
        let result = self.generate_with_stage(&mut stage, frame, request, viewport, world);
        self.upscaler.dlss = Some(stage);
        result
    }

    #[cfg(feature = "dlss")]
    #[allow(unreachable_code, unused_variables)] // uninhabited without the SDK
    fn generate_with_stage(
        &mut self,
        stage: &mut upscale::dlss_stage::DlssStage,
        frame: &SceneRenderFrame,
        request: FrameGenerationSettings,
        viewport: crate::PixelRect,
        world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        let camera = frame
            .camera3d
            .as_ref()
            .context("frame generation requires a Camera3D")?;
        let output = [viewport.width, viewport.height];
        let (context, session_options) =
            match stage.ensure_context(&self.gpu.device, &self.gpu.queue, output) {
                Ok(context) => context,
                Err(reason) => {
                    self.framegen
                        .remember_failure(request, (output, output), &reason);
                    self.framegen.fallback(request, &reason);
                    return Ok(None);
                }
            };
        // Super Resolution first: it decides the render size of depth/motion.
        let upscale = camera.camera.upscaler;
        let sr_ready = upscale.mode == UpscalerMode::DlssSr
            && match self.upscaler.precheck(upscale, output) {
                Some(reason) => {
                    self.upscaler.fallback(upscale, &reason);
                    false
                }
                None => match stage.ensure_session(
                    &self.gpu.device,
                    &self.gpu.queue,
                    upscale.quality,
                    output,
                ) {
                    Ok(new) => {
                        if new {
                            self.upscaler.history.request_reset();
                        }
                        true
                    }
                    Err(reason) => {
                        self.upscaler.remember_failure(upscale, output, &reason);
                        self.upscaler.fallback(upscale, &reason);
                        false
                    }
                },
            };
        let render = match (&stage.session, sr_ready) {
            (Some((_, session)), true) => session.status().render_size,
            _ => output,
        };
        let sizes = (output, render);
        let mut new_generator = false;
        if self
            .framegen
            .stage
            .as_ref()
            .is_none_or(|s| s.poisoned || (s.output, s.render) != sizes)
        {
            self.framegen.stage = None;
            match framegen::stage::FgStage::new(context, &session_options, output, render) {
                Ok(fg) => {
                    self.framegen.stage = Some(fg);
                    new_generator = true;
                }
                Err(reason) => {
                    self.framegen.remember_failure(request, sizes, &reason);
                    self.framegen.fallback(request, &reason);
                    return Ok(None);
                }
            }
        }
        let maximum = self
            .framegen
            .stage
            .as_ref()
            .expect("generator ensured above")
            .generator
            .status()
            .multi_frame_max;
        let count = request.multiplier - 1;
        if count > maximum {
            let reason = format!(
                "{}x frame generation needs {count} generated frames per real frame; NGX reports \
                 at most {maximum} on this GPU",
                request.multiplier
            );
            self.framegen.fallback(request, &reason);
            return Ok(None);
        }
        let now = std::time::Instant::now();
        let reset = new_generator || self.framegen.needs_reset(camera, now);
        let aspect = output[0] as f32 / output[1] as f32;
        let current_vp = camera_matrix(camera, aspect)?;
        let projection = scene3d::camera_projection(camera, aspect)?;
        let previous_vp =
            self.pipeline()?
                .previous_clip_from_world(ViewId::MAIN, current_vp, reset);

        // The real frame's scene (super-resolved, or at output size) into
        // the HUD-less colour, with render-size depth/motion for NGX.
        let fg = self.framegen.stage.as_ref().expect("generator");
        let hudless = OutputTarget {
            view: fg.hudless.view.clone(),
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            size: output,
            viewport: Viewport::full(output),
        };
        let fg_inputs = (fg.depth.1.clone(), fg.motion.0.clone());
        let (stats, jitter) = if sr_ready {
            let Some(scene) = self.super_resolve(
                stage,
                frame,
                upscale,
                &hudless,
                world,
                false,
                Some(fg_inputs),
            )?
            else {
                // SR failed this frame: generate from a native-resolution scene
                // next frame (SR's failure is sticky); present natively now.
                self.framegen.fallback(
                    request,
                    "this frame was rendered natively after a Super Resolution failure",
                );
                return Ok(None);
            };
            (scene.stats, scene.jitter)
        } else {
            if upscale.mode != UpscalerMode::DlssSr {
                self.upscaler.native(upscale);
            }
            let mut view = scene3d::camera_view(camera, ViewId::MAIN, output, output)?;
            view.reset_history = reset;
            view.host_needs.motion = true;
            view.presentation = Presentation::SceneOnly;
            let depth_export = self.depth_export()?;
            let (fg_depth, fg_motion) = fg_inputs;
            let mut hook = |gpu: &GpuContext, scene: &SceneTextures<'_>| -> Result<HostUpscale> {
                let motion = scene
                    .motion
                    .context("frame generation requests motion vectors")?;
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                depth_export.encode(&gpu.device, &mut encoder, &scene.depth.view, &fg_depth);
                encoder.copy_texture_to_texture(
                    motion.texture.as_image_copy(),
                    fg_motion.as_image_copy(),
                    motion.texture.size(),
                );
                gpu.queue.submit([encoder.finish()]);
                Ok(HostUpscale::NotHandled)
            };
            let (_, stats) = self.draw_3d(frame, view, &hudless, world, false, Some(&mut hook))?;
            (stats, [0.0; 2])
        };

        // NGX sees only the UI-free scene. Every presentable output receives
        // the same late painter/filter pass after all evaluations finish.
        let mut stats = stats;
        // Generated frames between the previous and this real frame.
        let camera_constants =
            framegen::generation_camera(camera, aspect, projection, current_vp, previous_vp);
        let orthographic = matches!(
            camera.camera.projection,
            engine_project::rendering3d::Projection3D::Orthographic { .. }
        );
        let fg = self.framegen.stage.as_mut().expect("generator");
        let mut presentable = Vec::new();
        let mut failure = None;
        for index in 1..=count {
            let inputs = engine_dlss::GenerationInputs {
                backbuffer: &fg.hudless.texture,
                hudless: None,
                depth: &fg.depth.0,
                motion: &fg.motion.0,
            };
            let generation = engine_dlss::GenerationFrame {
                camera: camera_constants,
                jitter: framegen::generation_jitter(jitter, render),
                motion_scale: [1.0, 1.0],
                reset: reset && index == 1,
                depth_inverted: false,
                camera_motion_included: true,
                hdr: false,
                orthographic,
                interpolation: engine_dlss::Interpolation { index, count },
            };
            match fg.generator.evaluate(
                &inputs,
                &generation,
                &fg.generated[index as usize - 1].texture,
            ) {
                Ok(engine_dlss::Generated::Presentable) => presentable.push(index as usize - 1),
                Ok(_) => {}
                Err(error) => {
                    failure = Some(format!("DLSS Frame Generation evaluation failed: {error}"));
                    break;
                }
            }
        }
        if failure.is_some() {
            fg.poisoned = true;
            presentable.clear();
        }
        let sources: Vec<wgpu::TextureView> = std::iter::once(fg.hudless.view.clone())
            .chain(presentable.iter().map(|&i| fg.generated[i].view.clone()))
            .collect();
        let destinations: Vec<OutputTarget> = std::iter::once(fg.backbuffer.view.clone())
            .chain(presentable.iter().map(|&i| fg.generated[i].view.clone()))
            .map(|view| OutputTarget {
                view,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                size: output,
                viewport: Viewport::full(output),
            })
            .collect();
        let outputs: Vec<PresentationOutput<'_>> = sources
            .iter()
            .zip(&destinations)
            .map(|(source, target)| PresentationOutput {
                source: PresentationInput {
                    view: source,
                    size: output,
                },
                target,
            })
            .collect();
        let (success, ui_stats) = self.complete_generated_source(
            frame,
            &outputs,
            world,
            request,
            sizes,
            failure.as_deref(),
        )?;
        sprite::add_stats(&mut stats, ui_stats);
        // Interpolation failure still presents the already-rendered real frame;
        // keeping the stage's textures avoids a second scene/history advance.
        if !success {
            return Ok(Some(stats));
        }
        if reset {
            self.framegen.resets += 1;
        }
        self.framegen.generated += presentable.len() as u64;
        self.framegen.presentable = presentable;
        self.framegen.active(request, maximum);
        Ok(Some(stats))
    }

    /// Complete an already rendered source, then present it even if NGX failed.
    /// Kept outside the SDK wrapper so tests can exercise the actual host's
    /// retained-colour/history path without claiming a successful NGX evaluation.
    #[cfg(any(feature = "dlss", test))]
    fn complete_generated_source(
        &mut self,
        frame: &SceneRenderFrame,
        outputs: &[PresentationOutput<'_>],
        world: Option<&World>,
        request: FrameGenerationSettings,
        sizes: ([u32; 2], [u32; 2]),
        failure: Option<&str>,
    ) -> Result<(bool, RenderStats)> {
        let camera = frame
            .camera3d
            .as_ref()
            .context("frame generation requires a Camera3D")?;
        let success = self.framegen.complete_evaluation(
            camera,
            std::time::Instant::now(),
            request,
            sizes,
            failure,
        );
        let stats = self.composite_presentations(frame, outputs, world)?;
        Ok((success, stats))
    }

    /// Presentable generated frames of the last [`Self::render_generated`].
    pub fn generated_frame_count(&self) -> usize {
        self.framegen.presentable.len()
    }

    /// Draw generated frame `index` (presentation order) into `target`, a
    /// view of this renderer's target size and format.
    pub fn present_generated(&mut self, index: usize, target: &wgpu::TextureView) -> Result<()> {
        let slot = *self
            .framegen
            .presentable
            .get(index)
            .context("no such generated frame")?;
        self.blit_generation(Some(slot), target)
    }

    /// Draw the last real frame of [`Self::render_generated`] into `target`.
    pub fn present_real(&mut self, target: &wgpu::TextureView) -> Result<()> {
        ensure!(
            self.framegen.real_ready,
            "no generated-path real frame to present"
        );
        self.blit_generation(None, target)
    }

    /// Record generated frames a host actually presented to a surface.
    pub fn note_generated_presented(&mut self, count: usize) {
        self.framegen.note_presented(count);
    }

    /// Read back the last real frame's generated frames (presentation order)
    /// followed by the real frame, as tightly packed sRGB RGBA8 rows of the 3D
    /// viewport size. For tests and tools without a window.
    pub fn capture_generated(&self) -> Result<Vec<CapturedFrame>> {
        self.capture_generated_frames()
    }

    #[cfg(not(feature = "dlss"))]
    fn blit_generation(&mut self, _: Option<usize>, _: &wgpu::TextureView) -> Result<()> {
        anyhow::bail!("frame generation is not built")
    }

    #[cfg(not(feature = "dlss"))]
    fn capture_generated_frames(&self) -> Result<Vec<CapturedFrame>> {
        Ok(Vec::new())
    }

    #[cfg(feature = "dlss")]
    fn blit_generation(&mut self, slot: Option<usize>, target: &wgpu::TextureView) -> Result<()> {
        let fg = self
            .framegen
            .stage
            .as_ref()
            .context("no frame generation stage")?;
        let source = match slot {
            Some(slot) => &fg.generated[slot].view,
            None => &fg.backbuffer.view,
        };
        let stage = self
            .upscaler
            .dlss
            .as_mut()
            .context("no frame generation stage")?;
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        stage.encode_composite(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            source,
            target,
            self.info.format,
            self.framegen.viewport,
        );
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu.check_errors()
    }

    #[cfg(feature = "dlss")]
    fn capture_generated_frames(&self) -> Result<Vec<CapturedFrame>> {
        let Some(fg) = self
            .framegen
            .stage
            .as_ref()
            .filter(|_| self.framegen.real_ready)
        else {
            return Ok(Vec::new());
        };
        let [width, height] = fg.output;
        let read = |texture: &wgpu::Texture, generated| -> Result<CapturedFrame> {
            Ok(CapturedFrame {
                generated,
                width,
                height,
                rgba8: engine_render_wgpu::read_texture_rgba(&self.gpu, texture, width, height)?,
            })
        };
        let mut frames = self
            .framegen
            .presentable
            .iter()
            .map(|&slot| read(&fg.generated[slot].texture, true))
            .collect::<Result<Vec<_>>>()?;
        frames.push(read(&fg.backbuffer.texture, false)?);
        Ok(frames)
    }

    /// Sprite-device resource counters (tests and tools).
    pub fn sprite_resource_counts(&self) -> engine_render::ResourceCounts {
        self.sprites.device().counts()
    }
}

/// One frame of [`GpuRenderer::capture_generated`].
#[derive(Clone, Debug)]
pub struct CapturedFrame {
    /// `true` for NGX-generated frames, `false` for the real frame.
    pub generated: bool,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// A super-resolved frame: its stats and SR jitter (render pixels).
#[cfg(feature = "dlss")]
struct SrScene {
    stats: RenderStats,
    jitter: [f32; 2],
}

/// External stream textures with the placeholder `Arc`s sprites reference.
fn stream_copies(
    frame: &SceneRenderFrame,
    streams: &engine_render_pipeline::TextureStreams,
) -> Vec<(Arc<GpuTexture>, Arc<engine_project::content::Texture>)> {
    frame
        .streams
        .iter()
        .filter(|s| s.mode == TextureStreamMode::External)
        .filter_map(|s| Some((streams.get(&s.name)?, s.texture.clone())))
        .collect()
}

/// The lighting status of a view report (bounded: at most four reasons).
fn lighting_status_of(report: &LightingReport, authored: usize) -> LightingStatus {
    let stats = report.cluster_stats;
    LightingStatus {
        state: RenderPipelineState::Rendered,
        authored: authored as u64,
        submitted: report.submitted as u64,
        culled: report.culled as u64,
        over_budget: report.over_budget as u64,
        directional: report.directional as u64,
        local: report.local as u64,
        cluster_mode: report.cluster_mode.name().into(),
        clustered: report.clustered as u64,
        cluster_dims: report.cluster_dims,
        cluster_overflowed: stats.map(|s| s.overflowed_clusters),
        cluster_longest_list: stats.map(|s| s.longest_list),
        cluster_gpu_ms: stats
            .and_then(|s| s.gpu_nanoseconds)
            .map(|ns| ns as f64 / 1.0e6),
        shadows_enabled: report.shadows_enabled,
        shadowed_lights: report.shadowed_lights as u64,
        shadow_views: report.shadow_views as u64,
        shadow_dropped: report.shadow_dropped as u64,
        atlas_page_size: report.atlas_page_size,
        atlas_pages: report.atlas_pages,
        atlas_texels_used: report.atlas_texels_used,
        atlas_texels_capacity: report.atlas_texels_capacity,
        overflow: report
            .overflow
            .iter()
            .map(|r| engine_project::rendering3d::bounded_upscaler_reason(r))
            .collect(),
    }
}

/// The status of a view report, bounded for snapshots.
fn status_of(
    report: &ViewReport,
    state: RenderPipelineState,
    overrides: &std::collections::BTreeMap<String, bool>,
) -> RenderPipelineStatus {
    RenderPipelineStatus {
        state,
        overrides: overrides.clone(),
        features: report
            .features
            .states
            .iter()
            .take(engine_project::rendering3d::MAX_REPORTED_RENDER_FEATURES)
            .map(|s| RenderFeatureView {
                name: s.name.clone(),
                stage: s.stage.name().into(),
                enabled: s.enabled,
                source: s.source.name().into(),
                activity: s.activity.name().into(),
            })
            .collect(),
        unknown_overrides: report.features.unknown.clone(),
        draws: report.draws as u64,
        draw_calls: report.draw_calls as u64,
        instances: report.instances as u64,
    }
}

/// The status a renderer would report for `overrides` without rendering
/// (headless runs): built-in features plus `features`, resolved for the view.
pub fn preview_pipeline_status(
    features: Option<&PipelineFeatures>,
    overrides: &std::collections::BTreeMap<String, bool>,
) -> Result<RenderPipelineStatus> {
    let mut registry = engine_render_pipeline::builtin_registry()?;
    registry.register(DepthOfFieldFeature::new(PostEffects::default()))?;
    registry.register(MotionBlurFeature::new(PostEffects::default()))?;
    registry.register(SpatialAaFeature::new(PostEffects::default()))?;
    if let Some(features) = features {
        registry.absorb(features.0.descriptor_copy()?)?;
    }
    let resolved = registry.resolve(&FeatureOverrides(overrides.clone()));
    let report = ViewReport {
        features: resolved,
        ..Default::default()
    };
    Ok(status_of(
        &report,
        RenderPipelineState::NotRendered,
        overrides,
    ))
}

/// Submit engine commands into a host view, always restoring the backend.
fn submit_to(
    sprites: &mut SpriteRenderer<WgpuBackend>,
    view: &wgpu::TextureView,
    info: RenderTargetInfo,
    desc: engine_render::FrameDesc,
    commands: engine_render::RenderCommandBuffer,
) -> Result<()> {
    sprites
        .device_mut()
        .backend_mut()
        .set_external_target(view.clone(), info);
    let result = sprites.submit(desc, commands);
    sprites.device_mut().backend_mut().clear_external_target();
    result.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn camera_buffer_size_matches_a_column_major_4x4() {
        // The shader's camera uniform is one mat4x4<f32>: 64 bytes.
        assert_eq!(size_of::<[f32; 16]>(), 64);
        let camera =
            crate::CameraView::new(&engine_project::Camera2D::default(), 320, 180).unwrap();
        assert_eq!(bytemuck::cast_slice::<f32, u8>(&camera.world).len(), 64);
        assert_eq!(bytemuck::cast_slice::<f32, u8>(&camera.ui).len(), 64);
    }

    #[test]
    fn headless_status_previews_built_ins_and_authored_overrides() {
        let overrides = [
            ("engine.opaque".to_owned(), false),
            ("nope.x".to_owned(), true),
        ]
        .into();
        let status = preview_pipeline_status(None, &overrides).unwrap();
        assert_eq!(status.state, RenderPipelineState::NotRendered);
        let opaque = status
            .features
            .iter()
            .find(|f| f.name == "engine.opaque")
            .unwrap();
        assert!(!opaque.enabled);
        assert_eq!(opaque.source, "view_override");
        assert_eq!(status.unknown_overrides, ["nope.x"]);
        assert!(status.features.iter().any(|f| f.name == "engine.sprites"));
    }

    struct HostProbe {
        stage: Stage,
        fail_begin: bool,
        calls: Arc<std::sync::Mutex<Vec<&'static str>>>,
    }
    impl engine_render_pipeline::PipelineFeature for HostProbe {
        fn descriptor(&self) -> engine_render_pipeline::FeatureDescriptor {
            engine_render_pipeline::FeatureDescriptor::new("test.host_probe", self.stage)
                .replayable()
        }
        fn begin_presentation_sequence(&mut self) -> Result<()> {
            self.calls.lock().unwrap().push("begin");
            ensure!(!self.fail_begin, "intentional snapshot failure");
            Ok(())
        }
        fn end_presentation_sequence(&mut self) {
            self.calls.lock().unwrap().push("end");
        }
        fn encode(&mut self, _: &mut engine_render_pipeline::FeatureContext<'_>) -> Result<()> {
            self.calls.lock().unwrap().push("encode");
            Ok(())
        }
    }

    fn presentation_test_scene() -> SceneRenderFrame {
        use engine_project::{
            Camera2D,
            rendering3d::{Camera3D, CameraInstance3D},
        };
        let mut frame: SceneRenderFrame = RenderFrame {
            camera: Camera2D {
                logical_size: [64, 64],
                ..Default::default()
            },
            draws: vec![],
        }
        .into();
        frame.camera3d = Some(CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 3.0)).to_cols_array_2d(),
        });
        frame
    }

    #[test]
    #[ignore = "requires a native GPU adapter"]
    fn unavailable_fg_prechecks_before_hooks_and_disabled_hook_allows_native_fallback() -> Result<()>
    {
        let gpu = pollster::block_on(GpuContext::headless())?;
        let target = crate::OffscreenTarget::new(&gpu, 64, 64)?;
        let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        renderer.pipeline()?.register(HostProbe {
            stage: Stage::Overlay,
            fail_begin: true,
            calls: calls.clone(),
        })?;
        let mut frame = presentation_test_scene();
        frame.camera3d.as_mut().unwrap().camera.frame_generation = FrameGenerationSettings {
            mode: FrameGenerationMode::DlssFg,
            multiplier: 2,
        };
        // The enabled hook would fail if snapshots were acquired before the
        // known unavailable-device check. Native rendering may still encode it.
        assert!(renderer.render_generated(&frame)?.is_none());
        assert!(calls.lock().unwrap().is_empty());
        renderer.render_scene(&frame, &target.view, None)?;
        assert_eq!(*calls.lock().unwrap(), ["encode"]);
        calls.lock().unwrap().clear();
        frame
            .camera3d
            .as_mut()
            .unwrap()
            .camera
            .render_features
            .insert("test.host_probe".into(), false);
        renderer.render_scene(&frame, &target.view, None)?;
        assert!(
            calls.lock().unwrap().is_empty(),
            "disabled hooks neither pin nor encode"
        );
        assert_eq!(
            renderer.frame_generation_status().state,
            engine_project::rendering3d::FrameGenerationState::Fallback
        );
        renderer.gpu.check_errors()
    }

    #[test]
    #[ignore = "requires a native GPU adapter"]
    fn first_rt_frame_and_rt_enabled_later_register_inside_pinned_presentation() -> Result<()> {
        let gpu = pollster::block_on(GpuContext::headless())?;
        let target = crate::OffscreenTarget::new(&gpu, 64, 64)?;
        for first_frame_rt in [true, false] {
            let mut renderer = GpuRenderer::new(gpu.clone(), target.info, RenderLimits::default())?;
            // Exercise the real RtHost lazy preparation, with a deterministic
            // raster tier; this test does not require hardware RT or NGX.
            renderer
                .rt
                .set_unavailable(engine_render_rt::RayTracingStatus {
                    tier: engine_render_rt::RayTracingTier::Unavailable,
                    cause: None,
                    reason: Some("lifecycle regression uses raster tier".into()),
                    adapter: None,
                });
            let mut frame = presentation_test_scene();
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            renderer.pipeline()?.register(HostProbe {
                stage: Stage::Overlay,
                fail_begin: false,
                calls: calls.clone(),
            })?;
            for enable_rt in if first_frame_rt {
                vec![true]
            } else {
                vec![false, true]
            } {
                frame.ray_tracing = enable_rt.then(|| {
                    let mut settings = engine_project::raytracing::RayTracing::default();
                    settings.ambient_occlusion.enabled = true;
                    settings
                });
                let mut view = scene3d::camera_view(
                    frame.camera3d.as_ref().unwrap(),
                    ViewId::MAIN,
                    [64, 64],
                    [64, 64],
                )?;
                view.presentation = Presentation::SceneOnly;
                let overrides = view.features.clone();
                renderer
                    .pipeline()?
                    .begin_presentation_sequence(&overrides)?;
                let output = renderer.output_target(
                    &target.view,
                    crate::PixelRect {
                        x: 0,
                        y: 0,
                        width: 64,
                        height: 64,
                    },
                );
                let result = renderer.draw_3d(&frame, view, &output, None, false, None);
                renderer.pipeline()?.end_presentation_sequence();
                result?;
                assert_eq!(
                    renderer
                        .pipeline()?
                        .registry()
                        .descriptor(rt_names::AO)
                        .is_some(),
                    enable_rt
                );
            }
            let expected = if first_frame_rt {
                vec!["begin", "end"]
            } else {
                vec!["begin", "end", "begin", "end"]
            };
            assert_eq!(*calls.lock().unwrap(), expected);
        }
        gpu.check_errors()
    }

    #[test]
    #[ignore = "requires a native GPU adapter"]
    fn interpolation_failure_composites_retained_real_source_without_a_second_scene_render()
    -> Result<()> {
        use engine_project::{
            content::{Mesh, Vertex},
            rendering3d::{MeshDraw3D, MeshMaterial},
        };
        let gpu = pollster::block_on(GpuContext::headless())?;
        let target = crate::OffscreenTarget::new(&gpu, 64, 64)?;
        let mut renderer = GpuRenderer::new(gpu.clone(), target.info, RenderLimits::default())?;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("retained source regression"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target.info.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let source = OutputTarget {
            view: texture.create_view(&Default::default()),
            format: target.info.format,
            size: [64, 64],
            viewport: Viewport::full([64, 64]),
        };
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        renderer.pipeline()?.register(HostProbe {
            stage: Stage::Prepare,
            fail_begin: false,
            calls: calls.clone(),
        })?;
        let mut frame = presentation_test_scene();
        let vertices = |positions: &[[f32; 3]], color: [f32; 4]| -> Vec<Vertex> {
            positions
                .iter()
                .map(|&position| Vertex {
                    position,
                    uv: [0.0; 2],
                    color,
                })
                .collect()
        };
        frame.meshes.push(MeshDraw3D {
            id: "red".into(),
            mesh: Arc::new(Mesh {
                vertices: vertices(
                    &[[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
                    [1.0; 4],
                ),
                normals: vec![],
                indices: vec![0, 1, 2],
            }),
            texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            model: Mat4::IDENTITY.to_cols_array_2d(),
            material: MeshMaterial {
                unlit: true,
                tint: [1.0, 0.0, 0.0, 1.0],
                ..Default::default()
            },
            shader: None,
        });
        frame.sprites.draws.push(engine_project::runtime::Draw {
            id: "ui".into(),
            mesh: Arc::new(Mesh {
                vertices: vertices(
                    &[
                        [0.0, 0.0, 0.5],
                        [64.0, 0.0, 0.5],
                        [64.0, 64.0, 0.5],
                        [0.0, 64.0, 0.5],
                    ],
                    [0.0, 0.0, 1.0, 0.5],
                ),
                normals: vec![],
                indices: vec![0, 1, 2, 0, 2, 3],
            }),
            texture: None,
            layer: 0,
            space: engine_project::runtime::DrawSpace::Ui,
            clip: None,
            state: Default::default(),
            motion: None,
            offset: [0.0; 3],
        });
        let request = FrameGenerationSettings {
            mode: FrameGenerationMode::DlssFg,
            multiplier: 2,
        };
        let mut view = scene3d::camera_view(
            frame.camera3d.as_ref().unwrap(),
            ViewId::MAIN,
            [64, 64],
            [64, 64],
        )?;
        view.presentation = Presentation::SceneOnly;
        let current = view.clip_from_world();
        renderer
            .pipeline()?
            .begin_presentation_sequence(&view.features)?;
        let drawn = renderer.draw_3d(&frame, view, &source, None, false, None);
        if let Err(error) = drawn {
            renderer.pipeline()?.end_presentation_sequence();
            return Err(error);
        }
        let before = engine_render_wgpu::read_texture_rgba(&gpu, &texture, 64, 64)?;
        let output = OutputTarget {
            view: target.view.clone(),
            format: target.info.format,
            size: [64, 64],
            viewport: Viewport::full([64, 64]),
        };
        let completed = renderer.complete_generated_source(
            &frame,
            &[PresentationOutput {
                source: PresentationInput {
                    view: &source.view,
                    size: [64, 64],
                },
                target: &output,
            }],
            None,
            request,
            ([64, 64], [64, 64]),
            Some("injected interpolation failure"),
        );
        renderer.pipeline()?.end_presentation_sequence();
        assert!(!completed?.0);
        assert_eq!(
            *calls.lock().unwrap(),
            ["encode"],
            "late UI must not rerender the scene"
        );
        assert_eq!(renderer.framegen.history.frame(), 1);
        assert_eq!(
            renderer
                .pipeline()?
                .previous_clip_from_world(ViewId::MAIN, Mat4::IDENTITY, false),
            current
        );
        let after = target.read_rgba(&gpu)?;
        let at = (32 * 64 + 32) * 4;
        assert!(before[at] > 100, "source contains the real red mesh");
        let decode = |n: u8| {
            let x = n as f32 / 255.0;
            if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        let encode = |x: f32| {
            let y = if x <= 0.0031308 {
                x * 12.92
            } else {
                1.055 * x.powf(1.0 / 2.4) - 0.055
            };
            (y * 255.0).round() as i16
        };
        for c in 0..3 {
            let expected = encode(decode(before[at + c]) * 0.5 + if c == 2 { 0.5 } else { 0.0 });
            assert!(
                (after[at + c] as i16 - expected).abs() <= 3,
                "UI alpha applies exactly once at channel {c}"
            );
        }
        assert_eq!(renderer.generated_frame_count(), 0);
        assert_eq!(
            renderer.frame_generation_status().state,
            engine_project::rendering3d::FrameGenerationState::Fallback
        );
        gpu.check_errors()
    }
}
