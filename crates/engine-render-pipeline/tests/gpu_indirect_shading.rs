//! Actual native ScenePipeline material readbacks; no authored/desktop adapter.
//! These tests are source-only until root runs the real-adapter gate.
use anyhow::Result;
use engine_render_pipeline::{indirect::*, *};
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

struct Harness {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    mesh: Arc<GpuMesh>,
}
impl Harness {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("native IBL shader adapter: {:?}", gpu.adapter.get_info());
        let vertices =
            [(-0.25, -0.25), (0.25, -0.25), (0.25, 0.25), (-0.25, 0.25)].map(|(x, y)| {
                EngineVertex {
                    position: [x, y, 0.0],
                    uv: [0.5; 2],
                    color: [1.0; 4],
                    normal: [0.0, 0.0, 1.0],
                }
            });
        let mesh = GpuMesh::from_engine_vertices(
            &gpu.device,
            "native IBL quad",
            &vertices,
            &[0, 1, 2, 0, 2, 3],
        )?;
        Ok(Self {
            pipeline: ScenePipeline::new(gpu.clone())?,
            gpu,
            mesh,
        })
    }
    fn draw(
        &mut self,
        id: ViewId,
        meshes: &[SceneMesh<'_>],
        size: [u32; 2],
        ambient: Vec3,
    ) -> Result<HdrImage> {
        self.draw_with_reset(id, meshes, size, ambient, false)
    }
    fn draw_with_reset(
        &mut self,
        id: ViewId,
        meshes: &[SceneMesh<'_>],
        size: [u32; 2],
        ambient: Vec3,
        reset: bool,
    ) -> Result<HdrImage> {
        let mut view = ViewData::new(
            id,
            Mat4::from_translation(Vec3::new(0.0, 0.0, 3.0)),
            glam::camera::rh::proj::directx::orthographic(-2.0, 2.0, -2.0, 2.0, 0.1, 10.0),
            size,
            size,
        )?;
        view.ambient = ambient;
        view.reset_history = reset;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("native IBL output"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
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
            format: texture.format(),
            size,
            viewport: Viewport::full(size),
        };
        self.pipeline.render_view(
            &SceneFrame {
                view: &view,
                meshes,
                lights: &LightList::default(),
                world: None,
            },
            &target,
            &mut NoHost,
        )?;
        self.pipeline.read_scene_hdr(id)
    }
    fn mesh<'a>(&self, id: &'a str, x: f32, metallic: f32, roughness: f32) -> SceneMesh<'a> {
        SceneMesh {
            id,
            mesh: self.mesh.clone(),
            world_from_local: Mat4::from_translation(Vec3::new(x, 0.0, 0.0)),
            shader: None,
            params: MaterialParams {
                metallic,
                roughness,
                ..Default::default()
            },
            base_color: None,
            metallic_roughness: None,
            normal_map: None,
        }
    }
    fn resources(&self, environments: &[EnvironmentData]) -> Result<Arc<GpuIndirectResources>> {
        let lut = BrdfLut::prepare(32, 4096)?;
        Ok(Arc::new(GpuIndirectResources::prepare(
            &self.gpu,
            environments,
            &[],
            &lut,
        )?))
    }
}
fn environment(diffuse: [f32; 3], colors: impl Fn(usize, usize) -> [f32; 3]) -> EnvironmentData {
    let mut coefficients = [[0.0; 3]; 9];
    coefficients[0] = diffuse;
    EnvironmentData::new(
        DiffusePolynomial::new(coefficients).unwrap(),
        (0..4)
            .map(|mip| {
                std::array::from_fn(|face| {
                    let edge = 8 >> mip;
                    RadianceImage::new([edge; 2], vec![colors(mip, face); (edge * edge) as usize])
                        .unwrap()
                })
            })
            .collect(),
    )
    .unwrap()
}
fn lighting(index: usize) -> EnvironmentLighting {
    EnvironmentLighting::new(index, Quat::IDENTITY, 1.0).unwrap()
}
fn close(actual: [f32; 4], expected: [f32; 3], tolerance: f32) {
    for c in 0..3 {
        assert!(
            (actual[c] - expected[c]).abs() < tolerance,
            "{actual:?} vs {expected:?}, tolerance {tolerance}"
        );
    }
}
fn at(image: &HdrImage, x: f32) -> [f32; 4] {
    image.pixel(
        ((x + 2.0) * 0.25 * image.size[0] as f32) as u32,
        image.size[1] / 2,
    )
}

#[test]
#[ignore = "requires a real GPU; actual native lit pixels, HDR and neutral toggle"]
fn native_constant_environment_affects_lit_mesh_and_absence_restores_baseline() -> Result<()> {
    let mut h = Harness::new()?;
    let mesh = h.mesh("surface", 0.0, 0.0, 1.0);
    let ambient = Vec3::new(0.1, 0.2, 0.3);
    let baseline = h.draw(ViewId::MAIN, std::slice::from_ref(&mesh), [64; 2], ambient)?;
    close(at(&baseline, 0.0), ambient.to_array(), 0.001);
    let resources = h.resources(&[environment([4.0, 2.0, 0.25], |_, _| [0.0; 3])])?;
    let scene = Arc::new(IndirectScene::new(resources, Some(lighting(0)), vec![])?);
    h.pipeline.set_indirect(ViewId::MAIN, Some(scene))?;
    let lit = h.draw(ViewId::MAIN, std::slice::from_ref(&mesh), [64; 2], ambient)?;
    close(at(&lit, 0.0), [3.84, 1.92, 0.24], 0.005);
    assert!(at(&lit, 0.0)[0] > 1.0, "linear HDR must reach the mesh");
    h.pipeline.set_indirect(ViewId::MAIN, None)?;
    let off = h.draw(ViewId::MAIN, std::slice::from_ref(&mesh), [64; 2], ambient)?;
    assert_eq!(at(&off, 0.0), at(&baseline, 0.0));
    h.gpu.check_errors()
}

// Independent f64 hemisphere integration of the actual engine BRDF, used only
// for roughness >= .5 here. Low-roughness convergence belongs to gpu_indirect.
fn specular_reference(nv: f64, roughness: f64, f0: f64) -> f64 {
    let a2 = roughness.max(0.045).powi(4);
    let vn = nv.max(0.0001);
    let vx = (1.0 - nv * nv).sqrt();
    let gv = 2.0 * vn / (vn + (a2 + (1.0 - a2) * vn * vn).sqrt());
    let mut sum = 0.0;
    for y in 0..192 {
        let nl = (y as f64 + 0.5) / 192.0;
        let radial = (1.0 - nl * nl).sqrt();
        for x in 0..384 {
            let phi = std::f64::consts::TAU * (x as f64 + 0.5) / 384.0;
            let hx = vx + radial * phi.cos();
            let hy = radial * phi.sin();
            let hz = nv + nl;
            let inv = (hx * hx + hy * hy + hz * hz).sqrt().max(1e-6).recip();
            let nh = (hz * inv).max(0.0);
            let vh = ((vx * hx + nv * hz) * inv).max(0.0);
            let d = a2 / (std::f64::consts::PI * (nh * nh * (a2 - 1.0) + 1.0).powi(2)).max(1e-7);
            let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(1e-6);
            let fresnel = f0 + (1.0 - f0) * (1.0 - vh).clamp(0.0, 1.0).powi(5);
            sum += d * gv * gl * fresnel * nl / (4.0 * vn * nl).max(1e-6);
        }
    }
    sum * std::f64::consts::TAU / (192.0 * 384.0)
}
fn center_nv(size: u32) -> f64 {
    let offset = 2.0 / f64::from(size);
    3.0 / (9.0 + 2.0 * offset * offset).sqrt()
}

#[test]
#[ignore = "requires a real GPU; native metallic/specular roughness and rotation shading"]
fn native_specular_uses_mips_brdf_and_environment_rotation() -> Result<()> {
    let mut h = Harness::new()?;
    let data = environment([0.0; 3], |mip, _| {
        [[1.0; 3], [2.0; 3], [4.0; 3], [8.0; 3]][mip]
    });
    let resources = h.resources(&[data])?;
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(lighting(0)),
            vec![],
        )?)),
    )?;
    for roughness in [0.5, 0.75] {
        let mesh = h.mesh("metal", 0.0, 1.0, roughness);
        let image = h.draw(ViewId::MAIN, &[mesh], [64; 2], Vec3::ZERO)?;
        let radiance = if roughness == 0.5 { 3.0 } else { 5.0 };
        let expected =
            radiance * specular_reference(center_nv(64), f64::from(roughness), 1.0) as f32;
        close(at(&image, 0.0), [expected; 3], 0.04);
    }
    let faces = [
        [8.0, 0.0, 0.0],
        [0.0, 8.0, 0.0],
        [0.0, 0.0, 8.0],
        [8.0, 8.0, 0.0],
        [1.0, 2.0, 4.0],
        [4.0, 2.0, 1.0],
    ];
    let resources = h.resources(&[environment([0.0; 3], |_, face| faces[face])])?;
    let mesh = h.mesh("oriented", 0.0, 1.0, 0.5);
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources.clone(),
            Some(lighting(0)),
            vec![],
        )?)),
    )?;
    let first = h.draw(
        ViewId::MAIN,
        std::slice::from_ref(&mesh),
        [64; 2],
        Vec3::ZERO,
    )?;
    let rotation =
        EnvironmentLighting::new(0, Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2), 1.0)?;
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(rotation),
            vec![],
        )?)),
    )?;
    let rotated = h.draw(ViewId::MAIN, &[mesh], [64; 2], Vec3::ZERO)?;
    let factor = specular_reference(center_nv(64), 0.5, 1.0) as f32;
    close(at(&first, 0.0), faces[4].map(|v| v * factor), 0.04);
    close(at(&rotated, 0.0), faces[0].map(|v| v * factor), 0.04);
    h.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU; native per-draw probes, view isolation, resize and forget"]
fn native_probe_priority_blend_and_per_view_configuration_reach_pixels() -> Result<()> {
    let mut h = Harness::new()?;
    let resources = h.resources(&[
        environment([0.0, 0.0, 2.0], |_, _| [0.0; 3]),
        environment([2.0, 0.0, 0.0], |_, _| [0.0; 3]),
        environment([0.0, 2.0, 0.0], |_, _| [0.0; 3]),
    ])?;
    let probe = |id: &str, priority, index| -> Result<IndirectProbe> {
        Ok(IndirectProbe {
            volume: ProbeVolume::new(id.into(), Mat4::IDENTITY, Vec3::ONE, 0.5, priority)?,
            lighting: lighting(index),
        })
    };
    let scene = Arc::new(IndirectScene::new(
        resources.clone(),
        Some(lighting(0)),
        vec![probe("z", 1, 2)?, probe("a", 1, 1)?],
    )?);
    h.pipeline.set_indirect(ViewId::MAIN, Some(scene))?;
    let meshes = [
        h.mesh("center", 0.0, 0.0, 1.0),
        h.mesh("blend", 0.75, 0.0, 1.0),
        h.mesh("outside", 1.5, 0.0, 1.0),
    ];
    let image = h.draw(ViewId::MAIN, &meshes, [128; 2], Vec3::ZERO)?;
    close(at(&image, 0.0), [1.92, 0.0, 0.0], 0.004);
    close(at(&image, 0.75), [0.96, 0.0, 0.96], 0.004);
    close(at(&image, 1.5), [0.0, 0.0, 1.92], 0.004);
    assert_eq!(
        h.pipeline.view_report(ViewId::MAIN).unwrap().draw_calls,
        3,
        "different resolved probe weights must not merge into one material batch"
    );
    let higher = Arc::new(IndirectScene::new(
        resources.clone(),
        Some(lighting(0)),
        vec![probe("a", 1, 1)?, probe("z", 2, 2)?],
    )?);
    h.pipeline.set_indirect(ViewId::MAIN, Some(higher))?;
    let changed = h.draw(
        ViewId::MAIN,
        std::slice::from_ref(&meshes[0]),
        [64; 2],
        Vec3::ZERO,
    )?;
    close(at(&changed, 0.0), [0.0, 1.92, 0.0], 0.004);
    let other = ViewId(33);
    h.pipeline.set_indirect(
        other,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(lighting(0)),
            vec![],
        )?)),
    )?;
    let separate = h.draw(other, std::slice::from_ref(&meshes[0]), [32; 2], Vec3::ZERO)?;
    close(at(&separate, 0.0), [0.0, 0.0, 1.92], 0.004);
    let retained = h.draw(
        ViewId::MAIN,
        std::slice::from_ref(&meshes[0]),
        [96; 2],
        Vec3::ZERO,
    )?;
    close(at(&retained, 0.0), [0.0, 1.92, 0.0], 0.004);
    let cut = h.draw_with_reset(
        ViewId::MAIN,
        std::slice::from_ref(&meshes[0]),
        [96; 2],
        Vec3::ZERO,
        true,
    )?;
    close(at(&cut, 0.0), [0.0, 1.92, 0.0], 0.004);
    h.pipeline.forget_view(ViewId::MAIN);
    let forgotten = h.draw(
        ViewId::MAIN,
        std::slice::from_ref(&meshes[0]),
        [64; 2],
        Vec3::splat(0.2),
    )?;
    close(at(&forgotten, 0.0), [0.2; 3], 0.001);
    h.gpu.check_errors()
}

struct ScreenInputs {
    pipeline: Option<wgpu::ComputePipeline>,
}
impl PipelineFeature for ScreenInputs {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.indirect_screen", Stage::Prepass).needs(FeatureNeeds {
            screen_lighting: true,
            ..Default::default()
        })
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let module = setup
            .gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("IBL screen precedence fixture"),
                source: wgpu::ShaderSource::Wgsl(
                    r#"
@group(0) @binding(0) var gi: texture_storage_2d<rgba16float,write>;
@group(0) @binding(1) var ao: texture_storage_2d<rgba16float,write>;
@group(0) @binding(2) var reflection: texture_storage_2d<rgba16float,write>;
@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy >= textureDimensions(gi)) {return;}
 textureStore(gi,vec2<i32>(id.xy),vec4<f32>(1,0,0,1));
 textureStore(ao,vec2<i32>(id.xy),vec4<f32>(0.25,0,0,0));
 textureStore(reflection,vec2<i32>(id.xy),vec4<f32>(0,1,0,1));
}
"#
                    .into(),
                ),
            });
        self.pipeline = Some(setup.gpu.device.create_compute_pipeline(
            &wgpu::ComputePipelineDescriptor {
                label: Some("IBL screen precedence fixture"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            },
        ));
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let screen = ctx.screen_lighting().unwrap();
        let pipeline = self.pipeline.as_ref().unwrap();
        let group = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("IBL screen precedence fixture"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&screen.indirect.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            &screen.ambient_occlusion.view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&screen.reflection.view),
                    },
                ],
            });
        let mut pass = ctx.encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            ctx.view.render_size[0].div_ceil(8),
            ctx.view.render_size[1].div_ceil(8),
            1,
        );
        Ok(())
    }
}

#[test]
#[ignore = "requires a real GPU; actual screen GI/AO/reflection precedence over IBL"]
fn screen_inputs_replace_ibl_instead_of_adding_it_twice() -> Result<()> {
    let mut h = Harness::new()?;
    h.pipeline.register(ScreenInputs { pipeline: None })?;
    let resources = h.resources(&[environment([0.0, 0.0, 10.0], |_, _| [0.0; 3])])?;
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(lighting(0)),
            vec![],
        )?)),
    )?;
    let diffuse = h.mesh("diffuse", 0.0, 0.0, 1.0);
    let image = h.draw(ViewId::MAIN, &[diffuse], [64; 2], Vec3::ZERO)?;
    close(at(&image, 0.0), [0.25, 0.0, 0.0], 0.002);
    let resources = h.resources(&[environment([0.0; 3], |_, _| [10.0, 0.0, 0.0])])?;
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(lighting(0)),
            vec![],
        )?)),
    )?;
    let metal = h.mesh("metal", 0.0, 1.0, 0.0);
    let image = h.draw(ViewId::MAIN, &[metal], [64; 2], Vec3::ZERO)?;
    close(at(&image, 0.0), [0.0, 1.0, 0.0], 0.002);
    h.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU; invalid native bindings fail before scene encoding"]
fn native_scene_errors_and_distinct_material_budget_are_explicit() -> Result<()> {
    let mut h = Harness::new()?;
    let resources = h.resources(&[environment([1.0; 3], |_, _| [0.0; 3])])?;
    assert!(IndirectScene::new(resources.clone(), Some(lighting(1)), vec![]).is_err());
    let bad = || IndirectProbe {
        volume: ProbeVolume::new("same".into(), Mat4::IDENTITY, Vec3::ONE, 0.0, 0).unwrap(),
        lighting: lighting(0),
    };
    assert!(IndirectScene::new(resources.clone(), None, vec![bad(), bad()]).is_err());
    assert!(
        IndirectScene::new(
            resources.clone(),
            None,
            (0..MAX_PROBES + 1).map(|_| bad()).collect()
        )
        .is_err()
    );
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(lighting(0)),
            vec![],
        )?)),
    )?;
    let ids: Vec<_> = (0..MAX_INDIRECT_MATERIALS + 1)
        .map(|i| format!("m{i}"))
        .collect();
    let meshes: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let mut mesh = h.mesh(id, 0.0, 0.0, 1.0);
            mesh.params.tint[0] = i as f32 / (MAX_INDIRECT_MATERIALS + 1) as f32;
            mesh
        })
        .collect();
    let error = h
        .draw(ViewId::MAIN, &meshes, [64; 2], Vec3::ZERO)
        .expect_err("material budget should reject");
    assert!(format!("{error:#}").contains("distinct indirect materials"));
    h.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU; diffuse coefficient rotation and legacy custom opt-in contract"]
fn diffuse_rotation_and_legacy_custom_material_remain_explicit() -> Result<()> {
    let mut h = Harness::new()?;
    let mut coefficients = [[0.0; 3]; 9];
    coefficients[0] = [0.5; 3];
    coefficients[3] = [0.5, 0.0, 0.0];
    let data = EnvironmentData::new(
        DiffusePolynomial::new(coefficients)?,
        environment([0.0; 3], |_, _| [0.0; 3]).mips().to_vec(),
    )?;
    let resources = h.resources(&[data])?;
    let scene = Arc::new(IndirectScene::new(
        resources.clone(),
        Some(lighting(0)),
        vec![],
    )?);
    h.pipeline.set_indirect(ViewId::MAIN, Some(scene.clone()))?;
    let mesh = h.mesh("normal", 0.0, 0.0, 1.0);
    let normal = h.draw(
        ViewId::MAIN,
        std::slice::from_ref(&mesh),
        [64; 2],
        Vec3::splat(0.2),
    )?;
    close(at(&normal, 0.0), [0.96, 0.48, 0.48], 0.003);
    let rotated =
        EnvironmentLighting::new(0, Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), 1.0)?;
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(
            resources,
            Some(rotated),
            vec![],
        )?)),
    )?;
    assert_eq!(
        Arc::strong_count(&scene),
        1,
        "replacement releases the old native scene"
    );
    let image = h.draw(
        ViewId::MAIN,
        std::slice::from_ref(&mesh),
        [64; 2],
        Vec3::splat(0.2),
    )?;
    close(at(&image, 0.0), [0.48; 3], 0.003);
    let source = LIT_WGSL.replace(
        "engine_material_indirect_lighting",
        "engine_indirect_lighting",
    );
    let mut custom = mesh;
    custom.shader = Some(&source);
    let image = h.draw(ViewId::MAIN, &[custom], [64; 2], Vec3::splat(0.2))?;
    close(at(&image, 0.0), [0.2; 3], 0.001);
    let empty = h.resources(&[])?;
    h.pipeline.set_indirect(
        ViewId::MAIN,
        Some(Arc::new(IndirectScene::new(empty, None, vec![])?)),
    )?;
    let mesh = h.mesh("empty", 0.0, 0.0, 1.0);
    let image = h.draw(ViewId::MAIN, &[mesh], [64; 2], Vec3::splat(0.2))?;
    close(at(&image, 0.0), [0.2; 3], 0.001);
    h.gpu.check_errors()
}
