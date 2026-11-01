//! Real-adapter tests of screen-space reflections (`engine.ssr`): a mirror
//! floor under a coloured unlit wall, the image-based fallback on a miss and
//! off screen, roughness cutoff, RT-reflection exclusion, the surface-material
//! producer (shaded normal, F0, custom shaders), feature-off identity, resize,
//! several views and the Hi-Z compacted camera path. Every expectation is read
//! back from the scene HDR target or the surface-material targets.
//!
//! Run with `cargo test -p engine-render-pipeline --test gpu_ssr --
//! --ignored --test-threads=1 --nocapture`.
use anyhow::Result;
use engine_render_pipeline::indirect::*;
use engine_render_pipeline::visibility::MeshVisibility;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Quat, Vec3};
use std::collections::BTreeMap;
use std::sync::Arc;

const SIZE: [u32; 2] = [128, 96];
const EYE: Vec3 = Vec3::new(0.0, 2.5, 6.0);
const FOCUS: Vec3 = Vec3::new(0.0, 0.5, 0.0);
/// Unlit wall colour (tint x white texture x vertex colour).
const RED: [f32; 3] = [1.0, 0.1, 0.1];
/// Constant specular radiance of the test environment. The engine's capped
/// GGX keeps a mirror's image-based specular near 4.5% of it (documented in
/// docs/rendering-indirect.md), which is still far from the wall's colour.
const ENV: [f32; 3] = [2.0, 3.0, 5.0];
const WALL_Z: f32 = -1.0;
const WALL_HEIGHT: f32 = 1.2;

struct Harness {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    floor: Arc<GpuMesh>,
    wall: Arc<GpuMesh>,
    patch: Arc<GpuMesh>,
}

fn vertex(position: [f32; 3], uv: [f32; 2], normal: [f32; 3]) -> EngineVertex {
    EngineVertex {
        position,
        uv,
        color: [1.0; 4],
        normal,
    }
}

impl Harness {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("ssr adapter: {:?}", gpu.adapter.get_info());
        let up = [0.0, 1.0, 0.0];
        // A 16 x 16 floor in the XZ plane facing +Y; uv follows x and z.
        let floor = GpuMesh::from_engine_vertices(
            &gpu.device,
            "ssr floor",
            &[(-8.0, -8.0), (8.0, -8.0), (8.0, 8.0), (-8.0, 8.0)]
                .map(|(x, z)| vertex([x, 0.0, z], [x / 16.0 + 0.5, z / 16.0 + 0.5], up)),
            &[0, 2, 1, 0, 3, 2],
        )?;
        // A vertical wall facing +Z standing on the floor, 1.2 x 1.2.
        let wall = GpuMesh::from_engine_vertices(
            &gpu.device,
            "ssr wall",
            &[
                (-0.6, 0.0),
                (0.6, 0.0),
                (0.6, WALL_HEIGHT),
                (-0.6, WALL_HEIGHT),
            ]
            .map(|(x, y)| vertex([x, y, 0.0], [0.5; 2], [0.0, 0.0, 1.0])),
            &[0, 1, 2, 0, 2, 3],
        )?;
        // A 2 x 2 patch just above the floor (hides the floor under it).
        let patch = GpuMesh::from_engine_vertices(
            &gpu.device,
            "ssr patch",
            &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .map(|(x, z)| vertex([x, 0.0, z], [0.5; 2], up)),
            &[0, 2, 1, 0, 3, 2],
        )?;
        Ok(Self {
            pipeline: ScenePipeline::new(gpu.clone())?,
            gpu,
            floor,
            wall,
            patch,
        })
    }

    fn floor_mesh<'a>(&self, id: &'a str, metallic: f32, roughness: f32) -> SceneMesh<'a> {
        SceneMesh {
            id,
            mesh: self.floor.clone(),
            world_from_local: Mat4::IDENTITY,
            shader: None,
            params: MaterialParams {
                metallic,
                roughness,
                ..Default::default()
            },
            base_color: None,
            metallic_roughness: None,
            normal_map: None,
            lightmap: None,
        }
    }

    fn wall_mesh<'a>(&self, id: &'a str, x: f32) -> SceneMesh<'a> {
        SceneMesh {
            id,
            mesh: self.wall.clone(),
            world_from_local: Mat4::from_translation(Vec3::new(x, 0.0, WALL_Z)),
            shader: None,
            params: MaterialParams {
                tint: [RED[0], RED[1], RED[2], 1.0],
                unlit: true,
                ..Default::default()
            },
            base_color: None,
            metallic_roughness: None,
            normal_map: None,
            lightmap: None,
        }
    }

    /// The standard scene: the mirror floor and the red wall.
    fn scene<'a>(&self, roughness: f32) -> Vec<SceneMesh<'a>> {
        vec![
            self.floor_mesh("floor", 1.0, roughness),
            self.wall_mesh("wall", 0.0),
        ]
    }

    fn view(&self, id: ViewId, size: [u32; 2]) -> ViewData {
        let projection = glam::camera::rh::proj::directx::perspective(
            0.9,
            size[0] as f32 / size[1] as f32,
            0.1,
            100.0,
        );
        ViewData::new(
            id,
            glam::camera::rh::view::look_at_mat4(EYE, FOCUS, Vec3::Y).inverse(),
            projection,
            size,
            size,
        )
        .unwrap()
    }

    fn render(
        &mut self,
        view: &ViewData,
        meshes: &[SceneMesh<'_>],
        visibility: Option<&[MeshVisibility]>,
    ) -> Result<ViewReport> {
        let size = view.output_size;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ssr output"),
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
            format: texture.format(),
            size,
            viewport: Viewport::full(size),
        };
        let report = self.pipeline.render_view_with_visibility(
            &SceneFrame {
                view,
                meshes,
                lights: &LightList::default(),
                world: None,
            },
            &target,
            &mut NoHost,
            visibility,
        )?;
        self.gpu.check_errors()?;
        Ok(report)
    }

    /// Render the main view at `SIZE` and read its scene HDR.
    fn draw(
        &mut self,
        meshes: &[SceneMesh<'_>],
        configure: impl FnOnce(&mut ViewData),
    ) -> Result<HdrImage> {
        let mut view = self.view(ViewId::MAIN, SIZE);
        configure(&mut view);
        self.render(&view, meshes, None)?;
        self.pipeline.read_scene_hdr(ViewId::MAIN)
    }

    fn indirect(&mut self, id: ViewId) -> Result<()> {
        let lut = BrdfLut::prepare(32, 1024)?;
        let environment = EnvironmentData::new(
            DiffusePolynomial::new([[0.0; 3]; 9])?,
            (0..4)
                .map(|mip| {
                    std::array::from_fn(|_| {
                        let edge = 8u32 >> mip;
                        RadianceImage::new([edge; 2], vec![ENV; (edge * edge) as usize]).unwrap()
                    })
                })
                .collect(),
        )?;
        let resources = Arc::new(GpuIndirectResources::prepare(
            &self.gpu,
            &[environment],
            &[],
            &lut,
        )?);
        let lighting = EnvironmentLighting::new(0, Quat::IDENTITY, 1.0)?;
        self.pipeline.set_indirect(
            id,
            Some(Arc::new(IndirectScene::new(
                resources,
                Some(lighting),
                vec![],
            )?)),
        )
    }
}

fn ssr() -> impl FnOnce(&mut ViewData) {
    |view| view.ssr = Some(SsrSettings::default())
}

/// Pixel of a world point in a view.
fn pixel_of(view: &ViewData, point: Vec3) -> (u32, u32) {
    let clip = view.clip_from_world() * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x * 0.5 + 0.5) * view.render_size[0] as f32) as u32,
        ((0.5 - ndc.y * 0.5) * view.render_size[1] as f32) as u32,
    )
}

/// The floor point where the mirror image of `point` (above the floor) shows.
fn mirror_point(point: Vec3) -> Vec3 {
    let image = Vec3::new(point.x, -point.y, point.z);
    let t = EYE.y / (EYE.y - image.y);
    EYE + (image - EYE) * t
}

fn wall_centre(x: f32) -> Vec3 {
    Vec3::new(x, WALL_HEIGHT * 0.5, WALL_Z)
}

fn at(image: &HdrImage, (x, y): (u32, u32)) -> [f32; 4] {
    image.pixel(x, y)
}

fn close(actual: [f32; 4], expected: [f32; 3], tolerance: f32, what: &str) {
    for c in 0..3 {
        assert!(
            (actual[c] - expected[c]).abs() <= tolerance,
            "{what}: {actual:?} vs {expected:?} (channel {c}, tolerance {tolerance})"
        );
    }
}

fn is_dark(pixel: [f32; 4]) -> bool {
    pixel[..3].iter().all(|v| v.abs() < 0.005)
}

fn state(pipeline: &ScenePipeline, view: ViewId, name: &str) -> Activity {
    pipeline
        .view_report(view)
        .unwrap()
        .features
        .get(name)
        .unwrap()
        .activity
        .clone()
}

#[test]
#[ignore = "requires a real GPU; the reflected wall is read back from the floor"]
fn a_mirror_floor_shows_the_wall_and_leaves_misses_untouched() -> Result<()> {
    let mut h = Harness::new()?;
    let meshes = h.scene(0.0);
    let view = h.view(ViewId::MAIN, SIZE);
    let reflected = pixel_of(&view, mirror_point(wall_centre(0.0)));
    let miss = pixel_of(&view, Vec3::new(-3.0, 0.0, 0.355));
    let wall = pixel_of(&view, wall_centre(0.0));
    let base_row = pixel_of(&view, Vec3::new(0.0, 0.0, WALL_Z)).1;
    let off = h.draw(&meshes, |_| {})?;
    assert!(is_dark(at(&off, reflected)), "no lighting, no environment");
    close(at(&off, wall), RED, 0.01, "unlit wall");

    let on = h.draw(&meshes, ssr())?;
    close(at(&on, reflected), RED, 0.03, "the wall in the mirror");
    for (dx, dy) in [(-3i32, 0i32), (3, 0), (0, -2), (0, 2), (-2, 2)] {
        let p = (
            (reflected.0 as i32 + dx) as u32,
            (reflected.1 as i32 + dy) as u32,
        );
        close(
            at(&on, p),
            RED,
            0.05,
            &format!("reflection neighbourhood {dx},{dy}"),
        );
    }
    // Misses, geometry and background are bit-identical to the baseline.
    assert_eq!(at(&on, miss), at(&off, miss));
    assert_eq!(at(&on, wall), at(&off, wall));
    assert_eq!(at(&on, (1, 1)), at(&off, (1, 1)));
    // SSR only ever adds the wall's colour, and only below the wall's base.
    let mut changed = 0;
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let (a, b) = (off.pixel(x, y), on.pixel(x, y));
            assert!(b.iter().all(|v| v.is_finite()), "finite at {x},{y}");
            if a != b {
                changed += 1;
                assert!(
                    y + 1 >= base_row,
                    "changed pixel {x},{y} above the wall base"
                );
                assert!(b[0] > a[0] && b[1] >= a[1] && b[2] >= a[2]);
                // Only the wall's hue ever appears: no stray hits on the floor.
                assert!(
                    (b[1] - RED[1] * b[0]).abs() < 0.02 && (b[2] - RED[2] * b[0]).abs() < 0.02,
                    "changed pixel {x},{y} is not the wall colour: {b:?}"
                );
            }
        }
    }
    assert!(changed > 80, "the reflection covers pixels: {changed}");
    assert!(changed < SIZE[0] * SIZE[1] / 4);
    // The pass ran and the surface material was produced for the view.
    assert_eq!(state(&h.pipeline, ViewId::MAIN, names::SSR), Activity::Ran);
    assert_eq!(
        state(&h.pipeline, ViewId::MAIN, names::SURFACE_MATERIAL),
        Activity::Ran
    );
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; image-based specular is replaced on a hit and kept on a miss"]
fn the_environment_specular_is_the_fallback_on_misses_and_off_screen() -> Result<()> {
    let mut h = Harness::new()?;
    h.indirect(ViewId::MAIN)?;
    let meshes = h.scene(0.0);
    let view = h.view(ViewId::MAIN, SIZE);
    let reflected = pixel_of(&view, mirror_point(wall_centre(0.0)));
    let miss = pixel_of(&view, Vec3::new(-3.0, 0.0, 0.355));

    let off = h.draw(&meshes, |_| {})?;
    let fallback = at(&off, reflected);
    assert!(
        fallback[2] > 0.15 && fallback[0] > 0.05,
        "environment shows: {fallback:?}"
    );
    assert_eq!(fallback, at(&off, miss), "the environment is uniform");
    let on = h.draw(&meshes, ssr())?;
    // A hit replaces the environment specular; it does not add to it.
    close(at(&on, reflected), RED, 0.03, "wall replaces environment");
    assert_eq!(at(&on, miss), at(&off, miss), "a miss keeps the fallback");
    // No black holes anywhere: every lit pixel stays lit and finite.
    for y in 0..SIZE[1] {
        for x in 0..SIZE[0] {
            let (a, b) = (off.pixel(x, y), on.pixel(x, y));
            assert!(b.iter().all(|v| v.is_finite()));
            let bright = |p: [f32; 4]| p[..3].iter().fold(0.0f32, |m, v| m.max(*v));
            if bright(a) > 0.05 {
                assert!(bright(b) > 0.05, "black hole at {x},{y}: {a:?} -> {b:?}");
            }
        }
    }

    // With the wall far outside the frame every ray leaves the screen or
    // misses: the whole frame is the environment fallback, unchanged.
    let away = [h.floor_mesh("floor", 1.0, 0.0), h.wall_mesh("wall", -40.0)];
    let off_screen_off = h.draw(&away, |_| {})?;
    let off_screen_on = h.draw(&away, ssr())?;
    assert_eq!(
        off_screen_off, off_screen_on,
        "off-screen reflectors change nothing"
    );
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; roughness cutoff and fade"]
fn rough_surfaces_are_cut_off_and_faded() -> Result<()> {
    let mut h = Harness::new()?;
    h.indirect(ViewId::MAIN)?;
    let view = h.view(ViewId::MAIN, SIZE);
    let reflected = pixel_of(&view, mirror_point(wall_centre(0.0)));

    let rough = h.scene(0.8);
    let off = h.draw(&rough, |_| {})?;
    let on = h.draw(&rough, ssr())?;
    assert_eq!(off, on, "roughness above the cutoff is bit-identical");
    assert_eq!(
        state(&h.pipeline, ViewId::MAIN, names::SSR),
        Activity::Ran,
        "the pass ran and copied"
    );

    // Between half the cutoff and the cutoff the blend is partial.
    let mirror = h.draw(&h.scene(0.0), ssr())?;
    let mid_scene = h.scene(0.3);
    let mid_off = h.draw(&mid_scene, |_| {})?;
    let mid_on = h.draw(&mid_scene, ssr())?;
    let (a, b, c) = (
        at(&mid_off, reflected),
        at(&mid_on, reflected),
        at(&mirror, reflected),
    );
    // Fallback environment `a`, the half-faded rough hit `b` and the full
    // mirror replacement `c` are strictly ordered (blue is the environment's
    // strongest channel and the wall's weakest).
    assert!(
        c[2] + 0.3 < b[2] && b[2] < a[2] - 0.3,
        "partial blend: {a:?} {b:?} {c:?}"
    );

    // A tighter cutoff removes the same surface again.
    let strict = h.draw(&mid_scene, |view| {
        view.ssr = Some(SsrSettings {
            roughness_cutoff: 0.25,
            ..Default::default()
        })
    })?;
    assert_eq!(strict, mid_off);
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; enable, zero intensity, disable and feature-off identity"]
fn the_feature_is_off_by_default_and_identity_when_off_or_idle() -> Result<()> {
    let mut h = Harness::new()?;
    h.indirect(ViewId::MAIN)?;
    let meshes = h.scene(0.0);
    let baseline = h.draw(&meshes, |_| {})?;
    assert_eq!(
        state(&h.pipeline, ViewId::MAIN, names::SSR),
        Activity::Disabled
    );
    assert!(
        h.pipeline
            .targets(ViewId::MAIN)
            .unwrap()
            .surface_material
            .is_none()
    );
    assert!(h.pipeline.read_surface_material(ViewId::MAIN).is_err());

    let on = h.draw(&meshes, ssr())?;
    assert_ne!(on, baseline);
    assert!(
        h.pipeline
            .targets(ViewId::MAIN)
            .unwrap()
            .surface_material
            .is_some()
    );

    // Zero intensity is requested but idle: nothing drawn, nothing allocated.
    let idle = h.draw(&meshes, |view| {
        view.ssr = Some(SsrSettings {
            intensity: 0.0,
            ..Default::default()
        })
    })?;
    assert_eq!(idle, baseline);
    assert_eq!(state(&h.pipeline, ViewId::MAIN, names::SSR), Activity::Idle);
    assert!(
        h.pipeline
            .targets(ViewId::MAIN)
            .unwrap()
            .surface_material
            .is_none()
    );

    // An explicit override wins over the settings and over enabling by settings.
    let overridden = h.draw(&meshes, |view| {
        view.ssr = Some(SsrSettings::default());
        view.features = FeatureOverrides(BTreeMap::from([(names::SSR.to_owned(), false)]));
    })?;
    assert_eq!(overridden, baseline);
    // A bare feature flag enables the pass with the default settings.
    let flagged = h.draw(&meshes, |view| {
        view.features = FeatureOverrides(BTreeMap::from([(names::SSR.to_owned(), true)]));
    })?;
    assert_eq!(flagged, on);

    // Turning it off again restores the baseline exactly and frees the targets.
    let again = h.draw(&meshes, |_| {})?;
    assert_eq!(again, baseline);
    assert!(
        h.pipeline
            .targets(ViewId::MAIN)
            .unwrap()
            .surface_material
            .is_none()
    );

    // Settings are validated before anything is submitted.
    let mut view = h.view(ViewId::MAIN, SIZE);
    view.ssr = Some(SsrSettings {
        max_steps: 0,
        ..Default::default()
    });
    assert!(h.render(&view, &meshes, None).is_err());
    Ok(())
}

struct RtStub;
impl PipelineFeature for RtStub {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::RT_REFLECTIONS, Stage::Prepass).disabled_by_default()
    }
    fn encode(&mut self, _: &mut FeatureContext<'_>) -> Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "requires a real GPU; RT reflections take the view and report why SSR is off"]
fn enabled_rt_reflections_turn_ssr_off_for_that_view_with_a_reason() -> Result<()> {
    let mut h = Harness::new()?;
    let mut registry = FeatureRegistry::default();
    registry.register_engine(RtStub)?;
    h.pipeline.add_features(registry)?;
    let meshes = h.scene(0.0);
    let baseline = h.draw(&meshes, |_| {})?;
    let on = h.draw(&meshes, ssr())?;
    assert_ne!(on, baseline);

    let both = h.draw(&meshes, |view| {
        view.ssr = Some(SsrSettings::default());
        view.features =
            FeatureOverrides(BTreeMap::from([(names::RT_REFLECTIONS.to_owned(), true)]));
    })?;
    assert_eq!(
        both, baseline,
        "SSR is off where RT reflections are enabled"
    );
    assert_eq!(
        state(&h.pipeline, ViewId::MAIN, names::SSR),
        Activity::Superseded(names::RT_REFLECTIONS.to_owned())
    );
    assert!(
        h.pipeline
            .targets(ViewId::MAIN)
            .unwrap()
            .surface_material
            .is_none()
    );

    // Another view without RT reflections still gets SSR.
    let mut other = h.view(ViewId(5), SIZE);
    other.ssr = Some(SsrSettings::default());
    h.render(&other, &meshes, None)?;
    let other_image = h.pipeline.read_scene_hdr(ViewId(5))?;
    assert_eq!(other_image, on);
    assert_eq!(state(&h.pipeline, ViewId(5), names::SSR), Activity::Ran);
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; the surface-material producer reads back normals, F0, coverage"]
fn the_surface_material_describes_lit_surfaces_and_marks_the_rest_non_reflective() -> Result<()> {
    let mut h = Harness::new()?;
    h.indirect(ViewId::MAIN)?;
    let mut meshes = h.scene(0.25);
    // A custom-shader patch over the floor: drawn by the opaque pass, but not
    // described by the surface material.
    meshes.push(SceneMesh {
        id: "custom",
        mesh: h.patch.clone(),
        world_from_local: Mat4::from_translation(Vec3::new(1.8, 0.02, 1.0)),
        shader: Some(LIT_WGSL),
        params: MaterialParams {
            metallic: 1.0,
            roughness: 0.0,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
        lightmap: None,
    });
    let mut view = h.view(ViewId::MAIN, SIZE);
    view.ssr = Some(SsrSettings::default());
    let reference = h.view(ViewId::MAIN, SIZE);
    h.render(&view, &meshes, None)?;
    let [normal, f0, fallback] = h.pipeline.read_surface_material(ViewId::MAIN)?;
    let floor = pixel_of(&reference, Vec3::new(-1.8, 0.0, 2.0));
    let n = at(&normal, floor);
    close(n, [0.0, 1.0, 0.0], 0.01, "floor normal faces up");
    assert!(
        (n[3] - 0.25).abs() < 0.002,
        "perceptual roughness: {}",
        n[3]
    );
    let f = at(&f0, floor);
    close(f, [1.0; 3], 0.01, "metallic white floor: F0 is the albedo");
    assert_eq!(f[3], 1.0, "coverage");
    // The unlit wall is described but marked non-reflective; the custom
    // shader patch and the background are not described at all.
    let wall = pixel_of(&reference, wall_centre(0.0));
    assert_eq!(at(&f0, wall)[3], 0.0, "unlit wall is non-reflective");
    let patch = pixel_of(&reference, Vec3::new(1.8, 0.02, 1.0));
    assert_eq!(at(&f0, patch), [0.0; 4], "custom shader patch");
    assert_eq!(
        at(&normal, patch),
        [0.0; 4],
        "custom shader patch writes no normal"
    );
    assert_eq!(at(&f0, (1, 1)), [0.0; 4], "background");

    // The SSR result on those pixels is the opaque colour, untouched.
    let on = h.pipeline.read_scene_hdr(ViewId::MAIN)?;
    // The fallback target is the specular the opaque pass added: for a lit
    // metal with no lights it is the whole scene colour away from reflections.
    let pixel = at(&on, floor);
    assert!(pixel[2] > 0.15, "environment specular present: {pixel:?}");
    close(
        at(&fallback, floor),
        [pixel[0], pixel[1], pixel[2]],
        0.005,
        "fallback specular",
    );
    let mut plain = h.view(ViewId::MAIN, SIZE);
    plain.ssr = None;
    h.render(&plain, &meshes, None)?;
    let off = h.pipeline.read_scene_hdr(ViewId::MAIN)?;
    for point in [wall_centre(0.0), Vec3::new(1.8, 0.02, 1.0)] {
        let p = pixel_of(&reference, point);
        assert_eq!(at(&on, p), at(&off, p));
    }
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; the produced normal includes the normal map"]
fn the_surface_normal_is_the_normal_mapped_shading_normal() -> Result<()> {
    let mut h = Harness::new()?;
    // Tangent-space tilt (0.6, 0, 0.8) towards the +u (+x) tangent.
    let map = GpuTexture::rgba8(
        &h.gpu.device,
        &h.gpu.queue,
        "tilt",
        [1, 1],
        &[204, 128, 230, 255],
    )?;
    let mut floor = h.floor_mesh("floor", 1.0, 0.0);
    floor.normal_map = Some(map);
    let mut view = h.view(ViewId::MAIN, SIZE);
    view.ssr = Some(SsrSettings::default());
    h.render(&view, &[floor], None)?;
    let [normal, _, _] = h.pipeline.read_surface_material(ViewId::MAIN)?;
    let p = pixel_of(&view, Vec3::new(1.0, 0.0, 2.0));
    close(at(&normal, p), [0.6, 0.8, 0.0], 0.03, "normal-mapped floor");
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; resize, independent views and forget"]
fn resize_two_views_and_forget_keep_their_own_surface_targets() -> Result<()> {
    let mut h = Harness::new()?;
    let meshes = h.scene(0.0);
    let expected = |view: &ViewData| pixel_of(view, mirror_point(wall_centre(0.0)));

    let mut a = h.view(ViewId::MAIN, SIZE);
    a.ssr = Some(SsrSettings::default());
    let small = [64, 48];
    let mut b = h.view(ViewId(9), small);
    b.ssr = Some(SsrSettings::default());
    let plain = h.view(ViewId(10), small);
    for _ in 0..2 {
        h.render(&a, &meshes, None)?;
        h.render(&b, &meshes, None)?;
        h.render(&plain, &meshes, None)?;
        let image_a = h.pipeline.read_scene_hdr(ViewId::MAIN)?;
        let image_b = h.pipeline.read_scene_hdr(ViewId(9))?;
        let image_plain = h.pipeline.read_scene_hdr(ViewId(10))?;
        close(at(&image_a, expected(&a)), RED, 0.04, "view A reflection");
        close(at(&image_b, expected(&b)), RED, 0.08, "view B reflection");
        assert!(
            is_dark(at(&image_plain, expected(&plain))),
            "view without SSR"
        );
        assert_eq!(
            h.pipeline
                .targets(ViewId::MAIN)
                .unwrap()
                .surface_material
                .as_ref()
                .unwrap()
                .f0_coverage
                .size(),
            SIZE
        );
        assert_eq!(
            h.pipeline
                .targets(ViewId(9))
                .unwrap()
                .surface_material
                .as_ref()
                .unwrap()
                .f0_coverage
                .size(),
            small
        );
        assert!(
            h.pipeline
                .targets(ViewId(10))
                .unwrap()
                .surface_material
                .is_none()
        );
    }

    // Resize view A: the targets follow and the reflection moves with the pixel grid.
    let resized = [96, 72];
    let mut a2 = h.view(ViewId::MAIN, resized);
    a2.ssr = Some(SsrSettings::default());
    h.render(&a2, &meshes, None)?;
    let image = h.pipeline.read_scene_hdr(ViewId::MAIN)?;
    assert_eq!(image.size, resized);
    close(at(&image, expected(&a2)), RED, 0.06, "resized reflection");
    assert_eq!(
        h.pipeline
            .targets(ViewId::MAIN)
            .unwrap()
            .surface_material
            .as_ref()
            .unwrap()
            .normal_roughness
            .size(),
        resized
    );
    let [normal, _, _] = h.pipeline.read_surface_material(ViewId::MAIN)?;
    assert_eq!(normal.size, resized);

    // Forgetting a view releases its targets; the other view is unaffected.
    h.pipeline.forget_view(ViewId::MAIN);
    assert!(h.pipeline.targets(ViewId::MAIN).is_none());
    h.render(&b, &meshes, None)?;
    let image_b = h.pipeline.read_scene_hdr(ViewId(9))?;
    close(
        at(&image_b, expected(&b)),
        RED,
        0.08,
        "view B after forgetting A",
    );
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; orthographic projection traces too"]
fn an_orthographic_view_reflects_the_wall() -> Result<()> {
    let mut h = Harness::new()?;
    let meshes = h.scene(0.0);
    let mut view = h.view(ViewId::MAIN, SIZE);
    view.clip_from_view =
        glam::camera::rh::proj::directx::orthographic(-4.0, 4.0, -3.0, 3.0, 0.1, 60.0);
    let off = {
        h.render(&view, &meshes, None)?;
        h.pipeline.read_scene_hdr(ViewId::MAIN)?
    };
    view.ssr = Some(SsrSettings::default());
    h.render(&view, &meshes, None)?;
    let on = h.pipeline.read_scene_hdr(ViewId::MAIN)?;
    let changed = off
        .pixels
        .iter()
        .zip(&on.pixels)
        .filter(|(a, b)| a != b)
        .count();
    assert!(changed > 40, "the wall appears in the mirror: {changed}");
    assert!(on.pixels.iter().all(|p| p.iter().all(|v| v.is_finite())));
    // Every changed pixel is the wall's colour (nothing else is reflective).
    for (a, b) in off.pixels.iter().zip(&on.pixels) {
        if a != b {
            close(*b, RED, 0.06, "orthographic reflection");
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a real GPU; the Hi-Z compacted camera path keeps SSR intact"]
fn ssr_works_over_the_hiz_compacted_opaque_path() -> Result<()> {
    let mut h = Harness::new()?;
    let meshes = h.scene(0.0);
    let metadata = [
        MeshVisibility {
            bounds: Some(([-8.0, 0.0, -8.0], [8.0, 0.0, 8.0])),
            occluder: true,
            shadow_mesh: None,
        },
        MeshVisibility {
            bounds: Some(([-0.6, 0.0, 0.0], [0.6, WALL_HEIGHT, 0.0])),
            occluder: false,
            shadow_mesh: None,
        },
    ];
    let mut view = h.view(ViewId::MAIN, SIZE);
    view.ssr = Some(SsrSettings::default());
    h.render(&view, &meshes, None)?;
    let direct = h.pipeline.read_scene_hdr(ViewId::MAIN)?;

    view.features = FeatureOverrides(BTreeMap::from([(names::HIZ_VISIBILITY.to_owned(), true)]));
    let report = h.render(&view, &meshes, Some(&metadata))?;
    let hiz = h.pipeline.read_scene_hdr(ViewId::MAIN)?;
    assert!(report.visibility.active, "{:?}", report.visibility);
    let reflected = pixel_of(&view, mirror_point(wall_centre(0.0)));
    close(
        at(&hiz, reflected),
        RED,
        0.03,
        "reflection over indirect draws",
    );
    assert_eq!(hiz, direct, "compaction changes no pixel");
    Ok(())
}
