//! Validated, renderer-native inputs for image-based and baked indirect lighting.
//!
//! Immutable resources and optional per-view material shading configured with
//! ScenePipeline::set_indirect. No authored asset adapter, probe capture, or baking.
mod brdf;
mod data;
mod gpu;
mod probes;
mod scene;

pub use brdf::{BrdfLut, MAX_LUT_SIZE, MAX_LUT_WORK, MAX_SAMPLES, integrate_brdf};
pub use data::{
    DiffusePolynomial, EnvironmentData, IndirectBudget, MAX_CUBE_SIZE, MAX_ENVIRONMENTS, MAX_HALF,
    MAX_LIGHTMAPS, MAX_PROBES, MAX_RADIANCE_BYTES, MAX_RADIANCE_EDGE, RadianceImage,
};
pub use gpu::{
    GpuBrdfLut, GpuEnvironment, GpuIndirectResources, GpuRadianceImage, check_capabilities,
};
pub use probes::{ProbeSelection, ProbeVolume, select_probe};

pub(crate) use scene::MATERIAL_WGSL;
pub use scene::{
    EnvironmentLighting, INDIRECT_UNIFORM_BYTES, IndirectMaterialLayouts, IndirectProbe,
    IndirectScene, MAX_INDIRECT_MATERIALS, check_material_limits,
};
pub(crate) use scene::{MaterialArena, black_cube, material_layouts};
