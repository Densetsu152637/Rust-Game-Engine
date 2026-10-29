//! Render extension API through the desktop renderer: authored per-camera
//! feature overrides, texture streams and XR eye targets. GPU tests are
//! ignored by default; run with `--ignored --test-threads=1`.

use engine_desktop::render_pipeline::ViewId;
use engine_desktop::*;
use engine_project::{
    Camera2D, EditOperation, Project, Runtime, SceneDocument,
    content::{Mesh, Texture, Vertex},
    rendering3d::*,
    runtime::{Draw, DrawSpace, RenderFrame},
};
use glam::{Mat4, Vec3};
use serde_json::json;
use std::{fs, sync::Arc};

const SIZE: u32 = 64;

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

/// A project with a camera at z=4 and a large unlit red triangle at the origin.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -2 -2 0 0 0\nvertex 2 -2 0 1 0\nvertex 0 2 0 0 1\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":[
            {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{},
                "Camera2D":{"logicalSize":[64,64]}}},
            {"id":"mesh","name":"Mesh","components":{"Transform":{"position":[0,0,0]},
                "MeshRenderer":{"mesh":"mesh-asset"},"MeshMaterial":{"tint":[1,0,0,1],"unlit":true}}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"features","startup_scene":"scene.json",
            "assets":[{"id":"mesh-asset","kind":"mesh","path":"mesh.cgmesh"}]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_camera_overrides_disable_features_in_rendering_and_status() -> anyhow::Result<()> {
    let dir = project();
    let mut document = SceneDocument::load(dir.path().join("project.json"))?;
    let mut renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, SIZE, SIZE, RenderLimits::default())?;
    let mut render = |document: &SceneDocument| -> anyhow::Result<(Vec<u8>, RenderPipelineStatus)> {
        let mut project = Project::load(dir.path().join("project.json"))?;
        project.scene = document.snapshot().clone();
        let runtime = Runtime::new(project)?;
        renderer.render_scene(&runtime.extract_scene_frame()?)?;
        let status = renderer.gpu_renderer().unwrap().pipeline_status().clone();
        Ok((renderer.read_rgba()?, status))
    };
    let (pixels, status) = render(&document)?;
    assert_eq!(pixel(&pixels, 32, 32), [255, 0, 0, 255]);
    assert_eq!(status.state, RenderPipelineState::Rendered);
    let opaque = status
        .features
        .iter()
        .find(|f| f.name == "engine.opaque")
        .unwrap();
    assert!(opaque.enabled && opaque.activity == "ran");
    assert_eq!(status.draws, 1);

    // The MCP `component_set` edit of the camera component.
    let revision = document.revision().to_owned();
    document.apply(
        EditOperation::SetComponent {
            entity_id: "camera".into(),
            component: "Camera3D".into(),
            value: json!({"render_features": {"engine.opaque": false, "game.missing": true}}),
            merge: true,
        },
        &revision,
    )?;
    let (pixels, status) = render(&document)?;
    assert_eq!(
        pixel(&pixels, 32, 32),
        [0, 0, 0, 255],
        "the mesh is not drawn"
    );
    let opaque = status
        .features
        .iter()
        .find(|f| f.name == "engine.opaque")
        .unwrap();
    assert!(!opaque.enabled);
    assert_eq!(
        (opaque.source.as_str(), opaque.activity.as_str()),
        ("view_override", "disabled")
    );
    assert_eq!(status.unknown_overrides, ["game.missing"]);
    assert_eq!(status.overrides.get("engine.opaque"), Some(&false));
    Ok(())
}

fn quad_mesh() -> Arc<Mesh> {
    Arc::new(Mesh {
        normals: vec![],
        vertices: [
            ([-3.0, -3.0, 0.0], [0.0, 1.0]),
            ([3.0, -3.0, 0.0], [1.0, 1.0]),
            ([3.0, 3.0, 0.0], [1.0, 0.0]),
            ([-3.0, 3.0, 0.0], [0.0, 0.0]),
        ]
        .into_iter()
        .map(|(position, uv)| Vertex {
            position,
            uv,
            color: [1.0; 4],
        })
        .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}

fn camera() -> CameraInstance3D {
    CameraInstance3D {
        camera: Camera3D::default(),
        world: Mat4::from_translation(Vec3::new(0.0, 0.0, 4.0)).to_cols_array_2d(),
    }
}

/// A scene whose full-screen quad samples `texture` (a stream frame).
fn streamed_scene(stream: &StreamFrame) -> SceneRenderFrame {
    SceneRenderFrame {
        indirect: None,
        applied_origin: [0.0; 3],
        sprites: RenderFrame {
            camera: Camera2D {
                logical_size: [64, 64],
                ..Default::default()
            },
            draws: vec![],
        },
        camera3d: Some(camera()),
        meshes: vec![MeshDraw3D {
            id: "screen".into(),
            mesh: quad_mesh(),
            texture: Some(stream.texture.clone()),
            metallic_roughness_texture: None,
            normal_texture: None,
            model: Mat4::IDENTITY.to_cols_array_2d(),
            material: MeshMaterial {
                unlit: true,
                ..Default::default()
            },
            shader: None,
        }],
        lights: vec![],
        post: Default::default(),
        ray_tracing: None,
        materials2d: Default::default(),
        lighting2d: None,
        streams: vec![stream.clone()],
    }
}

fn solid(color: [u8; 4], count: usize) -> Vec<u8> {
    color.repeat(count)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn streamed_textures_change_material_and_sprite_pixels_across_frames() -> anyhow::Result<()> {
    let mut streams = TextureStreams::default();
    streams.create_cpu("video", 2, 2)?;
    streams.create_external("camera", 2, 2)?;
    let mut offscreen =
        OffscreenSprites::wgpu(WgpuBackends::Primary, SIZE, SIZE, RenderLimits::default())?;

    // CPU uploads: each new revision is written into the same texture.
    let mut seen = Vec::new();
    for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
        streams.upload_rgba("video", solid(color, 4))?;
        let frame = streamed_scene(streams.get("video").unwrap());
        let stats = offscreen.render_scene(&frame)?;
        seen.push((
            pixel(&offscreen.read_rgba()?, 32, 32),
            stats.texture_uploads,
        ));
    }
    assert_eq!(seen[0].0, [255, 0, 0, 255]);
    assert_eq!(seen[1].0, [0, 255, 0, 255]);
    assert_eq!(
        (seen[0].1, seen[1].1),
        (0, 0),
        "streams are not re-uploaded as textures"
    );

    // External: the caller writes into the renderer-owned texture handle.
    let frame = streamed_scene(streams.get("camera").unwrap());
    for color in [[0, 0, 255, 255], [255, 255, 0, 255]] {
        let renderer = offscreen.gpu_renderer().unwrap();
        let texture = renderer.stream_texture("camera", [2, 2])?;
        texture.write(&renderer.gpu.queue, &solid(color, 4))?;
        offscreen.render_scene(&frame)?;
        assert_eq!(pixel(&offscreen.read_rgba()?, 32, 32), color);
    }

    // Sprites/UI sample the same streams.
    let ui = |texture: &Arc<Texture>| Draw {
        id: "ui".into(),
        mesh: Arc::new(
            engine_project::content::sprite_quad(
                [32.0, 32.0, 0.0],
                [64.0, 64.0],
                [0.5, 0.5],
                [0, 0, 2, 2],
                texture,
                [1.0; 4],
            )
            .unwrap(),
        ),
        texture: Some(texture.clone()),
        layer: 0,
        space: DrawSpace::Ui,
        clip: None,
        state: Default::default(),
        motion: None,
        offset: [0.0; 3],
    };
    for name in ["video", "camera"] {
        let stream = streams.get(name).unwrap().clone();
        let mut frame = streamed_scene(&stream);
        frame.camera3d = None;
        frame.meshes.clear();
        frame.sprites.draws.push(ui(&stream.texture));
        offscreen.render_scene(&frame)?;
        let expected = if name == "video" {
            [0, 255, 0, 255]
        } else {
            [255, 255, 0, 255]
        };
        assert_eq!(pixel(&offscreen.read_rgba()?, 32, 32), expected, "{name}");
    }
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn xr_eyes_render_into_external_targets_with_their_own_views() -> anyhow::Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let output = OffscreenTarget::new(&gpu, SIZE, SIZE)?;
    let mut renderer = GpuRenderer::new(gpu.clone(), output.info, RenderLimits::default())?;
    let red = Arc::new(Mesh {
        normals: vec![],
        vertices: [
            [-0.2, -0.2, 0.0],
            [0.2, -0.2, 0.0],
            [0.2, 0.2, 0.0],
            [-0.2, 0.2, 0.0],
        ]
        .into_iter()
        .map(|position| Vertex {
            position,
            uv: [0.0; 2],
            color: [1.0; 4],
        })
        .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    });
    let frame = SceneRenderFrame {
        indirect: None,
        applied_origin: [0.0; 3],
        sprites: RenderFrame {
            camera: Camera2D::default(),
            draws: vec![],
        },
        camera3d: Some(camera()),
        meshes: vec![MeshDraw3D {
            id: "red".into(),
            mesh: red,
            texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            model: Mat4::IDENTITY.to_cols_array_2d(),
            material: MeshMaterial {
                tint: [1.0, 0.0, 0.0, 1.0],
                unlit: true,
                ..Default::default()
            },
            shader: None,
        }],
        lights: vec![],
        post: Default::default(),
        ray_tracing: None,
        materials2d: Default::default(),
        lighting2d: None,
        streams: vec![],
    };
    // Asymmetric per-eye projections like OpenXR's (wider toward the nose).
    let projection = |left: f32, right: f32| {
        let near = 0.1;
        glam::camera::rh::proj::directx::frustum(
            left * near,
            right * near,
            -0.5 * near,
            0.5 * near,
            near,
            100.0,
        )
    };
    let targets = [
        OffscreenTarget::new(&gpu, SIZE, SIZE)?,
        OffscreenTarget::new(&gpu, SIZE, SIZE)?,
    ];
    let eyes = [
        EyeTarget {
            view: ViewId::LEFT_EYE,
            world_from_view: Mat4::from_translation(Vec3::new(-0.032, 0.0, 2.0)),
            clip_from_view: projection(-0.5, 0.5),
            target: &targets[0].view,
            format: targets[0].info.format,
            size: [SIZE, SIZE],
        },
        EyeTarget {
            view: ViewId::RIGHT_EYE,
            world_from_view: Mat4::from_translation(Vec3::new(0.032, 0.0, 2.0)),
            clip_from_view: projection(-0.5, 0.5),
            target: &targets[1].view,
            format: targets[1].info.format,
            size: [SIZE, SIZE],
        },
    ];
    let stats = renderer.render_eyes(&frame, &eyes, None)?;
    assert_eq!(stats.len(), 2);
    assert!(stats.iter().all(|s| s.draws == 1));
    let centroid = |pixels: &[u8]| {
        let (mut sum, mut count) = (0.0, 0.0);
        for y in 0..SIZE {
            for x in 0..SIZE {
                if pixel(pixels, x, y) == [255, 0, 0, 255] {
                    sum += f64::from(x);
                    count += 1.0;
                }
            }
        }
        assert!(count > 0.0, "the object is visible in each eye");
        sum / count
    };
    let left = centroid(&targets[0].read_rgba(&gpu)?);
    let right = centroid(&targets[1].read_rgba(&gpu)?);
    // A point at distance 2 shifts by focal * baseline / depth pixels:
    // focal = 32 px / tan-half 0.5 = 64 px; baseline 0.064 m -> 2.05 px.
    let expected = 64.0 * 0.064 / 2.0;
    assert!(
        ((left - right) - expected).abs() < 0.75,
        "disparity {} px, expected {expected}",
        left - right
    );
    // Eye views do not overwrite the main camera's status.
    assert_eq!(
        renderer.pipeline_status().state,
        RenderPipelineState::NotRendered
    );
    Ok(())
}
