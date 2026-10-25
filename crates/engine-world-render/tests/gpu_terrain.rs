//! GPU tests of `world.terrain`: render a small terrain offscreen and check pixels. Run with
//! `cargo test -p engine-world-render --test gpu_terrain -- --ignored --test-threads=1`.
use anyhow::Result;
use bevy_ecs::prelude::*;
use engine_project::world::terrain::{ResolvedLayer, TerrainRenderItem, TerrainRenderSet};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use engine_terrain::{Heightfield, LodLayout, LodSettings, build_chunk_mesh};
use engine_world_render::{TerrainFeature, names};
use glam::{Mat4, Vec3};
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

/// 65 x 65 samples (spacing 1) forming a ridge along Z at local x = 32 (rising 0.5 per metre
/// on the left, falling on the right), with an 8 x 8 cell hole on the right slope.
fn ridge() -> Heightfield {
    let heights = (0..65 * 65)
        .map(|i| 16.0 - 0.5 * ((i % 65) as f32 - 32.0).abs())
        .collect();
    let mut field = Heightfield::from_heights(65, 65, 1.0, heights).unwrap();
    for r in 28..36 {
        for c in 44..52 {
            field.set_hole(c, r, true).unwrap();
        }
    }
    field
}

fn lod() -> LodSettings {
    LodSettings {
        node_resolution: 8,
        lod_distance: 3.0,
        ..LodSettings::default()
    }
}

fn world_with(field: &Arc<Heightfield>, origin: Mat4) -> World {
    let mut world = World::new();
    world.insert_resource(TerrainRenderSet {
        items: vec![TerrainRenderItem {
            owner: Entity::PLACEHOLDER,
            id: "ridge".into(),
            field: field.clone(),
            lod: lod(),
            layers: vec![ResolvedLayer {
                name: "base".into(),
                tint: [0.8, 0.8, 0.8, 1.0],
                uv_tiling: 8.0,
                texture: None,
                image: None,
            }],
            world_from_local: origin.to_cols_array_2d(),
        }],
    });
    world
}

struct Offscreen {
    texture: wgpu::Texture,
    target: OutputTarget,
}
fn offscreen(gpu: &GpuContext) -> Offscreen {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain target"),
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
fn covered(p: [u8; 4]) -> bool {
    p[0] > 0 || p[1] > 0 || p[2] > 0
}
/// Linear luminance of an sRGB pixel.
fn luminance(p: [u8; 4]) -> f32 {
    let linear = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2])
}
/// Pixel of a world point.
fn pixel_of(view: &ViewData, p: Vec3) -> (u32, u32) {
    let clip = view.clip_from_world() * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x + 1.0) * 0.5 * SIZE[0] as f32) as u32,
        ((1.0 - ndc.y) * 0.5 * SIZE[1] as f32) as u32,
    )
}

fn sun() -> LightList {
    // Light travelling toward -X and down: the right (+X-facing) slope faces it.
    let toward = Vec3::new(1.0, 1.0, 0.0).normalize();
    LightList {
        lights: vec![Light::directional(-toward, Vec3::ONE, 1.0)],
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn terrain_renders_with_slope_shading_sky_and_a_hole() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let status = engine_world_render::TerrainRenderStatusHandle::default();
    let (color, normals) = TerrainFeature::pair(status.clone());
    pipeline.register(normals)?;
    pipeline.register(color)?;
    let field = Arc::new(ridge());
    // The terrain is centred on the world origin.
    let world = world_with(&field, Mat4::from_translation(Vec3::new(-32.0, 0.0, -32.0)));
    let eye = Vec3::new(0.0, 45.0, 60.0);
    let camera =
        glam::camera::rh::view::look_at_mat4(eye, Vec3::new(0.0, 0.0, -8.0), Vec3::Y).inverse();
    let mut view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE)?;
    view.ambient = Vec3::splat(0.05);
    view.host_needs.normals = true;
    let target = offscreen(&gpu);
    let report = pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &[],
            lights: &sun(),
            world: Some(&world),
        },
        &target.target,
        &mut NoHost,
    )?;
    gpu.check_errors()?;
    let pixels = engine_render_wgpu::read_texture_rgba(&gpu, &target.texture, SIZE[0], SIZE[1])?;
    if let Ok(path) = std::env::var("TERRAIN_DUMP") {
        std::fs::write(path, &pixels)?;
    }
    let state = status.get();
    eprintln!(
        "status {state:?}, draws {} calls {} instances {}",
        report.draws, report.draw_calls, report.instances
    );
    assert_eq!(
        report.features.get(names::TERRAIN).unwrap().activity,
        Activity::Ran
    );
    assert_eq!(
        report
            .features
            .get(names::TERRAIN_NORMALS)
            .unwrap()
            .activity,
        Activity::Ran
    );
    assert!(state.normals_pass);
    // One instanced call per pass for the single terrain.
    assert_eq!(state.draw_calls, 1);
    assert_eq!(state.terrains, 1);
    assert!(state.nodes >= 4, "{state:?}");
    assert_eq!(report.draw_calls, 2, "colour + normals passes");
    assert_eq!(report.instances, 2 * state.nodes);
    // Sky: the top rows see past the terrain (cleared HDR, black after the linear output).
    for x in [5, SIZE[0] / 2, SIZE[0] - 5] {
        assert!(!covered(pixel(&pixels, x, 2)), "sky at ({x}, 2)");
    }
    let coverage = pixels
        .chunks(4)
        .filter(|p| covered([p[0], p[1], p[2], p[3]]))
        .count() as f32
        / (SIZE[0] * SIZE[1]) as f32;
    eprintln!("terrain coverage {coverage:.3}");
    assert!((0.15..0.95).contains(&coverage), "coverage {coverage}");
    // Slope shading: the light-facing right slope is brighter than the left slope.
    let height = |x: f32| 16.0 - 0.5 * x.abs();
    let left = pixel_of(&view, Vec3::new(-16.0, height(-16.0), -20.0));
    let right = pixel_of(&view, Vec3::new(16.0, height(16.0), -20.0));
    let (l, r) = (
        pixel(&pixels, left.0, left.1),
        pixel(&pixels, right.0, right.1),
    );
    eprintln!("left {l:?} at {left:?}, right {r:?} at {right:?}");
    assert!(covered(l) && covered(r));
    // n.l is 0.95 on the right slope and 0.32 on the left (plus a little ambient).
    let ratio = luminance(r) / luminance(l);
    eprintln!("slope shading ratio {ratio:.2}");
    assert!(ratio > 2.0, "left {l:?} right {r:?}");
    // A horizontal gradient: brightness jumps at the ridge line, not inside each slope.
    let row = right.1;
    let step = |x: f32| {
        luminance(pixel(
            &pixels,
            pixel_of(&view, Vec3::new(x, height(x), -20.0)).0,
            row,
        ))
    };
    assert!((step(-24.0) - step(-8.0)).abs() < 0.25 * step(16.0));
    // The hole (local cells 44..52 x 28..36 = world x 12..20, z -4..4) shows the background.
    let hole = pixel_of(&view, Vec3::new(16.0, height(16.0), 0.0));
    assert!(!covered(pixel(&pixels, hole.0, hole.1)), "hole at {hole:?}");
    let beside = pixel_of(&view, Vec3::new(16.0, height(16.0), 8.0));
    assert!(covered(pixel(&pixels, beside.0, beside.1)));
    Ok(())
}

/// Coverage of the GPU-morphed terrain matches the CPU chunk meshes built for the same viewer.
#[test]
#[ignore = "requires a native GPU adapter"]
fn gpu_geomorphing_matches_the_cpu_chunk_meshes() -> Result<()> {
    let gpu = gpu()?;
    let heights = (0..129 * 129)
        .map(|i| {
            let (c, r) = ((i % 129) as f32, (i / 129) as f32);
            6.0 * engine_terrain::noise::fbm(c * 0.05, r * 0.05, 9)
        })
        .collect();
    let field = Arc::new(Heightfield::from_heights(129, 129, 1.0, heights).unwrap());
    let origin = Mat4::from_translation(Vec3::new(-64.0, 0.0, -64.0));
    let eye = Vec3::new(0.0, 4.0, 40.0);
    let camera =
        glam::camera::rh::view::look_at_mat4(eye, Vec3::new(0.0, 0.0, -20.0), Vec3::Y).inverse();
    let view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE)?;
    // GPU path.
    let mut gpu_pipeline = ScenePipeline::new(gpu.clone())?;
    let status = engine_world_render::TerrainRenderStatusHandle::default();
    let (color, _normals) = TerrainFeature::pair(status.clone());
    gpu_pipeline.register(color)?;
    let world = world_with(&field, origin);
    let target = offscreen(&gpu);
    gpu_pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &[],
            lights: &sun(),
            world: Some(&world),
        },
        &target.target,
        &mut NoHost,
    )?;
    let gpu_pixels =
        engine_render_wgpu::read_texture_rgba(&gpu, &target.texture, SIZE[0], SIZE[1])?;
    // CPU path: the same selection and morph, as unlit scene meshes.
    let layout = LodLayout::for_heightfield(&field, &lod()).unwrap();
    let local_eye = origin.inverse().transform_point3(eye);
    let selection = layout.select(local_eye, field.height_range());
    let morphing = selection
        .nodes
        .iter()
        .filter(|n| n.morph[0] < engine_terrain::lod::NO_MORPH)
        .count();
    eprintln!(
        "{} nodes, {} below the top level, levels {:?}",
        selection.nodes.len(),
        morphing,
        selection.level_histogram(layout.levels())
    );
    assert!(selection.level_histogram(layout.levels())[0] > 0);
    // Some vertices are mid-morph, so the comparison covers the GPU morph path.
    let n = layout.node_resolution();
    let partial = selection
        .nodes
        .iter()
        .flat_map(|node| (0..=n).flat_map(move |j| (0..=n).map(move |i| (node, i, j))))
        .filter(|(node, i, j)| {
            let [x, z] = engine_terrain::lod::vertex_sample(&field, node, *i, *j);
            let k = layout.morph_factor(
                node.key.level,
                selection.viewer.distance(x as f32, z as f32),
            );
            k > 0.0 && k < 1.0
        })
        .count();
    eprintln!("{partial} vertices mid-morph");
    assert!(partial > 100);
    let mut meshes = Vec::new();
    for node in &selection.nodes {
        let chunk = build_chunk_mesh(&field, &layout, node, &selection.viewer);
        let vertices: Vec<EngineVertex> = chunk
            .positions
            .iter()
            .map(|p| EngineVertex {
                position: *p,
                uv: [0.0; 2],
                color: [1.0; 4],
                normal: [0.0; 3],
            })
            .collect();
        meshes.push(GpuMesh::from_engine_vertices(
            &gpu.device,
            "cpu chunk",
            &vertices,
            &chunk.indices,
        )?);
    }
    let ids: Vec<String> = (0..meshes.len()).map(|i| format!("chunk{i}")).collect();
    let draws: Vec<SceneMesh<'_>> = meshes
        .iter()
        .zip(&ids)
        .map(|(mesh, id)| SceneMesh {
            id,
            mesh: mesh.clone(),
            world_from_local: origin,
            shader: None,
            params: MaterialParams {
                tint: [1.0; 4],
                unlit: true,
                ..Default::default()
            },
            base_color: None,
            metallic_roughness: None,
            normal_map: None,
        })
        .collect();
    let mut cpu_pipeline = ScenePipeline::new(gpu.clone())?;
    let cpu_target = offscreen(&gpu);
    cpu_pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &draws,
            lights: &LightList::default(),
            world: None,
        },
        &cpu_target.target,
        &mut NoHost,
    )?;
    gpu.check_errors()?;
    let cpu_pixels =
        engine_render_wgpu::read_texture_rgba(&gpu, &cpu_target.texture, SIZE[0], SIZE[1])?;
    let mut mismatched = 0;
    let mut terrain = 0;
    for (g, c) in gpu_pixels.chunks(4).zip(cpu_pixels.chunks(4)) {
        let (g, c) = (
            covered([g[0], g[1], g[2], 0]),
            covered([c[0], c[1], c[2], 0]),
        );
        terrain += usize::from(c);
        mismatched += usize::from(g != c);
    }
    let total = (SIZE[0] * SIZE[1]) as usize;
    eprintln!("terrain pixels {terrain}, coverage mismatches {mismatched} of {total}");
    assert!(terrain > total / 5 && terrain < total);
    assert!(
        mismatched as f32 <= 0.002 * total as f32,
        "{mismatched} pixels differ"
    );
    Ok(())
}
