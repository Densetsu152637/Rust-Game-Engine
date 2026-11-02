//! End-to-end authored screen-space reflections: project JSON (a metallic
//! mirror floor, an unlit red wall, `Camera3D.screen_space_reflections`) ->
//! runtime extraction -> desktop GPU HDR readback, including removal, the
//! explicit feature switch, an RT-reflection request without an RT device, resize and a
//! rejected invalid setting.
//! Run explicitly on a real adapter with --ignored --test-threads=1.
use anyhow::Result;
use engine_desktop::*;
use engine_project::{Project, Runtime, rendering3d::SceneRenderFrame};
use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};
use std::fs;

const EYE: Vec3 = Vec3::new(0.0, 2.5, 6.0);
/// Pitch of the camera towards the floor (radians).
const PITCH: f32 = 0.3217506;
const RED: [f32; 3] = [1.0, 0.1, 0.1];
const WALL_Z: f32 = -1.0;
const WALL_HEIGHT: f32 = 1.2;

fn project(camera3d: Value, size: [u32; 2]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let vertex =
        |x: f32, y: f32, z: f32| json!({"position":[x,y,z],"uv":[0.5,0.5],"color":[1,1,1,1]});
    // A 16 x 16 floor facing +Y (counter-clockwise seen from above).
    fs::write(
        root.join("floor.json"),
        serde_json::to_vec(&json!({
            "vertices":[vertex(-8.0,0.0,-8.0),vertex(8.0,0.0,-8.0),vertex(8.0,0.0,8.0),vertex(-8.0,0.0,8.0)],
            "normals":[[0,1,0],[0,1,0],[0,1,0],[0,1,0]],
            "indices":[0,2,1,0,3,2]}))
        .unwrap(),
    )
    .unwrap();
    // A 1.2 x 1.2 wall facing +Z.
    fs::write(
        root.join("wall.json"),
        serde_json::to_vec(&json!({
            "vertices":[vertex(-0.6,0.0,0.0),vertex(0.6,0.0,0.0),vertex(0.6,WALL_HEIGHT,0.0),vertex(-0.6,WALL_HEIGHT,0.0)],
            "normals":[[0,0,1],[0,0,1],[0,0,1],[0,0,1]],
            "indices":[0,1,2,0,2,3]}))
        .unwrap(),
    )
    .unwrap();
    let rotation = Quat::from_rotation_x(-PITCH);
    let entities = json!([
        {"id":"camera","name":"Camera","components":{
            "Transform":{"position":[EYE.x,EYE.y,EYE.z],"rotation":[rotation.x,rotation.y,rotation.z,rotation.w]},
            "Camera2D":{"logicalSize":size},
            "Camera3D":camera3d}},
        {"id":"floor","name":"Floor","components":{
            "Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"floor"},
            "MeshMaterial":{"metallic":1,"roughness":0}}},
        {"id":"wall","name":"Wall","components":{
            "Transform":{"position":[0,0,WALL_Z]},"MeshRenderer":{"mesh":"wall"},
            "MeshMaterial":{"unlit":true,"tint":[RED[0],RED[1],RED[2],1]}}}
    ]);
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"ssr","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-ssr",
            "startup_scene":"scene.json","assets":[
                {"id":"floor","path":"floor.json","kind":"mesh"},
                {"id":"wall","path":"wall.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn extract(camera3d: Value) -> Result<(tempfile::TempDir, SceneRenderFrame)> {
    extract_sized(camera3d, [96, 72])
}

fn extract_sized(camera3d: Value, size: [u32; 2]) -> Result<(tempfile::TempDir, SceneRenderFrame)> {
    let dir = project(camera3d, size);
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime.start()?;
    let frame = runtime.extract_scene_frame()?;
    Ok((dir, frame))
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

/// Pixel of a world point for the authored camera at `size`.
fn pixel_of(point: Vec3, size: [u32; 2]) -> (u32, u32) {
    let world_from_view =
        Mat4::from_translation(EYE) * Mat4::from_quat(Quat::from_rotation_x(-PITCH));
    let projection = glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_3,
        size[0] as f32 / size[1] as f32,
        0.1,
        1000.0,
    );
    let clip = projection * world_from_view.inverse() * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x * 0.5 + 0.5) * size[0] as f32) as u32,
        ((0.5 - ndc.y * 0.5) * size[1] as f32) as u32,
    )
}

/// Where the mirror image of the wall's centre shows on the floor.
fn reflection(size: [u32; 2]) -> (u32, u32) {
    let image = Vec3::new(0.0, -WALL_HEIGHT * 0.5, WALL_Z);
    let t = EYE.y / (EYE.y - image.y);
    pixel_of(EYE + (image - EYE) * t, size)
}

#[track_caller]
fn close(actual: [f32; 4], expected: [f32; 3], tolerance: f32, what: &str) {
    for c in 0..3 {
        assert!(
            (actual[c] - expected[c]).abs() <= tolerance,
            "{what}: {actual:?} vs {expected:?}"
        );
    }
}

fn feature(renderer: &GpuRenderer, name: &str) -> (bool, String) {
    let f = renderer
        .pipeline_status()
        .features
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} is not reported"));
    (f.enabled, f.activity.clone())
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_screen_space_reflections_reach_pixels_end_to_end() -> Result<()> {
    let authored = json!({"screen_space_reflections":{
        "max_steps":96,"max_distance":40,"thickness":0.6,"roughness_cutoff":0.5,
        "edge_fade":0.05,"intensity":1.0}});
    let (_plain_dir, plain) = extract(json!({}))?;
    let (_dir, frame) = extract(authored.clone())?;
    let settings = frame
        .camera3d
        .as_ref()
        .unwrap()
        .camera
        .screen_space_reflections
        .expect("authored settings reach the extracted frame");
    assert_eq!(settings.max_steps, 96);
    assert_eq!(settings.thickness, 0.6);
    assert!(
        plain
            .camera3d
            .as_ref()
            .unwrap()
            .camera
            .screen_space_reflections
            .is_none()
    );

    let size = [96, 72];
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, size[0], size[1])?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let spot = reflection(size);
    let wall = pixel_of(Vec3::new(0.0, WALL_HEIGHT * 0.5, WALL_Z), size);

    let baseline = hdr(&mut renderer, &target, &plain)?;
    assert_eq!(feature(&renderer, "engine.ssr"), (false, "disabled".into()));
    assert!(
        baseline.pixel(spot.0, spot.1)[..3]
            .iter()
            .all(|v| v.abs() < 0.01)
    );
    close(baseline.pixel(wall.0, wall.1), RED, 0.02, "unlit wall");

    let on = hdr(&mut renderer, &target, &frame)?;
    assert_eq!(feature(&renderer, "engine.ssr"), (true, "ran".into()));
    assert_eq!(
        renderer
            .pipeline_status()
            .features
            .iter()
            .find(|f| f.name == "engine.ssr")
            .unwrap()
            .source,
        "view_override",
        "settings enable the feature"
    );
    close(
        on.pixel(spot.0, spot.1),
        RED,
        0.05,
        "the wall in the authored mirror",
    );
    assert_eq!(on.pixel(wall.0, wall.1), baseline.pixel(wall.0, wall.1));
    assert_ne!(on.pixels, baseline.pixels);

    // Removing the authored settings restores the baseline exactly.
    assert_eq!(hdr(&mut renderer, &target, &plain)?.pixels, baseline.pixels);

    // An explicit render_features switch beats the settings.
    let (_off_dir, off) = extract(json!({
        "screen_space_reflections":{},"render_features":{"engine.ssr":false}}))?;
    assert_eq!(hdr(&mut renderer, &target, &off)?.pixels, baseline.pixels);
    assert_eq!(feature(&renderer, "engine.ssr"), (false, "disabled".into()));

    // A bare `{}` enables the defaults; the reflection appears again.
    let (_default_dir, defaults) = extract(json!({"screen_space_reflections":{}}))?;
    close(
        hdr(&mut renderer, &target, &defaults)?.pixel(spot.0, spot.1),
        RED,
        0.05,
        "default settings",
    );

    // This device was created without a ray-tracing request, so an RT
    // reflection request cannot run: it is withdrawn and SSR keeps the view
    // (with an RT device it would be superseded, see tests/ray_tracing.rs).
    let (_rt_dir, rt) = extract(json!({
        "screen_space_reflections":{},"render_features":{"engine.rt.reflections":true}}))?;
    let with_rt = hdr(&mut renderer, &target, &rt)?;
    assert_eq!(feature(&renderer, "engine.ssr"), (true, "ran".into()));
    assert_eq!(
        feature(&renderer, "engine.rt.reflections"),
        (false, "disabled".into()),
        "the unusable RT request is withdrawn"
    );
    close(
        with_rt.pixel(spot.0, spot.1),
        RED,
        0.05,
        "SSR without an RT device",
    );
    // The report describes the last camera view, so read it before rendering
    // another frame.
    let status = renderer
        .ray_tracing_status()
        .expect("the RT request is reported");
    let reflections = status
        .effects
        .iter()
        .find(|e| e.effect == "reflections")
        .unwrap();
    assert_eq!(reflections.mode, "fallback", "{reflections:?}");
    assert_eq!(reflections.fallback.as_deref(), Some("ssr"));
    assert_eq!(
        with_rt.pixels,
        hdr(&mut renderer, &target, &defaults)?.pixels
    );

    // Resize keeps the reflection on the new pixel grid.
    renderer.resize(128, 96)?;
    let target = OffscreenTarget::new(&renderer.gpu, 128, 96)?;
    let big = [128, 96];
    let (_big_dir, big_frame) = extract_sized(authored, big)?;
    let resized = hdr(&mut renderer, &target, &big_frame)?;
    let spot = reflection(big);
    close(resized.pixel(spot.0, spot.1), RED, 0.06, "after resize");
    Ok(())
}

#[test]
fn invalid_authored_reflection_settings_fail_at_the_renderer_boundary_or_load() {
    let (_dir, mut frame) = match extract(json!({"screen_space_reflections":{}})) {
        Ok(extracted) => extracted,
        Err(error) => panic!("valid settings extract: {error:#}"),
    };
    frame
        .camera3d
        .as_mut()
        .unwrap()
        .camera
        .screen_space_reflections
        .as_mut()
        .unwrap()
        .roughness_cutoff = 0.0;
    assert!(
        frame.camera3d.as_ref().unwrap().camera.validate().is_err(),
        "a zero cutoff is rejected"
    );
}
