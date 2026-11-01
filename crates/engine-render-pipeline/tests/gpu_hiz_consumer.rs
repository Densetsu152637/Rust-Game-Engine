//! Real-adapter tests of the Hi-Z camera visibility consumer inside
//! `ScenePipeline`: certified occluder depth, pyramid queries, GPU-compacted
//! indirect opaque draws, direct fallbacks and shadow independence.
//!
//! Run with `cargo test -p engine-render-pipeline --test gpu_hiz_consumer --
//! --ignored --test-threads=1 --nocapture`. The survivor counts are read back
//! from the indirect commands the GPU wrote, and a depth-disabled replay of
//! those exact commands shows which instances survive.

use anyhow::Result;
use engine_render_pipeline::visibility::{MeshVisibility, VisibilityDiagnostic};
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Vec3};
use std::collections::BTreeMap;
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

const SIZE: [u32; 2] = [64, 64];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("Hi-Z consumer adapter: {}", gpu.adapter.get_info().name);
    Ok(gpu)
}

fn feature(name: &str, enabled: bool) -> FeatureOverrides {
    FeatureOverrides(BTreeMap::from([(name.to_owned(), enabled)]))
}

fn camera(size: [u32; 2], world_from_view: Mat4, hiz: bool) -> ViewData {
    let projection = glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_3,
        size[0] as f32 / size[1] as f32,
        0.1,
        100.0,
    );
    let mut view = ViewData::new(ViewId::MAIN, world_from_view, projection, size, size).unwrap();
    if hiz {
        view.features = feature(names::HIZ_VISIBILITY, true);
    }
    view
}

/// A square in the XY plane facing +Z.
fn quad(gpu: &GpuContext, half: f32) -> Arc<GpuMesh> {
    let vertex = |x, y| EngineVertex {
        position: [x, y, 0.0],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: [0.0, 0.0, 1.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "hiz test quad",
        &[
            vertex(-half, -half),
            vertex(half, -half),
            vertex(half, half),
            vertex(-half, half),
        ],
        &[0, 1, 2, 0, 2, 3],
    )
    .unwrap()
}

fn unlit<'a>(
    id: &'a str,
    mesh: &Arc<GpuMesh>,
    position: Vec3,
    tint: [f32; 4],
    shader: Option<&'a str>,
) -> SceneMesh<'a> {
    SceneMesh {
        id,
        mesh: mesh.clone(),
        world_from_local: Mat4::from_translation(position),
        shader,
        params: MaterialParams {
            tint,
            unlit: true,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
        lightmap: None,
    }
}

fn flat_bounds(half: f32) -> Option<([f32; 3], [f32; 3])> {
    Some(([-half, -half, 0.0], [half, half, 0.0]))
}

fn meta(half: f32, occluder: bool) -> MeshVisibility {
    MeshVisibility {
        bounds: flat_bounds(half),
        occluder,
        shadow_mesh: None,
    }
}

struct Screen {
    texture: wgpu::Texture,
    target: OutputTarget,
    size: [u32; 2],
}

fn screen(gpu: &GpuContext, size: [u32; 2]) -> Screen {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hiz consumer target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
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
        size,
        viewport: Viewport::full(size),
    };
    Screen {
        texture,
        target,
        size,
    }
}

struct Rig {
    gpu: GpuContext,
    pipeline: ScenePipeline,
}

impl Rig {
    fn new() -> Result<Self> {
        let gpu = gpu()?;
        let pipeline = ScenePipeline::new(gpu.clone())?;
        Ok(Self { gpu, pipeline })
    }

    fn render(
        &mut self,
        view: &ViewData,
        meshes: &[SceneMesh<'_>],
        visibility: Option<&Vec<MeshVisibility>>,
        lights: &LightList,
    ) -> Result<(ViewReport, Vec<u8>)> {
        let output = screen(&self.gpu, view.output_size);
        let report = self.pipeline.render_view_with_visibility(
            &SceneFrame {
                view,
                meshes,
                lights,
                world: None,
            },
            &output.target,
            &mut NoHost,
            visibility.map(Vec::as_slice),
        )?;
        self.gpu.check_errors()?;
        let pixels = engine_render_wgpu::read_texture_rgba(
            &self.gpu,
            &output.texture,
            output.size[0],
            output.size[1],
        )?;
        Ok((report, pixels))
    }

    fn diagnostic(&self) -> Result<Option<VisibilityDiagnostic>> {
        self.pipeline.visibility_diagnostic(ViewId::MAIN)
    }
}

fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

/// A wall (marked occluder) in front of a small object it fully hides, and
/// a second small object of the same mesh and material beside the wall.
struct Scene {
    wall: Arc<GpuMesh>,
    small: Arc<GpuMesh>,
}

impl Scene {
    fn new(gpu: &GpuContext) -> Self {
        Self {
            wall: quad(gpu, 0.6),
            small: quad(gpu, 0.3),
        }
    }

    fn meshes(&self, wall_x: f32) -> Vec<SceneMesh<'static>> {
        vec![
            unlit("wall", &self.wall, Vec3::new(wall_x, 0.0, -2.0), RED, None),
            unlit(
                "hidden",
                &self.small,
                Vec3::new(0.0, 0.0, -5.0),
                GREEN,
                None,
            ),
            unlit("side", &self.small, Vec3::new(2.2, 0.0, -5.0), GREEN, None),
        ]
    }

    fn metadata() -> Vec<MeshVisibility> {
        vec![meta(0.6, true), meta(0.3, false), meta(0.3, false)]
    }
}

fn tags(diagnostic: &VisibilityDiagnostic) -> [usize; 3] {
    ["wall", "hidden", "side"].map(|id| diagnostic.pixels_with_tag(id_hash(id)))
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn gpu_skips_the_occluded_instance_and_matches_the_direct_image() -> Result<()> {
    let mut rig = Rig::new()?;
    let scene = Scene::new(&rig.gpu);
    let meshes = scene.meshes(0.0);
    let metadata = Scene::metadata();
    let lights = LightList::default();

    let off = camera(SIZE, Mat4::IDENTITY, false);
    let (direct_report, direct) = rig.render(&off, &meshes, Some(&metadata), &lights)?;
    assert!(!direct_report.visibility.requested, "feature off");
    assert!(rig.diagnostic()?.is_none(), "no indirect draws when off");

    let on = camera(SIZE, Mat4::IDENTITY, true);
    let (report, culled) = rig.render(&on, &meshes, Some(&metadata), &lights)?;
    let visibility = &report.visibility;
    eprintln!("visibility report: {visibility:?}");
    assert!(visibility.requested && visibility.active, "{visibility:?}");
    assert_eq!(visibility.fallback, None);
    assert_eq!(visibility.occluders, 1);
    assert_eq!(visibility.candidates, 3);
    assert_eq!(
        visibility.indirect_batches, 2,
        "wall batch + shared small batch"
    );

    // Real GPU readback of the indirect commands written by the compaction pass.
    let diagnostic = rig.diagnostic()?.expect("compacted camera draws");
    eprintln!("indirect batches: {:?}", diagnostic.batches);
    let mut counts: Vec<_> = diagnostic
        .batches
        .iter()
        .map(|b| (b.source.end - b.source.start, b.survivors))
        .collect();
    counts.sort_unstable();
    assert_eq!(
        counts,
        vec![(1, 1), (2, 1)],
        "the two-candidate batch keeps exactly one instance"
    );
    assert_eq!(diagnostic.survivors(), 2);

    // Depth-disabled replay of those exact commands: only culling can
    // remove pixels.
    let [wall, hidden, side] = tags(&diagnostic);
    eprintln!("diagnostic pixels: wall {wall}, hidden {hidden}, side {side}");
    assert!(wall > 100, "the wall survives: {wall}");
    assert!(side > 20, "the unoccluded sibling survives: {side}");
    assert_eq!(hidden, 0, "the occluded instance is absent");
    // Order-independent: only the wall covers the centre of the replay. A
    // surviving hidden instance would add a second fragment there.
    assert_eq!(
        diagnostic.coverage(32, 32),
        1,
        "the occluded instance is not replayed"
    );
    assert_eq!(
        diagnostic.coverage(56, 32),
        1,
        "the side object covers its own pixel"
    );

    // The skipped draw changed nothing visible.
    assert_eq!(culled, direct, "the culled frame equals the direct frame");
    assert_eq!(
        pixel(&culled, 64, 32, 32),
        [255, 0, 0, 255],
        "the wall covers the centre"
    );
    assert_eq!(
        pixel(&culled, 64, 56, 32)[1],
        255,
        "the side object is drawn"
    );
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn occluder_movement_makes_the_hidden_instance_reappear_and_hide_again() -> Result<()> {
    let mut rig = Rig::new()?;
    let scene = Scene::new(&rig.gpu);
    let metadata = Scene::metadata();
    let view = camera(SIZE, Mat4::IDENTITY, true);
    let lights = LightList::default();
    let mut sequence = Vec::new();
    for wall_x in [0.0, -3.0, 0.0] {
        let meshes = scene.meshes(wall_x);
        let (report, pixels) = rig.render(&view, &meshes, Some(&metadata), &lights)?;
        assert!(report.visibility.active, "{:?}", report.visibility);
        let diagnostic = rig.diagnostic()?.expect("indirect draws");
        let [_, hidden, _] = tags(&diagnostic);
        eprintln!(
            "wall at x={wall_x}: survivors {}, hidden pixels {hidden}, centre {:?}",
            diagnostic.survivors(),
            pixel(&pixels, 64, 32, 32)
        );
        sequence.push((
            diagnostic.survivors(),
            hidden,
            pixel(&pixels, 64, 32, 32),
            diagnostic.coverage(32, 32),
        ));
    }
    assert_eq!(sequence[0].0, 2, "hidden behind the wall");
    assert_eq!(sequence[0].1, 0);
    assert_eq!(sequence[0].2, [255, 0, 0, 255]);
    assert_eq!(sequence[0].3, 1, "only the wall is replayed at the centre");
    assert_eq!(sequence[1].0, 3, "the wall moved away");
    assert!(sequence[1].1 > 20, "the hidden instance reappears");
    assert_eq!(sequence[1].2[1], 255, "and is visible in the image");
    assert_eq!(sequence[2].0, 2, "the wall returned");
    assert_eq!(sequence[2].1, 0, "so the instance hides again");
    assert_eq!(sequence[2].2, [255, 0, 0, 255]);
    assert_eq!(sequence[2].3, 1);
    assert_eq!(
        sequence[1].3, 1,
        "the moved wall no longer overlaps the centre"
    );
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn resize_jitter_and_independent_views_keep_culling_correct() -> Result<()> {
    let mut rig = Rig::new()?;
    let scene = Scene::new(&rig.gpu);
    let meshes = scene.meshes(0.0);
    let metadata = Scene::metadata();
    let lights = LightList::default();
    for size in [[64, 64], [96, 48], [128, 128], [40, 40]] {
        let view = camera(size, Mat4::IDENTITY, true);
        let (report, _) = rig.render(&view, &meshes, Some(&metadata), &lights)?;
        let diagnostic = rig.diagnostic()?.expect("indirect draws after resize");
        let [wall, hidden, side] = tags(&diagnostic);
        eprintln!(
            "size {size:?}: survivors {}, tags {wall}/{hidden}/{side}",
            diagnostic.survivors()
        );
        assert_eq!(diagnostic.size, size, "diagnostic follows the render size");
        assert!(report.visibility.active);
        assert_eq!(diagnostic.survivors(), 2, "{size:?}");
        assert_eq!(hidden, 0, "{size:?}");
        assert!(wall > 0 && side > 0, "{size:?}: {wall} {side}");
    }
    // Sub-pixel jitter moves rasterization; the padded query stays conservative.
    for jitter in [[0.5, -0.5], [-0.4, 0.45]] {
        let mut view = camera(SIZE, Mat4::IDENTITY, true);
        view.jitter = jitter;
        rig.render(&view, &meshes, Some(&metadata), &lights)?;
        let diagnostic = rig.diagnostic()?.expect("indirect draws");
        let [wall, hidden, side] = tags(&diagnostic);
        assert_eq!(hidden, 0, "jitter {jitter:?}");
        assert!(wall > 0 && side > 0, "jitter {jitter:?}");
    }
    // A second view of the same pipeline has its own pyramid: from x=1 the
    // wall no longer hides the object completely, so it survives there only.
    let main = camera(SIZE, Mat4::IDENTITY, true);
    let mut shifted = camera(SIZE, Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0)), true);
    shifted.id = ViewId(9);
    rig.render(&main, &meshes, Some(&metadata), &lights)?;
    let main_survivors = rig.diagnostic()?.expect("main").survivors();
    let output = screen(&rig.gpu, SIZE);
    rig.pipeline.render_view_with_visibility(
        &SceneFrame {
            view: &shifted,
            meshes: &meshes,
            lights: &lights,
            world: None,
        },
        &output.target,
        &mut NoHost,
        Some(&metadata[..]),
    )?;
    let other = rig
        .pipeline
        .visibility_diagnostic(ViewId(9))?
        .expect("second view");
    let main_after = rig.diagnostic()?.expect("main stays valid");
    assert_eq!(main_survivors, 2);
    assert_eq!(main_after.survivors(), 2, "the first view is unchanged");
    assert_eq!(
        other.survivors(),
        3,
        "the shifted view still sees the object"
    );
    assert!(other.pixels_with_tag(id_hash("hidden")) > 0);
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn unsupported_inputs_take_the_visible_direct_path_with_a_reason() -> Result<()> {
    let mut rig = Rig::new()?;
    let scene = Scene::new(&rig.gpu);
    let meshes = scene.meshes(0.0);
    let metadata = Scene::metadata();
    let lights = LightList::default();
    let off = camera(SIZE, Mat4::IDENTITY, false);
    let (_, baseline) = rig.render(&off, &meshes, None, &lights)?;
    let on = camera(SIZE, Mat4::IDENTITY, true);

    // No metadata: the legacy path even though the feature is requested.
    let (report, pixels) = rig.render(&on, &meshes, None, &lights)?;
    assert!(!report.visibility.requested && !report.visibility.active);
    assert_eq!(pixels, baseline);

    // Nothing is certified: the wall is not marked.
    let unmarked = vec![meta(0.6, false), meta(0.3, false), meta(0.3, false)];
    let (report, pixels) = rig.render(&on, &meshes, Some(&unmarked), &lights)?;
    assert!(report.visibility.requested && !report.visibility.active);
    assert!(
        report
            .visibility
            .fallback
            .as_deref()
            .is_some_and(|r| r.contains("occluder"))
    );
    assert!(rig.diagnostic()?.is_none());
    assert_eq!(pixels, baseline);

    // A custom material may displace or discard, so it is never certified...
    let mut custom = scene.meshes(0.0);
    custom[0].shader = Some(LIT_WGSL);
    let (report, pixels) = rig.render(&on, &custom, Some(&metadata), &lights)?;
    assert_eq!(report.visibility.occluders, 0, "{:?}", report.visibility);
    assert!(!report.visibility.active);
    assert_eq!(pixels, baseline, "the lit shader is the same image");

    // ...and a custom-material candidate is never culled either.
    let mut candidate = scene.meshes(0.0);
    candidate[1].shader = Some(LIT_WGSL);
    let (report, _) = rig.render(&on, &candidate, Some(&metadata), &lights)?;
    assert!(report.visibility.active, "{:?}", report.visibility);
    let diagnostic = rig.diagnostic()?.expect("indirect draws");
    assert_eq!(
        diagnostic.survivors(),
        3,
        "the custom-material instance stays"
    );
    assert_eq!(
        diagnostic.coverage(32, 32),
        2,
        "the wall and the kept instance overlap at the centre"
    );

    // A transparent wall is never an occluder either.
    let mut glass = scene.meshes(0.0);
    glass[0].params.transparent = true;
    let (report, _) = rig.render(&on, &glass, Some(&metadata), &lights)?;
    assert_eq!(report.visibility.occluders, 0);

    // A bound that is missing keeps the instance visible.
    let mut unbounded = Scene::metadata();
    unbounded[1].bounds = None;
    let (report, pixels) = rig.render(&on, &meshes, Some(&unbounded), &lights)?;
    assert!(report.visibility.active);
    assert_eq!(rig.diagnostic()?.expect("draws").survivors(), 3);
    assert_eq!(pixels, baseline);

    // Turning the certified depth stage off leaves no trusted depth.
    let mut no_depth = camera(SIZE, Mat4::IDENTITY, true);
    no_depth
        .features
        .0
        .insert(names::HIZ_DEPTH.to_owned(), false);
    let (report, pixels) = rig.render(&no_depth, &meshes, Some(&metadata), &lights)?;
    assert!(!report.visibility.active);
    assert!(
        report
            .visibility
            .fallback
            .as_deref()
            .is_some_and(|r| r.contains("depth"))
    );
    assert_eq!(pixels, baseline);
    Ok(())
}

// ----------------------------------------------------------------- shadows

const VISIBILITY_WGSL: &str = "
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
    let light = engine_light(0u);
    return vec4<f32>(engine_light_visibility(light, input.world, n), 0.0, 0.0, 1.0);
}
";

/// A horizontal square (`half` metres) facing +Y.
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

fn top_view(extent: f32, height: f32) -> ViewData {
    let size = [128, 128];
    let projection = glam::camera::rh::proj::directx::orthographic(
        -extent * 0.5,
        extent * 0.5,
        -extent * 0.5,
        extent * 0.5,
        0.1,
        height * 2.0,
    );
    let world_from_view =
        Mat4::from_translation(Vec3::Y * height) * Mat4::from_rotation_x(-FRAC_PI_2);
    let mut view = ViewData::new(ViewId::MAIN, world_from_view, projection, size, size).unwrap();
    view.features = feature(names::HIZ_VISIBILITY, true);
    view
}

fn pixel_of(view: &ViewData, point: Vec3) -> (u32, u32) {
    let clip = view.clip_from_world() * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x + 1.0) * 0.5 * view.render_size[0] as f32) as u32,
        ((1.0 - ndc.y) * 0.5 * view.render_size[1] as f32) as u32,
    )
}

fn draw<'a>(
    id: &'a str,
    mesh: &Arc<GpuMesh>,
    position: Vec3,
    shader: Option<&'a str>,
) -> SceneMesh<'a> {
    SceneMesh {
        id,
        mesh: mesh.clone(),
        world_from_local: Mat4::from_translation(position),
        shader,
        params: MaterialParams {
            roughness: 0.6,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
        lightmap: None,
    }
}

fn flat_meta(half: f32, occluder: bool) -> MeshVisibility {
    MeshVisibility {
        bounds: Some(([-half, 0.0, -half], [half, 0.0, half])),
        occluder,
        shadow_mesh: None,
    }
}

fn lights() -> (LightList, Vec3) {
    let toward = Vec3::new(2.0, 1.0, 0.0).normalize();
    let shadow = ShadowSettings {
        resolution: 1024,
        filter: ShadowFilter::Pcf { radius: 1 },
        cascades: CascadeSettings {
            count: 1,
            max_distance: 30.0,
            ..Default::default()
        },
        ..Default::default()
    };
    (
        LightList {
            lights: vec![Light::directional(-toward, Vec3::ONE, 3.0).with_shadow(shadow)],
        },
        toward,
    )
}

fn shadow_visibility(
    rig: &mut Rig,
    view: &ViewData,
    meshes: &[SceneMesh<'_>],
    metadata: &[MeshVisibility],
    lights: &LightList,
    point: Vec3,
) -> Result<(f32, ViewReport)> {
    let output = screen(&rig.gpu, view.output_size);
    let report = rig.pipeline.render_view_with_visibility(
        &SceneFrame {
            view,
            meshes,
            lights,
            world: None,
        },
        &output.target,
        &mut NoHost,
        Some(metadata),
    )?;
    let image = rig.pipeline.read_scene_hdr(view.id)?;
    let (x, y) = pixel_of(view, point);
    Ok((image.pixel(x, y)[0], report))
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn shadows_use_their_own_caster_plan_independent_of_camera_visibility() -> Result<()> {
    let mut rig = Rig::new()?;
    let (lights, toward) = lights();
    let floor = floor_mesh(&rig.gpu, 50.0);
    let roof = floor_mesh(&rig.gpu, 1.0);
    let tiny = floor_mesh(&rig.gpu, 0.05);
    let plate = floor_mesh(&rig.gpu, 0.5);
    let caster = |position: Vec3| position - toward * (position.y / toward.y);

    // A. The caster sits under an occluding roof: the top camera never sees
    // it and the GPU drops its camera draw, yet its shadow lands on the
    // visible floor. Its camera mesh is a coarse stand-in; the shadow uses
    // the level-zero geometry.
    let view = top_view(12.0, 10.0);
    let under = Vec3::new(0.0, 1.0, 0.0);
    let shadow_point = caster(under);
    let lit_point = shadow_point + Vec3::new(0.0, 0.0, 3.0);
    let meshes = [
        draw("floor", &floor, Vec3::ZERO, Some(VISIBILITY_WGSL)),
        draw("roof", &roof, Vec3::new(0.0, 3.0, 0.0), None),
        draw("caster", &tiny, under, None),
    ];
    let metadata = [
        flat_meta(50.0, false),
        flat_meta(1.0, true),
        MeshVisibility {
            bounds: Some(([-0.5, 0.0, -0.5], [0.5, 0.0, 0.5])),
            occluder: false,
            shadow_mesh: Some(plate.clone()),
        },
    ];
    let (shadowed, report) =
        shadow_visibility(&mut rig, &view, &meshes, &metadata, &lights, shadow_point)?;
    let (lit, _) = shadow_visibility(&mut rig, &view, &meshes, &metadata, &lights, lit_point)?;
    let diagnostic = rig.diagnostic()?.expect("indirect camera draws");
    let caster_pixels = diagnostic.pixels_with_tag(id_hash("caster"));
    let roof_pixels = diagnostic.pixels_with_tag(id_hash("roof"));
    eprintln!(
        "A: active {}, survivors {}, roof px {roof_pixels}, caster px {caster_pixels}, shadow {shadowed:.3}, lit {lit:.3}",
        report.visibility.active,
        diagnostic.survivors()
    );
    assert!(report.visibility.active, "{:?}", report.visibility);
    assert_eq!(caster_pixels, 0, "the camera culled the caster");
    assert!(roof_pixels > 0);
    assert!(
        shadowed < 0.1,
        "the culled caster still shadows the floor: {shadowed}"
    );
    assert!(lit > 0.99, "outside the shadow: {lit}");

    // Control: without the caster there is no shadow at that point.
    let without = [meshes[0].clone(), meshes[1].clone()];
    let (open, _) = shadow_visibility(
        &mut rig,
        &view,
        &without,
        &metadata[..2],
        &lights,
        shadow_point,
    )?;
    assert!(open > 0.99, "no caster, no shadow: {open}");

    // B. A caster entirely outside the camera frustum still shadows the view.
    let view = top_view(6.0, 10.0);
    let far = Vec3::new(4.0, 2.0, 0.0);
    let (x, _) = pixel_of(&view, far);
    assert!(
        x >= view.render_size[0],
        "the caster is off-camera (pixel x {x})"
    );
    let shadow_point = caster(far);
    let corner_roof = Vec3::new(-2.0, 3.0, -2.0);
    let meshes = [
        draw("floor", &floor, Vec3::ZERO, Some(VISIBILITY_WGSL)),
        draw("roof", &roof, corner_roof, None),
        draw("caster", &plate, far, None),
    ];
    let metadata = [
        flat_meta(50.0, false),
        flat_meta(1.0, true),
        flat_meta(0.5, false),
    ];
    let (shadowed, report) =
        shadow_visibility(&mut rig, &view, &meshes, &metadata, &lights, shadow_point)?;
    eprintln!(
        "B: active {}, shadow {shadowed:.3}",
        report.visibility.active
    );
    assert!(report.visibility.active, "{:?}", report.visibility);
    assert!(
        shadowed < 0.1,
        "the off-camera caster shadows the view: {shadowed}"
    );
    let without = [meshes[0].clone(), meshes[1].clone()];
    let (open, _) = shadow_visibility(
        &mut rig,
        &view,
        &without,
        &metadata[..2],
        &lights,
        shadow_point,
    )?;
    assert!(open > 0.99, "no caster, no shadow: {open}");
    Ok(())
}
