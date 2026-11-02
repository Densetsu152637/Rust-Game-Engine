//! Map the authored camera clouds into the native per-view controls.

use anyhow::{Context, Result};
use bevy_ecs::world::World;
use engine_project::rendering3d::{CLOUDS_FEATURE, Clouds3D, SceneRenderFrame};
use engine_render_pipeline::ViewData;
use engine_render_pipeline::clouds::{CloudControls, CloudFrame, CloudSettings, absolute_position};

fn settings(authored: &Clouds3D) -> CloudSettings {
    CloudSettings {
        base_altitude: authored.base_altitude,
        top_altitude: authored.top_altitude,
        density: authored.density,
        coverage: authored.coverage,
        albedo: authored.albedo,
        anisotropy: authored.anisotropy,
        sun_intensity: authored.sun_intensity,
        ambient_intensity: authored.ambient_intensity,
        multiple_scattering: authored.multiple_scattering,
        noise_amount: authored.noise_amount,
        noise_scale: authored.noise_scale,
        noise_octaves: authored.noise_octaves,
        noise_seed: authored.noise_seed,
        wind: authored.wind,
        max_distance: authored.max_distance,
        fade_distance: authored.fade_distance,
        primary_steps: authored.primary_steps,
        light_steps: authored.light_steps,
        light_distance: authored.light_distance,
    }
}

/// Store (or clear) this view's clouds. The camera position is absolute: the
/// applied floating origin plus the origin-relative camera, summed in `f64`;
/// the wind reads the scene clock of the frame, never the wall clock. A camera
/// below a water surface gets no clouds (the water's own medium owns the view,
/// as for the air fog). `view.features` must already hold the camera's
/// effective overrides.
pub(super) fn prepare(
    controls: &CloudControls,
    frame: &SceneRenderFrame,
    view: &ViewData,
    world: Option<&World>,
) -> Result<()> {
    let instance = frame.camera3d.as_ref().context("clouds need a Camera3D")?;
    let camera = &instance.camera;
    camera
        .validate()
        .context("validating the authored clouds")?;
    let active = camera.clouds.filter(|_| {
        view.features.get(CLOUDS_FEATURE) != Some(false) && !super::fog::submerged(view, world)
    });
    let clouds = active
        .map(|authored| {
            let [x, y, z, _] = instance.world[3];
            let position = absolute_position(frame.applied_origin, [x, y, z]);
            CloudFrame::new(settings(&authored), position, frame.scene_time)
        })
        .transpose()?;
    controls.set(view.id, clouds)
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

    fn frame_at(camera: Camera3D, position: Vec3, origin: [f64; 3], time: f64) -> SceneRenderFrame {
        let mut frame = SceneRenderFrame::from(RenderFrame {
            camera: Camera2D::default(),
            draws: vec![],
        });
        frame.camera3d = Some(CameraInstance3D {
            camera,
            world: Mat4::from_translation(position).to_cols_array_2d(),
        });
        frame.applied_origin = origin;
        frame.scene_time = time;
        frame
    }

    fn view(id: u64, camera: &Camera3D, position: Vec3) -> ViewData {
        let mut view = ViewData::new(
            ViewId(id),
            Mat4::from_translation(position),
            glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 10.0),
            [8, 8],
            [8, 8],
        )
        .unwrap();
        view.features = FeatureOverrides(camera.effective_render_features());
        view
    }

    fn clouded_camera() -> Camera3D {
        Camera3D {
            clouds: Some(Clouds3D {
                density: 0.02,
                wind: [1.0, 0.0, 2.0],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn position_sums_the_applied_origin_in_f64_and_time_is_the_scene_clock() {
        let controls = CloudControls::default();
        let camera = clouded_camera();
        let at = Vec3::new(3.0, 2.5, 4.0);
        let v = view(5, &camera, at);
        let read = |controls: &CloudControls| controls.get(v.id).unwrap().unwrap();
        prepare(
            &controls,
            &frame_at(camera.clone(), at, [10.0, 2000.0, -20.0], 12.5),
            &v,
            None,
        )
        .unwrap();
        let frame = read(&controls);
        assert_eq!(frame.camera_position, [13.0, 2002.5, -16.0]);
        assert_eq!(frame.time, 12.5, "the frame's scene clock, not the wall");
        assert_eq!(frame.settings.density, 0.02);
        assert_eq!(frame.settings.wind, [1.0, 0.0, 2.0]);
        assert_eq!(frame.settings.base_altitude, 1500.0);
        // A far origin keeps sub-metre precision an f32 sum would lose.
        prepare(
            &controls,
            &frame_at(
                camera.clone(),
                Vec3::new(1.5, 1.5, 1.5),
                [1.0e9, 0.0, 0.0],
                0.0,
            ),
            &v,
            None,
        )
        .unwrap();
        assert!((read(&controls).camera_position[0] - 1_000_000_001.5).abs() < 1e-6);
    }

    #[test]
    fn every_authored_field_reaches_the_settings() {
        let authored = Clouds3D {
            base_altitude: 1.0,
            top_altitude: 2000.0,
            density: 0.1,
            coverage: 0.2,
            albedo: [0.3, 0.4, 0.5],
            anisotropy: -0.6,
            sun_intensity: 7.0,
            ambient_intensity: 8.0,
            multiple_scattering: false,
            noise_amount: 0.9,
            noise_scale: 33.0,
            noise_octaves: 4,
            noise_seed: 11,
            wind: [1.0, 2.0, 3.0],
            max_distance: 5000.0,
            fade_distance: 6000.0,
            primary_steps: 17,
            light_steps: 3,
            light_distance: 90.0,
        };
        let mapped = settings(&authored);
        assert_eq!(mapped.top_altitude, 2000.0);
        assert_eq!(mapped.albedo, [0.3, 0.4, 0.5]);
        assert!(!mapped.multiple_scattering);
        assert_eq!(
            (
                mapped.noise_octaves,
                mapped.primary_steps,
                mapped.light_steps
            ),
            (4, 17, 3)
        );
        assert_eq!(mapped.light_distance, 90.0);
        assert_eq!(mapped.fade_distance, 6000.0);
        mapped.validate().unwrap();
    }

    #[test]
    fn absent_settings_and_the_disabled_feature_clear_the_view() {
        let controls = CloudControls::default();
        let camera = clouded_camera();
        let frame = frame_at(camera.clone(), Vec3::ZERO, [0.0; 3], 0.0);
        let on = view(6, &camera, Vec3::ZERO);
        prepare(&controls, &frame, &on, None).unwrap();
        assert!(controls.get(on.id).unwrap().is_some());

        let mut off = camera.clone();
        off.render_features.insert(CLOUDS_FEATURE.into(), false);
        prepare(&controls, &frame, &view(6, &off, Vec3::ZERO), None).unwrap();
        assert!(controls.get(on.id).unwrap().is_none());
        prepare(&controls, &frame, &on, None).unwrap();
        assert!(controls.get(on.id).unwrap().is_some());
        let plain = frame_at(Default::default(), Vec3::ZERO, [0.0; 3], 0.0);
        prepare(
            &controls,
            &plain,
            &view(6, &Default::default(), Vec3::ZERO),
            None,
        )
        .unwrap();
        assert!(controls.get(on.id).unwrap().is_none());
    }

    #[test]
    fn invalid_authored_settings_fail_before_any_gpu_work() {
        let controls = CloudControls::default();
        let camera = Camera3D {
            clouds: Some(Clouds3D {
                base_altitude: 5000.0,
                top_altitude: 100.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let frame = frame_at(camera.clone(), Vec3::ZERO, [0.0; 3], 0.0);
        assert!(prepare(&controls, &frame, &view(7, &camera, Vec3::ZERO), None).is_err());
        assert_eq!(controls.view_count(), 0);
    }
}
