//! GPU tests of `world.water` and `world.water.underwater`: render small scenes offscreen and
//! check pixels. Run with
//! `cargo test -p engine-world-render --test gpu_water -- --ignored --test-threads=1`.
use anyhow::Result;
use bevy_ecs::prelude::*;
use engine_project::world::water::{WaterKind, WaterRenderItem, WaterRenderSet};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use engine_water::{
    GRAVITY, GerstnerSet, GerstnerWave, OceanSettings, OceanSpectrum, ShorelineSettings,
    WaterOptics, WaterShape, WaterSurface, Waves,
};
use engine_world_render::{names, water};
use glam::{Mat4, Vec2, Vec3};
use std::sync::Arc;

const SIZE: [u32; 2] = [160, 120];

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("adapter: {:?}", gpu.adapter.get_info().name);
    Ok(gpu)
}

fn projection() -> Mat4 {
    glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_3,
        SIZE[0] as f32 / SIZE[1] as f32,
        0.1,
        500.0,
    )
}

fn view(eye: Vec3, target: Vec3) -> Result<ViewData> {
    let camera = glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y).inverse();
    let mut view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE)?;
    view.ambient = Vec3::splat(0.1);
    Ok(view)
}

fn sun() -> LightList {
    let travel = Vec3::new(-0.3, -1.0, -0.2).normalize();
    LightList {
        lights: vec![Light::directional(travel, Vec3::ONE, 1.0)],
    }
}

fn surface(shape: WaterShape, waves: Waves, time: f32) -> WaterSurface {
    WaterSurface {
        shape,
        waves,
        optics: WaterOptics::default(),
        shoreline: ShorelineSettings::default(),
        time,
    }
}

fn world_with(surface: WaterSurface, half: f32, grid: u32) -> World {
    let mut world = World::new();
    world.insert_resource(WaterRenderSet {
        items: vec![WaterRenderItem {
            owner: Entity::PLACEHOLDER,
            id: "lake".into(),
            kind: WaterKind::Lake,
            surface: Arc::new(surface),
            world_from_local: Mat4::IDENTITY.to_cols_array_2d(),
            half_extent: [half, half],
            grid_resolution: grid,
            cast_shadows: false,
            shore: None,
            revision: 1,
        }],
        time: 0.0,
    });
    world
}

fn lake(half: f32) -> WaterShape {
    WaterShape::Lake {
        half_extent: Vec2::splat(half),
    }
}

fn gerstner() -> Waves {
    Waves::Gerstner(Arc::new(
        GerstnerSet::deep(&[
            GerstnerWave {
                wavelength: 9.0,
                amplitude: 0.5,
                steepness: 0.6,
                ..GerstnerWave::default()
            },
            GerstnerWave {
                direction_degrees: 70.0,
                wavelength: 5.0,
                amplitude: 0.2,
                steepness: 0.4,
                phase: 1.0,
            },
        ])
        .unwrap(),
    ))
}

struct Offscreen {
    texture: wgpu::Texture,
    target: OutputTarget,
}
fn offscreen(gpu: &GpuContext) -> Offscreen {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("water target"),
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

struct Renderer {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    status: water::WaterRenderStatusHandle,
}
impl Renderer {
    fn new() -> Result<Self> {
        let gpu = gpu()?;
        let mut pipeline = ScenePipeline::new(gpu.clone())?;
        let mut features = PipelineFeatures::default();
        let status = water::register(&mut features)?;
        pipeline.add_features(features.0)?;
        Ok(Self {
            gpu,
            pipeline,
            status,
        })
    }
    fn render(
        &mut self,
        view: &ViewData,
        meshes: &[SceneMesh<'_>],
        world: Option<&World>,
    ) -> Result<(Vec<u8>, ViewReport)> {
        self.render_with_lights(view, meshes, world, &sun())
    }
    fn render_with_lights(
        &mut self,
        view: &ViewData,
        meshes: &[SceneMesh<'_>],
        world: Option<&World>,
        lights: &LightList,
    ) -> Result<(Vec<u8>, ViewReport)> {
        let target = offscreen(&self.gpu);
        let report = self.pipeline.render_view(
            &SceneFrame {
                view,
                meshes,
                lights,
                world,
            },
            &target.target,
            &mut NoHost,
        )?;
        self.gpu.check_errors()?;
        let pixels =
            engine_render_wgpu::read_texture_rgba(&self.gpu, &target.texture, SIZE[0], SIZE[1])?;
        Ok((pixels, report))
    }
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE[0] + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}
fn covered(p: [u8; 4]) -> bool {
    p[0] > 0 || p[1] > 0 || p[2] > 0
}
fn linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    255.0
        * if l <= 0.003_130_8 {
            12.92 * l
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        }
}
fn luminance(p: [u8; 4]) -> f32 {
    0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2])
}
fn pixel_of(view: &ViewData, p: Vec3) -> (u32, u32) {
    let clip = view.clip_from_world() * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x + 1.0) * 0.5 * SIZE[0] as f32) as u32,
        ((1.0 - ndc.y) * 0.5 * SIZE[1] as f32) as u32,
    )
}

/// An unlit quad through four corners (counter-clockwise seen from the front).
fn quad(gpu: &GpuContext, corners: [Vec3; 4]) -> Result<Arc<GpuMesh>> {
    let vertices: Vec<EngineVertex> = corners
        .iter()
        .map(|p| EngineVertex {
            position: p.to_array(),
            uv: [0.0; 2],
            color: [1.0; 4],
            normal: [0.0; 3],
        })
        .collect();
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "quad",
        &vertices,
        &[0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2],
    )
}
fn unlit<'a>(id: &'a str, mesh: &Arc<GpuMesh>, tint: [f32; 4]) -> SceneMesh<'a> {
    SceneMesh {
        id,
        mesh: mesh.clone(),
        world_from_local: Mat4::IDENTITY,
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

/// A Gerstner lake seen from above covers the lower image, leaves the area past it black and
/// animates with the wave time; one draw call.
#[test]
#[ignore = "requires a native GPU adapter"]
fn gerstner_lake_covers_the_view_and_animates() -> Result<()> {
    let mut r = Renderer::new()?;
    let v = view(Vec3::new(0.0, 12.0, 30.0), Vec3::new(0.0, 0.0, -5.0))?;
    let world = world_with(surface(lake(30.0), gerstner(), 0.0), 30.0, 64);
    let (a, report) = r.render(&v, &[], Some(&world))?;
    let status = r.status.get();
    eprintln!("status {status:?}, calls {}", report.draw_calls);
    assert_eq!(
        report.features.get(names::WATER).unwrap().activity,
        Activity::Ran
    );
    assert_eq!(status.bodies, 1);
    assert_eq!(status.draw_calls, 1);
    assert_eq!(status.underwater, None);
    let coverage = a
        .chunks(4)
        .filter(|p| covered([p[0], p[1], p[2], 0]))
        .count() as f32
        / (SIZE[0] * SIZE[1]) as f32;
    eprintln!("water coverage {coverage:.3}");
    assert!((0.3..0.95).contains(&coverage), "{coverage}");
    for x in [5, SIZE[0] / 2, SIZE[0] - 5] {
        assert!(
            !covered(pixel(&a, x, 1)),
            "beyond the far shore at ({x}, 1)"
        );
    }
    // The near water is covered.
    let near = pixel_of(&v, Vec3::new(0.0, 0.0, 10.0));
    assert!(covered(pixel(&a, near.0, near.1)));
    // Later wave time: the surface moved.
    let world = world_with(surface(lake(30.0), gerstner(), 1.3), 30.0, 64);
    let (b, _) = r.render(&v, &[], Some(&world))?;
    let changed = a
        .chunks(4)
        .zip(b.chunks(4))
        .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 2))
        .count();
    eprintln!("pixels changed by the wave motion: {changed}");
    assert!(changed as f32 > 0.05 * coverage * (SIZE[0] * SIZE[1]) as f32);
    Ok(())
}

/// Over a ground ramp, shallow water shows the ground (bright), deep water is dark and
/// blue-shifted, and the shoreline (ground just below the surface) is foamy.
#[test]
#[ignore = "requires a native GPU adapter"]
fn shallow_water_is_brighter_than_deep_water_and_the_shore_foams() -> Result<()> {
    let mut r = Renderer::new()?;
    // Ground y = 0.25 x - 4 (x from -24 to 15), z from -40 to 15.
    let y = |x: f32| 0.25 * x - 4.0;
    let ramp = quad(
        &r.gpu,
        [
            Vec3::new(-24.0, y(-24.0), 15.0),
            Vec3::new(15.0, y(15.0), 15.0),
            Vec3::new(15.0, y(15.0), -40.0),
            Vec3::new(-24.0, y(-24.0), -40.0),
        ],
    )?;
    let meshes = [unlit("ramp", &ramp, [0.6, 0.6, 0.6, 1.0])];
    let v = view(Vec3::new(0.0, 25.0, 25.0), Vec3::new(0.0, 0.0, -5.0))?;
    let world = world_with(surface(lake(40.0), Waves::Flat, 0.0), 40.0, 32);
    let (with, _) = r.render(&v, &meshes, Some(&world))?;
    let (without, _) = r.render(&v, &meshes, None)?;
    let at = |pixels: &[u8], x: f32| {
        let (px, py) = pixel_of(&v, Vec3::new(x, 0.0, -5.0));
        pixel(pixels, px, py)
    };
    let (deep, shallow, shore) = (at(&with, -14.0), at(&with, 10.0), at(&with, 15.0 - 0.6));
    let (dry_deep, dry_shallow) = (at(&without, -14.0), at(&without, 10.0));
    eprintln!(
        "deep {deep:?} (ground {dry_deep:?}), shallow {shallow:?} (ground {dry_shallow:?}), shore {shore:?}"
    );
    let ratio = luminance(shallow) / luminance(deep);
    eprintln!("shallow / deep luminance {ratio:.2}");
    assert!(ratio > 1.3, "{ratio}");
    // Absorption removes red fastest: deep water is bluer than the grey ground.
    let blue = |p: [u8; 4]| linear(p[2]) / linear(p[0]).max(1.0e-4);
    assert!(blue(deep) > 3.0 * blue(dry_deep), "{deep:?}");
    assert!(blue(deep) > blue(shallow));
    // Shore foam (ground 0.15 m below the surface) is the brightest of the three.
    assert!(luminance(shore) > luminance(shallow), "{shore:?}");
    Ok(())
}

/// A camera 2 m below the surface looking at a white wall 15 m away sees it fogged exactly
/// as `mix(fog, white * exp(-absorption d), exp(-density d))`, and open water as the fog
/// colour; above the water the wall is white.
#[test]
#[ignore = "requires a native GPU adapter"]
fn underwater_view_is_fogged_by_distance() -> Result<()> {
    let mut r = Renderer::new()?;
    let wall = quad(
        &r.gpu,
        [
            Vec3::new(-10.0, -12.0, -15.0),
            Vec3::new(10.0, -12.0, -15.0),
            Vec3::new(10.0, 12.0, -15.0),
            Vec3::new(-10.0, 12.0, -15.0),
        ],
    )?;
    let meshes = [unlit("wall", &wall, [1.0, 1.0, 1.0, 1.0])];
    let world = world_with(surface(lake(60.0), Waves::Flat, 0.0), 60.0, 16);
    let below = view(Vec3::new(0.0, -2.0, 0.0), Vec3::new(0.0, -2.0, -15.0))?;
    let (pixels, report) = r.render(&below, &meshes, Some(&world))?;
    let status = r.status.get();
    eprintln!("status {status:?}");
    assert_eq!(
        report
            .features
            .get(names::WATER_UNDERWATER)
            .unwrap()
            .activity,
        Activity::Ran
    );
    assert_eq!(status.underwater.as_deref(), Some("lake"));
    assert_eq!(status.underwater_views, 1);
    let optics = WaterOptics::default();
    let expected = |distance: f32, scene: [f32; 3]| {
        let clear = (-optics.fog_density * distance).exp();
        [0, 1, 2].map(|c| {
            let absorbed = scene[c] * (-optics.absorption[c] * distance).exp();
            srgb(optics.fog_color[c] + (absorbed - optics.fog_color[c]) * clear)
        })
    };
    let (cx, cy) = (SIZE[0] / 2, SIZE[1] / 2);
    let centre = pixel(&pixels, cx, cy);
    // The centre pixel's ray hits the wall about 15 m away.
    let want = expected(15.0, [1.0; 3]);
    eprintln!("wall through 15 m of water {centre:?}, expected {want:?}");
    for c in 0..3 {
        assert!(
            (centre[c] as f32 - want[c]).abs() <= 3.0,
            "channel {c}: {centre:?} vs {want:?}"
        );
    }
    // Open water (no geometry): the fog colour.
    let open = pixel(&pixels, 1, SIZE[1] - 2);
    let fog = optics.fog_color.map(srgb);
    eprintln!("open water {open:?}, fog {fog:?}");
    for c in 0..3 {
        assert!(
            (open[c] as f32 - fog[c]).abs() <= 2.0,
            "{open:?} vs {fog:?}"
        );
    }
    // Above the water: no fog, the wall is white.
    let above = view(Vec3::new(0.0, 6.0, 0.0), Vec3::new(0.0, 6.0, -15.0))?;
    let (pixels, _) = r.render(&above, &meshes, Some(&world))?;
    assert_eq!(r.status.get().underwater, None);
    assert_eq!(r.status.get().underwater_views, 1);
    let wall = pixel(&pixels, cx, cy);
    assert!(wall[..3].iter().all(|&c| c >= 250), "{wall:?}");
    Ok(())
}

/// An FFT ocean uploads its frame textures, covers the view and evolves in time.
#[test]
#[ignore = "requires a native GPU adapter"]
fn fft_ocean_uploads_frames_and_renders() -> Result<()> {
    let mut r = Renderer::new()?;
    let settings = OceanSettings {
        resolution: 64,
        patch_length: 40.0,
        wind_speed: 9.0,
        ..OceanSettings::default()
    };
    let ocean = OceanSpectrum::new(&settings, GRAVITY)?;
    let v = view(Vec3::new(0.0, 10.0, 25.0), Vec3::new(0.0, 0.0, -5.0))?;
    let frame = |t: f32| -> Result<World> {
        Ok(world_with(
            surface(lake(40.0), Waves::Ocean(Arc::new(ocean.frame(t)?)), t),
            40.0,
            128,
        ))
    };
    let (a, _) = r.render(&v, &[], Some(&frame(0.0)?))?;
    let (b, _) = r.render(&v, &[], Some(&frame(2.0)?))?;
    let status = r.status.get();
    eprintln!("status {status:?}");
    assert_eq!(status.uploads, 2, "one frame upload per new frame");
    let coverage = a
        .chunks(4)
        .filter(|p| covered([p[0], p[1], p[2], 0]))
        .count() as f32
        / (SIZE[0] * SIZE[1]) as f32;
    let changed = a
        .chunks(4)
        .zip(b.chunks(4))
        .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 2))
        .count();
    eprintln!("coverage {coverage:.3}, changed {changed}");
    assert!(coverage > 0.3);
    assert!(changed > 500);
    Ok(())
}

/// Opt-in displaced water changes a native ground receiver through the shared atlas.
#[test]
#[ignore = "requires a native GPU adapter"]
fn opted_in_water_casts_animated_opaque_depth() -> Result<()> {
    let mut renderer = Renderer::new()?;
    let corner = |x: f32, z: f32| EngineVertex {
        position: [x, 0.0, z],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: Vec3::Y.to_array(),
    };
    let ground = GpuMesh::from_engine_vertices(
        &renderer.gpu.device,
        "water shadow receiver",
        &[
            corner(-20.0, -20.0),
            corner(-20.0, 20.0),
            corner(20.0, 20.0),
            corner(20.0, -20.0),
        ],
        &[0, 1, 2, 0, 2, 3],
    )?;
    let receiver = [SceneMesh {
        id: "ground",
        mesh: ground,
        world_from_local: Mat4::IDENTITY,
        shader: None,
        params: MaterialParams::default(),
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
    }];
    let light = LightList {
        lights: vec![
            Light::directional(Vec3::new(-1.0, -1.0, 0.0).normalize(), Vec3::ONE, 1.0)
                .with_shadow(ShadowSettings::default()),
        ],
    };
    let view = view(Vec3::new(0.0, 20.0, 25.0), Vec3::new(-5.0, 0.0, 0.0))?;
    let mut world = world_with(surface(lake(4.0), gerstner(), 0.0), 4.0, 64);
    {
        let mut set = world.resource_mut::<WaterRenderSet>();
        set.items[0].world_from_local =
            Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0)).to_cols_array_2d();
    }
    let (bare, _) = renderer.render_with_lights(&view, &receiver, Some(&world), &light)?;
    world.resource_mut::<WaterRenderSet>().items[0].cast_shadows = true;
    let (cast0, report) = renderer.render_with_lights(&view, &receiver, Some(&world), &light)?;
    assert_eq!(
        report.features.get(names::WATER_SHADOW).unwrap().activity,
        Activity::Ran
    );
    world.resource_mut::<WaterRenderSet>().items[0].surface =
        Arc::new(surface(lake(4.0), gerstner(), 1.3));
    let (cast1, _) = renderer.render_with_lights(&view, &receiver, Some(&world), &light)?;
    let mut shadowed = 0;
    let mut animated = 0;
    for ix in 0..49 {
        for iz in 0..25 {
            let (x, y) = pixel_of(
                &view,
                Vec3::new(-9.0 + ix as f32 * 0.125, 0.0, -3.0 + iz as f32 * 0.25),
            );
            if x >= SIZE[0] || y >= SIZE[1] {
                continue;
            }
            let a = luminance(pixel(&bare, x, y));
            let b = luminance(pixel(&cast0, x, y));
            let c = luminance(pixel(&cast1, x, y));
            if a > 0.02 && b < a * 0.8 {
                shadowed += 1;
            }
            if (b - c).abs() > 0.01 {
                animated += 1;
            }
        }
    }
    assert!(
        shadowed > 0,
        "opt-in water produced no native receiver shadow"
    );
    assert!(
        animated > 0,
        "water shadow did not follow the animated pose"
    );
    Ok(())
}
