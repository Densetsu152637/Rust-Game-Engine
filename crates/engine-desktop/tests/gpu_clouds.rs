//! End-to-end authored clouds: project JSON (a `Camera3D.clouds` looking up at
//! the layer, a `DirectionalLight` sun and an unlit wall mesh beyond it) ->
//! runtime extraction -> `GpuRenderer` HDR readback, compared with the CPU
//! reference. The status checks run everywhere; the GPU tests are ignored and
//! need a real adapter: `--include-ignored --test-threads=1`.
use anyhow::Result;
use engine_desktop::*;
use engine_project::rendering3d::{
    CLOUDS_FEATURE, Camera3D, Clouds3D, FOG_FEATURE, SceneRenderFrame,
};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_render_pipeline::clouds::{CloudFrame, CloudSettings, composite};
use serde_json::{Value, json};
use std::fs;

const SUN_ELEVATION_DEGREES: f64 = 25.0;
const LUX: f64 = 3.0;
const AMBIENT: f32 = 0.1;
const SIZE: u32 = 65;
const PITCH: f32 = 0.6;
const WALL_DEPTH: f32 = 4000.0;
const WALL_TINT: f64 = 0.5;

fn project(clouds: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Travel direction of the sun is its entity's -Z; it shines down toward -Z.
    let elevation = SUN_ELEVATION_DEGREES.to_radians() as f32;
    let sun = glam::Quat::from_rotation_x(std::f32::consts::PI + elevation).to_array();
    let look_up = glam::Quat::from_rotation_x(PITCH).to_array();
    let wall = |x: f32, y: f32| json!({"position":[x,y,0],"uv":[0,0],"color":[1,1,1,1]});
    fs::write(
        root.join("wall.json"),
        serde_json::to_vec(&json!({
            "vertices":[wall(-12000.0,-12000.0),wall(12000.0,-12000.0),wall(12000.0,12000.0),wall(-12000.0,12000.0)],
            "normals":[[0,0,1],[0,0,1],[0,0,1],[0,0,1]],
            "indices":[0,1,2,0,2,3]}))
        .unwrap(),
    )
    .unwrap();
    let mut camera = json!({
        "projection":{"kind":"perspective","fov_y_radians":1.0,"near":1.0,"far":20000.0},
        "lighting":{"ambient":AMBIENT,"exposure":1.0}
    });
    if !clouds.is_null() {
        camera["clouds"] = clouds;
    }
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"clouds","entities":[
            {"id":"camera","name":"Camera","components":{
                "Transform":{"position":[0,2,0],"rotation":look_up},
                "Camera2D":{"logicalSize":[SIZE, SIZE]},
                "Camera3D":camera}},
            {"id":"sun","name":"Sun","components":{
                "Transform":{"position":[0,0,0],"rotation":sun},
                "DirectionalLight":{"lux":LUX}}},
            {"id":"wall","name":"Wall","components":{
                "Transform":{"position":[0,0,-WALL_DEPTH]},
                "MeshRenderer":{"mesh":"wall"},
                "MeshMaterial":{"unlit":true,"tint":[WALL_TINT,WALL_TINT,WALL_TINT,1.0]}}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-clouds",
            "startup_scene":"scene.json","assets":[
                {"id":"wall","path":"wall.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn runtime(clouds: Value) -> (tempfile::TempDir, Runtime) {
    let dir = project(clouds);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}

fn without_wall(frame: &SceneRenderFrame) -> SceneRenderFrame {
    let mut sky = frame.clone();
    sky.meshes.clear();
    sky
}

fn hdr(
    renderer: &mut GpuRenderer,
    target: &OffscreenTarget,
    frame: &SceneRenderFrame,
) -> Result<render_pipeline::HdrImage> {
    renderer.render_scene(frame, &target.view, None)?;
    renderer
        .pipeline()?
        .read_scene_hdr(render_pipeline::ViewId::MAIN)
}

fn centre(image: &render_pipeline::HdrImage) -> [f64; 3] {
    let p = image.pixel(image.size[0] / 2, image.size[1] / 2);
    [p[0], p[1], p[2]].map(f64::from)
}

#[track_caller]
fn close(actual: [f64; 3], expected: [f64; 3], relative: f64, what: &str) {
    for k in 0..3 {
        let tolerance = relative * expected[k].abs() + 5.0e-4;
        assert!(
            (actual[k] - expected[k]).abs() <= tolerance,
            "{what}: {actual:?} vs reference {expected:?}"
        );
    }
}

fn sun() -> [f64; 3] {
    let e = SUN_ELEVATION_DEGREES.to_radians();
    [0.0, e.sin(), -e.cos()]
}

fn settings_of(c: &Clouds3D) -> CloudSettings {
    CloudSettings {
        base_altitude: c.base_altitude,
        top_altitude: c.top_altitude,
        density: c.density,
        coverage: c.coverage,
        albedo: c.albedo,
        anisotropy: c.anisotropy,
        sun_intensity: c.sun_intensity,
        ambient_intensity: c.ambient_intensity,
        multiple_scattering: c.multiple_scattering,
        noise_amount: c.noise_amount,
        noise_scale: c.noise_scale,
        noise_octaves: c.noise_octaves,
        noise_seed: c.noise_seed,
        wind: c.wind,
        max_distance: c.max_distance,
        fade_distance: c.fade_distance,
        primary_steps: c.primary_steps,
        light_steps: c.light_steps,
        light_distance: c.light_distance,
    }
}

/// The CPU reference of the centre ray (pitched up by [`PITCH`]) that meets
/// `scene` at `distance` metres (`None`: sky) from a camera at 2 m.
fn expected(clouds: &Clouds3D, time: f64, distance: Option<f64>, scene: f64) -> [f64; 3] {
    let (s, c) = f64::from(PITCH).sin_cos();
    composite(
        &CloudFrame::new(settings_of(clouds), [0.0, 2.0, 0.0], time).unwrap(),
        [0.0, s, -c],
        distance,
        [scene; 3],
        Some((sun(), [LUX; 3])),
        [f64::from(AMBIENT); 3],
    )
}

fn expected_wall(clouds: &Clouds3D, time: f64) -> [f64; 3] {
    let distance = f64::from(WALL_DEPTH) / f64::from(PITCH).cos();
    expected(clouds, time, Some(distance), WALL_TINT)
}

/// Thin enough that the wall shows through, with structure.
fn authored() -> Clouds3D {
    Clouds3D {
        base_altitude: 100.0,
        top_altitude: 300.0,
        density: 0.006,
        coverage: 0.7,
        noise_scale: 80.0,
        noise_octaves: 2,
        noise_seed: 3,
        primary_steps: 96,
        ..Default::default()
    }
}

#[test]
fn the_preview_lists_clouds_off_unless_the_camera_authors_them() {
    let find = |status: &engine_project::rendering3d::RenderPipelineStatus, name: &str| {
        status
            .features
            .iter()
            .position(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} listed"))
    };
    let plain = Camera3D::default();
    let status = preview_pipeline_status(None, &plain.effective_render_features()).unwrap();
    let feature = &status.features[find(&status, CLOUDS_FEATURE)];
    assert!(!feature.enabled && feature.source == "default");

    let authored = Camera3D {
        clouds: Some(Clouds3D::default()),
        ..Default::default()
    };
    let status = preview_pipeline_status(None, &authored.effective_render_features()).unwrap();
    let feature = &status.features[find(&status, CLOUDS_FEATURE)];
    assert!(feature.enabled && feature.source == "view_override");
    assert_eq!(feature.stage, "opaque_effects");
    assert!(find(&status, "engine.ssr") < find(&status, CLOUDS_FEATURE));
    assert!(find(&status, "engine.atmosphere.aerial") < find(&status, CLOUDS_FEATURE));
    // Clouds are composited before the fog is applied over them.
    assert!(find(&status, CLOUDS_FEATURE) < find(&status, FOG_FEATURE));
    assert!(find(&status, CLOUDS_FEATURE) < find(&status, "engine.transparent"));
    assert!(status.unknown_overrides.is_empty());
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_clouds_render_over_sky_and_geometry_and_survive_toggling_and_resize() -> Result<()> {
    let clouds = authored();
    let (_dir, cloud_runtime) = runtime(serde_json::to_value(clouds)?);
    let (_plain_dir, plain_runtime) = runtime(Value::Null);
    let frame = cloud_runtime.extract_scene_frame()?;
    let plain_frame = plain_runtime.extract_scene_frame()?;
    assert!(frame.camera3d.as_ref().unwrap().camera.clouds.is_some());

    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let mut target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;

    // No authored clouds: the wall is its plain unlit colour and the sky black.
    let baseline = hdr(&mut renderer, &target, &plain_frame)?;
    assert!((centre(&baseline)[0] - WALL_TINT).abs() < 2e-3);
    assert!(
        hdr(&mut renderer, &target, &without_wall(&plain_frame))?
            .pixels
            .iter()
            .all(|p| p[..3] == [0.0; 3])
    );
    assert!(
        renderer
            .pipeline_status()
            .features
            .iter()
            .filter(|f| f.name == CLOUDS_FEATURE)
            .all(|f| !f.enabled)
    );

    // Authored clouds cover the farther wall and match the CPU reference.
    let image = hdr(&mut renderer, &target, &frame)?;
    let want = expected_wall(&clouds, 0.0);
    close(centre(&image), want, 0.04, "authored clouds over the wall");
    assert!(
        (centre(&image)[0] - WALL_TINT).abs() > 0.02,
        "the wall is visibly covered: {:?}",
        centre(&image)
    );
    let status = renderer.pipeline_status();
    let feature = status
        .features
        .iter()
        .find(|f| f.name == CLOUDS_FEATURE)
        .unwrap();
    assert!(feature.enabled && feature.source == "view_override");

    // Sky pixels show the clouds lit by the sun and the ambient light.
    let sky = hdr(&mut renderer, &target, &without_wall(&frame))?;
    let sky_want = expected(&clouds, 0.0, None, 0.0);
    close(
        centre(&sky),
        sky_want,
        0.04,
        "authored clouds on the empty sky",
    );
    assert!(sky_want[0] > 1e-3, "the reference cloud pixel is lit");

    // A floating origin carries altitude: the same absolute height gives the
    // same image, whether it is the camera's height or the applied origin.
    let mut high = frame.clone();
    high.camera3d.as_mut().unwrap().world[3][1] = 62.0;
    let direct = hdr(&mut renderer, &target, &without_wall(&high))?;
    let mut rebased = frame.clone();
    rebased.applied_origin = [0.0, 60.0, 0.0];
    let shifted = hdr(&mut renderer, &target, &without_wall(&rebased))?;
    assert_eq!(
        shifted.pixels, direct.pixels,
        "rebasing leaves the sky alone"
    );
    assert_ne!(direct.pixels, sky.pixels, "60 m up looks different");

    // Disabling the feature through the camera restores the plain image.
    let mut off = frame.clone();
    off.camera3d
        .as_mut()
        .unwrap()
        .camera
        .render_features
        .insert(CLOUDS_FEATURE.into(), false);
    assert_eq!(hdr(&mut renderer, &target, &off)?.pixels, baseline.pixels);
    // Re-enabling restores the clouded result exactly.
    assert_eq!(hdr(&mut renderer, &target, &frame)?.pixels, image.pixels);

    // Geometry nearer than the layer hides the clouds.
    let mut near = frame.clone();
    for mesh in &mut near.meshes {
        mesh.model[3][2] = -60.0;
    }
    let near_image = hdr(&mut renderer, &target, &near)?;
    close(
        centre(&near_image),
        [WALL_TINT; 3],
        0.002,
        "a near wall is not clouded",
    );

    // Resize keeps the authored clouds.
    renderer.resize(81, 81)?;
    target = OffscreenTarget::new(&renderer.gpu, 81, 81)?;
    let resized = hdr(&mut renderer, &target, &frame)?;
    close(centre(&resized), want, 0.04, "after resize");
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn the_scene_clock_drives_the_wind_end_to_end() -> Result<()> {
    let clouds = Clouds3D {
        wind: [40.0, 0.0, 0.0],
        coverage: 0.9,
        density: 0.012,
        ..authored()
    };
    let (_dir, mut runtime) = runtime(serde_json::to_value(clouds)?);
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;

    let start = runtime.extract_scene_frame()?;
    assert_eq!(start.scene_time, 0.0);
    let at_zero = hdr(&mut renderer, &target, &start)?;
    close(
        centre(&at_zero),
        expected_wall(&clouds, 0.0),
        0.04,
        "clouds at t = 0",
    );

    // Real runtime ticks advance the wind; wall-clock time does not.
    for _ in 0..4 {
        runtime.tick(0.25, InputSnapshot::default())?;
    }
    let ticked = runtime.extract_scene_frame()?;
    assert_eq!(ticked.scene_time, 1.0);
    let moved = hdr(&mut renderer, &target, &ticked)?;
    assert_ne!(
        moved.pixels, at_zero.pixels,
        "the clouds moved with the clock"
    );
    close(
        centre(&moved),
        expected_wall(&clouds, 1.0),
        0.04,
        "clouds at t = 1 s",
    );
    // Re-rendering the same frame later gives the same image: no wall time.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(hdr(&mut renderer, &target, &ticked)?.pixels, moved.pixels);

    // Wind over one second equals a camera moved 40 m against it.
    let mut still = start.clone();
    still
        .camera3d
        .as_mut()
        .unwrap()
        .camera
        .clouds
        .as_mut()
        .unwrap()
        .wind = [0.0; 3];
    still.applied_origin = [-40.0, 0.0, 0.0];
    let equivalent = hdr(&mut renderer, &target, &still)?;
    let worst = moved
        .pixels
        .iter()
        .zip(&equivalent.pixels)
        .flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs()))
        .fold(0.0f32, f32::max);
    assert!(worst < 5e-4, "wind equals a shifted camera: {worst}");

    // Zero wind is static at any scene time.
    let mut calm = ticked.clone();
    calm.camera3d
        .as_mut()
        .unwrap()
        .camera
        .clouds
        .as_mut()
        .unwrap()
        .wind = [0.0; 3];
    let a = hdr(&mut renderer, &target, &calm)?;
    calm.scene_time = 5.0e5;
    assert_eq!(hdr(&mut renderer, &target, &calm)?.pixels, a.pixels);
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn a_submerged_camera_gets_no_clouds() -> Result<()> {
    use bevy_ecs::world::World;
    use engine_project::world::water::{WaterKind, WaterRenderItem, WaterRenderSet};
    use engine_water::{ShorelineSettings, WaterOptics, WaterShape, WaterSurface, Waves};

    let (_dir, runtime) = runtime(serde_json::to_value(authored())?);
    let (_plain_dir, plain_runtime) = self::runtime(Value::Null);
    let frame = runtime.extract_scene_frame()?;
    let plain = plain_runtime.extract_scene_frame()?;
    let mut world = World::new();
    world.insert_resource(WaterRenderSet {
        items: vec![WaterRenderItem {
            owner: bevy_ecs::entity::Entity::PLACEHOLDER,
            id: "lake".into(),
            kind: WaterKind::Lake,
            surface: std::sync::Arc::new(WaterSurface {
                shape: WaterShape::Lake {
                    half_extent: glam::Vec2::splat(500.0),
                },
                waves: Waves::Flat,
                optics: WaterOptics::default(),
                shoreline: ShorelineSettings::default(),
                time: 0.0,
            }),
            world_from_local: glam::Mat4::IDENTITY.to_cols_array_2d(),
            half_extent: [500.0, 500.0],
            grid_resolution: 8,
            cast_shadows: false,
            shore: None,
            revision: 1,
        }],
        time: 0.0,
    });

    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let render = |renderer: &mut GpuRenderer, frame: &SceneRenderFrame, world: Option<&World>| {
        renderer.render_scene(frame, &target.view, world)?;
        renderer
            .pipeline()?
            .read_scene_hdr(render_pipeline::ViewId::MAIN)
    };
    // Above the lake the clouds apply.
    assert_ne!(
        render(&mut renderer, &frame, Some(&world))?.pixels,
        render(&mut renderer, &plain, Some(&world))?.pixels,
        "clouds above the water"
    );
    // Below the surface the water's medium owns the view: no clouds.
    let mut below = frame.clone();
    below.camera3d.as_mut().unwrap().world[3][1] = -3.0;
    let mut below_plain = plain.clone();
    below_plain.camera3d.as_mut().unwrap().world[3][1] = -3.0;
    assert_eq!(
        render(&mut renderer, &below, Some(&world))?.pixels,
        render(&mut renderer, &below_plain, Some(&world))?.pixels,
        "no clouds under water"
    );
    // The same camera without any water is clouded again.
    assert_ne!(
        render(&mut renderer, &below, None)?.pixels,
        render(&mut renderer, &below_plain, None)?.pixels
    );
    Ok(())
}
