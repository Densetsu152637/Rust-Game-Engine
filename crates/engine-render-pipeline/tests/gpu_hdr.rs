//! GPU tests of the HDR output path: the unclamped display-linear output of
//! the output and tonemap stages (`ViewData::linear_output`) and the
//! `HdrOutputPass` encode into scRGB (`Rgba16Float`) and PQ (`Rgb10a2Unorm`)
//! targets, compared with the CPU reference in `engine_render_pipeline::hdr`.
//! Offscreen: no display is needed. Run with `--ignored --test-threads=1` on
//! a real adapter. Presenting on a real HDR monitor is a separate manual
//! check (docs/rendering-hdr-output.md).

use anyhow::Result;
use engine_render_pipeline::hdr::{
    HdrEncoding, HdrOutputSettings, nits_from_scrgb, pq_eotf, pq_oetf,
};
use engine_render_pipeline::post::lut::f32_to_f16_bits;
use engine_render_pipeline::post::tonemap::{srgb_encode, tonemap};
use engine_render_pipeline::readback::read_rgba16f;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::Mat4;
use std::sync::{Arc, Mutex};

const SIZE: [u32; 2] = [32, 32];

type Pixels = Arc<Mutex<Vec<[f32; 4]>>>;

/// Fills the HDR scene colour with chosen radiance values.
struct HdrSource {
    pixels: Pixels,
}

impl PipelineFeature for HdrSource {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.hdr_source", Stage::Transparent)
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let pixels = self.pixels.lock().unwrap().clone();
        let texture = ctx.hdr()?.texture.clone();
        let bytes = half_bytes(&pixels);
        let buffer = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test hdr source"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        {
            let slice = buffer.slice(..);
            let mut mapped = slice
                .get_mapped_range_mut()
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            mapped.copy_from_slice(&bytes);
        }
        buffer.unmap();
        ctx.encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE[0] * 8),
                    rows_per_image: Some(SIZE[1]),
                },
            },
            texture.as_image_copy(),
            wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}

fn half_bytes(pixels: &[[f32; 4]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 8);
    for pixel in pixels {
        for value in pixel {
            bytes.extend_from_slice(&f32_to_f16_bits(*value).to_le_bytes());
        }
    }
    bytes
}

fn target_texture(gpu: &GpuContext, format: wgpu::TextureFormat) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test target"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A texture holding `colors` (half-float RGBA), one value per 4-pixel column
/// band, for feeding `HdrOutputPass` directly.
fn source_texture(gpu: &GpuContext, colors: &[[f32; 3]]) -> wgpu::Texture {
    let texture = target_texture(gpu, wgpu::TextureFormat::Rgba16Float);
    let band = SIZE[0] as usize / colors.len();
    let mut pixels = Vec::new();
    for _ in 0..SIZE[1] {
        for x in 0..SIZE[0] as usize {
            let [r, g, b] = colors[(x / band).min(colors.len() - 1)];
            pixels.push([r, g, b, 1.0]);
        }
    }
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &half_bytes(&pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE[0] * 8),
            rows_per_image: Some(SIZE[1]),
        },
        texture.size(),
    );
    texture
}

fn band_x(colors: usize, index: usize) -> u32 {
    let band = SIZE[0] as usize / colors;
    (index * band + band / 2) as u32
}

/// Read a `Rgb10a2Unorm` texture as 0..1 channels.
fn read_rgb10a2(gpu: &GpuContext, texture: &wgpu::Texture) -> Result<Vec<[f32; 3]>> {
    let (width, height) = (texture.width(), texture.height());
    let row = width * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rgb10a2 readback"),
        size: u64::from(padded) * u64::from(height),
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
                rows_per_image: Some(height),
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
    gpu.wait_idle()?;
    receiver.recv()??;
    let mapped = slice
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut out = Vec::new();
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = y * padded as usize + x * 4;
            let word = u32::from_le_bytes(mapped[at..at + 4].try_into().unwrap());
            out.push([
                (word & 0x3ff) as f32 / 1023.0,
                ((word >> 10) & 0x3ff) as f32 / 1023.0,
                ((word >> 20) & 0x3ff) as f32 / 1023.0,
            ]);
        }
    }
    drop(mapped);
    buffer.unmap();
    Ok(out)
}

struct Rig {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    source: Pixels,
    texture: wgpu::Texture,
    target: OutputTarget,
}

impl Rig {
    fn new(format: wgpu::TextureFormat) -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {:?}", gpu.adapter.get_info().name);
        let mut pipeline = ScenePipeline::new(gpu.clone())?;
        let source: Pixels = Arc::new(Mutex::new(vec![[0.0; 4]; (SIZE[0] * SIZE[1]) as usize]));
        pipeline.register(HdrSource {
            pixels: source.clone(),
        })?;
        let texture = target_texture(&gpu, format);
        let target = OutputTarget {
            view: texture.create_view(&Default::default()),
            format,
            size: SIZE,
            viewport: Viewport::full(SIZE),
        };
        Ok(Self {
            gpu,
            pipeline,
            source,
            texture,
            target,
        })
    }

    /// Scene value `colors[i]` in the i-th vertical band.
    fn bands(&self, colors: &[[f32; 3]]) {
        let band = SIZE[0] as usize / colors.len();
        let mut pixels = self.source.lock().unwrap();
        for y in 0..SIZE[1] as usize {
            for x in 0..SIZE[0] as usize {
                let [r, g, b] = colors[(x / band).min(colors.len() - 1)];
                pixels[y * SIZE[0] as usize + x] = [r, g, b, 1.0];
            }
        }
    }

    fn frame(&mut self, post: Option<&PostSettings>, linear_output: bool) {
        let projection = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            1.0,
            0.1,
            100.0,
        );
        let mut view = ViewData::new(ViewId::MAIN, Mat4::IDENTITY, projection, SIZE, SIZE).unwrap();
        view.post = post.cloned();
        view.linear_output = linear_output;
        self.pipeline
            .render_view(
                &SceneFrame {
                    view: &view,
                    meshes: &[],
                    lights: &LightList::default(),
                    world: None,
                },
                &self.target,
                &mut NoHost,
            )
            .unwrap();
        self.gpu.check_errors().unwrap();
    }
}

fn close(a: f32, b: f32, relative: f32, absolute: f32) -> bool {
    (a - b).abs() <= absolute + relative * b.abs()
}

const SCENE_VALUES: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.0],
    [0.18, 0.18, 0.18],
    [0.5, 0.25, 0.1],
    [1.0, 1.0, 1.0],
    [2.0, 1.0, 0.5],
    [4.0, 4.0, 4.0],
    [16.0, 8.0, 2.0],
    [200.0, 200.0, 200.0],
];

#[test]
#[ignore = "requires a GPU adapter"]
fn linear_output_keeps_values_above_one_through_the_output_and_tonemap_stages() -> Result<()> {
    let mut rig = Rig::new(wgpu::TextureFormat::Rgba16Float)?;
    rig.bands(&SCENE_VALUES);
    let aces = PostSettings {
        tonemap: TonemapOperator::Aces,
        ..Default::default()
    };
    // engine.output (no post settings), then engine.tonemap with an operator
    // that would compress: linear output bypasses both, values pass through.
    for post in [None, Some(&aces)] {
        rig.frame(post, true);
        let image = read_rgba16f(&rig.gpu, &rig.texture)?;
        for (index, want) in SCENE_VALUES.iter().enumerate() {
            let got = image.pixel(band_x(SCENE_VALUES.len(), index), 16);
            for c in 0..3 {
                assert!(
                    close(got[c], want[c], 2e-3, 1e-6),
                    "band {index} channel {c} (post {}): {} vs {}",
                    post.is_some(),
                    got[c],
                    want[c]
                );
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sdr_output_still_clamps_and_encodes_when_linear_output_is_off() -> Result<()> {
    let mut rig = Rig::new(wgpu::TextureFormat::Rgba8UnormSrgb)?;
    rig.bands(&SCENE_VALUES);
    let read =
        |rig: &Rig| engine_render_wgpu::read_texture_rgba(&rig.gpu, &rig.texture, SIZE[0], SIZE[1]);
    let aces = PostSettings {
        tonemap: TonemapOperator::Aces,
        ..Default::default()
    };
    for post in [None, Some(&aces)] {
        rig.frame(post, false);
        let before = read(&rig)?;
        for (index, want) in SCENE_VALUES.iter().enumerate() {
            let at = ((16 * SIZE[0] + band_x(SCENE_VALUES.len(), index)) * 4) as usize;
            for c in 0..3 {
                let linear = match post {
                    None => want[c].clamp(0.0, 1.0),
                    Some(_) => tonemap(TonemapOperator::Aces, *want)[c],
                };
                let expect = (srgb_encode(linear) * 255.0).round();
                assert!(
                    (f32::from(before[at + c]) - expect).abs() <= 2.0,
                    "band {index} channel {c} (post {}): {} vs {expect}",
                    post.is_some(),
                    before[at + c]
                );
            }
        }
        // Above-one values clamp to 255 on the SDR path.
        let at = ((16 * SIZE[0] + band_x(SCENE_VALUES.len(), 7)) * 4) as usize;
        if post.is_none() {
            assert_eq!(&before[at..at + 3], &[255, 255, 255]);
        }
        // A linear-output frame in between leaves no state behind: the next
        // SDR frame is byte-identical to the one before it.
        rig.frame(post, true);
        rig.frame(post, false);
        assert_eq!(read(&rig)?, before, "SDR output is unchanged");
    }
    Ok(())
}

fn run_pass(
    gpu: &GpuContext,
    source: &wgpu::Texture,
    target: &wgpu::Texture,
    settings: &HdrOutputSettings,
) -> Result<()> {
    let mut pass = HdrOutputPass::new(&gpu.device)?;
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    pass.encode(
        &gpu.device,
        &gpu.queue,
        &mut encoder,
        &source.create_view(&Default::default()),
        &target.create_view(&Default::default()),
        target.format(),
        settings,
    )?;
    gpu.queue.submit([encoder.finish()]);
    gpu.check_errors()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn hdr_output_pass_encodes_scrgb_like_the_cpu_reference() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let source = source_texture(&gpu, &SCENE_VALUES);
    let target = target_texture(&gpu, wgpu::TextureFormat::Rgba16Float);
    let settings = HdrOutputSettings {
        encoding: HdrEncoding::ScRgb,
        paper_white_nits: 203.0,
        peak_nits: 1000.0,
    };
    run_pass(&gpu, &source, &target, &settings)?;
    let image = read_rgba16f(&gpu, &target)?;
    for (index, input) in SCENE_VALUES.iter().enumerate() {
        let got = image.pixel(band_x(SCENE_VALUES.len(), index), 8);
        let want = settings.encode(*input);
        for c in 0..3 {
            assert!(
                close(got[c], want[c], 3e-3, 2e-4),
                "scRGB band {index} channel {c}: {} vs {}",
                got[c],
                want[c]
            );
        }
        assert_eq!(got[3], 1.0, "opaque");
    }
    // Paper white is 203 nits = 203/80 in scRGB; the brightest band is capped
    // at the 1000 nit peak.
    let white = image.pixel(band_x(SCENE_VALUES.len(), 3), 8);
    assert!(close(white[0], 203.0 / 80.0, 3e-3, 0.0), "{white:?}");
    let top = image.pixel(band_x(SCENE_VALUES.len(), 7), 8);
    assert!(nits_from_scrgb(top[0]) <= 1000.0 * 1.005, "{top:?}");
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn hdr_output_pass_encodes_pq_like_the_cpu_reference() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let source = source_texture(&gpu, &SCENE_VALUES);
    let target = target_texture(&gpu, wgpu::TextureFormat::Rgb10a2Unorm);
    let settings = HdrOutputSettings {
        encoding: HdrEncoding::Pq,
        paper_white_nits: 203.0,
        peak_nits: 1000.0,
    };
    run_pass(&gpu, &source, &target, &settings)?;
    let pixels = read_rgb10a2(&gpu, &target)?;
    for (index, input) in SCENE_VALUES.iter().enumerate() {
        let got = pixels[(8 * SIZE[0] + band_x(SCENE_VALUES.len(), index)) as usize];
        let want = settings.encode(*input);
        for c in 0..3 {
            // One 10-bit step plus the shader's pow precision.
            assert!(
                (got[c] - want[c]).abs() <= 2.0 / 1023.0 + 2e-3,
                "PQ band {index} channel {c}: {} vs {}",
                got[c],
                want[c]
            );
        }
    }
    // Paper white decodes back to 203 nits; known code value 0.5807.
    let white = pixels[(8 * SIZE[0] + band_x(SCENE_VALUES.len(), 3)) as usize];
    assert!(
        (white[0] - pq_oetf(203.0)).abs() <= 2.0 / 1023.0,
        "{white:?}"
    );
    assert!(
        (pq_eotf(white[0]) - 203.0).abs() < 203.0 * 0.01,
        "{white:?} decodes to {} nits",
        pq_eotf(white[0])
    );
    let top = pixels[(8 * SIZE[0] + band_x(SCENE_VALUES.len(), 7)) as usize];
    assert!(
        pq_eotf(top[0]) <= 1000.0 * 1.02,
        "capped at the peak: {top:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn paper_white_scales_the_frame_and_the_peak_caps_it() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let source = source_texture(&gpu, &[[1.0; 3], [50.0; 3]]);
    let target = target_texture(&gpu, wgpu::TextureFormat::Rgba16Float);
    for (paper, peak) in [(100.0, 400.0), (203.0, 1000.0), (300.0, 600.0)] {
        let settings = HdrOutputSettings {
            encoding: HdrEncoding::ScRgb,
            paper_white_nits: paper,
            peak_nits: peak,
        };
        run_pass(&gpu, &source, &target, &settings)?;
        let image = read_rgba16f(&gpu, &target)?;
        let white = nits_from_scrgb(image.pixel(band_x(2, 0), 4)[0]);
        let bright = nits_from_scrgb(image.pixel(band_x(2, 1), 4)[0]);
        assert!(close(white, paper, 3e-3, 0.0), "paper {paper}: {white}");
        assert!(
            bright <= peak * 1.005 && bright >= peak * 0.97,
            "peak {peak}: {bright}"
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn scene_through_linear_output_and_the_pass_matches_the_cpu_encode() -> Result<()> {
    // The renderer's HDR chain: scene -> display-linear Rgba16Float (output
    // stage, unclamped) -> HdrOutputPass -> scRGB surface.
    let mut rig = Rig::new(wgpu::TextureFormat::Rgba16Float)?;
    rig.bands(&SCENE_VALUES);
    rig.frame(None, true);
    let surface = target_texture(&rig.gpu, wgpu::TextureFormat::Rgba16Float);
    let settings = HdrOutputSettings::default();
    run_pass(&rig.gpu, &rig.texture, &surface, &settings)?;
    let image = read_rgba16f(&rig.gpu, &surface)?;
    for (index, input) in SCENE_VALUES.iter().enumerate() {
        let got = image.pixel(band_x(SCENE_VALUES.len(), index), 16);
        let want = settings.encode(*input);
        for c in 0..3 {
            assert!(
                close(got[c], want[c], 4e-3, 3e-4),
                "band {index} channel {c}: {} vs {}",
                got[c],
                want[c]
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_pass_rejects_a_surface_format_that_cannot_carry_the_encoding() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let source = source_texture(&gpu, &[[1.0; 3]]);
    let mut pass = HdrOutputPass::new(&gpu.device)?;
    for (encoding, format) in [
        (HdrEncoding::ScRgb, wgpu::TextureFormat::Rgb10a2Unorm),
        (HdrEncoding::Pq, wgpu::TextureFormat::Rgba16Float),
        (HdrEncoding::ScRgb, wgpu::TextureFormat::Rgba8UnormSrgb),
    ] {
        let target = target_texture(&gpu, format);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let result = pass.encode(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &source.create_view(&Default::default()),
            &target.create_view(&Default::default()),
            format,
            &HdrOutputSettings {
                encoding,
                ..Default::default()
            },
        );
        assert!(result.is_err(), "{encoding:?} into {format:?}");
    }
    Ok(())
}
