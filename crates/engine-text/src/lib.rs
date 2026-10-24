//! `engine-text`: a pure-CPU text engine.
//!
//! Pipeline: [`FontLibrary`] (fonts from bytes, family/weight/style matching, fallback chains)
//! → [`RichText`] (from [`parse_markup`] or [`RichTextBuilder`]) → [`FontLibrary::layout`]
//! (UAX #9 bidi, script/face itemisation, harfrust shaping, UAX #14 line breaking, alignment,
//! ellipsis) → [`TextLayout`] (positioned glyphs, lines, links, images, caret geometry) →
//! [`build_quads`] with a [`GlyphAtlas`] (coverage or SDF pages) → neutral [`GlyphQuad`]s.
//!
//! The crate has no GPU, windowing or engine dependency. See `docs/text.md`.

mod atlas;
mod caret;
mod error;
mod font;
mod geometry;
mod layout;
mod limits;
mod markup;
mod quad;
mod raster;
mod rich;
mod shader;
mod shape;

pub use atlas::{AtlasConfig, AtlasGlyph, AtlasMode, AtlasPage, DirtyRegion, GlyphAtlas};
pub use caret::CaretMove;
pub use error::{MarkupErrorKind, TextError};
pub use font::{
    FaceId, FaceInfo, FaceMatch, FaceMetrics, FallbackConfig, FontLibrary, FontStyle,
    SYNTHETIC_BOLD_EM, SYNTHETIC_OBLIQUE_SKEW, Synthesis, weight,
};
pub use geometry::{Color, PixelRect, Rect, UvRect};
pub use layout::{
    BaseDirection, Decoration, InlineImageRect, LayoutOptions, LineHeight, LineMetrics, LinkRegion,
    Overflow, PositionedGlyph, TextAlign, TextLayout, TextStyle,
};
pub use limits::TextLimits;
pub use markup::{MarkupOptions, UnknownTagPolicy, parse_markup};
pub use quad::{GlyphQuad, WorldQuad, build_quads, transform_quads};
pub use raster::MAX_OUTLINE_SEGMENTS;
pub use rich::{
    InlineImage, MAX_IMAGE_EDGE, MAX_KEY_BYTES, OBJECT_REPLACEMENT, RichText, RichTextBuilder,
    Span, SpanStyle, StyleChange,
};
pub use shader::{GLYPH_SHADER_WGSL, SHADER_MODE_COVERAGE, SHADER_MODE_MSDF, SHADER_MODE_SDF};
