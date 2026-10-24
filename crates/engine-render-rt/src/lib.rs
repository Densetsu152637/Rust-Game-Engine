//! Hardware ray-tracing core for the renderer (docs/rendering-raytracing.md).
//!
//! - [`RtFeatureRequest`]: the features, acceleration-structure limits and the
//!   audited experimental-feature token a renderer merges into its device
//!   creation. Only Vulkan adapters exposing `EXPERIMENTAL_RAY_QUERY` get one.
//! - [`RtDevice`]: a device whose hardware tier passed its GPU self-test (exact
//!   hits against a CPU reference). Its [`RayTracingStatus`] is the honest
//!   tier report; every failure path returns an `Unavailable` status with a
//!   precise, bounded reason.
//! - [`RtScene`]: one BLAS per mesh, one TLAS of instances, incremental updates,
//!   validated limits and statistics.
//! - [`wgsl`]: the reusable WGSL ray-query library (`rt_occluded`,
//!   `rt_closest_hit`, `rt_surface`) for later passes.
//! - [`ShadowMaskKernel`] and [`PathTracer`]: end-to-end compute kernels.
//!
//! This crate opts out of the workspace `unsafe_code = "forbid"` table and
//! denies unsafe code instead. Its only `unsafe` expression is the
//! experimental-feature token in `device.rs`.

mod device;
mod error;
mod layout;
mod limits;
mod pathtrace;
mod probe;
mod request;
mod scene;
mod selftest;
mod shadow;
mod status;
pub mod wgsl;

pub use device::{RtDevice, RtInitOptions, StandaloneOptions};
pub use error::RtError;
pub use layout::{GpuGeometryData, GpuInstanceData, ProbeHit, ProbeRay};
pub use limits::{
    IDENTITY_TRANSFORM, MAX_CUSTOM_INDEX, RtIndices, RtInstanceDesc, RtMeshData, RtSceneLimits,
};
pub use pathtrace::{
    MAX_PATH_TRACER_BOUNCES, MAX_PATH_TRACER_MATERIALS, PathTracer, PathTracerMaterial,
    PathTracerParams,
};
pub use probe::MAX_PROBE_RAYS;
pub use request::{AdapterRayTracing, RtFeatureRequest, RtLimitsRequest, probe_adapters};
pub use scene::{InstanceId, MeshId, RtCommitStats, RtScene, RtSceneStats};
pub use selftest::SelfTestReport;
pub use shadow::{ShadowMaskFormat, ShadowMaskKernel, ShadowMaskParams, ShadowMaskTargets};
pub use status::{
    MAX_REASON_BYTES, RayTracingStatus, RayTracingTier, RtUnavailableCause, bounded_reason,
};
