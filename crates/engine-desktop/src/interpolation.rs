//! Presentation-time interpolation between the last two simulation ticks.
//!
//! The runner extracts a [`RenderFrame`] after a tick and keeps the previous
//! one in [`FrameHistory`]. Each redraw then blends them by the pacer's `alpha`
//! with [`interpolate_frame`]. This is presentation only: it reads two finished
//! frames and produces a third, and nothing here touches the runtime, so
//! simulation never sees an interpolated value.
//!
//! What blends and what does not:
//!
//! * Draws that carry a [`DrawMotion`] (sprites and meshes) blend their entity
//!   position, matched by draw id. The mesh is not rebuilt: the blended
//!   position becomes [`Draw::offset`] against the current tick's mesh, so the
//!   GPU copy is reused and a sprite animation frame is always the current
//!   tick's.
//! * Pixel snapping happens after the blend, with the same
//!   [`snap_to_pixels`] extraction uses, so a pixel-snapped sprite steps from
//!   pixel to pixel instead of sliding between them.
//! * The camera centre blends the same way (and snaps in `CameraView`).
//! * UI draws and anything without motion are the current tick's, untouched.
//! * Anything that cannot be a smooth move snaps to the current tick: a draw
//!   with no previous match (spawn), a scene transition, a changed camera
//!   projection, or a jump of more than a screen in one tick (a teleport).

use engine_project::{
    Camera2D,
    runtime::{Draw, DrawMotion, RenderFrame, snap_to_pixels},
};
use std::borrow::Cow;
use std::collections::HashMap;

/// A one-tick displacement larger than this many screens (the camera's logical
/// size over its pixels per unit) is a teleport, not motion, and is not blended.
const TELEPORT_SCREENS: f32 = 1.0;

/// `a` at `t = 0`, exactly `b` at `t = 1`.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

fn lerp_all<const N: usize>(a: [f32; N], b: [f32; N], t: f32) -> [f32; N] {
    std::array::from_fn(|axis| lerp(a[axis], b[axis], t))
}

fn teleport_distance(camera: &Camera2D) -> f32 {
    let [width, height] = camera.logical_size;
    width.max(height) as f32 / camera.pixels_per_unit * TELEPORT_SCREENS
}

fn is_jump<const N: usize>(previous: [f32; N], current: [f32; N], limit: f32) -> bool {
    previous.iter().zip(&current).any(|(a, b)| {
        let step = (b - a).abs();
        step.is_nan() || step > limit
    })
}

fn blend_camera(previous: &Camera2D, current: &Camera2D, alpha: f32) -> Camera2D {
    let mut camera = current.clone();
    let same_view = previous.logical_size == current.logical_size
        && previous.pixels_per_unit == current.pixels_per_unit;
    if same_view && !is_jump(previous.center, current.center, teleport_distance(current)) {
        camera.center = lerp_all(previous.center, current.center, alpha);
    }
    camera
}

/// The position to draw `current` at, blended toward `previous` unless that
/// would smear: `None` keeps the current tick's placement.
fn blended_position(
    previous: Option<&DrawMotion>,
    current: &DrawMotion,
    alpha: f32,
    camera: &Camera2D,
) -> [f32; 3] {
    let Some(previous) = previous else {
        return current.position;
    };
    if is_jump(
        previous.position,
        current.position,
        teleport_distance(camera),
    ) {
        return current.position;
    }
    lerp_all(previous.position, current.position, alpha)
}

/// Blend `previous` (the tick before) into `current` by `alpha` in `[0, 1]`:
/// 0 draws the previous tick's placement, 1 the current one. `alpha >= 1`
/// returns an exact copy of `current`, so a screenshot taken at `alpha = 1` is
/// the frame extraction produced.
///
/// The caller owns the "same scene" decision; see [`FrameHistory`].
pub fn interpolate_frame(previous: &RenderFrame, current: &RenderFrame, alpha: f32) -> RenderFrame {
    if alpha.is_nan() || alpha >= 1.0 {
        return current.clone();
    }
    let alpha = alpha.max(0.0);
    let ppu = current.camera.pixels_per_unit;
    let before: HashMap<&str, &DrawMotion> = previous
        .draws
        .iter()
        .filter_map(|draw| Some((draw.id.as_str(), draw.motion.as_ref()?)))
        .collect();
    let draws = current
        .draws
        .iter()
        .map(|draw| {
            let mut out: Draw = draw.clone();
            if let Some(motion) = &draw.motion {
                let position = blended_position(
                    before.get(draw.id.as_str()).copied(),
                    motion,
                    alpha,
                    &current.camera,
                );
                let drawn = if motion.pixel_snap {
                    snap_to_pixels(position, ppu)
                } else {
                    position
                };
                let baked = motion.baked_position(ppu);
                out.offset = std::array::from_fn(|axis| drawn[axis] - baked[axis]);
            }
            out
        })
        .collect();
    RenderFrame {
        camera: blend_camera(&previous.camera, &current.camera, alpha),
        draws,
    }
}

/// The last two extracted frames and the scene they belong to.
#[derive(Debug, Default)]
pub struct FrameHistory {
    previous: Option<RenderFrame>,
    current: Option<RenderFrame>,
    scene_revision: u64,
}

impl FrameHistory {
    /// Record the frame extracted after a tick. A frame from a different scene
    /// revision than the one before it starts the history afresh, so nothing
    /// blends across a scene transition.
    pub fn push(&mut self, frame: RenderFrame, scene_revision: u64) {
        let same_scene = self.current.is_some() && self.scene_revision == scene_revision;
        self.previous = if same_scene {
            self.current.take()
        } else {
            None
        };
        self.current = Some(frame);
        self.scene_revision = scene_revision;
    }

    /// The newest extracted frame.
    pub fn current(&self) -> Option<&RenderFrame> {
        self.current.as_ref()
    }

    /// Whether a previous frame exists to blend from.
    pub fn can_blend(&self) -> bool {
        self.previous.is_some()
    }

    /// The frame to present `alpha` of the way from the previous tick to the
    /// current one; the current frame itself (borrowed, not copied) when there
    /// is nothing to blend or `alpha` is 1. `None` before the first push.
    pub fn frame_at(&self, alpha: f64) -> Option<Cow<'_, RenderFrame>> {
        let current = self.current.as_ref()?;
        match &self.previous {
            Some(previous) if alpha < 1.0 => Some(Cow::Owned(interpolate_frame(
                previous,
                current,
                alpha as f32,
            ))),
            _ => Some(Cow::Borrowed(current)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        content::{Mesh, sprite_quad},
        runtime::{DrawSpace, DrawState},
    };
    use std::sync::Arc;

    fn camera(snap: bool) -> Camera2D {
        Camera2D {
            logical_size: [64, 64],
            pixels_per_unit: 16.0,
            pixel_snap: snap,
            ..Default::default()
        }
    }

    fn texture() -> engine_project::content::Texture {
        engine_project::content::Texture {
            width: 1,
            height: 1,
            pixels: vec![255; 4],
        }
    }

    fn sprite(id: &str, position: [f32; 3], snap: bool, ppu: f32) -> Draw {
        let motion = DrawMotion {
            position,
            pixel_snap: snap,
        };
        let mesh: Mesh = sprite_quad(
            motion.baked_position(ppu),
            [1.0, 1.0],
            [0.5, 0.5],
            [0, 0, 1, 1],
            &texture(),
            [1.0; 4],
        )
        .unwrap();
        Draw {
            id: id.into(),
            mesh: Arc::new(mesh),
            texture: None,
            layer: 0,
            space: DrawSpace::World,
            clip: None,
            state: DrawState::default(),
            motion: Some(motion),
            offset: [0.0; 3],
        }
    }

    fn ui(id: &str) -> Draw {
        Draw {
            motion: None,
            space: DrawSpace::Ui,
            ..sprite(id, [0.0; 3], false, 16.0)
        }
    }

    fn frame(snap: bool, draws: Vec<Draw>) -> RenderFrame {
        RenderFrame {
            camera: camera(snap),
            draws,
        }
    }

    fn offset_of(frame: &RenderFrame, id: &str) -> [f32; 3] {
        frame.draws.iter().find(|d| d.id == id).unwrap().offset
    }

    /// The position a draw ends up at: its baked mesh position plus its offset.
    fn drawn(frame: &RenderFrame, id: &str) -> [f32; 3] {
        let draw = frame.draws.iter().find(|d| d.id == id).unwrap();
        let motion = draw.motion.unwrap();
        let base = motion.baked_position(frame.camera.pixels_per_unit);
        [
            base[0] + draw.offset[0],
            base[1] + draw.offset[1],
            base[2] + draw.offset[2],
        ]
    }

    fn close(a: [f32; 3], b: [f32; 3]) {
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-5, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn blends_a_moving_draw_linearly_between_the_two_ticks() {
        let previous = frame(
            false,
            vec![sprite("sprite:a", [0.0, 0.0, 0.0], false, 16.0)],
        );
        let current = frame(
            false,
            vec![sprite("sprite:a", [2.0, -1.0, 0.0], false, 16.0)],
        );
        for (alpha, expected) in [
            (0.0, [0.0, 0.0, 0.0]),
            (0.25, [0.5, -0.25, 0.0]),
            (0.5, [1.0, -0.5, 0.0]),
            (0.75, [1.5, -0.75, 0.0]),
        ] {
            let blended = interpolate_frame(&previous, &current, alpha);
            close(drawn(&blended, "sprite:a"), expected);
        }
    }

    #[test]
    fn blending_reuses_the_current_mesh_and_only_moves_the_offset() {
        let previous = frame(false, vec![sprite("sprite:a", [0.0; 3], false, 16.0)]);
        let current = frame(
            false,
            vec![sprite("sprite:a", [2.0, 0.0, 0.0], false, 16.0)],
        );
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert!(Arc::ptr_eq(&blended.draws[0].mesh, &current.draws[0].mesh));
        close(offset_of(&blended, "sprite:a"), [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn alpha_one_is_the_extracted_frame_exactly() {
        let previous = frame(true, vec![sprite("sprite:a", [0.0; 3], true, 16.0)]);
        let current = frame(true, vec![sprite("sprite:a", [0.3, 0.4, 0.5], true, 16.0)]);
        for alpha in [1.0_f32, 2.0, f32::INFINITY] {
            let blended = interpolate_frame(&previous, &current, alpha);
            assert_eq!(blended.camera.center, current.camera.center);
            for (b, c) in blended.draws.iter().zip(&current.draws) {
                assert_eq!(b.offset, [0.0; 3]);
                assert_eq!(b.id, c.id);
                assert!(Arc::ptr_eq(&b.mesh, &c.mesh));
            }
        }
    }

    #[test]
    fn extraction_output_is_offset_free() {
        let current = frame(true, vec![sprite("sprite:a", [0.3, 0.4, 0.5], true, 16.0)]);
        assert_eq!(current.draws[0].offset, [0.0; 3]);
    }

    #[test]
    fn a_draw_with_no_previous_match_snaps_to_the_current_tick() {
        let previous = frame(false, vec![sprite("sprite:old", [0.0; 3], false, 16.0)]);
        let current = frame(
            false,
            vec![
                sprite("sprite:old", [1.0, 0.0, 0.0], false, 16.0),
                sprite("sprite:spawned", [5.0, 5.0, 0.0], false, 16.0),
            ],
        );
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(offset_of(&blended, "sprite:spawned"), [0.0; 3]);
        close(drawn(&blended, "sprite:old"), [0.5, 0.0, 0.0]);
    }

    #[test]
    fn a_draw_that_vanished_is_not_drawn() {
        let previous = frame(
            false,
            vec![
                sprite("sprite:a", [0.0; 3], false, 16.0),
                sprite("sprite:gone", [0.0; 3], false, 16.0),
            ],
        );
        let current = frame(
            false,
            vec![sprite("sprite:a", [1.0, 0.0, 0.0], false, 16.0)],
        );
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(blended.draws.len(), 1);
        assert_eq!(blended.draws[0].id, "sprite:a");
    }

    #[test]
    fn a_teleport_of_more_than_a_screen_snaps() {
        // The screen is 64 px / 16 ppu = 4 units wide.
        let previous = frame(false, vec![sprite("sprite:a", [0.0; 3], false, 16.0)]);
        let jump = frame(
            false,
            vec![sprite("sprite:a", [10.0, 0.0, 0.0], false, 16.0)],
        );
        let blended = interpolate_frame(&previous, &jump, 0.5);
        assert_eq!(offset_of(&blended, "sprite:a"), [0.0; 3]);
        let walk = frame(
            false,
            vec![sprite("sprite:a", [3.9, 0.0, 0.0], false, 16.0)],
        );
        let blended = interpolate_frame(&previous, &walk, 0.5);
        close(drawn(&blended, "sprite:a"), [1.95, 0.0, 0.0]);
    }

    #[test]
    fn non_finite_motion_never_produces_a_finite_blend() {
        let previous = frame(
            false,
            vec![sprite("sprite:a", [f32::NAN, 0.0, 0.0], false, 16.0)],
        );
        let current = frame(
            false,
            vec![sprite("sprite:a", [1.0, 0.0, 0.0], false, 16.0)],
        );
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(offset_of(&blended, "sprite:a"), [0.0; 3]);
    }

    #[test]
    fn pixel_snap_applies_after_the_blend() {
        // ppu 16: a pixel is 0.0625 units. Move 0 -> 0.5 (8 px).
        let previous = frame(true, vec![sprite("sprite:a", [0.0; 3], true, 16.0)]);
        let current = frame(true, vec![sprite("sprite:a", [0.5, 0.0, 0.0], true, 16.0)]);
        for step in 0..=15 {
            let alpha = step as f32 / 16.0 + 0.01;
            let blended = interpolate_frame(&previous, &current, alpha);
            let x = drawn(&blended, "sprite:a")[0];
            let pixels = x * 16.0;
            assert!(
                (pixels - pixels.round()).abs() < 1e-4,
                "alpha {alpha}: x {x} is {pixels} px, not a whole pixel"
            );
            // Snapped from the blended value, not from either tick's position.
            let exact = 0.5 * alpha;
            assert!((x - exact).abs() <= 0.5 / 16.0 + 1e-6, "{x} vs {exact}");
        }
    }

    #[test]
    fn unsnapped_sprites_move_by_fractions_of_a_pixel() {
        let previous = frame(false, vec![sprite("sprite:a", [0.0; 3], false, 16.0)]);
        let current = frame(
            false,
            vec![sprite("sprite:a", [0.5, 0.0, 0.0], false, 16.0)],
        );
        let blended = interpolate_frame(&previous, &current, 0.3);
        close(drawn(&blended, "sprite:a"), [0.15, 0.0, 0.0]);
    }

    #[test]
    fn snap_matches_what_extraction_would_have_baked() {
        // The blended+snapped position equals the baked position of a sprite
        // that sat exactly there: the offset is the difference of two snaps.
        let previous = frame(true, vec![sprite("sprite:a", [0.0; 3], true, 16.0)]);
        let current = frame(true, vec![sprite("sprite:a", [0.33, 0.0, 0.0], true, 16.0)]);
        let blended = interpolate_frame(&previous, &current, 0.5);
        let expected = snap_to_pixels([0.165, 0.0, 0.0], 16.0);
        close(drawn(&blended, "sprite:a"), expected);
    }

    #[test]
    fn ui_draws_are_the_current_ticks_and_never_offset() {
        let previous = frame(false, vec![ui("ui:hud")]);
        let current = frame(false, vec![ui("ui:hud")]);
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(offset_of(&blended, "ui:hud"), [0.0; 3]);
        assert!(Arc::ptr_eq(&blended.draws[0].mesh, &current.draws[0].mesh));
    }

    #[test]
    fn the_camera_centre_blends_and_snaps_in_the_camera_view() {
        let mut previous = frame(false, vec![]);
        let mut current = frame(false, vec![]);
        previous.camera.center = [0.0, 0.0];
        current.camera.center = [2.0, 1.0];
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(blended.camera.center, [1.0, 0.5]);
    }

    #[test]
    fn a_camera_teleport_or_projection_change_snaps() {
        let mut previous = frame(false, vec![]);
        let mut current = frame(false, vec![]);
        current.camera.center = [100.0, 0.0];
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(blended.camera.center, [100.0, 0.0]);

        previous.camera.center = [0.0, 0.0];
        current.camera.center = [1.0, 0.0];
        current.camera.pixels_per_unit = 32.0;
        let blended = interpolate_frame(&previous, &current, 0.5);
        assert_eq!(blended.camera.center, [1.0, 0.0]);
    }

    #[test]
    fn a_follow_camera_and_its_target_blend_in_lockstep() {
        // Both move by the same amount, so their blended difference is the
        // current tick's difference at every alpha: no relative jitter.
        let mut previous = frame(false, vec![sprite("sprite:a", [0.0; 3], false, 16.0)]);
        let mut current = frame(
            false,
            vec![sprite("sprite:a", [1.0, 0.0, 0.0], false, 16.0)],
        );
        previous.camera.center = [0.0, 0.0];
        current.camera.center = [1.0, 0.0];
        for alpha in [0.1_f32, 0.4, 0.9] {
            let blended = interpolate_frame(&previous, &current, alpha);
            let relative = drawn(&blended, "sprite:a")[0] - blended.camera.center[0];
            assert!(relative.abs() < 1e-6, "alpha {alpha}: {relative}");
        }
    }

    #[test]
    fn history_blends_only_within_one_scene() {
        let a = frame(false, vec![sprite("sprite:a", [0.0; 3], false, 16.0)]);
        let b = frame(
            false,
            vec![sprite("sprite:a", [1.0, 0.0, 0.0], false, 16.0)],
        );
        let mut history = FrameHistory::default();
        assert!(history.frame_at(0.5).is_none());
        history.push(a, 0);
        assert!(!history.can_blend());
        history.push(b.clone(), 0);
        assert!(history.can_blend());
        let blended = history.frame_at(0.5).unwrap();
        close(drawn(&blended, "sprite:a"), [0.5, 0.0, 0.0]);

        // A scene transition: same draw id, new revision. No smear.
        let after = frame(
            false,
            vec![sprite("sprite:a", [3.0, 0.0, 0.0], false, 16.0)],
        );
        history.push(after, 1);
        assert!(!history.can_blend());
        let shown = history.frame_at(0.5).unwrap();
        assert_eq!(offset_of(&shown, "sprite:a"), [0.0; 3]);
        assert!(matches!(shown, Cow::Borrowed(_)));

        // The next tick in the new scene blends again.
        history.push(b, 1);
        assert!(history.can_blend());
    }

    #[test]
    fn history_hands_back_the_current_frame_uncopied_at_alpha_one_or_without_a_previous() {
        let a = frame(false, vec![sprite("sprite:a", [0.0; 3], false, 16.0)]);
        let b = frame(
            false,
            vec![sprite("sprite:a", [1.0, 0.0, 0.0], false, 16.0)],
        );
        let mut history = FrameHistory::default();
        history.push(a, 0);
        assert!(matches!(history.frame_at(0.3), Some(Cow::Borrowed(_))));
        history.push(b, 0);
        assert!(matches!(history.frame_at(1.0), Some(Cow::Borrowed(_))));
        assert!(matches!(history.frame_at(0.999), Some(Cow::Owned(_))));
        assert!(history.current().is_some());
    }
}
