//! `engine-render` on wgpu, plus the native GPU context shared with hosts.
//!
//! * [`WgpuBackend`] implements [`engine_render::RenderBackend`]; wrap it in an
//!   [`engine_render::RenderDevice`] for handle management and validation.
//! * [`GpuContext`] / [`GpuRequirements`] / [`OffscreenTarget`] /
//!   [`RenderTargetInfo`] are the public native device access types moved here
//!   from `engine-desktop`. A [`GpuContext`] is cheaply cloneable, so a native
//!   renderer and a [`WgpuBackend`] can share one device
//!   ([`WgpuBackend::from_context`]).
//! * [`WgpuBackends`] / [`WgpuOptions`] select the graphics API and device
//!   requirements.
//!
//! ## Conventions (identical across backends)
//!
//! sRGB RGBA8 textures and targets, y-up WebGPU clip space with depth 0..1,
//! top-left scissor and viewport rectangles, uniform buffers addressed by
//! `@group/@binding` through dynamic offsets into a per-frame arena, the draw's
//! texture at `@group(1) @binding(0)` with a nearest/clamp sampler at
//! `@group(1) @binding(1)` (a 1x1 white texture when the draw has none),
//! straight alpha as `BlendState::ALPHA_BLENDING`, depth `LessEqual`/`Always`
//! with a write flag, a `Depth32Float` attachment owned per target, and
//! counter-clockwise front faces.

mod backend;
mod backends;
mod context;
mod core;
mod error;
mod frame;
mod shader;
mod shadow;
mod surface;
mod target;
pub mod translate;
pub mod uniforms;

pub use backend::{WgpuBackend, WgpuStats};
pub use backends::{BACKEND_ENV_VAR, WgpuBackends};
pub use context::{GpuContext, GpuRequirements, WgpuOptions};
pub use surface::WindowSource;
pub use target::{
    OffscreenTarget, RenderTargetInfo, padded_row_bytes, read_texture_rgba, rgba_row_bytes,
    unpad_rows,
};
pub use wgpu;
