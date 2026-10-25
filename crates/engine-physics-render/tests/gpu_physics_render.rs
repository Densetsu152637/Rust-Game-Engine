//! GPU tests of the physics render features. Run with `--ignored --test-threads=1` on a real
//! adapter. Every test renders a known primitive into a 64x64 offscreen target and asserts
//! pixels.

use anyhow::Result;
use bevy_ecs::prelude::*;
use engine_physics_render::{
    FeatureActivity, FluidColorMode, FragmentDraw, PhysicsRenderConfig, PhysicsRenderExtract,
    PhysicsRenderStatusHandle, SurfaceShading, names, register_physics_features_with,
};
use engine_project::simulation::physics::{
    ClothRender, DebugSegment, FluidRender, FragmentRender, PhysicsRenderData,
};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Vec3};
use std::sync::Arc;

const SIZE: [u32; 2] = [64, 64];
const BACKGROUND: [u8; 4] = [0, 0, 0, 255];

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("adapter: {:?}", gpu.adapter.get_info().name);
    Ok(gpu)
}

fn perspective() -> Mat4 {
    glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_3, 1.0, 0.1, 100.0)
}

fn view() -> ViewData {
    let mut view = ViewData::new(ViewId::MAIN, Mat4::IDENTITY, perspective(), SIZE, SIZE).unwrap();
    view.ambient = Vec3::splat(1.0);
    view
}

/// Pixel (column, row) of a world point on the test camera.
fn project(point: Vec3) -> (u32, u32) {
    let clip = perspective() * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x + 1.0) * 0.5 * SIZE[0] as f32) as u32,
        ((1.0 - ndc.y) * 0.5 * SIZE[1] as f32) as u32,
    )
}

struct Offscreen {
    texture: wgpu::Texture,
    target: OutputTarget,
}

fn offscreen(gpu: &GpuContext) -> Offscreen {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("physics render test target"),
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

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE[0] + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

/// An unlit quad in the XY plane at depth `z`.
fn occluder(gpu: &GpuContext, min: [f32; 2], max: [f32; 2], z: f32) -> SceneMesh<'static> {
    let vertex = |x, y| EngineVertex {
        position: [x, y, 0.0],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: [0.0, 0.0, 1.0],
    };
    let mesh = GpuMesh::from_engine_vertices(
        &gpu.device,
        "occluder",
        &[
            vertex(min[0], min[1]),
            vertex(max[0], min[1]),
            vertex(max[0], max[1]),
            vertex(min[0], max[1]),
        ],
        &[0, 1, 2, 0, 2, 3],
    )
    .unwrap();
    SceneMesh {
        id: "occluder",
        mesh,
        world_from_local: Mat4::from_translation(Vec3::new(0.0, 0.0, z)),
        shader: None,
        params: MaterialParams {
            tint: [1.0, 0.0, 0.0, 1.0],
            unlit: true,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
    }
}

struct Scene {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    status: PhysicsRenderStatusHandle,
    world: World,
}

fn scene(config: &PhysicsRenderConfig) -> Result<Scene> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let mut features = PipelineFeatures::default();
    let status = register_physics_features_with(&mut features, config)?;
    pipeline.add_features(features.0)?;
    Ok(Scene {
        gpu,
        pipeline,
        status,
        world: World::new(),
    })
}

impl Scene {
    fn render(&mut self, meshes: &[SceneMesh<'_>], lights: &LightList) -> Result<Vec<u8>> {
        let target = offscreen(&self.gpu);
        let view = view();
        self.pipeline.render_view(
            &SceneFrame {
                view: &view,
                meshes,
                lights,
                world: Some(&self.world),
            },
            &target.target,
            &mut NoHost,
        )?;
        self.gpu.check_errors()?;
        engine_render_wgpu::read_texture_rgba(&self.gpu, &target.texture, SIZE[0], SIZE[1])
    }

    fn set_extract(&mut self, extract: PhysicsRenderExtract) {
        self.world.insert_resource(extract);
    }
}

fn srgb_byte(linear: f32) -> f32 {
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    encoded.clamp(0.0, 1.0) * 255.0
}

fn close(actual: [u8; 4], expected: [f32; 3], tolerance: f32) -> bool {
    (0..3).all(|i| (actual[i] as f32 - srgb_byte(expected[i])).abs() <= tolerance)
}

fn feature_activity(scene: &Scene, name: &str) -> FeatureActivity {
    scene.status.snapshot().get(name).unwrap().activity
}

// ---------------------------------------------------------------- debug lines

fn line_extract(segments: Vec<DebugSegment>) -> PhysicsRenderExtract {
    PhysicsRenderExtract {
        revision: 1,
        debug: segments,
        debug_state: engine_physics_render::DebugState::On,
        ..Default::default()
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn debug_lines_draw_at_the_projected_pixels_with_the_requested_width() -> Result<()> {
    let mut scene = scene(&PhysicsRenderConfig {
        debug_line_width: 3.0,
        ..Default::default()
    })?;
    // A yellow horizontal line through the view centre at z = -3.
    scene.set_extract(line_extract(vec![DebugSegment {
        from: Vec3::new(-1.0, 0.0, -3.0),
        to: Vec3::new(1.0, 0.0, -3.0),
        color: [1.0, 1.0, 0.0, 1.0],
    }]));
    let pixels = scene.render(&[], &LightList::default())?;
    let (left, row) = project(Vec3::new(-1.0, 0.0, -3.0));
    let (right, _) = project(Vec3::new(1.0, 0.0, -3.0));
    let middle = (left + right) / 2;
    // A 3 px wide line centred on the pixel boundary at y = 32 covers rows 31 and 32.
    for y in [row - 1, row] {
        assert_eq!(pixel(&pixels, middle, y), [255, 255, 0, 255], "row {y}");
    }
    let covered: Vec<u32> = (0..SIZE[1])
        .filter(|&y| pixel(&pixels, middle, y) != BACKGROUND)
        .collect();
    assert!(
        (2..=4).contains(&covered.len()),
        "line width in rows: {covered:?}"
    );
    // The line stops at its end points and nothing else is drawn.
    assert_eq!(pixel(&pixels, left - 4, row), BACKGROUND);
    assert_eq!(pixel(&pixels, right + 4, row), BACKGROUND);
    assert_eq!(pixel(&pixels, middle, 5), BACKGROUND);
    assert_eq!(
        feature_activity(&scene, names::DEBUG_LINES),
        FeatureActivity::Drew
    );
    assert_eq!(
        scene
            .status
            .snapshot()
            .get(names::DEBUG_LINES)
            .unwrap()
            .primitives,
        1
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn debug_lines_are_depth_tested_against_the_scene_unless_disabled() -> Result<()> {
    let extract = || {
        line_extract(vec![DebugSegment {
            from: Vec3::new(-1.0, 0.0, -3.0),
            to: Vec3::new(1.0, 0.0, -3.0),
            color: [1.0, 1.0, 0.0, 1.0],
        }])
    };
    let (left_px, row) = project(Vec3::new(-0.5, 0.0, -3.0));
    let (right_px, _) = project(Vec3::new(0.5, 0.0, -3.0));
    for (depth_test, hidden_color) in [(true, [255, 0, 0, 255]), (false, [255, 255, 0, 255])] {
        let mut scene = scene(&PhysicsRenderConfig {
            debug_depth_test: depth_test,
            ..Default::default()
        })?;
        scene.set_extract(extract());
        // A red wall in front of the left half of the line.
        let wall = occluder(&scene.gpu, [-2.0, -2.0], [0.0, 2.0], -2.0);
        let pixels = scene.render(&[wall], &LightList::default())?;
        assert_eq!(
            pixel(&pixels, left_px, row),
            hidden_color,
            "left half, depth_test {depth_test}"
        );
        assert_eq!(
            pixel(&pixels, right_px, row),
            [255, 255, 0, 255],
            "right half is in front of nothing"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- surfaces

fn cloth_extract(normal: Vec3) -> PhysicsRenderExtract {
    let z = -3.0;
    let mut data = PhysicsRenderData {
        frame: 1,
        ..Default::default()
    };
    data.cloths.insert(
        Entity::from_raw_u32(1).unwrap(),
        ClothRender {
            positions: vec![
                Vec3::new(-1.0, -1.0, z),
                Vec3::new(1.0, -1.0, z),
                Vec3::new(1.0, 1.0, z),
                Vec3::new(-1.0, 1.0, z),
            ],
            normals: vec![normal; 4],
            triangles: Arc::from(vec![[0, 1, 2], [0, 2, 3]]),
        },
    );
    PhysicsRenderExtract {
        revision: 1,
        data,
        ..Default::default()
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_normal_shaded_cloth_covers_its_centre_and_is_depth_tested_against_the_scene() -> Result<()> {
    let config = PhysicsRenderConfig {
        shading: SurfaceShading::NormalShaded,
        ..Default::default()
    };
    let colour = config.cloth_color;
    let mut scene = scene(&config)?;
    scene.set_extract(cloth_extract(Vec3::Z));
    // A red wall in front of the left half of the cloth.
    let wall = occluder(&scene.gpu, [-3.0, -3.0], [0.0, 3.0], -2.0);
    let pixels = scene.render(&[wall], &LightList::default())?;
    // Normal +Z: hemisphere term 0.5 + 0.5 * dot(n, normalize(0.3, 1, 0.2)).
    let light = Vec3::new(0.3, 1.0, 0.2).normalize();
    let factor = 0.3 + 0.7 * (0.5 + 0.5 * Vec3::Z.dot(light));
    let expected = [colour[0] * factor, colour[1] * factor, colour[2] * factor];
    let (cx, cy) = project(Vec3::new(0.5, 0.0, -3.0));
    let actual = pixel(&pixels, cx, cy);
    assert!(
        close(actual, expected, 2.0),
        "cloth centre {actual:?}, expected about {:?}",
        expected.map(srgb_byte)
    );
    let (hx, hy) = project(Vec3::new(-0.5, 0.0, -3.0));
    assert_eq!(pixel(&pixels, hx, hy), [255, 0, 0, 255], "behind the wall");
    // Outside the 2 x 2 cloth nothing is drawn.
    let (ox, oy) = project(Vec3::new(1.3, 0.0, -3.0));
    assert_eq!(pixel(&pixels, ox, oy), BACKGROUND);
    let status = scene.status.snapshot();
    let cloth = status.get(names::CLOTH).unwrap();
    assert_eq!(cloth.activity, FeatureActivity::Drew);
    assert_eq!((cloth.primitives, cloth.truncated), (4, false));
    assert_eq!(
        status.get(names::SOFT_BODY).unwrap().activity,
        FeatureActivity::Idle
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_lit_cloth_reacts_to_the_view_lights_and_is_two_sided() -> Result<()> {
    let mut scene = scene(&PhysicsRenderConfig::default())?;
    let (cx, cy) = project(Vec3::new(0.0, 0.0, -3.0));
    let brightness = |p: [u8; 4]| p[0] as u32 + p[1] as u32 + p[2] as u32;
    scene.set_extract(cloth_extract(Vec3::Z));
    let ambient_only = pixel(&scene.render(&[], &LightList::default())?, cx, cy);
    assert_ne!(ambient_only, BACKGROUND, "ambient lights the cloth");
    let mut lights = LightList::default();
    lights
        .lights
        .push(Light::directional(Vec3::NEG_Z, Vec3::ONE, 3.0));
    let facing = pixel(&scene.render(&[], &lights)?, cx, cy);
    assert!(
        brightness(facing) > brightness(ambient_only),
        "a light shining on the front adds light: {facing:?} vs {ambient_only:?}"
    );
    // The same light from behind a sheet whose normal points away: the shader flips the back
    // face toward the viewer, so the camera-facing side is lit exactly like the front.
    scene.set_extract(cloth_extract(Vec3::NEG_Z));
    let flipped = pixel(&scene.render(&[], &lights)?, cx, cy);
    assert!(
        brightness(flipped) > brightness(ambient_only),
        "two-sided shading: {flipped:?}"
    );
    let status = scene.status.snapshot();
    assert_eq!(
        status.get(names::CLOTH).unwrap().shading,
        engine_physics_render::ShadingReport::Lit
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn destruction_fragments_draw_their_interior_faces_in_the_fracture_colour() -> Result<()> {
    let config = PhysicsRenderConfig {
        shading: SurfaceShading::NormalShaded,
        ..Default::default()
    };
    let interior = config.fragment_interior_color;
    let mut scene = scene(&config)?;
    // One triangle facing the camera, flagged as fracture surface, translated to z = -3.
    let mesh = Arc::new(FragmentRender {
        positions: vec![
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ],
        triangles: vec![[0, 1, 2]],
        interior: vec![true],
    });
    scene.set_extract(PhysicsRenderExtract {
        revision: 1,
        fragments: vec![FragmentDraw {
            owner: Entity::from_raw_u32(1).unwrap(),
            fragment: Entity::from_raw_u32(2).unwrap(),
            world_from_local: Mat4::from_translation(Vec3::new(0.0, 0.0, -3.0)),
            mesh,
        }],
        ..Default::default()
    });
    let pixels = scene.render(&[], &LightList::default())?;
    let (cx, cy) = project(Vec3::new(0.0, -0.3, -3.0));
    let actual = pixel(&pixels, cx, cy);
    let light = Vec3::new(0.3, 1.0, 0.2).normalize();
    let factor = 0.3 + 0.7 * (0.5 + 0.5 * Vec3::Z.dot(light));
    assert!(
        close(
            actual,
            [
                interior[0] * factor,
                interior[1] * factor,
                interior[2] * factor
            ],
            2.0
        ),
        "fragment centre {actual:?}"
    );
    // The top-left corner of the bounding box lies outside the triangle.
    let (ox, oy) = project(Vec3::new(-0.9, 0.9, -3.0));
    assert_eq!(pixel(&pixels, ox, oy), BACKGROUND);
    assert_eq!(
        feature_activity(&scene, names::FRAGMENTS),
        FeatureActivity::Drew
    );
    Ok(())
}

// ---------------------------------------------------------------- fluid

fn fluid_extract(positions: Vec<Vec3>, radius: f32) -> PhysicsRenderExtract {
    let count = positions.len();
    let mut data = PhysicsRenderData {
        frame: 1,
        ..Default::default()
    };
    data.fluids.insert(
        Entity::from_raw_u32(1).unwrap(),
        FluidRender {
            positions,
            velocities: vec![Vec3::ZERO; count],
            densities: vec![1000.0; count],
            surface: vec![false; count],
            particle_radius: radius,
            rest_density: 1000.0,
            capacity: count,
            domain_min: Vec3::splat(-10.0),
            domain_max: Vec3::splat(10.0),
        },
    );
    PhysicsRenderExtract {
        revision: 1,
        data,
        ..Default::default()
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn fluid_impostors_appear_at_the_projected_particle_positions() -> Result<()> {
    let config = PhysicsRenderConfig::default();
    let mut scene = scene(&config)?;
    let particles = vec![
        Vec3::new(-0.8, 0.5, -3.0),
        Vec3::new(0.0, 0.0, -3.0),
        Vec3::new(0.8, -0.5, -3.0),
    ];
    scene.set_extract(fluid_extract(particles.clone(), 0.2));
    let pixels = scene.render(&[], &LightList::default())?;
    for particle in &particles {
        let (x, y) = project(*particle);
        let centre = pixel(&pixels, x, y);
        assert_ne!(centre, BACKGROUND, "particle at {particle}");
        // Mid-density colour ramp: blue-green, so blue exceeds red.
        assert!(centre[2] > centre[0], "ramp colour {centre:?}");
        // 0.2 m radius at 3 m is about 3.7 px: 8 px away is empty.
        assert_eq!(
            pixel(&pixels, x + 8, y),
            BACKGROUND,
            "disc edge for {particle}"
        );
    }
    let status = scene.status.snapshot();
    let fluid = status.get(names::FLUID).unwrap();
    assert_eq!(
        (fluid.activity, fluid.primitives),
        (FeatureActivity::Drew, 3)
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn fluid_impostors_are_depth_correct_against_scene_geometry() -> Result<()> {
    let config = PhysicsRenderConfig {
        fluid: engine_physics_render::FluidStyle {
            mode: FluidColorMode::Flat,
            ..Default::default()
        },
        ..Default::default()
    };
    // A sphere of radius 0.8 centred at z = -3 covers about 14.8 px and its front reaches
    // z = -2.2. A red wall at z = -2.8 is in front of the centre plane but behind the front.
    let sphere = Vec3::new(0.0, 0.0, -3.0);
    let (cx, cy) = project(sphere);
    let mut with_wall = scene(&config)?;
    with_wall.set_extract(fluid_extract(vec![sphere], 0.8));
    let wall = occluder(&with_wall.gpu, [-3.0, -3.0], [3.0, 3.0], -2.8);
    let walled = with_wall.render(&[wall], &LightList::default())?;
    for offset in [0, 10] {
        let p = pixel(&walled, cx + offset, cy);
        assert!(
            p[2] > p[0] && p[0] < 200,
            "the sphere pokes through the wall {offset} px from its centre: {p:?}"
        );
    }
    // 14.5 px from the centre is 0.98 of the radius: the surface is at z = -2.85, behind
    // the wall, so the wall wins there.
    assert_eq!(
        pixel(&walled, cx + 14, cy),
        [255, 0, 0, 255],
        "silhouette hidden by the wall"
    );
    let mut open = scene(&config)?;
    open.set_extract(fluid_extract(vec![sphere], 0.8));
    let free = open.render(&[], &LightList::default())?;
    let edge = pixel(&free, cx + 14, cy);
    assert!(
        edge != BACKGROUND && edge[0] < 200,
        "without the wall the silhouette shows the sphere: {edge:?}"
    );
    Ok(())
}
