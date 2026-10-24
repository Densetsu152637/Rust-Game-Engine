//! Acoustic world, geometric propagation and the propagation backend contract.
//!
//! This crate currently holds the frozen public contract from
//! `docs/spatial-audio-design.md` section 7.2 as a compiling skeleton. Everything
//! that is not implemented yet returns an `acoustics.unimplemented` error and never
//! a fabricated result. Types whose values only the implementation can produce
//! (`AcousticWorld`, `ProxyMapping`, `AcousticsService`) cannot be constructed yet.

mod bvh;
mod error;
mod estimate;
mod geometry;
mod material;
mod math;
mod planes;
mod proxy2d;
pub mod reference;
pub mod rng;
mod service;
mod trace;
mod volume;
mod world;

pub use bvh::{MAX_BVH_DEPTH, MESH_MAX_LEAF, SAH_BINS};
pub use engine_audio::acoustic::{BANDS, BandGains};
pub use error::{AcousticsError, Result, unimplemented};
pub use estimate::{eyring_rt60, fit_reverb, sabine_rt60, synthesize_ir};
pub use geometry::{
    AcousticVolume, Enclosure, Footprint, Footprint2D, ListenerPose, Proxy2DSettings, ProxyMapping,
    ProxyPlane, VolumeKind, VolumeShape,
};
pub use material::{AcousticMaterial, MaterialId, MaterialPreset};
pub use math::IDENTITY as IDENTITY_AFFINE;
pub use planes::PlaneCluster;
pub use service::{
    AcousticState, AcousticStatus, AcousticsService, ParamHook, PropagationRequest,
    PropagationResult, PropagationSettings, PropagationSummary, ServiceConfig, ServiceMode,
    SourceRequest, SourceResult, SourceStatus, SourceSummary,
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
