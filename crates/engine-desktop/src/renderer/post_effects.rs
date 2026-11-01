//! Map validated camera effect settings into the native per-view controls.

use anyhow::{Context, Result};
use engine_project::rendering3d::{
    DEPTH_OF_FIELD_FEATURE, MOTION_BLUR_FEATURE, SPATIAL_AA_FEATURE, SceneRenderFrame,
};
use engine_render_pipeline::{FeatureOverrides, ViewData};
use engine_render_post_effects::{
    DepthOfFieldSettings, MotionBlurSettings, PostEffects, SpatialAaSettings,
};

/// Whether the camera asks for spatial AA (a present setting that
/// `render_features` does not explicitly disable).
pub(super) fn spatial_aa_requested(camera: &engine_project::rendering3d::Camera3D) -> bool {
    camera.spatial_aa.is_some()
        && camera
            .effective_render_features()
            .get(SPATIAL_AA_FEATURE)
            .copied()
            != Some(false)
}

/// `host_upscaled` is true when a temporal resolve (DLSS Super Resolution or
/// the native temporal resolve) owns the Upscale stage of this draw. Spatial AA
/// is then switched off for the view: the temporal resolve already
/// anti-aliases and a second spatial filter would only soften its output. The
/// pipeline report shows the feature as `disabled` and the upscaler status
/// sets `spatial_aa_skipped`.
pub(super) fn prepare(
    controls: &PostEffects,
    frame: &SceneRenderFrame,
    view: &mut ViewData,
    host_upscaled: bool,
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
    if host_upscaled {
        view.features.set(SPATIAL_AA_FEATURE, false);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        Camera2D,
        rendering3d::{CameraInstance3D, SpatialAa3D},
        runtime::RenderFrame,
    };
    use glam::Mat4;

    fn frame_with(camera: engine_project::rendering3d::Camera3D) -> SceneRenderFrame {
        let mut frame = SceneRenderFrame::from(RenderFrame {
            camera: Camera2D::default(),
            draws: vec![],
        });
        frame.camera3d = Some(CameraInstance3D {
            camera,
            world: Mat4::IDENTITY.to_cols_array_2d(),
        });
        frame
    }

    fn view(id: u64) -> ViewData {
        ViewData::new(
            engine_render_pipeline::ViewId(id),
            Mat4::IDENTITY,
            glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 10.0),
            [8, 8],
            [8, 8],
        )
        .unwrap()
    }

    #[test]
    fn spatial_aa_follows_the_camera_and_yields_to_a_host_upscaler() {
        let controls = PostEffects::default();
        let camera = engine_project::rendering3d::Camera3D {
            spatial_aa: Some(SpatialAa3D {
                edge_threshold: 0.2,
                strength: 0.5,
            }),
            ..Default::default()
        };
        let frame = frame_with(camera);
        let mut native = view(7);
        prepare(&controls, &frame, &mut native, false).unwrap();
        assert_eq!(native.features.get(SPATIAL_AA_FEATURE), Some(true));
        let stored = controls.spatial_aa(native.id).unwrap().unwrap();
        assert_eq!((stored.edge_threshold, stored.strength), (0.2, 0.5));

        let mut upscaled = view(7);
        prepare(&controls, &frame, &mut upscaled, true).unwrap();
        assert_eq!(upscaled.features.get(SPATIAL_AA_FEATURE), Some(false));
        assert!(controls.spatial_aa(upscaled.id).unwrap().is_none());

        // Returning to a native draw restores the authored settings.
        let mut again = view(7);
        prepare(&controls, &frame, &mut again, false).unwrap();
        assert!(controls.spatial_aa(again.id).unwrap().is_some());

        // No camera setting: nothing is stored and the feature stays unset.
        let plain = frame_with(Default::default());
        let mut off = view(7);
        prepare(&controls, &plain, &mut off, false).unwrap();
        assert!(controls.spatial_aa(off.id).unwrap().is_none());
        assert_ne!(off.features.get(SPATIAL_AA_FEATURE), Some(true));
    }
}
