//! Authored camera effects through the desktop GPU renderer.
//! Run with `--ignored --test-threads=1` on a native adapter.

use anyhow::Result;
use engine_desktop::{
    GpuContext, GpuRenderer, OffscreenTarget, RenderLimits, WgpuOptions, preview_pipeline_status,
};
use engine_project::{
    Camera2D,
    content::{Mesh, Texture, Vertex, sprite_quad},
    rendering3d::*,
    runtime::{Draw, DrawSpace, DrawState, RenderFrame},
};
use glam::{Mat4, Vec3};
use std::sync::Arc;

fn stripes() -> Arc<Mesh> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for stripe in 0..16 {
        let left = -1.0 + stripe as f32 * 0.125;
        let right = left + 0.125;
        let color = if stripe % 2 == 0 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0, 0.0, 0.0, 1.0]
        };
        let first = vertices.len() as u32;
        for position in [
            [left, -1.0, 0.0],
            [right, -1.0, 0.0],
            [right, 1.0, 0.0],
            [left, 1.0, 0.0],
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

fn diagonal() -> Arc<Mesh> {
    Arc::new(Mesh {
        uv2: vec![],
        normals: vec![],
        vertices: [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0]]
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

fn frame(camera: Camera3D, camera_x: f32, size: [u32; 2], mesh: Arc<Mesh>) -> SceneRenderFrame {
    let mut frame = SceneRenderFrame::from(RenderFrame {
        camera: Camera2D {
            logical_size: size,
            ..Default::default()
        },
        draws: vec![],
    });
    frame.camera3d = Some(CameraInstance3D {
        camera,
        world: Mat4::from_translation(Vec3::new(camera_x, 0.0, 0.0)).to_cols_array_2d(),
    });
    frame.meshes.push(MeshDraw3D {
        lightmap: None,
        id: "stripes".into(),
        mesh,
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, -2.0)).to_cols_array_2d(),
        material: MeshMaterial {
            unlit: true,
            ..Default::default()
        },
        shader: None,
    });
    frame
}

fn render(
    renderer: &mut GpuRenderer,
    target: &OffscreenTarget,
    scene: &SceneRenderFrame,
) -> Result<Vec<u8>> {
    renderer.render_scene(scene, &target.view, None)?;
    target.read_rgba(&renderer.gpu)
}

fn maximum_red_change(a: &[u8], b: &[u8], size: [u32; 2]) -> u8 {
    let y = size[1] / 2;
    ((size[0] / 4)..(size[0] * 3 / 4))
        .map(|x| {
            let index = ((y * size[0] + x) * 4) as usize;
            a[index].abs_diff(b[index])
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn headless_preview_lists_the_camera_effect_descriptors() {
    let camera = Camera3D {
        depth_of_field: Some(DepthOfField3D::default()),
        ..Default::default()
    };
    let status = preview_pipeline_status(None, &camera.effective_render_features()).unwrap();
    let dof = status
        .features
        .iter()
        .find(|feature| feature.name == DEPTH_OF_FIELD_FEATURE)
        .unwrap();
    let motion = status
        .features
        .iter()
        .find(|feature| feature.name == MOTION_BLUR_FEATURE)
        .unwrap();
    let spatial = status
        .features
        .iter()
        .find(|feature| feature.name == SPATIAL_AA_FEATURE)
        .unwrap();
    assert!(dof.enabled && dof.source == "view_override");
    assert!(!motion.enabled);
    assert!(!spatial.enabled);
    assert!(status.unknown_overrides.is_empty());

    // Disabled by default, enabled by the authored setting, and ordered after
    // upscale/bloom/DOF/motion blur and before the tonemap and overlays.
    let enabled = Camera3D {
        spatial_aa: Some(SpatialAa3D::default()),
        ..Default::default()
    };
    let status = preview_pipeline_status(None, &enabled.effective_render_features()).unwrap();
    let position = |name: &str| {
        status
            .features
            .iter()
            .position(|feature| feature.name == name)
            .unwrap_or_else(|| panic!("{name} is not listed"))
    };
    let spatial = &status.features[position(SPATIAL_AA_FEATURE)];
    assert!(spatial.enabled && spatial.source == "view_override");
    assert!(position("engine.upscale") < position("engine.post"));
    assert!(position("engine.post") < position(SPATIAL_AA_FEATURE));
    assert!(position(MOTION_BLUR_FEATURE) < position(SPATIAL_AA_FEATURE));
    assert!(position(SPATIAL_AA_FEATURE) < position("engine.output"));
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_spatial_aa_changes_diagonal_and_clears_on_disable_resize() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let mut target = OffscreenTarget::new(&gpu, 65, 43)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let mesh = diagonal();
    let base = Camera3D::default();
    let baseline = render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.0, [65, 43], mesh.clone()),
    )?;
    let enabled = Camera3D {
        spatial_aa: Some(SpatialAa3D::default()),
        ..base.clone()
    };
    let aa = render(
        &mut renderer,
        &target,
        &frame(enabled.clone(), 0.0, [65, 43], mesh.clone()),
    )?;
    assert!(
        renderer
            .pipeline_status()
            .features
            .iter()
            .any(|feature| feature.name == SPATIAL_AA_FEATURE
                && feature.enabled
                && feature.activity == "ran")
    );
    assert!(
        baseline
            .as_chunks::<4>()
            .0
            .iter()
            .zip(aa.as_chunks::<4>().0)
            .map(|(a, b)| a[0].abs_diff(b[0]))
            .max()
            .unwrap_or(0)
            > 2,
        "authored AA did not affect diagonal pixels"
    );

    let removed = render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.0, [65, 43], mesh.clone()),
    )?;
    assert_eq!(removed, baseline, "removed settings leaked into next frame");
    let mut disabled = enabled.clone();
    disabled
        .render_features
        .insert(SPATIAL_AA_FEATURE.into(), false);
    assert_eq!(
        render(
            &mut renderer,
            &target,
            &frame(disabled, 0.0, [65, 43], mesh.clone())
        )?,
        baseline
    );
    assert_eq!(
        render(
            &mut renderer,
            &target,
            &frame(enabled.clone(), 0.0, [65, 43], mesh.clone())
        )?,
        aa,
        "re-enabled settings did not restore result"
    );

    renderer.resize(67, 45)?;
    target = OffscreenTarget::new(&renderer.gpu, 67, 45)?;
    let resized_plain = render(
        &mut renderer,
        &target,
        &frame(base, 0.0, [67, 45], mesh.clone()),
    )?;
    let resized_aa = render(&mut renderer, &target, &frame(enabled, 0.0, [67, 45], mesh))?;
    assert!(
        resized_plain
            .as_chunks::<4>()
            .0
            .iter()
            .zip(resized_aa.as_chunks::<4>().0)
            .any(|(a, b)| a[0].abs_diff(b[0]) > 2)
    );
    Ok(())
}

fn rgba(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * width + x) * 4) as usize;
    pixels[index..index + 4].try_into().unwrap()
}

/// A pixel-aligned opaque green HUD rectangle in the sprite overlay.
fn with_overlay(mut scene: SceneRenderFrame) -> Result<SceneRenderFrame> {
    let white = Arc::new(Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    });
    scene.sprites.camera.pixels_per_unit = 1.0;
    scene.sprites.draws.push(Draw {
        id: "hud".into(),
        mesh: Arc::new(sprite_quad(
            [-9.0, -9.0, 0.5],
            [18.0, 18.0],
            [0.0, 0.0],
            [0, 0, 1, 1],
            &white,
            [0.0, 1.0, 0.0, 1.0],
        )?),
        texture: Some(white),
        layer: 0,
        space: DrawSpace::World,
        clip: None,
        state: DrawState::default(),
        motion: None,
        offset: [0.0; 3],
    });
    Ok(scene)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_spatial_aa_edge_pixels_flat_regions_and_hud_overlay() -> Result<()> {
    let size = [65u32, 43];
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, size[0], size[1])?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let mesh = diagonal();
    let off = Camera3D::default();
    let on = Camera3D {
        spatial_aa: Some(SpatialAa3D::default()),
        ..off.clone()
    };
    let plain = render(
        &mut renderer,
        &target,
        &frame(off.clone(), 0.0, size, mesh.clone()),
    )?;
    let aa = render(
        &mut renderer,
        &target,
        &frame(on.clone(), 0.0, size, mesh.clone()),
    )?;

    // AA off: the rasterised edge holds the two source colours only.
    let mut colours: Vec<[u8; 4]> = plain.as_chunks::<4>().0.to_vec();
    colours.sort_unstable();
    colours.dedup();
    assert_eq!(colours.len(), 2, "plain edge colours: {colours:?}");
    // AA on: intermediate edge pixels exist...
    let intermediate = aa
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] > 8 && pixel[0] < 247)
        .count();
    assert!(intermediate >= 10, "{intermediate} intermediate pixels");
    // ...and every pixel more than two pixels from any colour change is untouched.
    let mut checked = 0;
    for y in 2..size[1] - 2 {
        for x in 2..size[0] - 2 {
            let centre = rgba(&plain, size[0], x, y);
            let flat = (-2i32..=2).all(|dy| {
                (-2i32..=2).all(|dx| {
                    rgba(
                        &plain,
                        size[0],
                        (x as i32 + dx) as u32,
                        (y as i32 + dy) as u32,
                    ) == centre
                })
            });
            if flat {
                checked += 1;
                let value = rgba(&aa, size[0], x, y);
                assert!(
                    value.iter().zip(centre).all(|(a, b)| a.abs_diff(b) <= 1),
                    "flat pixel ({x}, {y}) changed: {value:?} vs {centre:?}"
                );
            }
        }
    }
    assert!(checked > 500, "only {checked} flat pixels were compared");

    // The HUD sprite overlay is composed after the scene: it stays exactly as
    // drawn and the scene outside it matches the AA frame without a HUD.
    let hud_plain = render(
        &mut renderer,
        &target,
        &with_overlay(frame(off, 0.0, size, mesh.clone()))?,
    )?;
    let hud_aa = render(
        &mut renderer,
        &target,
        &with_overlay(frame(on, 0.0, size, mesh))?,
    )?;
    let green = |p: [u8; 4]| p == [0, 255, 0, 255];
    let covered: Vec<(u32, u32)> = (0..size[1])
        .flat_map(|y| (0..size[0]).map(move |x| (x, y)))
        .filter(|&(x, y)| green(rgba(&hud_plain, size[0], x, y)))
        .collect();
    assert!(
        covered.len() >= 100,
        "HUD sprite covers {} pixels",
        covered.len()
    );
    let (min_x, max_x) = (
        covered.iter().map(|c| c.0).min().unwrap(),
        covered.iter().map(|c| c.0).max().unwrap(),
    );
    let (min_y, max_y) = (
        covered.iter().map(|c| c.1).min().unwrap(),
        covered.iter().map(|c| c.1).max().unwrap(),
    );
    assert_eq!(
        covered.len() as u32,
        (max_x - min_x + 1) * (max_y - min_y + 1)
    );
    let mut edge_pixels_under_hud = 0;
    for y in 0..size[1] {
        for x in 0..size[0] {
            let inside = (min_x..=max_x).contains(&x) && (min_y..=max_y).contains(&y);
            if inside {
                assert_eq!(
                    rgba(&hud_aa, size[0], x, y),
                    [0, 255, 0, 255],
                    "HUD pixel ({x}, {y}) changed"
                );
                if rgba(&plain, size[0], x, y) != rgba(&aa, size[0], x, y) {
                    edge_pixels_under_hud += 1;
                }
            } else {
                assert_eq!(
                    rgba(&hud_aa, size[0], x, y),
                    rgba(&aa, size[0], x, y),
                    "scene pixel ({x}, {y}) differs from the HUD-free AA frame"
                );
                assert_eq!(rgba(&hud_plain, size[0], x, y), rgba(&plain, size[0], x, y));
            }
        }
    }
    assert!(
        edge_pixels_under_hud >= 3,
        "the HUD must overlap filtered edge pixels to test the overlay ({edge_pixels_under_hud})"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_camera_effects_change_pixels_and_clear_on_disable_and_resize() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("adapter: {}", gpu.adapter.get_info().name);
    let mut target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let mesh = stripes();
    let base = Camera3D::default();
    let plain = render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.0, [64, 64], mesh.clone()),
    )?;

    let mut dof_camera = base.clone();
    dof_camera.depth_of_field = Some(DepthOfField3D {
        focus_distance: 5.0,
        focus_range: 1.0,
        max_radius_pixels: 8.0,
        edge_tolerance: 0.5,
    });
    let dof = render(
        &mut renderer,
        &target,
        &frame(dof_camera.clone(), 0.0, [64, 64], mesh.clone()),
    )?;
    assert!(renderer.pipeline_status().features.iter().any(|feature| {
        feature.name == DEPTH_OF_FIELD_FEATURE && feature.enabled && feature.activity == "ran"
    }));
    assert!(
        maximum_red_change(&dof, &plain, [64, 64]) > 12,
        "DOF did not change stripes"
    );
    let disabled = render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.0, [64, 64], mesh.clone()),
    )?;
    assert!(
        maximum_red_change(&disabled, &plain, [64, 64]) <= 2,
        "removed settings leaked into next frame"
    );
    dof_camera
        .render_features
        .insert(DEPTH_OF_FIELD_FEATURE.into(), false);
    let filtered = render(
        &mut renderer,
        &target,
        &frame(dof_camera.clone(), 0.0, [64, 64], mesh.clone()),
    )?;
    assert!(
        renderer
            .pipeline_status()
            .features
            .iter()
            .any(|feature| { feature.name == DEPTH_OF_FIELD_FEATURE && !feature.enabled })
    );
    assert!(
        maximum_red_change(&filtered, &plain, [64, 64]) <= 2,
        "explicit feature disable was ignored"
    );
    dof_camera.render_features.clear();
    let reenabled = render(
        &mut renderer,
        &target,
        &frame(dof_camera, 0.0, [64, 64], mesh.clone()),
    )?;
    assert!(
        maximum_red_change(&reenabled, &plain, [64, 64]) > 12,
        "DOF did not re-enable"
    );

    let mut motion_camera = base.clone();
    motion_camera.motion_blur = Some(MotionBlur3D {
        shutter_scale: 1.0,
        max_radius_pixels: 16.0,
        edge_tolerance: 0.5,
    });
    renderer.reset_temporal_history();
    render(
        &mut renderer,
        &target,
        &frame(motion_camera.clone(), 0.0, [64, 64], mesh.clone()),
    )?;
    let moving = render(
        &mut renderer,
        &target,
        &frame(motion_camera, 0.15, [64, 64], mesh.clone()),
    )?;
    renderer.reset_temporal_history();
    render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.0, [64, 64], mesh.clone()),
    )?;
    let still = render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.15, [64, 64], mesh.clone()),
    )?;
    assert!(
        maximum_red_change(&moving, &still, [64, 64]) > 8,
        "camera motion did not change pixels"
    );

    renderer.resize(96, 64)?;
    target = OffscreenTarget::new(&renderer.gpu, 96, 64)?;
    let resized_plain = render(
        &mut renderer,
        &target,
        &frame(base.clone(), 0.0, [96, 64], mesh.clone()),
    )?;
    let resized_dof = render(
        &mut renderer,
        &target,
        &frame(
            Camera3D {
                depth_of_field: Some(DepthOfField3D::default()),
                ..base
            },
            0.0,
            [96, 64],
            mesh,
        ),
    )?;
    assert!(maximum_red_change(&resized_dof, &resized_plain, [96, 64]) > 8);
    Ok(())
}
