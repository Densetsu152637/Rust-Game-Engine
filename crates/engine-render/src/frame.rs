//! Frame submission types shared by the device and backends.

use crate::resources::TargetHandle;
use crate::state::PixelRect;

/// Pixel format of every render target. Presentation surfaces use the
/// platform's closest sRGB equivalent (for example BGRA8 sRGB); readback
/// always yields RGBA byte order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TargetFormat {
    Rgba8Srgb,
}

/// Where a frame renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenderTarget {
    /// The backend's presentation surface (window swapchain), if it has one.
    Surface,
    /// An offscreen target created through the device.
    Offscreen(TargetHandle),
}

/// Requested size of an offscreen target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OffscreenDesc {
    pub width: u32,
    pub height: u32,
    pub format: TargetFormat,
}

/// Everything a backend needs to know about a frame apart from its draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameDesc {
    /// Monotonic frame number assigned by the device; backends may use
    /// `frame % frames_in_flight` to pick per-frame resources.
    pub frame: u64,
    pub target: RenderTarget,
    /// Linear RGBA clear color applied before drawing.
    pub clear_color: [f32; 4],
    /// Depth is cleared to this value; the target always has a depth attachment.
    pub clear_depth: f32,
    /// Viewport in target pixels; `None` covers the whole target.
    pub viewport: Option<PixelRect>,
}

impl FrameDesc {
    pub fn new(target: RenderTarget) -> Self {
        Self {
            frame: 0,
            target,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            clear_depth: 1.0,
            viewport: None,
        }
    }
}

/// Counters a backend reports for one submitted frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub draws: usize,
    /// Pipeline/binding/scissor changes between consecutive draws; order is
    /// never changed to reduce them.
    pub batches: usize,
}

/// Why a frame produced no output without being an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSkip {
    /// Zero-sized or minimized surface.
    Suspended,
    /// The swapchain was out of date and has been recreated; try the next frame.
    OutOfDate,
    /// The window is not visible.
    Occluded,
    /// Acquiring a presentable image timed out.
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Commands were submitted (and presented, for the surface target).
    Rendered(FrameStats),
    Skipped(FrameSkip),
}

/// Tightly packed RGBA8 rows, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadbackImage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

impl ReadbackImage {
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = (y as usize * self.width as usize + x as usize) * 4;
        self.rgba8.get(at..at + 4)?.try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_pixel_indexing_is_row_major_and_bounds_checked() {
        let image = ReadbackImage {
            width: 2,
            height: 2,
            rgba8: (0..16).collect(),
        };
        assert_eq!(image.pixel(1, 0), Some([4, 5, 6, 7]));
        assert_eq!(image.pixel(0, 1), Some([8, 9, 10, 11]));
        assert_eq!(image.pixel(2, 0), None);
        assert_eq!(image.pixel(0, 2), None);
    }
}
