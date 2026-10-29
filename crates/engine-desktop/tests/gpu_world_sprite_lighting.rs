//! Authored World tiles through Runtime extraction and the real desktop submission path.
use engine_desktop::{GpuContext, GpuRenderer, OffscreenTarget, RenderLimits};
use engine_project::{InputSnapshot, Project, Runtime, rendering3d::MeshMaterial, world::tilemap};
use engine_project::{
    content::{Texture, sprite_quad},
    runtime::{Draw, DrawSpace, DrawState},
};
use serde_json::json;
use std::{fs, sync::Arc};

fn pixel(bytes: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    bytes[(y * width + x) * 4..(y * width + x + 1) * 4]
        .try_into()
        .unwrap()
}

fn decode(byte: u8) -> f64 {
    let value = f64::from(byte) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn encode(value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}
fn unit(v: [f64; 3]) -> [f64; 3] {
    let len = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    v.map(|x| x / len)
}
// Scalar f64 reference, independent of the shader and desktop adapter.
fn brdf(albedo: f64, metal: f64, rough: f64, n: [f64; 3], l: [f64; 3], irradiance: f64) -> f64 {
    let nv = n[2].max(1e-4);
    let nl = (0..3).map(|i| n[i] * l[i]).sum::<f64>().max(0.0);
    let h = unit([l[0], l[1], l[2] + 1.0]);
    let nh = (0..3).map(|i| n[i] * h[i]).sum::<f64>().max(0.0);
    let vh = h[2].max(0.0);
    let a2 = rough.max(0.045).powi(4);
    let denom = nh * nh * (a2 - 1.0) + 1.0;
    let d = a2 / (std::f64::consts::PI * denom * denom).max(1e-7);
    let gv = 2.0 * nv / (nv + (a2 + (1.0 - a2) * nv * nv).sqrt());
    let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(1e-6);
    let f0 = 0.04 * (1.0 - metal) + albedo * metal;
    let f = f0 + (1.0 - f0) * (1.0 - vh).clamp(0.0, 1.0).powi(5);
    let diffuse = (1.0 - f) * (1.0 - metal) * albedo / std::f64::consts::PI;
    let spec = d * gv * gl * f / (4.0 * nv * nl).max(1e-6);
    (diffuse + spec) * irradiance * nl
}
fn expected_channel(n: [f64; 3], l: [f64; 3], irradiance: f64, metal: f64, rough: f64) -> u8 {
    let albedo = decode(180);
    let ambient = albedo * (1.0 - metal) * 0.05;
    encode((ambient + brdf(albedo, metal, rough, n, l, irradiance)) * 0.75)
}
fn close(actual: [u8; 4], expected: u8) {
    for channel in actual.into_iter().take(3) {
        assert!(
            (i16::from(channel) - i16::from(expected)).abs() <= 4,
            "actual {actual:?}, expected {expected}"
        );
    }
    assert_eq!(actual[3], 255);
}
fn close_rgb(actual: [u8; 4], expected: [u8; 3]) {
    for c in 0..3 {
        assert!(
            (i16::from(actual[c]) - i16::from(expected[c])).abs() <= 4,
            "actual {actual:?}, expected {expected:?}"
        );
    }
    assert_eq!(actual[3], 255);
}
fn point_expected_size(x: usize, y: usize, size: usize, scale: f64, metal: f64, rough: f64) -> u8 {
    let center = size as f64 / 2.0;
    let world = [
        (x as f64 + 0.5 - center) / (16.0 * scale),
        (center - y as f64 - 0.5) / (16.0 * scale),
        0.0,
    ];
    let delta = [0.5 - world[0], 0.5 - world[1], 2.0];
    let distance = delta.iter().map(|v| v * v).sum::<f64>().sqrt();
    let window = (1.0 - (distance / 8.0).powi(4)).clamp(0.0, 1.0).powi(2);
    let irradiance = 18.0 * window / (distance * distance).max(1e-4);
    let normal = unit([1.0, -1.0 / 255.0, 1.0]);
    expected_channel(normal, unit(delta), irradiance, metal, rough)
}
fn point_expected(x: usize, y: usize, metal: f64, rough: f64) -> u8 {
    point_expected_size(x, y, 64, 1.0, metal, rough)
}

fn project() -> anyhow::Result<(tempfile::TempDir, Runtime)> {
    let dir = tempfile::tempdir()?;
    let atlas = image::RgbaImage::from_pixel(16, 16, image::Rgba([180, 180, 180, 255]));
    atlas.save(dir.path().join("atlas.png"))?;
    let normal = image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 128, 255, 255]));
    normal.save(dir.path().join("normal.png"))?;
    let mr = image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 128, 64, 255]));
    mr.save(dir.path().join("mr.png"))?;
    fs::write(
        dir.path().join("tiles.json"),
        json!({
            "version":1,"name":"test","texture":"atlas","tile_size":[16,16],
            "tiles":[{"id":1,"name":"tile","source":[0,0,16,16]}]
        })
        .to_string(),
    )?;
    let mut tiles = vec![0u32; 256];
    tiles[0] = 1;
    fs::write(
        dir.path().join("map.json"),
        json!({
            "version":1,"bounds":{"min":[-16,-16],"max":[16,16]},
            "layers":[{"name":"ground","z_order":0,"visible":true,"opacity":0.75,
                "chunks":[{"x":0,"y":0,"tiles":tiles}]}]
        })
        .to_string(),
    )?;
    fs::write(
        dir.path().join("scene.json"),
        json!({
        "schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"camera","components":{
                "Transform":{"position":[0,0,0]},
                "Camera2D":{"logicalSize":[64,64],"pixelsPerUnit":16,"pixelSnap":false},
                "custom":{"engine.Lighting2D":{"version":1,"data":{
                    "enabled":true,"ambient":[0.05,0.05,0.05],"exposure":1.0}}}}},
            {"id":"map","name":"map","components":{
                "Transform":{"position":[0,0,0]},
                "MeshMaterial":{"normal_texture":"normal","metallic_roughness_texture":"mr",
                    "metallic":0.8,"roughness":1.0},
                "custom":{"engine.Tilemap":{"version":1,"data":{
                    "tileset":"tiles","map":"map","collision":{"enabled":false}}},
                    "engine.SpriteLighting2D":{"version":1,"data":{"enabled":true}}}}},
            {"id":"lamp","name":"lamp","components":{
                "Transform":{"position":[0.5,0.5,2.0]},
                "custom":{"engine.Light2D":{"version":1,"data":{
                    "kind":"point","color":[1.0,1.0,1.0],"intensity":18.0,
                    "range":8.0,"enabled":true}}}}}
        ]})
        .to_string(),
    )?;
    fs::write(
        dir.path().join("project.json"),
        json!({
            "schema":1,"name":"world-sprite-lighting","startup_scene":"scene.json",
            "assets":[
                {"id":"atlas","kind":"texture","path":"atlas.png"},
                {"id":"normal","kind":"texture","path":"normal.png"},
                {"id":"mr","kind":"texture","path":"mr.png"},
                {"id":"tiles","kind":"tileset","path":"tiles.json"},
                {"id":"map","kind":"tilemap","path":"map.json"}]
        })
        .to_string(),
    )?;
    let project = Project::load(dir.path().join("project.json"))?;
    let mut runtime = Runtime::new(project)?;
    runtime.tick(0.1, InputSnapshot::default())?;
    Ok((dir, runtime))
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn authored_world_tile_lighting_upload_reuse_and_recreation() -> anyhow::Result<()> {
    let (_files, mut runtime) = project()?;
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let frame = runtime.extract_scene_frame()?;
    let lit = frame.lighting2d.as_ref().unwrap();
    assert_eq!(lit.materials.len(), 1);
    let first = renderer.render_scene(&frame, &target.view, None)?;
    let color = pixel(&target.read_rgba(&renderer.gpu)?, 64, 40, 24);
    close(
        color,
        point_expected(40, 24, 0.8 * 64.0 / 255.0, 128.0 / 255.0),
    );
    assert_eq!(first.mesh_uploads, 1);
    assert_eq!(first.texture_uploads, 3);
    let same = renderer.render_scene(&runtime.extract_scene_frame()?, &target.view, None)?;
    assert_eq!((same.mesh_uploads, same.texture_uploads), (0, 0));
    let owner = runtime.entity("map").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<MeshMaterial>(owner)
        .unwrap()
        .metallic_roughness_texture = None;
    let unmapped = runtime.extract_scene_frame()?;
    renderer.render_scene(&unmapped, &target.view, None)?;
    let unmapped_color = pixel(&target.read_rgba(&renderer.gpu)?, 64, 40, 24);
    close(unmapped_color, point_expected(40, 24, 0.8, 1.0));
    assert_ne!(
        unmapped_color, color,
        "MR atlas must change actual World tile shading"
    );
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<MeshMaterial>(owner)
        .unwrap()
        .metallic_roughness_texture = Some("mr".into());
    let mut shifted = frame.clone();
    shifted.sprites.draws[0].offset = [1.0, 0.0, 0.0];
    renderer.render_scene(&shifted, &target.view, None)?;
    let shifted_pixels = target.read_rgba(&renderer.gpu)?;
    assert_eq!(pixel(&shifted_pixels, 64, 40, 24), [0, 0, 0, 255]);
    close(
        pixel(&shifted_pixels, 64, 56, 24),
        point_expected(56, 24, 0.8 * 64.0 / 255.0, 128.0 / 255.0),
    );
    renderer.render_scene(&frame, &target.view, None)?;

    tilemap::erase(runtime.app_mut().world_mut(), owner, "ground", [0, 0])?;
    tilemap::sync_tilemaps(runtime.app_mut().world_mut())?;
    let cleared = runtime.extract_scene_frame()?;
    assert!(
        cleared
            .sprites
            .draws
            .iter()
            .all(|draw| !draw.id.starts_with("mesh:tilemap:map/"))
    );
    renderer.render_scene(&cleared, &target.view, None)?;
    assert_eq!(
        pixel(&target.read_rgba(&renderer.gpu)?, 64, 40, 24),
        [0, 0, 0, 255]
    );

    tilemap::paint(runtime.app_mut().world_mut(), owner, "ground", [0, 0], 1, 0)?;
    tilemap::sync_tilemaps(runtime.app_mut().world_mut())?;
    let recreated = runtime.extract_scene_frame()?;
    let update = renderer.render_scene(&recreated, &target.view, None)?;
    assert_eq!(update.mesh_uploads, 1);
    assert_eq!(pixel(&target.read_rgba(&renderer.gpu)?, 64, 40, 24), color);
    tilemap::paint(runtime.app_mut().world_mut(), owner, "ground", [1, 0], 1, 0)?;
    tilemap::sync_tilemaps(runtime.app_mut().world_mut())?;
    let edited = renderer.render_scene(&runtime.extract_scene_frame()?, &target.view, None)?;
    assert_eq!((edited.mesh_uploads, edited.texture_uploads), (1, 0));
    tilemap::paint(runtime.app_mut().world_mut(), owner, "ground", [1, 0], 1, 0)?;
    tilemap::sync_tilemaps(runtime.app_mut().world_mut())?;
    let unchanged = renderer.render_scene(&runtime.extract_scene_frame()?, &target.view, None)?;
    assert_eq!((unchanged.mesh_uploads, unchanged.texture_uploads), (0, 0));
    Ok(())
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn authored_world_orientations_rotate_normal_response() -> anyhow::Result<()> {
    use engine_project::lighting2d::{Light2D, Light2DKind};
    let (_files, mut runtime) = project()?;
    let owner = runtime.entity("map").unwrap();
    let camera = runtime.entity("camera").unwrap();
    let lamp = runtime.entity("lamp").unwrap();
    {
        let world = runtime.app_mut().world_mut();
        let mut view = world.get_mut::<engine_project::Camera2D>(camera).unwrap();
        view.logical_size = [256, 64];
        view.center = [4.0, 0.0];
        let mut light = world.get_mut::<Light2D>(lamp).unwrap();
        light.kind = Light2DKind::Directional;
        light.direction = [-0.5, -0.3, -1.0];
        light.intensity = 0.5;
    }
    for flags in 0..8 {
        tilemap::paint(
            runtime.app_mut().world_mut(),
            owner,
            "ground",
            [flags, 0],
            1,
            flags as u8,
        )?;
    }
    tilemap::sync_tilemaps(runtime.app_mut().world_mut())?;
    let frame = runtime.extract_scene_frame()?;
    assert_eq!(frame.lighting2d.as_ref().unwrap().materials.len(), 1);
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, 256, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    renderer.render_scene(&frame, &target.view, None)?;
    let bytes = target.read_rgba(&renderer.gpu)?;
    let tangents = [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let bitangents = [
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
    ];
    let toward = unit([0.5, 0.3, 1.0]);
    for flags in 0..8 {
        let n = unit(std::array::from_fn(|c| {
            tangents[flags][c] + bitangents[flags][c] / 255.0 + if c == 2 { 1.0 } else { 0.0 }
        }));
        let expected = expected_channel(n, toward, 0.5, 0.8 * 64.0 / 255.0, 128.0 / 255.0);
        close(pixel(&bytes, 256, 72 + flags * 16, 24), expected);
    }
    Ok(())
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn world_tile_interleaves_with_sprite_and_clipped_ui() -> anyhow::Result<()> {
    let (_files, runtime) = project()?;
    let mut frame = runtime.extract_scene_frame()?;
    let white = Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    };
    frame.sprites.draws.push(Draw {
        id: "sprite:overlay".into(),
        mesh: Arc::new(sprite_quad(
            [0.5, 0.5, 0.0],
            [1.0, 1.0],
            [0.5, 0.5],
            [0, 0, 1, 1],
            &white,
            [0.0, 1.0, 0.0, 0.5],
        )?),
        texture: None,
        layer: 1,
        space: DrawSpace::World,
        clip: None,
        state: DrawState::default(),
        motion: None,
        offset: [0.0; 3],
    });
    frame.sprites.draws.push(Draw {
        id: "ui:overlay".into(),
        mesh: Arc::new(sprite_quad(
            [40.0, 24.0, 0.0],
            [16.0, 16.0],
            [0.5, 0.5],
            [0, 0, 1, 1],
            &white,
            [0.0, 0.0, 1.0, 0.5],
        )?),
        texture: None,
        layer: 0,
        space: DrawSpace::Ui,
        clip: Some([40.0, 24.0, 8.0, 8.0]),
        state: DrawState::default(),
        motion: None,
        offset: [0.0; 3],
    });
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let stats = renderer.render_scene(&frame, &target.view, None)?;
    assert_eq!(stats.draws, 3);
    let bytes = target.read_rgba(&renderer.gpu)?;
    let sprite_only = pixel(&bytes, 64, 36, 20);
    let with_ui = pixel(&bytes, 64, 44, 28);
    let tile_only = |x, y| decode(point_expected(x, y, 0.8 * 64.0 / 255.0, 128.0 / 255.0));
    let sprite_rgb = |x, y| {
        let tile = tile_only(x, y);
        [
            encode(tile * 0.5),
            encode(tile * 0.5 + 0.5),
            encode(tile * 0.5),
        ]
    };
    close_rgb(sprite_only, sprite_rgb(36, 20));
    let before_ui = sprite_rgb(44, 28).map(decode);
    close_rgb(
        with_ui,
        [
            encode(before_ui[0] * 0.5),
            encode(before_ui[1] * 0.5),
            encode(before_ui[2] * 0.5 + 0.5),
        ],
    );
    close_rgb(pixel(&bytes, 64, 44, 20), sprite_rgb(44, 20));
    close_rgb(pixel(&bytes, 64, 36, 28), sprite_rgb(36, 28));
    Ok(())
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn hybrid_camera_toggle_and_independent_sizes_keep_tile_snapshot_current() -> anyhow::Result<()> {
    use engine_project::{
        lighting2d::{Lighting2D, SpriteLighting2D},
        rendering3d::Camera3D,
    };
    let (_files, mut runtime) = project()?;
    let camera = runtime.entity("camera").unwrap();
    let owner = runtime.entity("map").unwrap();
    let gpu = pollster::block_on(GpuContext::headless())?;
    let small = OffscreenTarget::new(&gpu, 64, 64)?;
    let large = OffscreenTarget::new(&gpu, 128, 128)?;
    let mut renderer = GpuRenderer::new(gpu.clone(), small.info, RenderLimits::default())?;

    runtime
        .app_mut()
        .world_mut()
        .entity_mut(camera)
        .insert(Camera3D::default());
    let hybrid = runtime.extract_scene_frame()?;
    assert!(hybrid.camera3d.is_some());
    assert!(
        hybrid
            .meshes
            .iter()
            .all(|mesh| !mesh.id.starts_with("tilemap:map/"))
    );
    assert_eq!(hybrid.lighting2d.as_ref().unwrap().materials.len(), 1);
    renderer.render_scene(&hybrid, &small.view, None)?;
    close(
        pixel(&small.read_rgba(&renderer.gpu)?, 64, 40, 24),
        point_expected(40, 24, 0.8 * 64.0 / 255.0, 128.0 / 255.0),
    );

    runtime
        .app_mut()
        .world_mut()
        .get_mut::<SpriteLighting2D>(owner)
        .unwrap()
        .enabled = false;
    let routed_3d = runtime.extract_scene_frame()?;
    assert!(
        routed_3d
            .meshes
            .iter()
            .any(|mesh| mesh.id.starts_with("tilemap:map/"))
    );
    assert!(
        routed_3d
            .sprites
            .draws
            .iter()
            .all(|draw| !draw.id.starts_with("mesh:tilemap:map/"))
    );
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<SpriteLighting2D>(owner)
        .unwrap()
        .enabled = true;
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Camera3D>(camera)
        .unwrap()
        .active = false;
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Lighting2D>(camera)
        .unwrap()
        .enabled = false;
    let unlit = runtime.extract_scene_frame()?;
    assert!(unlit.lighting2d.is_none());
    renderer.render_scene(&unlit, &small.view, None)?;
    close(
        pixel(&small.read_rgba(&renderer.gpu)?, 64, 40, 24),
        encode(decode(180) * 0.75),
    );

    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Lighting2D>(camera)
        .unwrap()
        .enabled = true;
    let relit = runtime.extract_scene_frame()?;
    renderer.resize(128, 128)?;
    renderer.render_scene(&relit, &large.view, None)?;
    close(
        pixel(&large.read_rgba(&renderer.gpu)?, 128, 80, 48),
        point_expected_size(80, 48, 128, 2.0, 0.8 * 64.0 / 255.0, 128.0 / 255.0),
    );
    let mut independent = GpuRenderer::new(gpu, small.info, RenderLimits::default())?;
    independent.render_scene(&relit, &small.view, None)?;
    close(
        pixel(&small.read_rgba(&independent.gpu)?, 64, 40, 24),
        point_expected(40, 24, 0.8 * 64.0 / 255.0, 128.0 / 255.0),
    );
    Ok(())
}
