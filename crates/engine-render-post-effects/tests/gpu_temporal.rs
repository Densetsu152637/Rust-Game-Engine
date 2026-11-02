//! Native readbacks for the temporal resolve. Run ignored tests on a real adapter:
//! `cargo test -p engine-render-post-effects --test gpu_temporal -- --ignored --test-threads=1`.
//!
//! The scenes are analytic fragment shaders that honour the host jitter: texel
//! `t` of the render-size image holds the scene at the unjittered position
//! `t + 0.5 - jitter`, exactly like a jittered rasterization, and the motion
//! producer reports the analytic current-to-previous motion of the moving object.

use anyhow::{Context, Result};
use engine_render_pipeline::jitter::{jitter_offset, jitter_phase_count};
use engine_render_pipeline::*;
use engine_render_post_effects::*;
use engine_render_wgpu::WgpuOptions;
use glam::Mat4;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

const OUTPUT: [u32; 2] = [96, 64];

/// Per-view analytic scene description shared with the producers.
#[derive(Clone, Copy, Default)]
struct Scene {
    /// 0: diagonal edge + thin stripe; 1: gradient + red object; 2: checker + grey object.
    mode: f32,
    /// Left edge of the moving 16x16 object in unjittered output pixels.
    object_x: f32,
    /// Object velocity in output pixels per frame (+x).
    velocity: f32,
    /// Object width in output pixels; 0 means 16.
    width: f32,
    /// 1: the motion producer reports no motion for the object (an object the
    /// motion prepass does not cover, such as custom vertex displacement).
    blind: f32,
}

type Scenes = Arc<Mutex<HashMap<ViewId, Scene>>>;

const SCENE_SHADER: &str = r#"
struct Params { sizes: vec4<f32>, jitter: vec4<f32>, scene: vec4<f32>, extra: vec4<f32> }
@group(0) @binding(0) var<uniform> u: Params;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
fn unjittered(p: vec2<f32>) -> vec2<f32> {
    return (p - u.jitter.xy) * (u.sizes.zw / u.sizes.xy);
}
fn in_object(q: vec2<f32>) -> bool {
    return u.scene.x > 0.5 && q.x >= u.scene.y && q.x < u.scene.y + select(16.0, u.extra.x, u.extra.x > 0.0) && q.y >= 20.0 && q.y < 36.0;
}
fn background(q: vec2<f32>) -> f32 {
    if u.scene.x < 0.5 {
        var v = 0.05;
        if q.x >= 0.63 * q.y + 17.37 { v = 1.0; }
        if q.x >= 5.0 && q.x < 6.5 && q.y >= 20.0 && q.y < 60.0 { v = 1.0; }
        return v;
    }
    if u.scene.x < 1.5 { return 0.1 + 0.4 * q.x / 96.0; }
    let c = (i32(floor(q.x / 2.0)) + i32(floor(q.y / 2.0))) & 1;
    return select(0.2, 0.8, c == 1);
}
struct Pixel { @location(0) color: vec4<f32>, @builtin(frag_depth) depth: f32 }
@fragment fn fs_scene(@builtin(position) p: vec4<f32>) -> Pixel {
    var out: Pixel;
    let q = unjittered(p.xy);
    if in_object(q) {
        out.color = select(vec4<f32>(0.9, 0.2, 0.1, 1.0), vec4<f32>(0.5, 0.5, 0.5, 1.0), u.scene.x > 1.5);
        out.depth = 0.5;
    } else {
        let v = background(q);
        out.color = vec4<f32>(v, v, v, 1.0);
        out.depth = 0.9;
    }
    return out;
}
@fragment fn fs_motion(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let q = unjittered(p.xy);
    if in_object(q) && u.extra.y < 0.5 {
        return vec4<f32>(-u.scene.z * u.sizes.x / u.sizes.z, 0.0, 0.0, 1.0);
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"#;

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    entry: &str,
    format: wgpu::TextureFormat,
    depth: bool,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("temporal test scene"),
        source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("temporal test scene"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("temporal test scene"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: depth.then_some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn bind_uniform(
    ctx: &FeatureContext<'_>,
    layout: &wgpu::BindGroupLayout,
    scene: Scene,
) -> wgpu::BindGroup {
    let view = ctx.view;
    let data = [
        view.render_size[0] as f32,
        view.render_size[1] as f32,
        view.output_size[0] as f32,
        view.output_size[1] as f32,
        view.jitter[0],
        view.jitter[1],
        0.0,
        0.0,
        scene.mode,
        scene.object_x,
        scene.velocity,
        0.0,
        scene.width,
        scene.blind,
        0.0,
        0.0,
    ];
    let buffer = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("temporal test scene"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.gpu
        .queue
        .write_buffer(&buffer, 0, bytemuck::cast_slice(&data));
    ctx.gpu
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal test scene"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        })
}

fn uniform_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("temporal test scene"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

struct SceneProducer {
    scenes: Scenes,
    layout: Option<wgpu::BindGroupLayout>,
    pipeline: Option<wgpu::RenderPipeline>,
}

impl PipelineFeature for SceneProducer {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.scene", Stage::Opaque)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let layout = uniform_layout(&setup.gpu.device);
        self.pipeline = Some(pipeline(
            &setup.gpu.device,
            &layout,
            "fs_scene",
            HDR_FORMAT,
            true,
        ));
        self.layout = Some(layout);
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let scene = self
            .scenes
            .lock()
            .unwrap()
            .get(&ctx.view.id)
            .copied()
            .unwrap_or_default();
        let bind = bind_uniform(ctx, self.layout.as_ref().context("layout")?, scene);
        let mut pass = ctx.scene_pass("temporal test scene")?;
        pass.set_pipeline(self.pipeline.as_ref().context("pipeline")?);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

struct MotionProducer {
    scenes: Scenes,
    layout: Option<wgpu::BindGroupLayout>,
    pipeline: Option<wgpu::RenderPipeline>,
}

impl PipelineFeature for MotionProducer {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.motion", Stage::Prepass)
            .after(names::MOTION)
            .needs(FeatureNeeds {
                motion: true,
                ..Default::default()
            })
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let layout = uniform_layout(&setup.gpu.device);
        self.pipeline = Some(pipeline(
            &setup.gpu.device,
            &layout,
            "fs_motion",
            MOTION_FORMAT,
            false,
        ));
        self.layout = Some(layout);
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let scene = self
            .scenes
            .lock()
            .unwrap()
            .get(&ctx.view.id)
            .copied()
            .unwrap_or_default();
        let bind = bind_uniform(ctx, self.layout.as_ref().context("layout")?, scene);
        let target = ctx.motion().context("motion target")?.view.clone();
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("temporal test motion"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(self.pipeline.as_ref().context("pipeline")?);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// Fails the frame after the resolve, like a late-stage error.
struct Saboteur(Arc<AtomicBool>);
impl PipelineFeature for Saboteur {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.saboteur", Stage::PostProcess)
    }
    fn encode(&mut self, _ctx: &mut FeatureContext<'_>) -> Result<()> {
        anyhow::ensure!(!self.0.load(Ordering::Relaxed), "injected post failure");
        Ok(())
    }
}

struct Rig {
    gpu: GpuContext,
    scene: ScenePipeline,
    controls: TemporalControls,
    scenes: Scenes,
    sabotage: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
struct Frame {
    id: ViewId,
    render: [u32; 2],
    output: [u32; 2],
    /// Jitter sequence index; `None` leaves the view unjittered.
    jitter: Option<u64>,
    temporal: bool,
    reset: bool,
    sharpness: Option<f32>,
}

impl Frame {
    fn new(id: u64, render: [u32; 2], output: [u32; 2], jitter: u64) -> Self {
        Self {
            id: ViewId(id),
            render,
            output,
            jitter: Some(jitter),
            temporal: true,
            reset: false,
            sharpness: None,
        }
    }
}

impl Rig {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {}", gpu.adapter.get_info().name);
        let mut scene = ScenePipeline::new(gpu.clone())?;
        let controls = TemporalControls::default();
        let scenes = Scenes::default();
        let sabotage = Arc::new(AtomicBool::new(false));
        scene.register(MotionProducer {
            scenes: scenes.clone(),
            layout: None,
            pipeline: None,
        })?;
        scene.register(SceneProducer {
            scenes: scenes.clone(),
            layout: None,
            pipeline: None,
        })?;
        scene.register(TemporalResolveFeature::new(controls.clone()))?;
        scene.register(Saboteur(sabotage.clone()))?;
        Ok(Self {
            gpu,
            scene,
            controls,
            scenes,
            sabotage,
        })
    }

    fn set_scene(&self, id: u64, scene: Scene) {
        self.scenes.lock().unwrap().insert(ViewId(id), scene);
    }

    fn try_frame(&mut self, frame: Frame) -> Result<HdrImage> {
        let projection = glam::camera::rh::proj::directx::perspective(
            1.0,
            frame.output[0] as f32 / frame.output[1] as f32,
            0.1,
            100.0,
        );
        let mut view = ViewData::new(
            frame.id,
            Mat4::IDENTITY,
            projection,
            frame.render,
            frame.output,
        )?;
        view.features.set(TEMPORAL_RESOLVE_FEATURE, frame.temporal);
        if frame.temporal {
            let mut settings = TemporalResolveSettings::for_sizes(frame.render, frame.output);
            if let Some(sharpness) = frame.sharpness {
                settings.filter_sharpness = sharpness;
            }
            self.controls.set(frame.id, Some(settings))?;
        } else {
            self.controls.set(frame.id, None)?;
        }
        if let Some(index) = frame.jitter {
            view.jitter = jitter_offset(index, jitter_phase_count(frame.render, frame.output));
        }
        view.reset_history = frame.reset;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test output"),
            size: wgpu::Extent3d {
                width: frame.output[0],
                height: frame.output[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let target = OutputTarget {
            view: texture.create_view(&Default::default()),
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            size: frame.output,
            viewport: Viewport::full(frame.output),
        };
        self.scene.render_view(
            &SceneFrame {
                view: &view,
                meshes: &[],
                lights: &LightList::default(),
                world: None,
            },
            &target,
            &mut NoHost,
        )?;
        self.gpu.check_errors()?;
        self.scene.read_hdr(frame.id)
    }

    fn frame(&mut self, frame: Frame) -> Result<HdrImage> {
        self.try_frame(frame)
    }
}

/// Coverage-integrated truth of scene mode 0 (16x16 subsamples per pixel).
fn truth(x: u32, y: u32) -> f32 {
    let mut sum = 0.0;
    for sy in 0..16 {
        for sx in 0..16 {
            let qx = x as f32 + (sx as f32 + 0.5) / 16.0;
            let qy = y as f32 + (sy as f32 + 0.5) / 16.0;
            let mut v = 0.05;
            if qx >= 0.63 * qy + 17.37 {
                v = 1.0;
            }
            if (5.0..6.5).contains(&qx) && (20.0..60.0).contains(&qy) {
                v = 1.0;
            }
            sum += v;
        }
    }
    sum / 256.0
}

fn error_against_truth(image: &HdrImage) -> f64 {
    error_in(image, 2..image.size[0] - 2)
}

/// Squared error against the coverage truth in the columns `xs`.
fn error_in(image: &HdrImage, xs: std::ops::Range<u32>) -> f64 {
    let mut error = 0.0;
    for y in 2..image.size[1] - 2 {
        for x in xs.clone() {
            error += f64::from(image.pixel(x, y)[0] - truth(x, y)).powi(2);
        }
    }
    error
}

fn max_difference(a: &HdrImage, b: &HdrImage) -> f32 {
    a.pixels
        .iter()
        .zip(&b.pixels)
        .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
        .fold(0.0, f32::max)
}

fn intermediate_edge_pixels(image: &HdrImage) -> usize {
    (2..image.size[1] - 2)
        .flat_map(|y| (2..image.size[0] - 2).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let v = image.pixel(x, y)[0];
            truth(x, y) > 0.2 && truth(x, y) < 0.8 && v > 0.2 && v < 0.8
        })
        .count()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn static_edge_converges_to_an_antialiased_edge_and_stays_stable() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.set_scene(1, Scene::default());
    let size = OUTPUT;
    let mut plain = Frame::new(1, size, size, 0);
    plain.jitter = None;
    plain.temporal = false;
    let aliased = rig.frame(plain)?;
    let mut previous: Option<HdrImage> = None;
    let mut last_change = f32::MAX;
    let mut converged = None;
    for index in 0..40u64 {
        let image = rig.frame(Frame::new(2, size, size, index))?;
        if index >= 24
            && let Some(before) = &previous
        {
            last_change = last_change.min(max_difference(&image, before));
        }
        previous = Some(image.clone());
        converged = Some(image);
    }
    let converged = converged.unwrap();
    let aliased_error = error_against_truth(&aliased);
    let converged_error = error_against_truth(&converged);
    eprintln!(
        "1:1 error vs coverage truth: aliased {aliased_error:.4}, converged {converged_error:.4}; \
         intermediate edge pixels {} -> {}; steady frame-to-frame change {last_change:.4}",
        intermediate_edge_pixels(&aliased),
        intermediate_edge_pixels(&converged)
    );
    assert!(
        converged_error < 0.5 * aliased_error,
        "converged TAA must be markedly closer to coverage truth: {converged_error} vs {aliased_error}"
    );
    assert!(intermediate_edge_pixels(&converged) > intermediate_edge_pixels(&aliased));
    assert!(
        converged.pixel(80, 10)[0] > 0.95,
        "flat bright region drifted"
    );
    assert!(
        (converged.pixel(10, 5)[0] - 0.05).abs() < 0.01,
        "flat dark region drifted"
    );
    // Steady state: the periodic jitter must not make the image drift.
    let mut maximum: f32 = 0.0;
    let mut reference = None;
    for index in 40..48u64 {
        let image = rig.frame(Frame::new(2, size, size, index))?;
        if let Some(before) = &reference {
            maximum = maximum.max(max_difference(&image, before));
        }
        reference = Some(image);
    }
    eprintln!("steady-state maximum frame-to-frame change: {maximum:.4}");
    assert!(maximum < 0.1, "steady-state flicker {maximum}");
    let status = rig.controls.status(ViewId(2))?;
    assert!(status.history_valid);
    assert_eq!(status.resolved_frames, 48);
    assert_eq!(status.history_discards, 0);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sharpness_sweep_measurement() -> Result<()> {
    let render = [OUTPUT[0] / 2, OUTPUT[1] / 2];
    for (render, label) in [(OUTPUT, "1:1"), (render, "0.5")] {
        let mut rig = Rig::new()?;
        rig.set_scene(1, Scene::default());
        let mut plain = Frame::new(1, render, OUTPUT, 0);
        plain.jitter = None;
        plain.temporal = false;
        let base = rig.frame(plain)?;
        eprintln!(
            "{label} baseline: stripe {:.3} edge {:.3}",
            error_in(&base, 2..12),
            error_in(&base, 12..94)
        );
        for (n, sharpness) in [2.0f32, 3.0, 5.0, 8.0].into_iter().enumerate() {
            let id = 10 + n as u64;
            rig.set_scene(id, Scene::default());
            let mut last = None;
            for index in 0..96u64 {
                let mut frame = Frame::new(id, render, OUTPUT, index);
                frame.sharpness = Some(sharpness);
                last = Some(rig.frame(frame)?);
            }
            let last = last.unwrap();
            eprintln!(
                "{label} k={sharpness}: stripe {:.3} edge {:.3}",
                error_in(&last, 2..12),
                error_in(&last, 12..94)
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn half_scale_temporal_upscale_beats_bilinear_against_coverage_truth() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.set_scene(1, Scene::default());
    let render = [OUTPUT[0] / 2, OUTPUT[1] / 2];
    let mut plain = Frame::new(1, render, OUTPUT, 0);
    plain.jitter = None;
    plain.temporal = false;
    let bilinear = rig.frame(plain)?;
    let mut converged = None;
    for index in 0..96u64 {
        converged = Some(rig.frame(Frame::new(2, render, OUTPUT, index))?);
    }
    let converged = converged.unwrap();
    let bilinear_error = error_against_truth(&bilinear);
    let temporal_error = error_against_truth(&converged);
    eprintln!(
        "0.5 scale error vs truth: bilinear {bilinear_error:.4} (stripe {:.4}, edge {:.4}), temporal {temporal_error:.4} (stripe {:.4}, edge {:.4})",
        error_in(&bilinear, 2..12),
        error_in(&bilinear, 12..94),
        error_in(&converged, 2..12),
        error_in(&converged, 12..94),
    );
    assert_eq!(converged.size, OUTPUT);
    // The 1.5 output-pixel stripe is 0.75 render pixels wide: some jitter phases
    // miss it, so a neighbourhood clip against those frames' box erases it (the
    // temporal total once measured 46.9 against bilinear 35.4 because of that).
    // The printed total and both components are asserted.
    let (stripe_temporal, stripe_bilinear) =
        (error_in(&converged, 2..12), error_in(&bilinear, 2..12));
    let (edge_temporal, edge_bilinear) =
        (error_in(&converged, 12..94), error_in(&bilinear, 12..94));
    assert!(
        edge_temporal < 0.85 * edge_bilinear,
        "temporal upscale edge error {edge_temporal} vs bilinear {edge_bilinear}"
    );
    assert!(
        stripe_temporal < 0.6 * stripe_bilinear,
        "temporal upscale stripe error {stripe_temporal} vs bilinear {stripe_bilinear}"
    );
    assert!(
        temporal_error < 0.6 * bilinear_error,
        "temporal upscale total error {temporal_error} vs bilinear {bilinear_error}"
    );
    assert!(rig.controls.status(ViewId(2))?.history_valid);
    Ok(())
}

/// A one-pixel-wide object the motion prepass does not report: its step to the
/// background is the largest depth difference in the scene, so a slope term
/// that is not gated by curvature would widen the tolerance over the whole
/// step and accept the background's history for the object. The checker
/// background (0.2 / 0.8) contains the object's grey (0.5), so colour clipping
/// cannot reject it; only the distance test can.
#[test]
#[ignore = "requires a GPU adapter"]
fn a_one_pixel_feature_does_not_inherit_the_background_history() -> Result<()> {
    let thin = |k: u32| Scene {
        mode: 2.0,
        object_x: 20.0 + 6.0 * k as f32,
        velocity: 6.0,
        width: 1.0,
        blind: 1.0,
    };
    let mut rig = Rig::new()?;
    let size = OUTPUT;
    let mut worst: f32 = 0.0;
    for k in 0..10u32 {
        rig.set_scene(1, thin(k));
        rig.set_scene(2, thin(k));
        let moved = rig.frame(Frame::new(1, size, size, u64::from(k)))?;
        let mut fresh_frame = Frame::new(2, size, size, u64::from(k));
        fresh_frame.reset = true;
        let fresh = rig.frame(fresh_frame)?;
        if k >= 4 {
            let x = thin(k).object_x as u32;
            for y in 24..32 {
                for column in x - 1..x + 2 {
                    worst =
                        worst.max((moved.pixel(column, y)[0] - fresh.pixel(column, y)[0]).abs());
                }
            }
        }
    }
    eprintln!("one-pixel feature: worst difference from a no-history resolve {worst:.4}");
    assert!(
        worst < 0.01,
        "the one-pixel feature blended in background history: {worst}"
    );
    Ok(())
}

/// A non-finite history value must not reach the output or spread.
#[test]
#[ignore = "requires a GPU adapter"]
fn non_finite_history_is_dropped_and_the_output_stays_finite() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.set_scene(1, Scene::default());
    let size = OUTPUT;
    let mut settled = None;
    for index in 0..40u64 {
        settled = Some(rig.frame(Frame::new(1, size, size, index))?);
    }
    let settled = settled.unwrap();
    // NaN, +infinity and -infinity in the committed history, on a flat region,
    // an edge pixel and next to each other so filtering would spread them.
    let poison: [([u32; 2], [u16; 4]); 5] = [
        ([70, 30], [0x7e00; 4]),
        ([71, 30], [0x7c00; 4]),
        ([40, 40], [0xfc00; 4]),
        ([41, 40], [0x7e00, 0x3c00, 0x3c00, 0x3c00]),
        ([60, 17], [0x7c00, 0x3c00, 0x0000, 0x3c00]),
    ];
    for (pixel, bits) in poison {
        rig.controls
            .poison_history_pixel(&rig.gpu.queue, ViewId(1), pixel, bits)?;
    }
    let mut image = None;
    for index in 40..44u64 {
        let frame = rig.frame(Frame::new(1, size, size, index))?;
        assert!(
            frame.pixels.iter().all(|p| p.iter().all(|c| c.is_finite())),
            "non-finite pixel in the output at frame {index}"
        );
        image = Some(frame);
    }
    let image = image.unwrap();
    let mut worst: f32 = 0.0;
    for (pixel, _) in poison {
        for dy in 0..3u32 {
            for dx in 0..3u32 {
                let (x, y) = (pixel[0] + dx - 1, pixel[1] + dy - 1);
                worst = worst.max((image.pixel(x, y)[0] - settled.pixel(x, y)[0]).abs());
            }
        }
    }
    eprintln!("poisoned history: worst difference from the settled image {worst:.4}");
    assert!(worst < 0.15, "poisoned pixels did not recover: {worst}");
    Ok(())
}

/// Trailing-edge strip of the object at frame `index` (pixels covered by the
/// object last frame and not now) in output pixels.
/// Pixels within two pixels of the edge are excluded: they legitimately mix
/// object and background coverage, which is not a ghost.
fn strip(scene: Scene) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
    let start = (scene.object_x - scene.velocity).ceil() as u32;
    (start..scene.object_x.floor() as u32 - 2, 22..34)
}

fn moving(mode: f32, k: u32) -> Scene {
    Scene {
        mode,
        object_x: 8.0 + 6.0 * k as f32,
        velocity: 6.0,
        ..Scene::default()
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn a_moving_object_leaves_no_ghost_in_the_disoccluded_strip() -> Result<()> {
    for (mode, bound) in [(1.0, 0.005), (2.0, 0.005)] {
        let mut rig = Rig::new()?;
        let size = OUTPUT;
        let frames = 11u32;
        for k in 0..frames {
            rig.set_scene(1, moving(mode, k));
            rig.set_scene(2, moving(mode, k));
            let moved = rig.frame(Frame::new(1, size, size, u64::from(k)))?;
            // The same frame resolved with no history at all.
            let mut fresh_frame = Frame::new(2, size, size, u64::from(k));
            fresh_frame.reset = true;
            let fresh = rig.frame(fresh_frame)?;
            if k >= 5 {
                let (xs, ys) = strip(moving(mode, k));
                let mut worst: f32 = 0.0;
                let mut at = (0, 0, 0);
                for y in ys.clone() {
                    for x in xs.clone() {
                        for c in 0..3 {
                            let d = (moved.pixel(x, y)[c] - fresh.pixel(x, y)[c]).abs();
                            if d > worst {
                                worst = d;
                                at = (x, y, c);
                            }
                        }
                    }
                }
                eprintln!(
                    "mode {mode} frame {k}: strip {xs:?} worst difference {worst:.4} at {at:?} (moved {:?}, fresh {:?})",
                    moved.pixel(at.0, at.1),
                    fresh.pixel(at.0, at.1)
                );
                assert!(
                    worst < bound,
                    "mode {mode} frame {k}: ghost of {worst} in the disoccluded strip {xs:?}"
                );
            }
        }
        // The object itself keeps its colour while moving (history follows motion).
        let k = frames - 1;
        rig.set_scene(1, moving(mode, k));
        let image = rig.frame(Frame::new(1, size, size, u64::from(k) + 1))?;
        let expected = if mode < 1.5 {
            [0.9, 0.2, 0.1]
        } else {
            [0.5; 3]
        };
        let probe = image.pixel((moving(mode, k + 1).object_x as u32) + 8, 28);
        for c in 0..3 {
            assert!(
                (probe[c] - expected[c]).abs() < 0.05,
                "object colour {probe:?}"
            );
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn cuts_resets_and_aborts_drop_history_to_a_fresh_resolve() -> Result<()> {
    let mut rig = Rig::new()?;
    let size = OUTPUT;
    rig.set_scene(1, Scene::default());
    rig.set_scene(9, Scene::default());
    for index in 0..12u64 {
        rig.frame(Frame::new(1, size, size, index))?;
    }
    let before = rig.controls.status(ViewId(1))?;
    assert!(before.history_valid);
    // Camera cut: reset_history on the same view equals a first frame on a new view.
    let mut cut = Frame::new(1, size, size, 12);
    cut.reset = true;
    let cut_image = rig.frame(cut)?;
    let fresh = rig.frame(Frame::new(9, size, size, 12))?;
    assert!(
        max_difference(&cut_image, &fresh) < 1e-5,
        "reset frame differs from a fresh resolve"
    );
    let after = rig.controls.status(ViewId(1))?;
    assert_eq!(after.history_discards, before.history_discards + 1);
    assert!(
        after.history_valid,
        "the cut frame itself becomes the new history"
    );

    // The next frame accumulates on the cut frame, not on the pre-cut history.
    let next = rig.frame(Frame::new(1, size, size, 13))?;
    let next_fresh = rig.frame(Frame::new(9, size, size, 13))?;
    assert!(
        max_difference(&next, &next_fresh) < 1e-5,
        "second frame after the cut must equal two-frame accumulation of a fresh view"
    );

    // Abort: a failing late stage discards the pending history; the retry resets.
    for index in 14..20u64 {
        rig.frame(Frame::new(1, size, size, index))?;
        rig.frame(Frame::new(9, size, size, index))?;
    }
    let discards = rig.controls.status(ViewId(1))?.history_discards;
    rig.sabotage.store(true, Ordering::Relaxed);
    assert!(rig.try_frame(Frame::new(1, size, size, 20)).is_err());
    assert!(
        !rig.controls.status(ViewId(1))?.history_valid,
        "aborted frame left history valid"
    );
    assert_eq!(
        rig.controls.status(ViewId(1))?.history_discards,
        discards + 1
    );
    rig.sabotage.store(false, Ordering::Relaxed);
    let retry = rig.frame(Frame::new(1, size, size, 20))?;
    let mut fresh_retry = Frame::new(9, size, size, 20);
    fresh_retry.reset = true;
    let retry_fresh = rig.frame(fresh_retry)?;
    assert!(
        max_difference(&retry, &retry_fresh) < 1e-5,
        "retry after abort must start fresh"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn resize_mode_switching_and_forget_release_and_restart_history() -> Result<()> {
    let mut rig = Rig::new()?;
    rig.set_scene(1, Scene::default());
    rig.set_scene(9, Scene::default());
    let size = OUTPUT;
    for index in 0..10u64 {
        rig.frame(Frame::new(1, size, size, index))?;
    }
    assert_eq!(rig.controls.status(ViewId(1))?.history_size, Some(size));

    // Resize: history is reallocated at the new size and the first frame is fresh.
    let small = [80, 48];
    let resized = rig.frame(Frame::new(1, small, small, 10))?;
    let status = rig.controls.status(ViewId(1))?;
    assert_eq!(status.history_size, Some(small));
    assert!(status.history_discards >= 1);
    let fresh = rig.frame(Frame::new(9, small, small, 10))?;
    assert!(
        max_difference(&resized, &fresh) < 1e-5,
        "first frame after resize is not fresh"
    );

    // Render scale change at constant output (1:1 -> 0.5) is also a break.
    let half = [small[0] / 2, small[1] / 2];
    let scaled = rig.frame(Frame::new(1, half, small, 11))?;
    let scaled_fresh = rig.frame(Frame::new(9, half, small, 11))?;
    assert!(
        max_difference(&scaled, &scaled_fresh) < 1e-5,
        "a render-scale change at constant output must resolve like a fresh frame"
    );

    // Off -> on: leaving the mode releases the history, re-enabling starts fresh.
    let mut off = Frame::new(1, small, small, 12);
    off.temporal = false;
    off.jitter = None;
    rig.frame(off)?;
    let released = rig.controls.status(ViewId(1))?;
    assert_eq!(
        released.history_size, None,
        "mode off must release the history"
    );
    assert!(!released.history_valid);
    let again = rig.frame(Frame::new(1, small, small, 13))?;
    let again_fresh = rig.frame(Frame::new(9, small, small, 13))?;
    // View 9 has accumulated frames, so compare against a reset reference instead.
    let mut reference = Frame::new(9, small, small, 13);
    reference.reset = true;
    let reference = rig.frame(reference)?;
    assert!(
        max_difference(&again, &reference) < 1e-5,
        "re-enable must start fresh"
    );
    let _ = again_fresh;

    // Forget releases everything.
    rig.scene.forget_view(ViewId(1));
    assert_eq!(rig.controls.status(ViewId(1))?, TemporalStatus::default());
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn views_keep_separate_histories() -> Result<()> {
    let size = OUTPUT;
    // Solo reference: the static view alone.
    let mut solo = Rig::new()?;
    solo.set_scene(5, Scene::default());
    let mut solo_image = None;
    for index in 0..20u64 {
        solo_image = Some(solo.frame(Frame::new(5, size, size, index))?);
    }
    // Interleaved with a moving view with a different jitter phase.
    let mut rig = Rig::new()?;
    rig.set_scene(5, Scene::default());
    let mut shared_image = None;
    for index in 0..20u64 {
        rig.set_scene(6, moving(1.0, index as u32));
        rig.frame(Frame::new(6, size, size, index * 3 + 1))?;
        shared_image = Some(rig.frame(Frame::new(5, size, size, index))?);
    }
    assert!(
        max_difference(&solo_image.unwrap(), &shared_image.unwrap()) < 1e-5,
        "another view's frames changed this view's resolve"
    );
    assert!(rig.controls.status(ViewId(5))?.history_valid);
    assert!(rig.controls.status(ViewId(6))?.history_valid);
    rig.scene.forget_view(ViewId(6));
    assert!(
        rig.controls.status(ViewId(5))?.history_valid,
        "forgetting one view dropped another"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_feature_disabled_matches_the_builtin_upscale_exactly() -> Result<()> {
    let mut a = Rig::new()?;
    a.set_scene(1, Scene::default());
    let render = [OUTPUT[0] / 2, OUTPUT[1] / 2];
    let mut frame = Frame::new(1, render, OUTPUT, 0);
    frame.temporal = false;
    frame.jitter = None;
    let first = a.frame(frame)?;
    // A pipeline that never registered the resolve renders the same bits.
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let mut scene = ScenePipeline::new(gpu.clone())?;
    let scenes = Scenes::default();
    scenes.lock().unwrap().insert(ViewId(1), Scene::default());
    scene.register(MotionProducer {
        scenes: scenes.clone(),
        layout: None,
        pipeline: None,
    })?;
    scene.register(SceneProducer {
        scenes,
        layout: None,
        pipeline: None,
    })?;
    let projection = glam::camera::rh::proj::directx::perspective(
        1.0,
        OUTPUT[0] as f32 / OUTPUT[1] as f32,
        0.1,
        100.0,
    );
    let view = ViewData::new(ViewId(1), Mat4::IDENTITY, projection, render, OUTPUT)?;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test output"),
        size: wgpu::Extent3d {
            width: OUTPUT[0],
            height: OUTPUT[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target = OutputTarget {
        view: texture.create_view(&Default::default()),
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        size: OUTPUT,
        viewport: Viewport::full(OUTPUT),
    };
    scene.render_view(
        &SceneFrame {
            view: &view,
            meshes: &[],
            lights: &LightList::default(),
            world: None,
        },
        &target,
        &mut NoHost,
    )?;
    let baseline = scene.read_hdr(ViewId(1))?;
    assert_eq!(first, baseline, "a disabled resolve changed the output");
    Ok(())
}
