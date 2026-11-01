//! Validated, renderer-native inputs for image-based and baked indirect lighting.
//!
//! Immutable resources and optional per-view material shading configured with
//! ScenePipeline::set_indirect. Supplied lightmaps are consumed per mesh instance
//! through [`LightmapBinding`]. No probe capture, UV unwrapping, or baking.
mod brdf;
mod data;
mod gpu;
mod lightmap;
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
pub use lightmap::{
    LIGHTMAP_FLAG_ADD, LIGHTMAP_FLAG_ENABLED, LightmapBinding, LightmapMode, pack_unorm2x16,
    unpack_unorm2x16,
};
pub use probes::{ProbeSelection, ProbeVolume, select_probe};

pub(crate) use scene::MATERIAL_WGSL;
pub use scene::{
    EnvironmentLighting, INDIRECT_UNIFORM_BYTES, IndirectMaterialLayouts, IndirectProbe,
    IndirectScene, MAX_INDIRECT_MATERIALS, check_material_limits,
};
pub(crate) use scene::{MaterialArena, black_cube, black_lightmap, material_layouts};
