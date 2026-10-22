//! Window swapchain handling.

use crate::context::GpuContext;
use crate::error::{backend_error, check_gpu};
use crate::translate::choose_surface_format;
use engine_render::{FrameSkip, RenderError, Result};
use std::sync::Arc;

/// Anything a wgpu surface can be created from (winit windows implement this).
pub trait WindowSource: wgpu::WindowHandle + wgpu::rwh::HasDisplayHandle {}
impl<T: wgpu::WindowHandle + wgpu::rwh::HasDisplayHandle> WindowSource for T {}

pub(crate) type WindowRef = Arc<dyn WindowSource>;

pub(crate) struct SurfaceState {
    pub window: WindowRef,
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub suspended: bool,
}

/// Outcome of trying to acquire the next swapchain image.
pub(crate) enum Acquired {
    Frame {
        output: wgpu::SurfaceTexture,
        suboptimal: bool,
    },
    Skip(FrameSkip),
}

impl SurfaceState {
    /// Configure a swapchain for `window` of the given initial `size` (a zero
    /// size starts suspended until the first nonzero resize).
    pub fn configure(
        gpu: &GpuContext,
        window: WindowRef,
        surface: wgpu::Surface<'static>,
        size: (u32, u32),
        frames_in_flight: u32,
    ) -> Result<Self> {
        let mut config = surface
            .get_default_config(&gpu.adapter, size.0.max(1), size.1.max(1))
            .ok_or_else(|| {
                RenderError::Unavailable("the window surface has no supported configuration".into())
            })?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        if let Some(format) = choose_surface_format(&capabilities.formats) {
            config.format = format;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = frames_in_flight;
        surface.configure(&gpu.device, &config);
        check_gpu(gpu)?;
        Ok(Self {
            window,
            surface,
            config,
            suspended: size.0 == 0 || size.1 == 0,
        })
    }

    pub fn reconfigure(&self, gpu: &GpuContext) -> Result<()> {
        self.surface.configure(&gpu.device, &self.config);
        check_gpu(gpu)
    }

    pub fn resize(&mut self, gpu: &GpuContext, width: u32, height: u32) -> Result<()> {
        self.suspended = width == 0 || height == 0;
        if self.suspended {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.reconfigure(gpu)
    }

    /// Acquire the next image. Lost and outdated surfaces are reconfigured and
    /// reported as [`FrameSkip::OutOfDate`] so the caller retries next frame.
    pub fn acquire(&mut self, gpu: &GpuContext) -> Result<Acquired> {
        Ok(match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output) => Acquired::Frame {
                output,
                suboptimal: false,
            },
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => Acquired::Frame {
                output,
                suboptimal: true,
            },
            wgpu::CurrentSurfaceTexture::Timeout => Acquired::Skip(FrameSkip::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => Acquired::Skip(FrameSkip::Occluded),
            wgpu::CurrentSurfaceTexture::Lost => {
                check_gpu(gpu)?;
                self.surface = gpu
                    .instance
                    .create_surface(self.window.clone())
                    .map_err(|error| backend_error("recreating lost surface", error))?;
                self.reconfigure(gpu)?;
                Acquired::Skip(FrameSkip::OutOfDate)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.reconfigure(gpu)?;
                Acquired::Skip(FrameSkip::OutOfDate)
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                check_gpu(gpu)?;
                return Err(RenderError::Backend(
                    "validation failure acquiring swapchain image".into(),
                ));
            }
        })
    }
}
