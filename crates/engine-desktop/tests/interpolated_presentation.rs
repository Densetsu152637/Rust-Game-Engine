//! Presenting between two ticks through the real sprite renderer on the
//! recording backend: what is submitted for an interpolated frame, and that
//! alpha = 1 submits exactly what an uninterpolated frame does.
use engine_desktop::{
    CameraView, FrameHistory, RenderLimits, SpriteRenderer,
    engine_render::{RenderTarget, headless::RecordingBackend},
    interpolate_frame,
};
use engine_project::{
    Camera2D,
    content::{Texture, sprite_quad},
    runtime::{Draw, DrawMotion, DrawSpace, DrawState, RenderFrame},
};
use std::sync::Arc;

const PPU: f32 = 4.0;

fn camera(snap: bool) -> Camera2D {
    Camera2D {
        logical_size: [64, 64],
        pixels_per_unit: PPU,
        pixel_snap: snap,
        ..Default::default()
    }
}

fn atlas() -> Arc<Texture> {
    Arc::new(Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    })
}

/// A sprite draw exactly as extraction builds it: the mesh baked at the
/// (optionally pixel-snapped) entity position, the position kept in `motion`.
fn sprite(id: &str, position: [f32; 3], snap: bool, texture: &Arc<Texture>) -> Draw {
    let motion = DrawMotion {
        position,
        pixel_snap: snap,
    };
    Draw {
        id: id.into(),
        mesh: Arc::new(
            sprite_quad(
                motion.baked_position(PPU),
                [1.0, 1.0],
                [0.5, 0.5],
                [0, 0, 1, 1],
                texture,
                [1.0; 4],
            )
            .unwrap(),
        ),
        texture: Some(texture.clone()),
        layer: 0,
        space: DrawSpace::World,
        clip: None,
        state: DrawState::default(),
        motion: Some(motion),
        offset: [0.0; 3],
    }
}

fn renderer() -> (
    SpriteRenderer<RecordingBackend>,
    engine_desktop::engine_render::headless::RecordingLog,
) {
    let backend = RecordingBackend::new();
    let log = backend.log();
    let mut renderer = SpriteRenderer::new(backend, RenderLimits::default()).unwrap();
    renderer.set_surface_size((64, 64));
    (renderer, log)
}

fn bytes(matrix: &[f32; 16]) -> Vec<u8> {
    matrix.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

#[test]
fn alpha_one_submits_exactly_what_the_uninterpolated_frame_submits() {
    let texture = atlas();
    let previous = RenderFrame {
        camera: camera(true),
        draws: vec![sprite("sprite:a", [0.0, 0.0, 0.0], true, &texture)],
    };
    let current = RenderFrame {
        camera: camera(true),
        draws: vec![sprite("sprite:a", [1.3, 0.2, 0.0], true, &texture)],
    };
    let (mut renderer, log) = renderer();
    renderer.render(&current, RenderTarget::Surface).unwrap();
    let screenshot_path = interpolate_frame(&previous, &current, 1.0);
    renderer
        .render(&screenshot_path, RenderTarget::Surface)
        .unwrap();
    let frames = log.frames();
    let strip = |frame: &engine_desktop::engine_render::headless::RecordedFrame| {
        frame
            .draws
            .iter()
            .map(|d| (d.shader, d.mesh, d.texture, d.uniforms.clone(), d.state))
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&frames[0]), strip(&frames[1]));
}

#[test]
fn an_interpolated_frame_reuses_the_gpu_mesh_and_only_changes_the_matrix() {
    let texture = atlas();
    let previous = RenderFrame {
        camera: camera(false),
        draws: vec![sprite("sprite:a", [0.0, 0.0, 0.0], false, &texture)],
    };
    let current = RenderFrame {
        camera: camera(false),
        draws: vec![sprite("sprite:a", [2.0, 0.0, 0.0], false, &texture)],
    };
    let (mut renderer, log) = renderer();
    let first = renderer.render(&current, RenderTarget::Surface).unwrap();
    assert_eq!(first.unwrap().mesh_uploads, 1);
    let mut last_matrix = None;
    for alpha in [0.0_f32, 0.25, 0.5, 0.75] {
        let blended = interpolate_frame(&previous, &current, alpha);
        let stats = renderer
            .render(&blended, RenderTarget::Surface)
            .unwrap()
            .unwrap();
        assert_eq!(
            (stats.mesh_uploads, stats.cached_meshes),
            (0, 1),
            "alpha {alpha}"
        );
        let recorded = log.frames().pop().unwrap();
        let view = CameraView::new(&blended.camera, 64, 64).unwrap();
        let expected = view.world_with_offset([2.0 * alpha - 2.0, 0.0, 0.0]);
        assert_eq!(recorded.draws[0].uniforms[0].1, bytes(&expected), "{alpha}");
        assert_ne!(Some(expected), last_matrix, "the matrix advances");
        last_matrix = Some(expected);
    }
}

#[test]
fn history_drives_a_scene_transition_without_a_smear() {
    let texture = atlas();
    let frame_at = |x: f32| RenderFrame {
        camera: camera(false),
        draws: vec![sprite("sprite:player", [x, 0.0, 0.0], false, &texture)],
    };
    let mut history = FrameHistory::default();
    history.push(frame_at(1.0), 0);
    history.push(frame_at(2.0), 0);
    let (mut renderer, log) = renderer();
    // Mid-tick in the old scene: blended.
    renderer
        .render(&history.frame_at(0.5).unwrap(), RenderTarget::Surface)
        .unwrap();
    // The traveller keeps its draw id into the next scene, at a distant spawn.
    history.push(frame_at(3.0), 1);
    renderer
        .render(&history.frame_at(0.5).unwrap(), RenderTarget::Surface)
        .unwrap();
    let frames = log.frames();
    let view = CameraView::new(&camera(false), 64, 64).unwrap();
    assert_eq!(
        frames[0].draws[0].uniforms[0].1,
        bytes(&view.world_with_offset([-0.5, 0.0, 0.0]))
    );
    assert_eq!(
        frames[1].draws[0].uniforms[0].1,
        bytes(&view.world),
        "the first frame of a new scene is drawn where the scene put it"
    );
}
