#![cfg(ngx_available)]
//! Real NGX DLSS Frame Generation on an NVIDIA Vulkan GPU: a bright bar moves
//! right by a constant step; the generated frame must place it between the
//! previous and current real frames.
use engine_dlss::{
    Camera, ColorFormat, Error, Feature, FrameGenerationOptions, FrameGenerator, Generated,
    GenerationFrame, GenerationInputs, Interpolation, Quality, SessionOptions,
};

const W: u32 = 256;
const H: u32 = 128;
const BAR: u32 = 16;
const START: u32 = 40;
const STEP: u32 = 24;

struct Scene {
    backbuffer: wgpu::Texture,
    depth: wgpu::Texture,
    motion: wgpu::Texture,
    output: wgpu::Texture,
}

#[test]
#[ignore = "requires official NGX SDK/runtime and an NVIDIA Vulkan GPU; set NGX_RUNTIME_DIR and NGX_LOG_DIR"]
fn generates_real_dlss_interpolated_frame() {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    let session = SessionOptions {
        project_id: "c8091997-e7fa-48f6-8ef7-bf2211936fb5".into(),
        runtime_dir: std::env::var_os("NGX_RUNTIME_DIR")
            .expect("NGX_RUNTIME_DIR")
            .into(),
        log_dir: std::env::var_os("NGX_LOG_DIR").expect("NGX_LOG_DIR").into(),
        output_size: [W, H],
        quality: Quality::Quality,
    };
    let (device, queue) = engine_dlss::create_device_for_features(
        &adapter,
        &wgpu::DeviceDescriptor::default(),
        &session,
        &[Feature::SuperResolution, Feature::FrameGeneration],
    )
    .expect("DLSS-G-compatible Vulkan device");
    let options = FrameGenerationOptions {
        output_size: [W, H],
        render_size: [W, H],
        color_format: ColorFormat::Rgba8Unorm,
        hudless: false,
    };
    let mut generator =
        FrameGenerator::new(&device, &queue, &session, options).expect("initialize real NGX FG");
    eprintln!("NGX FG initialized: {:?}", generator.status());

    let scene = Scene {
        backbuffer: texture(&device, wgpu::TextureFormat::Rgba8Unorm),
        depth: texture(&device, wgpu::TextureFormat::R32Float),
        motion: texture(&device, wgpu::TextureFormat::Rg16Float),
        output: texture(&device, wgpu::TextureFormat::Rgba8Unorm),
    };
    let inputs = GenerationInputs {
        backbuffer: &scene.backbuffer,
        hudless: None,
        depth: &scene.depth,
        motion: &scene.motion,
    };

    // Invalid inputs are rejected before native work and do not poison.
    let small = device.create_texture(&wgpu::TextureDescriptor {
        size: wgpu::Extent3d {
            width: W / 2,
            height: H,
            depth_or_array_layers: 1,
        },
        ..descriptor_for(wgpu::TextureFormat::Rgba8Unorm)
    });
    let error = generator
        .evaluate(&inputs, &frame(true, Interpolation::default()), &small)
        .expect_err("wrong output size");
    assert!(matches!(error, Error::InvalidInput(_)), "{error}");
    let error = generator
        .evaluate(
            &inputs,
            &frame(false, Interpolation { index: 2, count: 2 }),
            &scene.output,
        )
        .expect_err("index 2 without index 1");
    assert!(matches!(error, Error::InvalidInput(_)), "{error}");
    // A texture from another device is rejected before native work.
    let (other_device, _other_queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("second Vulkan device");
    let foreign = texture(&other_device, wgpu::TextureFormat::Rgba8Unorm);
    let error = generator
        .evaluate(
            &GenerationInputs {
                backbuffer: &foreign,
                ..inputs
            },
            &frame(true, Interpolation::default()),
            &scene.output,
        )
        .expect_err("input from another device");
    eprintln!("foreign-device input: {error}");
    assert!(matches!(error, Error::InvalidInput(_)), "{error}");
    assert_eq!(generator.status().evaluated_frames, 0);

    let mut centroids = Vec::new();
    for k in 0..5 {
        fill(&queue, &scene, bar_x(k));
        let result = generator
            .evaluate(
                &inputs,
                &frame(k == 0, Interpolation::default()),
                &scene.output,
            )
            .expect("actual NGX FG evaluation");
        let data = read(&device, &queue, &scene.output);
        let generated = centroid(&data);
        eprintln!("frame {k}: {result:?}, generated bar centroid {generated:?}");
        if k == 0 {
            assert_eq!(result, Generated::NoHistory);
        } else {
            assert_eq!(result, Generated::Presentable);
            // Static textured background, away from the bar's path in this pair.
            let (mean, max) = background_error(&data, bar_x(k - 1) - 8, bar_x(k) + BAR + 8);
            eprintln!("frame {k}: static background error mean {mean:.3}, max {max}");
            assert!(
                mean < 1. && max <= 8,
                "static background must survive generation (mean {mean:.3}, max {max})"
            );
            centroids.push((k, generated));
        }
    }
    assert_eq!(generator.status().evaluated_frames, 5);
    for (k, generated) in &centroids {
        let (mass, x) = generated.expect("generated frame contains the bar");
        let (previous, current) = (center(k - 1), center(*k));
        eprintln!(
            "pair {}->{k}: previous {previous}, current {current}, generated {x:.2}, mass {mass:.0}",
            k - 1
        );
        assert!(
            (mass - (BAR * H) as f64).abs() < 0.1 * (BAR * H) as f64,
            "generated frame must keep one bar's brightness, got {mass:.0}"
        );
        let midpoint = (previous + current) / 2.;
        assert!(
            x > previous + 4. && x < current - 4. && (x - midpoint).abs() < 1.,
            "generated bar centroid {x:.2} must lie at the midpoint {midpoint} of {previous} and {current}"
        );
    }

    let max = generator.status().multi_frame_max;
    if max >= 3 {
        let k = 5;
        fill(&queue, &scene, bar_x(k));
        let mut previous = center(k - 1);
        for index in 1..=3 {
            let result = generator
                .evaluate(
                    &inputs,
                    &frame(false, Interpolation { index, count: 3 }),
                    &scene.output,
                )
                .expect("multi-frame NGX FG evaluation");
            let (mass, x) =
                centroid(&read(&device, &queue, &scene.output)).expect("multi-frame bar");
            eprintln!("4x frame {index}/3: {result:?}, centroid {x:.2}, mass {mass:.0}");
            assert_eq!(result, Generated::Presentable);
            let expected = center(k - 1) + f64::from(STEP * index) / 4.;
            assert!(
                x > previous && x < center(k) && (x - expected).abs() < 1.,
                "4x frame {index} centroid {x:.2} must advance from {previous} to about {expected}"
            );
            assert!(
                (mass - (BAR * H) as f64).abs() < 0.1 * (BAR * H) as f64,
                "4x frame {index} lost the bar"
            );
            previous = x;
        }
    } else {
        eprintln!("multi-frame generation not reported (max {max}); 4x not evaluated");
    }

    // A reset has no previous frame; NGX copies the current backbuffer.
    let k = 7;
    fill(&queue, &scene, bar_x(k));
    let result = generator
        .evaluate(
            &inputs,
            &frame(true, Interpolation::default()),
            &scene.output,
        )
        .expect("reset NGX FG evaluation");
    let (_, x) = centroid(&read(&device, &queue, &scene.output)).expect("reset bar");
    eprintln!("reset frame {k}: {result:?}, centroid {x:.2}");
    assert_eq!(result, Generated::NoHistory);
    assert!(
        (x - center(k)).abs() < 1.,
        "reset output must be the current frame"
    );

    scene.output.destroy();
    let error = generator
        .evaluate(
            &inputs,
            &frame(false, Interpolation::default()),
            &scene.output,
        )
        .expect_err("destroyed output must fail safely");
    assert!(error.to_string().contains("destroyed"), "{error}");
}

fn bar_x(k: u32) -> u32 {
    START + STEP * k
}
fn center(k: u32) -> f64 {
    f64::from(bar_x(k)) + f64::from(BAR) / 2. - 0.5
}

fn frame(reset: bool, interpolation: Interpolation) -> GenerationFrame {
    // Static camera, D3D-style [0,1] depth, row-major matrices for column
    // vectors (clip = M * v); only object motion, so clip_to_prev_clip is identity.
    let (near, far, fov, aspect) = (
        0.1f32,
        100f32,
        std::f32::consts::FRAC_PI_2,
        W as f32 / H as f32,
    );
    let f = 1. / (fov / 2.).tan();
    let q = far / (far - near);
    let view_to_clip = [
        [f / aspect, 0., 0., 0.],
        [0., f, 0., 0.],
        [0., 0., q, -q * near],
        [0., 0., 1., 0.],
    ];
    let clip_to_view = [
        [aspect / f, 0., 0., 0.],
        [0., 1. / f, 0., 0.],
        [0., 0., 0., 1.],
        [0., 0., -1. / (q * near), 1. / near],
    ];
    GenerationFrame {
        camera: Camera {
            view_to_clip,
            clip_to_view,
            near,
            far,
            fov_y: fov,
            aspect,
            ..Camera::default()
        },
        motion_scale: [1., 1.],
        reset,
        camera_motion_included: true,
        interpolation,
        ..GenerationFrame::default()
    }
}

fn descriptor_for(format: wgpu::TextureFormat) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("FG test texture"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    }
}
fn texture(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&descriptor_for(format))
}

/// Static 8-pixel checkerboard of two dark grays (zero motion).
fn background(x: u32, y: u32) -> u8 {
    if (x / 8 + y / 8).is_multiple_of(2) {
        24
    } else {
        72
    }
}
/// Mean and maximum channel error of non-bar columns outside `[lo, hi)`.
fn background_error(rgba: &[u8], lo: u32, hi: u32) -> (f64, u8) {
    let (mut sum, mut count, mut max) = (0f64, 0f64, 0u8);
    for y in 0..H {
        for x in (0..W).filter(|x| !(lo..hi).contains(x)) {
            let i = ((y * W + x) * 4) as usize;
            for c in 0..3 {
                let error = rgba[i + c].abs_diff(background(x, y));
                sum += f64::from(error);
                count += 1.;
                max = max.max(error);
            }
        }
    }
    (sum / count, max)
}

/// Bar pixels are white, at depth 0.4 and moved right by STEP since the
/// previous frame (current-to-previous motion -STEP pixels); the textured
/// background is static (zero motion) at depth 0.9.
fn fill(queue: &wgpu::Queue, scene: &Scene, x0: u32) {
    let inside = |x: u32| (x0..x0 + BAR).contains(&x);
    let mut color = Vec::with_capacity((W * H * 4) as usize);
    let mut depth = Vec::with_capacity((W * H * 4) as usize);
    let mut motion = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let bar = inside(x);
            let g = background(x, y);
            color.extend_from_slice(&if bar { [255; 4] } else { [g, g, g, 255] });
            depth.extend_from_slice(&(if bar { 0.4f32 } else { 0.9 }).to_le_bytes());
            // FP16 -24.0 is 0xCE00; 0.0 is 0x0000.
            let dx: u16 = if bar { 0xce00 } else { 0 };
            motion.extend_from_slice(&dx.to_le_bytes());
            motion.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    for (texture, data) in [
        (&scene.backbuffer, color),
        (&scene.depth, depth),
        (&scene.motion, motion),
    ] {
        queue.write_texture(
            texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
            texture.size(),
        );
    }
    queue.submit([]);
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("FG readback"),
        size: u64::from(W * H * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).expect("readback receiver");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("readback completion");
    rx.recv().expect("readback callback").expect("readback map");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped range")
        .to_vec();
    buffer.unmap();
    data
}

/// Brightness above the background, as (mass in bar-pixel units, x centroid).
/// The background (at most 72) carries no weight.
fn centroid(rgba: &[u8]) -> Option<(f64, f64)> {
    let (mut mass, mut moment) = (0f64, 0f64);
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let luma = (f64::from(rgba[i]) + f64::from(rgba[i + 1]) + f64::from(rgba[i + 2])) / 3.;
            let weight = ((luma - 128.) / (255. - 128.)).max(0.);
            mass += weight;
            moment += weight * f64::from(x);
        }
    }
    (mass > 0.).then(|| (mass, moment / mass))
}
