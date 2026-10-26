//! Validated, renderer-native inputs for image-based and baked indirect lighting.
//!
//! This module prepares immutable resources only. It does not install material
//! bindings, select a camera environment, capture probes, or bake lighting.
mod brdf;
mod data;
mod gpu;
mod probes;

pub use brdf::{BrdfLut, MAX_LUT_SIZE, MAX_LUT_WORK, MAX_SAMPLES, integrate_brdf};
pub use data::{
    DiffusePolynomial, EnvironmentData, IndirectBudget, MAX_CUBE_SIZE, MAX_ENVIRONMENTS, MAX_HALF,
    MAX_LIGHTMAPS, MAX_PROBES, MAX_RADIANCE_BYTES, MAX_RADIANCE_EDGE, RadianceImage,
};
pub use gpu::{
    GpuBrdfLut, GpuEnvironment, GpuIndirectResources, GpuRadianceImage, check_capabilities,
};
pub use probes::{ProbeSelection, ProbeVolume, select_probe};
