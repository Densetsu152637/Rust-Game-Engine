//! Authored post-processing through the desktop renderer. Ignored by
//! default; run with `--ignored --test-threads=1` on a real adapter.
use engine_desktop::{OffscreenSprites, RenderLimits, WgpuBackends};
use engine_project::{
    Camera2D,
    content::{Mesh, Vertex},
    post::*,
    rendering3d::*,
    runtime::RenderFrame,
};
use glam::{Mat4, Vec3};
use std::sync::Arc;

const SIZE: u32 = 64;

/// A large unlit quad of tint 0.5 straight ahead of a camera at `x`.
fn scene(x: f32) -> SceneRenderFrame {
    let mut frame = SceneRenderFrame::from(RenderFrame {
        camera: Camera2D {
            logical_size: [SIZE, SIZE],
            ..Default::default()
        },
        draws: vec![],
    });
    frame.camera3d = Some(CameraInstance3D {
        camera: Camera3D::default(),
        world: Mat4::from_translation(Vec3::new(x, 0.0, 0.0)).to_cols_array_2d(),
    });
    frame.meshes.push(MeshDraw3D {
        id: "wall".into(),
        mesh: Arc::new(Mesh {
            normals: vec![],
            vertices: [[-20.0, -20.0, 0.0], [20.0, -20.0, 0.0], [0.0, 20.0, 0.0]]
                .into_iter()
                .map(|position| Vertex {
                    position,
                    uv: [0.0; 2],
                    color: [1.0; 4],
                })
                .collect(),
            indices: vec![0, 1, 2],
        }),
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: Mat4::from_translation(Vec3::new(x, 0.0, -2.0)).to_cols_array_2d(),
        material: MeshMaterial {
            tint: [0.5, 0.5, 0.5, 1.0],
            unlit: true,
            ..Default::default()
        },
        shader: None,
    });
    frame
}

fn local_volume() -> VolumeInstance3D {
    let mut volume = PostProcessVolume {
        shape: VolumeShape::Box {
            size: [4.0, 4.0, 4.0],
        },
        ..Default::default()
    };
    volume.exposure.mode = Some(ExposureMode::Manual);
    volume.exposure.ev100 = Some(1.0);
    VolumeInstance3D {
        id: "darkroom".into(),
        volume,
        world: Mat4::IDENTITY.to_cols_array_2d(),
    }
}

fn centre_pixel(renderer: &mut OffscreenSprites) -> anyhow::Result<[u8; 4]> {
    let pixels = renderer.read_rgba()?;
    let at = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    Ok(pixels[at..at + 4].try_into().unwrap())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_camera_inside_a_local_volume_gets_the_override_and_one_outside_does_not() -> anyhow::Result<()>
{
    let mut renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, SIZE, SIZE, RenderLimits::default())?;
    // Reference: no post data at all (the default clamp output).
    renderer.render_scene(&scene(0.0))?;
    let plain = centre_pixel(&mut renderer)?;
    let status = renderer.gpu_renderer().unwrap().post_status().clone();
    assert!(!status.active, "no post data, no post stack");

    // Camera at the origin, inside the box: manual EV100 1 halves the scene.
    let mut inside = scene(0.0);
    inside.post.volumes = vec![local_volume()];
    renderer.render_scene(&inside)?;
    let dark = centre_pixel(&mut renderer)?;
    let status = renderer.gpu_renderer().unwrap().post_status().clone();
    assert!(status.active);
    assert_eq!(status.active_volumes.len(), 1);
    assert_eq!(status.active_volumes[0].id, "darkroom");

    // Camera 20 m away, outside: untouched, identical to the reference.
    let mut outside = scene(20.0);
    outside.post.volumes = vec![local_volume()];
    renderer.render_scene(&outside)?;
    let untouched = centre_pixel(&mut renderer)?;
    let status = renderer.gpu_renderer().unwrap().post_status().clone();
    assert!(!status.active && status.active_volumes.is_empty());
    eprintln!("plain {plain:?}, inside volume {dark:?}, outside volume {untouched:?}");
    assert_eq!(untouched, plain, "outside the volume nothing changes");
    // sRGB(0.5) = 188, sRGB(0.25) = 137.
    assert!((i32::from(plain[0]) - 188).abs() <= 2, "{plain:?}");
    assert!((i32::from(dark[0]) - 137).abs() <= 2, "{dark:?}");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn camera_post_process_tonemaps_the_scene() -> anyhow::Result<()> {
    let mut renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, SIZE, SIZE, RenderLimits::default())?;
    let mut frame = scene(0.0);
    frame.post.camera = Some(PostProcess {
        tonemap: TonemapMode::Aces,
        ..Default::default()
    });
    renderer.render_scene(&frame)?;
    let aces = centre_pixel(&mut renderer)?;
    // ACES(0.5) = 0.6164 linear, sRGB 0.81 -> 207.
    eprintln!("aces(0.5) -> {aces:?}");
    assert!((i32::from(aces[0]) - 207).abs() <= 3, "{aces:?}");
    let status = renderer.gpu_renderer().unwrap().post_status().clone();
    assert_eq!(status.tonemap, Some(TonemapMode::Aces));
    Ok(())
}
