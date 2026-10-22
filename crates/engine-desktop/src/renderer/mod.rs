//! Sprite rendering for games, editor viewports and offscreen tests.
//!
//! * [`SpriteRenderer`] (`sprite.rs`): backend-neutral conversion of a
//!   [`RenderFrame`] into `engine-render` draw commands.
//! * [`GpuRenderer`] (this file): the native wgpu renderer, a
//!   `SpriteRenderer<WgpuBackend>` sharing the caller's [`GpuContext`] that also
//!   runs [`NativeRenderPass`]es after the engine draws.
//! * `cache.rs`: Arc-keyed mesh/texture handle caches with eviction.
//! * `vertex.rs`: the 36-byte vertex record and its neutral layout.

mod cache;
mod sprite;
mod vertex;

pub use sprite::{SpriteRenderer, TEXTURED_WGSL};

use crate::{GpuContext, NativeRenderPasses, RenderTargetInfo};
use anyhow::{Context, Result, ensure};
use engine_core::bevy_ecs::world::World;
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
pub struct GpuRenderer {
    pub gpu: GpuContext,
    info: RenderTargetInfo,
    sprites: SpriteRenderer<WgpuBackend>,
    passes: NativeRenderPasses,
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
        })
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
        self.gpu.check_errors()?;
        self.sprites
            .device_mut()
            .backend_mut()
            .set_external_target(target.clone(), self.info);
        let stats = self.sprites.render(frame, RenderTarget::Surface);
        self.sprites
            .device_mut()
            .backend_mut()
            .clear_external_target();
        let stats = stats?.context("the wgpu backend skipped a frame into an external view")?;
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
