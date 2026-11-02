//! Map the authored `Camera3D.screen_space_reflections` into the view's
//! native `engine.ssr` settings.

use anyhow::{Context, Result};
use engine_project::rendering3d::{SSR_FEATURE, SceneRenderFrame};
use engine_render_pipeline::{SsrSettings, ViewData};

/// Set `view.ssr` from the frame's active camera. The settings are validated
/// here, before any upload; an explicit `render_features["engine.ssr"] =
/// false` (already in `view.features`) leaves the view without settings, and
/// every call overwrites the previous camera's state so a camera switch or an
/// authored removal cannot leave stale controls on the view.
pub(super) fn prepare(frame: &SceneRenderFrame, view: &mut ViewData) -> Result<()> {
    view.ssr = None;
    let camera = &frame
        .camera3d
        .as_ref()
        .context("screen-space reflections need a Camera3D")?
        .camera;
    let Some(authored) = camera.screen_space_reflections else {
        return Ok(());
    };
    authored
        .validate()
        .context("validating authored screen-space reflections")?;
    if view.features.get(SSR_FEATURE) == Some(false) {
        return Ok(());
    }
    let settings = SsrSettings {
        max_steps: authored.max_steps,
        max_distance: authored.max_distance,
        thickness: authored.thickness,
        roughness_cutoff: authored.roughness_cutoff,
        edge_fade: authored.edge_fade,
        intensity: authored.intensity,
    };
    settings
        .validate()
        .context("validating screen-space reflection settings")?;
    view.ssr = Some(settings);
    Ok(())
}
