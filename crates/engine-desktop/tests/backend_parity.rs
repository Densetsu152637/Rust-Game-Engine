//! Sprite scenes through `OffscreenSprites`.
//!
//! * Ordinary tests run the scenes on the deterministic recording backend and
//!   assert the command stream the renderer emits (no GPU needed).
//! * `#[ignore]` tests run the same exact-pixel scenes as `gpu_pixels.rs` on every
//!   wgpu graphics API (`Primary`, `Vulkan`, `Dx12`, `Gl`), skipping with a note an
//!   API that has no adapter on this machine, and compare the readbacks of the
//!   APIs that ran. Run: `cargo test -p engine-desktop --test backend_parity --
//!   --ignored --nocapture`.

use engine_desktop::{CameraView, OffscreenSprites, RenderLimits, WgpuBackends};
use engine_project::{
    Camera2D,
    content::{FontAtlas, FontGlyph, Texture, sprite_quad, text_mesh},
    runtime::{BlendMode, Draw, DrawSpace, DrawState, RenderFrame},
};
use engine_render::{
    PixelRect, UniformSlot,
    headless::{RecordedFrame, RecordingBackend, RecordingLog},
};
use std::{collections::BTreeMap, sync::Arc};

fn camera() -> Camera2D {
    Camera2D {
        logical_size: [64, 64],
        pixels_per_unit: 1.0,
        ..Default::default()
    }
}

fn pixel(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    pixels[(y * width + x) * 4..(y * width + x + 1) * 4]
        .try_into()
        .unwrap()
}

fn atlas() -> Arc<Texture> {
    Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    })
}

fn sprite_frame(atlas: &Arc<Texture>) -> RenderFrame {
    let sprite = Arc::new(
        sprite_quad(
            [0.0, 0.0, 0.5],
            [32.0, 32.0],
            [0.5, 0.5],
            [0, 0, 2, 2],
            atlas,
            [1.0; 4],
        )
        .unwrap(),
    );
    RenderFrame {
        camera: camera(),
        draws: vec![Draw {
            id: "sprite".into(),
            mesh: sprite,
            texture: Some(atlas.clone()),
            layer: 0,
            space: DrawSpace::World,
            clip: None,
            state: DrawState::default(),
        }],
    }
}

fn text_draw(atlas: &Arc<Texture>) -> Draw {
    let glyph = FontGlyph {
        source: [1, 1, 1, 1],
        advance: 1.0,
        bearing: [0.0; 2],
    };
    let font = FontAtlas {
        texture: "atlas".into(),
        line_height: 2.0,
        fallback: '?' as u32,
        glyphs: BTreeMap::from([('?' as u32, glyph.clone()), ('Ω' as u32, glyph)]),
    };
    let text = Arc::new(
        text_mesh(
            &font,
            atlas,
            "Ω?",
            [4.0, 4.0],
            0.0,
            8.0,
            [1.0, 1.0, 0.0, 1.0],
        )
        .unwrap(),
    );
    Draw {
        id: "text".into(),
        mesh: text,
        texture: Some(atlas.clone()),
        layer: 10,
        space: DrawSpace::Ui,
        clip: Some([4.0, 4.0, 10.0, 8.0]),
        state: DrawState::default(),
    }
}

fn white_quad(id: &str, layer: i32, z: f32, color: [f32; 4], state: DrawState) -> Draw {
    let white = Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    };
    Draw {
        id: id.into(),
        layer,
        mesh: Arc::new(
            sprite_quad(
                [0.0, 0.0, z],
                [64.0, 64.0],
                [0.5, 0.5],
                [0, 0, 1, 1],
                &white,
                color,
            )
            .unwrap(),
        ),
        texture: None,
        space: DrawSpace::World,
        clip: None,
        state,
    }
}

// ------------------------------------------------- deterministic (no GPU)

fn recording(limits: RenderLimits) -> (OffscreenSprites, RecordingLog) {
    let backend = RecordingBackend::new();
    let log = backend.log();
    (
        OffscreenSprites::new(Box::new(backend), 64, 64, limits).unwrap(),
        log,
    )
}

fn frames(log: &RecordingLog) -> Vec<RecordedFrame> {
    log.frames()
}

fn matrix_bytes(matrix: &[f32; 16]) -> Vec<u8> {
    matrix.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

#[test]
fn recorded_stream_orders_draws_by_layer_then_id_whatever_the_input_order() {
    let (mut sprites, log) = recording(RenderLimits::default());
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    frame.draws.push(text_draw(&atlas));
    frame
        .draws
        .push(white_quad("b", 0, 0.5, [1.0; 4], DrawState::default()));
    frame
        .draws
        .push(white_quad("a", 0, 0.5, [0.5; 4], DrawState::default()));
    sprites.render(&frame).unwrap();
    frame.draws.reverse();
    sprites.render(&frame).unwrap();
    let recorded = frames(&log);
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].draws.len(), 4);
    let order = |frame: &RecordedFrame| -> Vec<_> {
        frame.draws.iter().map(|d| (d.mesh, d.texture)).collect()
    };
    assert_eq!(order(&recorded[0]), order(&recorded[1]));
    // Layer 0 by id (a, b, sprite), then layer 10 (text): only the text and the
    // sprite sample a texture, and the text comes last.
    assert!(recorded[0].draws[0].texture.is_none());
    assert!(recorded[0].draws[1].texture.is_none());
    assert!(recorded[0].draws[2].texture.is_some());
    assert!(recorded[0].draws[3].texture.is_some());
    assert_eq!(
        recorded[0]
            .draws
            .iter()
            .map(|d| d.order)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
}

#[test]
fn recorded_camera_uniforms_differ_between_world_and_ui_space() {
    let (mut sprites, log) = recording(RenderLimits::default());
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    frame.draws.push(text_draw(&atlas));
    sprites.render(&frame).unwrap();
    let recorded = &frames(&log)[0];
    let camera = CameraView::new(&camera(), 64, 64).unwrap();
    let slot = UniformSlot::new(0, 0);
    assert_eq!(
        recorded.draws[0].uniforms,
        [(slot, matrix_bytes(&camera.world))]
    );
    assert_eq!(
        recorded.draws[1].uniforms,
        [(slot, matrix_bytes(&camera.ui))]
    );
    assert_ne!(camera.world, camera.ui);
}

#[test]
fn recorded_viewport_scissor_and_state_follow_the_scene() {
    let (mut sprites, log) = recording(RenderLimits::default());
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    frame.draws.push(text_draw(&atlas));
    let opaque = DrawState {
        blend: BlendMode::Opaque,
        depth_test: true,
        depth_write: true,
    };
    frame
        .draws
        .push(white_quad("solid", 5, 0.5, [1.0; 4], opaque));
    sprites.render(&frame).unwrap();
    let recorded = &frames(&log)[0];
    assert_eq!(recorded.desc.viewport, Some(PixelRect::new(0, 0, 64, 64)));
    assert_eq!(recorded.desc.clear_color, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(recorded.desc.clear_depth, 1.0);
    // sprite (layer 0), solid (layer 5), text (layer 10).
    let [sprite, solid, text] = &recorded.draws[..] else {
        panic!("expected three draws");
    };
    assert_eq!(sprite.state.scissor, Some(PixelRect::new(0, 0, 64, 64)));
    assert_eq!(text.state.scissor, Some(PixelRect::new(4, 4, 10, 8)));
    assert_eq!(sprite.state.blend, engine_render::BlendMode::StraightAlpha);
    assert!(!sprite.state.depth_test && !sprite.state.depth_write);
    assert_eq!(solid.state.blend, engine_render::BlendMode::Opaque);
    assert!(solid.state.depth_test && solid.state.depth_write);
    assert_eq!(solid.state.cull, engine_render::CullMode::None);
}

#[test]
fn evicted_resources_are_destroyed_only_after_frames_in_flight() {
    let (mut sprites, log) = recording(RenderLimits {
        max_frames_in_flight: 2,
        ..Default::default()
    });
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    let first = sprites.render(&frame).unwrap();
    assert_eq!((first.mesh_uploads, first.texture_uploads), (1, 1));
    assert!(log.destroyed().is_empty());
    frame.draws.clear();
    let evicting = sprites.render(&frame).unwrap();
    assert_eq!((evicting.cached_meshes, evicting.cached_textures), (0, 0));
    assert!(
        log.destroyed().is_empty(),
        "still referenced by frames that may be in flight"
    );
    sprites.render(&frame).unwrap();
    sprites.render(&frame).unwrap();
    assert_eq!(log.destroyed().len(), 2, "mesh and texture released");
    // The evicted assets upload again once, then stay cached.
    let again = sprite_frame(&atlas);
    let one = sprites.render(&again).unwrap();
    let two = sprites.render(&again).unwrap();
    assert_eq!((one.mesh_uploads, one.texture_uploads), (1, 1));
    assert_eq!((two.mesh_uploads, two.texture_uploads), (0, 0));
}

#[test]
fn limits_and_invalid_data_fail_before_anything_is_recorded() {
    let (mut sprites, log) = recording(RenderLimits {
        max_draws: 2,
        ..Default::default()
    });
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    let mut a = frame.draws[0].clone();
    a.id = "a".into();
    let mut b = frame.draws[0].clone();
    b.id = "b".into();
    frame.draws.extend([a, b]);
    assert!(sprites.render(&frame).is_err(), "draw limit");
    frame.draws.truncate(1);
    frame.draws[0].texture = Some(Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![0; 3],
    }));
    assert!(sprites.render(&frame).is_err(), "invalid texture data");
    assert!(frames(&log).is_empty());
    assert_eq!(sprites.renderer().device().counts().meshes, 0);
}

// ----------------------------------------------------------- real GPUs

type Make = dyn Fn(RenderLimits) -> anyhow::Result<OffscreenSprites>;

/// Textured sprite quadrants, then UTF-8 atlas text under a UI clip.
fn scene_sprite_and_text(sprites: &mut OffscreenSprites) -> (Vec<u8>, Vec<u8>) {
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    let first = sprites.render(&frame).unwrap();
    assert_eq!((first.mesh_uploads, first.texture_uploads), (1, 1));
    let quadrants = sprites.read_rgba().unwrap();
    assert_eq!(pixel(&quadrants, 64, 20, 20), [255, 0, 0, 255]);
    assert_eq!(pixel(&quadrants, 64, 44, 20), [0, 255, 0, 255]);
    assert_eq!(pixel(&quadrants, 64, 20, 44), [0, 0, 255, 255]);
    assert_eq!(pixel(&quadrants, 64, 44, 44), [255, 255, 255, 255]);
    assert_eq!(pixel(&quadrants, 64, 2, 2), [0, 0, 0, 255]);
    let unchanged = sprites.render(&frame).unwrap();
    assert_eq!((unchanged.mesh_uploads, unchanged.texture_uploads), (0, 0));

    frame.draws.push(text_draw(&atlas));
    let updated = sprites.render(&frame).unwrap();
    assert_eq!((updated.mesh_uploads, updated.texture_uploads), (1, 0));
    assert_eq!(updated.cached_textures, 1);
    let text_pixels = sprites.read_rgba().unwrap();
    assert_eq!(pixel(&text_pixels, 64, 5, 5), [255, 255, 0, 255]);
    assert_eq!(pixel(&text_pixels, 64, 13, 5), [255, 255, 0, 255]);
    assert_eq!(pixel(&text_pixels, 64, 14, 5), [0, 0, 0, 255], "clipped");

    // Layer+ID ordering is stable even if producer iteration reverses.
    frame.draws.reverse();
    sprites.render(&frame).unwrap();
    assert_eq!(text_pixels, sprites.read_rgba().unwrap());
    (quadrants, text_pixels)
}

/// Depth test/write flags and straight alpha, with stable equal-layer ordering.
fn scene_depth_and_alpha(sprites: &mut OffscreenSprites) -> Vec<Vec<u8>> {
    let opaque = DrawState {
        blend: BlendMode::Opaque,
        depth_test: true,
        depth_write: true,
    };
    let mut frame = RenderFrame {
        camera: camera(),
        draws: vec![
            white_quad("near", 0, 0.2, [1.0, 0.0, 0.0, 1.0], opaque),
            white_quad("far", 1, 0.8, [0.0, 0.0, 1.0, 1.0], opaque),
        ],
    };
    let mut images = Vec::new();
    sprites.render(&frame).unwrap();
    let depth_tested = sprites.read_rgba().unwrap();
    assert_eq!(pixel(&depth_tested, 64, 32, 32), [255, 0, 0, 255]);
    images.push(depth_tested);

    frame.draws[1].state = DrawState::default();
    sprites.render(&frame).unwrap();
    let untested = sprites.read_rgba().unwrap();
    assert_eq!(pixel(&untested, 64, 32, 32), [0, 0, 255, 255]);
    images.push(untested);

    frame.draws[1] = white_quad("alpha", 1, 0.8, [0.0, 1.0, 0.0, 0.5], DrawState::default());
    sprites.render(&frame).unwrap();
    let blended = sprites.read_rgba().unwrap();
    let color = pixel(&blended, 64, 32, 32);
    assert!((187..=188).contains(&color[0]) && (187..=188).contains(&color[1]));
    assert_eq!(&color[2..], &[0, 255]);
    images.push(blended);

    frame.draws[0].id = "a".into();
    frame.draws[1].id = "z".into();
    frame.draws[1].layer = 0;
    frame.draws.reverse();
    sprites.render(&frame).unwrap();
    let reordered = sprites.read_rgba().unwrap();
    assert_eq!(
        pixel(&reordered, 64, 32, 32),
        color,
        "equal-layer painter order must follow stable IDs, not input iteration"
    );
    images.push(reordered);
    images
}

/// Frame-limit and validation errors behave the same on every API.
fn scene_limits_and_resources(make: &Make) {
    let mut sprites = make(RenderLimits {
        max_draws: 2,
        ..Default::default()
    })
    .unwrap();
    let atlas = atlas();
    let mut frame = sprite_frame(&atlas);
    sprites.render(&frame).unwrap();
    let mut excess = frame.draws[0].clone();
    excess.id = "excess".into();
    let mut third = excess.clone();
    third.id = "third".into();
    frame.draws.extend([excess, third]);
    assert!(sprites.render(&frame).is_err(), "draw limit");
    frame.draws.truncate(1);
    frame.draws[0].texture = Some(Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![0; 3],
    }));
    assert!(sprites.render(&frame).is_err(), "invalid texture data");
    frame.draws[0].texture = Some(atlas);
    sprites.render(&frame).unwrap();
    frame.draws.clear();
    let empty = sprites.render(&frame).unwrap();
    assert_eq!((empty.cached_meshes, empty.cached_textures), (0, 0));
    assert_eq!(
        pixel(&sprites.read_rgba().unwrap(), 64, 32, 32),
        [0, 0, 0, 255]
    );
}

/// Run every scene on one API; `None` when it has no adapter here.
fn run_api(backends: WgpuBackends) -> Option<(String, Vec<Vec<u8>>)> {
    let make: Box<Make> = Box::new(move |limits| OffscreenSprites::wgpu(backends, 64, 64, limits));
    let mut sprites = match make(RenderLimits::default()) {
        Ok(sprites) => sprites,
        Err(error) => {
            eprintln!("NOTE: wgpu API {backends} not available here, skipped: {error:#}");
            return None;
        }
    };
    let description = sprites.backend_description();
    eprintln!("wgpu API {backends}: ran on {description}");
    let (quadrants, text) = scene_sprite_and_text(&mut sprites);
    let mut images = vec![quadrants, text];
    images.extend(scene_depth_and_alpha(&mut sprites));
    scene_limits_and_resources(&*make);
    Some((description, images))
}

macro_rules! api_test {
    ($name:ident, $backends:expr) => {
        #[test]
        #[ignore = "requires a real wgpu adapter for this graphics API"]
        fn $name() {
            run_api($backends);
        }
    };
}

api_test!(
    primary_api_renders_the_exact_expected_pixels,
    WgpuBackends::Primary
);
api_test!(
    vulkan_api_renders_the_exact_expected_pixels,
    WgpuBackends::Vulkan
);
api_test!(
    dx12_api_renders_the_exact_expected_pixels,
    WgpuBackends::Dx12
);
api_test!(gl_api_renders_the_exact_expected_pixels, WgpuBackends::Gl);

#[test]
#[ignore = "requires a real wgpu adapter"]
fn every_available_api_produces_byte_identical_readbacks() {
    let mut ran: Vec<(WgpuBackends, String, Vec<Vec<u8>>)> = Vec::new();
    for backends in [
        WgpuBackends::Primary,
        WgpuBackends::Vulkan,
        WgpuBackends::Dx12,
        WgpuBackends::Gl,
    ] {
        if let Some((description, images)) = run_api(backends) {
            ran.push((backends, description, images));
        }
    }
    assert!(!ran.is_empty(), "no wgpu API had an adapter");
    eprintln!(
        "APIs that ran: {}",
        ran.iter()
            .map(|(b, d, _)| format!("{b} ({d})"))
            .collect::<Vec<_>>()
            .join("; ")
    );
    let (first_api, _, reference) = &ran[0];
    for (api, _, images) in &ran[1..] {
        for (scene, (a, b)) in reference.iter().zip(images).enumerate() {
            let differing = a.iter().zip(b).filter(|(x, y)| x != y).count();
            if differing == 0 {
                continue;
            }
            let at = a.iter().zip(b).position(|(x, y)| x != y).unwrap() / 4;
            eprintln!(
                "DIFF {first_api} vs {api}, scene {scene}: {differing} bytes differ, first at pixel ({}, {})",
                at % 64,
                at / 64
            );
        }
        assert_eq!(
            reference, images,
            "{first_api} and {api} readbacks differ (see DIFF lines)"
        );
    }
}
