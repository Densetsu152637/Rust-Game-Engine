//! Offscreen rendering into an RGBA target, with readback.

use crate::{GpuRenderer, RenderLimits, RenderStats, SpriteRenderer};
use anyhow::{Result, anyhow};
use engine_project::runtime::RenderFrame;
use engine_render::{RenderBackend, RenderTarget, TargetHandle, headless::RecordingBackend};
use engine_render_wgpu::{GpuContext, OffscreenTarget, WgpuBackends, WgpuOptions};

enum Engine {
    /// Any `engine-render` backend (the recording backend in tests): sprites
    /// are drawn, 3D meshes are validated but not drawn.
    Neutral {
        renderer: Box<SpriteRenderer<Box<dyn RenderBackend>>>,
        target: TargetHandle,
    },
    /// The native renderer: the full 3D pipeline plus sprites.
    Gpu {
        renderer: Box<GpuRenderer>,
        target: OffscreenTarget,
    },
}

/// Rendering into an offscreen RGBA target, the same path a window uses,
/// for tests and tools. Build it on a real GPU with [`Self::wgpu`] (a
/// [`GpuRenderer`]: 3D render pipeline and sprites), or on the deterministic
/// recording backend with [`Self::headless`] (no GPU; sprite commands are
/// recorded and 3D scenes are validated, not drawn; inspect the command
/// stream through the log of the backend passed to [`Self::new`]).
pub struct OffscreenSprites {
    engine: Engine,
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
            engine: Engine::Neutral {
                renderer: Box::new(renderer),
                target,
            },
            size: (width, height),
        })
    }

    /// The native renderer on a headless device of the `backends` APIs.
    pub fn wgpu(
        backends: WgpuBackends,
        width: u32,
        height: u32,
        limits: RenderLimits,
    ) -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions {
            backends,
            frames_in_flight: limits.max_frames_in_flight as u32,
            ..Default::default()
        })?;
        let target = OffscreenTarget::new(&gpu, width, height)?;
        let renderer = GpuRenderer::new(gpu, target.info, limits)?;
        Ok(Self {
            engine: Engine::Gpu {
                renderer: Box::new(renderer),
                target,
            },
            size: (width, height),
        })
    }

    /// [`Self::wgpu`] on a device created with the `engine-render-rt`
    /// request when the adapter supports hardware ray query (otherwise the
    /// ordinary device; ray-traced effects then report the adapter's reason).
    pub fn wgpu_ray_traced(
        backends: WgpuBackends,
        width: u32,
        height: u32,
        limits: RenderLimits,
    ) -> Result<Self> {
        let mut options = WgpuOptions {
            backends,
            frames_in_flight: limits.max_frames_in_flight as u32,
            ..Default::default()
        };
        let instance = GpuContext::instance_with(backends);
        let request =
            match pollster::block_on(GpuContext::request_adapter(&instance, None, &options)) {
                Ok(adapter) => crate::request_ray_tracing(&mut options, &adapter),
                Err(error) => Err(engine_render_rt::RayTracingStatus {
                    tier: engine_render_rt::RayTracingTier::Unavailable,
                    cause: Some(engine_render_rt::RtUnavailableCause::NoAdapter),
                    reason: Some(format!("{error:#}")),
                    adapter: None,
                }),
            };
        let gpu = pollster::block_on(GpuContext::request_with_options(instance, None, &options))?;
        let target = OffscreenTarget::new(&gpu, width, height)?;
        let mut renderer = GpuRenderer::new(gpu, target.info, limits)?;
        renderer.note_ray_tracing_request(Some(request));
        Ok(Self {
            engine: Engine::Gpu {
                renderer: Box::new(renderer),
                target,
            },
            size: (width, height),
        })
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
        match &self.engine {
            Engine::Neutral { renderer, .. } => {
                let caps = renderer.device().capabilities();
                match &caps.device_name {
                    Some(device) => format!("{} on {device}", caps.backend_name),
                    None => caps.backend_name.clone(),
                }
            }
            Engine::Gpu { renderer, .. } => {
                let info = renderer.gpu.adapter.get_info();
                format!(
                    "{} on {}",
                    engine_render_wgpu::translate::backend_label(info.backend),
                    info.name
                )
            }
        }
    }

    /// The native renderer of a [`Self::wgpu`] target (register render
    /// features, read pipeline status); `None` on neutral backends.
    pub fn gpu_renderer(&mut self) -> Option<&mut GpuRenderer> {
        match &mut self.engine {
            Engine::Gpu { renderer, .. } => Some(renderer),
            Engine::Neutral { .. } => None,
        }
    }

    /// Live resource counters of the sprite device.
    pub fn sprite_resource_counts(&self) -> engine_render::ResourceCounts {
        match &self.engine {
            Engine::Neutral { renderer, .. } => renderer.device().counts(),
            Engine::Gpu { renderer, .. } => renderer.sprite_resource_counts(),
        }
    }

    /// Render `frame` into the target.
    pub fn render(&mut self, frame: &RenderFrame) -> Result<RenderStats> {
        match &mut self.engine {
            Engine::Neutral { renderer, target } => renderer
                .render(frame, RenderTarget::Offscreen(*target))?
                .ok_or_else(|| anyhow!("the backend skipped an offscreen frame")),
            Engine::Gpu { renderer, target } => renderer.render(frame, &target.view, None),
        }
    }

    pub fn render_scene(
        &mut self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
    ) -> Result<RenderStats> {
        match &mut self.engine {
            Engine::Neutral { renderer, target } => renderer
                .render_scene(frame, RenderTarget::Offscreen(*target))?
                .ok_or_else(|| anyhow!("the backend skipped an offscreen frame")),
            Engine::Gpu { renderer, target } => renderer.render_scene(frame, &target.view, None),
        }
    }

    /// Wait for the GPU, then return tightly packed RGBA8 rows (top row first).
    pub fn read_rgba(&mut self) -> Result<Vec<u8>> {
        match &mut self.engine {
            Engine::Neutral { renderer, target } => {
                Ok(renderer.device_mut().read_target(*target)?.rgba8)
            }
            Engine::Gpu { renderer, target } => target.read_rgba(&renderer.gpu),
        }
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
        assert!(sprites.gpu_renderer().is_none());
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
