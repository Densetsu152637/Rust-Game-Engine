//! Cross-backend parity: the same sprite scenes rendered through the
//! backend-neutral `SpriteRenderer` must produce the exact pixels the wgpu-native
//! path produces (see `gpu_pixels.rs`), and the wgpu and Vulkan backends must
//! produce byte-identical readbacks.
//!
//! These tests fail (not skip) without a working GPU, so they are ignored in
//! ordinary runs: `cargo test -p engine-desktop --test backend_parity -- --ignored`.

use engine_desktop::{BackendKind, OffscreenSprites, RenderLimits};
use engine_project::{
    Camera2D,
    content::{FontAtlas, FontGlyph, Texture, sprite_quad, text_mesh},
    runtime::{BlendMode, Draw, DrawSpace, DrawState, RenderFrame},
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

fn open(kind: BackendKind) -> OffscreenSprites {
    let sprites = OffscreenSprites::new(kind, 64, 64, RenderLimits::default())
        .unwrap_or_else(|error| panic!("{kind} backend unavailable: {error:#}"));
    eprintln!("parity backend: {}", sprites.backend_description());
    sprites
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

    let font = FontAtlas {
        texture: "atlas".into(),
        line_height: 2.0,
        fallback: '?' as u32,
        glyphs: BTreeMap::from([
            (
                '?' as u32,
                FontGlyph {
                    source: [1, 1, 1, 1],
                    advance: 1.0,
                    bearing: [0.0; 2],
                },
            ),
            (
                'Ω' as u32,
                FontGlyph {
                    source: [1, 1, 1, 1],
                    advance: 1.0,
                    bearing: [0.0; 2],
                },
            ),
        ]),
    };
    let text = Arc::new(
        text_mesh(
            &font,
            &atlas,
            "Ω?",
            [4.0, 4.0],
            0.0,
            8.0,
            [1.0, 1.0, 0.0, 1.0],
        )
        .unwrap(),
    );
    frame.draws.push(Draw {
        id: "text".into(),
        mesh: text,
        texture: Some(atlas.clone()),
        layer: 10,
        space: DrawSpace::Ui,
        clip: Some([4.0, 4.0, 10.0, 8.0]),
        state: DrawState::default(),
    });
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

/// Frame-limit and validation errors behave the same on every backend.
fn scene_limits_and_resources(kind: BackendKind) {
    let limits = RenderLimits {
        max_draws: 2,
        ..Default::default()
    };
    let mut sprites = OffscreenSprites::new(kind, 64, 64, limits).unwrap();
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

fn everything(kind: BackendKind) -> Vec<Vec<u8>> {
    let mut sprites = open(kind);
    let (quadrants, text) = scene_sprite_and_text(&mut sprites);
    let mut images = vec![quadrants, text];
    images.extend(scene_depth_and_alpha(&mut sprites));
    scene_limits_and_resources(kind);
    images
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn wgpu_backend_renders_the_exact_expected_pixels() {
    everything(BackendKind::Wgpu);
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_backend_renders_the_exact_expected_pixels() {
    everything(BackendKind::Vulkan);
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a real native wgpu adapter and a Vulkan device"]
fn wgpu_and_vulkan_produce_byte_identical_readbacks() {
    let wgpu = everything(BackendKind::Wgpu);
    let vulkan = everything(BackendKind::Vulkan);
    assert_eq!(wgpu.len(), vulkan.len());
    for (index, (a, b)) in wgpu.iter().zip(&vulkan).enumerate() {
        let differing = a.iter().zip(b).filter(|(x, y)| x != y).count();
        assert_eq!(a, b, "scene {index}: {differing} bytes differ");
    }
}

#[test]
fn headless_recording_backend_accepts_the_same_frames_without_a_gpu() {
    // The recording backend renders nothing but accepts the same frames.
    let mut recording =
        OffscreenSprites::new(BackendKind::Headless, 64, 64, RenderLimits::default()).unwrap();
    recording.render(&sprite_frame(&atlas())).unwrap();
    assert_eq!(recording.read_rgba().unwrap().len(), 64 * 64 * 4);
}
