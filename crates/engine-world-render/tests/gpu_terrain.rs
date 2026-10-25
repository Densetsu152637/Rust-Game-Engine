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

/// Every terrain feature registered on a fresh pipeline.
fn full_pipeline(
    gpu: &GpuContext,
) -> Result<(
    ScenePipeline,
    engine_world_render::TerrainRenderStatusHandle,
)> {
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let status = engine_world_render::TerrainRenderStatusHandle::default();
    let all = TerrainFeature::all(status.clone());
    pipeline.register(all.shadow)?;
    pipeline.register(all.normals)?;
    pipeline.register(all.color)?;
    pipeline.register(all.motion)?;
    Ok((pipeline, status))
}

/// `columns` x `columns` samples (spacing 1) from `height(local x, local z)`.
fn field_of(columns: u32, height: impl Fn(f32, f32) -> f32) -> Arc<Heightfield> {
    let heights = (0..columns * columns)
        .map(|i| height((i % columns) as f32, (i / columns) as f32))
        .collect();
    Arc::new(Heightfield::from_heights(columns, columns, 1.0, heights).unwrap())
}

/// A shadow-casting sun; `toward` points from surfaces to the light.
fn shadowed_sun(toward: Vec3) -> LightList {
    LightList {
        lights: vec![
            Light::directional(-toward.normalize(), Vec3::ONE, 1.0)
                .with_shadow(ShadowSettings::default()),
        ],
    }
}

fn render_once(
    pipeline: &mut ScenePipeline,
    gpu: &GpuContext,
    view: &ViewData,
    meshes: &[SceneMesh<'_>],
    lights: &LightList,
    world: &World,
) -> Result<(ViewReport, Vec<u8>)> {
    let target = offscreen(gpu);
    let report = pipeline.render_view(
        &SceneFrame {
            view,
            meshes,
            lights,
            world: Some(world),
        },
        &target.target,
        &mut NoHost,
    )?;
    gpu.check_errors()?;
    let pixels = engine_render_wgpu::read_texture_rgba(gpu, &target.texture, SIZE[0], SIZE[1])?;
    Ok((report, pixels))
}

/// A wall-like ridge (12 m high, along Z at local x = 32) on flat ground casts a terrain shadow
/// onto the flat ground on the side away from the light; disabling `world.terrain.shadow` per
/// view removes it.
#[test]
#[ignore = "requires a native GPU adapter"]
fn a_terrain_ridge_shadows_the_terrain_behind_it() -> Result<()> {
    let gpu = gpu()?;
    let (mut pipeline, status) = full_pipeline(&gpu)?;
    let field = field_of(65, |x, _| (12.0 - 3.0 * (x - 32.0).abs()).max(0.0));
    // Local (0, 0) at world (-32, 0, -32): the ridge crest runs along world x = 0.
    let world = world_with(&field, Mat4::from_translation(Vec3::new(-32.0, 0.0, -32.0)));
    // Sun toward +X, 31 degrees up: the ridge shadow reaches 12 / tan(31) = 20 m toward -X.
    let lights = shadowed_sun(Vec3::new(1.0, 0.6, 0.0));
    let eye = Vec3::new(0.0, 50.0, 45.0);
    let camera = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y).inverse();
    let mut view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE)?;
    view.ambient = Vec3::splat(0.05);
    let (report, pixels) = render_once(&mut pipeline, &gpu, &view, &[], &lights, &world)?;
    let state = status.get();
    eprintln!("status {state:?}");
    assert_eq!(
        report.features.get(names::TERRAIN_SHADOW).unwrap().activity,
        Activity::Ran
    );
    assert!(state.shadow_pass && state.shadowed, "{state:?}");
    // One call per terrain and cascade layer (4 layers).
    assert_eq!(state.shadow_draw_calls, 4);
    // Flat ground at world x = -10 (inside the shadow) and x = +18 (lit), z = 0.
    let shadowed = pixel_of(&view, Vec3::new(-10.0, 0.0, 0.0));
    let lit = pixel_of(&view, Vec3::new(18.0, 0.0, 0.0));
    let (s, l) = (
        pixel(&pixels, shadowed.0, shadowed.1),
        pixel(&pixels, lit.0, lit.1),
    );
    let ratio = luminance(s) / luminance(l);
    eprintln!("shadowed {s:?} at {shadowed:?}, lit {l:?} at {lit:?}, ratio {ratio:.3}");
    assert!(covered(s) && covered(l));
    // Shadowed ground keeps only the ambient term: albedo 0.8 x ambient 0.05 = 0.04 linear,
    // against 0.04 + 0.8 / pi x n.l 0.51 = 0.17 in the light.
    assert!(
        (luminance(s) - 0.04).abs() < 0.01,
        "shadowed luminance {}",
        luminance(s)
    );
    assert!(ratio < 0.3, "shadowed / lit luminance {ratio}");
    // Ground beyond the shadow on the same side is lit again (28 m from the crest).
    let beyond = pixel_of(&view, Vec3::new(-28.0, 0.0, 0.0));
    let beyond_ratio = luminance(pixel(&pixels, beyond.0, beyond.1)) / luminance(l);
    eprintln!("beyond the shadow {beyond_ratio:.3}");
    assert!(beyond_ratio > 0.8, "beyond {beyond_ratio}");

    // The same frame with the shadow feature disabled for the view: no terrain shadow.
    view.features.set(names::TERRAIN_SHADOW, false);
    let (report, pixels) = render_once(&mut pipeline, &gpu, &view, &[], &lights, &world)?;
    assert_eq!(
        report.features.get(names::TERRAIN_SHADOW).unwrap().activity,
        Activity::Disabled
    );
    assert!(!status.get().shadowed);
    let unshadowed =
        luminance(pixel(&pixels, shadowed.0, shadowed.1)) / luminance(pixel(&pixels, lit.0, lit.1));
    eprintln!("ratio with world.terrain.shadow disabled {unshadowed:.3}");
    assert!(unshadowed > 0.9, "unshadowed ratio {unshadowed}");
    Ok(())
}

/// A unit cube: 24 vertices (outward normals, counter-clockwise faces) and 36 indices.
fn cube() -> (Vec<EngineVertex>, Vec<u32>) {
    let faces = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y, Vec3::X),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, v) in faces {
        let base = vertices.len() as u32;
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            vertices.push(EngineVertex {
                position: ((n + u * a + v * b) * 0.5).to_array(),
                uv: [0.0; 2],
                color: [1.0; 4],
                normal: n.to_array(),
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

/// A floating engine box casts its (engine shadow atlas) shadow onto flat terrain.
#[test]
#[ignore = "requires a native GPU adapter"]
fn an_engine_box_shadows_the_terrain() -> Result<()> {
    let gpu = gpu()?;
    let (mut pipeline, _status) = full_pipeline(&gpu)?;
    let field = field_of(65, |_, _| 0.0);
    let world = world_with(&field, Mat4::from_translation(Vec3::new(-32.0, 0.0, -32.0)));
    // Light at 45 degrees toward +X: the box (x -15..-9, y 5..11) shadows the ground at
    // x - y, so x -26..-14, in view past the box.
    let lights = shadowed_sun(Vec3::new(1.0, 1.0, 0.0));
    let (vertices, indices) = cube();
    let mesh = GpuMesh::from_engine_vertices(&gpu.device, "box", &vertices, &indices)?;
    let meshes = [SceneMesh {
        id: "box",
        mesh,
        world_from_local: Mat4::from_scale_rotation_translation(
            Vec3::splat(6.0),
            glam::Quat::IDENTITY,
            Vec3::new(-12.0, 8.0, 0.0),
        ),
        shader: None,
        params: MaterialParams::default(),
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
    }];
    let eye = Vec3::new(0.0, 50.0, 45.0);
    let camera = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y).inverse();
    let mut view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE)?;
    view.ambient = Vec3::splat(0.05);
    let (_report, pixels) = render_once(&mut pipeline, &gpu, &view, &meshes, &lights, &world)?;
    let shadowed = pixel_of(&view, Vec3::new(-20.0, 0.0, 0.0));
    let lit = pixel_of(&view, Vec3::new(12.0, 0.0, 0.0));
    let (s, l) = (
        pixel(&pixels, shadowed.0, shadowed.1),
        pixel(&pixels, lit.0, lit.1),
    );
    let ratio = luminance(s) / luminance(l);
    eprintln!("box shadow {s:?} at {shadowed:?}, lit {l:?} at {lit:?}, ratio {ratio:.3}");
    assert!(covered(s) && covered(l));
    // Ambient only (0.04 linear) against 0.04 + 0.8 / pi x n.l 0.71 = 0.22 in the light.
    assert!(
        (luminance(s) - 0.04).abs() < 0.01,
        "shadowed luminance {}",
        luminance(s)
    );
    assert!(ratio < 0.3, "shadowed / lit luminance {ratio}");
    Ok(())
}

/// The view's motion vector at a world point's pixel.
fn motion_at(pipeline: &ScenePipeline, view: &ViewData, p: Vec3) -> Result<[f32; 2]> {
    let motion = pipeline.read_motion(ViewId::MAIN)?;
    let (x, y) = pixel_of(view, p);
    let m = motion.pixel(x, y);
    Ok([m[0], m[1]])
}

/// Render-pixel position of a world point.
fn pixel_position(view: &ViewData, p: Vec3) -> [f32; 2] {
    let clip = view.clip_from_world() * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    [
        (ndc.x + 1.0) * 0.5 * SIZE[0] as f32,
        (1.0 - ndc.y) * 0.5 * SIZE[1] as f32,
    ]
}

/// Expected current-to-previous motion of a world point that was at `before` in the previous
/// frame (seen by `previous`) and is at `now` (seen by `current`).
fn expected_motion(current: &ViewData, now: Vec3, previous: &ViewData, before: Vec3) -> [f32; 2] {
    let (c, p) = (
        pixel_position(current, now),
        pixel_position(previous, before),
    );
    [p[0] - c[0], p[1] - c[1]]
}

/// With motion needed (as DLSS requests it), `world.terrain.motion` writes camera motion for
/// terrain pixels (matching the analytic reprojection) and terrain-transform motion when the
/// terrain moves under a static camera; disabled, terrain pixels keep zero motion.
#[test]
#[ignore = "requires a native GPU adapter"]
fn terrain_motion_vectors_follow_the_camera_and_the_terrain_transform() -> Result<()> {
    let gpu = gpu()?;
    let (mut pipeline, status) = full_pipeline(&gpu)?;
    let field = field_of(65, |_, _| 0.0);
    let origin = Mat4::from_translation(Vec3::new(-32.0, 0.0, -32.0));
    let world = world_with(&field, origin);
    let lights = sun();
    let look = |x: f32| -> Result<ViewData> {
        let eye = Vec3::new(x, 30.0, 30.0);
        let camera =
            glam::camera::rh::view::look_at_mat4(eye, Vec3::new(x, 0.0, 0.0), Vec3::Y).inverse();
        let mut view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE)?;
        view.host_needs.motion = true;
        Ok(view)
    };
    let first = look(0.0)?;
    render_once(&mut pipeline, &gpu, &first, &[], &lights, &world)?;
    // The camera pans 2 m toward +X: terrain moves left on screen, so previous - current > 0.
    let second = look(2.0)?;
    let (report, pixels) = render_once(&mut pipeline, &gpu, &second, &[], &lights, &world)?;
    assert!(status.get().motion_pass, "{:?}", status.get());
    assert_eq!(
        report.features.get(names::TERRAIN_MOTION).unwrap().activity,
        Activity::Ran
    );
    let mut worst = 0.0_f32;
    for p in [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(-10.0, 0.0, 5.0),
        Vec3::new(12.0, 0.0, -8.0),
    ] {
        let (x, y) = pixel_of(&second, p);
        assert!(covered(pixel(&pixels, x, y)));
        let measured = motion_at(&pipeline, &second, p)?;
        let expected = expected_motion(&second, p, &first, p);
        eprintln!("camera motion at {p}: measured {measured:?}, expected {expected:?}");
        assert!(measured[0] > 1.0, "positive x motion {measured:?}");
        worst = worst
            .max((measured[0] - expected[0]).abs())
            .max((measured[1] - expected[1]).abs());
    }
    eprintln!("worst camera-motion error {worst:.3} px");
    // Within a pixel: the sampled pixel centre is up to half a pixel from the point.
    assert!(worst < 1.0, "worst error {worst}");
    // Sky keeps the cleared zero motion.
    let sky = pipeline.read_motion(ViewId::MAIN)?.pixel(SIZE[0] / 2, 1);
    assert_eq!([sky[0], sky[1]], [0.0, 0.0]);

    // Static camera, terrain moved 1.5 m toward -Z between frames: motion = projected shift.
    render_once(&mut pipeline, &gpu, &second, &[], &lights, &world)?;
    let moved = world_with(
        &field,
        Mat4::from_translation(Vec3::new(0.0, 0.0, -1.5)) * origin,
    );
    render_once(&mut pipeline, &gpu, &second, &[], &lights, &moved)?;
    let p = Vec3::new(2.0, 0.0, 0.0);
    let measured = motion_at(&pipeline, &second, p)?;
    // The terrain point now at p was at p + 1.5 z in the previous frame.
    let expected = expected_motion(&second, p, &second, p + Vec3::new(0.0, 0.0, 1.5));
    eprintln!("terrain-transform motion: measured {measured:?}, expected {expected:?}");
    assert!(
        measured[1] > 1.0,
        "the terrain moved away: it was lower on screen"
    );
    assert!(
        (measured[0] - expected[0]).abs() < 1.0 && (measured[1] - expected[1]).abs() < 1.0,
        "measured {measured:?} expected {expected:?}"
    );

    // world.terrain.motion disabled: the engine motion prepass leaves terrain pixels at zero.
    let mut disabled = look(4.0)?;
    disabled.features.set(names::TERRAIN_MOTION, false);
    let (report, _) = render_once(&mut pipeline, &gpu, &disabled, &[], &lights, &moved)?;
    assert_eq!(
        report.features.get(names::TERRAIN_MOTION).unwrap().activity,
        Activity::Disabled
    );
    let still = motion_at(&pipeline, &disabled, Vec3::new(4.0, 0.0, 0.0))?;
    assert_eq!(still, [0.0, 0.0], "no terrain motion without the feature");
    Ok(())
}
