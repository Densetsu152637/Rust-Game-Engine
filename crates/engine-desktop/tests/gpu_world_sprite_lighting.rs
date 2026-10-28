//! Authored World tiles through Runtime extraction and the real desktop submission path.
use engine_desktop::{GpuContext, GpuRenderer, OffscreenTarget, RenderLimits};
use engine_project::{InputSnapshot, Project, Runtime, world::tilemap};
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

fn project() -> anyhow::Result<(tempfile::TempDir, Runtime)> {
    let dir = tempfile::tempdir()?;
    let atlas = image::RgbaImage::from_pixel(16, 16, image::Rgba([180, 180, 180, 255]));
    atlas.save(dir.path().join("atlas.png"))?;
    let normal = image::RgbaImage::from_pixel(16, 16, image::Rgba([192, 128, 238, 255]));
    normal.save(dir.path().join("normal.png"))?;
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
                "MeshMaterial":{"normal_texture":"normal","metallic":0.0,"roughness":1.0},
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
    assert!(color[0] > 10 && color[0] < 255, "{color:?}");
    assert_eq!(first.mesh_uploads, 1);
    assert_eq!(first.texture_uploads, 2);
    let same = renderer.render_scene(&runtime.extract_scene_frame()?, &target.view, None)?;
    assert_eq!((same.mesh_uploads, same.texture_uploads), (0, 0));
    let mut shifted = frame.clone();
    shifted.sprites.draws[0].offset = [1.0, 0.0, 0.0];
    renderer.render_scene(&shifted, &target.view, None)?;
    let shifted_pixels = target.read_rgba(&renderer.gpu)?;
    assert_eq!(pixel(&shifted_pixels, 64, 40, 24), [0, 0, 0, 255]);
    assert!(pixel(&shifted_pixels, 64, 56, 24)[0] > 0);
    renderer.render_scene(&frame, &target.view, None)?;

    let owner = runtime.entity("map").unwrap();
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
    Ok(())
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn authored_world_orientations_rotate_normal_response() -> anyhow::Result<()> {
    use engine_project::lighting2d::{Light2D, Light2DKind};
    use std::collections::BTreeSet;
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
        light.direction = [1.0, 0.0, -1.0];
        light.intensity = 5.0;
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
    let samples: BTreeSet<_> = (0..8)
        .map(|i| pixel(&bytes, 256, 72 + i * 16, 24)[0])
        .collect();
    assert!(samples.len() >= 3, "oriented tangent response: {samples:?}");
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
    assert!(sprite_only[1] > sprite_only[0], "{sprite_only:?}");
    assert!(
        with_ui[2] > sprite_only[2],
        "{with_ui:?} vs {sprite_only:?}"
    );
    Ok(())
}
