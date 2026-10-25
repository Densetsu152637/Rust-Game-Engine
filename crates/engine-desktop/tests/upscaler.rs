//! Temporal upscaler stage of the desktop 3D renderer on a real GPU.
//!
//! `cargo test -p engine-desktop --test upscaler -- --ignored` checks the honest
//! native fallback on any adapter. With the `dlss` feature, the official SDK at
//! build time and `NGX_RUNTIME_DIR`/`NGX_LOG_DIR`, the DLSS test renders the
//! scene through NGX Super Resolution.

use engine_desktop::{GpuContext, GpuRenderer, OffscreenTarget, RenderLimits};
use engine_project::{
    Camera2D,
    content::{Mesh, Vertex},
    rendering3d::*,
    runtime::{Draw, DrawSpace, RenderFrame},
};
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

/// NGX allows one runtime per process; DLSS tests take turns.
#[cfg(feature = "dlss")]
static NGX: std::sync::Mutex<()> = std::sync::Mutex::new(());

const WIDTH: u32 = 640;
const HEIGHT: u32 = 384;

fn mesh(vertices: &[[f32; 3]], indices: Vec<u32>, color: [f32; 4]) -> Arc<Mesh> {
    Arc::new(Mesh {
        vertices: vertices
            .iter()
            .map(|&position| Vertex {
                position,
                uv: [0.0; 2],
                color,
            })
            .collect(),
        normals: vec![],
        indices,
    })
}

fn draw(id: &str, mesh: Arc<Mesh>, model: Mat4, tint: [f32; 4], unlit: bool) -> MeshDraw3D {
    MeshDraw3D {
        id: id.into(),
        mesh,
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: model.to_cols_array_2d(),
        material: MeshMaterial {
            tint,
            unlit,
            ..Default::default()
        },
        shader: None,
    }
}

/// A floor, a lit tilted quad, two unlit triangles and a white UI square.
fn scene(upscaler: UpscalerSettings) -> SceneRenderFrame {
    let quad = |half: f32| {
        mesh(
            &[
                [-half, -half, 0.0],
                [half, -half, 0.0],
                [half, half, 0.0],
                [-half, half, 0.0],
            ],
            vec![0, 1, 2, 0, 2, 3],
            [1.0; 4],
        )
    };
    let triangle = mesh(
        &[[-0.6, -0.5, 0.0], [0.6, -0.5, 0.0], [0.0, 0.7, 0.0]],
        vec![0, 1, 2],
        [1.0; 4],
    );
    let mut camera = Camera3D {
        upscaler,
        ..Default::default()
    };
    camera.lighting.direction = [0.3, 0.8, 1.0];
    let hud = mesh(
        &[
            [8.0, 8.0, 0.5],
            [40.0, 8.0, 0.5],
            [40.0, 40.0, 0.5],
            [8.0, 40.0, 0.5],
        ],
        vec![0, 1, 2, 0, 2, 3],
        [1.0; 4],
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
            camera,
            world: Mat4::from_translation(Vec3::new(0.0, 0.4, 4.0)).to_cols_array_2d(),
        }),
        meshes: vec![
            draw(
                "floor",
                quad(4.0),
                Mat4::from_rotation_translation(
                    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
                    Vec3::new(0.0, -1.0, 0.0),
                ),
                [0.5, 0.5, 0.55, 1.0],
                false,
            ),
            draw(
                "panel",
                quad(0.8),
                Mat4::from_rotation_translation(
                    Quat::from_rotation_y(0.6),
                    Vec3::new(-1.2, 0.0, -0.5),
                ),
                [0.9, 0.6, 0.2, 1.0],
                false,
            ),
            draw(
                "red",
                triangle.clone(),
                Mat4::from_translation(Vec3::new(0.6, 0.1, 0.0)),
                [1.0, 0.05, 0.05, 1.0],
                true,
            ),
            draw(
                "green",
                triangle,
                Mat4::from_translation(Vec3::new(1.6, -0.2, -1.5)),
                [0.1, 0.9, 0.2, 1.0],
                true,
            ),
        ],
        materials2d: Default::default(),
        streams: vec![],
    }
}

fn dlss(quality: UpscalerQuality) -> UpscalerSettings {
    UpscalerSettings {
        mode: UpscalerMode::DlssSr,
        quality,
    }
}

#[cfg(feature = "dlss")]
fn mean(pixels: &[u8]) -> [f64; 3] {
    let count = (pixels.len() / 4) as f64;
    [0, 1, 2].map(|c| {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| f64::from(p[c]))
            .sum::<f64>()
            / count
    })
}

#[cfg(feature = "dlss")]
fn luminance_stddev(pixels: &[u8]) -> f64 {
    let values: Vec<f64> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
        .collect();
    let average = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - average).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

#[cfg(feature = "dlss")]
/// Centroid of strongly red pixels (the unlit red triangle).
fn red_centroid(pixels: &[u8]) -> [f64; 2] {
    let (mut x, mut y, mut n) = (0.0, 0.0, 0.0);
    for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if p[0] > 200 && p[1] < 80 && p[2] < 80 {
            x += (i as u32 % WIDTH) as f64;
            y += (i as u32 / WIDTH) as f64;
            n += 1.0;
        }
    }
    assert!(n > 100.0, "the red triangle is visible");
    [x / n, y / n]
}

/// Mean absolute channel difference inside the red triangle's bounding box
/// (from `reference`), grown by `margin` pixels: where motion errors ghost.
#[cfg(feature = "dlss")]
fn red_region_difference(reference: &[u8], image: &[u8], margin: u32) -> f64 {
    let (mut x0, mut y0, mut x1, mut y1) = (WIDTH, HEIGHT, 0, 0);
    for (i, p) in reference.as_chunks::<4>().0.iter().enumerate() {
        if p[0] > 200 && p[1] < 80 && p[2] < 80 {
            let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    let (x0, y0) = (x0.saturating_sub(margin), y0.saturating_sub(margin));
    let (x1, y1) = ((x1 + margin).min(WIDTH - 1), (y1 + margin).min(HEIGHT - 1));
    let (mut sum, mut count) = (0.0, 0.0);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (a, b) = (pixel(reference, x, y), pixel(image, x, y));
            sum += (0..3).map(|c| f64::from(a[c].abs_diff(b[c]))).sum::<f64>();
            count += 3.0;
        }
    }
    sum / count
}

#[cfg(feature = "dlss")]
fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * WIDTH + x) * 4) as usize;
    pixels[i..i + 4].try_into().unwrap()
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn dlss_request_without_a_prepared_device_renders_natively_with_a_reason() -> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, WIDTH, HEIGHT)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    renderer.render_scene(&scene(UpscalerSettings::default()), &target.view, None)?;
    assert_eq!(renderer.upscaler_status().state, UpscalerState::Native);
    let native = target.read_rgba(&renderer.gpu)?;
    renderer.render_scene(&scene(dlss(UpscalerQuality::Quality)), &target.view, None)?;
    let fallback = target.read_rgba(&renderer.gpu)?;
    let status = renderer.upscaler_status().clone();
    eprintln!("fallback status: {status:?}");
    assert_eq!(status.state, UpscalerState::Fallback);
    assert_eq!(status.requested, dlss(UpscalerQuality::Quality));
    assert_eq!(status.evaluated_frames, 0);
    assert!(status.render_size.is_none() && status.adapter.is_none());
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
    assert!(native == fallback, "the fallback renders the native image");
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore = "requires a native DX12 adapter"]
fn dlss_on_a_non_vulkan_selection_prepares_an_ordinary_device_and_says_why() -> anyhow::Result<()> {
    use engine_desktop::{DlssConfig, WgpuBackends, WgpuOptions, request_upscaler_gpu};
    let options = WgpuOptions {
        backends: WgpuBackends::Dx12,
        ..Default::default()
    };
    let (gpu, device) = pollster::block_on(request_upscaler_gpu(
        GpuContext::instance_with(WgpuBackends::Dx12),
        None,
        &options,
        &DlssConfig::default(),
        engine_desktop::DlssRequest {
            super_resolution: true,
            frame_generation: false,
        },
    ))?;
    assert_eq!(gpu.adapter.get_info().backend, wgpu::Backend::Dx12);
    let reason = device.unavailable_reason().unwrap();
    eprintln!("non-Vulkan reason: {reason}");
    if cfg!(feature = "dlss") && engine_desktop::build_unavailable_reason().is_none() {
        assert!(reason.contains("requires the Vulkan backend"), "{reason}");
    } else {
        assert_eq!(Some(reason), engine_desktop::build_unavailable_reason());
    }
    Ok(())
}

#[cfg(feature = "dlss")]
#[test]
#[ignore = "requires the NVIDIA DLSS SDK at build time, NGX_RUNTIME_DIR/NGX_LOG_DIR and an NVIDIA Vulkan GPU"]
fn dlss_super_resolution_upscales_the_engine_scene_before_the_hud() -> anyhow::Result<()> {
    let _ngx = NGX.lock().unwrap_or_else(|e| e.into_inner());
    use engine_desktop::{DlssConfig, WgpuBackends, WgpuOptions, request_upscaler_gpu};
    if let Some(reason) = engine_desktop::build_unavailable_reason() {
        panic!("DLSS is not built: {reason}");
    }
    let options = WgpuOptions {
        backends: WgpuBackends::Vulkan,
        ..Default::default()
    };
    let (gpu, device) = pollster::block_on(request_upscaler_gpu(
        GpuContext::instance_with(WgpuBackends::Vulkan),
        None,
        &options,
        &DlssConfig::default(),
        engine_desktop::DlssRequest {
            super_resolution: true,
            frame_generation: false,
        },
    ))?;
    if let Some(reason) = device.unavailable_reason() {
        panic!("DLSS device unavailable: {reason}");
    }
    eprintln!("DLSS device: {:?}", gpu.adapter.get_info().name);
    let target = OffscreenTarget::new(&gpu, WIDTH, HEIGHT)?;
    let mut renderer =
        GpuRenderer::with_upscaler_device(gpu, target.info, RenderLimits::default(), device)?;
    let mut frame = scene(UpscalerSettings::default());
    renderer.render_scene(&frame, &target.view, None)?;
    let native = target.read_rgba(&renderer.gpu)?;

    frame.camera3d.as_mut().unwrap().camera.upscaler = dlss(UpscalerQuality::Quality);
    for _ in 0..8 {
        renderer.render_scene(&frame, &target.view, None)?;
    }
    let status = renderer.upscaler_status().clone();
    eprintln!("DLSS status: {status:?}");
    assert_eq!(
        status.state,
        UpscalerState::Active,
        "{:?}",
        status.fallback_reason
    );
    assert_eq!(status.evaluated_frames, 8);
    assert_eq!(status.history_resets, 1, "only the first frame resets");
    assert_eq!(status.output_size, Some([WIDTH, HEIGHT]));
    let [rw, rh] = status.render_size.unwrap();
    assert!(
        rw < WIDTH && rh < HEIGHT,
        "quality mode renders below output size"
    );
    let upscaled = target.read_rgba(&renderer.gpu)?;

    let (native_mean, upscaled_mean) = (mean(&native), mean(&upscaled));
    let mean_abs = native
        .iter()
        .zip(&upscaled)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / native.len() as f64;
    let (native_red, upscaled_red) = (red_centroid(&native), red_centroid(&upscaled));
    eprintln!(
        "native mean {native_mean:?} stddev {:.1}; DLSS mean {upscaled_mean:?} stddev {:.1}; \
         mean |diff| {mean_abs:.2}; red centroid native {native_red:?} DLSS {upscaled_red:?}",
        luminance_stddev(&native),
        luminance_stddev(&upscaled)
    );
    for (n, u) in native_mean.iter().zip(upscaled_mean) {
        assert!(
            (n - u).abs() < 6.0,
            "mean colour {native_mean:?} vs {upscaled_mean:?}"
        );
    }
    assert!(upscaled_mean.iter().sum::<f64>() > 30.0, "not black");
    assert!(luminance_stddev(&upscaled) > 20.0, "not uniform");
    assert!(mean_abs < 10.0, "per-pixel difference {mean_abs}");
    for axis in 0..2 {
        assert!(
            (native_red[axis] - upscaled_red[axis]).abs() < 1.5,
            "geometry registers with the native render: {native_red:?} vs {upscaled_red:?}"
        );
    }
    // The HUD is drawn after upscaling at output resolution: identical pixels.
    assert_eq!(pixel(&upscaled, 24, 24), [255, 255, 255, 255]);
    assert_eq!(pixel(&upscaled, 24, 24), pixel(&native, 24, 24));
    assert_eq!(pixel(&upscaled, 8, 8), pixel(&native, 8, 8));

    // Camera and object motion keep history; an explicit cut and a quality
    // change reset it.
    for step in 1..=6 {
        let camera = frame.camera3d.as_mut().unwrap();
        camera.world =
            Mat4::from_translation(Vec3::new(0.04 * step as f32, 0.4, 4.0)).to_cols_array_2d();
        frame.meshes[2].model =
            Mat4::from_translation(Vec3::new(0.6 + 0.08 * step as f32, 0.1, 0.0))
                .to_cols_array_2d();
        renderer.render_scene(&frame, &target.view, None)?;
    }
    let moving = renderer.upscaler_status().clone();
    assert_eq!(moving.state, UpscalerState::Active);
    assert_eq!((moving.evaluated_frames, moving.history_resets), (14, 1));
    let moved = target.read_rgba(&renderer.gpu)?;
    renderer.reset_temporal_history();
    renderer.render_scene(&frame, &target.view, None)?;
    assert_eq!(renderer.upscaler_status().history_resets, 2);
    frame.camera3d.as_mut().unwrap().camera.upscaler = dlss(UpscalerQuality::Dlaa);
    renderer.render_scene(&frame, &target.view, None)?;
    let dlaa = renderer.upscaler_status().clone();
    eprintln!("DLAA status: {dlaa:?}");
    assert_eq!(dlaa.state, UpscalerState::Active);
    assert_eq!(
        dlaa.render_size,
        Some([WIDTH, HEIGHT]),
        "DLAA renders at output size"
    );
    assert_eq!((dlaa.evaluated_frames, dlaa.history_resets), (16, 3));
    // Motion vectors: after six frames of camera and object motion, the DLSS
    // image matches a native render of the final state (no trailing ghost).
    frame.camera3d.as_mut().unwrap().camera.upscaler = UpscalerSettings::default();
    renderer.render_scene(&frame, &target.view, None)?;
    let native_moved = target.read_rgba(&renderer.gpu)?;
    let moved_abs = native_moved
        .iter()
        .zip(&moved)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / moved.len() as f64;
    let (native_red, moved_red) = (red_centroid(&native_moved), red_centroid(&moved));
    let region = red_region_difference(&native_moved, &moved, 16);
    eprintln!(
        "after motion: mean |diff| {moved_abs:.2}, near the moving triangle {region:.2}; red centroid native {native_red:?} DLSS {moved_red:?}"
    );
    assert!(
        moved_abs < 10.0,
        "per-pixel difference after motion {moved_abs}"
    );
    for axis in 0..2 {
        assert!(
            (native_red[axis] - moved_red[axis]).abs() < 1.5,
            "moving geometry registers: {native_red:?} vs {moved_red:?}"
        );
    }
    assert_eq!(renderer.upscaler_status().state, UpscalerState::Native);
    Ok(())
}

/// A one-texel checkerboard (nearest sampled) on the floor and panel.
#[cfg(feature = "dlss")]
fn checker_scene(upscaler: UpscalerSettings) -> SceneRenderFrame {
    let size = 64u32;
    let pixels = (0..size * size)
        .flat_map(|i| {
            let v = if (i % size + i / size).is_multiple_of(2) {
                230
            } else {
                20
            };
            [v, v, v, 255]
        })
        .collect();
    let checker = Arc::new(engine_project::content::Texture {
        width: size,
        height: size,
        pixels,
    });
    let mut frame = scene(upscaler);
    for draw in &mut frame.meshes[..2] {
        let mut mesh = (*draw.mesh).clone();
        for vertex in &mut mesh.vertices {
            vertex.uv = [
                vertex.position[0] * 0.25 + 0.5,
                vertex.position[1] * 0.25 + 0.5,
            ];
        }
        draw.mesh = Arc::new(mesh);
        draw.texture = Some(checker.clone());
        draw.material.unlit = true;
        draw.material.tint = [1.0; 4];
    }
    frame
}

#[cfg(feature = "dlss")]
fn pan(frame: &mut SceneRenderFrame, step: u32) {
    frame.camera3d.as_mut().unwrap().world =
        Mat4::from_translation(Vec3::new(0.015 * step as f32, 0.4, 4.0)).to_cols_array_2d();
    frame.meshes[2].model =
        Mat4::from_translation(Vec3::new(0.6 + 0.02 * step as f32, 0.1, 0.0)).to_cols_array_2d();
}

/// Temporal accumulation through the engine's motion vectors must beat
/// per-frame upscaling (history reset every frame) on a panning, moving,
/// high-frequency scene. Measured on an RTX 5080: correct vectors gave a
/// mean difference ratio near 0.5, zero vectors near 0.63 and sign-flipped
/// vectors near 0.96.
#[cfg(feature = "dlss")]
#[test]
#[ignore = "requires the NVIDIA DLSS SDK at build time, NGX_RUNTIME_DIR/NGX_LOG_DIR and an NVIDIA Vulkan GPU"]
fn dlss_motion_vectors_let_history_track_camera_and_object_motion() -> anyhow::Result<()> {
    let _ngx = NGX.lock().unwrap_or_else(|e| e.into_inner());
    use engine_desktop::{DlssConfig, WgpuBackends, WgpuOptions, request_upscaler_gpu};
    let options = WgpuOptions {
        backends: WgpuBackends::Vulkan,
        ..Default::default()
    };
    let (gpu, device) = pollster::block_on(request_upscaler_gpu(
        GpuContext::instance_with(WgpuBackends::Vulkan),
        None,
        &options,
        &DlssConfig::default(),
        engine_desktop::DlssRequest {
            super_resolution: true,
            frame_generation: false,
        },
    ))?;
    if let Some(reason) = device.unavailable_reason() {
        panic!("DLSS device unavailable: {reason}");
    }
    let target = OffscreenTarget::new(&gpu, WIDTH, HEIGHT)?;
    let mut renderer =
        GpuRenderer::with_upscaler_device(gpu, target.info, RenderLimits::default(), device)?;
    let mut frame = checker_scene(dlss(UpscalerQuality::Performance));
    let mut run =
        |renderer: &mut GpuRenderer, reset_every_frame: bool| -> anyhow::Result<Vec<u8>> {
            renderer.reset_temporal_history();
            for step in 0..24 {
                pan(&mut frame, step);
                if reset_every_frame {
                    renderer.reset_temporal_history();
                }
                renderer.render_scene(&frame, &target.view, None)?;
                assert_eq!(renderer.upscaler_status().state, UpscalerState::Active);
            }
            target.read_rgba(&renderer.gpu)
        };
    let per_frame = run(&mut renderer, true)?;
    let accumulated = run(&mut renderer, false)?;
    let mut reference = checker_scene(UpscalerSettings::default());
    pan(&mut reference, 23);
    renderer.render_scene(&reference, &target.view, None)?;
    let native = target.read_rgba(&renderer.gpu)?;
    let difference = |image: &[u8]| {
        native
            .iter()
            .zip(image)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / native.len() as f64
    };
    let (per_frame, accumulated) = (difference(&per_frame), difference(&accumulated));
    eprintln!(
        "panning checker vs native: per-frame {per_frame:.2}, accumulated {accumulated:.2}, ratio {:.2}",
        accumulated / per_frame
    );
    assert!(
        accumulated < 0.6 * per_frame,
        "history did not follow motion: {accumulated:.2} vs {per_frame:.2}"
    );
    Ok(())
}
