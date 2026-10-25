//! Acoustic world, geometric propagation and the propagation backend contract.
//!
//! Stream B of `docs/spatial-audio-design.md`: the two-level acoustic triangle world
//! (`AcousticWorldBuilder`, `AcousticWorld`, `gpu_layout`), cited materials, the 2D
//! proxy synthesizer, volumes, the CPU propagation backend (`CpuBackend`,
//! [`trace_cpu`], [`direct`], [`image_source`]), Schroeder fitting, analytic
//! estimates, IR synthesis, the world document and the `AcousticsService` worker.
//! [`rng`] and [`trace_cpu`] specify the energy model the GPU tiers mirror, and
//! [`reference`] holds the parity fixtures. See the crate README.

// Per-band loops index several parallel arrays by band.
#![allow(clippy::needless_range_loop)]

mod bvh;
pub mod direct;
mod document;
mod engine;
mod error;
mod estimate;
mod fit;
mod geometry;
pub mod image_source;
mod ir;
mod material;
mod math;
mod planes;
mod proxy2d;
pub mod reference;
pub mod rng;
mod service;
mod trace;
pub mod trace_cpu;
mod volume;
mod world;

pub use bvh::{MAX_BVH_DEPTH, MESH_MAX_LEAF, SAH_BINS};
pub use document::{
    AcousticWorldDocument, DOCUMENT_VERSION, DocumentInstance, DocumentProxy, DocumentVolume,
    MAX_DOCUMENT_BYTES,
};
pub use engine::{
    DEMOTE_AFTER_FAILURES, GPU_MAX_RAYS, MAX_SUBMITTED_SOURCES, MAX_TRACED_SOURCES,
    validate_request, validate_settings,
};
pub use engine_audio::acoustic::{BANDS, BandGains};
pub use error::{AcousticsError, Result, unimplemented};
pub use estimate::{SABINE_CONSTANT, eyring_rt60, sabine_rt60};
pub use fit::{BandFit, DecayFit, fit_bands, fit_reverb, validate_histogram};
pub use geometry::{
    AcousticVolume, Enclosure, Footprint, Footprint2D, ListenerPose, Proxy2DSettings, ProxyPlane,
    VolumeKind, VolumeShape,
};
pub use ir::synthesize_ir;
pub use material::{AcousticMaterial, MaterialId, MaterialPreset, mass_law_transmission};
pub use math::IDENTITY as IDENTITY_AFFINE;
pub use planes::PlaneCluster;
pub use proxy2d::{ProxyAir, ProxyMapping};
pub use service::{
    AcousticState, AcousticStatus, AcousticsService, MixerStats, ParamHook, PropagationRequest,
    PropagationResult, PropagationSettings, PropagationSummary, ServiceConfig, ServiceEvent,
    ServiceMode, SourceRequest, SourceResult, SourceStatus, SourceSummary,
};
pub use trace::{
    BackendError, BackendInfo, BackendKind, BackendPreference, CpuBackend, DirectPath,
    EnergyHistogram, EnergyJob, PropagationBackend, ReflectionPath, TraceSettings, TraceStats,
};
pub use volume::{BOX_FACES, RoomInfo};
pub use world::{
    AcousticMesh, AcousticWorld, AcousticWorldBuilder, Affine3, DEGENERATE_AREA_M2, GpuWorldView,
    Hit, InstanceId, MeshId, Mobility, RAY_OFFSET_ABS_M, RAY_OFFSET_REL, WorldLimits, WorldStats,
    gpu_layout, ray_offset,
};
