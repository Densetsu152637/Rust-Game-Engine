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
mod scene;
mod sprite;
mod temporal;
mod upscale;
mod vertex;

pub use scene::{LIT_WGSL, camera_matrix};
pub use sprite::{SpriteRenderer, TEXTURED_WGSL};
pub use temporal::{
    MOTION_WGSL, halton, jitter_matrix, jitter_offset, jitter_phase_count, pixel_motion,
};

use crate::{GpuContext, NativeRenderPasses, RenderTargetInfo, UpscalerDevice};
use anyhow::{Context, Result, ensure};
use engine_core::bevy_ecs::world::World;
use engine_project::rendering3d::{
    SceneRenderFrame, UpscalerMode, UpscalerSettings, UpscalerStatus,
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
        renderer.upscaler.set_device(device);
        Ok(renderer)
    }

    /// What the upscaler did for the last rendered 3D frame.
    pub fn upscaler_status(&self) -> &UpscalerStatus {
        self.upscaler.status()
    }

    /// Discard temporal history before the next frame (camera cut, teleport,
    /// scene transition). Resizes and quality changes reset automatically.
    pub fn reset_upscaler_history(&mut self) {
        self.upscaler.request_reset();
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

    pub fn render_scene(
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
        use crate::dlss::DeviceSupport;
        let viewport = self
            .sprites
            .scene_viewport(frame, (self.info.width, self.info.height))?;
        let output = [viewport.width, viewport.height];
        if let Some(reason) = self.upscaler.precheck(request, output) {
            self.upscaler.fallback(request, &reason);
            return Ok(None);
        }
        let mut stage = match (self.upscaler.dlss.take(), &self.upscaler.device.0) {
            (Some(stage), _) => stage,
            (None, DeviceSupport::Ready(paths)) => {
                upscale::dlss_stage::DlssStage::new(&self.gpu.device, paths.clone())
            }
            (None, _) => unreachable!("precheck accepts only a ready device"),
        };
        let result = self.render_with_stage(&mut stage, frame, target, request, viewport);
        self.upscaler.dlss = Some(stage);
        result
    }

    #[cfg(feature = "dlss")]
    // Without NGX_SDK_DIR at build time engine-dlss session types are
    // uninhabited, so code after them is unreachable in that configuration.
    #[allow(unreachable_code, unused_variables)]
    fn render_with_stage(
        &mut self,
        stage: &mut upscale::dlss_stage::DlssStage,
        frame: &SceneRenderFrame,
        target: &wgpu::TextureView,
        request: UpscalerSettings,
        viewport: crate::PixelRect,
    ) -> Result<Option<RenderStats>> {
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
        let built =
            match self
                .sprites
                .build_temporal(frame, (self.info.width, self.info.height), &temporal)
            {
                Ok(built) => built,
                Err(error) => {
                    stage.session = Some((key, session));
                    return Err(error);
                }
            };
        let [color, depth, motion] = session.inputs().map(Clone::clone);
        let render_info = |format| RenderTargetInfo {
            width: render[0],
            height: render[1],
            format,
        };
        let intermediate = stage.motion_view(&self.gpu.device, render);
        let mut desc = engine_render::FrameDesc::new(RenderTarget::Surface);
        let inputs = submit_to(
            &mut self.sprites,
            &color,
            render_info(wgpu::TextureFormat::Rgba16Float),
            desc,
            built.color,
        )
        .and_then(|()| {
            // Far depth and zero motion where no opaque mesh is drawn.
            desc.clear_color = [0.0, 0.0, 1.0, 0.0];
            submit_to(
                &mut self.sprites,
                &intermediate,
                render_info(wgpu::TextureFormat::Rgba32Float),
                desc,
                built.motion,
            )
        });
        if let Err(error) = inputs {
            stage.session = Some((key, session));
            return Err(error);
        }
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        stage.encode_split(
            &self.gpu.device,
            &mut encoder,
            &intermediate,
            &depth,
            &motion,
        );
        self.gpu.queue.submit([encoder.finish()]);
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
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        stage.encode_composite(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            session.output(),
            target,
            self.info.format,
            [viewport.x, viewport.y, viewport.width, viewport.height],
        );
        self.gpu.queue.submit([encoder.finish()]);
        let adapter = session.status().device_name.clone();
        stage.session = Some((key, session));
        let mut overlay = built.overlay.desc(RenderTarget::Surface);
        overlay.preserve_color = true;
        submit_to(
            &mut self.sprites,
            target,
            self.info,
            overlay,
            built.overlay.commands,
        )?;
        self.upscaler.active(request, render, output, adapter);
        Ok(Some(built.stats))
    }
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
