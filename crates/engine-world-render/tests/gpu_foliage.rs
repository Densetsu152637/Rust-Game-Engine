//! GPU tests of `world.foliage`: render small fields offscreen and check pixels, draw calls,
//! culling, impostors and wind. Run with
//! `cargo test -p engine-world-render --test gpu_foliage -- --ignored --test-threads=1`.
use anyhow::Result;
use bevy_ecs::prelude::*;
use engine_foliage::{
    FoliageField, FoliageType, Impostor, Instance, LodBands, LodLevel, WindParams,
};
use engine_project::content::{Mesh, Texture, Vertex};
use engine_project::world::foliage::{
    FoliageRenderItem, FoliageRenderSet, FoliageRenderType, RenderImpostor,
};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use engine_world_render::foliage::{FoliageFeature, FoliageRenderStatusHandle};
use engine_world_render::names;
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

/// An upright quad of `width` x `height` standing on the origin in the XY plane.
fn quad(width: f32, height: f32) -> Arc<Mesh> {
    let hw = width * 0.5;
    let v = |x: f32, y: f32, u: f32, w: f32| Vertex {
        position: [x, y, 0.0],
        uv: [u, w],
        color: [1.0; 4],
    };
    Arc::new(Mesh {
        normals: Vec::new(),
        vertices: vec![
            v(-hw, 0.0, 0.0, 1.0),
            v(hw, 0.0, 1.0, 1.0),
            v(hw, height, 1.0, 0.0),
            v(-hw, height, 0.0, 0.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}

fn solid(rgb: [u8; 3]) -> Arc<Texture> {
    Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: [rgb[0], rgb[1], rgb[2], 255].repeat(4),
    })
}

fn render_type(
    name: &str,
    meshes: Vec<Arc<Mesh>>,
    color: [u8; 3],
    impostor: Option<([u8; 3], [f32; 2])>,
    lod: &[f32],
    impostor_distance: Option<f32>,
    cull: f32,
) -> FoliageRenderType {
    let mut t = FoliageType::new(name, "m");
    t.lod_meshes = (1..meshes.len()).map(|i| format!("m{i}")).collect();
    t.lod_distances = lod.to_vec();
    if let Some((_, size)) = impostor {
        t.impostor = Some(Impostor {
            texture: "card".into(),
            size,
        });
        t.impostor_distance = impostor_distance;
    }
    t.cull_distance = Some(cull);
    t.validate().unwrap();
    FoliageRenderType {
        name: name.to_owned(),
        meshes,
        texture: Some(solid(color)),
        tint: [1.0; 4],
        impostor: impostor.map(|(rgb, size)| RenderImpostor {
            texture: solid(rgb),
            size,
        }),
        bands: LodBands::for_type(&t, cull, 1.0, 0.1),
        wind_weight: 1.0,
    }
}

fn instance(type_index: u16, x: f32, z: f32) -> Instance {
    Instance::new(type_index, [x, 0.0, z])
}

fn world_with(item: FoliageRenderItem) -> World {
    let mut world = World::new();
    world.insert_resource(FoliageRenderSet { items: vec![item] });
    world
}

struct Offscreen {
    texture: wgpu::Texture,
    target: OutputTarget,
}
fn offscreen(gpu: &GpuContext) -> Offscreen {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("foliage target"),
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
fn pixel_of(view: &ViewData, p: Vec3) -> (u32, u32) {
    let clip = view.clip_from_world() * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x + 1.0) * 0.5 * SIZE[0] as f32) as u32,
        ((1.0 - ndc.y) * 0.5 * SIZE[1] as f32) as u32,
    )
}
fn sun() -> LightList {
    // Light travelling toward -Z and down: the +Z faces of the quads are lit.
    LightList {
        lights: vec![Light::directional(
            Vec3::new(0.0, -1.0, -1.0).normalize(),
            Vec3::ONE,
            2.0,
        )],
    }
}
fn make_view(eye: Vec3, target: Vec3, time: f32) -> ViewData {
    let camera = glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y).inverse();
    let mut view = ViewData::new(ViewId::MAIN, camera, projection(), SIZE, SIZE).unwrap();
    view.ambient = Vec3::splat(0.3);
    view.time = time;
    view
}

struct Frame {
    pixels: Vec<u8>,
    report: ViewReport,
}
fn render(
    gpu: &GpuContext,
    pipeline: &mut ScenePipeline,
    world: &World,
    view: &ViewData,
) -> Result<Frame> {
    render_scene(gpu, pipeline, world, view, &[], &sun())
}
fn render_scene(
    gpu: &GpuContext,
    pipeline: &mut ScenePipeline,
    world: &World,
    view: &ViewData,
    meshes: &[SceneMesh<'_>],
    lights: &LightList,
) -> Result<Frame> {
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
    Ok(Frame { pixels, report })
}

fn dominant(p: [u8; 4]) -> char {
    if p[1] >= p[0] && p[1] >= p[2] && p[1] > 0 {
        'g'
    } else if p[0] >= p[1] && p[0] >= p[2] && p[0] > 0 {
        'r'
    } else if p[2] > 0 {
        'b'
    } else {
        '-'
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn foliage_renders_lods_impostors_culling_and_batches() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let status = FoliageRenderStatusHandle::default();
    pipeline.register(FoliageFeature::new(status.clone()))?;

    // Type 0 "post": green LOD0 (4 m), green LOD1 (shorter), red impostor card beyond 50 m,
    // culled beyond 100 m. Type 1 "bush": blue, one level.
    let post = render_type(
        "post",
        vec![quad(0.8, 4.0), quad(0.8, 3.0)],
        [30, 200, 30],
        Some(([220, 30, 30], [3.0, 4.0])),
        &[20.0],
        Some(50.0),
        100.0,
    );
    let bush = render_type(
        "bush",
        vec![quad(2.0, 1.5)],
        [30, 30, 220],
        None,
        &[],
        None,
        100.0,
    );
    let mut field = FoliageField::new(32.0)?;
    let mut count = 0;
    // Row A, 10 m from the camera: LOD0. Row B, 28 m: LOD1. Row C, 60 m: impostors. Row D,
    // 104 m: culled by distance.
    for (z, n) in [(4.0, 5), (-14.0, 5), (-46.0, 5), (-90.0, 5)] {
        for i in 0..n {
            field.insert(instance(0, -6.0 + 3.0 * i as f32, z))?;
            count += 1;
        }
    }
    for x in [-4.5, -1.5, 1.5, 4.5] {
        field.insert(instance(1, x, -5.0))?;
        count += 1;
    }
    // Outside the frustum: far to the right at 40 m.
    for k in 0..3 {
        field.insert(instance(0, 40.0 + k as f32, 4.0))?;
        count += 1;
    }
    let item = FoliageRenderItem {
        owner: Entity::PLACEHOLDER,
        id: "plants".into(),
        types: vec![post.clone(), bush.clone()],
        field: Arc::new(field),
        wind: WindParams::default(),
    };
    let world = world_with(item);
    let view = make_view(Vec3::new(0.0, 3.0, 14.0), Vec3::new(0.0, 2.0, 0.0), 0.0);
    let frame = render(&gpu, &mut pipeline, &world, &view)?;
    let state = status.get();
    eprintln!("status {state:#?}");
    eprintln!(
        "report: draws {} calls {} instances {}",
        frame.report.draws, frame.report.draw_calls, frame.report.instances
    );
    assert_eq!(
        frame.report.features.get(names::FOLIAGE).unwrap().activity,
        Activity::Ran
    );
    assert!(state.notes.is_empty(), "{:?}", state.notes);
    // One draw call per (type, level) batch: post LOD0, post LOD1, post impostor, bush LOD0.
    assert_eq!(state.batches.len(), 4, "{:?}", state.batches);
    assert_eq!(state.draw_calls, 4);
    assert_eq!(frame.report.draw_calls, 4);
    let by = |type_name: &str, level: LodLevel| {
        state
            .batches
            .iter()
            .find(|b| b.type_name == type_name && b.level == level)
            .map(|b| b.instances)
    };
    assert_eq!(by("post", LodLevel::Mesh(0)), Some(5));
    assert_eq!(by("post", LodLevel::Mesh(1)), Some(5));
    assert_eq!(by("post", LodLevel::Impostor), Some(5));
    assert_eq!(by("bush", LodLevel::Mesh(0)), Some(4));
    assert_eq!(state.instances, 19);
    assert_eq!(state.impostors, 5);
    assert_eq!(frame.report.instances, 19);
    // Row D by distance, the far-right posts by frustum (their cell or themselves).
    assert_eq!(state.culled_distance, 5, "{state:?}");
    assert!(state.culled_frustum + state.cells_culled >= 1, "{state:?}");
    assert!(count > state.instances);
    // Pixels, one band per frame so rows do not hide each other: LOD0 and LOD1 posts are
    // green, impostors red, bushes blue, the culled row draws nothing.
    let mut probe_row = |ty: usize, z: f32, y: f32| -> Result<[u8; 4]> {
        let mut f = FoliageField::new(32.0)?;
        f.insert(instance(ty as u16, 0.0, z))?;
        let w = world_with(FoliageRenderItem {
            owner: Entity::PLACEHOLDER,
            id: "probe".into(),
            types: vec![post.clone(), bush.clone()],
            field: Arc::new(f),
            wind: WindParams::default(),
        });
        let fr = render(&gpu, &mut pipeline, &w, &view)?;
        let (px, py) = pixel_of(&view, Vec3::new(0.0, y, z));
        Ok(pixel(&fr.pixels, px, py))
    };
    let a = probe_row(0, 4.0, 2.0)?;
    let b = probe_row(0, -14.0, 1.5)?;
    let c = probe_row(0, -46.0, 2.0)?;
    let d = probe_row(1, -5.0, 0.7)?;
    eprintln!("A {a:?} B {b:?} C {c:?} bush {d:?}");
    assert_eq!(dominant(a), 'g', "{a:?}");
    assert_eq!(dominant(b), 'g', "{b:?}");
    assert_eq!(dominant(c), 'r', "{c:?}");
    assert_eq!(dominant(d), 'b', "{d:?}");
    let culled = probe_row(0, -90.0, 2.0)?;
    assert!(!covered(culled), "culled row drew {culled:?}");
    let coverage = frame
        .pixels
        .chunks(4)
        .filter(|p| covered([p[0], p[1], p[2], p[3]]))
        .count();
    eprintln!("covered pixels {coverage}");
    assert!(coverage > 600, "coverage {coverage}");
    Ok(())
}

/// A single tall blade with strong wind: the lean seen in pixels equals the CPU model.
#[test]
#[ignore = "requires a native GPU adapter"]
fn wind_sway_changes_pixels_and_matches_the_cpu_model() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let status = FoliageRenderStatusHandle::default();
    pipeline.register(FoliageFeature::new(status.clone()))?;
    let blade = render_type(
        "blade",
        vec![quad(0.6, 4.0)],
        [30, 200, 30],
        None,
        &[],
        None,
        200.0,
    );
    let mut field = FoliageField::new(32.0)?;
    let mut one = instance(0, 0.0, 0.0);
    one.phase = 0.25;
    field.insert(one)?;
    let wind = WindParams {
        direction_degrees: 0.0,
        strength: 1.5,
        sway_frequency: 0.5,
        gust_frequency: 0.1,
        gust_strength: 0.3,
        flutter: 0.4,
        wavelength: 20.0,
    };
    let item = |wind| FoliageRenderItem {
        owner: Entity::PLACEHOLDER,
        id: "blade".into(),
        types: vec![blade.clone()],
        field: Arc::new(field.clone()),
        wind,
    };
    let eye = Vec3::new(0.0, 2.0, 12.0);
    let target = Vec3::new(0.0, 2.0, 0.0);
    // Pick the two times, among a few candidates, whose CPU top-of-blade lean differs most.
    let lean = |t: f32| wind.sway_offset(t, one.position, one.phase, 1.0)[0];
    let times: Vec<f32> = (0..12).map(|i| i as f32 * 0.37).collect();
    let (t0, t1) = times
        .iter()
        .flat_map(|a| times.iter().map(move |b| (*a, *b)))
        .max_by(|x, y| {
            (lean(x.0) - lean(x.1))
                .abs()
                .total_cmp(&(lean(y.0) - lean(y.1)).abs())
        })
        .unwrap();
    eprintln!("t0 {t0} lean {} t1 {t1} lean {}", lean(t0), lean(t1));
    assert!((lean(t0) - lean(t1)).abs() > 0.8);

    let still = world_with(item(WindParams {
        strength: 0.0,
        ..wind
    }));
    let a = render(&gpu, &mut pipeline, &still, &make_view(eye, target, t0))?;
    let b = render(&gpu, &mut pipeline, &still, &make_view(eye, target, t1))?;
    assert_eq!(a.pixels, b.pixels, "no wind: time must not change pixels");

    let windy = world_with(item(wind));
    let a = render(&gpu, &mut pipeline, &windy, &make_view(eye, target, t0))?;
    let b = render(&gpu, &mut pipeline, &windy, &make_view(eye, target, t1))?;
    let changed = a
        .pixels
        .chunks(4)
        .zip(b.pixels.chunks(4))
        .filter(|(p, q)| p != q)
        .count();
    eprintln!("pixels changed between t0 and t1: {changed}");
    assert!(changed > 40, "wind changed only {changed} pixels");
    // The base stays planted: the bottom row of the blade does not move, the top does.
    let view = make_view(eye, target, t0);
    let centre_x = |frame: &Frame, y: f32| -> f32 {
        let (_, row) = pixel_of(&view, Vec3::new(0.0, y, 0.0));
        let xs: Vec<u32> = (0..SIZE[0])
            .filter(|x| covered(pixel(&frame.pixels, *x, row)))
            .collect();
        assert!(!xs.is_empty(), "no blade pixels at height {y}");
        (xs[0] + xs[xs.len() - 1]) as f32 * 0.5 + 0.5
    };
    let base_shift = centre_x(&b, 0.25) - centre_x(&a, 0.25);
    let top_shift = centre_x(&b, 3.5) - centre_x(&a, 3.5);
    // CPU prediction at the same heights (bend = (h / 4)^2).
    let predict = |y: f32, t: f32| {
        let bend = (y / 4.0).powi(2);
        let o = wind.sway_offset(t, one.position, one.phase, bend);
        let p = Vec3::new(o[0], y, o[2]);
        let clip = view.clip_from_world() * p.extend(1.0);
        (clip.x / clip.w + 1.0) * 0.5 * SIZE[0] as f32
    };
    let predicted_base = predict(0.25, t1) - predict(0.25, t0);
    let predicted_top = predict(3.5, t1) - predict(3.5, t0);
    eprintln!(
        "base shift {base_shift:.2} (cpu {predicted_base:.2}), top shift {top_shift:.2} (cpu {predicted_top:.2})"
    );
    assert!(predicted_top.abs() > 4.0, "{predicted_top}");
    assert!((base_shift - predicted_base).abs() < 1.1);
    assert!(
        (top_shift - predicted_top).abs() < 1.1,
        "gpu {top_shift} vs cpu {predicted_top}"
    );
    assert!(top_shift.abs() > 4.0 * base_shift.abs().max(0.5));
    Ok(())
}

/// A camera-culled leaf still casts, its alpha cutout leaves gaps, and wind moves its depth.
#[test]
#[ignore = "requires a native GPU adapter"]
fn offscreen_alpha_foliage_casts_wind_animated_shadows() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let mut features = PipelineFeatures::default();
    let status = engine_world_render::register_all(&mut features)?.foliage;
    pipeline.add_features(features.0)?;
    let leaf = |y: f32, z: f32, u: f32| Vertex {
        position: [0.0, y, z],
        uv: [u, 1.0 - y / 6.0],
        color: [1.0; 4],
    };
    let mesh = Arc::new(Mesh {
        vertices: vec![
            leaf(0.0, -1.0, 0.0),
            leaf(0.0, 1.0, 1.0),
            leaf(6.0, 1.0, 1.0),
            leaf(6.0, -1.0, 0.0),
        ],
        normals: vec![Vec3::X.to_array(); 4],
        indices: vec![0, 1, 2, 0, 2, 3],
    });
    let mut ty = render_type("leaf", vec![mesh], [100, 200, 100], None, &[], None, 100.0);
    ty.texture = Some(Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: [
            255, 255, 255, 255, 255, 255, 255, 0, 255, 255, 255, 255, 255, 255, 255, 0,
        ]
        .to_vec(),
    }));
    let mut field = FoliageField::new(32.0)?;
    field.insert(instance(0, 25.0, 0.0))?;
    field.insert(instance(0, -25.0, 0.0))?;
    let wind = WindParams {
        strength: 2.0,
        direction_degrees: 0.0,
        ..WindParams::default()
    };
    let world = world_with(FoliageRenderItem {
        owner: Entity::PLACEHOLDER,
        id: "leaf".into(),
        types: vec![ty],
        field: Arc::new(field),
        wind,
    });
    let ground_vertex = |x: f32, z: f32| EngineVertex {
        position: [x, 0.0, z],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: Vec3::Y.to_array(),
    };
    let ground = GpuMesh::from_engine_vertices(
        &gpu.device,
        "foliage receiver",
        &[
            ground_vertex(-20.0, -20.0),
            ground_vertex(-20.0, 20.0),
            ground_vertex(20.0, 20.0),
            ground_vertex(20.0, -20.0),
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
    let lights = LightList {
        lights: vec![
            Light::directional(Vec3::new(-1.0, -0.25, 0.0).normalize(), Vec3::ONE, 1.0)
                .with_shadow(ShadowSettings::default()),
        ],
    };
    let view0 = make_view(Vec3::new(0.0, 8.0, 15.0), Vec3::ZERO, 0.0);
    let mut disabled = view0.clone();
    disabled.features.set(names::FOLIAGE_SHADOW, false);
    let bare = render_scene(&gpu, &mut pipeline, &world, &disabled, &receiver, &lights)?;
    let cast0 = render_scene(&gpu, &mut pipeline, &world, &view0, &receiver, &lights)?;
    // A disjoint spot tile sees only the left leaf. Its tiny intensity leaves the
    // right receiver unchanged; if tile uploads alias, it overwrites the sun's
    // instance buffer and erases the right leaf's shadow.
    let mut two_lights = lights.clone();
    two_lights.lights.push(
        Light::spot(
            Vec3::new(-25.0, 8.0, 0.0),
            Vec3::NEG_Y,
            Vec3::ONE,
            0.00001,
            25.0,
            0.2,
            0.35,
        )
        .with_shadow(ShadowSettings::default()),
    );
    let cast_two = render_scene(&gpu, &mut pipeline, &world, &view0, &receiver, &two_lights)?;
    assert_eq!(
        cast_two.report.lighting.shadow_views,
        cast0.report.lighting.shadow_views + 1
    );
    let cast1 = render_scene(
        &gpu,
        &mut pipeline,
        &world,
        &make_view(Vec3::new(0.0, 8.0, 15.0), Vec3::ZERO, 1.7),
        &receiver,
        &lights,
    )?;
    assert_eq!(
        status.get().instances,
        0,
        "caster should be outside colour frustum"
    );
    let mut darker = 0;
    let mut cutout_lit = 0;
    let mut lost_with_second_tile = 0;
    let mut animated = 0;
    for ix in 0..49 {
        for iz in 0..33 {
            let (x, y) = pixel_of(
                &view0,
                Vec3::new(2.0 + ix as f32 * 0.125, 0.0, -2.0 + iz as f32 * 0.125),
            );
            if x >= SIZE[0] || y >= SIZE[1] {
                continue;
            }
            let a = pixel(&bare.pixels, x, y);
            let b = pixel(&cast0.pixels, x, y);
            let c = pixel(&cast1.pixels, x, y);
            let d = pixel(&cast_two.pixels, x, y);
            if a[0] > b[0].saturating_add(20) {
                darker += 1;
                if d[0] > b[0].saturating_add(15) {
                    lost_with_second_tile += 1;
                }
            }
            // At the same projected shadow length, the transparent half of the
            // alpha card must leave some receiver samples lit.
            if (4.0..=7.0).contains(&(2.0 + ix as f32 * 0.125))
                && (-0.8..=0.8).contains(&(-2.0 + iz as f32 * 0.125))
                && a[0] > 30
                && a[0].abs_diff(b[0]) <= 5
            {
                cutout_lit += 1;
            }
            if b != c {
                animated += 1;
            }
        }
    }
    assert!(
        darker > 0,
        "offscreen alpha foliage cast no receiver shadow"
    );
    assert!(animated > 0, "foliage shadow ignored wind time");
    assert!(cutout_lit > 0, "alpha cutout left no lit receiver samples");
    assert_eq!(
        lost_with_second_tile, 0,
        "later disjoint spot tile overwrote the sun caster buffer"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn shadow_toggle_preserves_foliage_colour_pose_at_lod_hysteresis_edge() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let mut features = PipelineFeatures::default();
    let status = engine_world_render::register_all(&mut features)?.foliage;
    pipeline.add_features(features.0)?;
    let kind = render_type(
        "post",
        vec![quad(0.8, 4.0), quad(0.8, 2.0)],
        [30, 200, 30],
        None,
        &[20.0],
        None,
        100.0,
    );
    let mut field = FoliageField::new(32.0)?;
    field.insert(instance(0, 0.0, 0.0))?;
    let world = world_with(FoliageRenderItem {
        owner: Entity::PLACEHOLDER,
        id: "post".into(),
        types: vec![kind],
        field: Arc::new(field),
        wind: WindParams::default(),
    });
    let lights = LightList {
        lights: vec![
            Light::directional(Vec3::new(0.0, -1.0, -1.0).normalize(), Vec3::ONE, 1.0)
                .with_shadow(ShadowSettings::default()),
        ],
    };
    let near = make_view(Vec3::new(0.0, 6.0, 19.0), Vec3::ZERO, 0.0);
    render_scene(&gpu, &mut pipeline, &world, &near, &[], &lights)?;
    let far = make_view(Vec3::new(0.0, 6.0, 21.0), Vec3::ZERO, 0.0);
    let mut disabled = far.clone();
    disabled.features.set(names::FOLIAGE_SHADOW, false);
    let bare = render_scene(&gpu, &mut pipeline, &world, &disabled, &[], &lights)?;
    assert_eq!(status.get().batches[0].level, LodLevel::Mesh(0));
    let cast = render_scene(&gpu, &mut pipeline, &world, &far, &[], &lights)?;
    assert_eq!(status.get().batches[0].level, LodLevel::Mesh(0));
    let coverage = |pixels: &[u8]| {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0] > 0 || p[1] > 0 || p[2] > 0)
            .collect::<Vec<_>>()
    };
    assert_eq!(coverage(&bare.pixels), coverage(&cast.pixels));

    // Compare the receiver shadow at the hysteresis edge with fixed-geometry
    // references. The retained long mesh reaches z < -3; the short mesh cannot.
    // This catches a shadow-only LOD mismatch even when colour coverage agrees.
    let ground_vertex = |x: f32, z: f32| EngineVertex {
        position: [x, 0.0, z],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: Vec3::Y.to_array(),
    };
    let ground = GpuMesh::from_engine_vertices(
        &gpu.device,
        "foliage LOD shadow receiver",
        &[
            ground_vertex(-8.0, -8.0),
            ground_vertex(-8.0, 8.0),
            ground_vertex(8.0, 8.0),
            ground_vertex(8.0, -8.0),
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
    let edge = render_scene(&gpu, &mut pipeline, &world, &far, &receiver, &lights)?;
    let reference = |height: f32| -> Result<World> {
        let mut field = FoliageField::new(32.0)?;
        field.insert(instance(0, 0.0, 0.0))?;
        Ok(world_with(FoliageRenderItem {
            owner: Entity::PLACEHOLDER,
            id: "post".into(),
            types: vec![render_type(
                "post",
                vec![quad(0.8, height)],
                [30, 200, 30],
                None,
                &[],
                None,
                100.0,
            )],
            field: Arc::new(field),
            wind: WindParams::default(),
        }))
    };
    let long = render_scene(
        &gpu,
        &mut pipeline,
        &reference(4.0)?,
        &far,
        &receiver,
        &lights,
    )?;
    let short = render_scene(
        &gpu,
        &mut pipeline,
        &reference(2.0)?,
        &far,
        &receiver,
        &lights,
    )?;
    let mut mismatched_long = 0;
    let mut distinct_short = 0;
    let mut receiver_samples = 0;
    for ix in 0..9 {
        for iz in 0..17 {
            let (x, y) = pixel_of(
                &far,
                Vec3::new(-0.4 + ix as f32 * 0.1, 0.0, -3.8 + iz as f32 * 0.1),
            );
            if x >= SIZE[0] || y >= SIZE[1] {
                continue;
            }
            let e = pixel(&edge.pixels, x, y);
            let a = pixel(&long.pixels, x, y);
            let b = pixel(&short.pixels, x, y);
            if !covered(a) || !covered(b) {
                continue;
            }
            receiver_samples += 1;
            if e[0].abs_diff(a[0]) > 5 {
                mismatched_long += 1;
            }
            if a[0].abs_diff(b[0]) > 15 {
                distinct_short += 1;
            }
        }
    }
    assert!(
        receiver_samples > 0 && distinct_short > 0,
        "reference meshes did not produce distinct ground shadows"
    );
    assert_eq!(
        mismatched_long, 0,
        "edge shadow footprint differed from retained Mesh(0)"
    );
    Ok(())
}
