//! End-to-end authored decals: project JSON (an unlit grey floor and
//! `Camera3D.decals`) -> runtime extraction -> desktop GPU HDR readback,
//! including removal, the explicit feature switch, resize and a rejected
//! invalid decal.
//! Run explicitly on a real adapter with --ignored --test-threads=1.
use anyhow::Result;
use engine_desktop::*;
use engine_project::{Project, Runtime, rendering3d::SceneRenderFrame};
use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};
use std::f32::consts::FRAC_PI_2;
use std::fs;

const EYE: Vec3 = Vec3::new(0.0, 6.0, 0.0);
const GREY: f32 = 0.5;

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
    // A camera looking straight down.
    let rotation = Quat::from_rotation_x(-FRAC_PI_2);
    let entities = json!([
        {"id":"camera","name":"Camera","components":{
            "Transform":{"position":[EYE.x,EYE.y,EYE.z],"rotation":[rotation.x,rotation.y,rotation.z,rotation.w]},
            "Camera2D":{"logicalSize":size},
            "Camera3D":camera3d}},
        {"id":"floor","name":"Floor","components":{
            "Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"floor"},
            "MeshMaterial":{"unlit":true,"tint":[GREY,GREY,GREY,1]}}}
    ]);
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"decals","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-decals",
            "startup_scene":"scene.json","assets":[
                {"id":"floor","path":"floor.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn extract_sized(camera3d: Value, size: [u32; 2]) -> Result<(tempfile::TempDir, SceneRenderFrame)> {
    let dir = project(camera3d, size);
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime.start()?;
    let frame = runtime.extract_scene_frame()?;
    Ok((dir, frame))
}

fn extract(camera3d: Value) -> Result<(tempfile::TempDir, SceneRenderFrame)> {
    extract_sized(camera3d, [96, 72])
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
        Mat4::from_translation(EYE) * Mat4::from_quat(Quat::from_rotation_x(-FRAC_PI_2));
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

fn feature(renderer: &GpuRenderer, name: &str) -> (bool, String) {
    let f = renderer
        .pipeline_status()
        .features
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} is not reported"));
    (f.enabled, f.activity.clone())
}

/// A 2 m decal over the origin facing +Y: local +Z rotated onto world +Y is a
/// quarter turn about -X.
fn red_decal() -> Value {
    let q = Quat::from_rotation_arc(Vec3::Z, Vec3::Y);
    json!({"position":[0,0,0],"rotation":[q.x,q.y,q.z,q.w],"half_extents":[1,1,1],
        "albedo":[1,0,0],"opacity":1.0,"edge_fade":0.0,"angle_fade":0.0})
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_decals_reach_pixels_end_to_end() -> Result<()> {
    let (_plain_dir, plain) = extract(json!({}))?;
    let (_dir, frame) = extract(json!({"decals":[red_decal()]}))?;
    let camera = &frame.camera3d.as_ref().unwrap().camera;
    assert_eq!(camera.decals.len(), 1, "authored decals reach the frame");
    assert!(plain.camera3d.as_ref().unwrap().camera.decals.is_empty());

    let size = [96, 72];
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, size[0], size[1])?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let center = pixel_of(Vec3::ZERO, size);
    let outside = pixel_of(Vec3::new(3.0, 0.0, 2.0), size);

    let baseline = hdr(&mut renderer, &target, &plain)?;
    assert_eq!(
        feature(&renderer, "engine.decals"),
        (false, "disabled".into())
    );
    let base = baseline.pixel(center.0, center.1);
    assert!(base[0] > 0.1 && base[1] > 0.1, "lit grey floor: {base:?}");

    let on = hdr(&mut renderer, &target, &frame)?;
    assert_eq!(feature(&renderer, "engine.decals"), (true, "ran".into()));
    assert_eq!(
        renderer
            .pipeline_status()
            .features
            .iter()
            .find(|f| f.name == "engine.decals")
            .unwrap()
            .source,
        "view_override",
        "authored decals enable the feature"
    );
    let decorated = on.pixel(center.0, center.1);
    assert!(
        (decorated[0] - base[0]).abs() < 0.02 && decorated[1] < 0.01 && decorated[2] < 0.01,
        "decorated {decorated:?} vs base {base:?}"
    );
    assert_eq!(
        on.pixel(outside.0, outside.1),
        baseline.pixel(outside.0, outside.1),
        "outside the volume"
    );
    assert_eq!(on.pixel(0, 0), baseline.pixel(0, 0));
    assert_ne!(on.pixels, baseline.pixels);

    // Removing the decals restores the baseline exactly.
    assert_eq!(hdr(&mut renderer, &target, &plain)?.pixels, baseline.pixels);

    // An explicit render_features switch beats the authored decals.
    let (_off_dir, off) = extract(json!({
        "decals":[red_decal()],"render_features":{"engine.decals":false}}))?;
    assert_eq!(hdr(&mut renderer, &target, &off)?.pixels, baseline.pixels);
    assert_eq!(
        feature(&renderer, "engine.decals"),
        (false, "disabled".into())
    );

    // Resize keeps the decal on the new pixel grid.
    renderer.resize(128, 96)?;
    let target = OffscreenTarget::new(&renderer.gpu, 128, 96)?;
    let big = [128, 96];
    let (_big_dir, big_frame) = extract_sized(json!({"decals":[red_decal()]}), big)?;
    let resized = hdr(&mut renderer, &target, &big_frame)?;
    assert_eq!(resized.size, [128, 96]);
    let center = pixel_of(Vec3::ZERO, big);
    let p = resized.pixel(center.0, center.1);
    assert!(p[1] < 0.01 && p[0] > 0.1, "after resize: {p:?}");

    // An invalid decal that bypassed authoring validation is rejected before
    // any GPU work.
    let (_bad_dir, mut bad) = extract(json!({"decals":[red_decal()]}))?;
    bad.camera3d.as_mut().unwrap().camera.decals[0].opacity = 2.0;
    assert!(renderer.render_scene(&bad, &target.view, None).is_err());
    Ok(())
}

#[test]
fn invalid_authored_decals_fail_at_the_validation_boundary() {
    let (_dir, mut frame) = match extract(json!({"decals":[red_decal()]})) {
        Ok(extracted) => extracted,
        Err(error) => panic!("valid decals extract: {error:#}"),
    };
    let camera = &mut frame.camera3d.as_mut().unwrap().camera;
    camera.decals[0].half_extents = [1.0, 0.0, 1.0];
    assert!(camera.validate().is_err(), "a zero extent is rejected");
}
