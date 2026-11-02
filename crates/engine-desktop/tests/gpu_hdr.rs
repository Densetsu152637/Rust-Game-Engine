//! HDR display output through the desktop renderer, offscreen (no display or
//! HDR monitor needed): scene and UI are rendered into the display-linear
//! frame and encoded into `Rgba16Float` (scRGB) or `Rgb10a2Unorm` (PQ)
//! targets that stand in for the HDR surface. Run with
//! `cargo test -p engine-desktop --test gpu_hdr -- --ignored --test-threads=1`
//! on a real adapter. Presenting on a real HDR monitor is a separate manual
//! check (docs/rendering-hdr-output.md).

use engine_desktop::hdr::{PQ_CHOICE, SCRGB_CHOICE, output_settings};
use engine_desktop::render_pipeline::hdr::{nits_from_scrgb, pq_oetf};
use engine_desktop::render_pipeline::readback::read_rgba16f;
use engine_desktop::render_pipeline::{
    FeatureContext, FeatureDescriptor, HdrOutputSettings, PipelineFeature, PipelineFeatures, Stage,
    ViewData,
};
use engine_desktop::*;
use engine_project::{
    Camera2D,
    content::{Mesh, Vertex},
    post::*,
    rendering3d::*,
    runtime::{Draw, DrawSpace, RenderFrame},
};
use glam::{Mat4, Vec3};
use std::sync::Arc;

const SIZE: u32 = 64;

fn quad(min: f32, max: f32) -> Arc<Mesh> {
    let corners = [[min, min], [max, min], [max, max], [min, max]];
    Arc::new(Mesh {
        uv2: vec![],
        normals: vec![],
        vertices: corners
            .into_iter()
            .map(|[x, y]| Vertex {
                position: [x, y, 0.5],
                uv: [0.0; 2],
                color: [1.0; 4],
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}

/// An unlit scene of radiance `scene_value` filling the target with a white
/// UI square (`size/8 .. 5*size/8`) over it. `frame_generation` is the
/// camera's request.
fn frame_of(size: u32, scene_value: f32, generation: FrameGenerationSettings) -> SceneRenderFrame {
    let mut frame = SceneRenderFrame::from(RenderFrame {
        camera: Camera2D {
            logical_size: [size, size],
            ..Default::default()
        },
        draws: vec![Draw {
            id: "hud".into(),
            mesh: quad(size as f32 / 8.0, size as f32 * 5.0 / 8.0),
            texture: None,
            layer: 0,
            space: DrawSpace::Ui,
            clip: None,
            state: Default::default(),
            motion: None,
            offset: [0.0; 3],
        }],
    });
    frame.camera3d = Some(CameraInstance3D {
        camera: Camera3D {
            frame_generation: generation,
            ..Default::default()
        },
        world: Mat4::IDENTITY.to_cols_array_2d(),
    });
    frame.meshes.push(MeshDraw3D {
        lightmap: None,
        id: "wall".into(),
        mesh: Arc::new(Mesh {
            uv2: vec![],
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
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, -2.0)).to_cols_array_2d(),
        material: MeshMaterial {
            tint: [1.0; 4],
            unlit: true,
            ..Default::default()
        },
        shader: None,
    });
    // Material colours are limited to 0..1, so radiance above paper white
    // comes from the post stack's manual exposure: 2^-ev100 = scene_value.
    if scene_value != 1.0 {
        frame.post.camera = Some(PostProcess {
            exposure: Exposure {
                mode: ExposureMode::Manual,
                ev100: -scene_value.log2(),
                ..Default::default()
            },
            ..Default::default()
        });
    }
    frame
}

fn frame(size: u32, scene_value: f32) -> SceneRenderFrame {
    frame_of(size, scene_value, FrameGenerationSettings::default())
}

fn target(
    gpu: &GpuContext,
    format: wgpu::TextureFormat,
    size: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hdr test target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn renderer(format: wgpu::TextureFormat) -> anyhow::Result<GpuRenderer> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    eprintln!("HDR test adapter: {:?}", gpu.adapter.get_info().name);
    GpuRenderer::new(
        gpu,
        RenderTargetInfo {
            width: SIZE,
            height: SIZE,
            format,
        },
        RenderLimits::default(),
    )
}

fn enable(renderer: &mut GpuRenderer, choice: HdrChoice) -> anyhow::Result<HdrOutputSettings> {
    let request = HdrRequest::new(HdrPreference::Auto);
    let display = wgpu::DisplayHdrInfo::default();
    let settings = output_settings(choice, &request, &display);
    let status = HdrStatus::active(&request, choice, &settings, &display);
    renderer.enable_hdr_output(settings, status)?;
    Ok(settings)
}

/// Pixel `(x, y)` of a `Rgba16Float` target.
fn float_pixel(renderer: &GpuRenderer, texture: &wgpu::Texture, x: u32, y: u32) -> [f32; 3] {
    let image = read_rgba16f(&renderer.gpu, texture).unwrap();
    let p = image.pixel(x, y);
    [p[0], p[1], p[2]]
}

/// Pixel `(x, y)` of a `Rgb10a2Unorm` target as 0..1.
fn rgb10a2_pixel(renderer: &GpuRenderer, texture: &wgpu::Texture, x: u32, y: u32) -> [f32; 3] {
    let gpu = &renderer.gpu;
    let width = texture.width();
    let padded = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rgb10a2 readback"),
        size: u64::from(padded) * u64::from(texture.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(texture.height()),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    gpu.wait_idle().unwrap();
    receiver.recv().unwrap().unwrap();
    let mapped = slice.get_mapped_range().unwrap();
    let at = (y * padded + x * 4) as usize;
    let word = u32::from_le_bytes(mapped[at..at + 4].try_into().unwrap());
    [
        (word & 0x3ff) as f32 / 1023.0,
        ((word >> 10) & 0x3ff) as f32 / 1023.0,
        ((word >> 20) & 0x3ff) as f32 / 1023.0,
    ]
}

fn close(a: f32, b: f32, relative: f32) -> bool {
    (a - b).abs() <= 2e-4 + relative * b.abs()
}

/// Pixel inside the UI square and one outside it (scene only).
fn ui_at(size: u32) -> (u32, u32) {
    (size * 3 / 8, size * 3 / 8)
}
fn scene_at(size: u32) -> (u32, u32) {
    (size * 7 / 8, size * 7 / 8)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn scene_above_paper_white_and_ui_at_paper_white_reach_a_scrgb_surface() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgba16Float)?;
    let settings = enable(&mut renderer, SCRGB_CHOICE)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, SIZE);
    renderer.render_scene(&frame(SIZE, 4.0), &view, None)?;

    let status = renderer.hdr_status().clone();
    assert!(status.active && status.requested, "{status:?}");
    assert_eq!(
        renderer.target_info().format,
        wgpu::TextureFormat::Rgba16Float
    );

    // UI: a white sprite is display-linear 1.0, i.e. paper white = 203 nits =
    // 203/80 in scRGB (not 1.0 = 80 nits, and not the peak).
    let (x, y) = ui_at(SIZE);
    let ui = float_pixel(&renderer, &texture, x, y);
    for c in ui {
        assert!(close(c, 203.0 / 80.0, 3e-3), "UI pixel {ui:?}");
        assert!(close(nits_from_scrgb(c), 203.0, 3e-3), "UI pixel {ui:?}");
    }
    // The display-linear frame holds the UI at exactly 1.0 and the scene at
    // its unclamped 4.0.
    let linear = renderer.hdr_linear_texture().expect("HDR frame image");
    assert_eq!(linear.format(), wgpu::TextureFormat::Rgba16Float);
    let ui_linear = float_pixel(&renderer, linear, x, y);
    assert!(
        ui_linear.iter().all(|&c| close(c, 1.0, 2e-3)),
        "{ui_linear:?}"
    );
    let (sx, sy) = scene_at(SIZE);
    let scene_linear = float_pixel(&renderer, linear, sx, sy);
    assert!(
        scene_linear.iter().all(|&c| close(c, 4.0, 5e-3)),
        "unclamped scene value: {scene_linear:?}"
    );

    // Scene: 4.0 survives above paper white, rolled off to the peak.
    let scene = float_pixel(&renderer, &texture, sx, sy);
    let want = settings.encode([4.0; 3]);
    for c in 0..3 {
        assert!(
            close(scene[c], want[c], 5e-3),
            "scene pixel {scene:?} vs {want:?}"
        );
    }
    assert!(scene[0] > ui[0], "highlights are brighter than UI white");
    assert!(nits_from_scrgb(scene[0]) <= settings.peak_nits * 1.005);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn scene_and_ui_reach_a_pq_surface_with_the_cpu_encoding() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgb10a2Unorm)?;
    let settings = enable(&mut renderer, PQ_CHOICE)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgb10a2Unorm, SIZE);
    renderer.render_scene(&frame(SIZE, 2.0), &view, None)?;

    let (x, y) = ui_at(SIZE);
    let ui = rgb10a2_pixel(&renderer, &texture, x, y);
    let paper = pq_oetf(203.0);
    for c in ui {
        assert!(
            (c - paper).abs() <= 2.5 / 1023.0,
            "UI pixel {ui:?}, PQ(203 nits) = {paper}"
        );
    }
    let (sx, sy) = scene_at(SIZE);
    let scene = rgb10a2_pixel(&renderer, &texture, sx, sy);
    let want = settings.encode([2.0; 3]);
    for c in 0..3 {
        assert!(
            (scene[c] - want[c]).abs() <= 3.0 / 1023.0 + 2e-3,
            "scene pixel {scene:?} vs {want:?}"
        );
    }
    assert!(scene[0] > ui[0]);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn the_sdr_path_is_unchanged_when_hdr_is_off() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgba8UnormSrgb)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba8UnormSrgb, SIZE);
    assert!(!renderer.hdr_status().requested && !renderer.hdr_status().active);
    renderer.render_scene(&frame(SIZE, 4.0), &view, None)?;
    let before = engine_render_wgpu::read_texture_rgba(&renderer.gpu, &texture, SIZE, SIZE)?;
    let (x, y) = ui_at(SIZE);
    let (sx, sy) = scene_at(SIZE);
    let at = |x: u32, y: u32| ((y * SIZE + x) * 4) as usize;
    assert_eq!(&before[at(x, y)..at(x, y) + 4], &[255; 4], "UI white");
    assert_eq!(
        &before[at(sx, sy)..at(sx, sy) + 4],
        &[255; 4],
        "scene clamps"
    );

    // An sRGB surface cannot carry HDR: enabling is refused, the status and
    // the output stay what they were.
    let error = enable(&mut renderer, SCRGB_CHOICE).unwrap_err().to_string();
    assert!(error.contains("Rgba16Float"), "{error}");
    assert!(!renderer.hdr_status().active);
    assert_eq!(
        renderer.target_info().format,
        wgpu::TextureFormat::Rgba8UnormSrgb
    );
    renderer.render_scene(&frame(SIZE, 4.0), &view, None)?;
    let after = engine_render_wgpu::read_texture_rgba(&renderer.gpu, &texture, SIZE, SIZE)?;
    assert_eq!(before, after, "bit-identical SDR output");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn switching_hdr_on_and_off_restores_the_same_sdr_output() -> anyhow::Result<()> {
    // The same Rgba16Float surface in plain float mode (HDR output off).
    let mut renderer = renderer(wgpu::TextureFormat::Rgba16Float)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, SIZE);
    renderer.render_scene(&frame(SIZE, 4.0), &view, None)?;
    let (sx, sy) = scene_at(SIZE);
    let (x, y) = ui_at(SIZE);
    let off = [
        float_pixel(&renderer, &texture, x, y),
        float_pixel(&renderer, &texture, sx, sy),
    ];
    assert!(off[0].iter().all(|&c| close(c, 1.0, 1e-3)), "{off:?}");
    assert!(
        off[1].iter().all(|&c| close(c, 1.0, 1e-3)),
        "clamped: {off:?}"
    );

    enable(&mut renderer, SCRGB_CHOICE)?;
    renderer.render_scene(&frame(SIZE, 4.0), &view, None)?;
    let on = float_pixel(&renderer, &texture, sx, sy);
    assert!(on[0] > 1.5, "HDR scene exceeds 1.0 in scRGB: {on:?}");

    renderer.disable_hdr_output();
    assert!(!renderer.hdr_status().active);
    assert!(renderer.hdr_linear_texture().is_none());
    renderer.render_scene(&frame(SIZE, 4.0), &view, None)?;
    let again = [
        float_pixel(&renderer, &texture, x, y),
        float_pixel(&renderer, &texture, sx, sy),
    ];
    assert_eq!(off, again, "HDR off again is the SDR output");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn resize_keeps_the_hdr_format_and_paper_white() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgba16Float)?;
    enable(&mut renderer, SCRGB_CHOICE)?;
    let (_, big_view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, SIZE);
    renderer.render_scene(&frame(SIZE, 1.0), &big_view, None)?;
    assert_eq!(renderer.hdr_linear_texture().unwrap().width(), SIZE);

    let small = SIZE / 2;
    renderer.resize(small, small)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, small);
    renderer.render_scene(&frame(small, 1.0), &view, None)?;

    let info = renderer.target_info();
    assert_eq!(
        (info.width, info.height, info.format),
        (small, small, wgpu::TextureFormat::Rgba16Float)
    );
    assert!(renderer.hdr_status().active, "{:?}", renderer.hdr_status());
    let linear = renderer.hdr_linear_texture().unwrap();
    assert_eq!((linear.width(), linear.height()), (small, small));
    let (x, y) = ui_at(small);
    let ui = float_pixel(&renderer, &texture, x, y);
    assert!(
        ui.iter().all(|&c| close(c, 203.0 / 80.0, 3e-3)),
        "UI stays at paper white after resize: {ui:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn frame_generation_is_gated_off_while_hdr_output_is_active() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgba16Float)?;
    enable(&mut renderer, SCRGB_CHOICE)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, SIZE);
    let request = FrameGenerationSettings {
        mode: FrameGenerationMode::DlssFg,
        multiplier: 2,
    };
    renderer.render_scene(&frame_of(SIZE, 1.0, request), &view, None)?;
    let status = renderer.frame_generation_status().clone();
    assert_eq!(status.state, FrameGenerationState::Fallback, "{status:?}");
    let reason = status.fallback_reason.expect("a reason");
    assert!(reason.contains("HDR output"), "{reason}");
    // The real frame still reached the HDR surface.
    let (x, y) = ui_at(SIZE);
    let ui = float_pixel(&renderer, &texture, x, y);
    assert!(ui.iter().all(|&c| close(c, 203.0 / 80.0, 3e-3)), "{ui:?}");
    assert_eq!(renderer.generated_frame_count(), 0);

    // Back on SDR the HDR gate is gone (the device may still lack FG, but the
    // reason is no longer the HDR output).
    renderer.disable_hdr_output();
    renderer.render_scene(&frame_of(SIZE, 1.0, request), &view, None)?;
    let status = renderer.frame_generation_status().clone();
    if let Some(reason) = status.fallback_reason {
        assert!(!reason.contains("HDR output"), "{reason}");
    }
    Ok(())
}

/// A Composite-stage feature that is active but draws nothing: it only forces
/// the retained-display composite copy that used to clamp HDR values.
struct IdleComposite;

impl PipelineFeature for IdleComposite {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.idle_composite", Stage::Composite)
    }
    fn composite_active(&self, _view: &ViewData) -> bool {
        true
    }
    fn encode(&mut self, _ctx: &mut FeatureContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn every_upscaler_mode_without_dlss_keeps_values_above_paper_white() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgba16Float)?;
    let settings = enable(&mut renderer, SCRGB_CHOICE)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, SIZE);
    let (sx, sy) = scene_at(SIZE);
    let (ux, uy) = ui_at(SIZE);
    let want = settings.encode([4.0; 3]);
    let requests = [
        UpscalerSettings::default(),
        UpscalerSettings {
            mode: UpscalerMode::NativeTemporal,
            quality: UpscalerQuality::Dlaa,
        },
        UpscalerSettings {
            mode: UpscalerMode::NativeTemporal,
            quality: UpscalerQuality::Quality,
        },
        UpscalerSettings {
            mode: UpscalerMode::AutoTemporal,
            quality: UpscalerQuality::Dlaa,
        },
        // Without the DLSS feature/device this falls back to native.
        UpscalerSettings {
            mode: UpscalerMode::DlssSr,
            quality: UpscalerQuality::Quality,
        },
    ];
    for request in requests {
        let mut scene = frame(SIZE, 4.0);
        scene.camera3d.as_mut().unwrap().camera.upscaler = request;
        // Several frames: the temporal resolve needs history.
        for _ in 0..4 {
            renderer.render_scene(&scene, &view, None)?;
        }
        let pixel = float_pixel(&renderer, &texture, sx, sy);
        assert!(pixel[0] > 1.5, "{request:?}: scene pixel {pixel:?}");
        assert!(
            close(pixel[0], want[0], 0.03),
            "{request:?}: scene pixel {pixel:?} vs {want:?}"
        );
        let ui = float_pixel(&renderer, &texture, ux, uy);
        assert!(
            ui.iter().all(|&c| close(c, 203.0 / 80.0, 5e-3)),
            "{request:?}: UI {ui:?}"
        );
        assert!(renderer.hdr_status().active);
    }
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_2d_frame_with_a_composite_feature_keeps_values_above_paper_white() -> anyhow::Result<()> {
    let mut renderer = renderer(wgpu::TextureFormat::Rgba16Float)?;
    enable(&mut renderer, SCRGB_CHOICE)?;
    let mut features = PipelineFeatures::default();
    features.register(IdleComposite)?;
    renderer.set_features(features)?;
    let (texture, view) = target(&renderer.gpu, wgpu::TextureFormat::Rgba16Float, SIZE);
    // A 2D-only frame whose UI quad is brighter than paper white.
    let mut scene = frame(SIZE, 1.0);
    scene.camera3d = None;
    scene.meshes.clear();
    let mut bright = (*scene.sprites.draws[0].mesh).clone();
    for vertex in &mut bright.vertices {
        vertex.color = [4.0, 4.0, 4.0, 1.0];
    }
    scene.sprites.draws[0].mesh = Arc::new(bright);
    renderer.render_scene(&scene, &view, None)?;
    let (x, y) = ui_at(SIZE);
    let pixel = float_pixel(&renderer, &texture, x, y);
    assert!(
        pixel.iter().all(|&c| c > 1.5 * 203.0 / 80.0),
        "2D value 4.0 must not clamp to paper white: {pixel:?}"
    );
    Ok(())
}
