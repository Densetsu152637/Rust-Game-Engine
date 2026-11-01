//! Map the authored camera atmosphere into the native per-view controls.

use anyhow::{Context, Result};
use engine_project::rendering3d::{
    ATMOSPHERE_AERIAL_FEATURE, ATMOSPHERE_FEATURE, Atmosphere3D, SceneRenderFrame,
};
use engine_render_pipeline::ViewData;
use engine_render_pipeline::atmosphere::{
    AtmosphereControls, AtmosphereFrame, AtmosphereSettings, absolute_altitude,
};

fn settings(authored: &Atmosphere3D) -> AtmosphereSettings {
    AtmosphereSettings {
        planet_radius: authored.planet_radius,
        atmosphere_radius: authored.atmosphere_radius,
        rayleigh_scattering: authored.rayleigh_scattering,
        rayleigh_scale_height: authored.rayleigh_scale_height,
        mie_scattering: authored.mie_scattering,
        mie_absorption: authored.mie_absorption,
        mie_scale_height: authored.mie_scale_height,
        mie_anisotropy: authored.mie_anisotropy,
        ozone_absorption: authored.ozone_absorption,
        ozone_center_height: authored.ozone_center_height,
        ozone_width: authored.ozone_width,
        ground_albedo: authored.ground_albedo,
        sun_angular_diameter: authored.sun_angular_diameter,
        intensity: authored.intensity,
        aerial_perspective_scale: authored.aerial_perspective_scale,
    }
}

/// Store (or clear) this view's atmosphere. The altitude is the camera's
/// absolute height: the applied floating origin plus the origin-relative
/// camera position, summed in `f64`. `view.features` must already hold the
/// camera's effective overrides.
pub(super) fn prepare(
    controls: &AtmosphereControls,
    frame: &SceneRenderFrame,
    view: &ViewData,
) -> Result<()> {
    let instance = frame
        .camera3d
        .as_ref()
        .context("an atmosphere needs a Camera3D")?;
    let camera = &instance.camera;
    camera
        .validate()
        .context("validating the authored atmosphere")?;
    let enabled = |name: &str| view.features.get(name) != Some(false);
    let active = camera
        .atmosphere
        .filter(|_| enabled(ATMOSPHERE_FEATURE) || enabled(ATMOSPHERE_AERIAL_FEATURE));
    let atmosphere = active
        .map(|authored| {
            let altitude = absolute_altitude(frame.applied_origin[1], instance.world[3][1]);
            AtmosphereFrame::new(settings(&authored), altitude)
        })
        .transpose()?;
    controls.set(view.id, atmosphere)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        Camera2D,
        rendering3d::{Camera3D, CameraInstance3D},
        runtime::RenderFrame,
    };
    use engine_render_pipeline::{FeatureOverrides, ViewId};
    use glam::{Mat4, Vec3};

    fn frame_at(camera: Camera3D, y: f32, origin_y: f64) -> SceneRenderFrame {
        let mut frame = SceneRenderFrame::from(RenderFrame {
            camera: Camera2D::default(),
            draws: vec![],
        });
        frame.camera3d = Some(CameraInstance3D {
            camera,
            world: Mat4::from_translation(Vec3::new(3.0, y, 4.0)).to_cols_array_2d(),
        });
        frame.applied_origin = [10.0, origin_y, -20.0];
        frame
    }

    fn view(id: u64, camera: &Camera3D) -> ViewData {
        let mut view = ViewData::new(
            ViewId(id),
            Mat4::IDENTITY,
            glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 10.0),
            [8, 8],
            [8, 8],
        )
        .unwrap();
        view.features = FeatureOverrides(camera.effective_render_features());
        view
    }

    #[test]
    fn altitude_sums_the_applied_origin_and_the_camera_in_f64() {
        let controls = AtmosphereControls::default();
        let camera = Camera3D {
            atmosphere: Some(Atmosphere3D::default()),
            ..Default::default()
        };
        let v = view(5, &camera);
        let altitude =
            |controls: &AtmosphereControls| controls.get(v.id).unwrap().unwrap().camera_altitude;
        // The same absolute height reached with and without a rebase.
        prepare(&controls, &frame_at(camera.clone(), 2.5, 2000.0), &v).unwrap();
        assert_eq!(altitude(&controls), 2002.5);
        prepare(&controls, &frame_at(camera.clone(), 2002.5, 0.0), &v).unwrap();
        assert_eq!(altitude(&controls), 2002.5);
        // A far origin keeps sub-metre precision an f32 sum would lose.
        prepare(&controls, &frame_at(camera.clone(), 1.5, 1.0e9), &v).unwrap();
        assert!((altitude(&controls) - 1_000_000_001.5).abs() < 1e-6);
    }

    #[test]
    fn absent_settings_and_disabled_features_clear_the_view() {
        let controls = AtmosphereControls::default();
        let camera = Camera3D {
            atmosphere: Some(Atmosphere3D {
                intensity: 2.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let frame = frame_at(camera.clone(), 0.0, 0.0);
        let on = view(6, &camera);
        prepare(&controls, &frame, &on).unwrap();
        assert_eq!(
            controls.get(on.id).unwrap().unwrap().settings.intensity,
            2.0
        );

        // Only the aerial pass disabled: the sky still needs the settings.
        let mut sky_only = camera.clone();
        sky_only
            .render_features
            .insert(ATMOSPHERE_AERIAL_FEATURE.into(), false);
        prepare(&controls, &frame, &view(6, &sky_only)).unwrap();
        assert!(controls.get(on.id).unwrap().is_some());

        // Both passes disabled, then no authored atmosphere at all.
        let mut off = sky_only.clone();
        off.render_features.insert(ATMOSPHERE_FEATURE.into(), false);
        prepare(&controls, &frame, &view(6, &off)).unwrap();
        assert!(controls.get(on.id).unwrap().is_none());
        prepare(&controls, &frame, &on).unwrap();
        assert!(controls.get(on.id).unwrap().is_some());
        let plain = frame_at(Default::default(), 0.0, 0.0);
        prepare(&controls, &plain, &view(6, &Default::default())).unwrap();
        assert!(controls.get(on.id).unwrap().is_none());
    }

    #[test]
    fn invalid_authored_settings_fail_before_any_gpu_work() {
        let controls = AtmosphereControls::default();
        let camera = Camera3D {
            atmosphere: Some(Atmosphere3D {
                mie_anisotropy: 1.5,
                ..Default::default()
            }),
            ..Default::default()
        };
        let frame = frame_at(camera.clone(), 0.0, 0.0);
        assert!(prepare(&controls, &frame, &view(7, &camera)).is_err());
        assert_eq!(controls.view_count(), 0);
    }
}
