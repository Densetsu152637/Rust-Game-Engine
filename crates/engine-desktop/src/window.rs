use crate::{
    GpuContext, GpuRenderer, GpuRequirements, RenderLimits, RenderStats, RenderTargetInfo,
    WgpuOptions,
};
use anyhow::{Context, Result, bail};
use engine_core::bevy_ecs::world::World;
use engine_project::runtime::RenderFrame;
use std::sync::Arc;
use winit::window::Window;

/// Environment variable that selects the swapchain present mode of window
/// surfaces: `fifo` (vsync, the default), `mailbox`, `immediate` or `auto`
/// (the platform's lowest-latency mode, without vsync). Mailbox, immediate and
/// auto let presentation run faster than the display, which is how to measure
/// uncapped frame rates on a 60 Hz screen; a mode the surface does not support
/// falls back to `fifo`.
pub const PRESENT_MODE_ENV_VAR: &str = "ENGINE_PRESENT_MODE";

/// Pick the present mode for `value` (the content of [`PRESENT_MODE_ENV_VAR`],
/// unset or blank meaning vsync) among the modes the surface `supported`.
/// Unknown values are an error listing the accepted ones.
pub fn resolve_present_mode(
    value: Option<&str>,
    supported: &[wgpu::PresentMode],
) -> Result<wgpu::PresentMode> {
    let wanted = match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("" | "fifo" | "vsync") => return Ok(wgpu::PresentMode::Fifo),
        Some("mailbox") => wgpu::PresentMode::Mailbox,
        Some("immediate") => wgpu::PresentMode::Immediate,
        Some("auto" | "novsync") => wgpu::PresentMode::AutoNoVsync,
        Some(other) => bail!(
            "{PRESENT_MODE_ENV_VAR}={other:?} is not a present mode; \
             expected fifo, mailbox, immediate or auto"
        ),
    };
    // The auto modes are always available and resolve inside wgpu.
    let usable = wanted == wgpu::PresentMode::AutoNoVsync || supported.contains(&wanted);
    Ok(if usable {
        wanted
    } else {
        wgpu::PresentMode::Fifo
    })
}

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
        let options = WgpuOptions {
            requirements,
            ..Default::default()
        };
        Self::with_options(window, limits, options).await
    }

    /// Like [`Self::with_requirements`] with an explicit wgpu graphics API
    /// selection (`options.backends`), power preference and requirements.
    /// `options.frames_in_flight` is replaced by `limits.max_frames_in_flight`.
    pub async fn with_options(
        window: Arc<Window>,
        limits: RenderLimits,
        mut options: WgpuOptions,
    ) -> Result<Self> {
        options.frames_in_flight = limits.max_frames_in_flight as u32;
        let instance = GpuContext::instance_with(options.backends);
        let surface = instance
            .create_surface(window.clone())
            .context("creating window surface")?;
        let gpu = GpuContext::request_with_options(instance, Some(&surface), &options).await?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .context("surface has no supported configuration")?;
        let caps = surface.get_capabilities(&gpu.adapter);
        if let Some(format) = caps.formats.iter().find(|f| f.is_srgb()) {
            config.format = *format;
        }
        config.present_mode = resolve_present_mode(
            std::env::var(PRESENT_MODE_ENV_VAR).ok().as_deref(),
            &caps.present_modes,
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::PresentMode::*;

    #[test]
    fn unset_blank_and_vsync_values_use_fifo() {
        for value in [None, Some(""), Some("  "), Some("fifo"), Some("VSync")] {
            assert_eq!(resolve_present_mode(value, &[Fifo, Mailbox]).unwrap(), Fifo);
        }
    }

    #[test]
    fn named_modes_are_used_when_the_surface_supports_them() {
        let supported = [Fifo, Mailbox, Immediate];
        assert_eq!(
            resolve_present_mode(Some("Mailbox"), &supported).unwrap(),
            Mailbox
        );
        assert_eq!(
            resolve_present_mode(Some(" immediate "), &supported).unwrap(),
            Immediate
        );
        assert_eq!(
            resolve_present_mode(Some("auto"), &[Fifo]).unwrap(),
            AutoNoVsync
        );
    }

    #[test]
    fn an_unsupported_mode_falls_back_to_fifo() {
        assert_eq!(
            resolve_present_mode(Some("mailbox"), &[Fifo]).unwrap(),
            Fifo
        );
        assert_eq!(
            resolve_present_mode(Some("immediate"), &[Fifo, Mailbox]).unwrap(),
            Fifo
        );
    }

    #[test]
    fn unknown_values_name_the_variable_and_the_accepted_modes() {
        let error = resolve_present_mode(Some("turbo"), &[Fifo])
            .unwrap_err()
            .to_string();
        assert!(error.contains(PRESENT_MODE_ENV_VAR), "{error}");
        assert!(
            error.contains("fifo, mailbox, immediate or auto"),
            "{error}"
        );
    }
}
