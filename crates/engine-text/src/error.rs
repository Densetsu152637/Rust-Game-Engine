//! Typed errors with stable machine-readable codes.

use std::fmt;

/// Markup failure kinds; each maps to a stable `text.markup.*` code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkupErrorKind {
    /// A `[tag]` name is not part of the markup vocabulary (`text.markup.unknown_tag`).
    UnknownTag,
    /// A tag was opened and never closed (`text.markup.unclosed`).
    Unclosed,
    /// A closing tag does not match the innermost open tag (`text.markup.mismatched`).
    Mismatched,
    /// A tag value or attribute is missing, malformed or out of range (`text.markup.bad_value`).
    BadValue,
    /// Nesting exceeds the configured depth (`text.markup.too_deep`).
    TooDeep,
    /// A `[` has no matching `]` (`text.markup.unterminated`).
    Unterminated,
}

impl MarkupErrorKind {
    /// Stable error code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownTag => "text.markup.unknown_tag",
            Self::Unclosed => "text.markup.unclosed",
            Self::Mismatched => "text.markup.mismatched",
            Self::BadValue => "text.markup.bad_value",
            Self::TooDeep => "text.markup.too_deep",
            Self::Unterminated => "text.markup.unterminated",
        }
    }
}

/// Every failure reported by `engine-text`. Hostile input yields one of these, never a panic.
#[derive(Clone, Debug, PartialEq)]
pub enum TextError {
    /// Font file larger than the configured limit (`text.font.too_large`).
    FontTooLarge { size: usize, max: usize },
    /// Font bytes could not be parsed as TTF/OTF/TTC (`text.font.invalid`).
    FontInvalid { reason: String },
    /// Registering more faces would exceed the library limit (`text.font.limit`).
    FontLimit { max: usize },
    /// The library has no face that can serve the request (`text.font.none`).
    NoFonts,
    /// Fallback configuration exceeds its bounds (`text.font.fallback_limit`).
    FallbackLimit { max: usize },
    /// Text longer than the configured byte limit (`text.input.too_long`).
    TextTooLong { len: usize, max: usize },
    /// More spans than allowed (`text.input.too_many_spans`).
    TooManySpans { count: usize, max: usize },
    /// Shaping produced more glyphs than allowed (`text.input.too_many_glyphs`).
    TooManyGlyphs { max: usize },
    /// Font size outside the allowed pixel range or not finite (`text.input.size_out_of_range`).
    SizeOutOfRange { size: f32, min: f32, max: f32 },
    /// A span range is out of bounds, unordered, overlapping or off a char boundary
    /// (`text.input.invalid_span`).
    InvalidSpan { index: usize, reason: &'static str },
    /// A layout option is not finite or out of range (`text.layout.invalid_option`).
    InvalidOption {
        name: &'static str,
        reason: &'static str,
    },
    /// Markup could not be parsed; `offset` is the byte offset in the markup source.
    Markup {
        kind: MarkupErrorKind,
        offset: usize,
        message: String,
    },
    /// Atlas configuration is out of range (`text.atlas.config`).
    AtlasConfig { reason: &'static str },
    /// Every atlas page is in use this frame and none can be evicted (`text.atlas.full`).
    AtlasFull { pages: usize },
    /// A glyph bitmap cannot fit in an empty atlas page (`text.atlas.glyph_too_large`).
    GlyphTooLarge { width: u32, height: u32, page: u32 },
    /// A glyph outline has more segments than the rasterizer accepts (`text.atlas.glyph_too_complex`).
    GlyphTooComplex { segments: usize, max: usize },
    /// Distance-field generation for a glyph would cost more than the work budget
    /// (`text.atlas.glyph_too_expensive`).
    GlyphTooExpensive { work: u64, max: u64 },
}

impl TextError {
    /// Stable machine-readable code (`text.<area>.<reason>`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::FontTooLarge { .. } => "text.font.too_large",
            Self::FontInvalid { .. } => "text.font.invalid",
            Self::FontLimit { .. } => "text.font.limit",
            Self::NoFonts => "text.font.none",
            Self::FallbackLimit { .. } => "text.font.fallback_limit",
            Self::TextTooLong { .. } => "text.input.too_long",
            Self::TooManySpans { .. } => "text.input.too_many_spans",
            Self::TooManyGlyphs { .. } => "text.input.too_many_glyphs",
            Self::SizeOutOfRange { .. } => "text.input.size_out_of_range",
            Self::InvalidSpan { .. } => "text.input.invalid_span",
            Self::InvalidOption { .. } => "text.layout.invalid_option",
            Self::Markup { kind, .. } => kind.code(),
            Self::AtlasConfig { .. } => "text.atlas.config",
            Self::AtlasFull { .. } => "text.atlas.full",
            Self::GlyphTooLarge { .. } => "text.atlas.glyph_too_large",
            Self::GlyphTooComplex { .. } => "text.atlas.glyph_too_complex",
            Self::GlyphTooExpensive { .. } => "text.atlas.glyph_too_expensive",
        }
    }

    pub(crate) fn markup(kind: MarkupErrorKind, offset: usize, message: impl Into<String>) -> Self {
        Self::Markup {
            kind,
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Self::FontTooLarge { size, max } => {
                write!(f, "font is {size} bytes, limit is {max}")
            }
            Self::FontInvalid { reason } => write!(f, "font could not be parsed: {reason}"),
            Self::FontLimit { max } => write!(f, "font library holds at most {max} faces"),
            Self::NoFonts => write!(f, "no font face is registered"),
            Self::FallbackLimit { max } => {
                write!(
                    f,
                    "fallback configuration holds at most {max} family entries"
                )
            }
            Self::TextTooLong { len, max } => write!(f, "text is {len} bytes, limit is {max}"),
            Self::TooManySpans { count, max } => write!(f, "{count} spans, limit is {max}"),
            Self::TooManyGlyphs { max } => write!(f, "layout exceeds {max} glyphs"),
            Self::SizeOutOfRange { size, min, max } => {
                write!(f, "font size {size} px is outside {min}..={max} px")
            }
            Self::InvalidSpan { index, reason } => write!(f, "span {index}: {reason}"),
            Self::InvalidOption { name, reason } => write!(f, "option `{name}`: {reason}"),
            Self::Markup {
                offset, message, ..
            } => write!(f, "at byte {offset}: {message}"),
            Self::AtlasConfig { reason } => write!(f, "invalid atlas configuration: {reason}"),
            Self::AtlasFull { pages } => {
                write!(f, "all {pages} atlas pages are in use this frame")
            }
            Self::GlyphTooLarge {
                width,
                height,
                page,
            } => write!(
                f,
                "glyph bitmap {width}x{height} does not fit a {page} px page"
            ),
            Self::GlyphTooComplex { segments, max } => {
                write!(f, "glyph outline has {segments} segments, limit is {max}")
            }
            Self::GlyphTooExpensive { work, max } => {
                write!(
                    f,
                    "glyph distance field needs {work}+ steps, limit is {max}"
                )
            }
        }
    }
}

impl std::error::Error for TextError {}
