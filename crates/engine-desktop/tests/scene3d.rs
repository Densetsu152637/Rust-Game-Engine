use engine_desktop::{LIT_WGSL, OffscreenSprites, RenderLimits, WgpuBackends, camera_matrix};
use engine_project::{
    Camera2D,
    content::{Mesh, Vertex},
    rendering3d::*,
    runtime::RenderFrame,
};
use glam::{Mat4, Vec3};
use std::sync::Arc;

fn scene() -> SceneRenderFrame {
    SceneRenderFrame {
        sprites: RenderFrame {
            camera: Camera2D {
                logical_size: [64, 64],
                ..Default::default()
            },
            draws: vec![],
        },
        camera3d: Some(CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::IDENTITY.to_cols_array_2d(),
        }),
        meshes: vec![],
        materials2d: Default::default(),
    }
}
fn triangle(id: &str, z: f32, tint: [f32; 4]) -> MeshDraw3D {
    MeshDraw3D {
        id: id.into(),
        mesh: Arc::new(Mesh {
            vertices: [[-0.8, -0.8, 0.0], [0.8, -0.8, 0.0], [0.0, 0.8, 0.0]]
                .into_iter()
                .map(|position| Vertex {
                    position,
                    uv: [0.0; 2],
                    color: [1.0; 4],
                })
                .collect(),
            indices: vec![0, 1, 2],
        }),
        texture: None,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, z)).to_cols_array_2d(),
        material: MeshMaterial {
            tint,
            unlit: true,
            ..Default::default()
        },
        shader: None,
    }
}
#[test]
fn camera_depth_and_shader_contract_are_valid() {
    engine_desktop::validate_wgsl(LIT_WGSL).unwrap();
    let s = scene();
    let camera = camera_matrix(s.camera3d.as_ref().unwrap(), 1.0).unwrap();
    let depth = |z| {
        let p = camera * Vec3::new(0.0, 0.0, z).extend(1.0);
        p.z / p.w
    };
    assert!(depth(-0.1).abs() < 1e-5);
    assert!((depth(-1000.0) - 1.0).abs() < 1e-5);
    assert!(depth(-0.05) < 0.0);
}
#[test]
fn neutral_scene_path_rejects_invalid_custom_uniforms() {
    let mut renderer = OffscreenSprites::headless(64, 64, RenderLimits::default()).unwrap();
    let mut s = scene();
    s.meshes.push(triangle("mesh", -2.0, [1.0; 4]));
    assert_eq!(renderer.render_scene(&s).unwrap().draws, 1);
    s.meshes[0].shader = Some(Arc::from(
        LIT_WGSL.replace("@group(2) @binding(0)", "@group(3) @binding(0)"),
    ));
    assert!(
        renderer
            .render_scene(&s)
            .unwrap_err()
            .to_string()
            .contains("unsupported material uniform")
    );
}
#[test]
#[ignore = "requires a native GPU adapter"]
fn gpu_scene_depth_lighting_and_custom_shader_produce_pixels() -> anyhow::Result<()> {
    let mut renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, 64, 64, RenderLimits::default())?;
    eprintln!("{}", renderer.backend_description());
    let mut s = scene();
    s.meshes = vec![
        triangle("near", -2.0, [0.0, 1.0, 0.0, 1.0]),
        triangle("far", -3.0, [1.0, 0.0, 0.0, 1.0]),
    ];
    renderer.render_scene(&s)?;
    let pixel = |bytes: &[u8]| bytes[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4].to_vec();
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 255, 0, 255]);
    s.meshes.reverse();
    renderer.render_scene(&s)?;
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 255, 0, 255]);
    s.meshes.retain(|d| d.id == "near");
    s.meshes[0].material.unlit = false;
    renderer.render_scene(&s)?;
    let lit = pixel(&renderer.read_rgba()?);
    assert!(lit[1] > 100 && lit[1] < 255, "{lit:?}");
    s.meshes[0].shader = Some(Arc::from(
        LIT_WGSL.replace("color.rgb * intensity", "vec3<f32>(0.0, 0.0, 1.0)"),
    ));
    renderer.render_scene(&s)?;
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 0, 255, 255]);
    s.meshes[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, -0.05)).to_cols_array_2d();
    renderer.render_scene(&s)?;
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 0, 0, 255]);
    Ok(())
}
