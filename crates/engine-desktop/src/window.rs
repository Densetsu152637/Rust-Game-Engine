use crate::{
    GpuContext, GpuRenderer, GpuRequirements, RenderLimits, RenderStats, RenderTargetInfo,
};
use anyhow::{Context, Result, bail};
use engine_core::bevy_ecs::world::World;
use engine_project::runtime::RenderFrame;
use std::sync::Arc;
use winit::window::Window;

/// Reusable native surface host for games and the editor. Create on the winit
/// event-loop thread after `resumed`; no platform `any_thread` override is used.
/// The surface owns an Arc to its window, so handles outlive all GPU references.
pub struct WindowSurface {
    pub window: Arc<Window>,
    pub renderer: GpuRenderer,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    suspended: bool,
}

impl WindowSurface {
    pub async fn new(window: Arc<Window>, limits: RenderLimits) -> Result<Self> {
        Self::with_requirements(window, limits, GpuRequirements::default()).await
    }

    pub async fn with_requirements(
        window: Arc<Window>,
        limits: RenderLimits,
        requirements: GpuRequirements,
    ) -> Result<Self> {
        let instance = GpuContext::instance();
        let surface = instance
            .create_surface(window.clone())
            .context("creating window surface")?;
        let gpu =
            GpuContext::request_with_requirements(instance, Some(&surface), requirements).await?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .context("surface has no supported configuration")?;
        let caps = surface.get_capabilities(&gpu.adapter);
        if let Some(format) = caps.formats.iter().find(|f| f.is_srgb()) {
            config.format = *format;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = limits.max_frames_in_flight as u32;
        surface.configure(&gpu.device, &config);
        let renderer = GpuRenderer::new(
            gpu,
            RenderTargetInfo {
                width: config.width,
                height: config.height,
                format: config.format,
            },
            limits,
        )?;
        Ok(Self {
            window,
            renderer,
            surface,
            config,
            suspended: size.width == 0 || size.height == 0,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.suspended = width == 0 || height == 0;
        if self.suspended {
            return Ok(());
        }
        self.renderer.resize(width, height)?;
        self.config.width = width;
        self.config.height = height;
        self.surface
            .configure(&self.renderer.gpu.device, &self.config);
        self.renderer.gpu.check_errors()
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// Timeout skips a frame; lost/outdated surfaces reconfigure and retry on the
    /// next redraw. Out-of-memory and device errors propagate to the owner.
    pub fn present(
        &mut self,
        frame: &RenderFrame,
        world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        if self.suspended {
            return Ok(None);
        }
        let (output, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output) => (output, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => (output, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(None);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.renderer.gpu.check_errors()?;
                self.surface = self
                    .renderer
                    .gpu
                    .instance
                    .create_surface(self.window.clone())
                    .context("recreating lost surface")?;
                self.surface
                    .configure(&self.renderer.gpu.device, &self.config);
                self.renderer.gpu.check_errors()?;
                return Ok(None);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface
                    .configure(&self.renderer.gpu.device, &self.config);
                self.renderer.gpu.check_errors()?;
                return Ok(None);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.renderer.gpu.check_errors()?;
                bail!("validation failure acquiring swapchain image");
            }
        };
        let view = output.texture.create_view(&Default::default());
        let stats = self.renderer.render(frame, &view, world)?;
        self.window.pre_present_notify();
        self.renderer.gpu.queue.present(output);
        if suboptimal {
            self.surface
                .configure(&self.renderer.gpu.device, &self.config);
        }
        self.renderer.gpu.check_errors()?;
        Ok(Some(stats))
    }
}
