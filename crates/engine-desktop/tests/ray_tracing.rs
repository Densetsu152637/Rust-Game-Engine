//! The hardware ray-traced lighting tier through the desktop renderer, with
//! its raster fallbacks. Ignored by default; run with
//! `--ignored --test-threads=1` on an adapter with Vulkan ray query (the
//! fallback halves run on any adapter).
use engine_desktop::{OffscreenSprites, RenderLimits, WgpuBackends};
use engine_project::{
    Camera2D,
    content::{Mesh, Vertex},
    lights::{DirectionalLight, LightInstance3D},
    raytracing::{RayTracing, RayTracingReport, RtEffectReport},
    rendering3d::*,
    runtime::RenderFrame,
};
use glam::{Mat4, Vec3};
use std::sync::Arc;

const SIZE: u32 = 96;
/// Orthographic view height (m).
const VIEW_HEIGHT: f32 = 10.0;

fn quad(corners: [[f32; 3]; 4], normal: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    (corners.to_vec(), vec![normal; 4], vec![0, 1, 2, 0, 2, 3])
}

fn mesh(parts: &[(Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>)]) -> Arc<Mesh> {
    let mut vertices = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();
    for (positions, part_normals, part_indices) in parts {
        let base = vertices.len() as u32;
        vertices.extend(positions.iter().map(|&position| Vertex {
            position,
            uv: [0.0; 2],
            color: [1.0; 4],
        }));
        normals.extend_from_slice(part_normals);
        indices.extend(part_indices.iter().map(|i| i + base));
    }
    Arc::new(Mesh {
        normals,
        vertices,
        indices,
    })
}

/// A counter-clockwise (seen from +y) floor of half size `h` at y = 0.
fn floor_mesh(h: f32) -> Arc<Mesh> {
    mesh(&[quad(
        [[-h, 0.0, h], [h, 0.0, h], [h, 0.0, -h], [-h, 0.0, -h]],
        [0.0, 1.0, 0.0],
    )])
}

/// An axis-aligned box from `min` to `max` with outward face normals.
fn box_mesh(min: [f32; 3], max: [f32; 3]) -> Arc<Mesh> {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    mesh(&[
        quad(
            [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
            [1.0, 0.0, 0.0],
        ),
        quad(
            [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
            [-1.0, 0.0, 0.0],
        ),
        quad(
            [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
            [0.0, 1.0, 0.0],
        ),
        quad(
            [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
            [0.0, -1.0, 0.0],
        ),
        quad(
            [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
            [0.0, 0.0, 1.0],
        ),
        quad(
            [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
            [0.0, 0.0, -1.0],
        ),
    ])
}

fn draw(id: &str, mesh: Arc<Mesh>, material: MeshMaterial) -> MeshDraw3D {
    MeshDraw3D {
        id: id.into(),
        mesh,
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: Mat4::IDENTITY.to_cols_array_2d(),
        material,
        shader: None,
    }
}

fn lit(tint: [f32; 3]) -> MeshMaterial {
    MeshMaterial {
        tint: [tint[0], tint[1], tint[2], 1.0],
        metallic: 0.0,
        roughness: 1.0,
        ..Default::default()
    }
}

/// Orthographic camera at `eye` looking at the origin.
fn camera(eye: Vec3, ambient: f32) -> CameraInstance3D {
    let up = if eye.x.abs() < 1e-3 && eye.z.abs() < 1e-3 {
        Vec3::NEG_Z
    } else {
        Vec3::Y
    };
    let mut camera = Camera3D {
        projection: Projection3D::Orthographic {
            height: VIEW_HEIGHT,
            near: 0.1,
            far: 100.0,
        },
        ..Default::default()
    };
    camera.lighting.ambient = ambient;
    camera.lighting.exposure = 1.0;
    CameraInstance3D {
        camera,
        world: glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, up)
            .inverse()
            .to_cols_array_2d(),
    }
}

/// Pixel of world point `p` for `camera` (orthographic, square output).
fn pixel(camera: &CameraInstance3D, p: Vec3) -> (u32, u32) {
    let view = Mat4::from_cols_array_2d(&camera.world).inverse();
    let v = view.transform_point3(p);
    let half = VIEW_HEIGHT * 0.5;
    let x = (v.x / half * 0.5 + 0.5) * SIZE as f32;
    let y = (0.5 - v.y / half * 0.5) * SIZE as f32;
    (x as u32, y as u32)
}

fn frame(camera: CameraInstance3D, meshes: Vec<MeshDraw3D>) -> SceneRenderFrame {
    let mut frame = SceneRenderFrame::from(RenderFrame {
        camera: Camera2D {
            logical_size: [SIZE, SIZE],
            ..Default::default()
        },
        draws: vec![],
    });
    frame.camera3d = Some(camera);
    frame.meshes = meshes;
    frame
}

fn sun(toward: [f32; 3], lux: f32, shadow_maps: bool) -> LightInstance3D {
    let mut light = DirectionalLight {
        lux,
        ..Default::default()
    };
    light.shadows.enabled = shadow_maps;
    LightInstance3D::directional_toward("sun", toward, light).unwrap()
}

fn rgb(pixels: &[u8], (x, y): (u32, u32)) -> [f32; 3] {
    // 3 x 3 mean around the pixel.
    let mut sum = [0.0f32; 3];
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let px = (x as i32 + dx).clamp(0, SIZE as i32 - 1) as u32;
            let py = (y as i32 + dy).clamp(0, SIZE as i32 - 1) as u32;
            let at = ((py * SIZE + px) * 4) as usize;
            for c in 0..3 {
                sum[c] += f32::from(pixels[at + c]) / 9.0;
            }
        }
    }
    sum
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn render(renderer: &mut OffscreenSprites, frame: &SceneRenderFrame, frames: usize) -> Vec<u8> {
    for _ in 0..frames {
        renderer.render_scene(frame).unwrap();
    }
    renderer.read_rgba().unwrap()
}

fn report(renderer: &mut OffscreenSprites) -> RayTracingReport {
    renderer
        .gpu_renderer()
        .unwrap()
        .ray_tracing_status()
        .cloned()
        .expect("a frame with RT features reports ray tracing")
}

fn effect<'a>(report: &'a RayTracingReport, name: &str) -> &'a RtEffectReport {
    report.effects.iter().find(|e| e.effect == name).unwrap()
}

fn rt_renderer() -> OffscreenSprites {
    OffscreenSprites::wgpu_ray_traced(WgpuBackends::Vulkan, SIZE, SIZE, RenderLimits::default())
        .unwrap()
}

fn plain_renderer() -> OffscreenSprites {
    OffscreenSprites::wgpu(WgpuBackends::Vulkan, SIZE, SIZE, RenderLimits::default()).unwrap()
}

// ------------------------------------------------------------ shadows

fn shadow_scene(rt: bool, shadow_maps: bool) -> (SceneRenderFrame, (u32, u32), (u32, u32)) {
    let cam = camera(Vec3::new(0.0, 10.0, 0.0), 0.02);
    let occluded = pixel(&cam, Vec3::new(-2.0, 0.0, 0.0));
    let open = pixel(&cam, Vec3::new(3.0, 0.0, 2.5));
    let mut f = frame(
        cam,
        vec![
            draw("floor", floor_mesh(8.0), lit([0.8; 3])),
            draw(
                "box",
                box_mesh([-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]),
                lit([0.8; 3]),
            ),
        ],
    );
    // Sun along (1, 1, 0): the box shadows x in [-3, -1].
    f.lights = vec![sun([1.0, 1.0, 0.0], 2.0, shadow_maps)];
    if rt {
        let mut settings = RayTracing::default();
        settings.shadows.enabled = true;
        f.ray_tracing = Some(settings);
    }
    (f, occluded, open)
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query"]
fn rt_shadows_darken_the_occluded_floor_and_fall_back_to_shadow_maps() {
    // Ray traced, with the shadow maps off: the shadow comes from the rays.
    let mut renderer = rt_renderer();
    let (scene, occluded, open) = shadow_scene(true, false);
    let pixels = render(&mut renderer, &scene, 1);
    let status = report(&mut renderer);
    let shadows = effect(&status, "shadows");
    let (dark, bright) = (luma(rgb(&pixels, occluded)), luma(rgb(&pixels, open)));
    eprintln!(
        "RT: occluded {dark:.1}, lit {bright:.1}, status {shadows:?}, tier {}",
        status.tier
    );
    assert_eq!(status.tier, "hardware", "{status:?}");
    assert_eq!(shadows.mode, "ray_traced", "{shadows:?}");
    assert!(bright > 3.0 * dark, "lit {bright} vs occluded {dark}");
    assert!(
        status.blas_count == 2 && status.instance_count == 2,
        "{status:?}"
    );
    // A second identical frame builds no BLAS (incremental sync).
    render(&mut renderer, &scene, 1);
    assert_eq!(report(&mut renderer).last_blas_built, 0);

    // Without RT shadows and without shadow maps the floor is not shadowed.
    let (plain, _, _) = shadow_scene(false, false);
    let pixels = render(&mut renderer, &plain, 1);
    let unshadowed = luma(rgb(&pixels, occluded));
    eprintln!("no shadows: occluded pixel {unshadowed:.1}");
    assert!(unshadowed > 2.0 * dark);

    // A device created without the RT request: shadow maps, honest status.
    let mut fallback = plain_renderer();
    let (scene, occluded, open) = shadow_scene(true, true);
    let pixels = render(&mut fallback, &scene, 1);
    let status = report(&mut fallback);
    let shadows = effect(&status, "shadows");
    let (dark, bright) = (luma(rgb(&pixels, occluded)), luma(rgb(&pixels, open)));
    eprintln!("fallback: occluded {dark:.1}, lit {bright:.1}, status {shadows:?}");
    assert_eq!(status.tier, "unavailable");
    assert_eq!(shadows.mode, "fallback");
    assert_eq!(shadows.fallback.as_deref(), Some("shadow_maps"));
    let reason = shadows.reason.as_deref().unwrap();
    assert!(
        reason.contains("EXPERIMENTAL_RAY_QUERY") && reason.contains("raster fallback"),
        "{reason}"
    );
    assert!(
        bright > 3.0 * dark,
        "shadow maps: lit {bright} vs occluded {dark}"
    );
}

// ----------------------------------------------------------------- AO

fn ao_scene(ao: Option<(u32, bool)>) -> (SceneRenderFrame, (u32, u32), (u32, u32)) {
    let cam = camera(Vec3::new(6.0, 6.0, 3.0), 0.5);
    let crease = pixel(&cam, Vec3::new(0.15, 0.0, 0.0));
    let open = pixel(&cam, Vec3::new(3.5, 0.0, 0.0));
    let wall = mesh(&[quad(
        [
            [0.0, 0.0, 6.0],
            [0.0, 0.0, -6.0],
            [0.0, 6.0, -6.0],
            [0.0, 6.0, 6.0],
        ],
        [1.0, 0.0, 0.0],
    )]);
    let mut f = frame(
        cam,
        vec![
            draw("floor", floor_mesh(8.0), lit([0.8; 3])),
            draw("wall", wall, lit([0.8; 3])),
        ],
    );
    if let Some((rays, denoise)) = ao {
        let mut settings = RayTracing::default();
        settings.ambient_occlusion.enabled = true;
        settings.ambient_occlusion.radius = 1.0;
        settings.ambient_occlusion.rays_per_pixel = rays;
        settings.ambient_occlusion.denoise = denoise;
        f.ray_tracing = Some(settings);
    }
    (f, crease, open)
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query"]
fn ray_traced_and_ssao_occlusion_darken_the_crease() {
    let mut renderer = rt_renderer();
    let (none, crease, open) = ao_scene(None);
    let pixels = render(&mut renderer, &none, 1);
    let (crease_none, open_none) = (luma(rgb(&pixels, crease)), luma(rgb(&pixels, open)));
    let (scene, _, _) = ao_scene(Some((16, true)));
    let pixels = render(&mut renderer, &scene, 8);
    let (crease_rt, open_rt) = (luma(rgb(&pixels, crease)), luma(rgb(&pixels, open)));
    let status = report(&mut renderer);
    let ao = effect(&status, "ambient_occlusion");
    assert_eq!(ao.mode, "ray_traced", "{ao:?}");
    assert!(ao.denoised);

    let mut fallback = plain_renderer();
    let pixels = render(&mut fallback, &scene, 1);
    let (crease_ssao, open_ssao) = (luma(rgb(&pixels, crease)), luma(rgb(&pixels, open)));
    let status = report(&mut fallback);
    let ssao = effect(&status, "ambient_occlusion");
    eprintln!(
        "crease: none {crease_none:.1}, RT {crease_rt:.1}, SSAO {crease_ssao:.1}; open: none \
         {open_none:.1}, RT {open_rt:.1}, SSAO {open_ssao:.1}; fallback status {ssao:?}"
    );
    assert_eq!(ssao.mode, "fallback");
    assert_eq!(ssao.fallback.as_deref(), Some("ssao"));
    assert!(crease_rt < 0.85 * crease_none, "RT AO darkens the crease");
    assert!(crease_ssao < 0.92 * crease_none, "SSAO darkens the crease");
    assert!(open_rt > 0.95 * open_none && open_ssao > 0.95 * open_none);
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query"]
fn denoised_ao_is_temporally_stable_and_a_camera_cut_resets_it() {
    let mut renderer = rt_renderer();
    let (scene, crease, _) = ao_scene(Some((1, true)));
    // Mean absolute difference over a 24 x 24 window around the crease.
    let window = |pixels: &[u8]| -> Vec<f32> {
        let mut out = Vec::new();
        for y in crease.1.saturating_sub(12)..(crease.1 + 12).min(SIZE) {
            for x in crease.0.saturating_sub(12)..(crease.0 + 12).min(SIZE) {
                out.push(f32::from(pixels[((y * SIZE + x) * 4) as usize]));
            }
        }
        out
    };
    let diff = |a: &[f32], b: &[f32]| {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
    };
    let converged = window(&render(&mut renderer, &scene, 24));
    let next = window(&render(&mut renderer, &scene, 1));
    let stable = diff(&converged, &next);
    assert!(effect(&report(&mut renderer), "ambient_occlusion").denoised);
    renderer.gpu_renderer().unwrap().reset_temporal_history();
    let cut = window(&render(&mut renderer, &scene, 1));
    let after_cut = diff(&next, &cut);

    let (raw, _, _) = ao_scene(Some((1, false)));
    let a = window(&render(&mut renderer, &raw, 1));
    let b = window(&render(&mut renderer, &raw, 1));
    let noisy = diff(&a, &b);
    eprintln!(
        "temporal difference (8-bit): denoised static {stable:.3}, after a camera cut \
         {after_cut:.3}, undenoised {noisy:.3}"
    );
    assert!(stable < 1.0, "static denoised frames differ by {stable}");
    assert!(
        stable < 0.25 * noisy,
        "denoising lowers the frame-to-frame noise"
    );
    assert!(
        after_cut > 2.0 * stable,
        "the cut drops the accumulated history"
    );
}

// --------------------------------------------------------- reflections

fn mirror_scene(rt: bool) -> (SceneRenderFrame, (u32, u32), (u32, u32)) {
    let cam = camera(Vec3::new(0.0, 5.0, 5.0), 0.05);
    // The mirror ray of floor point (0, 0, 2) hits the box front at y = 1.
    let reflecting = pixel(&cam, Vec3::new(0.0, 0.0, 2.0));
    // Floor point (3.5, 0, 2) reflects only the sky.
    let empty = pixel(&cam, Vec3::new(3.5, 0.0, 2.0));
    let mirror = MeshMaterial {
        tint: [1.0; 4],
        metallic: 1.0,
        roughness: 0.0,
        ..Default::default()
    };
    let red = MeshMaterial {
        tint: [1.0, 0.05, 0.05, 1.0],
        unlit: true,
        ..Default::default()
    };
    let mut f = frame(
        cam,
        vec![
            draw("floor", floor_mesh(8.0), mirror),
            draw("box", box_mesh([-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]), red),
        ],
    );
    if rt {
        let mut settings = RayTracing::default();
        settings.reflections.enabled = true;
        f.ray_tracing = Some(settings);
    }
    (f, reflecting, empty)
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query"]
fn a_mirror_floor_reflects_the_box_only_when_reflections_are_traced() {
    let mut renderer = rt_renderer();
    let (scene, reflecting, empty) = mirror_scene(true);
    let pixels = render(&mut renderer, &scene, 4);
    let (hit, sky) = (rgb(&pixels, reflecting), rgb(&pixels, empty));
    let status = report(&mut renderer);
    let reflections = effect(&status, "reflections");
    eprintln!("RT: reflecting {hit:?}, sky-only {sky:?}, status {reflections:?}");
    assert_eq!(reflections.mode, "ray_traced", "{reflections:?}");
    assert!(
        hit[0] > 200.0 && hit[0] > 3.0 * hit[1],
        "the box colour is reflected"
    );
    assert!(sky[0] < 0.5 * hit[0]);

    let (off, _, _) = mirror_scene(false);
    let pixels = render(&mut renderer, &off, 1);
    let none = rgb(&pixels, reflecting);
    let status = report(&mut renderer);
    eprintln!("off: reflecting pixel {none:?}");
    assert_eq!(effect(&status, "reflections").mode, "disabled");
    assert!(none[0] < 0.3 * hit[0], "no reflection without the effect");

    let mut fallback = plain_renderer();
    let pixels = render(&mut fallback, &scene, 1);
    let honest = rgb(&pixels, reflecting);
    let status = report(&mut fallback);
    let reflections = effect(&status, "reflections");
    eprintln!("fallback: reflecting pixel {honest:?}, status {reflections:?}");
    assert_eq!(reflections.mode, "fallback");
    assert_eq!(reflections.fallback.as_deref(), Some("none"));
    assert!(honest[0] < 0.3 * hit[0]);
}

// ------------------------------------------------------------------ GI

fn gi_scene(gi: bool) -> (SceneRenderFrame, (u32, u32)) {
    let cam = camera(Vec3::new(6.0, 6.0, 3.0), 0.02);
    let near_wall = pixel(&cam, Vec3::new(0.3, 0.0, 0.0));
    let wall = mesh(&[quad(
        [
            [0.0, 0.0, 6.0],
            [0.0, 0.0, -6.0],
            [0.0, 6.0, -6.0],
            [0.0, 6.0, 6.0],
        ],
        [1.0, 0.0, 0.0],
    )]);
    let mut f = frame(
        cam,
        vec![
            draw("floor", floor_mesh(8.0), lit([0.8; 3])),
            draw("wall", wall, lit([0.8, 0.05, 0.05])),
        ],
    );
    f.lights = vec![sun([1.0, 1.0, 0.0], 3.0, false)];
    if gi {
        let mut settings = RayTracing::default();
        settings.diffuse_gi.enabled = true;
        settings.diffuse_gi.rays_per_pixel = 16;
        settings.diffuse_gi.bounces = 2;
        f.ray_tracing = Some(settings);
    }
    (f, near_wall)
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query"]
fn diffuse_gi_bleeds_the_red_wall_onto_the_white_floor() {
    let mut renderer = rt_renderer();
    let (off, near_wall) = gi_scene(false);
    let plain = rgb(&render(&mut renderer, &off, 1), near_wall);
    let (on, _) = gi_scene(true);
    let bled = rgb(&render(&mut renderer, &on, 8), near_wall);
    let status = report(&mut renderer);
    let gi = effect(&status, "diffuse_gi");
    let ratio = |c: [f32; 3]| c[0] / c[1].max(1.0);
    eprintln!(
        "floor near the wall: GI off {plain:?} (R/G {:.3}), GI on {bled:?} (R/G {:.3}); {gi:?}",
        ratio(plain),
        ratio(bled)
    );
    assert_eq!(gi.mode, "ray_traced", "{gi:?}");
    assert!(ratio(plain) < 1.03, "white floor without GI");
    assert!(ratio(bled) > 1.06, "red bleeding with GI");

    let mut fallback = plain_renderer();
    let constant = rgb(&render(&mut fallback, &on, 1), near_wall);
    let status = report(&mut fallback);
    let gi = effect(&status, "diffuse_gi");
    eprintln!("fallback: {constant:?}, {gi:?}");
    assert_eq!(gi.mode, "fallback");
    assert_eq!(gi.fallback.as_deref(), Some("constant_ambient"));
    assert!(ratio(constant) < 1.03);
}
