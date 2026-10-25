//! DLSS Frame Generation in the desktop renderer on a real GPU.
//!
//! `cargo test -p engine-desktop --test frame_generation -- --ignored` checks
//! the native fallback on any adapter. With the `dlss` feature, the SDK at
//! build time and `NGX_RUNTIME_DIR`/`NGX_LOG_DIR` naming a directory holding
//! both `nvngx_dlss.dll` and `nvngx_dlssg.dll`, the NGX test renders a moving
//! engine scene and inspects the generated frames through the capture API.

use engine_desktop::{GpuContext, GpuRenderer, OffscreenTarget, RenderLimits};
use engine_project::{
    Camera2D,
    content::{Mesh, Vertex},
    rendering3d::*,
    runtime::{Draw, DrawSpace, RenderFrame},
};
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 384;
/// Red triangle origin x per real frame, in world units (about 12 px).
#[cfg(feature = "dlss")]
const STEP: f32 = 0.15;

fn mesh(vertices: &[[f32; 3]], indices: Vec<u32>) -> Arc<Mesh> {
    Arc::new(Mesh {
        vertices: vertices
            .iter()
            .map(|&position| Vertex {
                position,
                uv: [0.0; 2],
                color: [1.0; 4],
            })
            .collect(),
        normals: vec![],
        indices,
    })
}

fn draw(id: &str, mesh: Arc<Mesh>, model: Mat4, tint: [f32; 4]) -> MeshDraw3D {
    MeshDraw3D {
        id: id.into(),
        mesh,
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: model.to_cols_array_2d(),
        material: MeshMaterial {
            tint,
            unlit: true,
            ..Default::default()
        },
        shader: None,
    }
}

/// A gray floor, an orange panel, a red triangle at `x` and a white UI square.
fn scene(x: f32, camera_x: f32, generation: FrameGenerationSettings) -> SceneRenderFrame {
    let quad = |half: f32| {
        mesh(
            &[
                [-half, -half, 0.0],
                [half, -half, 0.0],
                [half, half, 0.0],
                [-half, half, 0.0],
            ],
            vec![0, 1, 2, 0, 2, 3],
        )
    };
    let hud = mesh(
        &[
            [8.0, 8.0, 0.5],
            [40.0, 8.0, 0.5],
            [40.0, 40.0, 0.5],
            [8.0, 40.0, 0.5],
        ],
        vec![0, 1, 2, 0, 2, 3],
    );
    SceneRenderFrame {
        sprites: RenderFrame {
            camera: Camera2D {
                logical_size: [WIDTH, HEIGHT],
                ..Default::default()
            },
            draws: vec![Draw {
                id: "hud".into(),
                mesh: hud,
                texture: None,
                layer: 0,
                space: DrawSpace::Ui,
                clip: None,
                state: Default::default(),
                motion: None,
                offset: [0.0; 3],
            }],
        },
        camera3d: Some(CameraInstance3D {
            camera: Camera3D {
                frame_generation: generation,
                ..Default::default()
            },
            world: Mat4::from_translation(Vec3::new(camera_x, 0.4, 4.0)).to_cols_array_2d(),
        }),
        meshes: vec![
            draw(
                "floor",
                quad(4.0),
                Mat4::from_rotation_translation(
                    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
                    Vec3::new(0.0, -1.0, 0.0),
                ),
                [0.35, 0.35, 0.4, 1.0],
            ),
            draw(
                "panel",
                quad(0.8),
                Mat4::from_rotation_translation(
                    Quat::from_rotation_y(0.6),
                    Vec3::new(-1.2, 0.0, -0.5),
                ),
                [0.9, 0.6, 0.2, 1.0],
            ),
            draw(
                "red",
                mesh(
                    &[[-0.6, -0.5, 0.0], [0.6, -0.5, 0.0], [0.0, 0.7, 0.0]],
                    vec![0, 1, 2],
                ),
                Mat4::from_translation(Vec3::new(x, 0.1, 0.0)),
                [1.0, 0.05, 0.05, 1.0],
            ),
        ],
        materials2d: Default::default(),
        lights: vec![],
        post: Default::default(),
        streams: vec![],
    }
}

fn dlss_fg(multiplier: u32) -> FrameGenerationSettings {
    FrameGenerationSettings {
        mode: FrameGenerationMode::DlssFg,
        multiplier,
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn frame_generation_without_a_prepared_device_presents_real_frames_with_a_reason()
-> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, WIDTH, HEIGHT)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    renderer.render_scene(
        &scene(0.0, 0.0, FrameGenerationSettings::default()),
        &target.view,
        None,
    )?;
    assert_eq!(
        renderer.frame_generation_status().state,
        FrameGenerationState::Off
    );
    let native = target.read_rgba(&renderer.gpu)?;
    for _ in 0..2 {
        renderer.render_scene(&scene(0.0, 0.0, dlss_fg(2)), &target.view, None)?;
    }
    let status = renderer.frame_generation_status().clone();
    eprintln!("fallback status: {status:?}");
    assert_eq!(status.state, FrameGenerationState::Fallback);
    assert_eq!((status.generated_frames, status.presented_frames), (0, 0));
    assert!(status.multiplier.is_none());
    let reason = status.fallback_reason.unwrap();
    if cfg!(feature = "dlss") {
        assert!(
            reason.contains("without DLSS extensions") || reason.contains("NGX"),
            "{reason}"
        );
    } else {
        assert_eq!(
            reason,
            "engine-desktop was built without the `dlss` feature"
        );
    }
    assert!(
        renderer
            .render_generated(&scene(0.0, 0.0, dlss_fg(2)))?
            .is_none()
    );
    assert_eq!(renderer.generated_frame_count(), 0);
    assert!(renderer.capture_generated()?.is_empty());
    assert!(
        target.read_rgba(&renderer.gpu)? == native,
        "real frames only"
    );
    Ok(())
}

#[cfg(feature = "dlss")]
mod ngx {
    use super::*;
    use engine_desktop::{CapturedFrame, DlssConfig, DlssRequest, WgpuBackends, WgpuOptions};

    fn centroid(frame: &CapturedFrame) -> Option<[f64; 2]> {
        let (mut x, mut y, mut n) = (0.0, 0.0, 0.0);
        for (i, p) in frame.rgba8.as_chunks::<4>().0.iter().enumerate() {
            if p[0] > 200 && p[1] < 80 && p[2] < 80 {
                x += (i as u32 % frame.width) as f64;
                y += (i as u32 / frame.width) as f64;
                n += 1.0;
            }
        }
        (n > 100.0).then(|| [x / n, y / n])
    }

    fn pixel(frame: &CapturedFrame, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * frame.width + x) * 4) as usize;
        frame.rgba8[i..i + 4].try_into().unwrap()
    }

    struct Pair {
        previous: [f64; 2],
        current: [f64; 2],
        generated: Vec<[f64; 2]>,
    }

    /// Render real frames with the triangle at `positions` (and the camera at
    /// `cameras`), returning per frame the real and generated centroids.
    fn run(
        renderer: &mut GpuRenderer,
        target: &OffscreenTarget,
        frames: &[(f32, f32, FrameGenerationSettings, UpscalerSettings)],
    ) -> anyhow::Result<Vec<Pair>> {
        let mut previous = None;
        let mut pairs = Vec::new();
        for &(x, camera_x, generation, upscaler) in frames {
            let mut frame = scene(x, camera_x, generation);
            frame.camera3d.as_mut().unwrap().camera.upscaler = upscaler;
            renderer.render_scene(&frame, &target.view, None)?;
            let captured = renderer.capture_generated()?;
            let (real, generated) = captured.split_last().expect("real frame captured");
            assert!(!real.generated && generated.iter().all(|f| f.generated));
            // The HUD is composited onto the real frame and kept out of the
            // HUD-less input: generated frames show it unsmeared.
            for frame in generated {
                assert_eq!(pixel(frame, 24, 24), [255, 255, 255, 255], "HUD interior");
                assert_eq!(pixel(frame, 24, 24), pixel(real, 24, 24));
                for (x, y) in [(9, 9), (39, 39), (24, 9)] {
                    let (g, r) = (pixel(frame, x, y), pixel(real, x, y));
                    assert!(
                        g.iter().zip(r).all(|(a, b)| a.abs_diff(b) <= 2),
                        "HUD edge pixel ({x}, {y}): generated {g:?} vs real {r:?}"
                    );
                }
            }
            let current = centroid(real).expect("visible triangle");
            if let Some(previous) = previous {
                pairs.push(Pair {
                    previous,
                    current,
                    generated: generated
                        .iter()
                        .map(|f| centroid(f).expect("visible"))
                        .collect(),
                });
            }
            previous = Some(current);
        }
        Ok(pairs)
    }

    /// Largest error, in pixels, of generated centroids from evenly spaced
    /// positions between the real frames.
    fn spacing_error(pairs: &[Pair]) -> f64 {
        pairs
            .iter()
            .flat_map(|pair| {
                let k = pair.generated.len() as f64 + 1.0;
                pair.generated.iter().enumerate().map(move |(i, g)| {
                    let t = (i as f64 + 1.0) / k;
                    (0..2)
                        .map(|a| {
                            (g[a] - (pair.previous[a] + t * (pair.current[a] - pair.previous[a])))
                                .abs()
                        })
                        .fold(0.0, f64::max)
                })
            })
            .fold(0.0, f64::max)
    }

    #[test]
    #[ignore = "requires the NVIDIA DLSS SDK at build time, NGX_RUNTIME_DIR with the SR and FG runtimes, NGX_LOG_DIR and an NVIDIA Vulkan GPU"]
    fn frame_generation_interpolates_engine_motion_without_hud_smear() -> anyhow::Result<()> {
        if let Some(reason) = engine_desktop::fg_build_unavailable_reason() {
            panic!("DLSS Frame Generation is not built: {reason}");
        }
        let options = WgpuOptions {
            backends: WgpuBackends::Vulkan,
            ..Default::default()
        };
        let (gpu, device) = pollster::block_on(engine_desktop::request_upscaler_gpu(
            GpuContext::instance_with(WgpuBackends::Vulkan),
            None,
            &options,
            &DlssConfig::default(),
            DlssRequest::ALL,
        ))?;
        if let Some(reason) = device.frame_generation_unavailable_reason() {
            panic!("DLSS Frame Generation device unavailable: {reason}");
        }
        let target = OffscreenTarget::new(&gpu, WIDTH, HEIGHT)?;
        let mut renderer =
            GpuRenderer::with_upscaler_device(gpu, target.info, RenderLimits::default(), device)?;
        let native = UpscalerSettings::default();

        // 2x at native resolution: object motion, static camera.
        let frames: Vec<_> = (0..5)
            .map(|k| (k as f32 * STEP, 0.0, dlss_fg(2), native))
            .collect();
        let pairs = run(&mut renderer, &target, &frames)?;
        let status = renderer.frame_generation_status().clone();
        eprintln!("FG 2x status: {status:?}");
        assert_eq!(
            status.state,
            FrameGenerationState::Active,
            "{:?}",
            status.fallback_reason
        );
        assert_eq!(status.multiplier, Some(2));
        assert_eq!(status.history_resets, 1, "only the first frame resets");
        assert!(
            pairs.iter().all(|p| p.generated.len() == 1),
            "one generated frame per pair"
        );
        assert_eq!(status.generated_frames, 4);
        let error = spacing_error(&pairs);
        for pair in &pairs {
            eprintln!(
                "2x object: real {:.2} -> {:.2}, generated {:.2}",
                pair.previous[0], pair.current[0], pair.generated[0][0]
            );
            assert!(
                pair.previous[0] < pair.generated[0][0] && pair.generated[0][0] < pair.current[0]
            );
        }
        eprintln!("2x object spacing error {error:.2} px");
        assert!(
            error < 2.0,
            "generated frames lie between real frames: {error}"
        );

        // 2x with camera motion (the triangle moves left on screen).
        let frames: Vec<_> = (0..4)
            .map(|k| (4.0 * STEP, k as f32 * STEP, dlss_fg(2), native))
            .collect();
        let pairs = run(&mut renderer, &target, &frames)?;
        let error = spacing_error(&pairs);
        eprintln!("2x camera pan spacing error {error:.2} px");
        assert!(pairs.iter().all(|p| p.generated.len() == 1));
        assert!(error < 2.0, "camera motion: {error}");

        // Multi-frame generation, when NGX allows it.
        let maximum = renderer.frame_generation_status().multi_frame_max.unwrap();
        if maximum >= 3 {
            let frames: Vec<_> = (0..4)
                .map(|k| (k as f32 * 2.0 * STEP, 3.0 * STEP, dlss_fg(4), native))
                .collect();
            let pairs = run(&mut renderer, &target, &frames)?;
            let status = renderer.frame_generation_status().clone();
            eprintln!("FG 4x status: {status:?}");
            assert_eq!(status.multiplier, Some(4));
            for pair in &pairs {
                let xs: Vec<_> = pair.generated.iter().map(|g| g[0]).collect();
                eprintln!(
                    "4x: real {:.2} -> {:.2}, generated {xs:.2?}",
                    pair.previous[0], pair.current[0]
                );
                assert_eq!(xs.len(), 3);
                assert!(xs.windows(2).all(|w| w[0] < w[1]), "ordered");
            }
            let error = spacing_error(&pairs);
            eprintln!("4x spacing error {error:.2} px");
            assert!(error < 3.0, "4x generated frames evenly spaced: {error}");
        } else {
            eprintln!("NGX reports MultiFrameCountMax {maximum}; 4x skipped");
        }

        // Super Resolution plus frame generation: jittered, upscaled scene.
        let sr = UpscalerSettings {
            mode: UpscalerMode::DlssSr,
            quality: UpscalerQuality::Quality,
        };
        let frames: Vec<_> = (0..6)
            .map(|k| (k as f32 * STEP, 0.0, dlss_fg(2), sr))
            .collect();
        let pairs = run(&mut renderer, &target, &frames)?;
        let upscaler = renderer.upscaler_status().clone();
        eprintln!("SR+FG upscaler status: {upscaler:?}");
        assert_eq!(upscaler.state, UpscalerState::Active);
        assert!(upscaler.render_size.unwrap()[0] < WIDTH);
        // SR history needs a few frames to converge; judge the later pairs.
        let error = spacing_error(&pairs[2..]);
        eprintln!("SR+FG spacing error {error:.2} px");
        assert!(
            error < 2.0,
            "SR+FG generated frames lie between real frames: {error}"
        );

        // A generated SR+FG frame matches a native render of the midpoint state.
        let state = |t: f32, generation| scene(t * STEP, t * 0.03, generation);
        for k in 6..9 {
            let mut frame = state(k as f32, dlss_fg(2));
            frame.camera3d.as_mut().unwrap().camera.upscaler = sr;
            renderer.render_scene(&frame, &target.view, None)?;
        }
        let generated = renderer.capture_generated()?.remove(0);
        assert!(generated.generated);
        renderer.render_scene(
            &state(7.5, FrameGenerationSettings::default()),
            &target.view,
            None,
        )?;
        let midpoint = target.read_rgba(&renderer.gpu)?;
        let difference = midpoint
            .iter()
            .zip(&generated.rgba8)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / midpoint.len() as f64;
        eprintln!("SR+FG generated vs native midpoint: mean |diff| {difference:.3}");
        assert!(difference < 1.5, "generated frame {difference}");

        // An explicit cut and a pause without game frames both reset.
        let resets = renderer.frame_generation_status().history_resets;
        renderer.reset_temporal_history();
        renderer.render_scene(&scene(0.0, 0.0, dlss_fg(2)), &target.view, None)?;
        assert_eq!(
            renderer.generated_frame_count(),
            0,
            "a cut generates nothing"
        );
        std::thread::sleep(
            engine_desktop::MAX_GENERATION_GAP + std::time::Duration::from_millis(50),
        );
        renderer.render_scene(&scene(STEP, 0.0, dlss_fg(2)), &target.view, None)?;
        assert_eq!(renderer.generated_frame_count(), 0, "resume after a pause");
        renderer.render_scene(&scene(2.0 * STEP, 0.0, dlss_fg(2)), &target.view, None)?;
        assert_eq!(renderer.generated_frame_count(), 1);
        assert_eq!(
            renderer.frame_generation_status().history_resets,
            resets + 2
        );
        // Turning generation off presents real frames only.
        renderer.render_scene(
            &scene(0.0, 0.0, FrameGenerationSettings::default()),
            &target.view,
            None,
        )?;
        assert_eq!(
            renderer.frame_generation_status().state,
            FrameGenerationState::Off
        );
        assert!(renderer.capture_generated()?.is_empty());
        Ok(())
    }
}
