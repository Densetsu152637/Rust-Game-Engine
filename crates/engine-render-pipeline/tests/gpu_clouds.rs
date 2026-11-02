//! Real-adapter readbacks of `engine.clouds`. Pixels are compared with the CPU
//! reference in `clouds::model` (same medium, `f64`): a uniform slab against
//! its closed-form transmittance, the noisy layer against the CPU march, the
//! sun (forward scattering, self-shadowing, sun changes), depth occlusion
//! with an opaque wall at a chosen view depth, the order against the
//! atmosphere and the fog, and the wind against an exact symmetry (advecting
//! the noise equals moving the camera against the wind). Run:
//! `cargo test -p engine-render-pipeline --test gpu_clouds -- --include-ignored --test-threads=1 --nocapture`.

use anyhow::{Context, Result};
use engine_render_pipeline::atmosphere::{
    ATMOSPHERE_AERIAL_FEATURE, ATMOSPHERE_FEATURE, AtmosphereControls, AtmosphereFrame,
};
use engine_render_pipeline::clouds::{
    CLOUDS_FEATURE, CloudControls, CloudFrame, CloudSettings, composite, register_features,
    slab_transmittance,
};
use engine_render_pipeline::fog::{FOG_FEATURE, FogControls, FogFrame, FogSettings, fogged};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{DQuat, DVec3, EulerRot, Mat4, Quat, Vec3};
use std::sync::{Arc, Mutex};

const FOV: f32 = 1.0;
const NEAR: f32 = 1.0;
const FAR: f32 = 2.0e5;
const SUN_LUX: f32 = 3.0;

type Wall = Arc<Mutex<Option<([f32; 3], f32)>>>;

const WALL_WGSL: &str = r#"
@group(0) @binding(0) var<uniform> wall: vec4<f32>;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
struct Pixel { @location(0) color: vec4<f32>, @builtin(frag_depth) depth: f32 }
@fragment fn fs_main() -> Pixel {
    var out: Pixel;
    out.color = vec4<f32>(wall.rgb, 1.0);
    out.depth = wall.w;
    return out;
}
"#;

/// Opaque stage producer: a fullscreen opaque wall at a fixed depth.
struct WallProducer {
    wall: Wall,
    layout: Option<wgpu::BindGroupLayout>,
    pipeline: Option<wgpu::RenderPipeline>,
}

impl PipelineFeature for WallProducer {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("test.wall", Stage::Opaque)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test wall"),
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
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test wall"),
            source: wgpu::ShaderSource::Wgsl(WALL_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("test wall"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        self.pipeline = Some(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("test wall"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            }),
        );
        self.layout = Some(layout);
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let Some(([r, g, b], depth)) = *self.wall.lock().unwrap() else {
            return Ok(());
        };
        let buffer = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test wall"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.gpu
            .queue
            .write_buffer(&buffer, 0, bytemuck::cast_slice(&[r, g, b, depth]));
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("test wall"),
                layout: self.layout.as_ref().context("wall layout")?,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
        let mut pass = ctx.scene_pass("test wall")?;
        pass.set_pipeline(self.pipeline.as_ref().context("wall pipeline")?);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

#[derive(Clone)]
struct Shot {
    id: ViewId,
    size: [u32; 2],
    /// Camera position relative to the applied origin.
    camera: [f32; 3],
    /// Applied floating origin (absolute = origin + camera).
    origin: [f64; 3],
    yaw: f32,
    pitch: f32,
    sun_elevation: f32,
    sun_azimuth: f32,
    lux: f32,
    ambient: f32,
    time: f64,
    settings: Option<CloudSettings>,
    clouds: bool,
    sun: bool,
    /// Sub-pixel projection jitter in render pixels (x right, y down).
    jitter: [f32; 2],
    fog: Option<FogSettings>,
    atmosphere: bool,
}

/// A layer 100..300 m above sea level, uniform, no light: the pixel is the
/// wall times the closed-form slab transmittance.
fn slab() -> CloudSettings {
    CloudSettings {
        base_altitude: 100.0,
        top_altitude: 300.0,
        density: 0.005,
        coverage: 1.0,
        albedo: [1.0; 3],
        anisotropy: 0.0,
        noise_amount: 0.0,
        noise_scale: 80.0,
        noise_octaves: 2,
        primary_steps: 96,
        light_steps: 6,
        light_distance: 200.0,
        sun_intensity: 1.0,
        ambient_intensity: 1.0,
        multiple_scattering: true,
        max_distance: 20_000.0,
        ..Default::default()
    }
}

/// A broken, lumpy layer at the same altitudes.
fn lumpy() -> CloudSettings {
    CloudSettings {
        density: 0.012,
        coverage: 0.6,
        noise_amount: 1.0,
        noise_seed: 5,
        ..slab()
    }
}

impl Shot {
    fn new(id: u64) -> Self {
        Self {
            id: ViewId(id),
            size: [65, 37],
            camera: [0.0, 2.0, 0.0],
            origin: [0.0; 3],
            yaw: 0.0,
            pitch: 0.6,
            sun_elevation: 35.0,
            sun_azimuth: 0.0,
            lux: SUN_LUX,
            ambient: 0.0,
            time: 0.0,
            settings: Some(slab()),
            clouds: true,
            sun: false,
            jitter: [0.0; 2],
            fog: None,
            atmosphere: false,
        }
    }

    fn projection(&self) -> Mat4 {
        glam::camera::rh::proj::directx::perspective(
            FOV,
            self.size[0] as f32 / self.size[1] as f32,
            NEAR,
            FAR,
        )
    }

    fn world_from_view(&self) -> Mat4 {
        Mat4::from_rotation_translation(
            Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0),
            Vec3::from(self.camera),
        )
    }

    fn absolute(&self) -> [f64; 3] {
        std::array::from_fn(|k| self.origin[k] + f64::from(self.camera[k]))
    }

    fn frame(&self) -> CloudFrame {
        CloudFrame::new(
            self.settings.expect("shot has settings"),
            self.absolute(),
            self.time,
        )
        .unwrap()
    }

    fn fog_frame(&self) -> FogFrame {
        FogFrame::new(self.fog.expect("shot has fog"), self.absolute(), self.time).unwrap()
    }

    /// Unit vector toward the sun.
    fn sun_dir(&self) -> [f64; 3] {
        let (e, a) = (
            f64::from(self.sun_elevation).to_radians(),
            f64::from(self.sun_azimuth).to_radians(),
        );
        [e.cos() * a.sin(), e.sin(), -e.cos() * a.cos()]
    }

    /// The sun as the CPU reference takes it, `None` without a light.
    fn sun_light(&self) -> Option<([f64; 3], [f64; 3])> {
        self.sun.then(|| (self.sun_dir(), [f64::from(self.lux); 3]))
    }

    fn rotation(&self) -> DQuat {
        DQuat::from_euler(
            EulerRot::YXZ,
            f64::from(self.yaw),
            f64::from(self.pitch),
            0.0,
        )
    }

    /// World direction of the centre of pixel (x, y).
    fn dir(&self, x: u32, y: u32) -> [f64; 3] {
        (self.rotation() * self.local_dir(x, y)).to_array()
    }

    /// View-space unit direction of pixel (x, y). Jitter translates the
    /// rasterized image by `jitter` pixels, so the pixel shows the ray through
    /// the unjittered position `(x + 0.5 - jx, y + 0.5 - jy)`.
    fn local_dir(&self, x: u32, y: u32) -> DVec3 {
        let aspect = f64::from(self.size[0]) / f64::from(self.size[1]);
        let tan = (f64::from(FOV) * 0.5).tan();
        let px = f64::from(x) + 0.5 - f64::from(self.jitter[0]);
        let py = f64::from(y) + 0.5 - f64::from(self.jitter[1]);
        let nx = px / f64::from(self.size[0]) * 2.0 - 1.0;
        let ny = 1.0 - py / f64::from(self.size[1]) * 2.0;
        DVec3::new(nx * tan * aspect, ny * tan, -1.0).normalize()
    }

    /// Distance to a wall at view depth `z` along pixel (x, y)'s ray.
    fn wall_distance(&self, x: u32, y: u32, z: f32) -> f64 {
        f64::from(z) / -self.local_dir(x, y).z
    }

    /// Depth value of a wall at view depth `z`.
    fn wall_depth(&self, z: f32) -> f32 {
        let clip = self.projection() * glam::Vec4::new(0.0, 0.0, -z, 1.0);
        clip.z / clip.w
    }

    /// CPU reference pixel: `scene` (a wall at view depth `z`, or the sky when
    /// `z` is `None`) with the clouds over it.
    fn expected(&self, x: u32, y: u32, scene: [f64; 3], z: Option<f32>) -> [f64; 3] {
        composite(
            &self.frame(),
            self.dir(x, y),
            z.map(|z| self.wall_distance(x, y, z)),
            scene,
            self.sun_light(),
            [f64::from(self.ambient); 3],
        )
    }
}

struct Rig {
    gpu: GpuContext,
    scene: ScenePipeline,
    clouds: CloudControls,
    fog: FogControls,
    atmosphere: AtmosphereControls,
    wall: Wall,
    with_clouds: bool,
    with_scattering: bool,
}

impl Rig {
    /// A pipeline with the clouds (and, for `scattering`, the atmosphere and
    /// the fog registered ahead of them in the same registry).
    fn new(with_clouds: bool, with_scattering: bool) -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {}", gpu.adapter.get_info().name);
        let mut scene = ScenePipeline::new(gpu.clone())?;
        let clouds = CloudControls::default();
        let fog = FogControls::default();
        let atmosphere = AtmosphereControls::default();
        let wall: Wall = Arc::new(Mutex::new(None));
        let mut registry = FeatureRegistry::default();
        registry.register(WallProducer {
            wall: wall.clone(),
            layout: None,
            pipeline: None,
        })?;
        if with_scattering {
            engine_render_pipeline::atmosphere::register_features(&mut registry, &atmosphere)?;
            engine_render_pipeline::fog::register_features(&mut registry, &fog)?;
        }
        if with_clouds {
            register_features(&mut registry, &clouds)?;
        }
        scene.add_features(registry)?;
        Ok(Self {
            gpu,
            scene,
            clouds,
            fog,
            atmosphere,
            wall,
            with_clouds,
            with_scattering,
        })
    }

    fn render(&mut self, shot: &Shot) -> Result<HdrImage> {
        let mut view = ViewData::new(
            shot.id,
            shot.world_from_view(),
            shot.projection(),
            shot.size,
            shot.size,
        )?;
        view.jitter = shot.jitter;
        view.ambient = Vec3::splat(shot.ambient);
        if self.with_clouds {
            self.clouds
                .set(shot.id, shot.settings.map(|_| shot.frame()))?;
            view.features.set(CLOUDS_FEATURE, shot.clouds);
        }
        if self.with_scattering {
            self.fog.set(shot.id, shot.fog.map(|_| shot.fog_frame()))?;
            view.features.set(FOG_FEATURE, shot.fog.is_some());
            self.atmosphere.set(
                shot.id,
                shot.atmosphere
                    .then(|| AtmosphereFrame::new(Default::default(), shot.absolute()[1]))
                    .transpose()?,
            )?;
            view.features.set(ATMOSPHERE_FEATURE, shot.atmosphere);
            view.features
                .set(ATMOSPHERE_AERIAL_FEATURE, shot.atmosphere);
        }
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("clouds test output"),
            size: wgpu::Extent3d {
                width: shot.size[0],
                height: shot.size[1],
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
            size: shot.size,
            viewport: Viewport::full(shot.size),
        };
        let [x, y, z] = shot.sun_dir().map(|v| -v as f32);
        let lights = LightList {
            lights: if shot.sun {
                vec![Light::directional(Vec3::new(x, y, z), Vec3::ONE, shot.lux)]
            } else {
                vec![]
            },
        };
        self.scene.render_view(
            &SceneFrame {
                view: &view,
                meshes: &[],
                lights: &lights,
                world: None,
            },
            &target,
            &mut NoHost,
        )?;
        self.gpu.check_errors()?;
        self.scene.read_scene_hdr(shot.id)
    }

    fn set_wall(&self, wall: Option<([f32; 3], f32)>) {
        *self.wall.lock().unwrap() = wall;
    }

    /// A wall of `colour` at view depth `z` for `shot`.
    fn wall_at(&self, shot: &Shot, colour: [f32; 3], z: f32) {
        self.set_wall(Some((colour, shot.wall_depth(z))));
    }
}

fn rgb(p: [f32; 4]) -> [f64; 3] {
    [p[0], p[1], p[2]].map(f64::from)
}

fn luma(c: [f64; 3]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn assert_close(actual: [f64; 3], expected: [f64; 3], relative: f64, what: &str) {
    for k in 0..3 {
        let tolerance = relative * expected[k].abs() + 5.0e-4;
        assert!(
            (actual[k] - expected[k]).abs() <= tolerance,
            "{what}: channel {k} {actual:?} vs reference {expected:?}"
        );
    }
}

/// True when `actual` is outside the `assert_close` tolerance of `expected`.
fn misses(actual: [f64; 3], expected: [f64; 3], relative: f64) -> bool {
    (0..3).any(|k| (actual[k] - expected[k]).abs() > relative * expected[k].abs() + 5.0e-4)
}

fn max_difference(a: &HdrImage, b: &HdrImage) -> f32 {
    a.pixels
        .iter()
        .zip(&b.pixels)
        .flat_map(|(p, q)| (0..3).map(move |k| (p[k] - q[k]).abs()))
        .fold(0.0, f32::max)
}

fn mean_luma(image: &HdrImage) -> f64 {
    image.pixels.iter().map(|p| luma(rgb(*p))).sum::<f64>() / image.pixels.len() as f64
}

const SAMPLES: [(u32, u32); 7] = [
    (2, 2),
    (32, 2),
    (60, 5),
    (32, 18),
    (10, 30),
    (55, 33),
    (3, 18),
];

/// Transmittance below which the early exit can leave a residual.
const OPAQUE: f64 = 0.05;

/// Wall view depth far beyond the layer for every pixel.
const BEYOND: f32 = 4000.0;

// -- density and coverage --------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn a_constant_slab_matches_the_closed_form_transmittance() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let wall = [0.5f32; 3];
    let mut centre = Vec::new();
    for (density, pitch) in [
        (0.002f32, 0.6f32),
        (0.005, 0.6),
        (0.01, 0.6),
        (0.005, 0.9),
        (0.005, 1.3),
    ] {
        let mut shot = Shot::new(1);
        shot.pitch = pitch;
        shot.settings = Some(CloudSettings { density, ..slab() });
        rig.wall_at(&shot, wall, BEYOND);
        let image = rig.render(&shot)?;
        for (x, y) in SAMPLES {
            let dir = shot.dir(x, y);
            let want = 0.5 * slab_transmittance(f64::from(density), 200.0, dir[1]);
            let what = format!("slab density {density} pitch {pitch} pixel ({x},{y})");
            if want > 0.5 * OPAQUE {
                assert_close(rgb(image.pixel(x, y)), [want; 3], 0.015, &what);
            } else {
                // The march stops at the cutoff: nearly opaque stays nearly
                // opaque, within one step of the early exit.
                assert!(rgb(image.pixel(x, y))[0] < 0.5 * 2.0 * OPAQUE, "{what}");
            }
        }
        if pitch == 0.6 {
            centre.push(rgb(image.pixel(32, 18))[0]);
        }
    }
    assert!(
        centre[0] > centre[1] && centre[1] > centre[2],
        "more density, lower transmittance: {centre:?}"
    );
    // A camera inside the layer sees a shorter path through it.
    let mut inside = Shot::new(2);
    inside.camera[1] = 200.0;
    rig.wall_at(&inside, wall, BEYOND);
    let image = rig.render(&inside)?;
    let dir = inside.dir(32, 18);
    // The remaining path above 200 m: half the thickness, with the lower
    // half of the profile missing: integrate the CPU march instead.
    let want = composite(
        &inside.frame(),
        dir,
        Some(inside.wall_distance(32, 18, BEYOND)),
        [0.5; 3],
        None,
        [0.0; 3],
    );
    assert_close(rgb(image.pixel(32, 18)), want, 0.015, "camera in layer");
    assert!(want[0] > 0.5 * slab_transmittance(0.005, 200.0, dir[1]));
    // A camera above the layer looking down sees it from the top.
    let mut above = Shot::new(3);
    above.camera[1] = 500.0;
    above.pitch = -0.6;
    rig.wall_at(&above, wall, BEYOND);
    let image = rig.render(&above)?;
    let want = 0.5 * slab_transmittance(0.005, 200.0, above.dir(32, 18)[1]);
    assert_close(
        rgb(image.pixel(32, 18)),
        [want; 3],
        0.015,
        "camera above the layer",
    );
    // Looking away from the layer shows the plain wall.
    let mut away = Shot::new(4);
    away.pitch = -0.4;
    rig.wall_at(&away, wall, BEYOND);
    assert_close(
        rgb(rig.render(&away)?.pixel(32, 18)),
        [0.5; 3],
        0.0005,
        "no layer along the ray",
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn coverage_and_noise_match_the_cpu_march_and_lower_the_transmittance() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let wall = [0.5f32; 3];
    let mut means = Vec::new();
    for coverage in [0.3f32, 0.6, 0.9] {
        let mut shot = Shot::new(5);
        shot.settings = Some(CloudSettings {
            coverage,
            ..lumpy()
        });
        rig.wall_at(&shot, wall, BEYOND);
        let image = rig.render(&shot)?;
        for (x, y) in SAMPLES {
            assert_close(
                rgb(image.pixel(x, y)),
                shot.expected(x, y, wall.map(f64::from), Some(BEYOND)),
                0.04,
                &format!("coverage {coverage} pixel ({x},{y})"),
            );
        }
        means.push(mean_luma(&image));
    }
    assert!(
        means[0] > means[1] && means[1] > means[2],
        "more coverage, lower transmittance: {means:?}"
    );
    // No coverage: the plain wall.
    let mut none = Shot::new(5);
    none.settings = Some(CloudSettings {
        coverage: 0.0,
        ..lumpy()
    });
    let image = rig.render(&none)?;
    assert!(
        image.pixels.iter().all(|p| rgb(*p) == wall.map(f64::from)),
        "zero coverage changes nothing"
    );
    // The noise shows: a lumpy layer is far rougher across a row than a
    // uniform one of the same mean density, and another seed is another sky.
    let mut shot = Shot::new(5);
    shot.settings = Some(lumpy());
    let lumps = rig.render(&shot)?;
    shot.settings = Some(CloudSettings {
        noise_amount: 0.0,
        ..lumpy()
    });
    let flat = rig.render(&shot)?;
    let roughness = |image: &HdrImage| {
        let row: Vec<f64> = (0..65).map(|x| rgb(image.pixel(x, 18))[0]).collect();
        row.windows(3)
            .map(|w| (w[0] - 2.0 * w[1] + w[2]).abs())
            .sum::<f64>()
            / 63.0
    };
    assert!(roughness(&lumps) > 3.0 * roughness(&flat) + 1e-4);
    shot.settings = Some(CloudSettings {
        noise_seed: 6,
        ..lumpy()
    });
    assert!(max_difference(&lumps, &rig.render(&shot)?) > 0.01);
    // Octaves add detail.
    shot.settings = Some(CloudSettings {
        noise_octaves: 4,
        ..lumpy()
    });
    let detailed = rig.render(&shot)?;
    assert!(max_difference(&lumps, &detailed) > 0.01);
    assert_close(
        rgb(detailed.pixel(32, 18)),
        {
            let mut s = shot.clone();
            s.settings = Some(CloudSettings {
                noise_octaves: 4,
                ..lumpy()
            });
            s.expected(32, 18, wall.map(f64::from), Some(BEYOND))
        },
        0.04,
        "four octaves",
    );
    // A camera far from the origin and a long time still match the CPU
    // reference of the same absolute frame.
    let mut distant = Shot::new(5);
    distant.settings = Some(CloudSettings {
        wind: [3.0, 0.0, -2.0],
        ..lumpy()
    });
    distant.origin = [3.0e9, 0.0, -7.0e9];
    distant.time = 123_456.0;
    rig.wall_at(&distant, wall, BEYOND);
    let image = rig.render(&distant)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(image.pixel(x, y)),
            distant.expected(x, y, wall.map(f64::from), Some(BEYOND)),
            0.04,
            &format!("distant pixel ({x},{y})"),
        );
    }
    // Altitude is absolute: a floating origin that carries the layer's height
    // is the same sky.
    let mut high = Shot::new(5);
    high.settings = Some(CloudSettings {
        base_altitude: 5100.0,
        top_altitude: 5300.0,
        ..lumpy()
    });
    high.camera[1] = 2.0;
    high.origin = [0.0, 5000.0, 0.0];
    rig.wall_at(&high, wall, BEYOND);
    let shifted = rig.render(&high)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(shifted.pixel(x, y)),
            high.expected(x, y, wall.map(f64::from), Some(BEYOND)),
            0.04,
            &format!("high layer pixel ({x},{y})"),
        );
    }
    Ok(())
}

// -- sun -------------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn sunlight_scatters_forward_and_follows_the_sun() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let settings = CloudSettings {
        density: 0.004,
        anisotropy: 0.7,
        ..slab()
    };
    let mut shot = Shot::new(6);
    shot.settings = Some(settings);
    shot.sun = true;
    rig.set_wall(None);

    // The sun ahead of the camera (the view looks 0.6 rad up toward -z), then
    // behind it.
    shot.sun_azimuth = 0.0;
    shot.sun_elevation = 34.0;
    let toward = rig.render(&shot)?;
    shot.sun_azimuth = 180.0;
    let away = rig.render(&shot)?;
    let (a, b) = (rgb(toward.pixel(32, 18)), rgb(away.pixel(32, 18)));
    assert!(
        luma(a) > 1.5 * luma(b) && luma(b) > 0.0,
        "silver lining: toward {a:?}, away {b:?}"
    );
    for (name, sun_azimuth, image) in [("toward", 0.0, &toward), ("away", 180.0, &away)] {
        let shot = Shot {
            sun_azimuth,
            ..shot.clone()
        };
        for (x, y) in SAMPLES {
            assert_close(
                rgb(image.pixel(x, y)),
                shot.expected(x, y, [0.0; 3], None),
                0.04,
                &format!("sun {name} pixel ({x},{y})"),
            );
        }
    }

    // A sun to the right brightens the right of the image, to the left the
    // left.
    let column = |image: &HdrImage, x: u32| luma(rgb(image.pixel(x, 18)));
    shot.sun_azimuth = 50.0;
    shot.sun_elevation = 34.0;
    let right = rig.render(&shot)?;
    shot.sun_azimuth = -50.0;
    let left = rig.render(&shot)?;
    assert!(column(&right, 60) > 1.2 * column(&right, 4));
    assert!(column(&left, 4) > 1.2 * column(&left, 60));
    for x in [4, 20, 32, 44, 60] {
        let (r, l) = (column(&right, x), column(&left, 64 - x));
        assert!(
            (r - l).abs() < 0.02 * r + 5e-4,
            "mirror at column {x}: {r} vs {l}"
        );
    }

    // Raising and lowering the sun changes the image, in the reference's way;
    // below the horizon plane there is no sun term at all.
    shot.sun_azimuth = 0.0;
    let mut previous = luma(a);
    for elevation in [60.0f32, 85.0] {
        shot.sun_elevation = elevation;
        let image = rig.render(&shot)?;
        assert_close(
            rgb(image.pixel(32, 18)),
            shot.expected(32, 18, [0.0; 3], None),
            0.04,
            &format!("sun elevation {elevation}"),
        );
        let now = luma(rgb(image.pixel(32, 18)));
        assert!(now < previous, "leaving the view direction dims the cloud");
        previous = now;
    }
    shot.sun_elevation = -20.0;
    assert!(
        rig.render(&shot)?.pixels.iter().all(|p| p[..3] == [0.0; 3]),
        "the sun below the horizon plane lights nothing"
    );
    // The intensity multiplier scales the sun term; no light, no sun term.
    shot.sun_elevation = 34.0;
    shot.settings = Some(CloudSettings {
        sun_intensity: 2.0,
        ..settings
    });
    assert_close(
        rgb(rig.render(&shot)?.pixel(32, 18)),
        a.map(|v| v * 2.0),
        0.04,
        "2x sun",
    );
    shot.sun = false;
    assert!(rig.render(&shot)?.pixels.iter().all(|p| p[..3] == [0.0; 3]));
    // Ambient light alone lights the layer and the albedo tints it.
    shot.ambient = 0.3;
    shot.settings = Some(CloudSettings {
        albedo: [1.0, 0.5, 0.25],
        ..settings
    });
    let image = rig.render(&shot)?;
    assert_close(
        rgb(image.pixel(32, 18)),
        shot.expected(32, 18, [0.0; 3], None),
        0.04,
        "ambient",
    );
    let c = rgb(image.pixel(32, 18));
    assert!(c[0] > 1.8 * c[1] && c[1] > 1.8 * c[2] && c[2] > 0.0);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn self_shadowing_makes_the_base_darker_than_the_top() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let settings = CloudSettings {
        density: 0.01,
        light_distance: 300.0,
        ..slab()
    };
    let mut up = Shot::new(7);
    up.settings = Some(settings);
    up.sun = true;
    up.sun_elevation = 50.0;
    up.sun_azimuth = 180.0;
    rig.set_wall(None);
    // From below the camera sees the base: its samples look at the sun
    // through the cloud above them.
    let below = rig.render(&up)?;
    // From above it sees the sunlit top.
    let mut down = up.clone();
    down.camera[1] = 500.0;
    down.pitch = -0.6;
    let above = rig.render(&down)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(below.pixel(x, y)),
            up.expected(x, y, [0.0; 3], None),
            0.04,
            &format!("base pixel ({x},{y})"),
        );
        assert_close(
            rgb(above.pixel(x, y)),
            down.expected(x, y, [0.0; 3], None),
            0.04,
            &format!("top pixel ({x},{y})"),
        );
    }
    let (b, t) = (
        luma(rgb(below.pixel(32, 18))),
        luma(rgb(above.pixel(32, 18))),
    );
    assert!(
        t > 1.5 * b && b > 0.0,
        "the sunlit top is brighter than the shadowed base: {t} vs {b}"
    );
    // A longer light march sees more of the cloud above and darkens the base.
    let mut deep = up.clone();
    deep.settings = Some(CloudSettings {
        light_distance: 40.0,
        ..settings
    });
    let shallow = luma(rgb(rig.render(&deep)?.pixel(32, 18)));
    assert!(
        shallow > b,
        "a short light march misses the shadow: {shallow} vs {b}"
    );
    // Without multiple scattering the interior is darker.
    let mut single = up.clone();
    single.settings = Some(CloudSettings {
        multiple_scattering: false,
        ..settings
    });
    let s = rig.render(&single)?;
    assert!(luma(rgb(s.pixel(32, 18))) < b);
    assert_close(
        rgb(s.pixel(32, 18)),
        single.expected(32, 18, [0.0; 3], None),
        0.04,
        "single scattering",
    );
    Ok(())
}

// -- depth, sky and the other scattering passes ----------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn geometry_occludes_the_clouds_and_clouds_cover_distant_geometry() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let settings = CloudSettings {
        density: 0.01,
        ..slab()
    };
    let mut shot = Shot::new(8);
    shot.settings = Some(settings);
    shot.ambient = 0.5;
    let wall = [0.2, 0.4, 0.6];
    // A wall nearer than the layer entry (>= 100 m / 0.9 = ~110 m): unchanged.
    rig.wall_at(&shot, wall, 60.0);
    let near = rig.render(&shot)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(near.pixel(x, y)),
            wall.map(f64::from),
            0.0005,
            &format!("near wall pixel ({x},{y})"),
        );
    }
    // A wall beyond the layer is covered by it and matches the reference.
    rig.wall_at(&shot, wall, BEYOND);
    let far = rig.render(&shot)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(far.pixel(x, y)),
            shot.expected(x, y, wall.map(f64::from), Some(BEYOND)),
            0.03,
            &format!("far wall pixel ({x},{y})"),
        );
    }
    assert!(mean_luma(&far) != mean_luma(&near));
    // A wall inside the layer cuts the march short: between the two.
    rig.wall_at(&shot, wall, 220.0);
    let inside = rig.render(&shot)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(inside.pixel(x, y)),
            shot.expected(x, y, wall.map(f64::from), Some(220.0)),
            0.03,
            &format!("inside wall pixel ({x},{y})"),
        );
    }
    let (n, i, f) = (
        rgb(near.pixel(32, 18))[0],
        rgb(inside.pixel(32, 18))[0],
        rgb(far.pixel(32, 18))[0],
    );
    assert!(
        (n - i).abs() > 0.001 && (i - f).abs() > 0.001,
        "partial coverage differs from none and from full: {n} {i} {f}"
    );
    // The clouds on the sky: no wall, black sky, the ambient light only.
    rig.set_wall(None);
    let sky = rig.render(&shot)?;
    assert_close(
        rgb(sky.pixel(32, 18)),
        shot.expected(32, 18, [0.0; 3], None),
        0.03,
        "sky pixel",
    );
    assert!(luma(rgb(sky.pixel(32, 18))) > 0.05);
    // Rays that never meet the layer (looking down) stay black.
    let mut down = shot.clone();
    down.pitch = -0.8;
    assert!(rig.render(&down)?.pixels.iter().all(|p| p[..3] == [0.0; 3]));
    Ok(())
}

/// The atmosphere sky and the aerial perspective are already in the scene
/// colour when the clouds are composited, and the fog is applied over the
/// clouds afterwards.
#[test]
#[ignore = "requires a GPU adapter"]
fn clouds_composite_over_sky_and_aerial_perspective_and_under_fog() -> Result<()> {
    let mut rig = Rig::new(true, true)?;
    let settings = CloudSettings {
        density: 0.008,
        ..lumpy()
    };
    let mut shot = Shot::new(9);
    shot.settings = Some(settings);
    shot.sun = true;
    shot.atmosphere = true;
    shot.ambient = 0.2;
    let wall = [0.4, 0.4, 0.4];
    for (label, z) in [("sky", None), ("far wall", Some(BEYOND))] {
        match z {
            Some(z) => rig.wall_at(&shot, wall, z),
            None => rig.set_wall(None),
        }
        // The atmosphere alone: sky radiance or aerial-perspective geometry.
        let mut plain = shot.clone();
        plain.clouds = false;
        let backdrop = rig.render(&plain)?;
        let with_clouds = rig.render(&shot)?;
        let mut sensitive = 0;
        for (x, y) in SAMPLES {
            let scene = rgb(backdrop.pixel(x, y));
            assert!(luma(scene) > 0.0, "{label}: the backdrop is lit");
            assert_close(
                rgb(with_clouds.pixel(x, y)),
                shot.expected(x, y, scene, z),
                0.04,
                &format!("{label} over atmosphere pixel ({x},{y})"),
            );
            // The clouds change the backdrop pixel.
            if misses(rgb(with_clouds.pixel(x, y)), scene, 0.04) {
                sensitive += 1;
            }
        }
        assert!(sensitive >= 3, "{label}: clouds visibly cover the backdrop");

        // Fog afterwards: the fogged result is the fog formula applied to the
        // cloud-composited pixel, not the clouds applied to fogged geometry.
        let mut fogged_shot = shot.clone();
        fogged_shot.fog = Some(FogSettings {
            density: 0.002,
            height_falloff: 0.0,
            albedo: [0.9; 3],
            anisotropy: 0.0,
            sky_distance: 3000.0,
            ..Default::default()
        });
        let foggy = rig.render(&fogged_shot)?;
        let mut differs = 0;
        for (x, y) in SAMPLES {
            let fog_distance = match z {
                Some(z) => shot.wall_distance(x, y, z),
                None => 3000.0,
            };
            let after = fogged(
                &fogged_shot.fog_frame(),
                shot.dir(x, y),
                fog_distance,
                rgb(with_clouds.pixel(x, y)),
                shot.sun_light(),
                [0.2; 3],
            );
            assert_close(
                rgb(foggy.pixel(x, y)),
                after,
                0.03,
                &format!("{label} fog over clouds pixel ({x},{y})"),
            );
            // The opposite order (fog first, clouds on top) is a different
            // image.
            let before = shot.expected(
                x,
                y,
                fogged(
                    &fogged_shot.fog_frame(),
                    shot.dir(x, y),
                    fog_distance,
                    rgb(backdrop.pixel(x, y)),
                    shot.sun_light(),
                    [0.2; 3],
                ),
                z,
            );
            if misses(before, after, 0.03) {
                differs += 1;
            }
        }
        assert!(differs >= 2, "{label}: the order is observable ({differs})");
    }
    Ok(())
}

// -- wind ------------------------------------------------------------------

/// Sum of squared differences between two rows with `b` shifted right by `s`.
fn shifted_error(a: &[f64], b: &[f64], s: i32) -> f64 {
    let n = a.len() as i32;
    let (lo, hi) = (s.max(0) + 12, n + s.min(0) - 12);
    (lo..hi)
        .map(|x| (a[x as usize] - b[(x - s) as usize]).powi(2))
        .sum::<f64>()
        / f64::from(hi - lo)
}

#[test]
#[ignore = "requires a GPU adapter"]
fn wind_advects_the_clouds_with_the_scene_clock() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let wall = [0.5f32; 3];
    let wind = [40.0f32, 0.0, 0.0];
    let lumps = || CloudSettings {
        noise_octaves: 2,
        ..lumpy()
    };
    let mut shot = Shot::new(10);
    shot.settings = Some(CloudSettings { wind, ..lumps() });
    rig.wall_at(&shot, wall, BEYOND);
    let at_zero = rig.render(&shot)?;

    // Time moves the pattern.
    let mut later = shot.clone();
    later.time = 1.0;
    let moved = rig.render(&later)?;
    assert!(max_difference(&at_zero, &moved) > 0.01, "the pattern moved");

    // Exact symmetry: advecting by wind * dt equals moving the camera by
    // -wind * dt.
    let mut stand_in = shot.clone();
    stand_in.settings = Some(lumps());
    stand_in.time = 0.0;
    stand_in.origin = [-40.0, 0.0, 0.0];
    let equivalent = rig.render(&stand_in)?;
    assert!(
        max_difference(&moved, &equivalent) < 5e-4,
        "wind over time equals a camera shifted against it: {}",
        max_difference(&moved, &equivalent)
    );
    // The reverse wind is a different image: direction matters.
    let mut reversed = later.clone();
    reversed.settings = Some(CloudSettings {
        wind: [-40.0, 0.0, 0.0],
        ..lumps()
    });
    let backwards = rig.render(&reversed)?;
    assert!(max_difference(&moved, &backwards) > 0.01);

    // Direction on screen: a wind toward +x slides the pattern toward the
    // right edge, a wind toward -x toward the left.
    let row =
        |image: &HdrImage| -> Vec<f64> { (0..65).map(|x| rgb(image.pixel(x, 18))[0]).collect() };
    let best = |image: &HdrImage| {
        let (base, now) = (row(&at_zero), row(image));
        (-10..=10)
            .min_by(|a, b| {
                shifted_error(&now, &base, *a)
                    .partial_cmp(&shifted_error(&now, &base, *b))
                    .unwrap()
            })
            .unwrap()
    };
    let right = best(&moved);
    let left = best(&backwards);
    eprintln!("pattern shift: wind +x {right} px, wind -x {left} px");
    assert!(right >= 1, "+x wind moves the pattern right: {right}");
    assert!(left <= -1, "-x wind moves the pattern left: {left}");

    // No wind: the clock changes nothing, to the bit.
    let mut calm = shot.clone();
    calm.settings = Some(lumps());
    let a = rig.render(&calm)?;
    calm.time = 1.0e6;
    let b = rig.render(&calm)?;
    assert_eq!(a.pixels, b.pixels, "zero wind is static");
    // And the clock is the only input: the same time is the same image.
    assert_eq!(rig.render(&later)?.pixels, moved.pixels);
    Ok(())
}

// -- lifetime --------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn disabled_or_unconfigured_clouds_are_identical_to_a_pipeline_without_them() -> Result<()> {
    let mut baseline_rig = Rig::new(false, false)?;
    let mut rig = Rig::new(true, false)?;
    let mut shot = Shot::new(11);
    shot.sun = true;
    shot.ambient = 0.2;
    shot.settings = Some(lumpy());
    for rig in [&baseline_rig, &rig] {
        rig.wall_at(&shot, [0.4, 0.5, 0.6], BEYOND);
    }
    let baseline = baseline_rig.render(&Shot {
        clouds: false,
        ..shot.clone()
    })?;
    let clouded = rig.render(&shot)?;
    assert!(max_difference(&clouded, &baseline) > 0.05);
    // Settings present but the feature disabled in the view.
    let mut off = shot.clone();
    off.clouds = false;
    assert_eq!(
        rig.render(&off)?.pixels,
        baseline.pixels,
        "disabled feature"
    );
    // Feature enabled but the view has no settings.
    let mut none = shot.clone();
    none.settings = None;
    assert_eq!(rig.render(&none)?.pixels, baseline.pixels, "no settings");
    // Zero density or coverage change nothing either.
    for thin in [
        CloudSettings {
            density: 0.0,
            ..lumpy()
        },
        CloudSettings {
            coverage: 0.0,
            ..lumpy()
        },
    ] {
        let mut s = shot.clone();
        s.settings = Some(thin);
        assert_eq!(rig.render(&s)?.pixels, baseline.pixels, "invisible clouds");
    }
    // Re-enabling restores the clouded image exactly.
    assert_eq!(rig.render(&shot)?.pixels, clouded.pixels);
    // An empty, uncovered sky really renders black.
    baseline_rig.set_wall(None);
    assert!(
        baseline_rig
            .render(&Shot {
                clouds: false,
                ..shot.clone()
            })?
            .pixels
            .iter()
            .all(|p| p[..3] == [0.0; 3])
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn resize_two_views_and_forget_view() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let wall = [0.5f32; 3];
    let mut a = Shot::new(20);
    a.settings = Some(CloudSettings {
        density: 0.01,
        ..slab()
    });
    let mut b = Shot::new(21);
    b.size = [41, 29];
    b.settings = Some(CloudSettings {
        density: 0.002,
        ..slab()
    });
    rig.wall_at(&a, wall, BEYOND);
    let (ia, ib) = (rig.render(&a)?, rig.render(&b)?);
    assert_eq!((ia.size, ib.size), ([65, 37], [41, 29]));
    assert_close(
        rgb(ia.pixel(20, 12)),
        a.expected(20, 12, wall.map(f64::from), Some(BEYOND)),
        0.02,
        "view A",
    );
    // Interleaved frames keep their settings apart.
    assert_eq!(rig.render(&a)?.pixels, ia.pixels);
    assert_eq!(rig.clouds.view_count(), 2);

    // Resize view A: the targets recreate and the clouds follow.
    a.size = [97, 53];
    rig.wall_at(&a, wall, BEYOND);
    let resized = rig.render(&a)?;
    assert_eq!(resized.size, [97, 53]);
    for (x, y) in [(48, 20), (5, 45), (90, 3)] {
        assert_close(
            rgb(resized.pixel(x, y)),
            a.expected(x, y, wall.map(f64::from), Some(BEYOND)),
            0.02,
            &format!("resized A ({x},{y})"),
        );
    }

    // Forgetting a view drops its cloud state with its targets.
    rig.scene.forget_view(a.id);
    assert_eq!(rig.clouds.view_count(), 1);
    assert!(rig.clouds.get(a.id)?.is_none());
    // Rendering the forgotten id again without settings draws no clouds.
    let mut gone = a.clone();
    gone.settings = None;
    let plain = rig.render(&gone)?;
    assert_close(
        rgb(plain.pixel(48, 20)),
        wall.map(f64::from),
        0.0005,
        "forgotten view has no clouds",
    );
    Ok(())
}

/// Projection jitter (render pixels) and the relative tolerance of its checks.
const JITTER: [f32; 2] = [0.95, -0.9];
const JITTER_TOLERANCE: f64 = 0.02;

/// A jittered projection moves the rasterized image and the reconstructed rays
/// together: the clouds still match the reference for the shifted ray of each
/// pixel.
#[test]
#[ignore = "requires a GPU adapter"]
fn jittered_views_march_the_shifted_ray() -> Result<()> {
    let mut rig = Rig::new(true, false)?;
    let wall = [0.5f32; 3];
    let mut shot = Shot::new(40);
    shot.pitch = 0.9;
    shot.settings = Some(CloudSettings {
        density: 0.02,
        ..slab()
    });
    shot.jitter = JITTER;
    rig.wall_at(&shot, wall, BEYOND);
    let image = rig.render(&shot)?;
    let unjittered = Shot {
        jitter: [0.0; 2],
        ..shot.clone()
    };
    assert_ne!(rig.render(&unjittered)?.pixels, image.pixels);
    let [w, h] = shot.size;
    let mut sensitive = 0;
    for y in 0..h {
        for x in 0..w {
            let want = shot.expected(x, y, wall.map(f64::from), Some(BEYOND));
            assert_close(
                rgb(image.pixel(x, y)),
                want,
                JITTER_TOLERANCE,
                &format!("jittered cloud pixel ({x},{y})"),
            );
            // Pixels where ignoring the jitter would leave the tolerance.
            if misses(
                unjittered.expected(x, y, wall.map(f64::from), Some(BEYOND)),
                want,
                JITTER_TOLERANCE,
            ) {
                sensitive += 1;
            }
        }
    }
    assert!(sensitive >= 20, "only {sensitive} jitter-sensitive pixels");
    Ok(())
}
