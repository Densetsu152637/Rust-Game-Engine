//! [`WgpuBackend`]: the [`RenderBackend`] implementation on wgpu.

use crate::context::{GpuContext, WgpuOptions};
use crate::core::Core;
use crate::error::{backend_error, check_gpu};
use crate::frame::Destination;
use crate::surface::{SurfaceState, WindowRef, WindowSource};
use crate::target::RenderTargetInfo;
use engine_render::{
    BackendKind, DrawCommand, FrameDesc, FrameOutcome, FrameStats, MeshHandle, MeshResource,
    OffscreenDesc, ReadbackImage, RenderBackend, RenderCapabilities, RenderError, RenderFeature,
    ResourceId, Result, ShaderHandle, ShaderResource, TargetHandle, Texture2D, TextureHandle,
};
use std::sync::Arc;

enum Source {
    Offscreen,
    Window { window: WindowRef, size: (u32, u32) },
    Context(GpuContext),
}

/// Counters describing the live state of an initialized backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WgpuStats {
    pub shaders: usize,
    pub meshes: usize,
    pub textures: usize,
    pub targets: usize,
    pub pipelines: usize,
    pub bind_groups: usize,
    /// Size of the per-frame uniform arena buffer (0 before the first uniform).
    pub uniform_arena_bytes: u64,
}

/// The `engine-render` backend on wgpu. Wrap it in an
/// [`engine_render::RenderDevice`] for handle management and validation.
pub struct WgpuBackend {
    options: WgpuOptions,
    source: Option<Source>,
    core: Option<Core>,
    shut_down: bool,
}

impl WgpuBackend {
    /// Backend without a presentation surface (offscreen targets only).
    pub fn offscreen(options: WgpuOptions) -> Self {
        Self::build(options, Source::Offscreen)
    }

    /// Backend presenting to `window`, whose current inner size is `size`. The
    /// backend owns the surface and keeps the window alive. Nothing is created
    /// until [`RenderBackend::initialize`].
    pub fn for_window<W: WindowSource + 'static>(
        window: Arc<W>,
        size: (u32, u32),
        options: WgpuOptions,
    ) -> Self {
        let window: WindowRef = window;
        Self::build(options, Source::Window { window, size })
    }

    /// Backend sharing an existing device (offscreen and external targets only;
    /// the options' backend, preference and requirements are ignored).
    pub fn from_context(gpu: GpuContext) -> Self {
        Self::from_context_with(gpu, WgpuOptions::default())
    }

    /// [`Self::from_context`] with explicit frames in flight.
    pub fn from_context_with(gpu: GpuContext, options: WgpuOptions) -> Self {
        Self::build(options, Source::Context(gpu))
    }

    fn build(options: WgpuOptions, source: Source) -> Self {
        Self {
            options,
            source: Some(source),
            core: None,
            shut_down: false,
        }
    }

    fn ready(&mut self) -> Result<&mut Core> {
        let message = if self.shut_down {
            "the wgpu backend was shut down"
        } else {
            "the wgpu backend is not initialized"
        };
        self.core
            .as_mut()
            .ok_or_else(|| RenderError::NotReady(message.into()))
    }

    /// The underlying device, once initialized.
    pub fn gpu(&self) -> &GpuContext {
        &self
            .core
            .as_ref()
            .expect("WgpuBackend::gpu requires an initialized backend")
            .gpu
    }

    /// The underlying device, or `None` before `initialize` and after `shutdown`.
    pub fn try_gpu(&self) -> Option<&GpuContext> {
        self.core.as_ref().map(|core| &core.gpu)
    }

    /// Live resource and cache counters; `None` before `initialize`.
    pub fn stats(&self) -> Option<WgpuStats> {
        self.core.as_ref().map(|core| WgpuStats {
            shaders: core.shaders.len(),
            meshes: core.meshes.len(),
            textures: core.textures.len(),
            targets: core.targets.len(),
            pipelines: core.pipelines.len(),
            bind_groups: core.bind_groups.len(),
            uniform_arena_bytes: core.arena.as_ref().map_or(0, |arena| arena.capacity),
        })
    }

    /// Render the neutral `draws` into a caller-supplied color `view` (for
    /// example a surface texture owned by another host). `frame.target` is
    /// ignored; the view's size and format come from `info`. The backend owns a
    /// depth attachment per size. The frame is submitted before this returns, so
    /// later submissions on the same queue (custom passes) run after it.
    pub fn render_to_view(
        &mut self,
        view: &wgpu::TextureView,
        info: RenderTargetInfo,
        frame: &FrameDesc,
        draws: &[DrawCommand],
    ) -> Result<FrameStats> {
        let core = self.ready()?;
        let destination = Destination {
            view: view.clone(),
            depth: core.depth_for(info.width, info.height),
            width: info.width,
            height: info.height,
            format: info.format,
        };
        core.draw_frame(&destination, frame, draws)
    }

    fn uninitialized_capabilities(&self) -> RenderCapabilities {
        let mut caps = RenderCapabilities::full(BackendKind::Wgpu, "wgpu");
        caps.backend_ready = false;
        caps.max_frames_in_flight = self.options.frames_in_flight.clamp(1, 8);
        caps.surface_presentation = matches!(self.source, Some(Source::Window { .. }));
        caps.unsupported_reasons.insert(
            RenderFeature::BackendAvailable,
            "initialize has not completed".into(),
        );
        if !caps.surface_presentation {
            caps.unsupported_reasons.insert(
                RenderFeature::SurfacePresentation,
                "the backend was created without a window (offscreen only)".into(),
            );
        }
        caps
    }
}

fn unavailable(error: anyhow::Error) -> RenderError {
    RenderError::Unavailable(format!("{error:#}"))
}

impl RenderBackend for WgpuBackend {
    fn capabilities(&self) -> RenderCapabilities {
        match &self.core {
            Some(core) => core.caps.clone(),
            None => self.uninitialized_capabilities(),
        }
    }

    fn initialize(&mut self) -> Result<()> {
        if self.core.is_some() {
            return Ok(());
        }
        if self.shut_down {
            return Err(RenderError::NotReady(
                "the wgpu backend was shut down".into(),
            ));
        }
        let frames = self.options.frames_in_flight;
        let core = match self.source.as_ref() {
            Some(Source::Offscreen) => {
                let gpu = GpuContext::headless_with_options(&self.options).map_err(unavailable)?;
                Core::new(gpu, None, frames)?
            }
            Some(Source::Context(gpu)) => Core::new(gpu.clone(), None, frames)?,
            Some(Source::Window { window, size }) => {
                let instance = GpuContext::instance_with(self.options.backends);
                let surface = instance
                    .create_surface(window.clone())
                    .map_err(|error| backend_error("creating the window surface", error))?;
                let gpu = pollster::block_on(GpuContext::request_with_options(
                    instance,
                    Some(&surface),
                    &self.options,
                ))
                .map_err(unavailable)?;
                let state = SurfaceState::configure(
                    &gpu,
                    window.clone(),
                    surface,
                    *size,
                    frames.clamp(1, 8),
                )?;
                Core::new(gpu, Some(state), frames)?
            }
            None => {
                return Err(RenderError::NotReady(
                    "the wgpu backend has no device source".into(),
                ));
            }
        };
        self.core = Some(core);
        Ok(())
    }

    fn create_shader(&mut self, handle: ShaderHandle, resource: &ShaderResource) -> Result<()> {
        self.ready()?.create_shader(handle, resource)
    }

    fn create_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        self.ready()?.create_mesh(handle, resource)
    }

    fn create_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        self.ready()?.create_texture(handle, texture)
    }

    fn create_target(&mut self, handle: TargetHandle, desc: &OffscreenDesc) -> Result<()> {
        self.ready()?.create_target(handle, desc)
    }

    fn update_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        self.ready()?.update_mesh(handle, resource)
    }

    fn update_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        self.ready()?.update_texture(handle, texture)
    }

    fn destroy_resource(&mut self, id: ResourceId) -> Result<()> {
        match self.core.as_mut() {
            Some(core) => core.destroy_resource(id),
            // Everything went away with the device at shutdown.
            None => Ok(()),
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.ready()?.resize_surface(width, height)
    }

    fn submit_frame(&mut self, frame: &FrameDesc, draws: &[DrawCommand]) -> Result<FrameOutcome> {
        self.ready()?.submit_frame(frame, draws)
    }

    fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage> {
        self.ready()?.read_target(target)
    }

    fn wait_idle(&mut self) -> Result<()> {
        match self.core.as_mut() {
            Some(core) => core.wait_idle(),
            None => Ok(()),
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        let result = match self.core.as_mut() {
            Some(core) => core.wait_idle(),
            None => Ok(()),
        };
        // Dropping the core releases every GPU object; the surface goes before
        // the window it was created from.
        self.core = None;
        self.source = None;
        self.shut_down = true;
        result
    }
}

impl Drop for WgpuBackend {
    fn drop(&mut self) {
        if let Some(core) = &self.core {
            let _ = check_gpu(&core.gpu);
            let _ = core.gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(5)),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn backend_can_move_between_threads() {
        assert_send::<WgpuBackend>();
    }

    #[test]
    fn capabilities_before_initialize_say_not_ready_and_why() {
        let backend = WgpuBackend::offscreen(WgpuOptions {
            frames_in_flight: 40,
            ..Default::default()
        });
        let caps = backend.capabilities();
        assert_eq!(caps.kind, BackendKind::Wgpu);
        assert!(!caps.backend_ready);
        assert!(
            !caps
                .feature(RenderFeature::BackendAvailable)
                .reason
                .is_empty()
        );
        assert_eq!(caps.max_frames_in_flight, 8, "clamped to 8");
        assert!(!caps.surface_presentation);
        assert!(
            caps.feature(RenderFeature::SurfacePresentation)
                .reason
                .contains("without a window")
        );
        let zero = WgpuBackend::offscreen(WgpuOptions {
            frames_in_flight: 0,
            ..Default::default()
        });
        assert_eq!(zero.capabilities().max_frames_in_flight, 1);
    }

    #[test]
    fn calls_before_initialize_are_not_ready_but_cleanup_calls_succeed() {
        let mut backend = WgpuBackend::offscreen(WgpuOptions::default());
        assert!(matches!(
            backend.create_texture(TextureHandle::from_raw(1), &Texture2D::white()),
            Err(RenderError::NotReady(_))
        ));
        assert!(matches!(
            backend.submit_frame(&FrameDesc::new(engine_render::RenderTarget::Surface), &[]),
            Err(RenderError::NotReady(_))
        ));
        backend.wait_idle().unwrap();
        backend
            .destroy_resource(ResourceId::Texture(TextureHandle::from_raw(1)))
            .unwrap();
        assert!(backend.stats().is_none());
        assert!(backend.try_gpu().is_none());
    }

    #[test]
    fn shutdown_is_idempotent_and_blocks_later_initialize() {
        let mut backend = WgpuBackend::offscreen(WgpuOptions::default());
        backend.shutdown().unwrap();
        backend.shutdown().unwrap();
        assert!(matches!(
            backend.initialize(),
            Err(RenderError::NotReady(_))
        ));
        assert!(matches!(
            backend.read_target(TargetHandle::from_raw(1)),
            Err(RenderError::NotReady(m)) if m.contains("shut down")
        ));
    }
}
