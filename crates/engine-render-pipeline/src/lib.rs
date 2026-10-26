//! The engine's 3D frame graph and render extension API.
//!
//! * [`FeatureRegistry`] / [`PipelineFeature`]: features with stable unique
//!   names in named [`Stage`]s, ordered by before/after constraints, enabled
//!   or disabled per view ([`FeatureOverrides`]).
//! * [`ViewData`] / [`ViewUniform`]: per-view matrices (jittered and
//!   unjittered), inverses, previous view-projection, sizes, frame index and
//!   time, bound for every feature as `engine::view` (group 0) together with
//!   the light list, shadow views, shadow atlas and light clusters.
//! * [`LightList`] / [`Light`]: directional, point, spot and rectangular
//!   area lights in shading units, with [`ShadowSettings`] (cascades, cube
//!   faces, spot views in a [`ShadowAtlasConfig`] atlas; PCF or PCSS).
//!   `engine.lights.clusters` builds a froxel grid of light lists per view
//!   and `engine::lighting` shades them (docs/rendering-lights.md).
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
pub mod indirect;
mod instancing;
mod lighting;
mod lights;
mod overlay;
mod pipeline;
pub mod post;
pub mod readback;
mod resources;
pub mod shader;
pub mod shadows;
mod streams;
mod targets;
mod view;

pub use builtin::mesh::{
    GpuInstance, MAX_SCENE_MESHES, MaterialParams, SceneMesh, id_hash, validate_params,
};
pub use builtin::passes::DepthExport;
pub use context::{
    CompositeIo, FeatureContext, FeatureSetup, OutputTarget, PipelineLayouts, PresentationInput,
};
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
pub use lighting::{
    CLUSTER_DIMS, ClusterMode, ClusterStats, LightingReport, MAX_LIGHTING_REASONS,
    MAX_LIGHTS_PER_CLUSTER, frustum_planes, sphere_in_frustum,
};
pub use lights::{
    CascadeSettings, GpuLight, GpuShadow, Light, LightKind, LightList, LightShape, MAX_CASCADES,
    MAX_LIGHTS, MAX_PCF_RADIUS, MAX_PCSS_RADIUS, MAX_SHADOWS, MAX_SUBMITTED_LIGHTS,
    SHADOW_RESOLUTIONS, ShadowFilter, ShadowSettings, ShadowView, photometric,
};
pub use overlay::{OverlayBatch, OverlayItem, OverlayOrderKey};
pub use pipeline::{
    AutoExposureState, HdrImage, HostStages, HostUpscale, NoHost, SceneFrame, ScenePipeline,
    SceneTextures, ViewReport, builtin_registry, names,
};
pub use post::{
    BloomSettings, ExposureMode, ExposureSettings, GradingSettings, GrainSettings, Lut3d,
    PostSettings, TonemapOperator, VignetteSettings,
};
pub use resources::{ENGINE_VERTEX_LAYOUT, EngineVertex, GpuMesh, GpuTexture, TEXTURE_USAGES};
pub use shader::{LIT_WGSL, MaterialTarget, ShaderComposer, compose_material, validate_wgsl};
pub use shadows::{ShadowAtlasConfig, ShadowRenderView, cascade_splits};
pub use streams::{MAX_STREAMS, StreamMode, TextureStreams};
pub use targets::{
    DEPTH_FORMAT, FrameTargets, HDR_FORMAT, MOTION_FORMAT, NORMALS_FORMAT,
    SCREEN_LIGHTING_DEFAULTS, SCREEN_LIGHTING_FORMAT, ScreenLighting, Target,
};
pub use view::{
    Presentation, VIEW_UNIFORM_BYTES, ViewData, ViewHistory, ViewId, ViewKind, ViewUniform,
    Viewport, project,
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
