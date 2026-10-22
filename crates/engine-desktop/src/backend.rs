//! Backend-neutral hosts: a backend factory, a window host and an offscreen
//! helper, all built on [`SpriteRenderer`].
//!
//! The wgpu-native path ([`crate::WindowSurface`] + [`crate::GpuRenderer`]) is
//! the only one that runs [`crate::NativeRenderPass`]es; everything here works
//! with any `engine-render` backend.

use crate::{RenderLimits, RenderStats, SpriteRenderer};
use anyhow::{Context, Result, anyhow, bail};
use engine_project::runtime::RenderFrame;
use engine_render::{BackendKind, RenderBackend, RenderTarget, TargetHandle};
use engine_render_wgpu::{WgpuBackend, WgpuOptions};
use std::sync::Arc;
use winit::window::Window;

/// Create the backend of `kind`.
///
/// * `window = Some((window, size))`: a backend presenting to that window, whose
///   inner size is `size`.
/// * `window = None`: an offscreen-only backend.
///
/// [`BackendKind::Headless`] has no presentation surface: with a window it is an
/// error ("not presentable"), without one it is the recording backend of
/// `engine-render` (no GPU, useful for tests). Selecting
/// [`BackendKind::Vulkan`] in a build without the `vulkan` cargo feature is an
/// error that says so.
pub fn create_backend(
    kind: BackendKind,
    window: Option<(Arc<Window>, (u32, u32))>,
    limits: &RenderLimits,
) -> Result<Box<dyn RenderBackend>> {
    let frames = limits.max_frames_in_flight as u32;
    match kind {
        BackendKind::Wgpu => {
            let options = WgpuOptions {
                frames_in_flight: frames,
                ..Default::default()
            };
            Ok(match window {
                Some((window, size)) => Box::new(WgpuBackend::for_window(window, size, options)),
                None => Box::new(WgpuBackend::offscreen(options)),
            })
        }
        BackendKind::Vulkan => create_vulkan(window, frames),
        BackendKind::Headless => {
            if window.is_some() {
                bail!("the headless backend is not presentable: it has no window surface");
            }
            Ok(Box::new(engine_render::headless::RecordingBackend::new()))
        }
    }
}

#[cfg(feature = "vulkan")]
fn create_vulkan(
    window: Option<(Arc<Window>, (u32, u32))>,
    frames: u32,
) -> Result<Box<dyn RenderBackend>> {
    use engine_vulkan::{VulkanBackend, VulkanConfig};
    let config = VulkanConfig {
        frames_in_flight: frames,
        ..Default::default()
    };
    Ok(match window {
        Some((window, size)) => Box::new(VulkanBackend::for_window(window, size, config)),
        None => Box::new(VulkanBackend::offscreen(config)),
    })
}

#[cfg(not(feature = "vulkan"))]
fn create_vulkan(
    _window: Option<(Arc<Window>, (u32, u32))>,
    _frames: u32,
) -> Result<Box<dyn RenderBackend>> {
    Err(anyhow!(
        "the Vulkan backend is not compiled in: build engine-desktop with the `vulkan` cargo feature"
    ))
}

/// A winit window presenting through any [`RenderBackend`].
///
/// Create on the winit event-loop thread after `resumed`. Suspension follows
/// [`crate::WindowSurface`]: a zero-sized window is suspended and presents
/// nothing; occluded or timed-out frames are skipped by the backend and report
/// `None` from [`Self::present`].
pub struct BackendWindow {
    pub window: Arc<Window>,
    pub renderer: SpriteRenderer<Box<dyn RenderBackend>>,
    suspended: bool,
}

impl BackendWindow {
    /// Open `window` with the backend of `kind` (see [`create_backend`]).
    pub fn new(kind: BackendKind, window: Arc<Window>, limits: RenderLimits) -> Result<Self> {
        let size = window.inner_size();
        let backend = create_backend(
            kind,
            Some((window.clone(), (size.width, size.height))),
            &limits,
        )?;
        Self::with_backend(window, backend, limits)
    }

    /// Open `window` with a backend created for it (for example by
    /// [`create_backend`] or a custom implementation).
    pub fn with_backend(
        window: Arc<Window>,
        backend: Box<dyn RenderBackend>,
        limits: RenderLimits,
    ) -> Result<Self> {
        let size = window.inner_size();
        let mut renderer = SpriteRenderer::new(backend, limits)
            .context("initializing the render backend for the window")?;
        renderer.set_surface_size((size.width, size.height));
        Ok(Self {
            window,
            renderer,
            suspended: size.width == 0 || size.height == 0,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.suspended = width == 0 || height == 0;
        if self.suspended {
            return Ok(());
        }
        self.renderer.resize(width, height)
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// Render and present one frame. `None` when suspended or when the backend
    /// skipped the frame (occluded, timeout, swapchain recreated).
    pub fn present(&mut self, frame: &RenderFrame) -> Result<Option<RenderStats>> {
        if self.suspended {
            return Ok(None);
        }
        self.window.pre_present_notify();
        self.renderer.render(frame, RenderTarget::Surface)
    }

    /// Wait for the GPU to finish all submitted work.
    pub fn wait_idle(&mut self) -> Result<()> {
        Ok(self.renderer.device_mut().wait_idle()?)
    }
}

/// Sprite rendering into an offscreen RGBA target on any backend, with readback.
/// The same path a window uses, for tests and tools.
pub struct OffscreenSprites {
    renderer: SpriteRenderer<Box<dyn RenderBackend>>,
    target: TargetHandle,
    size: (u32, u32),
}

impl OffscreenSprites {
    pub fn new(kind: BackendKind, width: u32, height: u32, limits: RenderLimits) -> Result<Self> {
        let backend = create_backend(kind, None, &limits)?;
        let mut renderer = SpriteRenderer::new(backend, limits)?;
        let target = renderer.device_mut().create_target(width, height)?;
        Ok(Self {
            renderer,
            target,
            size: (width, height),
        })
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

    /// Wait for the GPU, then return tightly packed RGBA8 rows (top row first).
    pub fn read_rgba(&mut self) -> Result<Vec<u8>> {
        Ok(self.renderer.device_mut().read_target(self.target)?.rgba8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_has_no_window_surface_but_records_offscreen() {
        let limits = RenderLimits::default();
        assert!(create_backend(BackendKind::Headless, None, &limits).is_ok());
        let mut sprites = OffscreenSprites::new(BackendKind::Headless, 64, 64, limits).unwrap();
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
        assert!(
            OffscreenSprites::new(BackendKind::Headless, 0, 8, RenderLimits::default()).is_err()
        );
        let limits = RenderLimits {
            max_frames_in_flight: 0,
            ..Default::default()
        };
        assert!(OffscreenSprites::new(BackendKind::Headless, 8, 8, limits).is_err());
    }

    #[cfg(not(feature = "vulkan"))]
    #[test]
    fn selecting_vulkan_without_the_feature_names_the_feature() {
        let error = create_backend(BackendKind::Vulkan, None, &RenderLimits::default())
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("`vulkan` cargo feature"),
            "{error}"
        );
    }
}
