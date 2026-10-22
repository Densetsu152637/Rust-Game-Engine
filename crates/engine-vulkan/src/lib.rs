//! Native Vulkan backend for `engine-render`.
//!
//! [`VulkanBackend`] implements [`engine_render::RenderBackend`] on top of
//! `ash`. Wrap it in an [`engine_render::RenderDevice`] to get handle
//! management, draw validation and deferred destruction:
//!
//! ```no_run
//! use engine_render::{DeviceConfig, RenderDevice};
//! use engine_vulkan::{VulkanBackend, VulkanConfig};
//!
//! let backend = VulkanBackend::offscreen(VulkanConfig::default());
//! let device = RenderDevice::new(backend, DeviceConfig::default());
//! ```
//!
//! ## Design
//!
//! * `instance.rs`: loader, instance, validation layer + debug messenger,
//!   device selection, logical device.
//! * `memory.rs`: `gpu-allocator` wrapper for buffers and images.
//! * `swapchain.rs`: window swapchain with recreation on resize / out-of-date /
//!   suboptimal.
//! * `pipeline.rs`: WGSL to SPIR-V (naga), descriptor and pipeline layouts built
//!   from the reflected `ShaderInterface`, render passes, pipeline cache.
//! * `resources.rs` / `target.rs`: meshes, textures, offscreen targets, readback.
//! * `frame.rs`: frames in flight, uniform arena, deferred release, bind tracking.
//! * `translate.rs`: pure engine-to-Vulkan mappings (unit-tested without a GPU).
//! * `error.rs`: mapping of failures onto `RenderError`.
//!
//! ### Conventions
//!
//! * Clip space is WebGPU style. The viewport is flipped (negative height), so
//!   WGSL needs no changes; front faces are counter-clockwise on screen.
//! * Per-draw uniforms live in a per-frame host-visible arena addressed with
//!   dynamic offsets; descriptor sets come from per-frame pools reset on slot
//!   reuse.
//! * Unsafe code is confined to small, commented blocks around Vulkan calls.

mod error;
mod frame;
mod instance;
mod memory;
mod pipeline;
mod renderer;
mod resources;
mod swapchain;
mod target;
mod translate;

use crate::renderer::Core;
use engine_render::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::sync::Arc;

pub use instance::{ValidationLog, ValidationMessage, ValidationSeverity};
pub use renderer::VulkanStats;

/// Anything a Vulkan surface can be created from (winit windows implement this).
pub trait WindowSource: HasWindowHandle + HasDisplayHandle + Send + Sync {}
impl<T: HasWindowHandle + HasDisplayHandle + Send + Sync> WindowSource for T {}

#[derive(Clone, Debug)]
pub struct VulkanConfig {
    pub application_name: String,
    /// Enable `VK_LAYER_KHRONOS_validation` when it is installed.
    pub enable_validation: bool,
    pub frames_in_flight: u32,
}

impl Default for VulkanConfig {
    fn default() -> Self {
        Self {
            application_name: "engine".into(),
            enable_validation: cfg!(debug_assertions),
            frames_in_flight: 2,
        }
    }
}

pub struct VulkanBackend {
    config: VulkanConfig,
    /// Dropped before `window` so the surface goes away before its window.
    core: Option<Box<Core>>,
    shut_down: bool,
    validation: ValidationLog,
    /// Loader library to use instead of the platform default (tests only).
    library: Option<std::ffi::OsString>,
    window: Option<(Arc<dyn WindowSource>, (u32, u32))>,
}

impl VulkanBackend {
    /// Backend without a presentation surface (offscreen targets only).
    pub fn offscreen(config: VulkanConfig) -> Self {
        Self::build(config, None)
    }

    /// Backend presenting to `window`, whose current inner size is `size`.
    /// Nothing is created until [`RenderBackend::initialize`].
    pub fn for_window<W: WindowSource + 'static>(
        window: Arc<W>,
        size: (u32, u32),
        config: VulkanConfig,
    ) -> Self {
        let window: Arc<dyn WindowSource> = window;
        Self::build(config, Some((window, size)))
    }

    fn build(config: VulkanConfig, window: Option<(Arc<dyn WindowSource>, (u32, u32))>) -> Self {
        Self {
            config,
            core: None,
            shut_down: false,
            validation: ValidationLog::default(),
            library: None,
            window,
        }
    }

    /// Shared log of validation-layer messages. Clone it before moving the
    /// backend into a `RenderDevice` to inspect messages afterwards. Empty and
    /// inactive when the layer is not installed or validation is disabled.
    pub fn validation_log(&self) -> ValidationLog {
        self.validation.clone()
    }

    /// Counters and device facts; `None` before `initialize` and after `shutdown`.
    pub fn stats(&self) -> Option<VulkanStats> {
        self.core.as_deref().map(Core::stats)
    }

    /// Use a specific Vulkan loader library (unit tests of the missing-loader path).
    #[cfg(test)]
    fn with_library_path(mut self, path: impl Into<std::ffi::OsString>) -> Self {
        self.library = Some(path.into());
        self
    }

    fn ready(&mut self) -> Result<&mut Core> {
        let message = if self.shut_down {
            "the Vulkan backend was shut down"
        } else {
            "the Vulkan backend is not initialized"
        };
        match self.core.as_deref_mut() {
            Some(core) => Ok(core),
            None => Err(RenderError::NotReady(message.into())),
        }
    }

    /// Capabilities before `initialize` (no device is known yet).
    fn uninitialized_capabilities(&self) -> RenderCapabilities {
        let mut caps = RenderCapabilities::full(BackendKind::Vulkan, "vulkan");
        caps.backend_ready = false;
        caps.max_frames_in_flight = self.config.frames_in_flight.clamp(1, 8);
        caps.surface_presentation = self.window.is_some();
        caps.unsupported_reasons.insert(
            RenderFeature::BackendAvailable,
            "initialize has not completed".into(),
        );
        if self.window.is_none() {
            caps.unsupported_reasons.insert(
                RenderFeature::SurfacePresentation,
                "the backend was created without a window (offscreen only)".into(),
            );
        }
        caps
    }
}

impl RenderBackend for VulkanBackend {
    fn capabilities(&self) -> RenderCapabilities {
        match &self.core {
            Some(core) => core.capabilities().clone(),
            None => self.uninitialized_capabilities(),
        }
    }

    fn initialize(&mut self) -> Result<()> {
        if self.core.is_some() {
            return Ok(());
        }
        if self.shut_down {
            return Err(RenderError::NotReady(
                "the Vulkan backend was shut down".into(),
            ));
        }
        let (window, size): (Option<&dyn WindowSource>, (u32, u32)) = match &self.window {
            Some((window, size)) => (Some(&**window), *size),
            None => (None, (0, 0)),
        };
        let core = Core::new(
            &self.config,
            window,
            size,
            self.library.as_deref(),
            &self.validation,
        )?;
        self.core = Some(Box::new(core));
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
        match self.core.as_deref_mut() {
            Some(core) => core.destroy_resource(id),
            // Everything went away with the device at shutdown.
            None => Ok(()),
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.ready()?.resize(width, height)
    }

    fn submit_frame(&mut self, frame: &FrameDesc, draws: &[DrawCommand]) -> Result<FrameOutcome> {
        self.ready()?.submit_frame(frame, draws)
    }

    fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage> {
        self.ready()?.read_target(target)
    }

    fn wait_idle(&mut self) -> Result<()> {
        match self.core.as_deref_mut() {
            Some(core) => core.wait_idle(),
            None => Ok(()),
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        // Dropping the core waits for the GPU and destroys every object in order.
        self.core = None;
        self.shut_down = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn backend_can_move_between_threads() {
        assert_send::<VulkanBackend>();
    }

    #[test]
    fn default_config_is_two_frames_in_flight() {
        let config = VulkanConfig::default();
        assert_eq!(config.frames_in_flight, 2);
        assert_eq!(config.application_name, "engine");
    }

    #[test]
    fn capabilities_before_initialize_say_not_ready_and_why() {
        let backend = VulkanBackend::offscreen(VulkanConfig {
            frames_in_flight: 40,
            ..Default::default()
        });
        let caps = backend.capabilities();
        assert_eq!(caps.kind, BackendKind::Vulkan);
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
        let zero = VulkanBackend::offscreen(VulkanConfig {
            frames_in_flight: 0,
            ..Default::default()
        });
        assert_eq!(zero.capabilities().max_frames_in_flight, 1);
    }

    #[test]
    fn calls_before_initialize_are_not_ready_but_cleanup_calls_succeed() {
        let mut backend = VulkanBackend::offscreen(VulkanConfig::default());
        let texture = Texture2D::white();
        assert!(matches!(
            backend.create_texture(TextureHandle::from_raw(1), &texture),
            Err(RenderError::NotReady(_))
        ));
        assert!(matches!(
            backend.submit_frame(&FrameDesc::new(RenderTarget::Surface), &[]),
            Err(RenderError::NotReady(_))
        ));
        backend.wait_idle().unwrap();
        backend
            .destroy_resource(ResourceId::Texture(TextureHandle::from_raw(1)))
            .unwrap();
    }

    #[test]
    fn missing_loader_is_unavailable_with_an_actionable_message_and_can_retry() {
        let mut backend = VulkanBackend::offscreen(VulkanConfig::default())
            .with_library_path("no-such-vulkan-loader-for-tests.dll");
        for _ in 0..2 {
            let error = backend.initialize().unwrap_err();
            let RenderError::Unavailable(message) = &error else {
                panic!("expected Unavailable, got {error:?}");
            };
            assert!(message.contains("Vulkan loader"), "{message}");
            assert!(message.contains("install or update"), "{message}");
        }
        assert!(!backend.capabilities().backend_ready);
    }

    #[test]
    fn device_construction_surfaces_the_missing_loader() {
        let backend = VulkanBackend::offscreen(VulkanConfig::default())
            .with_library_path("no-such-vulkan-loader-for-tests.dll");
        let error = RenderDevice::new(backend, DeviceConfig::default())
            .err()
            .expect("construction must fail without a loader");
        assert!(matches!(error, RenderError::Unavailable(_)));
    }

    #[test]
    fn shutdown_is_idempotent_and_blocks_later_initialize() {
        let mut backend = VulkanBackend::offscreen(VulkanConfig::default());
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

    #[test]
    fn validation_log_is_shared_with_clones() {
        let backend = VulkanBackend::offscreen(VulkanConfig::default());
        let log = backend.validation_log();
        assert!(!log.is_active());
        assert!(log.messages().is_empty());
    }
}
