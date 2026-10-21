//! Main-thread desktop hosting and the native, extensible wgpu renderer.
//!
//! Games compile into the host executable and use this exact `wgpu` type universe.
//! Register [`NativeRenderPass`] implementations in [`NativeRenderPasses`] before
//! launching the runner, or construct [`GpuRenderer`] directly for another host.

mod camera;
mod gpu;
mod input;
mod offscreen;
mod passes;
mod renderer;
mod runner;
mod window;

pub use camera::*;
pub use gpu::*;
pub use input::key_token;
pub use offscreen::*;
pub use passes::*;
pub use renderer::*;
pub use runner::*;
pub use wgpu;
pub use window::*;
pub use winit;
