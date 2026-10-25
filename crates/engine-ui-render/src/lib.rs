//! GPU render features for the game UI, as `engine-render-pipeline` features.
//!
//! * `ui.sdf_text` (Overlay): screen-space UI text of SDF font families, drawn
//!   from engine-text SDF atlas pages ([`SdfTextFeature`]).
//! * `ui.world` (Transparent): `engine.UiWorldPanel` and `engine.WorldText` in
//!   `Camera3D` scenes, depth-tested, sorted back to front and billboarded per
//!   view ([`WorldUiFeature`]).
//! * `ui.color_filter` (PostProcess): the colour-vision filter driven by
//!   `engine.AccessibilitySettings` ([`ColorFilterFeature`], matrices in
//!   [`color`]).
//!
//! The features read what `engine-project`'s draw extraction publishes into
//! the shared [`UiGpuQueue`] and report their state back through it
//! (`RuntimeSnapshot.ui_render`, `TextRenderStatus`). Opting in is one call
//! before the runner starts, with no change to the renderer:
//!
//! ```ignore
//! engine_ui_render::install(runtime.app_mut())?;
//! ```
//!
//! or, when the game builds its own `PipelineFeatures` resource,
//! `engine_ui_render::register(&mut features, &queue)`. Content is routed to a
//! feature only while it is prepared on a renderer; otherwise it keeps its 2D
//! path and the status says why (see `docs/ui-render.md`).
pub mod color;
mod filter;
pub mod gpu;
pub mod layout;
mod sdf_text;
mod world;

pub use engine_project::ui_tree::{
    FEATURE_COLOR_FILTER, FEATURE_SDF_TEXT, FEATURE_WORLD, UiFeatureState, UiFeatureStatus,
    UiGpuFeature, UiGpuQueue, UiRenderStatus,
};
pub use filter::ColorFilterFeature;
pub use sdf_text::SdfTextFeature;
pub use world::WorldUiFeature;

use anyhow::Result;
use engine_core::EngineApp;
use engine_render_pipeline::PipelineFeatures;

/// Which UI features [`register_with`] adds. The default adds all three.
///
/// `sdf_text` is an Overlay-stage feature, and a registered overlay feature
/// disables DLSS Frame Generation (generated frames lack overlays): leave it
/// out when a project relies on frame generation and has no SDF text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiRenderOptions {
    pub sdf_text: bool,
    pub world_ui: bool,
    pub color_filter: bool,
}

impl Default for UiRenderOptions {
    fn default() -> Self {
        Self {
            sdf_text: true,
            world_ui: true,
            color_filter: true,
        }
    }
}

/// Register the three UI features into `features`, sharing `queue` (the
/// runtime's `UiGpuQueue` resource). Registering a name twice (for example
/// calling this twice) is an error.
pub fn register(features: &mut PipelineFeatures, queue: &UiGpuQueue) -> Result<()> {
    register_with(features, queue, UiRenderOptions::default())
}

/// [`register`] with a choice of features.
pub fn register_with(
    features: &mut PipelineFeatures,
    queue: &UiGpuQueue,
    options: UiRenderOptions,
) -> Result<()> {
    if options.world_ui {
        features.register(WorldUiFeature::new(queue.clone()))?;
    }
    if options.color_filter {
        features.register(ColorFilterFeature::new(queue.clone()))?;
    }
    if options.sdf_text {
        features.register(SdfTextFeature::new(queue.clone()))?;
    }
    Ok(())
}

/// Register the UI features into the app's `PipelineFeatures` resource (created
/// when absent), using the runtime's `UiGpuQueue`. Call once before the
/// runner starts.
pub fn install(app: &mut EngineApp) -> Result<()> {
    install_with(app, UiRenderOptions::default())
}

/// [`install`] with a choice of features.
pub fn install_with(app: &mut EngineApp, options: UiRenderOptions) -> Result<()> {
    let world = app.world_mut();
    let queue = match world.get_resource::<UiGpuQueue>() {
        Some(queue) => queue.clone(),
        None => {
            let queue = UiGpuQueue::default();
            world.insert_resource(queue.clone());
            queue
        }
    };
    let mut features = world
        .remove_resource::<PipelineFeatures>()
        .unwrap_or_default();
    let result = register_with(&mut features, &queue, options);
    world.insert_resource(features);
    result
}
