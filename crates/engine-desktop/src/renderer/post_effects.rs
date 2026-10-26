//! Map validated camera effect settings into the native per-view controls.

use anyhow::{Context, Result};
use engine_project::rendering3d::{
    DEPTH_OF_FIELD_FEATURE, MOTION_BLUR_FEATURE, SPATIAL_AA_FEATURE, SceneRenderFrame,
};
use engine_render_pipeline::{FeatureOverrides, ViewData};
use engine_render_post_effects::{
    DepthOfFieldSettings, MotionBlurSettings, PostEffects, SpatialAaSettings,
};

pub(super) fn prepare(
    controls: &PostEffects,
    frame: &SceneRenderFrame,
    view: &mut ViewData,
) -> Result<()> {
    let camera = &frame
        .camera3d
        .as_ref()
        .context("a post effect needs a Camera3D")?
        .camera;
    camera
        .validate()
        .context("validating authored camera effects")?;
    view.features = FeatureOverrides(camera.effective_render_features());
    let dof = camera
        .depth_of_field
        .filter(|_| view.features.get(DEPTH_OF_FIELD_FEATURE) != Some(false))
        .map(|settings| DepthOfFieldSettings {
            focus_distance: settings.focus_distance,
            focus_range: settings.focus_range,
            max_radius_pixels: settings.max_radius_pixels,
            edge_tolerance: settings.edge_tolerance,
        });
    let motion = camera
        .motion_blur
        .filter(|_| view.features.get(MOTION_BLUR_FEATURE) != Some(false))
        .map(|settings| MotionBlurSettings {
            shutter_scale: settings.shutter_scale,
            max_radius_pixels: settings.max_radius_pixels,
            edge_tolerance: settings.edge_tolerance,
        });
    // Always update both slots. Removing or disabling authored data cannot
    // leave controls from the previous camera/frame behind at this ViewId.
    controls.set_dof(view.id, dof)?;
    controls.set_motion_blur(view.id, motion)?;
    let spatial_aa = camera
        .spatial_aa
        .filter(|_| view.features.get(SPATIAL_AA_FEATURE) != Some(false))
        .map(|settings| SpatialAaSettings {
            edge_threshold: settings.edge_threshold,
            strength: settings.strength,
        });
    controls.set_spatial_aa(view.id, spatial_aa)?;
    Ok(())
}
