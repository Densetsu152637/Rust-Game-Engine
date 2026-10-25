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
        streams: vec![],
    }
}
fn triangle(id: &str, z: f32, tint: [f32; 4]) -> MeshDraw3D {
    MeshDraw3D {
        id: id.into(),
        mesh: Arc::new(Mesh {
            normals: vec![],
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
        metallic_roughness_texture: None,
        normal_texture: None,
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
    use engine_desktop::render_pipeline::{MaterialTarget, compose_material};
    compose_material(LIT_WGSL, MaterialTarget::Mesh).unwrap();
    compose_material(LIT_WGSL, MaterialTarget::Sprite).unwrap();
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
fn small_shadow_volumes_at_large_world_coordinates_remain_finite() {
    let mut renderer = OffscreenSprites::headless(64, 64, RenderLimits::default()).unwrap();
    let mut s = scene();
    s.meshes.push(triangle("mesh", -2.0, [1.0; 4]));
    let light = &mut s.camera3d.as_mut().unwrap().camera.lighting;
    light.direction = [1.0, 1.0, 1.0];
    light.shadows.enabled = true;
    light.shadows.center = [1_000_000.0; 3];
    light.shadows.depth = 0.1;
    // The neutral backend validates 3D scenes without drawing them.
    renderer.render_scene(&s).unwrap();
    let shadow = engine_desktop::render_pipeline::ShadowView::directional(
        Vec3::ONE,
        Vec3::splat(1_000_000.0),
        20.0,
        0.1,
        1024,
        0.001,
    );
    assert!(shadow.clip_from_world.is_finite());
    let projected = shadow.clip_from_world * Vec3::splat(1_000_000.0).extend(1.0);
    assert!(projected.is_finite() && projected.w != 0.0, "{projected}");
}
#[test]
fn scene_validation_rejects_shaders_breaking_the_material_contract() {
    let mut renderer = OffscreenSprites::headless(64, 64, RenderLimits::default()).unwrap();
    let mut s = scene();
    s.meshes.push(triangle("mesh", -2.0, [1.0; 4]));
    renderer.render_scene(&s).unwrap();
    s.meshes[0].shader = Some(Arc::from(format!(
        "{LIT_WGSL}\n@group(3) @binding(1) var<uniform> extra: vec4<f32>;"
    )));
    let error = format!("{:#}", renderer.render_scene(&s).unwrap_err());
    assert!(
        error.contains("unsupported material binding (group 3, binding 1)"),
        "{error}"
    );
}
#[test]
fn failed_material_edits_do_not_accumulate_gpu_resources() {
    use engine_project::runtime::{Draw, DrawSpace};
    let backend = engine_render::headless::RecordingBackend::new();
    let log = backend.log();
    let mut renderer =
        OffscreenSprites::new(Box::new(backend), 64, 64, RenderLimits::default()).unwrap();
    let baseline = log.live_resources();
    let mut frame = scene();
    frame.camera3d = None;
    for id in ["a", "b"] {
        frame.sprites.draws.push(Draw {
            id: id.into(),
            mesh: triangle(id, 0.0, [1.0; 4]).mesh,
            texture: None,
            layer: 0,
            space: DrawSpace::World,
            clip: None,
            state: Default::default(),
            motion: None,
            offset: [0.0; 3],
        });
    }
    frame.materials2d.insert(
        "b".into(),
        MaterialSnapshot {
            material: MeshMaterial::default(),
            shader: Some(Arc::from("invalid shader")),
        },
    );
    for revision in 0..4 {
        frame.materials2d.insert(
            "a".into(),
            MaterialSnapshot {
                material: MeshMaterial::default(),
                shader: Some(Arc::from(format!("{LIT_WGSL}\n// revision {revision}"))),
            },
        );
        assert!(renderer.render_scene(&frame).is_err());
        assert_eq!(log.live_resources(), baseline);
    }
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
    s.meshes[0].shader = Some(Arc::from(LIT_WGSL.replace(
        "vec4<f32>(result, color.a)",
        "vec4<f32>(0.0, 0.0, 1.0, 1.0)",
    )));
    renderer.render_scene(&s)?;
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 0, 255, 255]);
    s.meshes[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, -0.05)).to_cols_array_2d();
    renderer.render_scene(&s)?;
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 0, 0, 255]);
    // The same custom material must execute for legacy 2D submissions too.
    let draw = s.meshes.pop().unwrap();
    s.camera3d = None;
    s.sprites.draws.push(engine_project::runtime::Draw {
        id: "flat".into(),
        mesh: draw.mesh,
        texture: None,
        layer: 0,
        space: engine_project::runtime::DrawSpace::World,
        clip: None,
        state: Default::default(),
        motion: None,
        offset: [0.0; 3],
    });
    s.materials2d.insert(
        "flat".into(),
        MaterialSnapshot {
            material: draw.material,
            shader: draw.shader,
        },
    );
    renderer.render_scene(&s)?;
    assert_eq!(pixel(&renderer.read_rgba()?), [0, 0, 255, 255]);
    Ok(())
}
fn quad(id: &str, z: f32, half: f32) -> MeshDraw3D {
    let mut draw = triangle(id, z, [0.65, 0.25, 0.1, 1.0]);
    draw.material.unlit = false;
    draw.mesh = Arc::new(Mesh {
        vertices: [
            ([-half, -half, 0.0], [0.0, 0.0]),
            ([half, -half, 0.0], [1.0, 0.0]),
            ([half, half, 0.0], [1.0, 1.0]),
            ([-half, half, 0.0], [0.0, 1.0]),
        ]
        .into_iter()
        .map(|(position, uv)| Vertex {
            position,
            uv,
            color: [1.0; 4],
        })
        .collect(),
        normals: vec![[0.0, 0.0, 1.0]; 4],
        indices: vec![0, 1, 2, 0, 2, 3],
    });
    draw
}
fn texel(pixel: [u8; 4]) -> Arc<engine_project::content::Texture> {
    Arc::new(engine_project::content::Texture {
        width: 1,
        height: 1,
        pixels: pixel.to_vec(),
    })
}
#[test]
#[ignore = "requires a native GPU adapter"]
fn gpu_pbr_factors_and_linear_data_textures_change_pixels() -> anyhow::Result<()> {
    let mut renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, 64, 64, RenderLimits::default())?;
    let mut s = scene();
    s.camera3d.as_mut().unwrap().camera.lighting.direction = [0.3, 0.0, 1.0];
    s.meshes.push(quad("surface", -3.0, 1.5));
    s.meshes[0].material.roughness = 1.0;
    renderer.render_scene(&s)?;
    let dielectric = renderer.read_rgba()?;
    s.meshes[0].material.metallic = 1.0;
    renderer.render_scene(&s)?;
    let metal = renderer.read_rgba()?;
    assert!(
        dielectric
            .iter()
            .zip(&metal)
            .filter(|(a, b)| a.abs_diff(**b) > 15)
            .count()
            > 100
    );
    s.meshes[0].material.roughness = 0.1;
    renderer.render_scene(&s)?;
    let smooth = renderer.read_rgba()?;
    assert!(
        smooth
            .iter()
            .zip(&metal)
            .filter(|(a, b)| a.abs_diff(**b) > 15)
            .count()
            > 100
    );
    s.meshes[0].material.roughness = 1.0;
    s.meshes[0].metallic_roughness_texture = Some(texel([0, 255, 0, 255]));
    renderer.render_scene(&s)?;
    assert_eq!(
        renderer.read_rgba()?,
        dielectric,
        "blue=0 produces dielectric and green=255 preserves roughness"
    );
    s.meshes[0].normal_texture = Some(texel([255, 128, 128, 255]));
    renderer.render_scene(&s)?;
    let normal = renderer.read_rgba()?;
    assert!(
        normal
            .iter()
            .zip(&dielectric)
            .filter(|(a, b)| a.abs_diff(**b) > 15)
            .count()
            > 100
    );
    // Values must be interpreted linearly, not sRGB decoded: compare a mid-green map with the same scalar.
    s.meshes[0].normal_texture = None;
    s.meshes[0].metallic_roughness_texture = Some(texel([0, 128, 255, 255]));
    renderer.render_scene(&s)?;
    let mapped = renderer.read_rgba()?;
    s.meshes[0].metallic_roughness_texture = None;
    s.meshes[0].material.roughness = 128.0 / 255.0;
    renderer.render_scene(&s)?;
    assert!(
        renderer.read_rgba()? == mapped,
        "midrange data texture stays linear"
    );
    // A nonuniform model scale must transform authored smooth normals with inverse transpose.
    let mesh = Arc::make_mut(&mut s.meshes[0].mesh);
    mesh.normals = vec![[0.70710677, 0.0, 0.70710677]; 4];
    s.meshes[0].model = Mat4::from_scale_rotation_translation(
        Vec3::new(2.0, 1.0, 1.0),
        glam::Quat::IDENTITY,
        Vec3::new(0.0, 0.0, -3.0),
    )
    .to_cols_array_2d();
    renderer.render_scene(&s)?;
    let transformed = renderer.read_rgba()?;
    let mesh = Arc::make_mut(&mut s.meshes[0].mesh);
    for vertex in &mut mesh.vertices {
        vertex.position[0] *= 2.0;
    }
    mesh.normals = vec![Vec3::new(0.5, 0.0, 1.0).normalize().to_array(); 4];
    s.meshes[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, -3.0)).to_cols_array_2d();
    renderer.render_scene(&s)?;
    assert!(
        renderer
            .read_rgba()?
            .iter()
            .zip(&transformed)
            .all(|(a, b)| a.abs_diff(*b) <= 1),
        "inverse transpose equals baked normal transform"
    );
    Arc::make_mut(&mut s.meshes[0].mesh).normals.clear();
    renderer.render_scene(&s)?;
    assert!(
        renderer
            .read_rgba()?
            .iter()
            .zip(&transformed)
            .filter(|(a, b)| a.abs_diff(**b) > 10)
            .count()
            > 100,
        "smooth normals affect GPU lighting"
    );
    Ok(())
}
#[test]
#[ignore = "requires a native GPU adapter"]
fn gpu_shadow_occlusion_cast_receive_and_resolution_are_observable() -> anyhow::Result<()> {
    let mut renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, 64, 64, RenderLimits::default())?;
    let mut s = scene();
    let lighting = &mut s.camera3d.as_mut().unwrap().camera.lighting;
    lighting.direction = [0.8, 0.0, 1.0];
    lighting.shadows = ShadowSettings {
        resolution: 256,
        center: [0.0, 0.0, -3.0],
        extent: 6.0,
        depth: 10.0,
        ..Default::default()
    };
    s.meshes = vec![quad("receiver", -3.0, 1.5), quad("occluder", -2.0, 0.3)];
    renderer.render_scene(&s)?;
    let baseline = renderer.read_rgba()?;
    s.camera3d.as_mut().unwrap().camera.lighting.shadows.enabled = true;
    renderer.render_scene(&s)?;
    let shadowed = renderer.read_rgba()?;
    let darker = baseline
        .as_chunks::<4>()
        .0
        .iter()
        .zip(shadowed.as_chunks::<4>().0.iter())
        .filter(|(a, b)| a[0] > b[0].saturating_add(25))
        .count();
    assert!(darker > 20, "shadow darkened {darker} pixels");
    s.meshes[1].material.cast_shadows = false;
    renderer.render_scene(&s)?;
    assert!(
        renderer.read_rgba()? == baseline,
        "disabling the occluder restores receiver lighting without self-shadow acne"
    );
    s.meshes[1].material.cast_shadows = true;
    s.meshes[0].material.receive_shadows = false;
    renderer.render_scene(&s)?;
    assert!(
        renderer.read_rgba()? == baseline,
        "receiver can opt out of shadows"
    );
    s.meshes[0].material.receive_shadows = true;
    s.camera3d
        .as_mut()
        .unwrap()
        .camera
        .lighting
        .shadows
        .resolution = 512;
    renderer.render_scene(&s)?;
    let resized = renderer.read_rgba()?;
    assert_ne!(resized, baseline);
    // Save inspectable evidence only when explicitly requested by the test invoker.
    if let Some(path) = std::env::var_os("ENGINE_PBR_CAPTURE") {
        image::save_buffer(path, &resized, 64, 64, image::ColorType::Rgba8)?;
    }
    Ok(())
}
#[test]
fn pbr_texture_budgets_and_normal_arrays_fail_before_upload() {
    let mut renderer = OffscreenSprites::headless(
        64,
        64,
        RenderLimits {
            max_texture_bytes: 4,
            ..Default::default()
        },
    )
    .unwrap();
    let mut s = scene();
    s.meshes.push(quad("surface", -3.0, 1.0));
    s.meshes[0].normal_texture = Some(texel([128, 128, 255, 255]));
    s.meshes[0].metallic_roughness_texture = Some(texel([0, 255, 0, 255]));
    assert!(
        renderer
            .render_scene(&s)
            .unwrap_err()
            .to_string()
            .contains("texture byte limit")
    );
    s.meshes[0].normal_texture = None;
    s.meshes[0].metallic_roughness_texture = None;
    Arc::make_mut(&mut s.meshes[0].mesh).normals.pop();
    assert!(
        renderer
            .render_scene(&s)
            .unwrap_err()
            .to_string()
            .contains("normals")
    );
}
