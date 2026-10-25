//! Bounds applied to hostile or oversized input.

/// Limits enforced by [`crate::FontLibrary`] loading, markup parsing and layout.
///
/// Every limit is checked before the corresponding allocation or work; exceeding one
/// returns a [`crate::TextError`] with a stable code instead of panicking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLimits {
    /// Largest accepted font file (default 32 MiB).
    pub max_font_bytes: usize,
    /// Most faces one library may register (default 256).
    pub max_faces: usize,
    /// Most faces taken from one collection file (default 64).
    pub max_collection_faces: usize,
    /// Longest text or markup source in bytes (default 256 KiB).
    pub max_text_bytes: usize,
    /// Most spans in one rich text, which also bounds markup tags (default 4096).
    pub max_spans: usize,
    /// Most shaped glyphs in one layout (default 65 536).
    pub max_glyphs: usize,
    /// Smallest font size in pixels (default 1).
    pub min_font_px: f32,
    /// Largest font size in pixels (default 512).
    pub max_font_px: f32,
    /// Deepest markup tag nesting (default 32).
    pub max_markup_depth: usize,
    /// Most family names across the whole fallback configuration (default 256).
    pub max_fallback_entries: usize,
}

impl Default for TextLimits {
    fn default() -> Self {
        Self {
            max_font_bytes: 32 * 1024 * 1024,
            max_faces: 256,
            max_collection_faces: 64,
            max_text_bytes: 256 * 1024,
            max_spans: 4096,
            max_glyphs: 65_536,
            min_font_px: 1.0,
            max_font_px: 512.0,
            max_markup_depth: 32,
            max_fallback_entries: 256,
        }
    }
}

impl TextLimits {
    pub(crate) fn check_size(&self, size: f32) -> Result<(), crate::TextError> {
        if size.is_finite() && size >= self.min_font_px && size <= self.max_font_px {
            Ok(())
        } else {
            Err(crate::TextError::SizeOutOfRange {
                size,
                min: self.min_font_px,
                max: self.max_font_px,
            })
        }
    }
}
