//! Main-thread desktop hosting and the sprite renderer, on any graphics backend.
//!
//! Layout:
//!
//! * `engine-render`: the backend-neutral graphics adapter interface.
//! * `engine-render-wgpu` / `engine-vulkan`: the wgpu and native Vulkan
//!   backends. [`GpuContext`], [`OffscreenTarget`] and friends are re-exported
//!   from `engine-render-wgpu`.
//! * this crate: [`SpriteRenderer`] (neutral), [`GpuRenderer`] (wgpu-native,
//!   with [`NativeRenderPass`]es), [`WindowSurface`] / [`BackendWindow`] window
//!   hosts, and the [`run`] loop with backend selection
//!   (`ENGINE_RENDER_BACKEND=wgpu|vulkan`).
//!
//! Games compile into the host executable and use this exact `wgpu` type universe.
//! Register [`NativeRenderPass`] implementations in [`NativeRenderPasses`] before
//! launching the runner, or construct [`GpuRenderer`] directly for another host.
//! Native passes are wgpu-specific by design and need the wgpu backend.

mod backend;
mod camera;
mod input;
mod passes;
mod render_host;
mod renderer;
mod runner;
mod shader;
mod window;

pub use backend::*;
pub use camera::*;
pub use engine_render;
pub use engine_render::BackendKind;
pub use engine_render_wgpu::{
    GpuContext, GpuRequirements, OffscreenTarget, RenderTargetInfo, WgpuBackend, WgpuBackends,
    WgpuOptions,
};
pub use input::key_token;
pub use passes::*;
pub use render_host::resolve_render_backend;
pub use renderer::*;
pub use runner::*;
pub use shader::validate_wgsl;
pub use wgpu;
pub use window::*;
pub use winit;
