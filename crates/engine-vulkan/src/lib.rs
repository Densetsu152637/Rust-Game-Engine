//! Native Vulkan backend for `engine-render`.
//!
//! STUB: the public API shape is final; the implementation is being written.

use engine_render::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::sync::Arc;

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
    #[allow(dead_code)]
    config: VulkanConfig,
    #[allow(dead_code)]
    window: Option<(Arc<dyn WindowSource>, (u32, u32))>,
}

impl VulkanBackend {
    /// Backend without a presentation surface (offscreen targets only).
    pub fn offscreen(config: VulkanConfig) -> Self {
        Self {
            config,
            window: None,
        }
    }

    /// Backend presenting to `window`, whose current inner size is `size`.
    /// Nothing is created until [`RenderBackend::initialize`].
    pub fn for_window<W: WindowSource + 'static>(
        window: Arc<W>,
        size: (u32, u32),
        config: VulkanConfig,
    ) -> Self {
        Self {
            config,
            window: Some((window, size)),
        }
    }
}

fn unimplemented_backend() -> RenderError {
    RenderError::Unavailable("the Vulkan backend is not implemented yet".into())
}

impl RenderBackend for VulkanBackend {
    fn capabilities(&self) -> RenderCapabilities {
        let mut caps = RenderCapabilities::full(BackendKind::Vulkan, "vulkan");
        caps.backend_ready = false;
        caps
    }
    fn initialize(&mut self) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn create_shader(&mut self, _: ShaderHandle, _: &ShaderResource) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn create_mesh(&mut self, _: MeshHandle, _: &MeshResource) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn create_texture(&mut self, _: TextureHandle, _: &Texture2D) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn create_target(&mut self, _: TargetHandle, _: &OffscreenDesc) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn update_mesh(&mut self, _: MeshHandle, _: &MeshResource) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn update_texture(&mut self, _: TextureHandle, _: &Texture2D) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn destroy_resource(&mut self, _: ResourceId) -> Result<()> {
        Ok(())
    }
    fn resize(&mut self, _: u32, _: u32) -> Result<()> {
        Err(unimplemented_backend())
    }
    fn submit_frame(&mut self, _: &FrameDesc, _: &[DrawCommand]) -> Result<FrameOutcome> {
        Err(unimplemented_backend())
    }
    fn read_target(&mut self, _: TargetHandle) -> Result<ReadbackImage> {
        Err(unimplemented_backend())
    }
    fn wait_idle(&mut self) -> Result<()> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}
