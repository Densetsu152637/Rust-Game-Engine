//! Main-thread desktop hosting and the sprite renderer on wgpu.
//!
//! Layout:
//!
//! * `engine-render`: the thin backend-neutral adapter (handles, command
//!   buffer, deferred destruction) plus a deterministic recording backend for
//!   GPU-free tests.
//! * `engine-render-wgpu`: the wgpu backend and the native [`GpuContext`]; wgpu
//!   itself selects Vulkan, DX12, Metal or GL ([`WgpuBackends`]).
//! * `engine-render-pipeline`: the 3D frame graph and render extension API
//!   (render features, view data, HDR targets, light list, instancing),
//!   re-exported as [`render_pipeline`].
//! * this crate: [`SpriteRenderer`] (adapter-based), [`GpuRenderer`] (shares a
//!   [`GpuContext`], drives the render pipeline), [`WindowSurface`], and the
//!   [`run`] loop. `ENGINE_RENDER_BACKEND=primary|vulkan|dx12|metal|gl|all`
//!   picks the wgpu graphics API.
//!
//! The windowed [`run`] loop steps the simulation at a fixed rate and presents
//! once per redraw at the display rate, blending the last two ticks
//! ([`FramePacer`], [`FrameHistory`], [`interpolate_frame`]).
//!
//! The optional `dlss` feature (forwarding to `engine-dlss/ngx-vulkan`) lets a
//! 3D camera's `upscaler` request DLSS Super Resolution; see [`request_upscaler_gpu`].
//!
//! Games compile into the host executable and use this exact `wgpu` type universe.
//! Register render features ([`render_pipeline::PipelineFeature`]) in a
//! [`PipelineFeatures`] resource before launching the runner, or construct
//! [`GpuRenderer`] directly for another host.

mod backend;
mod camera;
mod dlss;
mod input;
mod interpolation;
mod pacing;
mod renderer;
mod runner;
mod shader;
mod window;

pub use backend::*;
pub use camera::*;
pub use dlss::{
    DEFAULT_NGX_PROJECT_ID, DlssConfig, DlssRequest, NGX_LOG_DIR_ENV_VAR, NGX_RUNTIME_DIR_ENV_VAR,
    UpscalerDevice, build_unavailable_reason, fg_build_unavailable_reason, request_upscaler_gpu,
};
pub use engine_render;
pub use engine_render_pipeline as render_pipeline;
pub use engine_render_pipeline::PipelineFeatures;
pub use engine_render_wgpu::{
    BACKEND_ENV_VAR, DeviceFeatureReport, DeviceFeatureRequest, GpuContext, GpuRequirements,
    OffscreenTarget, RenderTargetInfo, WgpuBackend, WgpuBackends, WgpuOptions,
};
pub use input::key_token;
pub use interpolation::*;
pub use pacing::*;
pub use renderer::*;
pub use runner::*;
pub use shader::validate_wgsl;
pub use wgpu;
pub use window::*;
pub use winit;
