//! Rich text: a string plus styled byte-range spans, and the builder that markup uses.

use std::ops::Range;

use crate::{Color, TextError, TextLimits};

/// Placeholder character that carries an inline image span.
pub const OBJECT_REPLACEMENT: char = '\u{FFFC}';
/// Largest inline image edge in pixels.
pub const MAX_IMAGE_EDGE: f32 = 4096.0;
/// Longest link id or image key in bytes.
pub const MAX_KEY_BYTES: usize = 256;

/// Inline image placeholder; layout reserves `width` x `height` pixels sitting on the baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineImage {
    pub key: String,
    pub width: f32,
    pub height: f32,
}

/// Style overrides for one span. `None`/`false` inherit from the layout's [`crate::TextStyle`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanStyle {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub color: Option<Color>,
    /// Selects weight 700 (synthesised when the family has no bold face).
    pub bold: bool,
    /// Selects an italic face (synthetic oblique when the family has none).
    pub italic: bool,
    pub underline: bool,
    /// Link id reported in [`crate::LinkRegion`]s.
    pub link: Option<String>,
    /// Set only on a span covering exactly one [`OBJECT_REPLACEMENT`] character.
    pub image: Option<InlineImage>,
}

/// Styled byte range of [`RichText::text`].
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: SpanStyle,
}

/// Text plus sorted, non-overlapping spans. Bytes outside every span use the base style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RichText {
    pub text: String,
    pub spans: Vec<Span>,
}

impl RichText {
    /// Unstyled text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    pub fn builder() -> RichTextBuilder {
        RichTextBuilder::default()
    }

    /// Checks byte, span, size and range bounds; layout calls this before any work.
    pub fn validate(&self, limits: &TextLimits) -> Result<(), TextError> {
        if self.text.len() > limits.max_text_bytes {
            return Err(TextError::TextTooLong {
                len: self.text.len(),
                max: limits.max_text_bytes,
            });
        }
        if self.spans.len() > limits.max_spans {
            return Err(TextError::TooManySpans {
                count: self.spans.len(),
                max: limits.max_spans,
            });
        }
        let mut previous_end = 0;
        for (index, span) in self.spans.iter().enumerate() {
            let bad = |reason| Err(TextError::InvalidSpan { index, reason });
            let Range { start, end } = span.range;
            if start >= end {
                return bad("range is empty or reversed");
            }
            if end > self.text.len() {
                return bad("range ends past the text");
            }
            if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
                return bad("range is not on a char boundary");
            }
            if start < previous_end {
                return bad("spans overlap or are not sorted");
            }
            previous_end = end;
            if let Some(size) = span.style.size {
                limits.check_size(size)?;
            }
            let key_too_long = span
                .style
                .link
                .as_ref()
                .is_some_and(|l| l.len() > MAX_KEY_BYTES)
                || span
                    .style
                    .family
                    .as_ref()
                    .is_some_and(|f| f.len() > MAX_KEY_BYTES);
            if key_too_long {
                return bad("link id or family name is too long");
            }
            if let Some(image) = &span.style.image {
                let one_placeholder = self.text[start..end].chars().eq([OBJECT_REPLACEMENT]);
                if !one_placeholder {
                    return bad("image span must cover exactly one U+FFFC");
                }
                if !valid_image_edge(image.width) || !valid_image_edge(image.height) {
                    return bad("image size must be finite and in (0, 4096] px");
                }
                if image.key.is_empty() || image.key.len() > MAX_KEY_BYTES {
                    return bad("image key must be 1..=256 bytes");
                }
            }
        }
        Ok(())
    }

    /// Style of the span containing byte `index`, if any (binary search).
    pub(crate) fn span_at(&self, index: usize) -> Option<usize> {
        let pos = self.spans.partition_point(|s| s.range.end <= index);
        self.spans
            .get(pos)
            .filter(|s| s.range.start <= index)
            .map(|_| pos)
    }
}

pub(crate) fn valid_image_edge(value: f32) -> bool {
    value.is_finite() && value > 0.0 && value <= MAX_IMAGE_EDGE
}

/// One push onto the builder's style stack.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleChange {
    Bold,
    Italic,
    Underline,
    Color(Color),
    Size(f32),
    Family(String),
    Link(String),
}

/// Stack-based builder; the markup parser is a thin front end over it, so markup and builder
/// output are identical for equivalent input.
#[derive(Clone, Debug, Default)]
pub struct RichTextBuilder {
    text: String,
    spans: Vec<Span>,
    stack: Vec<SpanStyle>,
}

impl RichTextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn current(&self) -> SpanStyle {
        self.stack.last().cloned().unwrap_or_default()
    }

    /// Current nesting depth.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Pushes a style derived from the current one.
    pub fn push_style(&mut self, change: StyleChange) -> &mut Self {
        let mut style = self.current();
        match change {
            StyleChange::Bold => style.bold = true,
            StyleChange::Italic => style.italic = true,
            StyleChange::Underline => style.underline = true,
            StyleChange::Color(color) => style.color = Some(color),
            StyleChange::Size(size) => style.size = Some(size),
            StyleChange::Family(family) => style.family = Some(family),
            StyleChange::Link(link) => style.link = Some(link),
        }
        self.stack.push(style);
        self
    }

    /// Pops the innermost style; returns false when the stack was empty.
    pub fn pop(&mut self) -> bool {
        self.stack.pop().is_some()
    }

    /// Appends text in the current style.
    pub fn text(&mut self, text: &str) -> &mut Self {
        if text.is_empty() {
            return self;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let style = self.current();
        if style == SpanStyle::default() {
            return self;
        }
        match self.spans.last_mut() {
            Some(last) if last.range.end == start && last.style == style => {
                last.range.end = self.text.len();
            }
            _ => self.spans.push(Span {
                range: start..self.text.len(),
                style,
            }),
        }
        self
    }

    /// Appends an inline image placeholder in the current style.
    pub fn image(&mut self, key: impl Into<String>, width: f32, height: f32) -> &mut Self {
        let start = self.text.len();
        self.text.push(OBJECT_REPLACEMENT);
        let mut style = self.current();
        style.image = Some(InlineImage {
            key: key.into(),
            width,
            height,
        });
        self.spans.push(Span {
            range: start..self.text.len(),
            style,
        });
        self
    }

    /// Finishes the text; unclosed styles simply end here (markup reports them as errors).
    pub fn build(&mut self) -> RichText {
        self.stack.clear();
        RichText {
            text: std::mem::take(&mut self.text),
            spans: std::mem::take(&mut self.spans),
        }
    }

    pub(crate) fn text_len(&self) -> usize {
        self.text.len()
    }

    pub(crate) fn span_count(&self) -> usize {
        self.spans.len()
    }
}
