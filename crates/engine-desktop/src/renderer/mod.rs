//! Sprite rendering for games, editor viewports and offscreen tests.
//!
//! * [`SpriteRenderer`] (`sprite.rs`): backend-neutral conversion of a
//!   [`RenderFrame`] into `engine-render` draw commands.
//! * [`GpuRenderer`] (this file): the native wgpu renderer, a
//!   `SpriteRenderer<WgpuBackend>` sharing the caller's [`GpuContext`] that also
//!   runs [`NativeRenderPass`]es after the engine draws.
//! * `cache.rs`: Arc-keyed mesh/texture handle caches with eviction.
//! * `vertex.rs`: the 48-byte vertex record and its neutral layout.

mod cache;
mod framegen;
mod scene;
mod sprite;
mod temporal;
mod upscale;
mod vertex;

pub use framegen::{MAX_GENERATION_GAP, MAX_PRESENT_SPACING, PresentPacer, presentation_offsets};
pub use scene::{LIT_WGSL, camera_matrix};
pub use sprite::{SpriteRenderer, TEXTURED_WGSL};
pub use temporal::{
    MOTION_WGSL, halton, jitter_matrix, jitter_offset, jitter_phase_count, pixel_motion,
};

use crate::{GpuContext, NativeRenderPasses, RenderTargetInfo, UpscalerDevice};
use anyhow::{Context, Result, ensure};
use engine_core::bevy_ecs::world::World;
use engine_project::rendering3d::{
    FrameGenerationMode, FrameGenerationSettings, FrameGenerationStatus, SceneRenderFrame,
    UpscalerMode, UpscalerSettings, UpscalerStatus,
};
use engine_project::runtime::RenderFrame;
use engine_render::RenderTarget;
use engine_render_wgpu::{WgpuBackend, WgpuOptions};

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

/// A renderer usable by a game window, editor viewport or offscreen test. CPU
/// assets are immutable Arc snapshots: retaining the same Arc means unchanged;
/// publish a new Arc for edits. GPU resources retain the Arc to prevent address
/// reuse and are evicted after their last frame reference.
///
/// Internally a [`SpriteRenderer`] over a [`WgpuBackend`] that shares `gpu`;
/// native render passes are encoded into a separate command buffer submitted
/// after the engine draws on the same queue.
///
/// A 3D camera whose `upscaler` requests DLSS Super Resolution renders through
/// the temporal stage (see [`Self::upscaler_status`]) when the device was
/// prepared by [`crate::request_upscaler_gpu`]; otherwise the frame renders
/// natively and the status reports why.
pub struct GpuRenderer {
    pub gpu: GpuContext,
    info: RenderTargetInfo,
    sprites: SpriteRenderer<WgpuBackend>,
    passes: NativeRenderPasses,
    upscaler: upscale::Upscaler,
    framegen: framegen::FrameGeneration,
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
        sprites.set_surface_size((info.width, info.height));
        gpu.check_errors()?;
        Ok(Self {
            gpu,
            info,
            sprites,
            passes: NativeRenderPasses::default(),
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

    /// Discard upscaler and frame generation history before the next frame
    /// (camera cut, teleport, scene transition, resume after a pause). Resizes
    /// and quality changes reset automatically.
    pub fn reset_temporal_history(&mut self) {
        self.upscaler.request_reset();
        self.framegen.request_reset();
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
        let max = self.gpu.device.limits().max_texture_dimension_2d;
        ensure!(
            width <= max && height <= max,
            "render target exceeds device limit"
        );
        self.gpu.wait_idle()?;
        self.info.width = width;
        self.info.height = height;
        self.sprites.set_surface_size((width, height));
        for (name, pass) in &mut self.passes.passes {
            pass.prepare(&self.gpu, self.info)
                .with_context(|| format!("resizing render pass {name}"))?;
        }
        self.gpu.check_errors()
    }

    /// Encode and submit one frame. At most `max_frames_in_flight` submissions
    /// are retained; backpressure waits on the oldest submission. Invalid CPU
    /// data fails before changing cached resources or submitting partial draws.
    /// The engine draws are submitted first, then the native passes' commands.
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
        if let Some(stats) = self.render_generated(frame)? {
            self.present_real(target)?;
            return Ok(stats);
        }
        self.render_scene_without_generation(frame, target, world)
    }

    /// [`Self::render_scene`] for a frame [`Self::render_generated`] declined:
    /// Super Resolution or native rendering, then native render passes.
    pub fn render_scene_without_generation(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        world: Option<&World>,
    ) -> Result<RenderStats> {
        self.gpu.check_errors()?;
        let request = frame.camera3d.as_ref().map(|c| c.camera.upscaler);
        let stats = match request {
            Some(request) if request.mode == UpscalerMode::DlssSr => {
                match self.render_upscaled(frame, target, request)? {
                    Some(stats) => stats,
                    None => self.render_native(frame, target)?,
                }
            }
            _ => {
                let stats = self.render_native(frame, target)?;
                self.upscaler.native(request.unwrap_or_default());
                stats
            }
        };
        if !self.passes.passes.is_empty() {
            let mut encoder =
                self.gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("engine native passes"),
                    });
            for (name, pass) in &mut self.passes.passes {
                pass.encode(&self.gpu, &mut encoder, target, self.info, world)
                    .with_context(|| format!("encoding render pass {name}"))?;
            }
            self.gpu.check_errors()?;
            self.gpu.queue.submit([encoder.finish()]);
            self.gpu
                .device
                .poll(wgpu::PollType::Poll)
                .context("polling GPU frame")?;
        }
        self.gpu.check_errors()?;
        Ok(stats)
    }

    /// 3D and 2D at output resolution in one engine submission.
    fn render_native(
        &mut self,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
    ) -> Result<RenderStats> {
        self.sprites
            .device_mut()
            .backend_mut()
            .set_external_target(target.clone(), self.info);
        let stats = self.sprites.render_scene(frame, RenderTarget::Surface);
        self.sprites
            .device_mut()
            .backend_mut()
            .clear_external_target();
        stats?.context("the wgpu backend skipped a frame into an external view")
    }

    /// `Ok(None)`: the request could not run; the status holds the fallback
    /// reason and the caller renders natively. Invalid frame data is an error,
    /// exactly as on the native path.
    #[cfg(not(feature = "dlss"))]
    fn render_upscaled(
        &mut self,
        _: &SceneRenderFrame,
        _: &wgpu::TextureView,
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
        let result = self
            .super_resolve(
                &mut stage,
                frame,
                request,
                viewport,
                (self.info.width, self.info.height),
            )
            .and_then(|scene| {
                let Some(scene) = scene else { return Ok(None) };
                let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
                stage.encode_composite(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut encoder,
                    scene.session.1.output(),
                    target,
                    self.info.format,
                    [viewport.x, viewport.y, viewport.width, viewport.height],
                );
                self.gpu.queue.submit([encoder.finish()]);
                stage.session = Some(scene.session);
                let mut overlay = scene.overlay.desc(RenderTarget::Surface);
                overlay.preserve_color = true;
                submit_to(
                    &mut self.sprites,
                    target,
                    self.info,
                    overlay,
                    scene.overlay.commands,
                )?;
                Ok(Some(scene.stats))
            });
        self.upscaler.dlss = Some(stage);
        result
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

    /// Jittered 3D color, engine depth/motion and a successful SR evaluation.
    /// `Ok(None)` records the SR fallback reason; the session is then left in
    /// `stage` (or dropped after a failed evaluation). On success the session is
    /// returned for the caller's composite and must be put back into `stage`.
    /// `size` is the target size the 2D overlay is built for.
    #[cfg(feature = "dlss")]
    // Without NGX_SDK_DIR at build time engine-dlss session types are
    // uninhabited, so code after them is unreachable in that configuration.
    #[allow(unreachable_code, unused_variables)]
    fn super_resolve(
        &mut self,
        stage: &mut upscale::dlss_stage::DlssStage,
        frame: &SceneRenderFrame,
        request: UpscalerSettings,
        viewport: crate::PixelRect,
        size: (u32, u32),
    ) -> Result<Option<SrScene>> {
        use temporal::{TemporalFrame, jitter_matrix, jitter_offset, jitter_phase_count};
        let camera = frame
            .camera3d
            .as_ref()
            .context("an upscaled frame requires a Camera3D")?;
        let output = [viewport.width, viewport.height];
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
        let current_vp = camera_matrix(camera, output[0] as f32 / output[1] as f32)?;
        let reset = self.upscaler.history.needs_reset(camera);
        let jitter = jitter_offset(
            self.upscaler.history.frame(),
            jitter_phase_count(render, output),
        );
        let no_history = std::collections::HashMap::new();
        let temporal = TemporalFrame {
            jitter: jitter_matrix(jitter, render),
            current_vp,
            previous_vp: self.upscaler.history.previous_vp(current_vp, reset),
            previous_models: if reset {
                &no_history
            } else {
                self.upscaler.history.previous_models()
            },
            render_size: render,
        };
        // Invalid frame data fails here, before any submission.
        let built = match self.sprites.build_temporal(frame, size, &temporal) {
            Ok(built) => built,
            Err(error) => {
                stage.session = Some((key, session));
                return Err(error);
            }
        };
        let [color, depth, motion] = session.inputs().map(Clone::clone);
        let sprite::TemporalBuild {
            color: color_commands,
            motion: motion_commands,
            overlay,
            stats,
        } = built;
        if let Err(error) = self.submit_scene_inputs(
            stage,
            (color_commands, motion_commands),
            render,
            (&color, wgpu::TextureFormat::Rgba16Float),
            (&depth, &motion),
        ) {
            stage.session = Some((key, session));
            return Err(error);
        }
        let now = std::time::Instant::now();
        let delta_ms = stage.last_evaluation.map_or(1000.0 / 60.0, |last| {
            (now.duration_since(last).as_secs_f32() * 1000.0).clamp(0.1, 1000.0)
        });
        let evaluation = session.evaluate(engine_dlss::Frame {
            jitter,
            motion_scale: [1.0, 1.0],
            reset,
            delta_ms,
        });
        if let Err(error) = evaluation {
            // A failed evaluation poisons the session (dropped here); retry only
            // after the request changes, and present this frame natively.
            let reason = format!("DLSS evaluation failed: {error}");
            self.upscaler.remember_failure(request, output, &reason);
            self.upscaler.fallback(request, &reason);
            return Ok(None);
        }
        stage.last_evaluation = Some(now);
        self.upscaler.evaluated += 1;
        if reset {
            self.upscaler.resets += 1;
        }
        self.upscaler
            .history
            .commit(camera, current_vp, &frame.meshes);
        let adapter = session.status().device_name.clone();
        self.upscaler.active(request, render, output, adapter);
        Ok(Some(SrScene {
            textures: [color, depth, motion].map(|v| v.texture().clone()),
            session: (key, session),
            overlay,
            stats,
            jitter,
        }))
    }

    /// Submit the 3D color pass into `color`, then the engine depth/motion
    /// pass split into the `R32Float` depth and `Rg16Float` motion views.
    #[cfg(feature = "dlss")]
    fn submit_scene_inputs(
        &mut self,
        stage: &mut upscale::dlss_stage::DlssStage,
        (color_commands, motion_commands): (
            engine_render::RenderCommandBuffer,
            engine_render::RenderCommandBuffer,
        ),
        render: [u32; 2],
        color: (&wgpu::TextureView, wgpu::TextureFormat),
        (depth, motion): (&wgpu::TextureView, &wgpu::TextureView),
    ) -> Result<()> {
        let render_info = |format| RenderTargetInfo {
            width: render[0],
            height: render[1],
            format,
        };
        let intermediate = stage.motion_view(&self.gpu.device, render);
        let mut desc = engine_render::FrameDesc::new(RenderTarget::Surface);
        submit_to(
            &mut self.sprites,
            color.0,
            render_info(color.1),
            desc,
            color_commands,
        )?;
        // Far depth and zero motion where no opaque mesh is drawn.
        desc.clear_color = [0.0, 0.0, 1.0, 0.0];
        submit_to(
            &mut self.sprites,
            &intermediate,
            render_info(wgpu::TextureFormat::Rgba32Float),
            desc,
            motion_commands,
        )?;
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        stage.encode_split(&self.gpu.device, &mut encoder, &intermediate, depth, motion);
        self.gpu.queue.submit([encoder.finish()]);
        Ok(())
    }

    /// Render the real frame through DLSS Frame Generation when requested and
    /// supported: the scene (optionally super-resolved) into the engine's
    /// HUD-less color, plus 2D/UI into its backbuffer, then NGX generates the
    /// frames between the previous and this real frame. Hosts then present
    /// [`Self::present_generated`] for `0..generated_frame_count()` followed by
    /// [`Self::present_real`]. `Ok(None)`: not requested or unsupported (see
    /// [`Self::frame_generation_status`]); render with [`Self::render_scene`].
    pub fn render_generated(&mut self, frame: &SceneRenderFrame) -> Result<Option<RenderStats>> {
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
        let stats = self.render_generated_frame(frame, request, viewport)?;
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
        _: &SceneRenderFrame,
        request: FrameGenerationSettings,
        viewport: crate::PixelRect,
    ) -> Result<Option<RenderStats>> {
        let output = [viewport.width, viewport.height];
        let reason = self
            .framegen
            .precheck(
                request,
                &self.upscaler.device,
                !self.passes.passes.is_empty(),
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
    ) -> Result<Option<RenderStats>> {
        let output = [viewport.width, viewport.height];
        let prior = self
            .framegen
            .stage
            .as_ref()
            .map_or((output, output), |s| (s.output, s.render));
        if let Some(reason) = self.framegen.precheck(
            request,
            &self.upscaler.device,
            !self.passes.passes.is_empty(),
            prior,
        ) {
            self.framegen.fallback(request, &reason);
            return Ok(None);
        }
        let mut stage = self.take_stage();
        let result = self.generate_with_stage(&mut stage, frame, request, viewport);
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
    ) -> Result<Option<RenderStats>> {
        use temporal::TemporalFrame;
        let camera = frame
            .camera3d
            .as_ref()
            .context("frame generation requires a Camera3D")?;
        let output = [viewport.width, viewport.height];
        let size = (output[0], output[1]);
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
            .is_none_or(|s| (s.output, s.render) != sizes)
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
        let projection = scene::camera_projection(camera, aspect)?;
        let previous_vp = self.framegen.history.previous_vp(current_vp, reset);

        // The real frame's scene: super-resolved, or rendered at output size.
        let (overlay, stats, depth, motion, jitter) = if sr_ready {
            let Some(scene) = self.super_resolve(stage, frame, upscale, viewport, size)? else {
                // SR failed this frame: generate from a native-resolution scene
                // next frame (SR's failure is sticky); present natively now.
                self.framegen.fallback(
                    request,
                    "this frame was rendered natively after a Super Resolution failure",
                );
                return Ok(None);
            };
            let hudless = &self.framegen.stage.as_ref().expect("generator").hudless;
            let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
            stage.encode_composite(
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                scene.session.1.output(),
                &hudless.view,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                [0, 0, output[0], output[1]],
            );
            self.gpu.queue.submit([encoder.finish()]);
            stage.session = Some(scene.session);
            let [_, depth, motion] = scene.textures;
            (scene.overlay, scene.stats, depth, motion, scene.jitter)
        } else {
            if upscale.mode != UpscalerMode::DlssSr {
                self.upscaler.native(upscale);
            }
            let no_history = std::collections::HashMap::new();
            let temporal = TemporalFrame {
                jitter: glam::Mat4::IDENTITY,
                current_vp,
                previous_vp,
                previous_models: if reset {
                    &no_history
                } else {
                    self.framegen.history.previous_models()
                },
                render_size: output,
            };
            let sprite::TemporalBuild {
                color: color_commands,
                motion: motion_commands,
                overlay,
                stats,
            } = self.sprites.build_temporal(frame, size, &temporal)?;
            let fg = self.framegen.stage.as_ref().expect("generator");
            let (hudless, depth, motion) =
                (fg.hudless.view.clone(), fg.depth.clone(), fg.motion.clone());
            self.submit_scene_inputs(
                stage,
                (color_commands, motion_commands),
                output,
                (&hudless, wgpu::TextureFormat::Rgba8UnormSrgb),
                (&depth.1, &motion.1),
            )?;
            (overlay, stats, depth.0, motion.0, [0.0; 2])
        };

        // Backbuffer = HUD-less scene + 2D/UI at output resolution.
        let fg = self.framegen.stage.as_ref().expect("generator");
        let (backbuffer, hudless) = (fg.backbuffer.view.clone(), fg.hudless.texture.clone());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            hudless.as_image_copy(),
            fg.backbuffer.texture.as_image_copy(),
            hudless.size(),
        );
        self.gpu.queue.submit([encoder.finish()]);
        let overlay_desc = {
            let mut desc = overlay.desc(RenderTarget::Surface);
            desc.preserve_color = true;
            desc
        };
        submit_to(
            &mut self.sprites,
            &backbuffer,
            RenderTargetInfo {
                width: output[0],
                height: output[1],
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
            },
            overlay_desc,
            overlay.commands,
        )?;

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
                backbuffer: &fg.backbuffer.texture,
                hudless: Some(&fg.hudless.texture),
                depth: &depth,
                motion: &motion,
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
        self.framegen
            .history
            .commit(camera, current_vp, &frame.meshes);
        self.framegen.finish_frame(std::time::Instant::now());
        if let Some(reason) = failure {
            // The generator is poisoned: present the real frame only, and
            // retry after the request or sizes change.
            self.framegen.stage = None;
            self.framegen.remember_failure(request, sizes, &reason);
            self.framegen.fallback(request, &reason);
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

/// A super-resolved scene: the session (to return to its stage), the NGX
/// color/depth/motion input textures, and the built overlay.
#[cfg(feature = "dlss")]
struct SrScene {
    session: (
        (engine_project::rendering3d::UpscalerQuality, [u32; 2]),
        engine_dlss::Session,
    ),
    textures: [wgpu::Texture; 3],
    overlay: sprite::BuiltFrame,
    stats: RenderStats,
    /// SR projection jitter in render pixels.
    jitter: [f32; 2],
}

/// Submit engine commands into a host view, always restoring the backend.
#[cfg(feature = "dlss")]
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
}
