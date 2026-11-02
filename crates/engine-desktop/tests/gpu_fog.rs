//! End-to-end authored fog: project JSON (a `Camera3D.fog`, a
//! `DirectionalLight` sun and an unlit wall mesh) -> runtime extraction ->
//! `GpuRenderer` HDR readback, compared with the CPU reference. The status
//! checks run everywhere; the GPU tests are ignored and need a real adapter:
//! `--include-ignored --test-threads=1`.
use anyhow::Result;
use engine_desktop::*;
use engine_project::rendering3d::{Camera3D, FOG_FEATURE, Fog3D, SceneRenderFrame};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_render_pipeline::fog::{FogFrame, FogSettings, fogged};
use serde_json::{Value, json};
use std::fs;

const SUN_ELEVATION_DEGREES: f64 = 25.0;
const LUX: f64 = 3.0;
const AMBIENT: f32 = 0.1;
const SIZE: u32 = 65;
const WALL_DISTANCE: f32 = 120.0;
const WALL_TINT: f64 = 0.5;

fn project(fog: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Travel direction of the sun is its entity's -Z; it shines down toward -Z.
    let elevation = SUN_ELEVATION_DEGREES.to_radians() as f32;
    let sun = glam::Quat::from_rotation_x(std::f32::consts::PI + elevation).to_array();
    let wall = |x: f32, y: f32| json!({"position":[x,y,0],"uv":[0,0],"color":[1,1,1,1]});
    fs::write(
        root.join("wall.json"),
        serde_json::to_vec(&json!({
            "vertices":[wall(-6000.0,-6000.0),wall(6000.0,-6000.0),wall(6000.0,6000.0),wall(-6000.0,6000.0)],
            "normals":[[0,0,1],[0,0,1],[0,0,1],[0,0,1]],
            "indices":[0,1,2,0,2,3]}))
        .unwrap(),
    )
    .unwrap();
    let mut camera = json!({
        "projection":{"kind":"perspective","fov_y_radians":1.0,"near":1.0,"far":20000.0},
        "lighting":{"ambient":AMBIENT,"exposure":1.0}
    });
    if !fog.is_null() {
        camera["fog"] = fog;
    }
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"fog","entities":[
            {"id":"camera","name":"Camera","components":{
                "Transform":{"position":[0,2,0]},
                "Camera2D":{"logicalSize":[SIZE, SIZE]},
                "Camera3D":camera}},
            {"id":"sun","name":"Sun","components":{
                "Transform":{"position":[0,0,0],"rotation":sun},
                "DirectionalLight":{"lux":LUX}}},
            {"id":"wall","name":"Wall","components":{
                "Transform":{"position":[0,0,-WALL_DISTANCE]},
                "MeshRenderer":{"mesh":"wall"},
                "MeshMaterial":{"unlit":true,"tint":[WALL_TINT,WALL_TINT,WALL_TINT,1.0]}}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-fog",
            "startup_scene":"scene.json","assets":[
                {"id":"wall","path":"wall.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn runtime(fog: Value) -> (tempfile::TempDir, Runtime) {
    let dir = project(fog);
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
        let tolerance = relative * expected[k].abs() + 2.0e-4;
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

fn settings_of(fog: &Fog3D) -> FogSettings {
    FogSettings {
        density: fog.density,
        height_falloff: fog.height_falloff,
        base_height: fog.base_height,
        albedo: fog.albedo,
        anisotropy: fog.anisotropy,
        sun_intensity: fog.sun_intensity,
        ambient_intensity: fog.ambient_intensity,
        sky_distance: fog.sky_distance,
        noise_amount: fog.noise_amount,
        noise_scale: fog.noise_scale,
        noise_distance: fog.noise_distance,
        noise_steps: fog.noise_steps,
        noise_seed: fog.noise_seed,
        wind: fog.wind,
    }
}

/// The CPU reference of a level ray toward -Z that crosses `distance` metres
/// of fog in front of `scene`.
fn expected(fog: &Fog3D, time: f64, distance: f64, scene: f64) -> [f64; 3] {
    fogged(
        &FogFrame::new(settings_of(fog), [0.0, 2.0, 0.0], time).unwrap(),
        [0.0, 0.0, -1.0],
        distance,
        [scene; 3],
        Some((sun(), [LUX; 3])),
        [f64::from(AMBIENT); 3],
    )
}

fn expected_wall(fog: &Fog3D, time: f64) -> [f64; 3] {
    expected(fog, time, f64::from(WALL_DISTANCE), WALL_TINT)
}

#[test]
fn the_preview_lists_fog_off_unless_the_camera_authors_it() {
    let find = |status: &engine_project::rendering3d::RenderPipelineStatus, name: &str| {
        status
            .features
            .iter()
            .position(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} listed"))
    };
    let plain = Camera3D::default();
    let status = preview_pipeline_status(None, &plain.effective_render_features()).unwrap();
    let feature = &status.features[find(&status, FOG_FEATURE)];
    assert!(!feature.enabled && feature.source == "default");

    let authored = Camera3D {
        fog: Some(Fog3D::default()),
        ..Default::default()
    };
    let status = preview_pipeline_status(None, &authored.effective_render_features()).unwrap();
    let feature = &status.features[find(&status, FOG_FEATURE)];
    assert!(feature.enabled && feature.source == "view_override");
    assert_eq!(feature.stage, "opaque_effects");
    assert!(find(&status, "engine.ssr") < find(&status, FOG_FEATURE));
    assert!(find(&status, FOG_FEATURE) < find(&status, "engine.transparent"));
    assert!(status.unknown_overrides.is_empty());
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_fog_renders_height_fog_and_survives_toggling_and_resize() -> Result<()> {
    let authored = Fog3D {
        density: 0.012,
        height_falloff: 0.02,
        anisotropy: 0.5,
        sky_distance: 3000.0,
        ..Default::default()
    };
    let (_fog_dir, fog_runtime) = runtime(serde_json::to_value(authored)?);
    let (_plain_dir, plain_runtime) = runtime(Value::Null);
    let frame = fog_runtime.extract_scene_frame()?;
    let plain_frame = plain_runtime.extract_scene_frame()?;
    assert!(frame.camera3d.as_ref().unwrap().camera.fog.is_some());

    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let mut target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;

    // No authored fog: the wall is its plain unlit colour and the sky black.
    let baseline = hdr(&mut renderer, &target, &plain_frame)?;
    assert!(
        (centre(&baseline)[0] - WALL_TINT).abs() < 2e-3,
        "the plain wall is its tint"
    );
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
            .filter(|f| f.name == FOG_FEATURE)
            .all(|f| !f.enabled)
    );

    // Authored fog matches the CPU reference at the view centre.
    let image = hdr(&mut renderer, &target, &frame)?;
    let want = expected_wall(&authored, 0.0);
    close(centre(&image), want, 0.02, "authored fog on the wall");
    let status = renderer.pipeline_status();
    let feature = status
        .features
        .iter()
        .find(|f| f.name == FOG_FEATURE)
        .unwrap();
    assert!(feature.enabled && feature.source == "view_override");
    let fogged_centre = centre(&image)[0];
    assert!(
        (fogged_centre - WALL_TINT).abs() > 0.05,
        "the wall is visibly fogged: {fogged_centre}"
    );

    // Sky pixels integrate to the authored sky distance.
    let sky = hdr(&mut renderer, &target, &without_wall(&frame))?;
    let sky_want = expected(&authored, 0.0, f64::from(authored.sky_distance), 0.0);
    close(
        centre(&sky),
        sky_want,
        0.02,
        "authored fog on the empty sky",
    );
    assert!(sky_want[0] > 1e-3, "the reference sky pixel is lit");

    // A floating origin moves the layer: the same absolute altitude gives the
    // same image, whether it is the camera's height or the applied origin.
    let mut high = frame.clone();
    high.camera3d.as_mut().unwrap().world[3][1] = 62.0;
    let direct = hdr(&mut renderer, &target, &high)?;
    let mut rebased = frame.clone();
    rebased.applied_origin = [123_456.0, 60.0, -789.0];
    let shifted = hdr(&mut renderer, &target, &rebased)?;
    assert_eq!(
        shifted.pixels, direct.pixels,
        "rebasing leaves the fog alone"
    );
    assert_ne!(direct.pixels, image.pixels, "60 m up looks different");

    // Disabling the feature through the camera restores the plain image.
    let mut off = frame.clone();
    off.camera3d
        .as_mut()
        .unwrap()
        .camera
        .render_features
        .insert(FOG_FEATURE.into(), false);
    assert_eq!(hdr(&mut renderer, &target, &off)?.pixels, baseline.pixels);
    // Re-enabling restores the fogged result exactly.
    assert_eq!(hdr(&mut renderer, &target, &frame)?.pixels, image.pixels);

    // Resize keeps the authored fog.
    renderer.resize(81, 81)?;
    target = OffscreenTarget::new(&renderer.gpu, 81, 81)?;
    let resized = hdr(&mut renderer, &target, &frame)?;
    close(centre(&resized), want, 0.02, "after resize");
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn the_scene_clock_drives_the_wind_end_to_end() -> Result<()> {
    let authored = Fog3D {
        density: 0.02,
        height_falloff: 0.0,
        noise_amount: 0.9,
        noise_scale: 12.0,
        noise_distance: 90.0,
        noise_steps: 32,
        sky_distance: 0.0,
        wind: [4.0, 0.0, 0.0],
        ..Default::default()
    };
    let (_dir, mut runtime) = runtime(serde_json::to_value(authored)?);
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;

    let start = runtime.extract_scene_frame()?;
    assert_eq!(start.scene_time, 0.0);
    let at_zero = hdr(&mut renderer, &target, &start)?;
    close(
        centre(&at_zero),
        expected_wall(&authored, 0.0),
        0.03,
        "noisy fog at t = 0",
    );

    // Real runtime ticks advance the wind; wall-clock time does not.
    for _ in 0..4 {
        runtime.tick(0.25, InputSnapshot::default())?;
    }
    let ticked = runtime.extract_scene_frame()?;
    assert_eq!(ticked.scene_time, 1.0);
    let moved = hdr(&mut renderer, &target, &ticked)?;
    assert_ne!(moved.pixels, at_zero.pixels, "the fog moved with the clock");
    close(
        centre(&moved),
        expected_wall(&authored, 1.0),
        0.03,
        "noisy fog at t = 1 s",
    );
    // Re-rendering the same frame later gives the same image: no wall time.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(hdr(&mut renderer, &target, &ticked)?.pixels, moved.pixels);

    // Wind over one second equals a camera moved 4 m against it.
    let mut still = start.clone();
    {
        let camera = &mut still.camera3d.as_mut().unwrap().camera;
        camera.fog.as_mut().unwrap().wind = [0.0; 3];
    }
    still.applied_origin = [-4.0, 0.0, 0.0];
    let equivalent = hdr(&mut renderer, &target, &still)?;
    let worst = moved
        .pixels
        .iter()
        .zip(&equivalent.pixels)
        .flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs()))
        .fold(0.0f32, f32::max);
    assert!(worst < 2e-4, "wind equals a shifted camera: {worst}");

    // Zero wind is static at any scene time.
    let mut calm = ticked.clone();
    calm.camera3d
        .as_mut()
        .unwrap()
        .camera
        .fog
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
fn a_submerged_camera_is_not_fogged_twice() -> Result<()> {
    use bevy_ecs::world::World;
    use engine_project::world::water::{WaterKind, WaterRenderItem, WaterRenderSet};
    use engine_water::{ShorelineSettings, WaterOptics, WaterShape, WaterSurface, Waves};

    let authored = Fog3D {
        density: 0.012,
        height_falloff: 0.0,
        ..Default::default()
    };
    let (_dir, runtime) = runtime(serde_json::to_value(authored)?);
    let (_plain_dir, plain_runtime) = self::runtime(Value::Null);
    let frame = runtime.extract_scene_frame()?;
    let plain = plain_runtime.extract_scene_frame()?;
    let lake = |world: &mut World| {
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
    };
    let mut world = World::new();
    lake(&mut world);

    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let render = |renderer: &mut GpuRenderer, frame: &SceneRenderFrame, world: Option<&World>| {
        renderer.render_scene(frame, &target.view, world)?;
        renderer
            .pipeline()?
            .read_scene_hdr(render_pipeline::ViewId::MAIN)
    };

    // Above the lake (camera at 2 m, surface at 0): the air fog applies.
    let above = render(&mut renderer, &frame, Some(&world))?;
    let baseline = render(&mut renderer, &plain, Some(&world))?;
    assert_ne!(above.pixels, baseline.pixels, "fog above the water");
    // Below the surface the water's fog owns the medium: this renderer has no
    // underwater pass, so the image is exactly the unfogged baseline.
    let mut below = frame.clone();
    below.camera3d.as_mut().unwrap().world[3][1] = -3.0;
    let mut below_plain = plain.clone();
    below_plain.camera3d.as_mut().unwrap().world[3][1] = -3.0;
    assert_eq!(
        render(&mut renderer, &below, Some(&world))?.pixels,
        render(&mut renderer, &below_plain, Some(&world))?.pixels,
        "no air fog under water"
    );
    // The same camera without any water is fogged again.
    assert_ne!(
        render(&mut renderer, &below, None)?.pixels,
        render(&mut renderer, &below_plain, None)?.pixels
    );
    Ok(())
}
