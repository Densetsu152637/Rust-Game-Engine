//! An uninhabited session keeps the optional API usable without an installed SDK.
use crate::{Error, Frame, Result, SessionOptions, Status};
#[derive(Clone)]
pub enum Context {}
impl Context {
    /// Unavailable constructor.
    /// Same API as the SDK-enabled context constructor.
    pub fn new(_: &wgpu::Device, _: &wgpu::Queue, _: &SessionOptions) -> Result<Self> {
        Err(unavailable())
    }
    pub fn device(&self) -> &wgpu::Device {
        match *self {}
    }
    pub fn queue(&self) -> &wgpu::Queue {
        match *self {}
    }
    pub fn device_name(&self) -> &str {
        match *self {}
    }
    pub fn driver_version(&self) -> u32 {
        match *self {}
    }
}
pub enum Session {}
impl Session {
    /// Unavailable constructor.
    /// Same device/queue/lifetime contract as the SDK-enabled constructor.
    pub fn new(_: &wgpu::Device, _: &wgpu::Queue, _: SessionOptions) -> Result<Self> {
        Err(unavailable())
    }
    pub fn with_context(context: Context, _: [u32; 2], _: crate::Quality) -> Result<Self> {
        match context {}
    }
    pub fn context(&self) -> &Context {
        match *self {}
    }
    pub fn status(&self) -> &Status {
        match *self {}
    }
    pub fn inputs(&self) -> [&wgpu::TextureView; 3] {
        match *self {}
    }
    pub fn output(&self) -> &wgpu::TextureView {
        match *self {}
    }
    pub fn output_texture(&self) -> &wgpu::Texture {
        match *self {}
    }
    pub fn evaluate(&mut self, _: Frame) -> Result<()> {
        match *self {}
    }
}
pub fn create_device(
    _: &wgpu::Adapter,
    _: &wgpu::DeviceDescriptor<'_>,
    _: &SessionOptions,
) -> Result<(wgpu::Device, wgpu::Queue)> {
    Err(unavailable())
}
fn unavailable() -> Error {
    Error::Unavailable("NGX adapter unavailable: build with official SDK on Windows/Linux".into())
}
pub fn create_device_for_features(
    _: &wgpu::Adapter,
    _: &wgpu::DeviceDescriptor<'_>,
    _: &SessionOptions,
    _: &[crate::Feature],
) -> Result<(wgpu::Device, wgpu::Queue)> {
    Err(unavailable())
}

/// Unavailable FG adapter with the SDK-enabled generator's API.
pub enum FrameGenerator {}
impl FrameGenerator {
    pub fn new(
        _: &wgpu::Device,
        _: &wgpu::Queue,
        _: &SessionOptions,
        _: crate::FrameGenerationOptions,
    ) -> Result<Self> {
        Err(frame_generation_unavailable())
    }
    pub fn with_context(
        context: Context,
        _: &SessionOptions,
        _: crate::FrameGenerationOptions,
    ) -> Result<Self> {
        match context {}
    }
    pub fn context(&self) -> &Context {
        match *self {}
    }
    pub fn status(&self) -> &crate::FrameGenerationStatus {
        match *self {}
    }
    pub fn evaluate(
        &mut self,
        _: &crate::GenerationInputs<'_>,
        _: &crate::GenerationFrame,
        _: &wgpu::Texture,
    ) -> Result<crate::Generated> {
        match *self {}
    }
}
fn frame_generation_unavailable() -> Error {
    Error::Unavailable(
        "NGX DLSS Frame Generation unavailable: this build has no official DLSS SDK \
         (set NGX_SDK_DIR and VULKAN_HEADERS_DIR on Windows/Linux)"
            .into(),
    )
}
