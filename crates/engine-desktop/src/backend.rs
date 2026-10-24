//! Offscreen sprite rendering on any [`RenderBackend`], with readback.

use crate::{RenderLimits, RenderStats, SpriteRenderer};
use anyhow::{Result, anyhow};
use engine_project::runtime::RenderFrame;
use engine_render::{RenderBackend, RenderTarget, TargetHandle, headless::RecordingBackend};
use engine_render_wgpu::{WgpuBackend, WgpuBackends, WgpuOptions};

/// Sprite rendering into an offscreen RGBA target, the same path a window uses,
/// for tests and tools. Build it on a real GPU with [`Self::wgpu`], or on the
/// deterministic recording backend with [`Self::headless`] (no GPU; inspect the
/// command stream through the log of the backend passed to [`Self::new`]).
pub struct OffscreenSprites {
    renderer: SpriteRenderer<Box<dyn RenderBackend>>,
    target: TargetHandle,
    size: (u32, u32),
}

impl OffscreenSprites {
    /// Use any backend (for example a [`RecordingBackend`] whose log you keep).
    pub fn new(
        backend: Box<dyn RenderBackend>,
        width: u32,
        height: u32,
        limits: RenderLimits,
    ) -> Result<Self> {
        let mut renderer = SpriteRenderer::new(backend, limits)?;
        let target = renderer.device_mut().create_target(width, height)?;
        Ok(Self {
            renderer,
            target,
            size: (width, height),
        })
    }

    /// Offscreen sprites on the wgpu backend, restricted to the `backends` APIs.
    pub fn wgpu(
        backends: WgpuBackends,
        width: u32,
        height: u32,
        limits: RenderLimits,
    ) -> Result<Self> {
        let backend = WgpuBackend::offscreen(WgpuOptions {
            backends,
            frames_in_flight: limits.max_frames_in_flight as u32,
            ..Default::default()
        });
        Self::new(Box::new(backend), width, height, limits)
    }

    /// Offscreen sprites on the deterministic recording backend.
    pub fn headless(width: u32, height: u32, limits: RenderLimits) -> Result<Self> {
        Self::new(Box::new(RecordingBackend::new()), width, height, limits)
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Description of the backend in use, for example `wgpu/vulkan on <adapter>`.
    pub fn backend_description(&self) -> String {
        let caps = self.renderer.device().capabilities();
        match &caps.device_name {
            Some(device) => format!("{} on {device}", caps.backend_name),
            None => caps.backend_name.clone(),
        }
    }

    pub fn renderer(&self) -> &SpriteRenderer<Box<dyn RenderBackend>> {
        &self.renderer
    }

    pub fn renderer_mut(&mut self) -> &mut SpriteRenderer<Box<dyn RenderBackend>> {
        &mut self.renderer
    }

    /// Render `frame` into the target.
    pub fn render(&mut self, frame: &RenderFrame) -> Result<RenderStats> {
        self.renderer
            .render(frame, RenderTarget::Offscreen(self.target))?
            .ok_or_else(|| anyhow!("the backend skipped an offscreen frame"))
    }

    pub fn render_scene(
        &mut self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
    ) -> Result<RenderStats> {
        self.renderer
            .render_scene(frame, RenderTarget::Offscreen(self.target))?
            .ok_or_else(|| anyhow!("the backend skipped an offscreen frame"))
    }

    /// Wait for the GPU, then return tightly packed RGBA8 rows (top row first).
    pub fn read_rgba(&mut self) -> Result<Vec<u8>> {
        Ok(self.renderer.device_mut().read_target(self.target)?.rgba8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_records_offscreen_without_a_gpu() {
        let mut sprites = OffscreenSprites::headless(64, 64, RenderLimits::default()).unwrap();
        assert_eq!(sprites.size(), (64, 64));
        assert!(!sprites.backend_description().is_empty());
        let frame = RenderFrame {
            camera: engine_project::Camera2D {
                logical_size: [64, 64],
                pixels_per_unit: 1.0,
                ..Default::default()
            },
            draws: vec![],
        };
        assert_eq!(sprites.render(&frame).unwrap(), RenderStats::default());
        assert_eq!(sprites.read_rgba().unwrap().len(), 64 * 64 * 4);
    }

    #[test]
    fn invalid_offscreen_sizes_and_limits_are_rejected() {
        assert!(OffscreenSprites::headless(0, 8, RenderLimits::default()).is_err());
        let limits = RenderLimits {
            max_frames_in_flight: 0,
            ..Default::default()
        };
        assert!(OffscreenSprites::headless(8, 8, limits).is_err());
    }
}
