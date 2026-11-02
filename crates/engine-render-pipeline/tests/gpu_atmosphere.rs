//! Real-adapter readbacks of `engine.atmosphere` and `engine.atmosphere.aerial`.
//! Sky pixels are compared with the CPU reference in `atmosphere::model`
//! (same integral, `f64`); aerial perspective uses an opaque wall at a chosen
//! view distance. Run:
//! `cargo test -p engine-render-pipeline --test gpu_atmosphere -- --ignored --test-threads=1 --nocapture`.

use anyhow::{Context, Result};
use engine_render_pipeline::atmosphere::{
    ATMOSPHERE_AERIAL_FEATURE, ATMOSPHERE_FEATURE, AtmosphereControls, AtmosphereFrame,
    AtmosphereSettings, Medium, aerial, register_features, sky_radiance,
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
    altitude: f64,
    yaw: f32,
    pitch: f32,
    sun_elevation: f32,
    sun_azimuth: f32,
    lux: f32,
    settings: Option<AtmosphereSettings>,
    sky: bool,
    aerial: bool,
    sun: bool,
    ortho: bool,
    /// Sub-pixel projection jitter in render pixels (x right, y down).
    jitter: [f32; 2],
}

impl Shot {
    fn new(id: u64) -> Self {
        Self {
            id: ViewId(id),
            size: [65, 37],
            altitude: 2.0,
            yaw: 0.0,
            pitch: 0.0,
            sun_elevation: 60.0,
            sun_azimuth: 0.0,
            lux: SUN_LUX,
            settings: Some(AtmosphereSettings::default()),
            sky: true,
            aerial: true,
            sun: true,
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
            Vec3::new(0.0, self.altitude as f32, 0.0),
        )
    }

    /// Unit vector toward the sun.
    fn sun(&self) -> [f64; 3] {
        let (e, a) = (
            f64::from(self.sun_elevation).to_radians(),
            f64::from(self.sun_azimuth).to_radians(),
        );
        [e.cos() * a.sin(), e.sin(), -e.cos() * a.cos()]
    }

    /// World direction of the centre of pixel (x, y), as the CPU reference sees it.
    fn dir(&self, x: u32, y: u32) -> [f64; 3] {
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

    /// Euclidean distance to a wall at view depth `z` along pixel (x, y)'s ray.
    fn wall_distance(&self, x: u32, y: u32, z: f32) -> f64 {
        f64::from(z) / -self.local_dir(x, y).z
    }

    fn irradiance(&self) -> [f64; 3] {
        let i = f64::from(self.lux) * self.settings.map_or(1.0, |s| f64::from(s.intensity));
        [i; 3]
    }

    fn medium(&self) -> Medium {
        Medium::new(&self.settings.expect("shot has settings"))
    }

    /// CPU reference radiance of a sky pixel.
    fn expected_sky(&self, x: u32, y: u32) -> [f64; 3] {
        let s = self.settings.expect("shot has settings");
        sky_radiance(
            &self.medium(),
            self.altitude,
            self.dir(x, y),
            self.sun(),
            self.irradiance(),
            0.5 * f64::from(s.sun_angular_diameter),
        )
    }

    /// Depth value of a wall whose centre pixel is `distance` metres away.
    fn wall_depth(&self, distance: f32) -> f32 {
        let clip = self.projection() * glam::Vec4::new(0.0, 0.0, -distance, 1.0);
        clip.z / clip.w
    }
}

struct Rig {
    gpu: GpuContext,
    scene: ScenePipeline,
    controls: AtmosphereControls,
    wall: Wall,
    atmosphere: bool,
}

impl Rig {
    fn new(atmosphere: bool) -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("adapter: {}", gpu.adapter.get_info().name);
        let mut scene = ScenePipeline::new(gpu.clone())?;
        let controls = AtmosphereControls::default();
        let wall: Wall = Arc::new(Mutex::new(None));
        let mut registry = FeatureRegistry::default();
        registry.register(WallProducer {
            wall: wall.clone(),
            layout: None,
            pipeline: None,
        })?;
        if atmosphere {
            register_features(&mut registry, &controls)?;
        }
        scene.add_features(registry)?;
        Ok(Self {
            gpu,
            scene,
            controls,
            wall,
            atmosphere,
        })
    }

    fn render(&mut self, shot: &Shot) -> Result<HdrImage> {
        let projection = shot.projection();
        let mut view = ViewData::new(
            shot.id,
            shot.world_from_view(),
            projection,
            shot.size,
            shot.size,
        )?;
        view.jitter = shot.jitter;
        if self.atmosphere {
            self.controls.set(
                shot.id,
                shot.settings
                    .map(|s| AtmosphereFrame::new(s, shot.altitude))
                    .transpose()?,
            )?;
            view.features.set(ATMOSPHERE_FEATURE, shot.sky);
            view.features.set(ATMOSPHERE_AERIAL_FEATURE, shot.aerial);
        }
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atmosphere test output"),
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
        let [x, y, z] = shot.sun().map(|v| -v as f32);
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

fn all_pixels(image: &HdrImage) -> impl Iterator<Item = [f32; 4]> + '_ {
    image.pixels.iter().copied()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sky_matches_the_cpu_reference_across_the_view() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(1);
    shot.pitch = 0.35;
    let image = rig.render(&shot)?;
    let [w, h] = shot.size;
    let mut compared = 0;
    for (x, y) in [
        (2, 2),
        (32, 2),
        (60, 5),
        (10, 18),
        (32, 18),
        (55, 30),
        (3, 35),
        (62, 34),
    ] {
        let actual = rgb(image.pixel(x, y));
        let expected = shot.expected_sky(x, y);
        assert!(
            luma(expected) > 1.0e-3,
            "reference pixel ({x},{y}) is lit: {expected:?}"
        );
        assert_close(
            actual,
            expected,
            0.04,
            &format!("pixel ({x},{y}) of {w}x{h}"),
        );
        compared += 1;
    }
    assert_eq!(compared, 8);
    assert!(all_pixels(&image).all(|p| p.iter().all(|v| v.is_finite())));
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn feature_off_or_unconfigured_is_identical_to_a_pipeline_without_it() -> Result<()> {
    let mut baseline_rig = Rig::new(false)?;
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(2);
    shot.sky = false;
    shot.aerial = false;
    baseline_rig.set_wall(Some(([0.4, 0.5, 0.6], 0.5)));
    rig.set_wall(Some(([0.4, 0.5, 0.6], 0.5)));
    let baseline = baseline_rig.render(&shot)?;
    // Settings present but both features disabled in the view.
    assert_eq!(
        rig.render(&shot)?.pixels,
        baseline.pixels,
        "disabled features"
    );
    // Features enabled but the view has no settings.
    shot.sky = true;
    shot.aerial = true;
    shot.settings = None;
    assert_eq!(rig.render(&shot)?.pixels, baseline.pixels, "no settings");
    // Enabled with settings but no directional light: nothing to scatter.
    shot.settings = Some(AtmosphereSettings::default());
    shot.sun = false;
    assert_eq!(rig.render(&shot)?.pixels, baseline.pixels, "no sun");
    // An empty sky really renders black, as before.
    baseline_rig.set_wall(None);
    let empty = baseline_rig.render(&Shot {
        sky: false,
        aerial: false,
        ..Shot::new(2)
    })?;
    assert!(all_pixels(&empty).all(|p| p[..3] == [0.0; 3]));
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn zenith_is_bluer_than_the_horizon_at_noon() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut up = Shot::new(3);
    up.sun_elevation = 75.0;
    up.pitch = 1.3;
    let zenith = rgb(rig.render(&up)?.pixel(32, 18));
    let mut level = up.clone();
    level.pitch = 0.0;
    let image = rig.render(&level)?;
    // The row just above the horizon (the camera looks along it).
    let horizon = rgb(image.pixel(32, 17));
    let blue = |c: [f64; 3]| c[2] / c[0];
    eprintln!("zenith {zenith:?} horizon {horizon:?}");
    assert!(
        blue(zenith) > 1.2 * blue(horizon),
        "zenith blue/red {} vs horizon {}",
        blue(zenith),
        blue(horizon)
    );
    assert_close(zenith, up.expected_sky(32, 18), 0.04, "zenith vs reference");
    assert_close(
        horizon,
        level.expected_sky(32, 17),
        0.04,
        "horizon vs reference",
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn low_sun_reddens_the_horizon_toward_the_sun() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut sunset = Shot::new(4);
    sunset.sun_elevation = 2.0;
    sunset.pitch = 0.05;
    let toward = rgb(rig.render(&sunset)?.pixel(32, 12));
    let mut away = sunset.clone();
    away.yaw = std::f32::consts::PI;
    let opposite = rgb(rig.render(&away)?.pixel(32, 12));
    let mut noon = sunset.clone();
    noon.sun_elevation = 70.0;
    let midday = rgb(rig.render(&noon)?.pixel(32, 12));
    let redness = |c: [f64; 3]| c[0] / c[2];
    eprintln!("sunset toward {toward:?} away {opposite:?} noon {midday:?}");
    assert!(redness(toward) > 2.0 * redness(midday));
    assert!(redness(toward) > redness(opposite));
    assert_close(
        toward,
        sunset.expected_sky(32, 12),
        0.05,
        "sunset vs reference",
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sky_is_brighter_around_the_sun_and_the_disk_is_visible() -> Result<()> {
    let mut rig = Rig::new(true)?;
    // Sun 15 degrees above a level view: elevation shows near the top rows.
    let mut shot = Shot::new(5);
    shot.sun_elevation = 15.0;
    shot.pitch = 0.2;
    let image = rig.render(&shot)?;
    // Find the brightest pixel; it must be the sun disk, far above the sky.
    let (mut best, mut at) = (0.0, (0, 0));
    for y in 0..shot.size[1] {
        for x in 0..shot.size[0] {
            let l = luma(rgb(image.pixel(x, y)));
            if l > best {
                (best, at) = (l, (x, y));
            }
        }
    }
    let dir = shot.dir(at.0, at.1);
    let sun = shot.sun();
    let angle = (dir[0] * sun[0] + dir[1] * sun[1] + dir[2] * sun[2]).acos();
    eprintln!("brightest {best} at {at:?}, {angle} rad from the sun");
    assert!(angle < 0.03, "brightest pixel is at the sun");
    let sky_far_from_sun = luma(rgb(image.pixel(5, 30)));
    assert!(
        best > 200.0 * sky_far_from_sun,
        "disk {best} vs sky {sky_far_from_sun}"
    );
    assert!(
        best <= 60_000.0 && best.is_finite(),
        "disk radiance bounded: {best}"
    );
    // The Mie forward peak: sky next to the disk beats sky across the view.
    let next_to_sun = luma(shot.expected_sky(at.0, at.1.saturating_sub(5)));
    let across = luma(shot.expected_sky(5, 30));
    assert!(next_to_sun > 1.5 * across);
    assert_close(
        rgb(image.pixel(at.0, at.1.saturating_sub(5))),
        shot.expected_sky(at.0, at.1.saturating_sub(5)),
        0.06,
        "sky next to the sun",
    );
    // No disk when its diameter is zero.
    let mut hidden = shot.clone();
    hidden.settings = Some(AtmosphereSettings {
        sun_angular_diameter: 0.0,
        ..Default::default()
    });
    let image = rig.render(&hidden)?;
    let brightest = all_pixels(&image).map(|p| luma(rgb(p))).fold(0.0, f64::max);
    assert!(brightest < best * 0.01, "no disk: {brightest}");
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn denser_media_and_albedo_change_the_sky_as_the_model_predicts() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut base = Shot::new(6);
    base.sun_elevation = 45.0;
    base.pitch = 0.9;
    let thin = rig.render(&base)?;
    let mut dense = base.clone();
    dense.settings = Some(AtmosphereSettings {
        rayleigh_scattering: [5.802e-6 * 3.0, 13.558e-6 * 3.0, 33.1e-6 * 3.0],
        ..Default::default()
    });
    let thick = rig.render(&dense)?;
    let (a, b) = (rgb(thin.pixel(32, 10)), rgb(thick.pixel(32, 10)));
    eprintln!("Rayleigh x1 {a:?}  x3 {b:?}");
    assert!(
        luma(b) > 1.4 * luma(a) && b[2] > a[2] && b[0] > 2.0 * a[0],
        "triple Rayleigh brightens the sky"
    );
    assert_close(
        b,
        dense.expected_sky(32, 10),
        0.04,
        "dense Rayleigh vs reference",
    );

    // Intensity scales linearly.
    let mut bright = base.clone();
    bright.settings = Some(AtmosphereSettings {
        intensity: 4.0,
        ..Default::default()
    });
    let c = rgb(rig.render(&bright)?.pixel(32, 10));
    for k in 0..3 {
        assert!(
            (c[k] / a[k] - 4.0).abs() < 0.05,
            "intensity x4: {c:?} vs {a:?}"
        );
    }

    // Ground albedo shows below the horizon only.
    let mut down = base.clone();
    down.pitch = -0.3;
    let dark_ground = rig.render(&Shot {
        settings: Some(AtmosphereSettings {
            ground_albedo: [0.0; 3],
            ..Default::default()
        }),
        ..down.clone()
    })?;
    let bright_ground = rig.render(&Shot {
        settings: Some(AtmosphereSettings {
            ground_albedo: [0.9; 3],
            ..Default::default()
        }),
        ..down.clone()
    })?;
    let (g0, g1) = (
        rgb(dark_ground.pixel(32, 30)),
        rgb(bright_ground.pixel(32, 30)),
    );
    eprintln!("ground albedo 0 {g0:?} 0.9 {g1:?}");
    assert!(luma(g1) > 3.0 * luma(g0));
    assert_close(
        g1,
        Shot {
            settings: Some(AtmosphereSettings {
                ground_albedo: [0.9; 3],
                ..Default::default()
            }),
            ..down
        }
        .expected_sky(32, 30),
        0.05,
        "lit planet vs reference",
    );
    // Albedo does not touch the sky above the horizon.
    assert_eq!(
        dark_ground.pixel(32, 2),
        bright_ground.pixel(32, 2),
        "sky unaffected by albedo"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn higher_camera_altitude_thins_the_sky() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut ground = Shot::new(7);
    ground.pitch = 1.0;
    ground.sun_elevation = 50.0;
    let mut high = ground.clone();
    high.altitude = 30_000.0;
    let (a, b) = (
        rgb(rig.render(&ground)?.pixel(32, 18)),
        rgb(rig.render(&high)?.pixel(32, 18)),
    );
    eprintln!("sea level {a:?} 30 km {b:?}");
    assert!(luma(b) < 0.5 * luma(a), "less air above 30 km");
    assert_close(b, high.expected_sky(32, 18), 0.05, "30 km vs reference");
    // From space the planet fills the lower view and the limb glows.
    let mut space = Shot::new(8);
    space.altitude = 400_000.0;
    space.pitch = -std::f32::consts::FRAC_PI_2;
    space.sun_elevation = 60.0;
    let image = rig.render(&space)?;
    assert!(all_pixels(&image).all(|p| p.iter().all(|v| v.is_finite())));
    let center = rgb(image.pixel(32, 18));
    assert!(
        luma(center) > 0.01,
        "sunlit planet seen from orbit: {center:?}"
    );
    assert_close(
        center,
        space.expected_sky(32, 18),
        0.06,
        "orbit vs reference",
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn aerial_perspective_tints_far_geometry_more_than_near_geometry() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(9);
    shot.sun_elevation = 45.0;
    shot.sun_azimuth = 90.0;
    let wall = [0.2, 0.2, 0.2];
    let centre = (32, 18);
    let sky_here = {
        rig.set_wall(None);
        rgb(rig.render(&shot)?.pixel(centre.0, centre.1))
    };
    let mut tint = Vec::new();
    for distance in [300.0f32, 3_000.0, 30_000.0] {
        rig.set_wall(Some((wall, shot.wall_depth(distance))));
        let pixel = rgb(rig.render(&shot)?.pixel(centre.0, centre.1));
        let (inscatter, transmittance) = aerial(
            &shot.medium(),
            shot.altitude,
            shot.dir(centre.0, centre.1),
            shot.sun(),
            shot.irradiance(),
            f64::from(distance),
        );
        let expected: [f64; 3] =
            std::array::from_fn(|k| f64::from(wall[k]) * transmittance[k] + inscatter[k]);
        eprintln!("{distance} m: {pixel:?} reference {expected:?}");
        assert_close(pixel, expected, 0.05, &format!("wall at {distance} m"));
        // Distance from the sky colour in this direction: smaller = more tinted.
        let gap: f64 = (0..3).map(|k| (pixel[k] - sky_here[k]).abs()).sum();
        tint.push(gap);
    }
    assert!(
        tint[0] > tint[1] && tint[1] > tint[2],
        "geometry fades toward the sky with distance: {tint:?}"
    );
    // Identical object, same depth, aerial feature off: untouched.
    rig.set_wall(Some((wall, shot.wall_depth(30_000.0))));
    let mut off = shot.clone();
    off.aerial = false;
    let plain = rig.render(&off)?;
    assert_close(
        rgb(plain.pixel(centre.0, centre.1)),
        wall.map(f64::from),
        0.001,
        "aerial off keeps the wall",
    );
    // A zero scale disables aerial perspective as well.
    let mut zero = shot.clone();
    zero.settings = Some(AtmosphereSettings {
        aerial_perspective_scale: 0.0,
        ..Default::default()
    });
    assert_eq!(rig.render(&zero)?.pixels, plain.pixels);
    // A larger scale tints the same wall further.
    rig.set_wall(Some((wall, shot.wall_depth(300.0))));
    let near = rgb(rig.render(&shot)?.pixel(centre.0, centre.1));
    let mut exaggerated = shot.clone();
    exaggerated.settings = Some(AtmosphereSettings {
        aerial_perspective_scale: 50.0,
        ..Default::default()
    });
    let stretched = rgb(rig.render(&exaggerated)?.pixel(centre.0, centre.1));
    assert!(
        (0..3)
            .map(|k| (stretched[k] - sky_here[k]).abs())
            .sum::<f64>()
            < (0..3).map(|k| (near[k] - sky_here[k]).abs()).sum::<f64>()
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn sky_pixels_stay_sky_and_geometry_hides_the_sky() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let shot = Shot::new(10);
    rig.set_wall(Some(([0.3, 0.3, 0.3], shot.wall_depth(50.0))));
    let image = rig.render(&shot)?;
    // The wall covers every pixel: no sky anywhere, only (slight) aerial tint.
    for p in all_pixels(&image) {
        assert!(p[2] < 0.4 && p[0] > 0.25, "wall colour survives: {p:?}");
    }
    // Aerial perspective alone never paints the far-plane pixels.
    rig.set_wall(None);
    let mut aerial_only = shot.clone();
    aerial_only.sky = false;
    assert!(all_pixels(&rig.render(&aerial_only)?).all(|p| p[..3] == [0.0; 3]));
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn resize_two_views_and_forget_view() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut a = Shot::new(20);
    a.pitch = 0.4;
    let mut b = Shot::new(21);
    b.size = [41, 29];
    b.sun_elevation = 8.0;
    b.altitude = 500.0;
    let (ia, ib) = (rig.render(&a)?, rig.render(&b)?);
    assert_eq!((ia.size, ib.size), ([65, 37], [41, 29]));
    assert_close(
        rgb(ia.pixel(20, 12)),
        a.expected_sky(20, 12),
        0.04,
        "view A",
    );
    assert_close(
        rgb(ib.pixel(20, 14)),
        b.expected_sky(20, 14),
        0.05,
        "view B",
    );
    // Interleaved frames keep their settings apart.
    assert_eq!(rig.render(&a)?.pixels, ia.pixels);
    assert_eq!(rig.controls.view_count(), 2);

    // Resize view A.
    a.size = [97, 53];
    let resized = rig.render(&a)?;
    assert_eq!(resized.size, [97, 53]);
    assert_close(
        rgb(resized.pixel(48, 20)),
        a.expected_sky(48, 20),
        0.04,
        "resized A",
    );
    assert_close(
        rgb(resized.pixel(5, 45)),
        a.expected_sky(5, 45),
        0.04,
        "resized A corner",
    );

    // Forgetting a view drops its atmosphere state with its targets.
    rig.scene.forget_view(a.id);
    assert_eq!(rig.controls.view_count(), 1);
    assert!(rig.controls.get(a.id)?.is_none());
    assert_close(
        rgb(rig.render(&b)?.pixel(20, 14)),
        b.expected_sky(20, 14),
        0.05,
        "B after forgetting A",
    );
    // Rendering the forgotten id again without settings draws no sky.
    let mut gone = a.clone();
    gone.settings = None;
    assert!(all_pixels(&rig.render(&gone)?).all(|p| p[..3] == [0.0; 3]));
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn orthographic_views_are_finite_and_use_the_view_axis() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(30);
    shot.ortho = true;
    shot.pitch = 0.5;
    let image = rig.render(&shot)?;
    assert!(all_pixels(&image).all(|p| p.iter().all(|v| v.is_finite())));
    // Every orthographic ray shares one direction, so the sky is uniform.
    let first = image.pixel(1, 1);
    assert!(all_pixels(&image).all(|p| (0..3).all(|k| (p[k] - first[k]).abs() < 1e-3)));
    assert!(first[2] > 1e-3);
    Ok(())
}

/// A jittered projection moves the rasterized image and the reconstructed rays
/// together: sky pixels and an aerial-perspective wall still match the
/// reference evaluated at the shifted direction.
#[test]
#[ignore = "requires a GPU adapter"]
fn jittered_views_use_the_shifted_ray_for_sky_and_aerial_perspective() -> Result<()> {
    let mut rig = Rig::new(true)?;
    let mut shot = Shot::new(40);
    shot.sun_elevation = 35.0;
    shot.sun_azimuth = 60.0;
    shot.pitch = 0.3;
    shot.jitter = [0.75, -0.5];
    let plain = rig.render(&Shot {
        jitter: [0.0; 2],
        ..shot.clone()
    })?;
    let jittered = rig.render(&shot)?;
    assert_ne!(plain.pixels, jittered.pixels, "jitter moves the sky");
    for (x, y) in [(3, 3), (32, 10), (60, 20), (20, 33)] {
        assert_close(
            rgb(jittered.pixel(x, y)),
            shot.expected_sky(x, y),
            0.04,
            &format!("jittered sky pixel ({x},{y})"),
        );
    }

    // An opaque wall: distance follows the jittered ray, not the pixel centre.
    shot.pitch = 0.0;
    shot.sun_azimuth = 90.0;
    let distance = 4_000.0f32;
    rig.set_wall(Some(([0.2; 3], shot.wall_depth(distance))));
    let image = rig.render(&shot)?;
    for (x, y) in [(32, 18), (6, 4), (58, 30)] {
        let reach = shot.wall_distance(x, y, distance);
        let (inscatter, transmittance) = aerial(
            &shot.medium(),
            shot.altitude,
            shot.dir(x, y),
            shot.sun(),
            shot.irradiance(),
            reach,
        );
        let expected: [f64; 3] = std::array::from_fn(|k| 0.2 * transmittance[k] + inscatter[k]);
        assert_close(
            rgb(image.pixel(x, y)),
            expected,
            0.05,
            &format!("jittered wall pixel ({x},{y}) at {reach:.0} m"),
        );
    }
    Ok(())
}
