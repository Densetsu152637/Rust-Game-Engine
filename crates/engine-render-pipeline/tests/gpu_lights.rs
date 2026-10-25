//! GPU tests of lights, clustering and shadows through the public pipeline
//! API. Measured values are printed; run with
//! `cargo test -p engine-render-pipeline --test gpu_lights -- --ignored --test-threads=1 --nocapture`.

use anyhow::Result;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Vec3};
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

fn gpu() -> Result<GpuContext> {
    let mut options = WgpuOptions::default();
    let mut request = DeviceFeatureRequest::new("gpu-lights-test");
    request.optional = wgpu::Features::TIMESTAMP_QUERY;
    options
        .requirements
        .request(request)
        .map_err(anyhow::Error::msg)?;
    let gpu = GpuContext::headless_with_options(&options)?;
    eprintln!("adapter: {:?}", gpu.adapter.get_info().name);
    Ok(gpu)
}

/// Irradiance (shading units, cosine included) of every light, with shadows.
const IRRADIANCE_WGSL: &str = "
#import engine::material
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
}
@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> Output {
    var out: Output;
    let world = engine_world_from_local(instance) * vec4<f32>(vertex.position, 1.0);
    out.position = engine_clip_from_world() * world;
    out.world = world.xyz;
    out.normal = (engine_normal_from_local(instance) * vec4<f32>(vertex.normal, 0.0)).xyz;
    return out;
}
@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    let n = normalize(input.normal);
    let v = normalize(engine_camera_position() - input.world);
    var e = vec3<f32>(0.0);
    for (var i = 0u; i < engine_light_count(); i++) {
        let light = engine_light(i);
        e += engine_light_incident(light, input.world, n, v).irradiance *
            engine_light_visibility(light, input.world, n);
    }
    return vec4<f32>(e, 1.0);
}
";

/// Light 0's shadow visibility (r) and cascade index (g).
fn visibility_wgsl() -> String {
    IRRADIANCE_WGSL.replace(
        "    var e = vec3<f32>(0.0);
    for (var i = 0u; i < engine_light_count(); i++) {
        let light = engine_light(i);
        e += engine_light_incident(light, input.world, n, v).irradiance *
            engine_light_visibility(light, input.world, n);
    }
    return vec4<f32>(e, 1.0);",
        "    let light = engine_light(0u);
    return vec4<f32>(engine_light_visibility(light, input.world, n),
        f32(engine_cascade_index(light, input.world)), 0.0, 1.0);",
    )
}

/// A horizontal square (`half` metres) facing +Y (counter-clockwise seen
/// from above).
fn floor_mesh(gpu: &GpuContext, half: f32) -> Arc<GpuMesh> {
    let vertex = |x, z| EngineVertex {
        position: [x, 0.0, z],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: [0.0, 1.0, 0.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "floor",
        &[
            vertex(-half, -half),
            vertex(half, -half),
            vertex(half, half),
            vertex(-half, half),
        ],
        &[0, 3, 2, 0, 2, 1],
    )
    .unwrap()
}

fn draw<'a>(
    id: &'a str,
    mesh: &Arc<GpuMesh>,
    world: Mat4,
    shader: Option<&'a str>,
) -> SceneMesh<'a> {
    SceneMesh {
        id,
        mesh: mesh.clone(),
        world_from_local: world,
        shader,
        params: MaterialParams {
            roughness: 0.6,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
    }
}

/// An orthographic camera looking straight down from `height`, covering
/// `extent` metres vertically; screen right is +X and screen down is +Z.
fn top_view(size: [u32; 2], center: Vec3, height: f32, extent: f32) -> ViewData {
    let aspect = size[0] as f32 / size[1] as f32;
    let projection = glam::camera::rh::proj::directx::orthographic(
        -extent * aspect * 0.5,
        extent * aspect * 0.5,
        -extent * 0.5,
        extent * 0.5,
        0.1,
        height * 2.0,
    );
    let world_from_view =
        Mat4::from_translation(center + Vec3::Y * height) * Mat4::from_rotation_x(-FRAC_PI_2);
    ViewData::new(ViewId::MAIN, world_from_view, projection, size, size).unwrap()
}

/// An orthographic camera looking straight up from `depth` below the floor
/// (it sees the floor's underside, never what is above it).
fn bottom_view(size: [u32; 2], depth: f32, extent: f32) -> ViewData {
    let mut view = top_view(size, Vec3::ZERO, depth, extent);
    view.world_from_view =
        Mat4::from_translation(Vec3::NEG_Y * depth) * Mat4::from_rotation_x(FRAC_PI_2);
    view
}

/// World floor point (y = 0) under pixel centre (`x`, `y`) of a top view.
fn floor_point(view: &ViewData, x: u32, y: u32) -> Vec3 {
    let ndc = glam::Vec4::new(
        (x as f32 + 0.5) / view.render_size[0] as f32 * 2.0 - 1.0,
        1.0 - (y as f32 + 0.5) / view.render_size[1] as f32 * 2.0,
        0.5,
        1.0,
    );
    let world = view.clip_from_world().inverse() * ndc;
    let world = world.truncate() / world.w;
    Vec3::new(world.x, 0.0, world.z)
}

/// Pixel of a world point.
fn pixel_of(view: &ViewData, point: Vec3) -> (u32, u32) {
    let clip = view.clip_from_world() * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x + 1.0) * 0.5 * view.render_size[0] as f32) as u32,
        ((1.0 - ndc.y) * 0.5 * view.render_size[1] as f32) as u32,
    )
}

struct Target {
    _texture: wgpu::Texture,
    target: OutputTarget,
}

fn target(gpu: &GpuContext, size: [u32; 2]) -> Target {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test target"),
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
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        size,
        viewport: Viewport::full(size),
    };
    Target {
        _texture: texture,
        target,
    }
}

fn render(
    pipeline: &mut ScenePipeline,
    view: &ViewData,
    meshes: &[SceneMesh<'_>],
    lights: &LightList,
) -> Result<(HdrImage, ViewReport)> {
    let output = target(pipeline.gpu(), view.output_size);
    let report = pipeline.render_view(
        &SceneFrame {
            view,
            meshes,
            lights,
            world: None,
        },
        &output.target,
        &mut NoHost,
    )?;
    Ok((pipeline.read_scene_hdr(view.id)?, report))
}

fn window(distance: f32, range: f32) -> f32 {
    let ratio = distance / range;
    (1.0 - ratio.powi(4)).clamp(0.0, 1.0).powi(2)
}

#[test]
#[ignore = "requires a GPU adapter"]
fn point_light_falloff_is_windowed_inverse_square_and_zero_beyond_range() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 20.0);
    let view = top_view([256, 256], Vec3::ZERO, 10.0, 16.0);
    let (height, range, candela) = (1.0f32, 6.0f32, 10.0f32);
    let lights = LightList {
        lights: vec![Light::point(
            Vec3::new(0.0, height, 0.0),
            Vec3::ONE,
            candela,
            range,
        )],
    };
    let meshes = [draw("floor", &floor, Mat4::IDENTITY, Some(IRRADIANCE_WGSL))];
    let (image, _) = render(&mut pipeline, &view, &meshes, &lights)?;
    let row = 128;
    let (mut previous, mut worst, mut worst_pure) = (f32::INFINITY, 0.0f32, 0.0f32);
    let mut zero_beyond = 0;
    for x in 128..256 {
        let point = floor_point(&view, x, row);
        let r = point.length();
        let d = (r * r + height * height).sqrt();
        let measured = image.pixel(x, row)[0];
        let expected = candela * window(d, range) / (d * d) * (height / d);
        if d >= range {
            assert_eq!(measured, 0.0, "beyond range at d = {d}");
            zero_beyond += 1;
            continue;
        }
        assert!(measured <= previous + 1e-6, "monotonic at d = {d}");
        previous = measured;
        if expected > 1e-3 {
            worst = worst.max((measured - expected).abs() / expected);
        }
        if d <= 0.35 * range {
            let pure = candela / (d * d) * (height / d);
            worst_pure = worst_pure.max((measured - pure).abs() / pure);
        }
    }
    eprintln!(
        "point light: max relative error vs windowed inverse-square {worst:.5}; vs pure \
         inverse-square within 0.35 range {worst_pure:.5}; {zero_beyond} pixels beyond range are 0"
    );
    assert!(worst < 0.01, "windowed inverse-square within 1%: {worst}");
    assert!(
        worst_pure < 0.035,
        "pure inverse-square within 3.5%: {worst_pure}"
    );
    assert!(zero_beyond > 20);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn spot_light_cone_is_dark_outside_full_inside_and_smooth_between() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 20.0);
    let view = top_view([256, 256], Vec3::ZERO, 10.0, 4.0);
    let (height, inner, outer, candela) = (2.0f32, 0.3f32, 0.5f32, 10.0f32);
    let lights = LightList {
        lights: vec![Light::spot(
            Vec3::new(0.0, height, 0.0),
            Vec3::NEG_Y,
            Vec3::ONE,
            candela,
            50.0,
            inner,
            outer,
        )],
    };
    let meshes = [draw("floor", &floor, Mat4::IDENTITY, Some(IRRADIANCE_WGSL))];
    let (image, _) = render(&mut pipeline, &view, &meshes, &lights)?;
    let (mut inside, mut outside, mut between) = (0, 0, 0);
    let mut worst = 0.0f32;
    let mut previous = f32::INFINITY;
    for x in 128..256 {
        let point = floor_point(&view, x, 128);
        let r = point.length();
        let d = (r * r + height * height).sqrt();
        let angle = (r / height).atan();
        let measured = image.pixel(x, 128)[0];
        let point_light = candela * window(d, 50.0) / (d * d) * (height / d);
        let factor = measured / point_light;
        if angle < inner {
            inside += 1;
            assert!(
                (factor - 1.0).abs() < 0.01,
                "full inside the inner cone: {factor}"
            );
        } else if angle > outer {
            outside += 1;
            assert_eq!(measured, 0.0, "unlit outside the outer cone");
        } else {
            between += 1;
            let t = ((angle.cos() - outer.cos()) / (inner.cos() - outer.cos())).clamp(0.0, 1.0);
            let smooth = t * t * (3.0 - 2.0 * t);
            worst = worst.max((factor - smooth).abs());
            assert!(measured <= previous + 1e-6, "monotonic falloff");
        }
        previous = measured;
    }
    eprintln!(
        "spot light: {inside} pixels inside the inner cone, {between} between (max smoothstep \
         error {worst:.4}), {outside} outside"
    );
    assert!(inside > 5 && between > 5 && outside > 5);
    assert!(worst < 0.01);
    Ok(())
}

/// Irradiance from a one-sided rectangle of unit luminance by midpoint
/// integration (`steps`^2 cells per unit area along each edge).
fn area_reference(center: Vec3, half: [f32; 2], point: Vec3, range: f32) -> f32 {
    let (nx, nz) = (400, 200);
    let cell = (2.0 * half[0] / nx as f32) * (2.0 * half[1] / nz as f32);
    let mut sum = 0.0f64;
    for i in 0..nx {
        for j in 0..nz {
            let x = -half[0] + (i as f32 + 0.5) * 2.0 * half[0] / nx as f32;
            let z = -half[1] + (j as f32 + 0.5) * 2.0 * half[1] / nz as f32;
            let sample = center + Vec3::new(x, 0.0, z);
            let to = sample - point;
            let d2 = to.length_squared();
            let cos_receiver = (to.y / d2.sqrt()).max(0.0);
            let cos_light = (to.y / d2.sqrt()).max(0.0);
            sum += f64::from(cos_receiver * cos_light / d2 * cell);
        }
    }
    sum as f32 * window(center.distance(point), range)
}

#[test]
#[ignore = "requires a GPU adapter"]
fn area_light_matches_numerical_integration() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 20.0);
    let view = top_view([128, 128], Vec3::ZERO, 10.0, 8.0);
    let center = Vec3::new(0.0, 1.5, 0.0);
    let (width, depth, range) = (2.0f32, 1.0f32, 20.0f32);
    let lights = LightList {
        lights: vec![Light {
            shape: LightShape::Area {
                width,
                height: depth,
                right: Vec3::X,
                two_sided: false,
            },
            color: Vec3::ONE,
            intensity: 1.0,
            position: center,
            direction: Vec3::NEG_Y,
            range,
            shadow: None,
        }],
    };
    let meshes = [draw("floor", &floor, Mat4::IDENTITY, Some(IRRADIANCE_WGSL))];
    let (image, _) = render(&mut pipeline, &view, &meshes, &lights)?;
    let mut worst = 0.0f32;
    let mut samples = 0;
    for y in (4..128).step_by(8) {
        for x in (4..128).step_by(8) {
            let point = floor_point(&view, x, y);
            let reference = area_reference(center, [width * 0.5, depth * 0.5], point, range);
            let measured = image.pixel(x, y)[0];
            worst = worst.max((measured - reference).abs() / reference.max(0.02));
            samples += 1;
        }
    }
    // Shape: the 2 m x 1 m light lights farther along X than along Z.
    let along_x = image.pixel(64 + 28, 64)[0];
    let along_z = image.pixel(64, 64 + 28)[0];
    eprintln!(
        "area light: max error vs numerical integration {worst:.5} over {samples} samples \
         (relative, absolute below 0.02); E(1.75 m along X) {along_x:.4} vs along Z {along_z:.4}; \
         centre {:.4}",
        image.pixel(64, 64)[0]
    );
    assert!(worst < 0.01, "{worst}");
    assert!(along_x > along_z * 1.2);
    Ok(())
}

fn lcg(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*seed >> 8) as f32 / (1u32 << 24) as f32
}

fn many_lights(count: usize, spacing: f32, range: f32) -> LightList {
    let side = (count as f32).sqrt().ceil() as usize;
    let mut seed = 7;
    LightList {
        lights: (0..count)
            .map(|i| {
                let (gx, gz) = ((i % side) as f32, (i / side) as f32);
                let offset = (side as f32 - 1.0) * 0.5 * spacing;
                let color = Vec3::new(lcg(&mut seed), lcg(&mut seed), lcg(&mut seed));
                Light::point(
                    Vec3::new(gx * spacing - offset, 0.3, gz * spacing - offset),
                    color,
                    0.5 + lcg(&mut seed),
                    range,
                )
            })
            .collect(),
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn clustered_lighting_of_1024_point_lights_matches_the_all_lights_path() -> Result<()> {
    let gpu = gpu()?;
    let timestamps = gpu
        .device
        .features()
        .contains(wgpu::Features::TIMESTAMP_QUERY);
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 40.0);
    let size = [640, 360];
    let projection = glam::camera::rh::proj::directx::perspective(1.0, 16.0 / 9.0, 0.1, 200.0);
    let camera = glam::camera::rh::view::look_at_mat4(
        Vec3::new(0.0, 9.0, 22.0),
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::Y,
    )
    .inverse();
    let mut view = ViewData::new(ViewId::MAIN, camera, projection, size, size)?;
    view.ambient = Vec3::splat(0.0);
    let meshes = [draw("floor", &floor, Mat4::IDENTITY, None)];
    let lights = many_lights(1024, 1.0, 1.4);
    // Twice per mode: the second frame reports the first frame's GPU stats.
    let mut clustered = None;
    for _ in 0..3 {
        clustered = Some(render(&mut pipeline, &view, &meshes, &lights)?);
        gpu.wait_idle()?;
    }
    let (clustered, report) = clustered.unwrap();
    let mut brute_view = view.clone();
    brute_view.features.0.insert(names::CLUSTERS.into(), false);
    let (brute, brute_report) = render(&mut pipeline, &brute_view, &meshes, &lights)?;
    let mut max = 0.0f32;
    let mut lit = 0;
    for (a, b) in clustered.pixels.iter().zip(&brute.pixels) {
        for c in 0..3 {
            max = max.max((a[c] - b[c]).abs());
        }
        if a[0] + a[1] + a[2] > 0.05 {
            lit += 1;
        }
    }
    let lighting = &report.lighting;
    let stats = lighting.cluster_stats.expect("cluster readback");
    eprintln!(
        "1024 lights: submitted {} culled {} clustered {} (mode {}), {} lit pixels, max channel \
         difference clustered vs all lights {max:e}; clusters: longest list {}, {} entries in {} \
         nonempty clusters, {} over capacity; cluster pass GPU time {}",
        lighting.submitted,
        lighting.culled,
        lighting.clustered,
        lighting.cluster_mode.name(),
        lit,
        stats.longest_list,
        stats.stored_entries,
        stats.nonempty_clusters,
        stats.overflowed_clusters,
        stats
            .gpu_nanoseconds
            .map_or("unavailable (no timestamp queries)".into(), |ns| format!(
                "{:.3} ms",
                ns as f64 / 1e6
            ))
    );
    assert_eq!(lighting.submitted, 1024);
    assert_eq!(lighting.clustered + lighting.culled, 1024);
    assert!(lighting.clustered > 600, "most lights are visible");
    assert_eq!(brute_report.lighting.cluster_mode, ClusterMode::AllLights);
    assert_eq!(stats.overflowed_clusters, 0);
    assert!(stats.longest_list > 0 && stats.nonempty_clusters > 100);
    assert!(lit > 50_000, "{lit}");
    assert!(max < 1e-4, "clustered equals all lights: {max}");
    if timestamps {
        assert!(stats.gpu_nanoseconds.is_some());
    }

    // Frame cost, clustered vs every light per fragment (wall time with a
    // GPU wait per frame).
    let time = |pipeline: &mut ScenePipeline, view: &ViewData, lights: &LightList| -> Result<f64> {
        render(pipeline, view, &meshes, lights)?;
        gpu.wait_idle()?;
        let start = std::time::Instant::now();
        for _ in 0..10 {
            let output = target(&gpu, size);
            pipeline.render_view(
                &SceneFrame {
                    view,
                    meshes: &meshes,
                    lights,
                    world: None,
                },
                &output.target,
                &mut NoHost,
            )?;
            gpu.wait_idle()?;
        }
        Ok(start.elapsed().as_secs_f64() * 100.0)
    };
    for count in [1024, 4096] {
        let lights = many_lights(count, 1024f32.sqrt() / (count as f32).sqrt(), 1.4);
        let clustered_ms = time(&mut pipeline, &view, &lights)?;
        let brute_ms = time(&mut pipeline, &brute_view, &lights)?;
        let report = pipeline.view_report(ViewId::MAIN).unwrap().lighting.clone();
        let stats = report.cluster_stats.unwrap_or_default();
        eprintln!(
            "{count} lights at 640x360: clustered frame {clustered_ms:.2} ms, all-lights frame \
             {brute_ms:.2} ms; cluster pass {}; longest list {}, {} clusters over capacity",
            stats
                .gpu_nanoseconds
                .map_or("n/a".into(), |ns| format!("{:.3} ms", ns as f64 / 1e6)),
            stats.longest_list,
            stats.overflowed_clusters
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn cascades_select_near_and_far_maps_and_shadow_their_occluders() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 200.0);
    let plate = floor_mesh(&gpu, 0.5);
    let size = [480, 270];
    let projection = glam::camera::rh::proj::directx::perspective(1.0, 16.0 / 9.0, 0.1, 500.0);
    let eye = Vec3::new(0.0, 1.5, 10.0);
    let camera =
        glam::camera::rh::view::look_at_mat4(eye, Vec3::new(0.0, 0.0, -10.0), Vec3::Y).inverse();
    let view = ViewData::new(ViewId::MAIN, camera, projection, size, size)?;
    let toward = Vec3::new(0.3, 1.0, 0.2).normalize();
    let shadow = ShadowSettings {
        resolution: 1024,
        cascades: CascadeSettings {
            count: 4,
            max_distance: 60.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let lights = LightList {
        lights: vec![Light::directional(-toward, Vec3::ONE, 3.0).with_shadow(shadow)],
    };
    let splits = cascade_splits(0.1, 60.0, 4, shadow.cascades.split_lambda);
    let near_plate = Vec3::new(0.0, 1.0, 7.3);
    let far_plate = Vec3::new(0.0, 1.0, -32.0);
    let visibility = visibility_wgsl();
    let scale = |s: f32| Mat4::from_scale(Vec3::new(s, 1.0, s));
    let meshes = [
        draw("floor", &floor, Mat4::IDENTITY, Some(&visibility)),
        draw(
            "near",
            &plate,
            Mat4::from_translation(near_plate) * scale(1.0),
            Some(&visibility),
        ),
        draw(
            "far",
            &plate,
            Mat4::from_translation(far_plate) * scale(6.0),
            Some(&visibility),
        ),
    ];
    let (image, report) = render(&mut pipeline, &view, &meshes, &lights)?;
    let view_depth = |p: Vec3| -(view.view_from_world().transform_point3(p)).z;
    let mut found = Vec::new();
    for plate in [near_plate, far_plate] {
        let shadow_center = plate - toward * (plate.y / toward.y);
        let (x, y) = pixel_of(&view, shadow_center);
        let [visible, cascade, ..] = image.pixel(x, y);
        let lit_point = shadow_center + Vec3::new(if plate.z > 0.0 { 1.5 } else { 8.0 }, 0.0, 0.0);
        let (lx, ly) = pixel_of(&view, lit_point);
        let [lit_visible, ..] = image.pixel(lx, ly);
        let depth = view_depth(shadow_center);
        let expected = splits.windows(2).position(|w| depth <= w[1]).unwrap() as f32;
        eprintln!(
            "plate at z {:.1}: shadow centre pixel ({x}, {y}) view depth {depth:.2} -> cascade \
             {cascade} (expected {expected}), visibility {visible:.3}; lit neighbour visibility \
             {lit_visible:.3}",
            plate.z
        );
        assert_eq!(cascade, expected);
        assert!(visible < 0.1, "shadowed: {visible}");
        assert!(lit_visible > 0.99, "lit: {lit_visible}");
        found.push(cascade);
    }
    eprintln!("splits {splits:?}; report {:?}", report.lighting);
    assert_eq!(found[0], 0.0, "near plate in the near cascade");
    assert_eq!(found[1], 3.0, "far plate in the far cascade");
    assert_eq!(report.lighting.shadow_views, 4);
    assert_eq!(report.lighting.atlas_pages, 1);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn cascade_shadow_edges_do_not_change_under_sub_texel_camera_motion() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 50.0);
    let plate = floor_mesh(&gpu, 0.7);
    let size = [256, 256];
    let extent = 8.0;
    let pixel = extent / size[1] as f32;
    let toward = Vec3::new(0.4, 1.0, 0.3).normalize();
    let shadow = ShadowSettings {
        resolution: 256,
        filter: ShadowFilter::Pcf { radius: 1 },
        cascades: CascadeSettings {
            count: 1,
            max_distance: 20.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let lights = LightList {
        lights: vec![Light::directional(-toward, Vec3::ONE, 3.0).with_shadow(shadow)],
    };
    let visibility = visibility_wgsl();
    let meshes = [
        draw("floor", &floor, Mat4::IDENTITY, Some(&visibility)),
        draw(
            "plate",
            &plate,
            Mat4::from_translation(Vec3::new(0.3, 1.0, -0.2)) * Mat4::from_rotation_y(0.35),
            Some(&visibility),
        ),
    ];
    let shift_pixels = 2;
    let shift = shift_pixels as f32 * pixel;
    let a_view = top_view(size, Vec3::ZERO, 10.0, extent);
    let b_view = top_view(size, Vec3::new(shift, 0.0, 0.0), 10.0, extent);
    let (a, _) = render(&mut pipeline, &a_view, &meshes, &lights)?;
    let (b, _) = render(&mut pipeline, &b_view, &meshes, &lights)?;
    let texel = engine_render_pipeline::shadows::cascade_view(
        a_view.world_from_view,
        a_view.clip_from_view,
        [0.1, 20.0],
        -toward,
        256,
        shadow.cascades.caster_extension,
    )
    .texel_world;
    let (mut edge, mut changed, mut max) = (0, 0, 0.0f32);
    for y in 0..size[1] {
        for x in 0..size[0] - shift_pixels {
            let va = a.pixel(x + shift_pixels, y)[0];
            let vb = b.pixel(x, y)[0];
            if va > 0.02 && va < 0.98 {
                edge += 1;
            }
            let difference = (va - vb).abs();
            max = max.max(difference);
            if difference > 1e-3 {
                changed += 1;
            }
        }
    }
    eprintln!(
        "CSM stability: camera moved {shift:.4} m ({:.2} shadow texels of {texel:.4} m); \
         {edge} shadow-edge pixels, {changed} changed (max difference {max:e})",
        shift / texel
    );
    assert!(shift < texel, "the motion is sub-texel");
    assert!(edge > 30, "the edge is in view: {edge}");
    assert_eq!(changed, 0);
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn spot_and_point_shadows_land_at_the_projected_occluder_position() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 20.0);
    let plate = floor_mesh(&gpu, 0.5);
    let view = top_view([256, 256], Vec3::ZERO, 10.0, 8.0);
    let visibility = visibility_wgsl();
    let shadow = ShadowSettings {
        resolution: 512,
        ..Default::default()
    };
    // Spot at 4 m, a 0.4 m plate at 2 m offset by 0.5 m: shadow centre at
    // x = 1 m on the floor, 0.8 m wide.
    let spot = LightList {
        lights: vec![
            Light::spot(
                Vec3::new(0.0, 4.0, 0.0),
                Vec3::NEG_Y,
                Vec3::ONE,
                10.0,
                20.0,
                0.5,
                0.7,
            )
            .with_shadow(shadow),
        ],
    };
    let scaled =
        |p: Vec3, s: f32| Mat4::from_translation(p) * Mat4::from_scale(Vec3::new(s, 1.0, s));
    let meshes = [
        draw("floor", &floor, Mat4::IDENTITY, Some(&visibility)),
        draw(
            "plate",
            &plate,
            scaled(Vec3::new(0.5, 2.0, 0.0), 0.4),
            Some(&visibility),
        ),
    ];
    let (image, report) = render(&mut pipeline, &view, &meshes, &spot)?;
    let sample = |p: Vec3| {
        let (x, y) = pixel_of(&view, p);
        image.pixel(x, y)[0]
    };
    let (center, below, beside) = (
        sample(Vec3::new(1.0, 0.0, 0.0)),
        sample(Vec3::new(0.0, 0.0, 0.0)),
        sample(Vec3::new(1.0, 0.0, 0.7)),
    );
    let (edge_in, edge_out) = (
        sample(Vec3::new(1.0, 0.0, 0.3)),
        sample(Vec3::new(1.0, 0.0, 0.5)),
    );
    eprintln!(
        "spot shadow: visibility at the projected centre {center:.3}, below the light {below:.3}, \
         0.7 m aside {beside:.3}; inside/outside the 0.4 m edge {edge_in:.3}/{edge_out:.3}"
    );
    assert!(center < 0.05 && edge_in < 0.2);
    assert!(below > 0.99 && beside > 0.99 && edge_out > 0.8);
    assert_eq!(report.lighting.shadow_views, 1);

    // Point light at 2 m: plates over the -Y face and the +X face.
    let point = LightList {
        lights: vec![
            Light::point(Vec3::new(0.0, 2.0, 0.0), Vec3::ONE, 10.0, 20.0).with_shadow(shadow),
        ],
    };
    let down_plate = Vec3::new(-0.6, 1.0, 0.4);
    let side_plate = Vec3::new(1.5, 1.0, 0.0);
    let meshes = [
        draw("floor", &floor, Mat4::IDENTITY, Some(&visibility)),
        draw("down", &plate, scaled(down_plate, 0.3), Some(&visibility)),
        draw("side", &plate, scaled(side_plate, 0.3), Some(&visibility)),
    ];
    let (image, report) = render(&mut pipeline, &view, &meshes, &point)?;
    let sample = |p: Vec3| {
        let (x, y) = pixel_of(&view, p);
        image.pixel(x, y)[0]
    };
    let light = Vec3::new(0.0, 2.0, 0.0);
    for (plate, face) in [(down_plate, "-Y"), (side_plate, "+X")] {
        let center = light + (plate - light) * (light.y / (light.y - plate.y));
        let lit = center + Vec3::new(0.0, 0.0, 1.0);
        let (shadowed, open) = (sample(center), sample(lit));
        eprintln!(
            "point shadow ({face} face): visibility at the projected centre ({:.2}, {:.2}) \
             {shadowed:.3}, 1 m aside {open:.3}",
            center.x, center.z
        );
        assert!(shadowed < 0.05, "{face}: {shadowed}");
        assert!(open > 0.99, "{face}: {open}");
    }
    assert_eq!(report.lighting.shadow_views, 6);
    assert_eq!(report.lighting.shadowed_lights, 1);
    Ok(())
}

/// Width (metres) of the 10%..90% visibility transition along +X at z = 0.
fn penumbra_width(view: &ViewData, image: &HdrImage) -> f32 {
    let y = view.render_size[1] / 2;
    let (mut low, mut high) = (None, None);
    for x in 0..view.render_size[0] {
        let v = image.pixel(x, y)[0];
        let at = floor_point(view, x, y).x;
        if v >= 0.1 && low.is_none() {
            low = Some(at);
        }
        if v >= 0.9 && high.is_none() {
            high = Some(at);
        }
    }
    high.unwrap() - low.unwrap()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn pcss_penumbrae_widen_with_blocker_receiver_distance() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let floor = floor_mesh(&gpu, 20.0);
    let blocker = floor_mesh(&gpu, 3.0);
    let view = bottom_view([512, 64], 10.0, 0.5);
    let visibility = visibility_wgsl();
    let light_height = 6.0;
    let source_radius = 0.3;
    let mut widths = Vec::new();
    for (filter, name) in [
        (
            ShadowFilter::Pcss {
                source_size: source_radius,
                max_radius: 32,
            },
            "pcss",
        ),
        (ShadowFilter::Pcf { radius: 2 }, "pcf"),
    ] {
        let lights = LightList {
            lights: vec![
                Light::spot(
                    Vec3::new(0.0, light_height, 0.0),
                    Vec3::NEG_Y,
                    Vec3::ONE,
                    10.0,
                    30.0,
                    0.5,
                    0.6,
                )
                .with_shadow(ShadowSettings {
                    resolution: 1024,
                    filter,
                    ..Default::default()
                }),
            ],
        };
        let mut row = Vec::new();
        for height in [0.5f32, 1.0, 2.0] {
            // A blocker covering x < 0 at `height` above the floor.
            let meshes = [
                draw("floor", &floor, Mat4::IDENTITY, Some(&visibility)),
                draw(
                    "blocker",
                    &blocker,
                    Mat4::from_translation(Vec3::new(-3.0, height, 0.0)),
                    Some(&visibility),
                ),
            ];
            let (image, _) = render(&mut pipeline, &view, &meshes, &lights)?;
            let width = penumbra_width(&view, &image);
            let receiver = light_height;
            let blocker_depth = light_height - height;
            let geometric = 2.0 * source_radius * (receiver - blocker_depth) / blocker_depth;
            eprintln!(
                "{name}: blocker {height} m above the receiver -> 10-90% penumbra {width:.4} m \
                 (geometric full penumbra {geometric:.4} m)"
            );
            row.push(width);
        }
        widths.push(row);
    }
    let pcss = &widths[0];
    assert!(pcss[0] < pcss[1] && pcss[1] < pcss[2], "{pcss:?}");
    assert!(pcss[2] > 2.5 * pcss[0], "{pcss:?}");
    let pcf = &widths[1];
    assert!(
        pcf[2] < 1.6 * pcf[0],
        "PCF width does not depend on the blocker distance: {pcf:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a GPU adapter"]
fn shadow_atlas_overflow_is_reported_and_shadows_still_render() -> Result<()> {
    let gpu = gpu()?;
    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    pipeline.set_shadow_atlas(ShadowAtlasConfig {
        page_size: 1024,
        max_pages: 1,
    })?;
    let floor = floor_mesh(&gpu, 20.0);
    let view = top_view([64, 64], Vec3::ZERO, 10.0, 8.0);
    let shadow = ShadowSettings {
        resolution: 256,
        ..Default::default()
    };
    // Each point light needs 6 x 256^2 texels; one 1024^2 page holds two.
    let lights = LightList {
        lights: (0..4)
            .map(|i| {
                Light::point(Vec3::new(i as f32, 2.0, 0.0), Vec3::ONE, 1.0, 10.0)
                    .with_shadow(shadow)
            })
            .collect(),
    };
    let meshes = [draw("floor", &floor, Mat4::IDENTITY, None)];
    let (_, report) = render(&mut pipeline, &view, &meshes, &lights)?;
    let lighting = &report.lighting;
    eprintln!("atlas overflow report: {lighting:?}");
    assert_eq!(lighting.shadowed_lights, 2);
    assert_eq!(lighting.shadow_dropped, 2);
    assert_eq!(lighting.shadow_views, 12);
    assert_eq!(lighting.atlas_texels_used, 12 * 256 * 256);
    assert!(lighting.overflow[0].contains("shadow atlas"));
    Ok(())
}
