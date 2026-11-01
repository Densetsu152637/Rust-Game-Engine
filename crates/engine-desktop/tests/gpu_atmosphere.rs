//! End-to-end authored atmosphere: project JSON (a `Camera3D.atmosphere`, a
//! `DirectionalLight` sun and an optional far wall mesh) -> runtime extraction
//! -> `GpuRenderer` HDR and display readback, compared with the CPU reference.
//! The status checks run everywhere; the GPU test is ignored and needs a real
//! adapter: `--ignored --test-threads=1`.
use anyhow::Result;
use engine_desktop::*;
use engine_project::rendering3d::{
    ATMOSPHERE_AERIAL_FEATURE, ATMOSPHERE_FEATURE, Atmosphere3D, Camera3D, SceneRenderFrame,
};
use engine_project::{Project, Runtime};
use engine_render_pipeline::atmosphere::{AtmosphereSettings, Medium, aerial, sky_radiance};
use serde_json::{Value, json};
use std::fs;

const SUN_ELEVATION_DEGREES: f64 = 40.0;
const LUX: f64 = 3.0;
const SIZE: u32 = 65;
const WALL_DISTANCE: f32 = 20_000.0;

fn project(atmosphere: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Travel direction of the sun is its entity's -Z; it shines down toward -Z.
    let elevation = SUN_ELEVATION_DEGREES.to_radians() as f32;
    let sun = glam::Quat::from_rotation_x(std::f32::consts::PI + elevation).to_array();
    let wall = |x: f32, y: f32| json!({"position":[x,y,0],"uv":[0,0],"color":[1,1,1,1]});
    fs::write(
        root.join("wall.json"),
        serde_json::to_vec(&json!({
            "vertices":[wall(-60000.0,-60000.0),wall(60000.0,-60000.0),wall(60000.0,60000.0),wall(-60000.0,60000.0)],
            "normals":[[0,0,1],[0,0,1],[0,0,1],[0,0,1]],
            "indices":[0,1,2,0,2,3]}))
        .unwrap(),
    )
    .unwrap();
    let mut camera = json!({
        "projection":{"kind":"perspective","fov_y_radians":1.0,"near":1.0,"far":200000.0},
        "lighting":{"ambient":0.0,"exposure":1.0}
    });
    if !atmosphere.is_null() {
        camera["atmosphere"] = atmosphere;
    }
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"sky","entities":[
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
                "MeshMaterial":{"unlit":true,"tint":[0.5,0.5,0.5,1.0]}}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-atmosphere",
            "startup_scene":"scene.json","assets":[
                {"id":"wall","path":"wall.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn extract(atmosphere: Value) -> (tempfile::TempDir, SceneRenderFrame) {
    let dir = project(atmosphere);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    (dir, frame)
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

#[test]
fn the_preview_lists_both_features_off_unless_the_camera_authors_an_atmosphere() {
    let plain = Camera3D::default();
    let status = preview_pipeline_status(None, &plain.effective_render_features()).unwrap();
    let find = |status: &engine_project::rendering3d::RenderPipelineStatus, name: &str| {
        status
            .features
            .iter()
            .position(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} listed"))
    };
    for name in [ATMOSPHERE_FEATURE, ATMOSPHERE_AERIAL_FEATURE] {
        let feature = &status.features[find(&status, name)];
        assert!(!feature.enabled && feature.source == "default", "{name}");
    }
    let authored = Camera3D {
        atmosphere: Some(Atmosphere3D::default()),
        ..Default::default()
    };
    let status = preview_pipeline_status(None, &authored.effective_render_features()).unwrap();
    for (name, stage) in [
        (ATMOSPHERE_FEATURE, "sky"),
        (ATMOSPHERE_AERIAL_FEATURE, "opaque_effects"),
    ] {
        let feature = &status.features[find(&status, name)];
        assert!(
            feature.enabled && feature.source == "view_override",
            "{name}"
        );
        assert_eq!(feature.stage, stage);
    }
    assert!(find(&status, "engine.opaque") < find(&status, ATMOSPHERE_FEATURE));
    assert!(find(&status, ATMOSPHERE_FEATURE) < find(&status, ATMOSPHERE_AERIAL_FEATURE));
    assert!(find(&status, ATMOSPHERE_AERIAL_FEATURE) < find(&status, "engine.transparent"));
    assert!(status.unknown_overrides.is_empty());
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_atmosphere_renders_sky_aerial_perspective_and_survives_rebasing_and_resize()
-> Result<()> {
    let (_atmosphere_dir, frame) = extract(json!({}));
    let (_plain_dir, plain_frame) = extract(Value::Null);
    let sky_frame = without_wall(&frame);
    assert!(frame.camera3d.as_ref().unwrap().camera.atmosphere.is_some());

    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let mut target = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;

    // No authored atmosphere: the sky is the black background and the wall is
    // its plain unlit colour, exactly as before the feature existed.
    let baseline_sky = hdr(&mut renderer, &target, &without_wall(&plain_frame))?;
    assert!(baseline_sky.pixels.iter().all(|p| p[..3] == [0.0; 3]));
    let baseline_wall = hdr(&mut renderer, &target, &plain_frame)?;
    assert!(
        renderer
            .pipeline_status()
            .features
            .iter()
            .filter(|f| f.name.starts_with("engine.atmosphere"))
            .all(|f| !f.enabled)
    );

    // Authored sky matches the CPU reference at the view centre (level, -Z).
    let medium = Medium::new(&AtmosphereSettings::default());
    let irradiance = [LUX; 3];
    let sky = hdr(&mut renderer, &target, &sky_frame)?;
    let expected = sky_radiance(
        &medium,
        2.0,
        [0.0, 0.0, -1.0],
        sun(),
        irradiance,
        0.5 * f64::from(AtmosphereSettings::default().sun_angular_diameter),
    );
    close(centre(&sky), expected, 0.05, "authored sky at altitude 2 m");
    assert!(expected[2] > 0.01, "the reference sky is lit: {expected:?}");
    let status = renderer.pipeline_status();
    for name in [ATMOSPHERE_FEATURE, ATMOSPHERE_AERIAL_FEATURE] {
        let feature = status.features.iter().find(|f| f.name == name).unwrap();
        assert!(
            feature.enabled && feature.source == "view_override",
            "{name}"
        );
    }
    // The sky is bluer overhead than the horizon: top row against centre row.
    let top = sky.pixel(SIZE / 2, 1);
    let mid = sky.pixel(SIZE / 2, SIZE / 2);
    assert!(top[2] / top[0] > mid[2] / mid[0]);

    // Aerial perspective on the wall 20 km away.
    let wall = hdr(&mut renderer, &target, &frame)?;
    let (inscatter, transmittance) = aerial(
        &medium,
        2.0,
        [0.0, 0.0, -1.0],
        sun(),
        irradiance,
        f64::from(WALL_DISTANCE),
    );
    let grey = f64::from(baseline_wall.pixel(SIZE / 2, SIZE / 2)[0]);
    assert!(
        (grey - 0.5).abs() < 2e-3,
        "the plain wall is its tint: {grey}"
    );
    let expected_wall: [f64; 3] = std::array::from_fn(|k| grey * transmittance[k] + inscatter[k]);
    close(centre(&wall), expected_wall, 0.05, "wall 20 km away");
    // The wall moved toward the sky colour seen beside it.
    let sky_centre = centre(&sky);
    let gap = |c: [f64; 3]| (0..3).map(|k| (c[k] - sky_centre[k]).abs()).sum::<f64>();
    let plain_wall = [grey; 3];
    assert!(
        gap(centre(&wall)) < gap(plain_wall) * 0.98,
        "distance shifts the wall toward the sky: {:?}",
        centre(&wall)
    );

    // Display output (tonemapped sRGB) shows the blue sky.
    renderer.render_scene(&sky_frame, &target.view, None)?;
    let pixels = target.read_rgba(&renderer.gpu)?;
    let at = |x: u32, y: u32| {
        let i = ((y * SIZE + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    };
    let top = at(SIZE / 2, 2);
    assert!(top[2] > top[0], "display sky is bluish: {top:?}");

    // Large-world rebasing: the same absolute altitude gives the same image,
    // whether it is the camera's local height or the applied origin.
    let mut high = sky_frame.clone();
    high.camera3d.as_mut().unwrap().world[3][1] = 3002.0;
    let direct = hdr(&mut renderer, &target, &high)?;
    let mut rebased = sky_frame.clone();
    rebased.applied_origin = [123_456.0, 3000.0, -789.0];
    let shifted = hdr(&mut renderer, &target, &rebased)?;
    assert_eq!(
        shifted.pixels, direct.pixels,
        "rebasing leaves the sky alone"
    );
    assert_ne!(
        direct.pixels, sky.pixels,
        "3 km up looks different from 2 m"
    );
    // A huge origin keeps working; the altitude saturates the model, not the f32.
    let mut orbit = sky_frame.clone();
    orbit.applied_origin = [0.0, 1.0e9, 0.0];
    let far = hdr(&mut renderer, &target, &orbit)?;
    assert!(far.pixels.iter().all(|p| p.iter().all(|v| v.is_finite())));

    // Disabling the passes through the camera restores the plain image.
    let mut off = frame.clone();
    {
        let camera = &mut off.camera3d.as_mut().unwrap().camera;
        camera
            .render_features
            .insert(ATMOSPHERE_FEATURE.into(), false);
        camera
            .render_features
            .insert(ATMOSPHERE_AERIAL_FEATURE.into(), false);
    }
    assert_eq!(
        hdr(&mut renderer, &target, &off)?.pixels,
        baseline_wall.pixels
    );
    // Re-enabling restores the atmosphere result exactly.
    assert_eq!(hdr(&mut renderer, &target, &frame)?.pixels, wall.pixels);

    // Resize keeps the authored atmosphere.
    renderer.resize(81, 81)?;
    target = OffscreenTarget::new(&renderer.gpu, 81, 81)?;
    let resized = hdr(&mut renderer, &target, &sky_frame)?;
    close(centre(&resized), expected, 0.05, "after resize");
    Ok(())
}
