//! Real-adapter readbacks of `engine.fog`. Pixels are compared with the CPU
//! reference in `fog::model` (same medium, `f64`): pure height fog against
//! the closed-form transmittance, in-scatter against the phase function and
//! the sun direction, and the wind against an exact symmetry (advecting the
//! noise equals moving the camera against the wind). An opaque wall at a chosen
//! view depth provides the geometry. Run:
//! `cargo test -p engine-render-pipeline --test gpu_fog -- --include-ignored --test-threads=1 --nocapture`.

use anyhow::{Context, Result};
use engine_render_pipeline::fog::{
    FOG_FEATURE, FogControls, FogFrame, FogSettings, fogged, height_transmittance,
    register_features,
};
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
    settings: Option<FogSettings>,
    fog: bool,
    sun: bool,
    ortho: bool,
    /// Sub-pixel projection jitter in render pixels (x right, y down).
    jitter: [f32; 2],
}

/// Pure height fog without lights: pixel = wall * T.
fn clear_fog() -> FogSettings {
    FogSettings {
        density: 0.01,
        height_falloff: 0.05,
        base_height: 0.0,
        albedo: [1.0; 3],
        anisotropy: 0.0,
        sky_distance: 0.0,
        ..Default::default()
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
            pitch: 0.0,
            sun_elevation: 0.0,
            sun_azimuth: 0.0,
            lux: SUN_LUX,
            ambient: 0.0,
            time: 0.0,
            settings: Some(clear_fog()),
            fog: true,
            sun: false,
            ortho: false,
            jitter: [0.0; 2],
        }
    }

    fn projection(&self) -> Mat4 {
        if self.ortho {
            let half_height = 5.0 * self.size[1] as f32 / self.size[0] as f32;
            return glam::camera::rh::proj::directx::orthographic(
                -5.0,
                5.0,
                -half_height,
                half_height,
                NEAR,
                100.0,
            );
        }
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

    fn frame(&self) -> FogFrame {
        FogFrame::new(
            self.settings.expect("shot has settings"),
            self.absolute(),
            self.time,
        )
        .unwrap()
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

    /// World direction of the centre of pixel (x, y).
    fn dir(&self, x: u32, y: u32) -> [f64; 3] {
        if self.ortho {
            let q = DQuat::from_euler(
                EulerRot::YXZ,
                f64::from(self.yaw),
                f64::from(self.pitch),
                0.0,
            );
            return (q * DVec3::new(0.0, 0.0, -1.0)).to_array();
        }
        let q = DQuat::from_euler(
            EulerRot::YXZ,
            f64::from(self.yaw),
            f64::from(self.pitch),
            0.0,
        );
        (q * self.local_dir(x, y)).to_array()
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
        if self.ortho {
            return f64::from(z);
        }
        f64::from(z) / -self.local_dir(x, y).z
    }

    /// Depth value of a wall at view depth `z`.
    fn wall_depth(&self, z: f32) -> f32 {
        let clip = self.projection() * glam::Vec4::new(0.0, 0.0, -z, 1.0);
        clip.z / clip.w
    }

    /// CPU reference pixel: the wall (colour `scene`, view depth `z`) or the
    /// sky at `settings.sky_distance` when `z` is `None`.
    fn expected(&self, x: u32, y: u32, scene: [f64; 3], z: Option<f32>) -> [f64; 3] {
        let frame = self.frame();
        let distance = match z {
            Some(z) => self.wall_distance(x, y, z),
            None => f64::from(frame.settings.sky_distance),
        };
        fogged(
            &frame,
            self.dir(x, y),
            distance,
            scene,
            self.sun_light(),
            [f64::from(self.ambient); 3],
        )
    }
}

struct Rig {
    gpu: GpuContext,
    scene: ScenePipeline,
    controls: FogControls,
    wall: Wall,
    fog: bool,
}

impl Rig {
    fn new(fog: bool) -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {}", gpu.adapter.get_info().name);
        let mut scene = ScenePipeline::new(gpu.clone())?;
        let controls = FogControls::default();
        let wall: Wall = Arc::new(Mutex::new(None));
        let mut registry = FeatureRegistry::default();
        registry.register(WallProducer {
            wall: wall.clone(),
            layout: None,
            pipeline: None,
        })?;
        if fog {
            register_features(&mut registry, &controls)?;
        }
        scene.add_features(registry)?;
        Ok(Self {
            gpu,
            scene,
            controls,
            wall,
            fog,
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
        if self.fog {
            self.controls.set(
                shot.id,
                shot.settings
                    .map(|_| shot.frame())
                    .map(Some)
                    .unwrap_or(None),
            )?;
            view.features.set(FOG_FEATURE, shot.fog);
        }
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fog test output"),
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
        let tolerance = relative * expected[k].abs() + 2.0e-4;
        assert!(
            (actual[k] - expected[k]).abs() <= tolerance,
            "{what}: channel {k} {actual:?} vs reference {expected:?}"
        );
    }
}

/// True when `actual` is outside the `assert_close` tolerance of `expected`.
fn misses(actual: [f64; 3], expected: [f64; 3], relative: f64) -> bool {
    (0..3).any(|k| (actual[k] - expected[k]).abs() > relative * expected[k].abs() + 2.0e-4)
}

fn all_pixels(image: &HdrImage) -> impl Iterator<Item = [f32; 4]> + '_ {
    image.pixels.iter().copied()
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

// -- density ---------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn density_matches_the_analytic_height_transmittance() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.5f32; 3];
    let mut centre = Vec::new();
    for (density, falloff, pitch) in [
        (0.005, 0.05, 0.0f32),
        (0.01, 0.05, 0.0),
        (0.02, 0.05, 0.0),
        (0.02, 0.05, 0.3),
        (0.02, 0.05, -0.3),
        (0.02, 0.2, 0.15),
        (0.02, 0.0, 0.0),
    ] {
        let mut shot = Shot::new(1);
        shot.pitch = pitch;
        shot.settings = Some(FogSettings {
            density,
            height_falloff: falloff,
            ..clear_fog()
        });
        rig.wall_at(&shot, wall, 100.0);
        let image = rig.render(&shot)?;
        for (x, y) in SAMPLES {
            let dir = shot.dir(x, y);
            let t = height_transmittance(
                f64::from(density),
                f64::from(falloff),
                f64::from(shot.camera[1]),
                0.0,
                dir[1],
                shot.wall_distance(x, y, 100.0),
            );
            let actual = rgb(image.pixel(x, y));
            assert_close(
                actual,
                [0.5 * t; 3],
                0.01,
                &format!("density {density} falloff {falloff} pitch {pitch} pixel ({x},{y})"),
            );
        }
        if pitch == 0.0 && falloff == 0.05 {
            centre.push(rgb(image.pixel(32, 18))[0]);
        }
    }
    assert!(
        centre[0] > centre[1] && centre[1] > centre[2],
        "more density, lower transmittance: {centre:?}"
    );
    // Uniform fog is plain Beer-Lambert at the centre pixel (100 m).
    let mut uniform = Shot::new(2);
    uniform.settings = Some(FogSettings {
        density: 0.02,
        height_falloff: 0.0,
        ..clear_fog()
    });
    rig.wall_at(&uniform, wall, 100.0);
    let pixel = rgb(rig.render(&uniform)?.pixel(32, 18))[0];
    assert!((pixel - 0.5 * (-2.0f64).exp()).abs() < 0.01 * pixel + 2e-4);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn base_height_and_camera_altitude_move_the_fog_layer() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.5f32; 3];
    let mut shot = Shot::new(3);
    shot.settings = Some(FogSettings {
        density: 0.03,
        height_falloff: 0.1,
        ..clear_fog()
    });
    rig.wall_at(&shot, wall, 80.0);
    let low = rgb(rig.render(&shot)?.pixel(32, 18))[0];
    // Camera 60 m above the layer looking horizontally: far thinner fog.
    shot.camera[1] = 62.0;
    let high = rgb(rig.render(&shot)?.pixel(32, 18))[0];
    // Raising the base height by the same amount restores the original fog.
    shot.settings = Some(FogSettings {
        base_height: 60.0,
        ..shot.settings.unwrap()
    });
    let restored = rgb(rig.render(&shot)?.pixel(32, 18))[0];
    assert!(high > 2.0 * low, "higher camera, thinner fog: {low} {high}");
    assert!(
        (restored - low).abs() < 0.01 * low,
        "base height shifts the layer: {low} vs {restored}"
    );
    // A floating origin is the same absolute altitude: same image.
    shot.camera[1] = 2.0;
    shot.origin = [0.0, 60.0, 0.0];
    let rebased = rgb(rig.render(&shot)?.pixel(32, 18))[0];
    assert!(
        (rebased - low).abs() < 0.01 * low,
        "origin counts toward the altitude: {low} vs {rebased}"
    );
    Ok(())
}

// -- sun -------------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn in_scatter_is_brighter_toward_the_sun_and_follows_it() -> Result<()> {
    let mut rig = Rig::new(true)?;
    // Dense uniform fog in front of a black wall: the pixel is the source.
    let settings = FogSettings {
        density: 0.03,
        height_falloff: 0.0,
        anisotropy: 0.7,
        ..clear_fog()
    };
    let mut shot = Shot::new(4);
    shot.settings = Some(settings);
    shot.sun = true;
    rig.wall_at(&shot, [0.0; 3], 150.0);

    // The sun straight ahead, then behind the camera.
    shot.sun_azimuth = 0.0;
    let toward = rig.render(&shot)?;
    shot.sun_azimuth = 180.0;
    let away = rig.render(&shot)?;
    let (a, b) = (rgb(toward.pixel(32, 18)), rgb(away.pixel(32, 18)));
    assert!(
        luma(a) > 5.0 * luma(b) && luma(b) > 0.0,
        "forward scattering: toward {a:?}, away {b:?}"
    );
    for (name, sun_azimuth, image) in [("toward", 0.0, &toward), ("away", 180.0, &away)] {
        let shot = Shot {
            sun_azimuth,
            ..shot.clone()
        };
        for (x, y) in SAMPLES {
            assert_close(
                rgb(image.pixel(x, y)),
                shot.expected(x, y, [0.0; 3], Some(150.0)),
                0.015,
                &format!("sun {name} pixel ({x},{y})"),
            );
        }
    }

    // A sun to the right brightens the right of the image, to the left the
    // left: the brightest column follows the light.
    let column = |image: &HdrImage, x: u32| luma(rgb(image.pixel(x, 18)));
    shot.sun_azimuth = 60.0;
    let right = rig.render(&shot)?;
    shot.sun_azimuth = -60.0;
    let left = rig.render(&shot)?;
    assert!(column(&right, 60) > 1.5 * column(&right, 4));
    assert!(column(&left, 4) > 1.5 * column(&left, 60));
    // Mirror symmetry of the two renders.
    for x in [4, 20, 32, 44, 60] {
        let mirrored = 64 - x;
        let (r, l) = (column(&right, x), column(&left, mirrored));
        assert!(
            (r - l).abs() < 0.01 * r + 2e-4,
            "mirror at column {x}: {r} vs {l}"
        );
    }
    // Raising the sun changes the image again, in the CPU reference's way.
    shot.sun_azimuth = 0.0;
    shot.sun_elevation = 50.0;
    let raised = rig.render(&shot)?;
    assert!(luma(rgb(raised.pixel(32, 18))) < 0.5 * luma(a));
    assert_close(
        rgb(raised.pixel(32, 18)),
        shot.expected(32, 18, [0.0; 3], Some(150.0)),
        0.015,
        "raised sun",
    );

    // The intensity multiplier scales the sun term; no light means no sun term.
    shot.sun_elevation = 0.0;
    shot.settings = Some(FogSettings {
        sun_intensity: 2.0,
        ..settings
    });
    let doubled = rgb(rig.render(&shot)?.pixel(32, 18));
    assert_close(
        doubled,
        [a[0] * 2.0, a[1] * 2.0, a[2] * 2.0],
        0.015,
        "2x sun",
    );
    shot.sun = false;
    assert!(all_pixels(&rig.render(&shot)?).all(|p| p[..3] == [0.0; 3]));
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn ambient_and_albedo_tint_the_in_scatter() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let settings = FogSettings {
        density: 0.03,
        height_falloff: 0.0,
        albedo: [1.0, 0.5, 0.25],
        ..clear_fog()
    };
    let mut shot = Shot::new(5);
    shot.settings = Some(settings);
    shot.ambient = 0.2;
    rig.wall_at(&shot, [0.0; 3], 150.0);
    let image = rig.render(&shot)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(image.pixel(x, y)),
            shot.expected(x, y, [0.0; 3], Some(150.0)),
            0.015,
            &format!("ambient pixel ({x},{y})"),
        );
    }
    let centre = rgb(image.pixel(32, 18));
    assert!(centre[0] > 1.8 * centre[1] && centre[1] > 1.8 * centre[2]);
    // The ambient multiplier scales it.
    shot.settings = Some(FogSettings {
        ambient_intensity: 0.5,
        ..settings
    });
    let half = rgb(rig.render(&shot)?.pixel(32, 18));
    assert_close(half, centre.map(|v| v * 0.5), 0.015, "half ambient");
    Ok(())
}

// -- depth and sky ---------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter"]
fn farther_surfaces_are_more_fogged_and_near_ones_are_untouched() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.6, 0.4, 0.2];
    let mut shot = Shot::new(6);
    shot.settings = Some(FogSettings {
        density: 0.004,
        height_falloff: 0.0,
        albedo: [0.5; 3],
        ..clear_fog()
    });
    shot.ambient = 0.4;
    let mut gaps = Vec::new();
    for z in [1.5f32, 20.0, 100.0, 400.0] {
        rig.wall_at(&shot, wall, z);
        let image = rig.render(&shot)?;
        let actual = rgb(image.pixel(32, 18));
        assert_close(
            actual,
            shot.expected(32, 18, wall.map(f64::from), Some(z)),
            0.015,
            &format!("wall at {z} m"),
        );
        gaps.push(
            (0..3)
                .map(|k| (actual[k] - f64::from(wall[k])).abs())
                .sum::<f64>(),
        );
    }
    assert!(
        gaps.windows(2).all(|g| g[0] < g[1]),
        "fog grows with distance: {gaps:?}"
    );
    // Near geometry (1.5 m through density 0.004) keeps its colour within 1%.
    assert!(gaps[0] < 0.01 * 1.2, "near wall unchanged: {gaps:?}");
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sky_pixels_are_fogged_only_up_to_the_sky_distance() -> Result<()> {
    let mut rig = Rig::new(true)?;
    rig.set_wall(None);
    let mut shot = Shot::new(7);
    shot.ambient = 0.3;
    shot.sun = true;
    shot.sun_elevation = 30.0;
    shot.pitch = 0.25;
    // A zero sky distance leaves the empty sky exactly as the baseline.
    shot.settings = Some(FogSettings {
        density: 0.01,
        sky_distance: 0.0,
        ..clear_fog()
    });
    let untouched = rig.render(&shot)?;
    assert!(all_pixels(&untouched).all(|p| p[..3] == [0.0; 3]));
    // A sky distance integrates the fog along the view ray.
    shot.settings = Some(FogSettings {
        density: 0.01,
        height_falloff: 0.05,
        anisotropy: 0.5,
        sky_distance: 8000.0,
        ..clear_fog()
    });
    let fogged_sky = rig.render(&shot)?;
    for (x, y) in SAMPLES {
        let actual = rgb(fogged_sky.pixel(x, y));
        let expected = shot.expected(x, y, [0.0; 3], None);
        assert!(luma(expected) > 1e-3, "reference ({x},{y}) is lit");
        assert_close(actual, expected, 0.015, &format!("sky pixel ({x},{y})"));
    }
    // The ray near the horizon crosses more fog than the ray looking up.
    let top = luma(rgb(fogged_sky.pixel(32, 1)));
    let low = luma(rgb(fogged_sky.pixel(32, 35)));
    assert!(low > top * 1.05, "denser toward the ground: {low} vs {top}");
    // A longer sky distance never fogs less.
    shot.settings = Some(FogSettings {
        sky_distance: 100.0,
        ..shot.settings.unwrap()
    });
    let short = rig.render(&shot)?;
    assert!(luma(rgb(short.pixel(32, 35))) < luma(rgb(fogged_sky.pixel(32, 35))));
    // Geometry hides the sky: a wall in front is fogged by its own distance.
    rig.wall_at(&shot, [0.3; 3], 25.0);
    let walled = rig.render(&shot)?;
    assert_close(
        rgb(walled.pixel(32, 18)),
        shot.expected(32, 18, [0.3; 3], Some(25.0)),
        0.015,
        "wall hides the sky",
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn orthographic_views_fog_along_the_view_axis() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(8);
    shot.ortho = true;
    shot.pitch = 0.3;
    shot.settings = Some(FogSettings {
        density: 0.03,
        height_falloff: 0.0,
        ..clear_fog()
    });
    rig.wall_at(&shot, [0.5; 3], 40.0);
    let image = rig.render(&shot)?;
    assert!(all_pixels(&image).all(|p| p.iter().all(|v| v.is_finite())));
    // Parallel rays and one axis distance: a uniform image at the analytic value.
    let want = 0.5 * (-0.03f64 * 40.0).exp();
    for (x, y) in SAMPLES {
        assert_close(rgb(image.pixel(x, y)), [want; 3], 0.01, "ortho wall");
    }
    Ok(())
}

// -- noise and wind --------------------------------------------------------

fn noisy() -> FogSettings {
    FogSettings {
        density: 0.02,
        height_falloff: 0.0,
        noise_amount: 0.9,
        noise_scale: 12.0,
        noise_distance: 90.0,
        noise_steps: 32,
        noise_seed: 5,
        ..clear_fog()
    }
}

fn max_difference(a: &HdrImage, b: &HdrImage) -> f32 {
    a.pixels
        .iter()
        .zip(&b.pixels)
        .flat_map(|(p, q)| (0..3).map(move |k| (p[k] - q[k]).abs()))
        .fold(0.0, f32::max)
}

#[test]
#[ignore = "requires a GPU adapter"]
fn noise_matches_the_cpu_march_and_varies_across_the_view() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.5f32; 3];
    let mut shot = Shot::new(9);
    shot.settings = Some(noisy());
    rig.wall_at(&shot, wall, 60.0);
    let image = rig.render(&shot)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(image.pixel(x, y)),
            shot.expected(x, y, wall.map(f64::from), Some(60.0)),
            0.03,
            &format!("noise pixel ({x},{y})"),
        );
    }
    // The pattern is visible: noise off gives a smoother image and a different one.
    let mut plain = shot.clone();
    plain.settings = Some(FogSettings {
        noise_amount: 0.0,
        ..noisy()
    });
    let smooth = rig.render(&plain)?;
    // Roughness: mean second difference along a row. The smooth image only
    // bends with the view distance; the noise adds cell-sized structure.
    let roughness = |image: &HdrImage| {
        let row: Vec<f64> = (0..65).map(|x| rgb(image.pixel(x, 18))[0]).collect();
        row.windows(3)
            .map(|w| (w[0] - 2.0 * w[1] + w[2]).abs())
            .sum::<f64>()
            / 63.0
    };
    assert!(
        roughness(&image) > 3.0 * roughness(&smooth),
        "noise shows: {} vs {}",
        roughness(&image),
        roughness(&smooth)
    );
    assert!(max_difference(&image, &smooth) > 0.01);
    // Beyond the noise distance the remainder is plain height fog.
    rig.wall_at(&shot, wall, 400.0);
    let far = rig.render(&shot)?;
    assert_close(
        rgb(far.pixel(32, 18)),
        shot.expected(32, 18, wall.map(f64::from), Some(400.0)),
        0.03,
        "wall beyond the noise distance",
    );
    // A different seed is a different pattern.
    rig.wall_at(&shot, wall, 60.0);
    let mut reseeded = shot.clone();
    reseeded.settings = Some(FogSettings {
        noise_seed: 6,
        ..noisy()
    });
    assert!(max_difference(&image, &rig.render(&reseeded)?) > 0.01);
    // A camera far from the origin (huge absolute coordinates and a long
    // time) still matches the CPU reference of the same absolute frame.
    let mut distant = shot.clone();
    distant.origin = [3.0e9, 0.0, -7.0e9];
    distant.time = 123_456.0;
    distant.settings = Some(FogSettings {
        wind: [3.0, 0.0, -2.0],
        ..noisy()
    });
    let image = rig.render(&distant)?;
    for (x, y) in SAMPLES {
        assert_close(
            rgb(image.pixel(x, y)),
            distant.expected(x, y, wall.map(f64::from), Some(60.0)),
            0.03,
            &format!("distant pixel ({x},{y})"),
        );
    }
    Ok(())
}

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
fn wind_advects_the_noise_with_the_scene_clock() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.5f32; 3];
    let wind = [4.0f32, 0.0, 0.0];
    let mut shot = Shot::new(10);
    shot.settings = Some(FogSettings { wind, ..noisy() });
    rig.wall_at(&shot, wall, 60.0);
    let at_zero = rig.render(&shot)?;

    // Time moves the pattern: one second of wind is a different image.
    let mut later = shot.clone();
    later.time = 1.0;
    let moved = rig.render(&later)?;
    assert!(
        max_difference(&at_zero, &moved) > 0.005,
        "the pattern moved"
    );

    // Exact symmetry: advecting by wind * dt equals moving the camera by
    // -wind * dt (the noise field is only ever sampled at p - wind * t).
    let mut stand_in = shot.clone();
    stand_in.settings = Some(FogSettings {
        wind: [0.0; 3],
        ..noisy()
    });
    stand_in.time = 0.0;
    stand_in.origin = [-4.0, 0.0, 0.0];
    let equivalent = rig.render(&stand_in)?;
    assert!(
        max_difference(&moved, &equivalent) < 2e-4,
        "wind over time equals a camera shifted against it: {}",
        max_difference(&moved, &equivalent)
    );
    // The reverse wind is a different image: direction matters.
    let mut reversed = later.clone();
    reversed.settings = Some(FogSettings {
        wind: [-4.0, 0.0, 0.0],
        ..noisy()
    });
    assert!(max_difference(&moved, &rig.render(&reversed)?) > 0.005);

    // Direction on screen: a wind toward +x slides the pattern toward the right
    // edge, a wind toward -x toward the left.
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
    let left = best(&rig.render(&reversed)?);
    eprintln!("pattern shift: wind +x {right} px, wind -x {left} px");
    assert!(right >= 1, "+x wind moves the pattern right: {right}");
    assert!(left <= -1, "-x wind moves the pattern left: {left}");

    // No wind: the clock changes nothing, to the bit.
    let mut calm = shot.clone();
    calm.settings = Some(noisy());
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
fn disabled_or_unconfigured_fog_is_identical_to_a_pipeline_without_it() -> Result<()> {
    let mut baseline_rig = Rig::new(false)?;
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(11);
    shot.sun = true;
    shot.ambient = 0.2;
    shot.settings = Some(FogSettings {
        sky_distance: 5000.0,
        ..clear_fog()
    });
    for rig in [&baseline_rig, &rig] {
        rig.wall_at(&shot, [0.4, 0.5, 0.6], 70.0);
    }
    let baseline = baseline_rig.render(&Shot {
        fog: false,
        ..shot.clone()
    })?;
    // Fogged: visibly different from the baseline.
    let fogged_image = rig.render(&shot)?;
    assert!(max_difference(&fogged_image, &baseline) > 0.05);
    // Settings present but the feature disabled in the view.
    let mut off = shot.clone();
    off.fog = false;
    assert_eq!(
        rig.render(&off)?.pixels,
        baseline.pixels,
        "disabled feature"
    );
    // Feature enabled but the view has no settings.
    let mut none = shot.clone();
    none.settings = None;
    assert_eq!(rig.render(&none)?.pixels, baseline.pixels, "no settings");
    // Zero density changes nothing either.
    let mut thin = shot.clone();
    thin.settings = Some(FogSettings {
        density: 0.0,
        ..shot.settings.unwrap()
    });
    assert_eq!(rig.render(&thin)?.pixels, baseline.pixels, "zero density");
    // Re-enabling restores the fogged image exactly.
    assert_eq!(rig.render(&shot)?.pixels, fogged_image.pixels);
    // An empty, unfogged sky really renders black.
    baseline_rig.set_wall(None);
    assert!(
        all_pixels(&baseline_rig.render(&Shot {
            fog: false,
            ..shot.clone()
        })?)
        .all(|p| p[..3] == [0.0; 3])
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn resize_two_views_and_forget_view() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.5f32; 3];
    rig.set_wall(None);
    let mut a = Shot::new(20);
    a.settings = Some(FogSettings {
        density: 0.02,
        height_falloff: 0.0,
        ..clear_fog()
    });
    let mut b = Shot::new(21);
    b.size = [41, 29];
    b.settings = Some(FogSettings {
        density: 0.005,
        height_falloff: 0.0,
        ..clear_fog()
    });
    rig.wall_at(&a, wall, 80.0);
    let (ia, ib) = (rig.render(&a)?, rig.render(&b)?);
    assert_eq!((ia.size, ib.size), ([65, 37], [41, 29]));
    // The wall depth value differs per projection, so a stays correct only
    // for view a; compare each against its reference.
    assert_close(
        rgb(ia.pixel(20, 12)),
        a.expected(20, 12, wall.map(f64::from), Some(80.0)),
        0.015,
        "view A",
    );
    // Interleaved frames keep their settings apart.
    assert_eq!(rig.render(&a)?.pixels, ia.pixels);
    assert_eq!(rig.controls.view_count(), 2);

    // Resize view A: the targets recreate and the fog follows.
    a.size = [97, 53];
    rig.wall_at(&a, wall, 80.0);
    let resized = rig.render(&a)?;
    assert_eq!(resized.size, [97, 53]);
    for (x, y) in [(48, 20), (5, 45), (90, 3)] {
        assert_close(
            rgb(resized.pixel(x, y)),
            a.expected(x, y, wall.map(f64::from), Some(80.0)),
            0.015,
            &format!("resized A ({x},{y})"),
        );
    }

    // Forgetting a view drops its fog state with its targets.
    rig.scene.forget_view(a.id);
    assert_eq!(rig.controls.view_count(), 1);
    assert!(rig.controls.get(a.id)?.is_none());
    // Rendering the forgotten id again without settings draws no fog.
    let mut gone = a.clone();
    gone.settings = None;
    let plain = rig.render(&gone)?;
    assert_close(
        rgb(plain.pixel(48, 20)),
        wall.map(f64::from),
        0.001,
        "forgotten view is unfogged",
    );
    Ok(())
}

/// Projection jitter (render pixels) and the relative tolerance of its checks.
const JITTER: [f32; 2] = [0.95, -0.9];
const JITTER_TOLERANCE: f64 = 0.02;

/// A jittered projection moves the rasterized image and the reconstructed rays
/// together: the fog still matches the reference for the shifted ray and
/// distance of each pixel.
#[test]
#[ignore = "requires a GPU adapter"]
fn jittered_views_fog_along_the_shifted_ray() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let wall = [0.5f32; 3];
    let mut shot = Shot::new(40);
    shot.settings = Some(FogSettings {
        density: 0.03,
        height_falloff: 0.0,
        ..clear_fog()
    });
    shot.jitter = JITTER;
    let distance = 60.0;
    rig.wall_at(&shot, wall, distance);
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
            let want = shot.expected(x, y, wall.map(f64::from), Some(distance));
            assert_close(
                rgb(image.pixel(x, y)),
                want,
                JITTER_TOLERANCE,
                &format!("jittered fog pixel ({x},{y})"),
            );
            // Pixels where ignoring the jitter would leave the tolerance.
            if misses(
                unjittered.expected(x, y, wall.map(f64::from), Some(distance)),
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
