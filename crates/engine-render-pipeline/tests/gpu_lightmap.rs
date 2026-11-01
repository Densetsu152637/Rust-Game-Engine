//! Real-adapter tests of supplied-lightmap consumption by the built-in lit mesh
//! shader: second UV stream, per-instance atlas regions on shared geometry,
//! replace/add against probe and ambient diffuse, untouched unbound draws, and
//! the Hi-Z compacted camera path. Every expectation is read back from the
//! scene HDR target; nothing here bakes anything.
//!
//! Run with `cargo test -p engine-render-pipeline --test gpu_lightmap --
//! --ignored --test-threads=1 --nocapture`.
use anyhow::Result;
use engine_render_pipeline::indirect::*;
use engine_render_pipeline::visibility::MeshVisibility;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Quat, Vec3};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Dielectric Fresnel at roughness 1 is F0 for every view angle, so baked
/// diffuse is weighted by exactly 1 - 0.04.
const WEIGHT: f32 = 0.96;
const SIZE: [u32; 2] = [64, 64];

struct Harness {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    /// Quad (half 0.35) with UV2 spanning the full atlas, v down.
    quad: Arc<GpuMesh>,
    /// The same quad without a second UV set.
    plain: Arc<GpuMesh>,
}

fn quad_vertices(half: f32) -> [EngineVertex; 4] {
    [(-half, -half), (half, -half), (half, half), (-half, half)].map(|(x, y)| EngineVertex {
        position: [x, y, 0.0],
        uv: [0.5; 2],
        color: [1.0; 4],
        normal: [0.0, 0.0, 1.0],
    })
}

/// UV2 of `quad_vertices`: u rightwards, v downwards (top texture row first).
fn quad_uv2(half: f32) -> [[f32; 2]; 4] {
    quad_vertices(half).map(|v| {
        [
            (v.position[0] / half + 1.0) * 0.5,
            (1.0 - v.position[1] / half) * 0.5,
        ]
    })
}

impl Harness {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("lightmap adapter: {:?}", gpu.adapter.get_info());
        let half = 0.35;
        let indices = [0, 1, 2, 0, 2, 3];
        let quad = GpuMesh::from_engine_vertices_with_uv2(
            &gpu.device,
            "lightmap quad",
            &quad_vertices(half),
            &quad_uv2(half),
            &indices,
        )?;
        let plain = GpuMesh::from_engine_vertices(
            &gpu.device,
            "plain quad",
            &quad_vertices(half),
            &indices,
        )?;
        Ok(Self {
            pipeline: ScenePipeline::new(gpu.clone())?,
            gpu,
            quad,
            plain,
        })
    }

    fn draw(&mut self, meshes: &[SceneMesh<'_>], ambient: Vec3) -> Result<HdrImage> {
        let mut view = ViewData::new(
            ViewId::MAIN,
            Mat4::from_translation(Vec3::new(0.0, 0.0, 3.0)),
            glam::camera::rh::proj::directx::orthographic(-2.0, 2.0, -2.0, 2.0, 0.1, 10.0),
            SIZE,
            SIZE,
        )?;
        view.ambient = ambient;
        let output = target(&self.gpu);
        self.pipeline.render_view(
            &SceneFrame {
                view: &view,
                meshes,
                lights: &LightList::default(),
                world: None,
            },
            &output,
            &mut NoHost,
        )?;
        self.gpu.check_errors()?;
        self.pipeline.read_scene_hdr(ViewId::MAIN)
    }

    fn mesh<'a>(&self, id: &'a str, x: f32, lightmap: Option<LightmapBinding>) -> SceneMesh<'a> {
        SceneMesh {
            id,
            mesh: self.quad.clone(),
            world_from_local: Mat4::from_translation(Vec3::new(x, 0.0, 0.0)),
            shader: None,
            params: MaterialParams {
                roughness: 1.0,
                ..Default::default()
            },
            base_color: None,
            metallic_roughness: None,
            normal_map: None,
            lightmap,
        }
    }

    fn scene(
        &self,
        environments: &[EnvironmentData],
        global: Option<EnvironmentLighting>,
    ) -> Result<Arc<IndirectScene>> {
        let lut = BrdfLut::prepare(32, 1024)?;
        let resources = Arc::new(GpuIndirectResources::prepare(
            &self.gpu,
            environments,
            &lightmaps(),
            &lut,
        )?);
        Ok(Arc::new(IndirectScene::new(resources, global, vec![])?))
    }
}

fn target(gpu: &GpuContext) -> OutputTarget {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lightmap output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    OutputTarget {
        view: texture.create_view(&Default::default()),
        format: texture.format(),
        size: SIZE,
        viewport: Viewport::full(SIZE),
    }
}

/// Quadrant colours of lightmap 0 (top-left, top-right, bottom-left, bottom-right).
const QUADRANTS: [[f32; 3]; 4] = [
    [2.0, 0.5, 0.25],
    [0.25, 3.0, 0.5],
    [0.5, 0.25, 1.5],
    [1.25, 1.0, 0.125],
];
/// The four texels of lightmap 1, row-major from the top-left.
const ORIENTATION: [[f32; 3]; 4] = [
    [4.0, 0.0, 0.0],
    [0.0, 4.0, 0.0],
    [0.0, 0.0, 4.0],
    [2.0, 2.0, 0.0],
];
const UNIFORM: [f32; 3] = [1.0, 0.5, 0.25];

/// 0: a 4x4 atlas of four 2x2 colour quadrants; 1: a 2x2 image with four
/// distinct texels; 2: a uniform 1x1 image.
fn lightmaps() -> Vec<RadianceImage> {
    let atlas = (0..16)
        .map(|i| {
            let (x, y) = (i % 4, i / 4);
            QUADRANTS[(y / 2) * 2 + x / 2]
        })
        .collect();
    vec![
        RadianceImage::new([4, 4], atlas).unwrap(),
        RadianceImage::new([2, 2], ORIENTATION.to_vec()).unwrap(),
        RadianceImage::new([1, 1], vec![UNIFORM]).unwrap(),
    ]
}

fn binding(index: usize, scale: [f32; 2], offset: [f32; 2], mode: LightmapMode) -> LightmapBinding {
    LightmapBinding::new(index, scale, offset, mode).unwrap()
}

/// The pixel under world point (x, y) of the orthographic harness camera.
fn at(image: &HdrImage, x: f32, y: f32) -> [f32; 4] {
    image.pixel(
        ((x + 2.0) * 0.25 * image.size[0] as f32) as u32,
        ((2.0 - y) * 0.25 * image.size[1] as f32) as u32,
    )
}

fn close(actual: [f32; 4], expected: [f32; 3], what: &str) {
    for c in 0..3 {
        let tolerance = 0.004 + expected[c] * 0.004;
        assert!(
            (actual[c] - expected[c]).abs() <= tolerance,
            "{what}: {actual:?} vs {expected:?} (channel {c}, tolerance {tolerance})"
        );
    }
}

fn scaled(color: [f32; 3], by: f32) -> [f32; 3] {
    color.map(|v| v * by)
}

fn plus(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn constant_environment(diffuse: [f32; 3]) -> EnvironmentData {
    let mut coefficients = [[0.0; 3]; 9];
    coefficients[0] = diffuse;
    EnvironmentData::new(
        DiffusePolynomial::new(coefficients).unwrap(),
        (0..4)
            .map(|mip| {
                std::array::from_fn(|_| {
                    let edge = 8u32 >> mip;
                    RadianceImage::new([edge; 2], vec![[0.0; 3]; (edge * edge) as usize]).unwrap()
                })
            })
            .collect(),
    )
    .unwrap()
}

#[test]
#[ignore = "requires a real GPU; lightmapped lit pixels read back from the scene HDR target"]
fn instances_sharing_one_mesh_sample_different_atlas_regions_and_texel_orientation() -> Result<()> {
    let mut h = Harness::new()?;
    h.pipeline
        .set_indirect(ViewId::MAIN, Some(h.scene(&[], None)?))?;
    let replace = LightmapMode::Replace;
    let regions = [
        ([0.5, 0.5], [0.0, 0.0]),
        ([0.5, 0.5], [0.5, 0.0]),
        ([0.5, 0.5], [0.0, 0.5]),
        ([0.5, 0.5], [0.5, 0.5]),
    ];
    let xs = [-1.5, -0.5, 0.5, 1.5];
    let ids = ["q0", "q1", "q2", "q3"];
    let mut meshes: Vec<_> = (0..4)
        .map(|i| {
            h.mesh(
                ids[i],
                xs[i],
                Some(binding(0, regions[i].0, regions[i].1, replace)),
            )
        })
        .collect();
    // A different lightmap on the same shared geometry: the full 2x2 image.
    let mut orientation = h.mesh(
        "orientation",
        0.0,
        Some(binding(1, [1.0; 2], [0.0; 2], replace)),
    );
    // Scaled to half 0.4375 (7 px) and centred on a pixel edge, so the four
    // quarter points below fall exactly on pixel centres and texel centres.
    orientation.world_from_local = Mat4::from_translation(Vec3::new(0.0, 1.25, 0.0))
        * Mat4::from_scale(Vec3::new(1.25, 1.25, 1.0));
    meshes.push(orientation);
    let image = h.draw(&meshes, Vec3::new(0.1, 0.2, 0.3))?;
    for i in 0..4 {
        close(
            at(&image, xs[i], 0.0),
            scaled(QUADRANTS[i], WEIGHT),
            &format!("instance {} atlas region", ids[i]),
        );
    }
    // Quarter points of the quad hit the centres of the four texels, so this
    // also fixes the UV2 orientation: v grows downwards (top row first).
    let q = 0.4375 * 0.5;
    for (dx, dy, texel) in [(-q, q, 0), (q, q, 1), (-q, -q, 2), (q, -q, 3)] {
        close(
            at(&image, dx, 1.25 + dy),
            scaled(ORIENTATION[texel], WEIGHT),
            &format!("orientation texel {texel}"),
        );
    }
    // Outside every quad the clear colour is untouched.
    close(at(&image, 1.9, -1.9), [0.0; 3], "background");
    h.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU; replace and add modes against ambient and probe diffuse"]
fn replace_and_add_modes_differ_by_exactly_the_probe_or_ambient_diffuse() -> Result<()> {
    let mut h = Harness::new()?;
    let ambient = Vec3::new(0.1, 0.2, 0.3);
    let baked = scaled(UNIFORM, WEIGHT);
    let frame = |h: &mut Harness| -> Result<HdrImage> {
        let full = [1.0; 2];
        let meshes = [
            h.mesh("none", -1.2, None),
            h.mesh(
                "replace",
                0.0,
                Some(binding(2, full, [0.0; 2], LightmapMode::Replace)),
            ),
            h.mesh(
                "add",
                1.2,
                Some(binding(2, full, [0.0; 2], LightmapMode::Add)),
            ),
        ];
        h.draw(&meshes, ambient)
    };

    // Constant ambient (no environment in the set): the legacy diffuse term.
    h.pipeline
        .set_indirect(ViewId::MAIN, Some(h.scene(&[], None)?))?;
    let image = frame(&mut h)?;
    close(at(&image, -1.2, 0.0), ambient.to_array(), "unbound ambient");
    close(at(&image, 0.0, 0.0), baked, "replace drops the ambient");
    close(
        at(&image, 1.2, 0.0),
        plus(ambient.to_array(), baked),
        "add keeps the ambient",
    );

    // A global environment: its diffuse probe term (weighted by 1 - F0).
    let probe = scaled([4.0, 2.0, 0.25], WEIGHT);
    let environment = constant_environment([4.0, 2.0, 0.25]);
    let lighting = EnvironmentLighting::new(0, Quat::IDENTITY, 1.0)?;
    h.pipeline
        .set_indirect(ViewId::MAIN, Some(h.scene(&[environment], Some(lighting))?))?;
    let image = frame(&mut h)?;
    close(at(&image, -1.2, 0.0), probe, "unbound probe diffuse");
    close(
        at(&image, 0.0, 0.0),
        baked,
        "replace drops the probe diffuse",
    );
    close(
        at(&image, 1.2, 0.0),
        plus(probe, baked),
        "add keeps the probe diffuse",
    );

    // Specular IBL is untouched: a fully metallic surface has no diffuse term,
    // so a bound lightmap cannot change a single pixel of it.
    let metal = |h: &Harness, lightmap| {
        let mut mesh = h.mesh("metal", 0.0, lightmap);
        mesh.params.metallic = 1.0;
        mesh.params.roughness = 0.4;
        mesh
    };
    let unbound = h.draw(&[metal(&h, None)], ambient)?;
    let bound_mesh = metal(&h, Some(binding(2, [1.0; 2], [0.0; 2], LightmapMode::Add)));
    let bound = h.draw(&[bound_mesh], ambient)?;
    assert_eq!(
        unbound.pixels, bound.pixels,
        "metallic specular is unchanged"
    );
    h.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU; unbound and UV2-less draws are unchanged and bad bindings are rejected"]
fn meshes_without_bindings_or_uv2_render_exactly_as_before_and_invalid_bindings_fail() -> Result<()>
{
    let mut h = Harness::new()?;
    let ambient = Vec3::new(0.1, 0.2, 0.3);
    h.pipeline
        .set_indirect(ViewId::MAIN, Some(h.scene(&[], None)?))?;
    let mut plain = h.mesh("plain", -0.6, None);
    plain.mesh = h.plain.clone();
    let with_uv2 = h.mesh("uv2", 0.6, None);
    let image = h.draw(&[plain.clone(), with_uv2.clone()], ambient)?;
    close(at(&image, -0.6, 0.0), ambient.to_array(), "plain mesh");
    close(
        at(&image, 0.6, 0.0),
        ambient.to_array(),
        "UV2 mesh without binding",
    );
    // A mesh carrying UV2 but no binding is the same draw as one without UV2.
    assert_eq!(at(&image, -0.6, 0.0), at(&image, 0.6, 0.0));
    // Without any indirect scene the same meshes still render the legacy ambient.
    h.pipeline.set_indirect(ViewId::MAIN, None)?;
    let legacy = h.draw(&[plain.clone(), with_uv2.clone()], ambient)?;
    assert_eq!(at(&legacy, -0.6, 0.0), at(&image, -0.6, 0.0));
    assert_eq!(at(&legacy, 0.6, 0.0), at(&image, 0.6, 0.0));
    h.pipeline
        .set_indirect(ViewId::MAIN, Some(h.scene(&[], None)?))?;

    let region = binding(0, [1.0; 2], [0.0; 2], LightmapMode::Replace);
    let message = |h: &mut Harness, mesh: SceneMesh<'_>| -> String {
        format!("{:#}", h.draw(&[mesh], ambient).unwrap_err())
    };
    let mut no_uv2 = plain.clone();
    no_uv2.lightmap = Some(region);
    assert!(
        message(&mut h, no_uv2).contains("second UV set"),
        "UV2 is required"
    );
    let mut custom = h.mesh("custom", 0.0, Some(region));
    custom.shader = Some(LIT_WGSL);
    assert!(
        message(&mut h, custom).contains("built-in lit shader"),
        "custom shaders are not lightmapped"
    );
    let missing = h.mesh(
        "missing",
        0.0,
        Some(binding(3, [1.0; 2], [0.0; 2], LightmapMode::Add)),
    );
    assert!(message(&mut h, missing).contains("lightmap 3 is missing"));
    h.pipeline.set_indirect(ViewId::MAIN, None)?;
    let orphan = h.mesh("orphan", 0.0, Some(region));
    assert!(message(&mut h, orphan).contains("needs an indirect scene"));
    // Rejected frames do not poison later ones.
    let recovered = h.draw(&[plain], ambient)?;
    close(
        at(&recovered, -0.6, 0.0),
        ambient.to_array(),
        "after rejected frames",
    );
    h.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU; Hi-Z compacted camera draws keep each instance's atlas region"]
fn hiz_compacted_draws_keep_lightmap_regions_of_the_surviving_instances() -> Result<()> {
    let mut h = Harness::new()?;
    h.pipeline
        .set_indirect(ViewId::MAIN, Some(h.scene(&[], None)?))?;
    let wall_mesh = GpuMesh::from_engine_vertices(
        &h.gpu.device,
        "wall",
        &quad_vertices(0.6),
        &[0, 1, 2, 0, 2, 3],
    )?;
    // `a_hidden` sorts first and sits behind the wall, so compaction shifts the
    // two survivors down by one record; their regions must follow them.
    let build = |h: &Harness| -> Vec<SceneMesh<'static>> {
        let at = |x: f32, z: f32| Mat4::from_translation(Vec3::new(x, 0.0, z));
        let mut wall = h.mesh("wall", 0.0, None);
        wall.mesh = wall_mesh.clone();
        wall.world_from_local = at(0.0, -2.0);
        wall.params.tint = [0.0, 0.0, 0.0, 1.0];
        let instance = |id: &'static str, x: f32, z: f32, offset: [f32; 2]| {
            let mut mesh = h.mesh(
                id,
                0.0,
                Some(binding(0, [0.5, 0.5], offset, LightmapMode::Replace)),
            );
            mesh.world_from_local = at(x, z);
            mesh
        };
        vec![
            wall,
            instance("a_hidden", 0.0, -5.0, [0.5, 0.5]),
            instance("b_left", -2.2, -5.0, [0.0, 0.5]),
            instance("c_right", 2.2, -5.0, [0.5, 0.0]),
        ]
    };
    let meta = |bounds: f32, occluder| MeshVisibility {
        bounds: Some(([-bounds, -bounds, 0.0], [bounds, bounds, 0.0])),
        occluder,
        shadow_mesh: None,
    };
    let visibility = vec![
        meta(0.6, true),
        meta(0.35, false),
        meta(0.35, false),
        meta(0.35, false),
    ];
    let ambient = Vec3::new(0.1, 0.2, 0.3);
    let camera = |hiz: bool| -> ViewData {
        let projection = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            1.0,
            0.1,
            100.0,
        );
        let mut view = ViewData::new(ViewId::MAIN, Mat4::IDENTITY, projection, SIZE, SIZE).unwrap();
        view.ambient = ambient;
        if hiz {
            view.features =
                FeatureOverrides(BTreeMap::from([(names::HIZ_VISIBILITY.to_owned(), true)]));
        }
        view
    };
    let render = |h: &mut Harness, hiz: bool| -> Result<(ViewReport, HdrImage)> {
        let meshes = build(h);
        let view = camera(hiz);
        let output = target(&h.gpu);
        let report = h.pipeline.render_view_with_visibility(
            &SceneFrame {
                view: &view,
                meshes: &meshes,
                lights: &LightList::default(),
                world: None,
            },
            &output,
            &mut NoHost,
            Some(&visibility),
        )?;
        h.gpu.check_errors()?;
        Ok((report, h.pipeline.read_scene_hdr(ViewId::MAIN)?))
    };
    let (direct_report, direct) = render(&mut h, false)?;
    assert!(!direct_report.visibility.requested);
    let (report, culled) = render(&mut h, true)?;
    assert!(report.visibility.active, "{:?}", report.visibility);
    assert_eq!(report.visibility.fallback, None);
    let diagnostic = h
        .pipeline
        .visibility_diagnostic(ViewId::MAIN)?
        .expect("compacted draws");
    let batches: Vec<_> = diagnostic
        .batches
        .iter()
        .map(|b| (b.source.end - b.source.start, b.survivors))
        .collect();
    eprintln!("batches (candidates, survivors): {batches:?}");
    assert!(
        batches.contains(&(3, 2)),
        "the lightmapped batch of three candidates keeps two: {batches:?}"
    );
    // Project the side objects: ndc = x / (depth * tan(fov / 2)).
    let half_width = 5.0 * (std::f32::consts::FRAC_PI_3 / 2.0).tan();
    let column = |x: f32| ((x / half_width + 1.0) * 0.5 * SIZE[0] as f32) as u32;
    let row = SIZE[1] / 2;
    close(
        culled.pixel(column(-2.2), row),
        scaled(QUADRANTS[2], WEIGHT),
        "compacted b_left region",
    );
    close(
        culled.pixel(column(2.2), row),
        scaled(QUADRANTS[1], WEIGHT),
        "compacted c_right region",
    );
    assert_eq!(culled.pixels, direct.pixels, "culling changed no pixel");
    // The black wall hides a_hidden (bottom-right quadrant colour).
    close(
        culled.pixel(SIZE[0] / 2, row),
        [0.0; 3],
        "the wall hides the third instance",
    );
    Ok(())
}
