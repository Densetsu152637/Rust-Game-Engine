//! PC VR through OpenXR (see docs/xr.md).
//!
//! * Runtime discovery without an SDK or a bundled loader: the active-runtime
//!   manifest is resolved ([`manifest`]), its library loaded and negotiated
//!   with `xrNegotiateLoaderRuntimeInterface`, and the `openxr` crate entry built
//!   from the runtime's `xrGetInstanceProcAddr`. This built-in path bypasses
//!   OpenXR API layers; [`LoaderMode::KhronosLoader`] opts into an installed
//!   Khronos loader instead.
//! * Every unavailable capability reports an [`XrStatus`] with a precise reason.
//! * [`XrSession`] owns the session lifecycle, action-based input, tracking,
//!   haptics and per-eye swapchains wrapped as `wgpu::Texture`s on a wgpu device
//!   created through `XR_KHR_vulkan_enable2`.
//! * Rendering stays behind [`XrViewRenderer`]; [`bridge`] connects a session to
//!   an `engine-project` world.
//!
//! Unsafe code is confined to `negotiate` (library loading and the loader
//! negotiation call), `registry` (Windows registry read) and `vulkan`
//! (XR_KHR_vulkan_enable2 and wgpu-hal interop).
#![deny(unsafe_code)]

pub mod bridge;
pub mod input;
pub mod lifecycle;
pub mod manifest;
mod negotiate;
#[cfg(windows)]
mod registry;
pub mod render;
mod runtime;
mod session;
mod status;
pub mod view;
#[cfg(any(windows, target_os = "linux"))]
mod vulkan;

pub use render::{Eye, MirrorBlit, TestPattern, XrFrameContext, XrRenderTarget, XrViewRenderer};
pub use runtime::{LoaderMode, XrAppInfo, XrInstance, XrRuntime, XrSystem, probe};
pub use session::{FrameReport, MirrorTarget, ReferenceSpace, XrError, XrSession, XrSessionConfig};
pub use status::{CheckedLocation, ManifestSource, SessionPhase, XrStatus, result_name};
pub use view::{Fov, Pose, XrView};
