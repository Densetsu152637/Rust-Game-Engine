//! Map the authored `Camera3D.decals` into the view's native `engine.decals`
//! list.

use anyhow::{Context, Result};
use engine_project::rendering3d::{
    DECALS_FEATURE, Decal3D, DecalBlend3D, MAX_DECALS_3D, SceneRenderFrame,
};
use engine_render_pipeline::{Decal, DecalBlend, ViewData};
use glam::{Quat, Vec3};

/// The native decal of an authored one, translated by the applied floating
/// origin so it matches the extracted camera and mesh transforms.
pub(super) fn native(authored: &Decal3D, applied_origin: [f64; 3]) -> Decal {
    let local = |i: usize| (f64::from(authored.position[i]) - applied_origin[i]) as f32;
    let [x, y, z, w] = authored.rotation;
    Decal {
        center: Vec3::new(local(0), local(1), local(2)),
        rotation: Quat::from_xyzw(x, y, z, w),
        half_extents: Vec3::from(authored.half_extents),
        albedo: Vec3::from(authored.albedo),
        opacity: authored.opacity,
        blend: match authored.blend {
            DecalBlend3D::Multiply => DecalBlend::Multiply,
            DecalBlend3D::Paint => DecalBlend::Paint,
        },
        emissive: Vec3::from(authored.emissive),
        max_angle: authored.max_angle,
        angle_fade: authored.angle_fade,
        edge_fade: authored.edge_fade,
        ..Default::default()
    }
}

/// Set `view.decals` from the frame's active camera. Authored decals are
/// validated here, before any upload. An explicit
/// `render_features["engine.decals"] = false` (already in `view.features`)
/// leaves the view without decals, and every call overwrites the previous
/// camera's list so a camera switch or an authored removal cannot leave stale
/// decals on the view.
pub(super) fn prepare(frame: &SceneRenderFrame, view: &mut ViewData) -> Result<()> {
    view.decals.clear();
    let camera = &frame
        .camera3d
        .as_ref()
        .context("decals need a Camera3D")?
        .camera;
    if camera.decals.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        camera.decals.len() <= MAX_DECALS_3D,
        "a camera accepts at most {MAX_DECALS_3D} decals"
    );
    for (index, decal) in camera.decals.iter().enumerate() {
        decal
            .validate()
            .with_context(|| format!("validating authored decal {index}"))?;
    }
    if view.features.get(DECALS_FEATURE) == Some(false) {
        return Ok(());
    }
    view.decals = camera
        .decals
        .iter()
        .map(|decal| native(decal, frame.applied_origin))
        .collect();
    for (index, decal) in view.decals.iter().enumerate() {
        decal
            .validate()
            .with_context(|| format!("validating decal {index}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        Camera2D,
        rendering3d::{Camera3D, CameraInstance3D},
        runtime::RenderFrame,
    };
    use glam::Mat4;

    fn frame_with(camera: Camera3D) -> SceneRenderFrame {
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

    fn view() -> ViewData {
        ViewData::new(
            engine_render_pipeline::ViewId(3),
            Mat4::IDENTITY,
            glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 10.0),
            [8, 8],
            [8, 8],
        )
        .unwrap()
    }

    #[test]
    fn authored_decals_reach_the_view_with_the_floating_origin_applied() {
        let camera = Camera3D {
            decals: vec![Decal3D {
                position: [1010.0, 2.0, -5.0],
                half_extents: [1.0, 2.0, 3.0],
                albedo: [0.5, 0.25, 0.125],
                blend: DecalBlend3D::Paint,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut frame = frame_with(camera);
        frame.applied_origin = [1000.0, 0.0, 0.0];
        let mut view = view();
        prepare(&frame, &mut view).unwrap();
        assert_eq!(view.decals.len(), 1);
        let decal = &view.decals[0];
        assert_eq!(decal.center, Vec3::new(10.0, 2.0, -5.0));
        assert_eq!(decal.half_extents, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(decal.albedo, Vec3::new(0.5, 0.25, 0.125));
        assert_eq!(decal.blend, DecalBlend::Paint);
        decal.validate().unwrap();
    }

    #[test]
    fn removal_override_and_invalid_input_are_handled_before_submission() {
        let with_decal = Camera3D {
            decals: vec![Decal3D::default()],
            ..Default::default()
        };
        let mut view = view();
        prepare(&frame_with(with_decal.clone()), &mut view).unwrap();
        assert_eq!(view.decals.len(), 1);

        // Removing the authored decals clears the previous list.
        prepare(&frame_with(Camera3D::default()), &mut view).unwrap();
        assert!(view.decals.is_empty());

        // An explicit render_features override keeps the view decal-free.
        let mut disabled = view.clone();
        disabled.features.set(DECALS_FEATURE, false);
        prepare(&frame_with(with_decal), &mut disabled).unwrap();
        assert!(disabled.decals.is_empty());

        // Invalid authored decals fail with their index and leave nothing behind.
        let bad = Camera3D {
            decals: vec![
                Decal3D::default(),
                Decal3D {
                    opacity: f32::NAN,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let error = prepare(&frame_with(bad), &mut view).unwrap_err();
        assert!(format!("{error:#}").contains("decal 1"), "{error:#}");
        assert!(view.decals.is_empty());
    }
}
