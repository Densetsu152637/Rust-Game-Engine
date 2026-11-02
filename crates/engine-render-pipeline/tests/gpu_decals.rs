//! GPU readback tests of `engine.decals` (projected decals). Run with
//! `cargo test -p engine-render-pipeline --test gpu_decals -- --ignored --test-threads=1`.
//!
//! The scene is an unlit grey floor (y = 0, facing +Y) seen by an orthographic
//! top view, so every measured pixel is compared with the same pixel of a
//! decal-free render of the same scene. Screen right is +X and screen down is
//! +Z; 8 pixels per metre at the default 64 x 64 / 8 m view.

use anyhow::Result;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Quat, Vec3};
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

const GREY: f32 = 0.5;

fn gpu() -> Result<GpuContext> {
    GpuContext::headless_with_options(&WgpuOptions::default())
}

/// A horizontal square (`half` metres) facing +Y.
fn quad(gpu: &GpuContext, half: f32) -> Arc<GpuMesh> {
    let vertex = |x, z| EngineVertex {
        position: [x, 0.0, z],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: [0.0, 1.0, 0.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "decal test quad",
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

fn unlit<'a>(id: &'a str, mesh: &Arc<GpuMesh>, world: Mat4, tint: [f32; 3]) -> SceneMesh<'a> {
    SceneMesh {
        id,
        mesh: mesh.clone(),
        world_from_local: world,
        shader: None,
        params: MaterialParams {
            tint: [tint[0], tint[1], tint[2], 1.0],
            unlit: true,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
        lightmap: None,
    }
}

/// An orthographic camera looking straight down from 10 m, 8 m across.
fn top_view(size: u32) -> ViewData {
    let extent = 8.0;
    let projection = glam::camera::rh::proj::directx::orthographic(
        -extent * 0.5,
        extent * 0.5,
        -extent * 0.5,
        extent * 0.5,
        0.1,
        20.0,
    );
    let world_from_view =
        Mat4::from_translation(Vec3::Y * 10.0) * Mat4::from_rotation_x(-FRAC_PI_2);
    ViewData::new(
        ViewId::MAIN,
        world_from_view,
        projection,
        [size; 2],
        [size; 2],
    )
    .unwrap()
}

fn target(gpu: &GpuContext, size: u32) -> (wgpu::Texture, OutputTarget) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("decal test target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = OutputTarget {
        view: texture.create_view(&Default::default()),
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        size: [size; 2],
        viewport: Viewport::full([size; 2]),
    };
    (texture, output)
}

struct Rig {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    floor: Arc<GpuMesh>,
}

impl Rig {
    fn new() -> Result<Self> {
        let gpu = gpu()?;
        let pipeline = ScenePipeline::new(gpu.clone())?;
        let floor = quad(&gpu, 20.0);
        Ok(Self {
            gpu,
            pipeline,
            floor,
        })
    }

    /// Render the floor plus `extra` meshes with `decals`.
    fn render(
        &mut self,
        size: u32,
        decals: Vec<Decal>,
        extra: &[SceneMesh<'_>],
    ) -> Result<(HdrImage, ViewReport)> {
        let mut view = top_view(size);
        view.decals = decals;
        let mut meshes = vec![unlit("floor", &self.floor, Mat4::IDENTITY, [GREY; 3])];
        meshes.extend(extra.iter().cloned());
        let (_texture, output) = target(&self.gpu, size);
        let report = self.pipeline.render_view(
            &SceneFrame {
                view: &view,
                meshes: &meshes,
                lights: &LightList::default(),
                world: None,
            },
            &output,
            &mut NoHost,
        )?;
        Ok((self.pipeline.read_scene_hdr(view.id)?, report))
    }
}

/// A decal facing +Y (its local +Z axis), centred on the floor origin.
fn floor_decal(half: f32) -> Decal {
    Decal {
        rotation: Quat::from_rotation_arc(Vec3::Z, Vec3::Y),
        half_extents: Vec3::splat(half),
        edge_fade: 0.0,
        angle_fade: 0.0,
        ..Default::default()
    }
}

fn red() -> Vec3 {
    Vec3::new(1.0, 0.0, 0.0)
}

fn close(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tolerance)
}

fn changed_pixels(a: &HdrImage, b: &HdrImage) -> usize {
    a.pixels
        .iter()
        .zip(&b.pixels)
        .filter(|(x, y)| x != y)
        .count()
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn decal_changes_pixels_inside_its_volume_and_leaves_the_rest_unchanged() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, base_report) = rig.render(64, vec![], &[])?;
    assert_eq!(
        base_report.features.get(names::DECALS).unwrap().activity,
        Activity::Disabled,
        "no decals, no pass"
    );
    assert_eq!(rig.pipeline.decal_report(ViewId::MAIN), None);

    let decal = Decal {
        albedo: red(),
        ..floor_decal(1.0)
    };
    let (image, report) = rig.render(64, vec![decal], &[])?;
    assert_eq!(
        report.features.get(names::DECALS).unwrap().activity,
        Activity::Ran
    );
    let decal_report = rig.pipeline.decal_report(ViewId::MAIN).unwrap();
    assert_eq!((decal_report.submitted, decal_report.drawn), (1, 1));

    // Inside the 2 m square (16 px): grey multiplied by red keeps only red.
    let reference = base.pixel(32, 32);
    assert!(
        close(
            image.pixel(32, 32),
            [reference[0], 0.0, 0.0, reference[3]],
            0.02
        ),
        "inside: {:?} vs base {reference:?}",
        image.pixel(32, 32)
    );
    // Inside near a corner of the square (7 px from the centre on each axis).
    assert!(
        close(
            image.pixel(25, 25),
            [reference[0], 0.0, 0.0, reference[3]],
            0.02
        ),
        "inside corner: {:?}",
        image.pixel(25, 25)
    );
    // Outside: identical to the decal-free render.
    for (x, y) in [
        (2, 2),
        (60, 60),
        (32, 8),
        (32, 56),
        (8, 32),
        (56, 32),
        (21, 32),
    ] {
        assert_eq!(image.pixel(x, y), base.pixel(x, y), "({x}, {y}) is outside");
    }
    let changed = changed_pixels(&base, &image);
    assert!(
        (200..=320).contains(&changed),
        "about 16 x 16 pixels change, got {changed}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn decal_respects_depth_surfaces_outside_the_box_are_unaffected() -> Result<()> {
    let mut rig = Rig::new()?;
    // A floating box that never reaches the floor.
    let mut floating = Decal {
        albedo: red(),
        ..floor_decal(1.0)
    };
    floating.center = Vec3::new(0.0, 3.0, 0.0);
    let (base, _) = rig.render(64, vec![], &[])?;
    let (image, _) = rig.render(64, vec![floating], &[])?;
    assert_eq!(changed_pixels(&base, &image), 0, "box above the floor");

    // A roof (outside the box, nearer to the camera) hides the left half of
    // a box that does reach the floor.
    let roof_mesh = quad(&rig.gpu, 2.0);
    let roof = unlit(
        "roof",
        &roof_mesh,
        Mat4::from_translation(Vec3::new(-2.0, 2.0, 0.0)),
        [0.2, 0.8, 0.2],
    );
    let decal = Decal {
        albedo: red(),
        ..floor_decal(1.0)
    };
    let (roof_base, _) = rig.render(64, vec![], std::slice::from_ref(&roof))?;
    let (roofed, _) = rig.render(64, vec![decal], std::slice::from_ref(&roof))?;
    assert!(
        close(roofed.pixel(28, 32), roof_base.pixel(28, 32), 1e-6),
        "the roof hides the volume: {:?}",
        roofed.pixel(28, 32)
    );
    let right = roofed.pixel(36, 32);
    assert!(
        right[1] < 0.01 && right[0] > 0.1,
        "uncovered half is decorated: {right:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn angle_fade_rejects_surfaces_facing_away_from_the_projection_axis() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, _) = rig.render(64, vec![], &[])?;
    let center = |image: &HdrImage| image.pixel(32, 32);
    let make = |axis: Vec3, max_angle_degrees: f32| Decal {
        rotation: Quat::from_rotation_arc(Vec3::Z, axis.normalize()),
        half_extents: Vec3::splat(3.0),
        albedo: red(),
        max_angle: max_angle_degrees.to_radians(),
        angle_fade: 0.0,
        edge_fade: 0.0,
        ..Default::default()
    };
    // Axis +Y: the floor faces the decal.
    let (facing, _) = rig.render(64, vec![make(Vec3::Y, 70.0)], &[])?;
    assert!(center(&facing)[1] < 0.01, "facing: {:?}", center(&facing));
    // Axis +X: the floor is edge-on (90 degrees).
    let (edge_on, _) = rig.render(64, vec![make(Vec3::X, 70.0)], &[])?;
    assert_eq!(center(&edge_on), center(&base), "edge-on");
    // Axis -Y: the floor faces away from the decal.
    let (away, _) = rig.render(64, vec![make(Vec3::NEG_Y, 89.0)], &[])?;
    assert_eq!(center(&away), center(&base), "facing away");
    // 45 degrees tilt: kept by a 60 degree cut-off, rejected by a 30 degree one.
    let tilted = Vec3::new(1.0, 1.0, 0.0);
    let (kept, _) = rig.render(64, vec![make(tilted, 60.0)], &[])?;
    assert!(center(&kept)[1] < 0.01, "within 60 degrees");
    let (rejected, _) = rig.render(64, vec![make(tilted, 30.0)], &[])?;
    assert_eq!(center(&rejected), center(&base), "beyond 30 degrees");
    // A soft cut-off applies part of the decal between the two angles.
    let mut soft = make(tilted, 50.0);
    soft.angle_fade = 20f32.to_radians();
    let (partial, _) = rig.render(64, vec![soft], &[])?;
    let g = center(&partial)[1];
    assert!(
        g > 0.01 && g < center(&base)[1] - 0.01,
        "partial angle fade: {g}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn edge_fade_softens_the_box_boundary() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, _) = rig.render(64, vec![], &[])?;
    let make = |edge_fade| Decal {
        albedo: red(),
        edge_fade,
        ..floor_decal(2.0)
    };
    // Half extent 2 m = 16 px: pixel x = 32 + 14 is 2 px (12%) from the face.
    let (hard, _) = rig.render(64, vec![make(0.0)], &[])?;
    let (soft, _) = rig.render(64, vec![make(0.5)], &[])?;
    let near_edge = (32 + 14, 32);
    assert!(hard.pixel(near_edge.0, near_edge.1)[1] < 0.01);
    let soft_g = soft.pixel(near_edge.0, near_edge.1)[1];
    let base_g = base.pixel(near_edge.0, near_edge.1)[1];
    assert!(
        soft_g > hard.pixel(near_edge.0, near_edge.1)[1] + 0.05 && soft_g <= base_g + 1e-4,
        "faded edge: {soft_g} between hard 0 and base {base_g}"
    );
    assert!(
        soft.pixel(32, 32)[1] < 0.01,
        "the centre is fully decorated"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn material_response_opacity_paint_and_emissive() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, _) = rig.render(64, vec![], &[])?;
    let b = base.pixel(32, 32);

    let half = Decal {
        albedo: red(),
        opacity: 0.5,
        ..floor_decal(1.0)
    };
    let (image, _) = rig.render(64, vec![half], &[])?;
    let p = image.pixel(32, 32);
    assert!(
        close(p, [b[0], b[1] * 0.5, b[2] * 0.5, b[3]], 0.02),
        "{p:?}"
    );

    let paint = Decal {
        albedo: Vec3::new(0.0, 0.0, 1.0),
        blend: DecalBlend::Paint,
        ..floor_decal(1.0)
    };
    let (image, _) = rig.render(64, vec![paint], &[])?;
    let luma = 0.2126 * b[0] + 0.7152 * b[1] + 0.0722 * b[2];
    let p = image.pixel(32, 32);
    assert!(close(p, [0.0, 0.0, luma, b[3]], 0.02), "{p:?} luma {luma}");

    let glow = Decal {
        opacity: 0.0,
        emissive: Vec3::new(0.0, 0.0, 2.0),
        ..floor_decal(1.0)
    };
    let (image, _) = rig.render(64, vec![glow], &[])?;
    let p = image.pixel(32, 32);
    assert!(close(p, [b[0], b[1], b[2] + 2.0, b[3]], 0.05), "{p:?}");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn decals_apply_in_list_order() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, _) = rig.render(64, vec![], &[])?;
    let b = base.pixel(32, 32);
    let multiply_red = Decal {
        albedo: red(),
        ..floor_decal(1.0)
    };
    let paint_green = Decal {
        albedo: Vec3::new(0.0, 1.0, 0.0),
        blend: DecalBlend::Paint,
        ..floor_decal(1.0)
    };
    let (red_then_green, _) =
        rig.render(64, vec![multiply_red.clone(), paint_green.clone()], &[])?;
    let (green_then_red, _) = rig.render(64, vec![paint_green, multiply_red], &[])?;
    let first = red_then_green.pixel(32, 32);
    let second = green_then_red.pixel(32, 32);
    // Red keeps (r, 0, 0); painting green then uses the luma of that.
    let red_luma = 0.2126 * b[0];
    assert!(close(first, [0.0, red_luma, 0.0, b[3]], 0.02), "{first:?}");
    // Green first, then multiplying by red leaves nothing.
    assert!(close(second, [0.0, 0.0, 0.0, b[3]], 0.02), "{second:?}");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn texture_alpha_masks_and_a_missing_texture_renders_flat_and_is_reported() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, _) = rig.render(64, vec![], &[])?;
    // Two texels: opaque red on the left (u < 0.5), transparent on the right.
    let texture = GpuTexture::rgba8(
        &rig.gpu.device,
        &rig.gpu.queue,
        "decal test texture",
        [2, 1],
        &[255, 0, 0, 255, 255, 255, 255, 0],
    )?;
    let textured = Decal {
        texture: DecalTexture::Texture(texture),
        ..floor_decal(1.0)
    };
    let (image, _) = rig.render(64, vec![textured], &[])?;
    // Local +X is world +X, which is screen right: x < 32 is the left texel.
    let left = image.pixel(26, 32);
    assert!(left[1] < 0.01 && left[0] > 0.1, "left texel: {left:?}");
    assert_eq!(image.pixel(38, 32), base.pixel(38, 32), "transparent texel");

    let missing = Decal {
        texture: DecalTexture::Missing,
        albedo: red(),
        ..floor_decal(1.0)
    };
    let (flat, _) = rig.render(64, vec![missing], &[])?;
    assert!(flat.pixel(26, 32)[1] < 0.01 && flat.pixel(38, 32)[1] < 0.01);
    let report = rig.pipeline.decal_report(ViewId::MAIN).unwrap();
    assert_eq!(report.missing_textures, 1);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn overflow_keeps_the_first_decals_and_reports_the_dropped_ones() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base, _) = rig.render(64, vec![], &[])?;
    // 64 decals far from the centre, then three more, the first of which would
    // paint the centre red if it were not dropped.
    let mut decals = vec![
        Decal {
            center: Vec3::new(3.0, 0.0, 3.0),
            ..floor_decal(0.25)
        };
        MAX_DECALS
    ];
    decals.push(Decal {
        albedo: red(),
        ..floor_decal(1.0)
    });
    decals.push(floor_decal(0.25));
    decals.push(floor_decal(0.25));
    let (image, _) = rig.render(64, decals, &[])?;
    let report = rig.pipeline.decal_report(ViewId::MAIN).unwrap();
    assert_eq!(report.submitted, MAX_DECALS + 3);
    assert_eq!(report.drawn, MAX_DECALS);
    assert_eq!(report.dropped_overflow, 3);
    assert_eq!(image.pixel(32, 32), base.pixel(32, 32), "dropped decal");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn invalid_decals_fail_the_frame_with_the_decal_index() -> Result<()> {
    let mut rig = Rig::new()?;
    let bad = Decal {
        opacity: f32::NAN,
        ..floor_decal(1.0)
    };
    let error = rig
        .render(64, vec![floor_decal(1.0), bad], &[])
        .expect_err("a NaN opacity is invalid");
    let message = format!("{error:#}");
    assert!(
        message.contains("decal 1") && message.contains("opacity"),
        "{message}"
    );
    // The pipeline recovers on the next valid frame.
    let (_, report) = rig.render(64, vec![floor_decal(1.0)], &[])?;
    assert_eq!(
        report.features.get(names::DECALS).unwrap().activity,
        Activity::Ran
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn resizing_and_removing_decals_keep_the_output_correct() -> Result<()> {
    let mut rig = Rig::new()?;
    let (base64, _) = rig.render(64, vec![], &[])?;
    let decal = || Decal {
        albedo: red(),
        ..floor_decal(1.0)
    };
    let (small, _) = rig.render(64, vec![decal()], &[])?;
    assert!(small.pixel(32, 32)[1] < 0.01);

    // Larger render targets: the pass follows the new size (16 px per metre).
    let (large, _) = rig.render(96, vec![decal()], &[])?;
    assert_eq!(large.size, [96, 96]);
    assert!(large.pixel(48, 48)[1] < 0.01, "decorated centre");
    assert!(large.pixel(4, 4)[1] > 0.1, "untouched corner");
    let (large_base, _) = rig.render(96, vec![], &[])?;
    let changed = changed_pixels(&large_base, &large);
    // 2 m at 12 px per metre is about 24 x 24 pixels.
    assert!((450..=700).contains(&changed), "changed {changed}");

    // Removing the decal restores the undecorated output exactly.
    let (removed, report) = rig.render(64, vec![], &[])?;
    assert_eq!(changed_pixels(&base64, &removed), 0, "decal removal");
    assert_eq!(
        report.features.get(names::DECALS).unwrap().activity,
        Activity::Disabled
    );
    assert_eq!(rig.pipeline.decal_report(ViewId::MAIN), None);

    // An explicit override disables a view's decals.
    let mut view = top_view(64);
    view.decals = vec![decal()];
    view.features.set(names::DECALS, false);
    let meshes = [unlit("floor", &rig.floor, Mat4::IDENTITY, [GREY; 3])];
    let (_texture, output) = target(&rig.gpu, 64);
    rig.pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &meshes,
            lights: &LightList::default(),
            world: None,
        },
        &output,
        &mut NoHost,
    )?;
    let overridden = rig.pipeline.read_scene_hdr(ViewId::MAIN)?;
    assert_eq!(changed_pixels(&base64, &overridden), 0, "explicit disable");
    rig.gpu.check_errors()?;
    Ok(())
}
