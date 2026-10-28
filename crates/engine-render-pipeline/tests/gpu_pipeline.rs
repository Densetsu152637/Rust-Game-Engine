//! GPU tests of the render extension API, written as an external crate
//! would use it. Run with `--ignored --test-threads=1` on a real adapter.

use anyhow::Result;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Vec3};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SIZE: [u32; 2] = [64, 64];

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("adapter: {:?}", gpu.adapter.get_info().name);
    Ok(gpu)
}

fn perspective() -> Mat4 {
    glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_3, 1.0, 0.1, 100.0)
}

fn view(id: ViewId, world_from_view: Mat4) -> ViewData {
    ViewData::new(id, world_from_view, perspective(), SIZE, SIZE).unwrap()
}

/// An axis-aligned quad in the XY plane.
fn quad(gpu: &GpuContext, min: [f32; 2], max: [f32; 2]) -> Arc<GpuMesh> {
    let vertex = |x, y, u, v| EngineVertex {
        position: [x, y, 0.0],
        uv: [u, v],
        color: [1.0; 4],
        normal: [0.0, 0.0, 1.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "quad",
        &[
            vertex(min[0], min[1], 0.0, 1.0),
            vertex(max[0], min[1], 1.0, 1.0),
            vertex(max[0], max[1], 1.0, 0.0),
            vertex(min[0], max[1], 0.0, 0.0),
        ],
        &[0, 1, 2, 0, 2, 3],
    )
    .unwrap()
}

fn unlit(id: &str, mesh: Arc<GpuMesh>, z: f32, tint: [f32; 4]) -> SceneMesh<'_> {
    SceneMesh {
        id,
        mesh,
        world_from_local: Mat4::from_translation(Vec3::new(0.0, 0.0, z)),
        shader: None,
        params: MaterialParams {
            tint,
            unlit: true,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
    }
}

struct Offscreen {
    texture: wgpu::Texture,
    target: OutputTarget,
}

fn offscreen(gpu: &GpuContext) -> Offscreen {
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
    Offscreen { texture, target }
}

fn read(gpu: &GpuContext, target: &Offscreen) -> Vec<u8> {
    engine_render_wgpu::read_texture_rgba(gpu, &target.texture, SIZE[0], SIZE[1]).unwrap()
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE[0] + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

/// Pixel column of a world point on the test camera (identity transform).
fn column(world_x: f32, z: f32) -> u32 {
    let clip = perspective() * Vec3::new(world_x, 0.0, z).extend(1.0);
    (((clip.x / clip.w) + 1.0) * 0.5 * SIZE[0] as f32) as u32
}

// ------------------------------------------------------------- features

/// An opaque-stage feature from "another crate": N small quads drawn with
/// one instanced call, depth-tested against the scene.
struct InstancedQuads {
    offsets: Vec<[f32; 4]>,
    pipeline: Option<wgpu::RenderPipeline>,
    mesh: Option<Arc<GpuMesh>>,
    batcher: InstanceBatcher<[f32; 4]>,
    last: DrawStats,
}

const QUADS_WGSL: &str = "
#import engine::view
@vertex fn vs(@location(0) position: vec3<f32>, @location(4) offset: vec4<f32>) -> @builtin(position) vec4<f32> {
    return engine_view.clip_from_world * vec4<f32>(position + offset.xyz, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
";

impl PipelineFeature for InstancedQuads {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.quads", Stage::Opaque).after(names::OPAQUE)
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let source = setup.composer.compose(QUADS_WGSL)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test quads"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("test quads"),
            bind_group_layouts: &[Some(&setup.layouts.view)],
            immediate_size: 0,
        });
        let instance_attributes = wgpu::vertex_attr_array![4 => Float32x4];
        self.pipeline = Some(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("test quads"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[
                        Some(ENGINE_VERTEX_LAYOUT),
                        Some(wgpu::VertexBufferLayout {
                            array_stride: 16,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &instance_attributes,
                        }),
                    ],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(HDR_FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            }),
        );
        self.mesh = Some(quad(setup.gpu, [-0.1, -0.1], [0.1, 0.1]));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mesh = self.mesh.clone().unwrap();
        for offset in &self.offsets {
            self.batcher.push(&mesh, 1, *offset);
        }
        self.batcher
            .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
        let mut pass = ctx.scene_pass("test quads")?;
        let pipeline = self.pipeline.as_ref().unwrap();
        let view = ctx.view_bind_group;
        let stats = self.batcher.draw(&mut pass, Some(1), |pass, _| {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, view, &[]);
        });
        drop(pass);
        self.last = stats;
        ctx.record_draws(stats);
        Ok(())
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn an_external_opaque_feature_instance_draws_quads_depth_tested_against_scene_geometry()
-> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let xs: Vec<f32> = (0..8).map(|i| -1.4 + 0.4 * i as f32).collect();
    pipeline.register(InstancedQuads {
        offsets: xs.iter().map(|&x| [x, 0.0, -3.0, 0.0]).collect(),
        pipeline: None,
        mesh: None,
        batcher: InstanceBatcher::new("test quads"),
        last: DrawStats::default(),
    })?;
    let order = pipeline.registry().names();
    let quads = order.iter().position(|n| *n == "test.quads").unwrap();
    assert_eq!(
        order[quads - 1],
        names::OPAQUE,
        "after the built-in opaque pass"
    );
    // A red occluder in front of the left half of the row.
    let occluder = quad(&gpu, [-2.0, -0.5], [0.0, 0.5]);
    let meshes = [unlit("occluder", occluder, -2.0, [1.0, 0.0, 0.0, 1.0])];
    let target = offscreen(&gpu);
    let view = view(ViewId::MAIN, Mat4::IDENTITY);
    let report = pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &meshes,
            lights: &LightList::default(),
            world: None,
        },
        &target.target,
        &mut NoHost,
    )?;
    gpu.check_errors()?;
    let pixels = read(&gpu, &target);
    let center = SIZE[1] / 2;
    let mut green = 0;
    let mut red = 0;
    for &x in &xs {
        let p = pixel(&pixels, column(x, -3.0), center);
        if x < -0.05 {
            assert_eq!(p, [255, 0, 0, 255], "quad at x={x} is behind the occluder");
            red += 1;
        } else if x > 0.05 {
            assert_eq!(p, [0, 255, 0, 255], "quad at x={x} is visible");
            green += 1;
        }
    }
    assert!(green >= 3 && red >= 3, "green {green}, red {red}");
    // Background stays black, alpha 1.
    assert_eq!(pixel(&pixels, 60, 4), [0, 0, 0, 255]);
    // One instanced call drew all eight quads; the scene mesh is one more.
    assert_eq!(report.draws, 1);
    assert_eq!(report.draw_calls, 2);
    assert_eq!(report.instances, 9);
    assert_eq!(
        report.features.get("test.quads").unwrap().activity,
        Activity::Ran
    );
    Ok(())
}

/// A post-process feature reading HDR and writing `scale * x + bias`.
struct Affine {
    name: &'static str,
    stage: Stage,
    after: Option<&'static str>,
    scale: f32,
    bias: f32,
    pipeline: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Buffer)>,
}

const AFFINE_WGSL: &str = "
#import engine::fullscreen
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> affine: vec4<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> { return engine_fullscreen_position(i); }
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(source, vec2<i32>(p.xy), 0);
    return vec4<f32>(c.rgb * affine.x + vec3<f32>(affine.y), c.a);
}
";

impl PipelineFeature for Affine {
    fn descriptor(&self) -> FeatureDescriptor {
        let mut d = FeatureDescriptor::new(self.name, self.stage);
        if let Some(after) = self.after {
            d = d.after(after);
        }
        d
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(self.name),
            source: wgpu::ShaderSource::Wgsl(setup.composer.compose(AFFINE_WGSL)?.into()),
        });
        let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(self.name),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(self.name),
            bind_group_layouts: &[Some(&group)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(self.name),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.name),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        setup.gpu.queue.write_buffer(
            &uniform,
            0,
            bytemuck::cast_slice(&[self.scale, self.bias, 0.0, 0.0]),
        );
        self.pipeline = Some((pipeline, group, uniform));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let (pipeline, group, uniform) = self.pipeline.as_ref().unwrap();
        let (source, destination) = if self.stage == Stage::OpaqueEffects {
            ctx.opaque_effect_io()?
        } else {
            ctx.post_process_io()?
        };
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(self.name),
                layout: group,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(self.name),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

fn affine(name: &'static str, after: Option<&'static str>, scale: f32, bias: f32) -> Affine {
    Affine {
        name,
        stage: Stage::PostProcess,
        after,
        scale,
        bias,
        pipeline: None,
    }
}

fn opaque_affine(name: &'static str, after: Option<&'static str>, scale: f32, bias: f32) -> Affine {
    Affine {
        name,
        stage: Stage::OpaqueEffects,
        after,
        scale,
        bias,
        pipeline: None,
    }
}

struct ReturnSceneSource;
impl HostStages for ReturnSceneSource {
    fn upscale(&mut self, _gpu: &GpuContext, scene: &SceneTextures<'_>) -> Result<HostUpscale> {
        Ok(HostUpscale::Upscaled(scene.hdr.view.clone()))
    }
}

struct AbortSource;
impl HostStages for AbortSource {
    fn upscale(&mut self, _gpu: &GpuContext, _scene: &SceneTextures<'_>) -> Result<HostUpscale> {
        Ok(HostUpscale::Abort)
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn aborted_scene_resets_camera_and_model_history_together() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let target = offscreen(&gpu);
    let mesh = quad(&gpu, [-0.3, -0.3], [0.3, 0.3]);
    let mut render =
        |camera_x: f32, model_x: f32, host: &mut dyn HostStages| -> Result<ViewReport> {
            let mut view = view(
                ViewId::MAIN,
                Mat4::from_translation(Vec3::new(camera_x, 0.0, 0.0)),
            );
            view.host_needs.motion = true;
            let mut object = unlit("moving", mesh.clone(), -2.0, [1.0; 4]);
            object.world_from_local = Mat4::from_translation(Vec3::new(model_x, 0.0, -2.0));
            pipeline.render_view(
                &SceneFrame {
                    view: &view,
                    meshes: &[object],
                    lights: &LightList::default(),
                    world: None,
                },
                &target.target,
                host,
            )
        };
    let a = render(0.0, 0.0, &mut NoHost)?;
    let b = render(0.1, 0.2, &mut AbortSource)?;
    assert!(b.aborted);
    assert_ne!(a.evaluation_id, b.evaluation_id);
    let c = render(0.2, 0.4, &mut NoHost)?;
    assert_ne!(b.evaluation_id, c.evaluation_id);
    let motion = pipeline
        .read_motion(ViewId::MAIN)?
        .pixel(column(0.2, -2.0), 32);
    assert!(
        motion[0].abs() < 0.05 && motion[1].abs() < 0.05,
        "camera and model must both reset: {motion:?}"
    );
    gpu.check_errors()?;
    Ok(())
}

struct CountScene(Arc<AtomicUsize>);
impl PipelineFeature for CountScene {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.count_scene", Stage::OpaqueEffects)
    }
    fn encode(&mut self, _ctx: &mut FeatureContext<'_>) -> Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct FailOnceOverlay(Arc<AtomicUsize>);
impl PipelineFeature for FailOnceOverlay {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.fail_once_ui", Stage::Overlay)
    }
    fn encode(&mut self, _ctx: &mut FeatureContext<'_>) -> Result<()> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            anyhow::bail!("intentional late UI failure")
        }
        Ok(())
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn late_ui_retry_uses_one_committed_scene() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let scenes = Arc::new(AtomicUsize::new(0));
    let overlays = Arc::new(AtomicUsize::new(0));
    pipeline.register(CountScene(scenes.clone()))?;
    pipeline.register(FailOnceOverlay(overlays.clone()))?;
    let target = offscreen(&gpu);
    let view = view(ViewId::MAIN, Mat4::IDENTITY);
    let first = pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &[],
            lights: &LightList::default(),
            world: None,
        },
        &target.target,
        &mut NoHost,
    );
    assert!(first.unwrap_err().to_string().contains("test.fail_once_ui"));
    let id = pipeline
        .committed_scene_id(ViewId::MAIN)
        .expect("tonemap committed before UI");
    let retry =
        pipeline.retry_view_presentation(ViewId::MAIN, &target.target, None, &mut NoHost)?;
    assert_eq!(retry.evaluation_id, Some(id));
    assert_eq!(pipeline.committed_scene_id(ViewId::MAIN), Some(id));
    assert_eq!(scenes.load(Ordering::SeqCst), 1);
    assert_eq!(overlays.load(Ordering::SeqCst), 2);
    gpu.check_errors()?;
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn opaque_effects_feed_transparency_host_upscale_and_scene_readback() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    pipeline.register(opaque_affine(
        "test.opaque_bias",
        Some("test.opaque_scale"),
        1.0,
        0.5,
    ))?;
    pipeline.register(opaque_affine("test.opaque_scale", None, 0.25, 0.0))?;
    let target = offscreen(&gpu);
    let full = quad(&gpu, [-5.0, -5.0], [5.0, 5.0]);
    let front = quad(&gpu, [-0.35, -0.35], [0.35, 0.35]);
    let opaque = unlit("back", full, -2.0, [1.0; 4]);
    let mut transparent = unlit("front", front, -1.0, [1.0, 0.0, 0.0, 0.5]);
    transparent.params.transparent = true;
    let meshes = [opaque, transparent];
    let view = view(ViewId::MAIN, Mat4::IDENTITY);
    let frame = SceneFrame {
        view: &view,
        meshes: &meshes,
        lights: &LightList::default(),
        world: None,
    };
    let report = pipeline.render_view(&frame, &target.target, &mut ReturnSceneSource)?;
    assert!(report.evaluation_id.is_some());
    let scene = pipeline.read_scene_hdr(ViewId::MAIN)?;
    let outside = scene.pixel(2, 2);
    let inside = scene.pixel(32, 32);
    assert!(
        (outside[0] - 0.75).abs() < 0.02,
        "noncommuting scale then bias: {outside:?}"
    );
    assert!(
        inside[0] > inside[1] && inside[1] < outside[1],
        "transparent geometry uses effect result: {inside:?}"
    );
    assert_ne!(
        pixel(&read(&gpu, &target), 32, 32),
        pixel(&read(&gpu, &target), 2, 2)
    );
    let mut native = view.clone();
    native.features.set("test.opaque_scale", false);
    let native_frame = SceneFrame {
        view: &native,
        meshes: &meshes,
        lights: &LightList::default(),
        world: None,
    };
    pipeline.render_view(&native_frame, &target.target, &mut NoHost)?;
    assert!((pipeline.read_scene_hdr(ViewId::MAIN)?.pixel(2, 2)[0] - 1.5).abs() < 0.02);
    native.features.set("test.opaque_bias", false);
    let disabled_frame = SceneFrame {
        view: &native,
        meshes: &meshes,
        lights: &LightList::default(),
        world: None,
    };
    pipeline.render_view(&disabled_frame, &target.target, &mut NoHost)?;
    assert!((pipeline.read_scene_hdr(ViewId::MAIN)?.pixel(2, 2)[0] - 1.0).abs() < 0.02);
    let other = view(ViewId(42), Mat4::IDENTITY);
    let other_frame = SceneFrame {
        view: &other,
        meshes: &meshes,
        lights: &LightList::default(),
        world: None,
    };
    pipeline.render_view(&other_frame, &target.target, &mut NoHost)?;
    assert!((pipeline.read_scene_hdr(ViewId(42))?.pixel(2, 2)[0] - 0.75).abs() < 0.02);
    pipeline.forget_view(ViewId::MAIN);
    assert!(pipeline.read_scene_hdr(ViewId::MAIN).is_err());
    let new_report = pipeline.render_view(&frame, &target.target, &mut NoHost)?;
    assert_ne!(new_report.evaluation_id, report.evaluation_id);
    let mut resized = view.clone();
    resized.render_size = [31, 27];
    pipeline.render_view(
        &SceneFrame {
            view: &resized,
            meshes: &meshes,
            lights: &LightList::default(),
            world: None,
        },
        &target.target,
        &mut NoHost,
    )?;
    let image = pipeline.read_scene_hdr(ViewId::MAIN)?;
    assert_eq!(image.size, [31, 27]);
    assert!((image.pixel(2, 2)[0] - 0.75).abs() < 0.02);
    gpu.check_errors()?;
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn opaque_effects_feed_odd_size_native_upscale() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    pipeline.register(opaque_affine("test.odd_scale", None, 0.25, 0.0))?;
    pipeline.register(opaque_affine(
        "test.odd_bias",
        Some("test.odd_scale"),
        1.0,
        0.5,
    ))?;
    let output_size = [67, 63];
    let render_size = [31, 27];
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odd output"),
        size: wgpu::Extent3d {
            width: output_size[0],
            height: output_size[1],
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
        size: output_size,
        viewport: Viewport::full(output_size),
    };
    let view = ViewData::new(
        ViewId(99),
        Mat4::IDENTITY,
        perspective(),
        render_size,
        output_size,
    )?;
    let mesh = quad(&gpu, [-5.0, -5.0], [5.0, 5.0]);
    let meshes = [unlit("odd", mesh, -2.0, [1.0; 4])];
    pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &meshes,
            lights: &LightList::default(),
            world: None,
        },
        &target,
        &mut NoHost,
    )?;
    assert!((pipeline.read_scene_hdr(view.id)?.pixel(2, 2)[0] - 0.75).abs() < 0.02);
    let pixels =
        engine_render_wgpu::read_texture_rgba(&gpu, &texture, output_size[0], output_size[1])?;
    let offset = ((2 * output_size[0] + 2) * 4) as usize;
    assert!(
        pixels[offset] > 180 && pixels[offset] < 255,
        "native upscale must read opaque effects"
    );
    gpu.check_errors()?;
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn post_process_features_keep_values_above_one_and_run_in_constraint_order() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    // Registered first, but constrained to run after the scale.
    pipeline.register(affine("test.bias", Some("test.scale"), 1.0, 0.5))?;
    pipeline.register(affine("test.scale", None, 4.0, 0.0))?;
    let names = pipeline.registry().names();
    let position = |n: &str| names.iter().position(|x| *x == n).unwrap();
    assert!(position("test.scale") < position("test.bias"));
    let white = quad(&gpu, [-5.0, -5.0], [5.0, 5.0]);
    let meshes = [unlit("white", white, -2.0, [1.0; 4])];
    let target = offscreen(&gpu);
    let mut view = view(ViewId::MAIN, Mat4::IDENTITY);
    let render = |pipeline: &mut ScenePipeline, view: &ViewData| {
        pipeline.render_view(
            &SceneFrame {
                view,
                meshes: &meshes,
                lights: &LightList::default(),
                world: None,
            },
            &target.target,
            &mut NoHost,
        )
    };
    render(&mut pipeline, &view)?;
    let hdr = pipeline.read_hdr(ViewId::MAIN)?;
    // (1 * 4) + 0.5, not (1 + 0.5) * 4 = 6.
    assert_eq!(hdr.pixel(32, 32), [4.5, 4.5, 4.5, 1.0]);
    assert_eq!(
        pipeline.read_scene_hdr(ViewId::MAIN)?.pixel(32, 32),
        [1.0; 4]
    );
    assert_eq!(
        pixel(&read(&gpu, &target), 32, 32),
        [255; 4],
        "the output clamps"
    );
    // Values above one survive in the Rgba16Float target.
    view.features.set("test.bias", false);
    let report = render(&mut pipeline, &view)?;
    assert_eq!(
        pipeline.read_hdr(ViewId::MAIN)?.pixel(32, 32),
        [4.0, 4.0, 4.0, 1.0]
    );
    assert_eq!(
        report.features.get("test.bias").unwrap().activity,
        Activity::Disabled
    );
    gpu.check_errors()?;
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn per_view_overrides_disable_built_in_and_external_features() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let red = quad(&gpu, [-5.0, -5.0], [5.0, 5.0]);
    let meshes = [unlit("red", red, -2.0, [1.0, 0.0, 0.0, 1.0])];
    let target = offscreen(&gpu);
    let mut view = view(ViewId::MAIN, Mat4::IDENTITY);
    let mut render = |view: &ViewData| {
        pipeline
            .render_view(
                &SceneFrame {
                    view,
                    meshes: &meshes,
                    lights: &LightList::default(),
                    world: None,
                },
                &target.target,
                &mut NoHost,
            )
            .unwrap()
    };
    render(&view);
    assert_eq!(pixel(&read(&gpu, &target), 32, 32), [255, 0, 0, 255]);
    view.features.set(names::OPAQUE, false);
    view.features.set(names::OUTPUT, false);
    let report = render(&view);
    assert_eq!(pixel(&read(&gpu, &target), 32, 32), [0, 0, 0, 255]);
    assert!(!report.features.is_enabled(names::OPAQUE));
    assert_eq!(
        report.features.get(names::OUTPUT).unwrap().source,
        EnableSource::RequiredOverrideIgnored
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn two_views_with_different_eyes_render_into_external_targets() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let red = quad(&gpu, [-0.2, -0.2], [0.2, 0.2]);
    let meshes = [unlit("red", red, -2.0, [1.0, 0.0, 0.0, 1.0])];
    let centroid = |pixels: &[u8]| {
        let (mut sum, mut count) = (0.0, 0.0);
        for y in 0..SIZE[1] {
            for x in 0..SIZE[0] {
                if pixel(pixels, x, y)[0] > 128 {
                    sum += x as f64;
                    count += 1.0;
                }
            }
        }
        assert!(count > 0.0);
        sum / count
    };
    let mut centroids = Vec::new();
    for (id, eye_x) in [(ViewId::LEFT_EYE, -0.1), (ViewId::RIGHT_EYE, 0.1)] {
        let target = offscreen(&gpu);
        let eye = view(id, Mat4::from_translation(Vec3::new(eye_x, 0.0, 0.0)));
        pipeline.render_view(
            &SceneFrame {
                view: &eye,
                meshes: &meshes,
                lights: &LightList::default(),
                world: None,
            },
            &target.target,
            &mut NoHost,
        )?;
        centroids.push(centroid(&read(&gpu, &target)));
    }
    // Parallax: the left eye sees the object further right.
    let disparity = centroids[0] - centroids[1];
    let expected = (column(0.1, -2.0) as f64) - (column(-0.1, -2.0) as f64);
    assert!(
        disparity > 2.0 && (disparity - expected).abs() <= 1.5,
        "disparity {disparity} px, expected about {expected}"
    );
    assert!(pipeline.view_report(ViewId::LEFT_EYE).is_some());
    assert!(pipeline.view_report(ViewId::RIGHT_EYE).is_some());
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn optional_device_features_are_reported_honestly() -> Result<()> {
    let wanted = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::SHADER_F64;
    let requirements = GpuRequirements::default()
        .with(DeviceFeatureRequest::new("test.optional").prefer(wanted))
        .map_err(anyhow::Error::msg)?;
    let options = WgpuOptions {
        requirements: requirements.clone(),
        ..Default::default()
    };
    let gpu = GpuContext::headless_with_options(&options)?;
    let supported = gpu.adapter.features() & wanted;
    let outcome = gpu
        .feature_report()
        .outcome("test.optional")
        .unwrap()
        .clone();
    eprintln!(
        "adapter supports {supported:?}; enabled {:?}",
        outcome.enabled
    );
    assert_eq!(outcome.enabled, supported);
    assert_eq!(outcome.unavailable, wanted - supported);
    assert!(gpu.device.features().contains(outcome.enabled));
    assert_eq!(
        gpu.device.features() & wanted,
        supported,
        "nothing unrequested was claimed"
    );

    // An externally created device (the XR path) is adopted and verified.
    let adapter = gpu.adapter.clone();
    let composed = GpuContext::compose_device(&options, &adapter)?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&composed.descriptor("external")))?;
    let adopted = GpuContext::adopt(
        gpu.instance.clone(),
        adapter.clone(),
        device.clone(),
        queue.clone(),
        &requirements,
    )?;
    assert_eq!(
        adopted
            .feature_report()
            .outcome("test.optional")
            .unwrap()
            .enabled,
        supported
    );
    let strict = GpuRequirements::default()
        .with(DeviceFeatureRequest::new("test.strict").require(wgpu::Features::all_native_mask()))
        .map_err(anyhow::Error::msg)?;
    let error = GpuContext::adopt(gpu.instance.clone(), adapter, device, queue, &strict)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("test.strict requires"),
        "{error}"
    );
    Ok(())
}

/// A feature that only declares it needs normals.
struct NeedsNormals;
impl PipelineFeature for NeedsNormals {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.needs-normals", Stage::PostProcess).needs(FeatureNeeds {
            normals: true,
            motion: false,
            screen_lighting: false,
        })
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        assert!(ctx.normals().is_some());
        Ok(())
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn motion_vectors_and_normals_are_produced_on_request() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let target = offscreen(&gpu);
    let mesh = quad(&gpu, [-0.3, -0.3], [0.3, 0.3]);
    let mut view = view(ViewId::MAIN, Mat4::IDENTITY);
    view.host_needs.motion = true;
    let render = |pipeline: &mut ScenePipeline, x: f32| -> Result<ViewReport> {
        let mut moving = unlit("moving", mesh.clone(), -2.0, [1.0; 4]);
        moving.world_from_local = Mat4::from_translation(Vec3::new(x, 0.0, -2.0));
        pipeline.render_view(
            &SceneFrame {
                view: &view,
                meshes: &[moving],
                lights: &LightList::default(),
                world: None,
            },
            &target.target,
            &mut NoHost,
        )
    };
    let first = render(&mut pipeline, 0.0)?;
    assert_eq!(
        first.features.get(names::MOTION).unwrap().activity,
        Activity::Ran
    );
    assert_eq!(
        first.features.get(names::NORMALS).unwrap().activity,
        Activity::Idle
    );
    // A new draw has no history: camera-only (zero) motion.
    assert_eq!(
        pipeline.read_motion(ViewId::MAIN)?.pixel(32, 32)[..2],
        [0.0, 0.0]
    );
    render(&mut pipeline, 0.2)?;
    let expected = column(0.2, -2.0) as f32 - column(0.0, -2.0) as f32;
    let motion = pipeline
        .read_motion(ViewId::MAIN)?
        .pixel(column(0.2, -2.0), 32);
    // Current-to-previous: the object moved right, so motion points left.
    assert!(
        (motion[0] + expected).abs() <= 1.0 && motion[1].abs() < 0.05,
        "motion {motion:?}, expected x about {}",
        -expected
    );
    assert!(
        pipeline.read_normals(ViewId::MAIN).is_err(),
        "nobody needed normals"
    );

    pipeline.register(NeedsNormals)?;
    let report = render(&mut pipeline, 0.2)?;
    assert_eq!(
        report.features.get(names::NORMALS).unwrap().activity,
        Activity::Ran
    );
    let normals = pipeline.read_normals(ViewId::MAIN)?;
    assert_eq!(normals.pixel(column(0.2, -2.0), 32), [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(
        normals.pixel(1, 1),
        [0.0; 4],
        "coverage is zero where nothing drew"
    );
    // Unmoved this frame: motion is zero again.
    assert!(
        pipeline
            .read_motion(ViewId::MAIN)?
            .pixel(column(0.2, -2.0), 32)[0]
            .abs()
            < 1e-3
    );
    Ok(())
}

/// Lifecycle probes deliberately do no GPU work; a real pipeline is needed to
/// exercise prepared feature ownership and lazy registration around snapshots.
struct PresentationHook {
    name: &'static str,
    stage: Stage,
    fail: bool,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}
impl PipelineFeature for PresentationHook {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(self.name, self.stage).replayable()
    }
    fn begin_presentation_sequence(&mut self) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("begin {}", self.name));
        anyhow::ensure!(!self.fail, "intentional begin failure");
        Ok(())
    }
    fn end_presentation_sequence(&mut self) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("end {}", self.name));
    }
    fn encode(&mut self, _: &mut FeatureContext<'_>) -> Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn presentation_pins_only_enabled_participants_and_cleans_up_exactly_once() -> Result<()> {
    let context = gpu()?;
    let mut pipeline = ScenePipeline::new(context.clone())?;
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    for (name, fail) in [
        ("test.first", false),
        ("test.failing", true),
        ("test.last", false),
    ] {
        pipeline.register(PresentationHook {
            name,
            stage: Stage::Overlay,
            fail,
            calls: calls.clone(),
        })?;
    }
    assert!(
        pipeline
            .begin_presentation_sequence(&FeatureOverrides::default())
            .is_err()
    );
    pipeline.end_presentation_sequence();
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "begin test.first",
            "begin test.failing",
            "end test.failing",
            "end test.first"
        ]
    );
    calls.lock().unwrap().clear();
    let mut overrides = FeatureOverrides::default();
    overrides.set("test.failing", false);
    let result: Result<()> = pipeline.with_presentation_sequence(&overrides, |pipeline| {
        // Lazy scene registration appends indices and leaves pinned owners intact.
        pipeline.register(PresentationHook {
            name: "test.lazy_scene",
            stage: Stage::Prepass,
            fail: true,
            calls: calls.clone(),
        })?;
        assert!(
            pipeline
                .register(PresentationHook {
                    name: "test.late_ui",
                    stage: Stage::Overlay,
                    fail: false,
                    calls: calls.clone()
                })
                .is_err()
        );
        anyhow::bail!("intentional output failure")
    });
    assert!(result.is_err());
    pipeline.end_presentation_sequence();
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "begin test.first",
            "begin test.last",
            "end test.last",
            "end test.first"
        ]
    );
    context.check_errors()?;
    Ok(())
}
