//! Map the authored camera fog into the native per-view controls.

use anyhow::{Context, Result};
use bevy_ecs::world::World;
use engine_project::rendering3d::{FOG_FEATURE, Fog3D, SceneRenderFrame};
use engine_project::world::water::WaterRenderSet;
use engine_render_pipeline::ViewData;
use engine_render_pipeline::fog::{FogControls, FogFrame, FogSettings, absolute_position};

fn settings(authored: &Fog3D) -> FogSettings {
    FogSettings {
        density: authored.density,
        height_falloff: authored.height_falloff,
        base_height: authored.base_height,
        albedo: authored.albedo,
        anisotropy: authored.anisotropy,
        sun_intensity: authored.sun_intensity,
        ambient_intensity: authored.ambient_intensity,
        sky_distance: authored.sky_distance,
        noise_amount: authored.noise_amount,
        noise_scale: authored.noise_scale,
        noise_distance: authored.noise_distance,
        noise_steps: authored.noise_steps,
        noise_seed: authored.noise_seed,
        wind: authored.wind,
    }
}

/// Whether the view's camera is below a water surface that the underwater pass
/// fogs. The air fog then stays off: the water's own fog owns the medium, so
/// no pixel is fogged twice. Uses the same test as `world.water.underwater`.
pub(super) fn submerged(view: &ViewData, world: Option<&World>) -> bool {
    if view
        .features
        .get(engine_world_render::names::WATER_UNDERWATER)
        == Some(false)
    {
        return false;
    }
    world
        .and_then(|world| world.get_resource::<WaterRenderSet>())
        .is_some_and(|set| {
            engine_world_render::water::camera_underwater(set, view.camera_position()).is_some()
        })
}

/// Store (or clear) this view's fog. The camera position is absolute: the
/// applied floating origin plus the origin-relative camera, summed in `f64`;
/// the wind reads the scene clock of the frame, never the wall clock.
/// `view.features` must already hold the camera's effective overrides.
pub(super) fn prepare(
    controls: &FogControls,
    frame: &SceneRenderFrame,
    view: &ViewData,
    world: Option<&World>,
) -> Result<()> {
    let instance = frame.camera3d.as_ref().context("a fog needs a Camera3D")?;
    let camera = &instance.camera;
    camera.validate().context("validating the authored fog")?;
    let active = camera
        .fog
        .filter(|_| view.features.get(FOG_FEATURE) != Some(false) && !submerged(view, world));
    let fog = active
        .map(|authored| {
            let [x, y, z, _] = instance.world[3];
            let position = absolute_position(frame.applied_origin, [x, y, z]);
            FogFrame::new(settings(&authored), position, frame.scene_time)
        })
        .transpose()?;
    controls.set(view.id, fog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        Camera2D,
        rendering3d::{Camera3D, CameraInstance3D},
        runtime::RenderFrame,
        world::water::WaterRenderItem,
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

    fn fogged_camera() -> Camera3D {
        Camera3D {
            fog: Some(Fog3D {
                density: 0.02,
                wind: [1.0, 0.0, 2.0],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn position_sums_the_applied_origin_in_f64_and_time_is_the_scene_clock() {
        let controls = FogControls::default();
        let camera = fogged_camera();
        let at = Vec3::new(3.0, 2.5, 4.0);
        let v = view(5, &camera, at);
        let read = |controls: &FogControls| controls.get(v.id).unwrap().unwrap();
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
    fn absent_settings_and_the_disabled_feature_clear_the_view() {
        let controls = FogControls::default();
        let camera = fogged_camera();
        let frame = frame_at(camera.clone(), Vec3::ZERO, [0.0; 3], 0.0);
        let on = view(6, &camera, Vec3::ZERO);
        prepare(&controls, &frame, &on, None).unwrap();
        assert!(controls.get(on.id).unwrap().is_some());

        let mut off = camera.clone();
        off.render_features.insert(FOG_FEATURE.into(), false);
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
        let controls = FogControls::default();
        let camera = Camera3D {
            fog: Some(Fog3D {
                anisotropy: 1.5,
                ..Default::default()
            }),
            ..Default::default()
        };
        let frame = frame_at(camera.clone(), Vec3::ZERO, [0.0; 3], 0.0);
        assert!(prepare(&controls, &frame, &view(7, &camera, Vec3::ZERO), None).is_err());
        assert_eq!(controls.view_count(), 0);
    }

    /// A flat lake whose surface is the plane `y = 0`, 1 km across.
    fn lake() -> WaterRenderSet {
        use engine_water::{ShorelineSettings, WaterOptics, WaterShape, WaterSurface, Waves};
        let surface = WaterSurface {
            shape: WaterShape::Lake {
                half_extent: glam::Vec2::splat(500.0),
            },
            waves: Waves::Flat,
            optics: WaterOptics::default(),
            shoreline: ShorelineSettings::default(),
            time: 0.0,
        };
        WaterRenderSet {
            items: vec![WaterRenderItem {
                owner: bevy_ecs::entity::Entity::PLACEHOLDER,
                id: "lake".into(),
                kind: engine_project::world::water::WaterKind::Lake,
                surface: std::sync::Arc::new(surface),
                world_from_local: Mat4::IDENTITY.to_cols_array_2d(),
                half_extent: [500.0, 500.0],
                grid_resolution: 8,
                cast_shadows: false,
                shore: None,
                revision: 1,
            }],
            time: 0.0,
        }
    }

    #[test]
    fn a_submerged_camera_gets_no_air_fog() {
        let controls = FogControls::default();
        let camera = fogged_camera();
        let mut world = World::new();
        world.insert_resource(lake());
        let frame = frame_at(camera.clone(), Vec3::new(0.0, -3.0, 0.0), [0.0; 3], 0.0);
        let below = view(8, &camera, Vec3::new(0.0, -3.0, 0.0));
        let above = view(8, &camera, Vec3::new(0.0, 4.0, 0.0));
        // Under the surface: the water's fog owns the medium.
        prepare(&controls, &frame, &below, Some(&world)).unwrap();
        assert!(controls.get(below.id).unwrap().is_none(), "no double fog");
        // Above it the fog applies, and so it does without any water.
        prepare(&controls, &frame, &above, Some(&world)).unwrap();
        assert!(controls.get(above.id).unwrap().is_some());
        prepare(&controls, &frame, &below, None).unwrap();
        assert!(controls.get(below.id).unwrap().is_some());
        // The water pass disabled in the view: nothing else fogs, so the air
        // fog stays.
        let mut no_water_fog = camera.clone();
        no_water_fog
            .render_features
            .insert(engine_world_render::names::WATER_UNDERWATER.into(), false);
        let submerged_without_pass = view(8, &no_water_fog, Vec3::new(0.0, -3.0, 0.0));
        prepare(&controls, &frame, &submerged_without_pass, Some(&world)).unwrap();
        assert!(controls.get(below.id).unwrap().is_some());
    }
}
