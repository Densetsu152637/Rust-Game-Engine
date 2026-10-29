//! End-to-end authored project -> runtime extraction -> desktop GPU HDR readback.
//! Run explicitly on a real adapter with --ignored --test-threads=1.
use anyhow::Result;
use engine_desktop::*;
use engine_project::{Project, Runtime, rendering3d::SceneRenderFrame, world::coordinates};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};
use std::fs;

fn constant(value: [f32; 3]) -> Value {
    let mut coefficients = vec![[0.0; 3]; 9];
    coefficients[0] = value;
    json!(coefficients)
}

fn authored_project(base: f64, rotated: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut environments = Vec::new();
    let colors = [[4.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 4.0]];
    for (index, color) in colors.into_iter().enumerate() {
        let mut faces = Vec::new();
        for face in 0..6 {
            let file = format!("environment-{index}-face-{face}.rgb32f");
            fs::write(dir.path().join(&file), [0u8; 12]).unwrap();
            faces.push(file);
        }
        let mut diffuse = constant(color);
        if rotated && index == 0 {
            diffuse[0] = json!([2.0, 0.0, 0.0]);
            diffuse[1] = json!([2.0, 0.0, 0.0]);
        }
        environments.push(json!({"edge":1,"diffuse":diffuse,"mips":[faces]}));
    }
    let rotation = if rotated {
        [
            0.0,
            -std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        ]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let probes = if rotated {
        vec![]
    } else {
        vec![
            json!({"id":"lower","positionAbsolute":[base,0.0,0.0],"scale":[1,1,1],
            "halfExtents":[1,1,1],"blendDistance":0.5,"priority":1,
            "lighting":{"environment":1,"rotation":[0,0,0,1],"intensity":1}}),
            json!({"id":"higher","positionAbsolute":[base,0.0,0.0],"scale":[1,1,1],
            "halfExtents":[1,1,1],"blendDistance":0.5,"priority":2,
            "lighting":{"environment":2,"rotation":[0,0,0,1],"intensity":1}}),
        ]
    };
    fs::write(
        dir.path().join("sky.json"),
        serde_json::to_vec(&json!({
            "version":1,"environments":environments,
            "global":{"environment":0,"rotation":rotation,"intensity":1},"probes":probes
        }))
        .unwrap(),
    )
    .unwrap();
    let mesh = json!({
        "vertices":[
            {"position":[-0.2,-0.2,0],"uv":[0,0],"color":[1,1,1,1]},
            {"position":[0.2,-0.2,0],"uv":[1,0],"color":[1,1,1,1]},
            {"position":[0.2,0.2,0],"uv":[1,1],"color":[1,1,1,1]},
            {"position":[-0.2,0.2,0],"uv":[0,1],"color":[1,1,1,1]}],
        "normals":[[0,0,1],[0,0,1],[0,0,1],[0,0,1]],
        "indices":[0,1,2,0,2,3]
    });
    fs::write(
        dir.path().join("panel.json"),
        serde_json::to_vec(&mesh).unwrap(),
    )
    .unwrap();
    let positions = if rotated {
        vec![0.0]
    } else {
        vec![-1.5, 0.0, 0.75]
    };
    let mut entities = vec![json!({"id":"camera","name":"Camera","components":{
        "Transform":{"position":[0,0,3]},
        "Camera2D":{"logicalSize":[64,64]},
        "Camera3D":{"projection":{"kind":"orthographic","height":4,"near":0.1,"far":10},
            "lighting":{"ambient":0,"exposure":1,"indirect":"sky"}}
    }})];
    for (i, x) in positions.into_iter().enumerate() {
        entities.push(
            json!({"id":format!("panel-{i}"),"name":"Panel","components":{
                "Transform":{"position":[x,0,0]},"MeshRenderer":{"mesh":"panel"},
                "MeshMaterial":{"metallic":0,"roughness":1}
            }}),
        );
    }
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({
            "schema":1,"scene_id":"authored-ibl","entities":entities
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"authored-ibl","startup_scene":"scene.json",
            "assets":[{"id":"sky","path":"sky.json","kind":"indirect"},
                      {"id":"panel","path":"panel.json","kind":"mesh"}]
        }))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn frame(dir: &tempfile::TempDir, base: f64) -> Result<(Runtime, SceneRenderFrame)> {
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<coordinates::WorldOrigin>()
        .offset = [base, 0.0, 0.0];
    runtime.start()?;
    let frame = runtime.extract_scene_frame()?;
    assert_eq!(frame.applied_origin[0], base);
    assert!(
        frame.indirect.is_some(),
        "camera resolved its catalog IBL asset"
    );
    Ok((runtime, frame))
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

#[track_caller]
fn close(actual: [f32; 4], expected: [f32; 3], tolerance: f32) {
    for channel in 0..3 {
        assert!(
            (actual[channel] - expected[channel]).abs() < tolerance,
            "actual {actual:?}, expected {expected:?}, tolerance {tolerance}"
        );
    }
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_hdr_priority_blend_disable_resize_and_forget() -> Result<()> {
    let dir = authored_project(0.0, false);
    let (_runtime, frame) = frame(&dir, 0.0)?;
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let image = hdr(&mut renderer, &target, &frame)?;
    close(image.pixel(8, 32), [3.84, 0.0, 0.0], 0.04);
    close(image.pixel(32, 32), [0.0, 0.0, 3.84], 0.04);
    close(image.pixel(44, 32), [1.92, 0.0, 1.92], 0.04);

    let mut disabled = frame.clone();
    disabled.camera3d.as_mut().unwrap().camera.lighting.indirect = None;
    disabled.indirect = None;
    let off = hdr(&mut renderer, &target, &disabled)?;
    close(off.pixel(32, 32), [0.0; 3], 0.01);
    close(
        hdr(&mut renderer, &target, &frame)?.pixel(32, 32),
        [0.0, 0.0, 3.84],
        0.04,
    );

    let eye_targets = [
        OffscreenTarget::new(&renderer.gpu, 64, 64)?,
        OffscreenTarget::new(&renderer.gpu, 64, 64)?,
    ];
    let projection = glam::camera::rh::proj::directx::orthographic(-2.0, 2.0, -2.0, 2.0, 0.1, 10.0);
    let eyes = [
        EyeTarget {
            view: render_pipeline::ViewId::LEFT_EYE,
            world_from_view: Mat4::from_translation(Vec3::new(0.0, 0.0, 3.0)),
            clip_from_view: projection,
            target: &eye_targets[0].view,
            format: eye_targets[0].info.format,
            size: [64, 64],
        },
        EyeTarget {
            view: render_pipeline::ViewId::RIGHT_EYE,
            world_from_view: Mat4::from_translation(Vec3::new(0.0, 0.0, 3.0)),
            clip_from_view: projection,
            target: &eye_targets[1].view,
            format: eye_targets[1].info.format,
            size: [64, 64],
        },
    ];
    renderer.render_eyes(&frame, &eyes, None)?;
    for view in [
        render_pipeline::ViewId::LEFT_EYE,
        render_pipeline::ViewId::RIGHT_EYE,
    ] {
        close(
            renderer.pipeline()?.read_scene_hdr(view)?.pixel(32, 32),
            [0.0, 0.0, 3.84],
            0.04,
        );
    }
    renderer.render_eyes(&disabled, &eyes, None)?;
    renderer.render_eyes(&frame, &eyes[..1], None)?;
    close(
        renderer
            .pipeline()?
            .read_scene_hdr(render_pipeline::ViewId::LEFT_EYE)?
            .pixel(32, 32),
        [0.0, 0.0, 3.84],
        0.04,
    );
    close(
        renderer
            .pipeline()?
            .read_scene_hdr(render_pipeline::ViewId::RIGHT_EYE)?
            .pixel(32, 32),
        [0.0; 3],
        0.01,
    );

    renderer.resize(96, 96)?;
    let resized = OffscreenTarget::new(&renderer.gpu, 96, 96)?;
    let resized_image = hdr(&mut renderer, &resized, &frame)?;
    close(
        resized_image.pixel(resized_image.size[0] / 2, resized_image.size[1] / 2),
        [0.0, 0.0, 3.84],
        0.04,
    );
    renderer.forget_view(render_pipeline::ViewId::MAIN);
    let forgotten_image = hdr(&mut renderer, &resized, &frame)?;
    close(
        forgotten_image.pixel(forgotten_image.size[0] / 2, forgotten_image.size[1] / 2),
        [0.0, 0.0, 3.84],
        0.04,
    );
    renderer.gpu.check_errors()
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_sh_rotation_and_rebase_keep_absolute_probe_placement() -> Result<()> {
    let rotated = authored_project(0.0, true);
    let (_runtime, rotated_frame) = frame(&rotated, 0.0)?;
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let rotated_value = hdr(&mut renderer, &target, &rotated_frame)?.pixel(32, 32)[0];
    assert!(
        rotated_value > 3.7 && rotated_value < 3.9,
        "rotated SH x coefficient: {rotated_value}"
    );

    let far = authored_project(999_999_995_904.0, false);
    let (mut runtime, frame) = frame(&far, 999_999_995_904.0)?;
    close(
        hdr(&mut renderer, &target, &frame)?.pixel(32, 32),
        [0.0, 0.0, 3.84],
        0.04,
    );
    coordinates::rebase_now(runtime.app_mut().world_mut(), [999_999_997_952.0, 0.0, 0.0])?;
    let rebased = runtime.extract_scene_frame()?;
    assert_eq!(rebased.applied_origin[0], 999_999_997_952.0);
    close(
        hdr(&mut renderer, &target, &rebased)?.pixel(32, 32),
        [0.0, 0.0, 3.84],
        0.04,
    );
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<coordinates::WorldOrigin>()
        .offset = [999_999_999_000.0, 0.0, 0.0];
    let pending = runtime.extract_scene_frame()?;
    assert_eq!(pending.applied_origin[0], 999_999_997_952.0);
    close(
        hdr(&mut renderer, &target, &pending)?.pixel(32, 32),
        [0.0, 0.0, 3.84],
        0.04,
    );
    renderer.gpu.check_errors()
}
