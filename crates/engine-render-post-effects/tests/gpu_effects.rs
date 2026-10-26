//! Native readbacks for post effects. Run ignored tests on a real adapter.

use anyhow::{Context, Result};
use engine_render_pipeline::*;
use engine_render_post_effects::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Vec4};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

const RENDER: [u32; 2] = [65, 37];
const OUTPUT: [u32; 2] = [129, 55];

const SCENE_SHADER: &str = r#"
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
struct Pixel { @location(0) color: vec4<f32>, @builtin(frag_depth) depth: f32 }
@fragment fn fs_main(@builtin(position) p: vec4<f32>) -> Pixel {
    var out: Pixel;
    if p.x < 32.0 {
        let green = select(0.0, 1.0, p.x >= 16.0 || i32(p.x) % 2 == 0);
        out.color = vec4<f32>(0.0, green, 0.0, 1.0);
        out.depth = 0.5;
    } else {
        let white = select(0.0, 1.0, i32(p.x) % 2 == 0);
        out.color = vec4<f32>(white, 0.0, 0.0, 1.0);
        out.depth = 0.8;
    }
    return out;
}
"#;

const MOTION_SHADER: &str = r#"
@group(0) @binding(0) var<uniform> mode: vec4<f32>;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs_main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let moving = mode.x > 1.5 || (mode.x > 0.5 && p.x >= 32.0);
    return vec4<f32>(select(0.0, 4.0, moving), 0.0, 0.0, 1.0);
}
"#;

fn pipeline(
    device: &wgpu::Device,
    shader: &str,
    format: wgpu::TextureFormat,
    depth: bool,
    bind: Option<&wgpu::BindGroupLayout>,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("test source"),
        source: wgpu::ShaderSource::Wgsl(shader.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("test source"),
        bind_group_layouts: &bind.map(|b| vec![Some(b)]).unwrap_or_default(),
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("test source"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: depth.then_some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
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

#[derive(Default)]
struct SceneProducer(Option<wgpu::RenderPipeline>);

impl PipelineFeature for SceneProducer {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.scene", Stage::Opaque)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.0 = Some(pipeline(
            &setup.gpu.device,
            SCENE_SHADER,
            HDR_FORMAT,
            true,
            None,
        ));
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mut pass = ctx.scene_pass("test scene depth and colour")?;
        pass.set_pipeline(self.0.as_ref().context("scene pipeline")?);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

struct MotionProducer {
    mode: Arc<AtomicU32>,
    layout: Option<wgpu::BindGroupLayout>,
    pipeline: Option<wgpu::RenderPipeline>,
}

impl MotionProducer {
    fn new(mode: Arc<AtomicU32>) -> Self {
        Self {
            mode,
            layout: None,
            pipeline: None,
        }
    }
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
        let device = &setup.gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test motion mode"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        self.pipeline = Some(pipeline(
            device,
            MOTION_SHADER,
            MOTION_FORMAT,
            false,
            Some(&layout),
        ));
        self.layout = Some(layout);
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mode = self.mode.load(Ordering::Relaxed) as f32;
        let buffer = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test motion mode"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.gpu
            .queue
            .write_buffer(&buffer, 0, bytemuck::cast_slice(&[mode, 0.0, 0.0, 0.0]));
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test motion mode"),
                layout: self.layout.as_ref().context("motion layout")?,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
        let target = ctx.motion().context("motion target")?.view.clone();
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test motion vectors"),
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
        pass.set_pipeline(self.pipeline.as_ref().context("motion pipeline")?);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

struct Rig {
    gpu: GpuContext,
    scene: ScenePipeline,
    controls: PostEffects,
    motion_mode: Arc<AtomicU32>,
}

impl Rig {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {}", gpu.adapter.get_info().name);
        let mut scene = ScenePipeline::new(gpu.clone())?;
        let controls = PostEffects::default();
        let motion_mode = Arc::new(AtomicU32::new(0));
        scene.register(MotionProducer::new(motion_mode.clone()))?;
        scene.register(SceneProducer::default())?;
        scene.register(DepthOfFieldFeature::new(controls.clone()))?;
        scene.register(MotionBlurFeature::new(controls.clone()))?;
        Ok(Self {
            gpu,
            scene,
            controls,
            motion_mode,
        })
    }

    fn frame(
        &mut self,
        id: ViewId,
        render: [u32; 2],
        output: [u32; 2],
        projection: Mat4,
        dof: bool,
        motion: bool,
        reset: bool,
    ) -> Result<HdrImage> {
        self.frame_with_upscale(id, render, output, projection, dof, motion, reset, true)
    }

    fn frame_with_upscale(
        &mut self,
        id: ViewId,
        render: [u32; 2],
        output: [u32; 2],
        projection: Mat4,
        dof: bool,
        motion: bool,
        reset: bool,
        upscale: bool,
    ) -> Result<HdrImage> {
        let mut view = ViewData::new(id, Mat4::IDENTITY, projection, render, output)?;
        view.features.set(DOF_FEATURE, dof);
        view.features.set(MOTION_BLUR_FEATURE, motion);
        view.features.set(names::UPSCALE, upscale);
        view.reset_history = reset;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test output"),
            size: wgpu::Extent3d {
                width: output[0],
                height: output[1],
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
            size: output,
            viewport: Viewport::full(output),
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
        self.scene.read_hdr(id)
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn effects_expand_render_sized_hdr_when_upscale_is_disabled() -> Result<()> {
    let mut rig = Rig::new()?;
    let id = ViewId(30);
    let projection = perspective(RENDER[0] as f32 / RENDER[1] as f32);
    let focus = distance_for_depth(projection, 0.5);
    let far = distance_for_depth(projection, 0.8);
    rig.controls.set_dof(
        id,
        Some(DepthOfFieldSettings {
            focus_distance: focus,
            focus_range: (far - focus) * 0.25,
            max_radius_pixels: 6.0,
            edge_tolerance: (far - focus) * 0.2,
        }),
    )?;
    rig.controls
        .set_motion_blur(id, Some(MotionBlurSettings::default()))?;
    let source =
        rig.frame_with_upscale(id, RENDER, OUTPUT, projection, false, false, false, false)?;
    assert_eq!(source.size, RENDER);
    let dof = rig.frame_with_upscale(id, RENDER, OUTPUT, projection, true, false, false, false)?;
    assert_eq!(dof.size, OUTPUT);
    assert!(
        dof.pixel(120, 27)[0] > 0.05,
        "right side was cropped or black"
    );
    assert!(dof.pixel(32, 27)[1] > 0.95, "left foreground lost colour");
    // Zero vectors and a camera reset still copy from the correct render-size
    // texel into the output-size post destination.
    rig.motion_mode.store(0, Ordering::Relaxed);
    let zero = rig.frame_with_upscale(id, RENDER, OUTPUT, projection, false, true, false, false)?;
    assert_eq!(zero.size, OUTPUT);
    assert!(close(zero.pixel(120, 27), source.pixel(60, 18), 0.02));
    rig.motion_mode.store(1, Ordering::Relaxed);
    let cut = rig.frame_with_upscale(id, RENDER, OUTPUT, projection, false, true, true, false)?;
    assert!(close(cut.pixel(120, 27), source.pixel(60, 18), 0.02));
    let moving =
        rig.frame_with_upscale(id, RENDER, OUTPUT, projection, false, true, false, false)?;
    assert!(maximum_channel_change(&moving, &zero, 85, 110, 27, 0) > 0.1);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn bilinear_upscale_halo_is_not_attributed_to_dof_gather() -> Result<()> {
    let mut rig = Rig::new()?;
    let id = ViewId(31);
    let projection = perspective(RENDER[0] as f32 / RENDER[1] as f32);
    let focus = distance_for_depth(projection, 0.5);
    let far = distance_for_depth(projection, 0.8);
    rig.controls.set_dof(
        id,
        Some(DepthOfFieldSettings {
            focus_distance: focus,
            focus_range: (far - focus) * 0.25,
            max_radius_pixels: 6.0,
            edge_tolerance: (far - focus) * 0.2,
        }),
    )?;
    let baseline = rig.frame(id, RENDER, OUTPUT, projection, false, false, false)?;
    let effected = rig.frame(id, RENDER, OUTPUT, projection, true, false, false)?;
    // Output x=63 samples between the green foreground texel 31 and red
    // background texel 32 in the bilinear upscale. The centre is focused;
    // DOF leaves that pre-existing red component unchanged.
    assert!(baseline.pixel(63, 27)[0] > 0.1);
    assert!(close(effected.pixel(63, 27), baseline.pixel(63, 27), 0.02));
    assert!(effected.pixel(65, 27)[1] <= baseline.pixel(65, 27)[1] + 0.02);
    Ok(())
}

fn perspective(aspect: f32) -> Mat4 {
    glam::camera::rh::proj::directx::perspective(1.0, aspect, 0.1, 100.0)
}

fn distance_for_depth(projection: Mat4, depth: f32) -> f32 {
    let view = projection.inverse() * Vec4::new(0.0, 0.0, depth, 1.0);
    (view.z / view.w).abs()
}

fn close(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < tolerance)
}

fn maximum_channel_change(
    a: &HdrImage,
    b: &HdrImage,
    start: u32,
    end: u32,
    y: u32,
    channel: usize,
) -> f32 {
    (start..end)
        .map(|x| (a.pixel(x, y)[channel] - b.pixel(x, y)[channel]).abs())
        .fold(0.0, f32::max)
}

#[test]
#[ignore = "requires a GPU adapter"]
fn depth_focus_occlusion_projection_and_views() -> Result<()> {
    let mut rig = Rig::new()?;
    for (index, projection) in [
        perspective(RENDER[0] as f32 / RENDER[1] as f32),
        glam::camera::rh::proj::directx::orthographic(-1.0, 1.0, -1.0, 1.0, 0.1, 100.0),
    ]
    .into_iter()
    .enumerate()
    {
        let id = ViewId(index as u64 + 10);
        let baseline = rig.frame(id, RENDER, RENDER, projection, false, false, false)?;
        let focus = distance_for_depth(projection, 0.5);
        let far = distance_for_depth(projection, 0.8);
        rig.controls.set_dof(
            id,
            Some(DepthOfFieldSettings {
                focus_distance: focus,
                focus_range: (far - focus) * 0.25,
                max_radius_pixels: 6.0,
                edge_tolerance: (far - focus) * 0.2,
            }),
        )?;
        let blurred = rig.frame(id, RENDER, RENDER, projection, true, false, false)?;
        assert!(
            close(blurred.pixel(12, 18), baseline.pixel(12, 18), 0.02),
            "focused surface changed"
        );
        assert!(
            close(blurred.pixel(30, 18), baseline.pixel(30, 18), 0.02),
            "foreground edge leaked"
        );
        assert!(
            (blurred.pixel(48, 18)[0] - baseline.pixel(48, 18)[0]).abs() > 0.2,
            "far stripes did not blur"
        );
        assert!(
            blurred.pixel(34, 18)[1] < 0.02,
            "background edge received green foreground"
        );
        let disabled = rig.frame(id, RENDER, RENDER, projection, false, false, false)?;
        assert!(close(disabled.pixel(48, 18), baseline.pixel(48, 18), 0.02));
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn motion_scaling_reset_zero_resize_and_order() -> Result<()> {
    let mut rig = Rig::new()?;
    let projection = perspective(RENDER[0] as f32 / RENDER[1] as f32);
    let a = ViewId(20);
    let b = ViewId(21);
    rig.controls.set_motion_blur(
        a,
        Some(MotionBlurSettings {
            shutter_scale: 1.0,
            max_radius_pixels: 12.0,
            edge_tolerance: 10.0,
        }),
    )?;
    rig.controls.set_dof(
        a,
        Some(DepthOfFieldSettings {
            focus_distance: distance_for_depth(projection, 0.5),
            focus_range: 0.05,
            max_radius_pixels: 3.0,
            edge_tolerance: 0.01,
        }),
    )?;
    let baseline = rig.frame(a, RENDER, OUTPUT, projection, false, false, false)?;
    rig.motion_mode.store(0, Ordering::Relaxed);
    let zero = rig.frame(a, RENDER, OUTPUT, projection, false, true, false)?;
    assert!(
        close(zero.pixel(95, 27), baseline.pixel(95, 27), 0.02),
        "zero motion did not copy source"
    );
    rig.motion_mode.store(1, Ordering::Relaxed);
    let vectors = rig.frame(a, RENDER, OUTPUT, projection, false, true, false)?;
    let produced = rig.scene.read_motion(a)?;
    assert!(
        (produced.pixel(48, 18)[0] - 4.0).abs() < 0.05,
        "prepass vector was not produced"
    );
    assert!(
        maximum_channel_change(&vectors, &baseline, 85, 110, 27, 0) > 0.1,
        "render-to-output scaled motion did not blur"
    );
    assert!(
        close(vectors.pixel(20, 27), baseline.pixel(20, 27), 0.02),
        "still object changed"
    );
    let cut = rig.frame(a, RENDER, OUTPUT, projection, false, true, true)?;
    assert!(
        close(cut.pixel(95, 27), baseline.pixel(95, 27), 0.02),
        "camera reset did not copy source"
    );
    rig.motion_mode.store(2, Ordering::Relaxed);
    let camera = rig.frame(a, RENDER, OUTPUT, projection, false, true, false)?;
    assert!(
        maximum_channel_change(&camera, &baseline, 10, 30, 27, 1) > 0.1,
        "camera-wide vectors did not blur foreground stripes"
    );
    let both = rig.frame(a, RENDER, OUTPUT, projection, true, true, false)?;
    assert!(
        both.pixel(95, 27)[0].is_finite(),
        "ordered effects produced invalid HDR"
    );
    let other = rig.frame(b, RENDER, OUTPUT, projection, true, true, false)?;
    assert!(
        close(other.pixel(95, 27), baseline.pixel(95, 27), 0.02),
        "unconfigured view changed"
    );
    let resized = rig.frame(a, [67, 39], [131, 57], projection, false, true, false)?;
    assert_eq!(resized.size, [131, 57]);
    assert!(resized.pixel(100, 28)[0].is_finite());
    Ok(())
}
