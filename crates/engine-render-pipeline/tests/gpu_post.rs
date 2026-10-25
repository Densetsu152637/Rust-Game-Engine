//! GPU tests of the HDR post stack (tonemap, exposure, bloom, grading,
//! vignette, grain). Run with `--ignored --test-threads=1` on a real adapter.
//! A test feature fills the HDR scene colour with chosen radiance values.

use anyhow::Result;
use engine_render_pipeline::post::lut::f32_to_f16_bits;
use engine_render_pipeline::post::tonemap::{srgb_decode, tonemap};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::Mat4;
use std::sync::{Arc, Mutex};

const SIZE: [u32; 2] = [64, 64];

type Pixels = Arc<Mutex<Vec<[f32; 4]>>>;

struct HdrSource {
    pixels: Pixels,
}

impl PipelineFeature for HdrSource {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.hdr_source", Stage::Transparent)
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let pixels = self.pixels.lock().unwrap().clone();
        let mut bytes = Vec::with_capacity(pixels.len() * 8);
        for pixel in &pixels {
            for value in pixel {
                bytes.extend_from_slice(&f32_to_f16_bits(*value).to_le_bytes());
            }
        }
        let texture = ctx.hdr()?.texture.clone();
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

struct Rig {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    source: Pixels,
    texture: wgpu::Texture,
    target: OutputTarget,
    view_id: ViewId,
}

impl Rig {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {:?}", gpu.adapter.get_info().name);
        let mut pipeline = ScenePipeline::new(gpu.clone())?;
        let source: Pixels = Arc::new(Mutex::new(vec![[0.0; 4]; (SIZE[0] * SIZE[1]) as usize]));
        pipeline.register(HdrSource {
            pixels: source.clone(),
        })?;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test target"),
            size: wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = OutputTarget {
            view: texture.create_view(&Default::default()),
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            size: SIZE,
            viewport: Viewport::full(SIZE),
        };
        Ok(Self {
            gpu,
            pipeline,
            source,
            texture,
            target,
            view_id: ViewId::MAIN,
        })
    }

    fn fill(&self, f: impl Fn(u32, u32) -> [f32; 3]) {
        let mut pixels = self.source.lock().unwrap();
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                let [r, g, b] = f(x, y);
                pixels[(y * SIZE[0] + x) as usize] = [r, g, b, 1.0];
            }
        }
    }

    fn uniform(&self, rgb: [f32; 3]) {
        self.fill(|_, _| rgb);
    }

    fn frame(&mut self, post: Option<&PostSettings>, delta_time: f32, reset: bool) -> ViewReport {
        let projection = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            1.0,
            0.1,
            100.0,
        );
        let mut view = ViewData::new(self.view_id, Mat4::IDENTITY, projection, SIZE, SIZE).unwrap();
        view.post = post.cloned();
        view.delta_time = delta_time;
        view.reset_history = reset;
        let report = self
            .pipeline
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
        report
    }

    fn hdr(&self) -> HdrImage {
        self.pipeline.read_hdr(self.view_id).unwrap()
    }

    /// Display-linear output of pixel (x, y).
    fn out(&self, x: u32, y: u32) -> [f32; 3] {
        let bytes =
            engine_render_wgpu::read_texture_rgba(&self.gpu, &self.texture, SIZE[0], SIZE[1])
                .unwrap();
        let at = ((y * SIZE[0] + x) * 4) as usize;
        [0, 1, 2].map(|c| srgb_decode(f32::from(bytes[at + c]) / 255.0))
    }

    fn out_all(&self) -> Vec<[f32; 3]> {
        let bytes =
            engine_render_wgpu::read_texture_rgba(&self.gpu, &self.texture, SIZE[0], SIZE[1])
                .unwrap();
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| [0, 1, 2].map(|c| srgb_decode(f32::from(p[c]) / 255.0)))
            .collect()
    }
}

fn settings() -> PostSettings {
    PostSettings::default()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn tonemap_operators_match_the_reference_curves() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.uniform([4.0; 3]);
    // Default pipeline (no post settings): the clamp is untouched.
    let report = rig.frame(None, 0.0, false);
    assert_eq!(
        report.features.get(names::OUTPUT).unwrap().activity,
        Activity::Ran
    );
    assert!(!report.features.is_enabled(names::TONEMAP));
    let clamp = rig.out(10, 10);
    eprintln!("default 4.0 -> {clamp:?}");
    assert!(clamp.iter().all(|c| (c - 1.0).abs() < 1e-3));
    for operator in [
        TonemapOperator::None,
        TonemapOperator::Aces,
        TonemapOperator::Agx,
        TonemapOperator::AgxPunchy,
        TonemapOperator::Neutral,
    ] {
        for input in [0.18f32, 1.0, 4.0] {
            rig.uniform([input; 3]);
            let mut s = settings();
            s.tonemap = operator;
            let report = rig.frame(Some(&s), 0.0, false);
            assert!(report.features.is_enabled(names::TONEMAP));
            assert_eq!(
                report.features.get(names::OUTPUT).unwrap().activity,
                Activity::Superseded(names::TONEMAP.to_owned())
            );
            let got = rig.out(5, 5);
            let want = tonemap(operator, [input; 3]);
            eprintln!("{operator:?}({input}) = {got:?}, reference {want:?}");
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() < 0.012,
                    "{operator:?}({input}) channel {c}: {} vs {}",
                    got[c],
                    want[c]
                );
            }
        }
    }
    // ACES at 4.0 (documented value 0.9734) and "none" still clamps.
    rig.uniform([4.0; 3]);
    let mut s = settings();
    s.tonemap = TonemapOperator::Aces;
    rig.frame(Some(&s), 0.0, false);
    let aces = rig.out(1, 1)[0];
    assert!((aces - 0.9734).abs() < 0.012, "aces(4) = {aces}");
    s.tonemap = TonemapOperator::None;
    rig.frame(Some(&s), 0.0, false);
    assert!((rig.out(1, 1)[0] - 1.0).abs() < 1e-3, "none clamps");
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn auto_exposure_converges_lower_for_bright_scenes() -> Result<()> {
    let mut rig = Rig::new()?;
    let mut s = settings();
    s.exposure.mode = ExposureMode::Auto;
    s.exposure.speed_up = 3.0;
    s.exposure.speed_down = 3.0;
    let dt = 1.0 / 60.0;
    rig.uniform([8.0; 3]);
    rig.frame(Some(&s), dt, true);
    let bright = rig.pipeline.auto_exposure(rig.view_id)?.unwrap();
    rig.uniform([0.02; 3]);
    let mut dark_rig_state = Vec::new();
    for _ in 0..240 {
        rig.frame(Some(&s), dt, false);
        dark_rig_state.push(rig.pipeline.auto_exposure(rig.view_id)?.unwrap());
    }
    let dark = *dark_rig_state.last().unwrap();
    let ideal_bright = (0.18f32 / 8.0).log2();
    let ideal_dark = (0.18f32 / 0.02).log2();
    eprintln!(
        "bright ev {:.3} (ideal {ideal_bright:.3}); dark ev {:.3} (ideal {ideal_dark:.3})",
        bright.ev, dark.ev
    );
    assert!(bright.ev < dark.ev, "bright scenes expose lower");
    assert!((bright.ev - ideal_bright).abs() < 0.3);
    assert!((dark.ev - ideal_dark).abs() < 0.3);
    // Monotonic rise from the bright state; remaining gap per documented
    // rate: e^(-speed * t) of the initial gap.
    let gap0 = dark.target_ev - bright.ev;
    let mut previous = bright.ev;
    for (i, state) in dark_rig_state.iter().enumerate() {
        assert!(state.ev >= previous - 1e-4, "monotonic at frame {i}");
        previous = state.ev;
    }
    let remaining = |frames: usize| (dark.target_ev - dark_rig_state[frames - 1].ev) / gap0;
    let (one_second, two_seconds) = (remaining(60), remaining(120));
    eprintln!(
        "remaining gap after 1 s {:.4} (model {:.4}), after 2 s {:.4} (model {:.4})",
        one_second,
        (-3.0f32).exp(),
        two_seconds,
        (-6.0f32).exp()
    );
    assert!(one_second < 0.07, "within 7% after 1 s at speed 3/s");
    assert!(two_seconds < 0.01, "within 1% after 2 s at speed 3/s");
    // The exposed scene lands at middle grey.
    let exposed = rig.hdr().pixel(8, 8)[0];
    eprintln!("exposed dark pixel {exposed:.4}");
    assert!((exposed - 0.18).abs() < 0.18 * 0.15);
    // A camera cut snaps to the target immediately.
    rig.uniform([8.0; 3]);
    rig.frame(Some(&s), dt, true);
    let cut = rig.pipeline.auto_exposure(rig.view_id)?.unwrap();
    assert!(
        (cut.ev - cut.target_ev).abs() < 1e-4,
        "reset snaps: {cut:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn manual_exposure_scales_the_scene() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.uniform([0.8; 3]);
    let mut s = settings();
    s.exposure.mode = ExposureMode::Manual;
    s.exposure.ev100 = 2.0;
    rig.frame(Some(&s), 0.0, false);
    let v = rig.hdr().pixel(3, 3)[0];
    eprintln!("0.8 at ev100 2 -> {v}");
    assert!((v - 0.2).abs() < 2e-3);
    s.exposure.compensation = 1.0;
    rig.frame(Some(&s), 0.0, false);
    assert!((rig.hdr().pixel(3, 3)[0] - 0.4).abs() < 3e-3);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn bloom_spreads_a_bright_pixel_and_leaves_dim_content() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.fill(|x, y| {
        if (x, y) == (32, 32) {
            [50.0; 3]
        } else {
            [0.0; 3]
        }
    });
    let mut s = settings();
    s.bloom.enabled = true;
    s.bloom.threshold = 1.0;
    s.bloom.intensity = 1.0;
    s.bloom.scatter = 0.7;
    rig.frame(Some(&s), 0.0, false);
    let image = rig.hdr();
    let at = |dx: i32, dy: i32| image.pixel((32 + dx) as u32, (32 + dy) as u32)[0];
    let centre = at(0, 0);
    let falloff: Vec<f32> = [1, 2, 4, 8, 16].iter().map(|d| at(*d, 0)).collect();
    eprintln!("bloom centre {centre:.3}, falloff at 1/2/4/8/16 px: {falloff:?}");
    assert!(centre > 50.0, "the source stays, plus bloom");
    assert!(falloff[0] > 0.0, "energy reaches the neighbours");
    for pair in falloff.windows(2) {
        assert!(pair[0] > pair[1], "monotonic falloff: {falloff:?}");
    }
    assert!(falloff[1] > 1e-3 && falloff[2] > 1e-4, "wide halo");
    assert!(
        (at(0, 1) - at(1, 0)).abs() < 0.05 * at(1, 0),
        "roughly isotropic"
    );
    assert!(at(-4, 0) > 0.0 && at(0, -4) > 0.0, "symmetric spread");
    // Below the threshold nothing changes.
    rig.uniform([0.5; 3]);
    rig.frame(Some(&s), 0.0, false);
    let image = rig.hdr();
    let worst = image
        .pixels
        .iter()
        .map(|p| (p[0] - 0.5).abs())
        .fold(0.0f32, f32::max);
    eprintln!("sub-threshold max deviation {worst}");
    assert!(worst < 2e-3);
    // Scatter widens the halo.
    rig.fill(|x, y| {
        if (x, y) == (32, 32) {
            [50.0; 3]
        } else {
            [0.0; 3]
        }
    });
    s.bloom.scatter = 0.2;
    rig.frame(Some(&s), 0.0, false);
    let narrow = rig.hdr().pixel(32 + 16, 32)[0];
    eprintln!("scatter 0.2 at 16 px: {narrow:e} vs 0.7: {:e}", falloff[4]);
    assert!(narrow < falloff[4]);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn grading_saturation_contrast_and_identity_lut() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.uniform([0.8, 0.4, 0.1]);
    let mut s = settings();
    s.grading.enabled = true;
    s.grading.saturation = 0.0;
    rig.frame(Some(&s), 0.0, false);
    let p = rig.hdr().pixel(4, 4);
    let luma = 0.2126 * 0.8 + 0.7152 * 0.4 + 0.0722 * 0.1;
    eprintln!("saturation 0: {p:?}, luma {luma}");
    assert!((p[0] - p[1]).abs() < 1e-3 && (p[1] - p[2]).abs() < 1e-3);
    assert!((p[0] - luma).abs() < 2e-3);

    let mut s = settings();
    s.grading.enabled = true;
    s.grading.contrast = 2.0;
    rig.uniform([0.36; 3]);
    rig.frame(Some(&s), 0.0, false);
    let bright = rig.hdr().pixel(1, 1)[0];
    rig.uniform([0.09; 3]);
    rig.frame(Some(&s), 0.0, false);
    let dark = rig.hdr().pixel(1, 1)[0];
    eprintln!("contrast 2: 0.36 -> {bright}, 0.09 -> {dark}");
    assert!((bright - 0.72).abs() < 0.72 * 0.02);
    assert!((dark - 0.045).abs() < 0.045 * 0.02);

    // White balance warms, hue shift rotates, lift raises blacks.
    let mut s = settings();
    s.grading.enabled = true;
    s.grading.temperature = 0.5;
    rig.uniform([0.5; 3]);
    rig.frame(Some(&s), 0.0, false);
    let warm = rig.hdr().pixel(1, 1);
    assert!(warm[0] > warm[1] && warm[1] > warm[2], "warm: {warm:?}");
    let mut s = settings();
    s.grading.enabled = true;
    s.grading.lift = [0.1; 3];
    rig.uniform([0.0; 3]);
    rig.frame(Some(&s), 0.0, false);
    assert!((rig.hdr().pixel(1, 1)[0] - 0.1).abs() < 2e-3);

    // An identity LUT is a no-op.
    let mut s = settings();
    s.grading.enabled = true;
    s.grading.lut = Some(Arc::new(Lut3d::identity(17)?));
    for rgb in [[0.3, 1.5, 4.0], [0.02, 0.2, 0.9]] {
        rig.uniform(rgb);
        rig.frame(Some(&s), 0.0, false);
        let p = rig.hdr().pixel(2, 2);
        eprintln!("identity LUT {rgb:?} -> {p:?}");
        for c in 0..3 {
            assert!(
                (p[c] - rgb[c]).abs() < rgb[c] * 0.02 + 1e-3,
                "channel {c}: {} vs {}",
                p[c],
                rgb[c]
            );
        }
    }
    // An inverting LUT changes the image.
    let mut invert = Lut3d::identity(9)?;
    for rgb in &mut invert.data {
        *rgb = rgb.map(|v| 1.0 - v);
    }
    s.grading.lut = Some(Arc::new(invert));
    rig.uniform([0.18; 3]);
    rig.frame(Some(&s), 0.0, false);
    let inverted = rig.hdr().pixel(2, 2)[0];
    eprintln!("inverting LUT 0.18 -> {inverted}");
    assert!(inverted > 1.0, "0.18 maps through 1 - 0.1525");
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn vignette_darkens_corners_and_grain_is_animated_with_the_mean_kept() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.uniform([0.5; 3]);
    let mut s = settings();
    s.vignette.enabled = true;
    s.vignette.intensity = 0.5;
    s.vignette.smoothness = 0.5;
    rig.frame(Some(&s), 0.0, false);
    let centre = rig.out(32, 32)[0];
    let corner = rig.out(0, 0)[0];
    let ratio = corner / centre;
    eprintln!("vignette centre {centre:.4} corner {corner:.4} ratio {ratio:.4} (expected 0.5)");
    assert!((centre - 0.5).abs() < 0.01, "centre untouched");
    assert!((ratio - 0.5).abs() < 0.04);

    let mut s = settings();
    s.grain.enabled = true;
    s.grain.intensity = 0.2;
    s.grain.response = 0.0;
    rig.frame(Some(&s), 0.0, false);
    let first = rig.out_all();
    rig.frame(Some(&s), 0.0, false);
    let second = rig.out_all();
    let mean = |v: &[[f32; 3]]| v.iter().map(|p| p[0]).sum::<f32>() / v.len() as f32;
    let (m1, m2) = (mean(&first), mean(&second));
    let changed = first
        .iter()
        .zip(&second)
        .filter(|(a, b)| (a[0] - b[0]).abs() > 1e-3)
        .count();
    let spread = first.iter().map(|p| (p[0] - 0.5).powi(2)).sum::<f32>() / first.len() as f32;
    eprintln!(
        "grain means {m1:.4}/{m2:.4}, changed pixels {changed}/{}, std {:.4} (uniform model {:.4})",
        first.len(),
        spread.sqrt(),
        0.2 / 3f32.sqrt()
    );
    assert!((m1 - 0.5).abs() < 0.01 && (m2 - 0.5).abs() < 0.01);
    assert!(changed > first.len() * 9 / 10, "grain changes every frame");
    assert!((spread.sqrt() - 0.2 / 3f32.sqrt()).abs() < 0.02);
    Ok(())
}
