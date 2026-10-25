use crate::{
    DlssConfig, GpuContext, GpuRenderer, GpuRequirements, RenderLimits, RenderStats,
    RenderTargetInfo, WgpuOptions,
};
use anyhow::{Context, Result, bail};
use engine_core::bevy_ecs::world::World;
use engine_project::runtime::RenderFrame;
use std::{sync::Arc, time::Instant};
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
    pacer: crate::PresentPacer,
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
        options: WgpuOptions,
    ) -> Result<Self> {
        Self::with_upscaler(
            window,
            limits,
            options,
            &DlssConfig::default(),
            crate::DlssRequest::NONE,
        )
        .await
    }

    /// [`Self::with_options`] that, for a DLSS `request`, first attempts a
    /// DLSS-capable device ([`crate::request_upscaler_gpu`]). The renderer
    /// records whether DLSS can run and why not.
    pub async fn with_upscaler(
        window: Arc<Window>,
        limits: RenderLimits,
        options: WgpuOptions,
        dlss: &DlssConfig,
        request: crate::DlssRequest,
    ) -> Result<Self> {
        Self::with_ray_tracing(window, limits, options, dlss, request, None).await
    }

    /// [`Self::with_upscaler`] that also adds the `engine-render-rt` device
    /// request when `frame` (the first frame) enables a `RayTracing` effect
    /// on its camera ([`crate::prepare_ray_tracing`]).
    pub async fn with_ray_tracing(
        window: Arc<Window>,
        limits: RenderLimits,
        mut options: WgpuOptions,
        dlss: &DlssConfig,
        request: crate::DlssRequest,
        frame: Option<&engine_project::rendering3d::SceneRenderFrame>,
    ) -> Result<Self> {
        options.frames_in_flight = limits.max_frames_in_flight as u32;
        let instance = GpuContext::instance_with(options.backends);
        let surface = instance
            .create_surface(window.clone())
            .context("creating window surface")?;
        let ray_tracing = match frame {
            Some(frame) => {
                crate::prepare_ray_tracing(&instance, Some(&surface), &mut options, frame).await
            }
            None => None,
        };
        let (gpu, upscaler) =
            crate::request_upscaler_gpu(instance, Some(&surface), &options, dlss, request).await?;
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
        let renderer = GpuRenderer::with_upscaler_device(
            gpu,
            RenderTargetInfo {
                width: config.width,
                height: config.height,
                format: config.format,
            },
            limits,
            upscaler,
        )?;
        let mut renderer = renderer;
        renderer.note_ray_tracing_request(ray_tracing);
        Ok(Self {
            window,
            renderer,
            surface,
            config,
            suspended: size.width == 0 || size.height == 0,
            pacer: crate::PresentPacer::default(),
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
        self.present_scene(&frame.clone().into(), world)
    }

    /// Present one real frame. With DLSS Frame Generation active, the
    /// generated frames are presented first, in order, each on its own
    /// swapchain image, then the real frame, paced evenly over the recent
    /// real-frame interval ([`crate::presentation_offsets`]). Returns `None`
    /// when the real frame was not presented (suspended, occluded, lost).
    pub fn present_scene(
        &mut self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
        world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        if self.suspended {
            return Ok(None);
        }
        if let Some(stats) = self.renderer.render_generated(frame)? {
            let generated = self.renderer.generated_frame_count();
            let start = Instant::now();
            let offsets = crate::presentation_offsets(self.pacer.begin(start), generated);
            let mut shown = 0;
            for (index, offset) in offsets.iter().take(generated).enumerate() {
                sleep_until(start + *offset);
                if self.present_with(|renderer, view| renderer.present_generated(index, view))? {
                    shown += 1;
                }
            }
            sleep_until(start + offsets[generated]);
            let real = self.present_with(|renderer, view| renderer.present_real(view))?;
            self.renderer.note_generated_presented(shown);
            return Ok(real.then_some(stats));
        }
        self.pacer.reset();
        let mut stats = None;
        self.present_with(|renderer, view| {
            stats = Some(renderer.render_scene_without_generation(frame, view, world)?);
            Ok(())
        })?;
        Ok(stats)
    }

    /// Acquire a swapchain image, draw into it and present it. `false` when no
    /// image could be acquired (timeout, occlusion, lost/outdated surface).
    fn present_with(
        &mut self,
        draw: impl FnOnce(&mut GpuRenderer, &wgpu::TextureView) -> Result<()>,
    ) -> Result<bool> {
        let (output, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output) => (output, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => (output, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
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
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface
                    .configure(&self.renderer.gpu.device, &self.config);
                self.renderer.gpu.check_errors()?;
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.renderer.gpu.check_errors()?;
                bail!("validation failure acquiring swapchain image");
            }
        };
        let view = output.texture.create_view(&Default::default());
        draw(&mut self.renderer, &view)?;
        self.window.pre_present_notify();
        self.renderer.gpu.queue.present(output);
        if suboptimal {
            self.surface
                .configure(&self.renderer.gpu.device, &self.config);
        }
        self.renderer.gpu.check_errors()?;
        Ok(true)
    }
}

/// Sleep until `deadline` (no-op when it passed): the simple pacing baseline.
fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        std::thread::sleep(deadline - now);
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
