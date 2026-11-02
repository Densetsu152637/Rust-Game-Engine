//! The native temporal resolve through the desktop GPU renderer: authored
//! `Camera3D.upscaler` -> jitter owner and Upscale winner -> history -> pixels.
//! Run with `--ignored --test-threads=1` on a native adapter.

use anyhow::Result;
use engine_desktop::{GpuContext, GpuRenderer, OffscreenTarget, RenderLimits, WgpuOptions};
use engine_project::{
    Camera2D, Project, Runtime,
    content::{Mesh, Vertex},
    rendering3d::*,
    runtime::RenderFrame,
};
use glam::{Mat4, Vec3};
use serde_json::json;
use std::sync::Arc;

const SIZE: [u32; 2] = [65, 43];

fn triangle() -> Arc<Mesh> {
    Arc::new(Mesh {
        uv2: vec![],
        normals: vec![],
        vertices: [[-1.4, -0.9, 0.0], [1.4, -0.9, 0.0], [0.3, 0.9, 0.0]]
            .into_iter()
            .map(|position| Vertex {
                position,
                uv: [0.0; 2],
                color: [1.0, 0.0, 0.0, 1.0],
            })
            .collect(),
        indices: vec![0, 1, 2],
    })
}

fn quad(half: [f32; 2], color: [f32; 4]) -> Arc<Mesh> {
    Arc::new(Mesh {
        uv2: vec![],
        normals: vec![],
        vertices: [
            [-half[0], -half[1], 0.0],
            [half[0], -half[1], 0.0],
            [half[0], half[1], 0.0],
            [-half[0], half[1], 0.0],
        ]
        .into_iter()
        .map(|position| Vertex {
            position,
            uv: [0.0; 2],
            color,
        })
        .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}

fn draw(id: &str, mesh: Arc<Mesh>, position: Vec3) -> MeshDraw3D {
    MeshDraw3D {
        lightmap: None,
        id: id.into(),
        mesh,
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: Mat4::from_translation(position).to_cols_array_2d(),
        material: MeshMaterial {
            unlit: true,
            ..Default::default()
        },
        shader: None,
    }
}

fn frame(camera: Camera3D, size: [u32; 2], meshes: Vec<MeshDraw3D>) -> SceneRenderFrame {
    let mut frame = SceneRenderFrame::from(RenderFrame {
        camera: Camera2D {
            logical_size: size,
            ..Default::default()
        },
        draws: vec![],
    });
    frame.camera3d = Some(CameraInstance3D {
        camera,
        world: Mat4::IDENTITY.to_cols_array_2d(),
    });
    frame.meshes = meshes;
    frame
}

fn camera(mode: UpscalerMode, quality: UpscalerQuality) -> Camera3D {
    Camera3D {
        upscaler: UpscalerSettings { mode, quality },
        ..Default::default()
    }
}

fn static_scene(camera: Camera3D, size: [u32; 2]) -> SceneRenderFrame {
    frame(
        camera,
        size,
        vec![draw("triangle", triangle(), Vec3::new(0.0, 0.0, -2.5))],
    )
}

struct Rig {
    renderer: GpuRenderer,
    target: OffscreenTarget,
}

impl Rig {
    fn new(size: [u32; 2]) -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        let target = OffscreenTarget::new(&gpu, size[0], size[1])?;
        let renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
        Ok(Self { renderer, target })
    }

    fn render(&mut self, scene: &SceneRenderFrame) -> Result<Vec<u8>> {
        self.renderer.render_scene(scene, &self.target.view, None)?;
        self.target.read_rgba(&self.renderer.gpu)
    }

    fn status(&self) -> UpscalerStatus {
        self.renderer.upscaler_status().clone()
    }
}

fn red(pixels: &[u8], width: u32, x: u32, y: u32) -> u8 {
    pixels[((y * width + x) * 4) as usize]
}

fn intermediate_red(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 40 && p[0] < 215)
        .count()
}

fn max_change(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

fn feature<'a>(
    renderer: &'a GpuRenderer,
    name: &str,
) -> &'a engine_project::rendering3d::RenderFeatureView {
    renderer
        .pipeline_status()
        .features
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} is not listed"))
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn native_temporal_anti_aliases_a_static_scene_cuts_and_restarts_deterministically() -> Result<()> {
    let mut rig = Rig::new(SIZE)?;
    let baseline = rig.render(&static_scene(Camera3D::default(), SIZE))?;
    assert_eq!(rig.status().state, UpscalerState::Native);
    assert_eq!(intermediate_red(&baseline), 0, "the plain scene is aliased");

    let scene = static_scene(
        camera(UpscalerMode::NativeTemporal, UpscalerQuality::Dlaa),
        SIZE,
    );
    let first = rig.render(&scene)?;
    let status = rig.status();
    assert_eq!(status.state, UpscalerState::Active, "{status:?}");
    assert_eq!(status.backend, Some(UpscalerBackend::NativeTemporal));
    assert_eq!(status.render_size, Some(SIZE));
    assert_eq!(status.output_size, Some(SIZE));
    assert_eq!(status.history_resets, 1);
    assert!(feature(&rig.renderer, "effects.temporal_resolve").activity == "ran");
    assert!(feature(&rig.renderer, "engine.upscale").activity == "superseded");

    let mut previous = first.clone();
    let mut last_change = 0;
    let mut converged = first.clone();
    for _ in 1..28 {
        converged = rig.render(&scene)?;
        last_change = max_change(&converged, &previous);
        previous = converged.clone();
    }
    let status = rig.status();
    assert_eq!(status.evaluated_frames, 28);
    assert_eq!(status.history_resets, 1, "no extra resets while static");
    eprintln!(
        "intermediate edge pixels: aliased {} temporal {}; steady change {last_change}/255",
        intermediate_red(&baseline),
        intermediate_red(&converged)
    );
    assert!(intermediate_red(&converged) > 20, "no anti-aliased edge");
    assert!(last_change < 22, "steady state drifts by {last_change}/255");

    // A camera cut drops the history and restarts the jitter sequence: the
    // next frame is exactly what the very first frame was.
    rig.renderer.reset_temporal_history();
    let cut = rig.render(&scene)?;
    assert_eq!(cut, first, "cut frame differs from a fresh first frame");
    assert_eq!(rig.status().history_resets, 2);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn half_render_scale_upscales_to_the_output_size() -> Result<()> {
    let mut rig = Rig::new(SIZE)?;
    let reference_scene = static_scene(
        camera(UpscalerMode::NativeTemporal, UpscalerQuality::Dlaa),
        SIZE,
    );
    let mut reference = Vec::new();
    for _ in 0..40 {
        reference = rig.render(&reference_scene)?;
    }
    let half_scene = static_scene(
        camera(UpscalerMode::NativeTemporal, UpscalerQuality::Performance),
        SIZE,
    );
    // The render-scale change is a history break: the first half-scale frame
    // equals a fresh renderer's first frame, not a blend with the 1:1 history.
    let first_half = rig.render(&half_scene)?;
    let fresh_half = Rig::new(SIZE)?.render(&half_scene)?;
    assert_eq!(
        first_half, fresh_half,
        "render-scale change kept old history"
    );
    let mut half = first_half;
    for _ in 1..80 {
        half = rig.render(&half_scene)?;
    }
    let status = rig.status();
    assert_eq!(status.state, UpscalerState::Active);
    assert_eq!(status.render_size, Some([33, 22]));
    assert_eq!(status.output_size, Some(SIZE));
    assert_eq!(half.len(), reference.len());
    let error = |a: &[u8], b: &[u8]| -> f64 {
        a.as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0.iter())
            .map(|(p, q)| (f64::from(p[0]) - f64::from(q[0])).powi(2))
            .sum::<f64>()
            / (255.0 * 255.0)
    };
    let half_error = error(&half, &reference);
    eprintln!("half-scale vs 1:1 reference: squared error {half_error:.3}");
    assert!(
        intermediate_red(&half) > 15,
        "no anti-aliased edge at half scale"
    );
    // The covered area (linear light: the target is sRGB) matches the analytic
    // projected triangle for both the 1:1 and the half-scale resolve.
    let covered = |p: &[u8]| {
        p.as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let c = f64::from(p[0]) / 255.0;
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            })
            .sum::<f64>()
    };
    let Projection3D::Perspective { fov_y_radians, .. } = Camera3D::default().projection else {
        panic!("the default camera is perspective");
    };
    let tan = f64::from(fov_y_radians / 2.0).tan();
    // Base 2.8, height 1.8 at distance 2.5; pixels per NDC^2 area is W*H/4.
    let analytic = 0.5 * 2.8 * 1.8 / (2.5 * 2.5 * tan * tan) * f64::from(SIZE[1]).powi(2) / 4.0;
    let (a, b) = (covered(&half), covered(&reference));
    eprintln!("covered area: analytic {analytic:.1}, 1:1 {b:.1}, half {a:.1}");
    assert!(
        (b - analytic).abs() / analytic < 0.03,
        "1:1 area {b} vs {analytic}"
    );
    assert!(
        (a - analytic).abs() / analytic < 0.03,
        "half-scale area {a} vs {analytic}"
    );
    // The change of render scale is a history break reported once.
    assert!(status.history_resets >= 2);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn mode_switching_resizing_and_unavailable_dlss_behave_as_documented() -> Result<()> {
    let mut rig = Rig::new(SIZE)?;
    let plain = static_scene(Camera3D::default(), SIZE);
    let temporal = static_scene(
        camera(UpscalerMode::NativeTemporal, UpscalerQuality::Dlaa),
        SIZE,
    );
    let baseline = rig.render(&plain)?;
    let first = rig.render(&temporal)?;
    for _ in 0..6 {
        rig.render(&temporal)?;
    }
    // Off: byte-identical to the baseline, history released.
    let off = rig.render(&plain)?;
    assert_eq!(off, baseline, "turning the resolve off changed pixels");
    assert_eq!(rig.status().state, UpscalerState::Native);
    assert!(feature(&rig.renderer, "effects.temporal_resolve").activity != "ran");
    // On again: a fresh first frame, not a blend with the old history.
    let again = rig.render(&temporal)?;
    assert_eq!(again, first, "re-enable did not start fresh");

    // DLSS requested on a build/device without it: documented native fallback,
    // never the temporal resolve; pixels equal the baseline.
    #[cfg(not(feature = "dlss"))]
    {
        let dlss = static_scene(camera(UpscalerMode::DlssSr, UpscalerQuality::Quality), SIZE);
        let image = rig.render(&dlss)?;
        let status = rig.status();
        assert_eq!(status.state, UpscalerState::Fallback, "{status:?}");
        assert!(status.fallback_reason.is_some());
        assert_eq!(status.backend, None);
        assert_eq!(
            image, baseline,
            "DLSS fallback must be a plain native frame"
        );
        // auto_temporal: the same DLSS reason, but the native resolve runs.
        let auto = static_scene(
            camera(UpscalerMode::AutoTemporal, UpscalerQuality::Dlaa),
            SIZE,
        );
        rig.render(&auto)?;
        let status = rig.status();
        assert_eq!(status.state, UpscalerState::Active, "{status:?}");
        assert_eq!(status.backend, Some(UpscalerBackend::NativeTemporal));
        assert!(
            status.fallback_reason.is_some(),
            "the DLSS reason must be reported"
        );
    }

    // Resize: the first frame at the new size equals a fresh renderer's.
    let small = [48, 32];
    rig.renderer.resize(small[0], small[1])?;
    rig.target = OffscreenTarget::new(&rig.renderer.gpu, small[0], small[1])?;
    let small_scene = static_scene(
        camera(UpscalerMode::NativeTemporal, UpscalerQuality::Dlaa),
        small,
    );
    rig.render(&small_scene)?;
    let resized = rig.render(&small_scene)?;
    let mut fresh = Rig::new(small)?;
    fresh.render(&small_scene)?;
    let fresh_second = fresh.render(&small_scene)?;
    assert_eq!(rig.status().output_size, Some(small));
    // The resized renderer had a history of the old size: it was discarded at
    // the first frame, so frame 2 matches a fresh renderer's frame 2 closely.
    assert!(
        max_change(&resized, &fresh_second) <= 1,
        "resize leaked old history: {}",
        max_change(&resized, &fresh_second)
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn spatial_aa_yields_to_the_temporal_resolve_and_the_feature_off_is_unchanged() -> Result<()> {
    let mut rig = Rig::new(SIZE)?;
    let mut spatial = Camera3D {
        spatial_aa: Some(SpatialAa3D::default()),
        ..Default::default()
    };
    let spatial_only = rig.render(&static_scene(spatial.clone(), SIZE))?;
    assert!(feature(&rig.renderer, SPATIAL_AA_FEATURE).activity == "ran");
    assert!(!rig.status().spatial_aa_skipped);

    spatial.upscaler = UpscalerSettings {
        mode: UpscalerMode::NativeTemporal,
        quality: UpscalerQuality::Dlaa,
    };
    rig.render(&static_scene(spatial.clone(), SIZE))?;
    let status = rig.status();
    assert_eq!(status.state, UpscalerState::Active);
    assert!(status.spatial_aa_skipped, "{status:?}");
    assert!(!feature(&rig.renderer, SPATIAL_AA_FEATURE).enabled);
    assert!(feature(&rig.renderer, "effects.temporal_resolve").enabled);

    // An authored render_features entry cannot enable the resolve by itself.
    let mut forced = Camera3D::default();
    forced
        .render_features
        .insert("effects.temporal_resolve".into(), true);
    let plain = rig.render(&static_scene(Camera3D::default(), SIZE))?;
    let ignored = rig.render(&static_scene(forced, SIZE))?;
    assert_eq!(ignored, plain, "render_features enabled the resolve");
    assert!(!feature(&rig.renderer, "effects.temporal_resolve").enabled);

    // And back to spatial AA only: restored exactly.
    spatial.upscaler = UpscalerSettings::default();
    let restored = rig.render(&static_scene(spatial, SIZE))?;
    assert_eq!(restored, spatial_only);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_moving_mesh_leaves_no_ghost_trail_through_the_real_motion_prepass() -> Result<()> {
    let size = [96, 48];
    let mut rig = Rig::new(size)?;
    let backdrop = quad([8.0, 8.0], [0.0, 0.0, 1.0, 1.0]);
    let mover = quad([0.25, 0.25], [1.0, 0.0, 0.0, 1.0]);
    let cam = camera(UpscalerMode::NativeTemporal, UpscalerQuality::Dlaa);
    let mut last = Vec::new();
    for step in 0..18 {
        let x = -1.2 + 0.1 * step as f32;
        let scene = frame(
            cam.clone(),
            size,
            vec![
                draw("backdrop", backdrop.clone(), Vec3::new(0.0, 0.0, -4.0)),
                draw("mover", mover.clone(), Vec3::new(x, 0.0, -2.0)),
            ],
        );
        last = rig.render(&scene)?;
    }
    assert_eq!(rig.status().state, UpscalerState::Active);
    let row = size[1] / 2;
    let reds: Vec<u32> = (0..size[0])
        .filter(|&x| red(&last, size[0], x, row) > 128)
        .collect();
    assert!(!reds.is_empty(), "the moving mesh is not visible");
    let (left, right) = (*reds.first().unwrap(), *reds.last().unwrap());
    // Everything left of the trailing edge was covered by the mesh within the
    // last frames. The bound is stated here: no red above 12/255 from three
    // pixels behind the edge on (the edge pixels legitimately mix colours).
    let worst_trail = (0..left.saturating_sub(3))
        .map(|x| red(&last, size[0], x, row))
        .max()
        .unwrap_or(0);
    eprintln!("mesh spans {left}..={right}; worst trail red {worst_trail}/255");
    assert!(
        left > 8,
        "the mesh barely moved: {left}; the test would be vacuous"
    );
    assert!(worst_trail <= 12, "ghost trail of {worst_trail}/255");
    let inside = red(&last, size[0], (left + right) / 2, row);
    assert!(inside > 230, "the mesh body lost colour: {inside}");
    Ok(())
}

/// Alternating green and blue stripes along x on the ground plane y = 0,
/// receding from z = -1.5 to z = -80 (a grazing plane far away).
fn striped_ground() -> Arc<Mesh> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for stripe in 0..40 {
        let left = -10.0 + stripe as f32 * 0.5;
        let color = if stripe % 2 == 0 {
            [0.0, 0.8, 0.0, 1.0]
        } else {
            [0.0, 0.1, 0.8, 1.0]
        };
        let first = vertices.len() as u32;
        for position in [
            [left, 0.0, -1.5],
            [left + 0.5, 0.0, -1.5],
            [left + 0.5, 0.0, -80.0],
            [left, 0.0, -80.0],
        ] {
            vertices.push(Vertex {
                position,
                uv: [0.0; 2],
                color,
            });
        }
        indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    Arc::new(Mesh {
        uv2: vec![],
        normals: vec![],
        vertices,
        indices,
    })
}

/// A scene seen from `pose` (translation then yaw): the striped ground and a
/// red wall standing on it at depth 4.
fn camera_scene(cam: Camera3D, size: [u32; 2], position: Vec3, yaw: f32) -> SceneRenderFrame {
    let mut scene = frame(
        cam,
        size,
        vec![
            draw("ground", striped_ground(), Vec3::ZERO),
            draw(
                "wall",
                quad([0.5, 0.6], [1.0, 0.0, 0.0, 1.0]),
                Vec3::new(0.0, 0.6, -4.0),
            ),
        ],
    );
    scene.camera3d.as_mut().unwrap().world =
        (Mat4::from_translation(position) * Mat4::from_rotation_y(yaw)).to_cols_array_2d();
    scene
}

/// Pixels whose green or blue differs between two green/blue stripe edges: the
/// anti-aliased transitions on the ground below the horizon rows.
fn intermediate_ground(pixels: &[u8], width: u32, rows: std::ops::Range<u32>) -> usize {
    let mut count = 0;
    for y in rows {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let (g, b) = (pixels[i + 1], pixels[i + 2]);
            // Pure stripes are (g high, b low) or (g low, b high) in sRGB.
            let mixed = g > 70 && b > 70 && !(g > 200 && b < 70) && !(g < 100 && b > 190);
            count += usize::from(mixed);
        }
    }
    count
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn camera_motion_keeps_history_on_a_grazing_ground_plane_and_leaves_no_ghost() -> Result<()> {
    let size = [96, 64];
    let cam = camera(UpscalerMode::NativeTemporal, UpscalerQuality::Dlaa);
    let eye = Vec3::new(0.0, 1.0, 0.0);
    // Horizon is at the image centre (row 32); rows below it see the ground,
    // the farthest visible rows being the most grazing.
    let ground_rows = 36..64;

    // Slow pan and dolly: sub-pixel motion per frame, so stable surfaces must
    // keep accumulating. Compared with the same final pose held static.
    let steps = 30;
    let pose = |k: u32| {
        (
            eye + Vec3::new(0.0, 0.0, -0.03 * k as f32),
            0.004 * k as f32,
        )
    };
    let mut moving = Rig::new(size)?;
    let mut last = Vec::new();
    for k in 0..steps {
        let (position, yaw) = pose(k);
        last = moving.render(&camera_scene(cam.clone(), size, position, yaw))?;
    }
    assert_eq!(moving.status().state, UpscalerState::Active);
    assert_eq!(
        moving.status().history_resets,
        1,
        "camera motion alone must not reset history"
    );
    let mut still = Rig::new(size)?;
    let (position, yaw) = pose(steps - 1);
    let mut held = Vec::new();
    for _ in 0..steps {
        held = still.render(&camera_scene(cam.clone(), size, position, yaw))?;
    }
    let (m, h) = (
        intermediate_ground(&last, size[0], ground_rows.clone()),
        intermediate_ground(&held, size[0], ground_rows.clone()),
    );
    // Without accumulation each frame is close to aliased (few mixed pixels).
    let aliased = {
        let mut plain = Rig::new(size)?;
        let image = plain.render(&camera_scene(Camera3D::default(), size, position, yaw))?;
        intermediate_ground(&image, size[0], ground_rows.clone())
    };
    eprintln!("ground edge pixels: aliased {aliased}, moving {m}, held {h}");
    assert!(
        h > aliased + 40,
        "the held pose did not anti-alias: {h} vs {aliased}"
    );
    assert!(
        m * 10 >= h * 7,
        "history was rejected on the grazing plane while moving: {m} vs {h}"
    );

    // Fast pan (about 2 pixels per frame) past the wall: nothing red may be
    // left outside the wall's silhouette (dilated by 3 px) in the final frame.
    let mut fast = Rig::new(size)?;
    let mut image = Vec::new();
    let mut final_pose = (eye, 0.0);
    for k in 0..18u32 {
        final_pose = (eye, 0.03 * k as f32);
        image = fast.render(&camera_scene(cam.clone(), size, final_pose.0, final_pose.1))?;
    }
    let mut plain = Rig::new(size)?;
    let reference = plain.render(&camera_scene(
        Camera3D::default(),
        size,
        final_pose.0,
        final_pose.1,
    ))?;
    let mask = |x: i32, y: i32| -> bool {
        (-3..=3).any(|dy| {
            (-3..=3).any(|dx| {
                let (px, py) = (x + dx, y + dy);
                px >= 0
                    && py >= 0
                    && (px as u32) < size[0]
                    && (py as u32) < size[1]
                    && red(&reference, size[0], px as u32, py as u32) > 128
            })
        })
    };
    let mut ghost = 0;
    let mut wall = 0;
    for y in 0..size[1] {
        for x in 0..size[0] {
            let r = red(&image, size[0], x, y);
            if r > 128 {
                wall += 1;
            }
            if r > 60 && !mask(x as i32, y as i32) {
                ghost += 1;
            }
        }
    }
    eprintln!("fast pan: wall pixels {wall}, ghost pixels {ghost}");
    assert!(
        wall > 20,
        "the wall left the view; the test would be vacuous"
    );
    assert!(
        ghost <= 2,
        "{ghost} ghost pixels outside the wall silhouette"
    );
    Ok(())
}

fn authored_project(dir: &std::path::Path, mode: &str) {
    let vertex = |x: f32, y: f32| json!({"position":[x,y,0],"uv":[0,0],"color":[1,0,0,1]});
    std::fs::write(
        dir.join("tri.json"),
        serde_json::to_vec(&json!({
            "vertices":[vertex(-1.4,-0.9),vertex(1.4,-0.9),vertex(0.3,0.9)],
            "normals":[[0,0,1],[0,0,1],[0,0,1]],
            "indices":[0,1,2]}))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"temporal","entities":[
            {"id":"camera","name":"Camera","components":{
                "Transform":{"position":[0,0,2.5]},
                "Camera2D":{"logicalSize":[65,43]},
                "Camera3D":{"upscaler":{"mode":mode,"quality":"dlaa"}}}},
            {"id":"tri","name":"Triangle","components":{
                "Transform":{"position":[0,0,0]},
                "MeshRenderer":{"mesh":"tri"},
                "MeshMaterial":{"unlit":true}}}]}))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-temporal",
            "startup_scene":"scene.json","assets":[
                {"id":"tri","path":"tri.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_native_temporal_project_renders_reports_and_follows_a_runtime_override() -> Result<()> {
    let dir = tempfile::Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    authored_project(dir.path(), "native_temporal");
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime.start()?;
    let mut rig = Rig::new(SIZE)?;
    let mut previous = Vec::new();
    for _ in 0..16 {
        let scene = runtime.extract_scene_frame()?;
        assert_eq!(
            scene.camera3d.as_ref().unwrap().camera.upscaler.mode,
            UpscalerMode::NativeTemporal
        );
        previous = rig.render(&scene)?;
        runtime
            .app_mut()
            .world_mut()
            .resource_mut::<UpscalerControl>()
            .status = rig.status();
    }
    let reported = runtime.upscaler_status().expect("a 3D camera reports");
    assert_eq!(reported.state, UpscalerState::Active, "{reported:?}");
    assert_eq!(reported.backend, Some(UpscalerBackend::NativeTemporal));
    assert_eq!(reported.evaluated_frames, 16);
    let value = serde_json::to_value(&reported)?;
    assert_eq!(value["backend"], "native_temporal");
    assert!(
        intermediate_red(&previous) > 20,
        "the authored scene is not anti-aliased"
    );

    // A runtime override replaces the authored request as a whole: DLSS is
    // requested, falls back natively (this build has no SDK), the temporal
    // history is released, and the output is the plain native frame.
    #[cfg(not(feature = "dlss"))]
    {
        runtime
            .app_mut()
            .world_mut()
            .resource_mut::<UpscalerControl>()
            .override_settings = Some(UpscalerSettings {
            mode: UpscalerMode::DlssSr,
            quality: UpscalerQuality::Quality,
        });
        let scene = runtime.extract_scene_frame()?;
        let image = rig.render(&scene)?;
        assert_eq!(rig.status().state, UpscalerState::Fallback);
        assert_eq!(
            intermediate_red(&image),
            0,
            "the DLSS fallback is plain native"
        );
    }
    Ok(())
}
