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
//!   `rt_closest_hit`, `rt_surface`) and shading library for later passes.
//! - Frame-ready kernels on G-buffer textures: [`ShadowMaskKernel`] (hard or
//!   soft sun shadows), [`AmbientOcclusionKernel`], [`ReflectionKernel`] and
//!   [`DiffuseGiKernel`], plus the spatio-temporal [`Denoiser`] for their
//!   signals and the reference [`PathTracer`].
//! - [`RtEffectsSettings`]: validated serde settings of every effect and the
//!   denoiser, with an honest per-effect [`RtEffectsStatus`].
//!
//! This crate opts out of the workspace `unsafe_code = "forbid"` table and
//! denies unsafe code instead. Its only `unsafe` expression is the
//! experimental-feature token in `device.rs`.

mod ao;
mod denoiser;
mod device;
mod error;
mod gi;
mod kernel;
mod layout;
mod limits;
mod material;
mod pathtrace;
mod probe;
mod reflections;
mod request;
mod scene;
mod selftest;
mod settings;
mod shadow;
mod status;
pub mod wgsl;

pub use ao::{AmbientOcclusionKernel, AoInputs};
pub use denoiser::{Denoiser, DenoiserInputs, DenoiserSignal};
pub use device::{RtDevice, RtInitOptions, StandaloneOptions};
pub use error::RtError;
pub use gi::{DiffuseGiKernel, GiInputs};
pub use kernel::{RtGBuffer, RtLighting, RtScalarFormat};
pub use layout::{GpuGeometryData, GpuInstanceData, ProbeHit, ProbeRay};
pub use limits::{
    IDENTITY_TRANSFORM, MAX_CUSTOM_INDEX, RtIndices, RtInstanceDesc, RtMeshData, RtSceneLimits,
};
pub use material::{MAX_RT_MATERIALS, RtMaterial, RtMaterialTable};
pub use pathtrace::{MAX_PATH_TRACER_BOUNCES, PathTracer, PathTracerParams};
pub use probe::MAX_PROBE_RAYS;
pub use reflections::{
    REFLECTION_MISS_DISTANCE, REFLECTION_NO_GEOMETRY_FLAG, REFLECTION_ROUGH_FLAG, ReflectionInputs,
    ReflectionKernel,
};
pub use request::{AdapterRayTracing, RtFeatureRequest, RtLimitsRequest, probe_adapters};
pub use scene::{InstanceId, MeshId, RtCommitStats, RtScene, RtSceneStats};
pub use selftest::SelfTestReport;
pub use settings::{
    DenoiserSettings, MAX_AO_RADIUS, MAX_ATROUS_ITERATIONS, MAX_GI_BOUNCES, MAX_HISTORY_LENGTH,
    MAX_NORMAL_BIAS, MAX_RAY_DISTANCE, MAX_RAYS_PER_PIXEL, MAX_SUN_ANGULAR_RADIUS, RtAoSettings,
    RtEffectsSettings, RtGiSettings, RtReflectionSettings, RtShadowSettings,
};
pub use shadow::{ShadowMaskKernel, ShadowMaskParams, ShadowMaskTargets};
pub use status::{
    MAX_REASON_BYTES, RayTracingStatus, RayTracingTier, RtEffect, RtEffectMode, RtEffectStatus,
    RtEffectsStatus, RtUnavailableCause, bounded_reason,
};
