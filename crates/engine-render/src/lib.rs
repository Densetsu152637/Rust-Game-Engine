//! Backend-neutral graphics adapter interface.
//!
//! The Rust port of the legacy C++ `rendering/render_device.h` contract, with
//! no GPU dependency:
//!
//! * [`RenderBackend`] is the trait a graphics implementation provides
//!   (`engine-render-wgpu`, or the in-crate
//!   [`headless::RecordingBackend`]).
//! * [`RenderDevice`] owns a backend, hands out typed resource handles
//!   ([`ShaderHandle`], [`MeshHandle`], [`TextureHandle`], [`TargetHandle`]),
//!   validates draws against the reflected WGSL interface and defers
//!   destruction until frames in flight complete.
//! * [`RenderCommandBuffer`] collects [`DrawCommand`]s either in call order or
//!   under stable `(producer, draw)` keys.
//!
//! ## Shader and binding contract
//!
//! WGSL is the authoring language ([`ShaderSource`]). A shader is a vertex plus
//! fragment entry point (`vs_main` / `fs_main` by default). Uniform buffers bind
//! by their `@group/@binding` through [`DrawUniform`]; a draw's optional
//! texture binds at [`TEXTURE_SLOT`] with a nearest/clamp sampler at
//! [`SAMPLER_SLOT`]. Draws without a texture bind a 1x1 white texture.
//! Textures and targets are RGBA8 sRGB. Meshes are triangle lists (optionally
//! indexed with `u32`), clip space is WebGPU-style (y up, depth 0..1) and the
//! scissor rectangle uses a top-left pixel origin.

pub mod backend;
pub mod command;
pub mod device;
pub mod error;
pub mod frame;
pub mod headless;
pub mod resources;
pub mod shader;
pub mod state;

pub use backend::{
    BackendKind, DeviceLimits, RenderBackend, RenderCapabilities, RenderFeature,
    RenderFeatureStatus,
};
pub use command::{
    DrawCommand, DrawOrderKey, DrawUniform, RenderCommandBuffer, SAMPLER_SLOT, TEXTURE_SLOT,
    UniformSlot,
};
pub use device::{DeviceConfig, RenderDevice, ResourceCounts};
pub use error::{RenderError, Result};
pub use frame::{
    FrameDesc, FrameOutcome, FrameSkip, FrameStats, OffscreenDesc, ReadbackImage, RenderTarget,
    TargetFormat,
};
pub use resources::{
    MeshHandle, MeshResource, ResourceId, ResourceKind, ShaderHandle, ShaderLanguage,
    ShaderResource, ShaderSource, TargetHandle, Texture2D, TextureHandle, VertexAttribute,
    VertexFormat, VertexLayout,
};
pub use shader::{ShaderInterface, validate_wgsl};
pub use state::{BlendMode, CullMode, DrawState, PipelineState, PixelRect};
