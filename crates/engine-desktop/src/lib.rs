//! Main-thread desktop hosting and the sprite renderer on wgpu.
//!
//! Layout:
//!
//! * `engine-render`: the thin backend-neutral adapter (handles, command
//!   buffer, deferred destruction) plus a deterministic recording backend for
//!   GPU-free tests.
//! * `engine-render-wgpu`: the wgpu backend and the native [`GpuContext`]; wgpu
//!   itself selects Vulkan, DX12, Metal or GL ([`WgpuBackends`]).
//! * this crate: [`SpriteRenderer`] (adapter-based), [`GpuRenderer`] (shares a
//!   [`GpuContext`], runs [`NativeRenderPass`]es), [`WindowSurface`], and the
//!   [`run`] loop. `ENGINE_RENDER_BACKEND=primary|vulkan|dx12|metal|gl|all`
//!   picks the wgpu graphics API.
//!
//! The windowed [`run`] loop steps the simulation at a fixed rate and presents
//! once per redraw at the display rate, blending the last two ticks
//! ([`FramePacer`], [`FrameHistory`], [`interpolate_frame`]).
//!
//! Games compile into the host executable and use this exact `wgpu` type universe.
//! Register [`NativeRenderPass`] implementations in [`NativeRenderPasses`] before
//! launching the runner, or construct [`GpuRenderer`] directly for another host.

mod backend;
mod camera;
mod input;
mod interpolation;
mod pacing;
mod passes;
mod renderer;
mod runner;
mod shader;
mod window;

pub use backend::*;
pub use camera::*;
pub use engine_render;
pub use engine_render_wgpu::{
    BACKEND_ENV_VAR, GpuContext, GpuRequirements, OffscreenTarget, RenderTargetInfo, WgpuBackend,
    WgpuBackends, WgpuOptions,
};
pub use input::key_token;
pub use interpolation::*;
pub use pacing::*;
pub use passes::*;
pub use renderer::*;
pub use runner::*;
pub use shader::validate_wgsl;
pub use wgpu;
pub use window::*;
pub use winit;
