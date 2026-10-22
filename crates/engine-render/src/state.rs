//! Fixed-function draw state.

/// Pixel rectangle with a top-left origin, in target pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Intersection with a `width` x `height` target anchored at the origin.
    pub fn clamped_to(self, width: u32, height: u32) -> Self {
        let x = self.x.min(width);
        let y = self.y.min(height);
        Self {
            x,
            y,
            width: self.width.min(width - x),
            height: self.height.min(height - y),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlendMode {
    /// Source replaces the destination.
    #[default]
    Opaque,
    /// `src * a + dst * (1 - a)` on color; alpha accumulates as `a + dst_a * (1 - a)`.
    StraightAlpha,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CullMode {
    /// Draw both faces. Front faces wind counter-clockwise when culling is on.
    #[default]
    None,
    Front,
    Back,
}

/// Per-draw state. Everything except `scissor` selects a pipeline; `scissor`
/// is dynamic and never forces a new pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DrawState {
    pub blend: BlendMode,
    pub depth_test: bool,
    pub depth_write: bool,
    pub cull: CullMode,
    pub scissor: Option<PixelRect>,
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            blend: BlendMode::Opaque,
            depth_test: true,
            depth_write: true,
            cull: CullMode::None,
            scissor: None,
        }
    }
}

/// The pipeline-selecting subset of [`DrawState`]. Backends key their pipeline
/// caches by `(shader, vertex layout, PipelineState, target format)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PipelineState {
    pub blend: BlendMode,
    pub depth_test: bool,
    pub depth_write: bool,
    pub cull: CullMode,
}

impl DrawState {
    pub fn pipeline_state(&self) -> PipelineState {
        PipelineState {
            blend: self.blend,
            depth_test: self.depth_test,
            depth_write: self.depth_write,
            cull: self.cull,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scissor_never_changes_the_pipeline_key() {
        let plain = DrawState::default();
        let clipped = DrawState {
            scissor: Some(PixelRect::new(1, 2, 3, 4)),
            ..plain
        };
        assert_ne!(plain, clipped);
        assert_eq!(plain.pipeline_state(), clipped.pipeline_state());
        let blended = DrawState {
            blend: BlendMode::StraightAlpha,
            ..plain
        };
        assert_ne!(plain.pipeline_state(), blended.pipeline_state());
    }

    #[test]
    fn rect_clamps_to_target_bounds() {
        assert_eq!(
            PixelRect::new(60, 10, 20, 100).clamped_to(64, 32),
            PixelRect::new(60, 10, 4, 22)
        );
        assert!(PixelRect::new(100, 100, 5, 5).clamped_to(64, 64).is_empty());
    }
}
