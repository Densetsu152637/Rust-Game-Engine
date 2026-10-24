//! The engine's 3D frame graph and render extension API.
//!
//! * [`FeatureRegistry`] / [`PipelineFeature`]: features with stable unique
//!   names in named [`Stage`]s, ordered by before/after constraints, enabled
//!   or disabled per view ([`FeatureOverrides`]).
//! * [`ViewData`] / [`ViewUniform`]: per-view matrices (jittered and
//!   unjittered), inverses, previous view-projection, sizes, frame index and
//!   time, bound for every feature as `engine::view` (group 0) together with
//!   the [`LightList`] storage buffer and the shadow atlas.
//! * [`FrameTargets`]: HDR scene colour (`Rgba16Float`), depth, optional
//!   normals and motion vectors.
//! * [`InstanceBatcher`] and the built-in mesh path: identical mesh +
//!   material draws become one instanced call.
//! * [`ShaderComposer`]: `#import`-based WGSL composition; the material
//!   shader contract is [`compose_material`].
//! * [`ScenePipeline`]: per-view execution of the stages, with
//!   [`HostStages`] hooks for host upscalers (DLSS) and host overlay nodes
//!   (2D sprites).
//! * Device feature composition lives in `engine-render-wgpu`
//!   ([`GpuRequirements`], [`DeviceFeatureRequest`]) and is re-exported.
//!
//! See docs/rendering-architecture.md for the contracts.

mod builtin;
mod context;
mod feature;
mod instancing;
mod lights;
mod pipeline;
pub mod readback;
mod resources;
pub mod shader;
mod streams;
mod targets;
mod view;

pub use builtin::mesh::{
    GpuInstance, MAX_SCENE_MESHES, MaterialParams, SceneMesh, id_hash, validate_params,
};
pub use builtin::passes::DepthExport;
pub use context::{FeatureContext, FeatureSetup, OutputTarget, PipelineLayouts};
pub use engine_render_wgpu::{
    ComposedDevice, DeviceFeatureReport, DeviceFeatureRequest, GpuContext, GpuRequirements,
    RequestOutcome,
};
pub use feature::{
    Activity, EnableSource, FeatureDescriptor, FeatureNeeds, FeatureOverrides, FeatureRegistry,
    FeatureState, MAX_FEATURE_NAME_BYTES, MAX_FEATURES, PipelineFeature, PipelineFeatures,
    ResolvedFeatures, Stage, StageRule, validate_feature_name,
};
pub use instancing::{DrawStats, Grouping, InstanceBatcher, InstancedBatch, group_keys};
pub use lights::{GpuLight, GpuShadow, LightKind, LightList, MAX_LIGHTS, MAX_SHADOWS, ShadowView};
pub use pipeline::{
    HdrImage, HostStages, HostUpscale, NoHost, SceneFrame, ScenePipeline, SceneTextures,
    ViewReport, builtin_registry, names,
};
pub use resources::{ENGINE_VERTEX_LAYOUT, EngineVertex, GpuMesh, GpuTexture, TEXTURE_USAGES};
pub use shader::{LIT_WGSL, MaterialTarget, ShaderComposer, compose_material, validate_wgsl};
pub use streams::{MAX_STREAMS, StreamMode, TextureStreams};
pub use targets::{DEPTH_FORMAT, FrameTargets, HDR_FORMAT, MOTION_FORMAT, NORMALS_FORMAT, Target};
pub use view::{
    VIEW_UNIFORM_BYTES, ViewData, ViewHistory, ViewId, ViewKind, ViewUniform, Viewport, project,
};

use anyhow::{Result, bail};

/// Run `body` inside a validation error scope; a captured GPU validation
/// error fails the call.
pub(crate) fn scoped<T>(device: &wgpu::Device, body: impl FnOnce() -> Result<T>) -> Result<T> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let result = body();
    let captured = pollster::block_on(scope.pop());
    let value = result?;
    if let Some(error) = captured {
        bail!("GPU validation: {error}");
    }
    Ok(value)
}
