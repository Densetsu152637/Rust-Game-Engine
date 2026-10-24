//! Text backend contract (UI contract 3). `ui_tree` never touches fonts
//! directly: layout measures text and rendering lays it out through a
//! [`UiTextBackend`]. [`BitmapTextBackend`] implements it over the bitmap
//! [`FontAtlas`]; a shaping backend (engine-text) plugs in through the same trait.
use super::*;
use crate::content::{FontAtlas, Texture};
use std::{collections::BTreeMap, sync::Arc};

/// How the backend interprets [`UiTextRequest::text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiTextKind {
    Plain,
    /// Rich-text markup. Backends without rich text render it as plain text
    /// with `<...>` tags removed and `&lt; &gt; &amp; &quot;` decoded.
    Markup,
}

/// One measure/layout request. Coordinates are logical pixels with a
/// top-left origin at (0, 0).
#[derive(Clone, Copy, Debug)]
pub struct UiTextRequest<'a> {
    pub text: &'a str,
    pub kind: UiTextKind,
    /// Font asset key (`UiStyle::font`).
    pub font: &'a str,
    pub scale: f32,
    /// Base colour; markup may override it per glyph.
    pub color: [f32; 4],
    /// Wrap width; `None` never wraps (only `\n` breaks lines).
    pub max_width: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UiTextMetrics {
    /// Widest line.
    pub width: f32,
    /// Total height of all lines.
    pub height: f32,
    /// Height of one line at the requested scale.
    pub line_height: f32,
    pub lines: u32,
    /// Widest unbreakable run (CSS min-content width).
    pub min_width: f32,
}

/// A textured glyph rectangle. `uv` is `[u0, v0, u1, v1]` (top-left, bottom-right).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiGlyphQuad {
    pub rect: Rect,
    pub uv: [f32; 4],
    pub color: [f32; 4],
    /// Index into [`UiTextLayout::pages`].
    pub page: u32,
}

/// Caret position before the character starting at byte `index` of the laid
/// out text (plain text after markup removal); the last stop is the end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiCaretStop {
    pub index: u32,
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Default)]
pub struct UiTextLayout {
    pub metrics: UiTextMetrics,
    pub glyphs: Vec<UiGlyphQuad>,
    /// RGBA8 atlas pages; glyph pages use white RGB with coverage in alpha.
    pub pages: Vec<Arc<Texture>>,
    /// Ascending by `index`; may be empty for markup.
    pub carets: Vec<UiCaretStop>,
}

/// Object-safe text backend. Implementations must be deterministic for a
/// request so layout, hit testing and rendering agree, and must return errors
/// (stable `render.*`/`text.*` codes) instead of panicking.
pub trait UiTextBackend: Send + Sync {
    fn measure(&self, request: &UiTextRequest<'_>) -> Result<UiTextMetrics>;
    fn layout(&self, request: &UiTextRequest<'_>) -> Result<UiTextLayout>;
}

/// Measures every string as empty. Used by headless layout without fonts.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullTextBackend;

impl UiTextBackend for NullTextBackend {
    fn measure(&self, _: &UiTextRequest<'_>) -> Result<UiTextMetrics> {
        Ok(UiTextMetrics::default())
    }
    fn layout(&self, _: &UiTextRequest<'_>) -> Result<UiTextLayout> {
        Ok(UiTextLayout {
            carets: vec![UiCaretStop {
                index: 0,
                x: 0.0,
                y: 0.0,
                height: 0.0,
            }],
            ..Default::default()
        })
    }
}

/// Default backend over bitmap JSON font atlases. Wrapping matches
/// `content::text_mesh`: words wrap first, then over-long words break per glyph.
#[derive(Clone, Copy)]
pub struct BitmapTextBackend<'a> {
    pub fonts: &'a BTreeMap<String, FontAtlas>,
    pub textures: &'a BTreeMap<String, Arc<Texture>>,
}

impl<'a> BitmapTextBackend<'a> {
    pub fn new(
        fonts: &'a BTreeMap<String, FontAtlas>,
        textures: &'a BTreeMap<String, Arc<Texture>>,
    ) -> Self {
        Self { fonts, textures }
    }
    fn font(&self, key: &str) -> Result<&'a FontAtlas> {
        self.fonts
            .get(key)
            .ok_or_else(|| Error::new("render.font.unknown", key))
    }
}

fn check(request: &UiTextRequest<'_>) -> Result<()> {
    if request.text.len() > 65536
        || !request.scale.is_finite()
        || request.scale <= 0.0
        || request.max_width.is_some_and(|w| !w.is_finite())
    {
        return Err(Error::new(
            "ui.text.invalid",
            "Invalid text geometry parameters",
        ));
    }
    Ok(())
}

impl UiTextBackend for BitmapTextBackend<'_> {
    fn measure(&self, request: &UiTextRequest<'_>) -> Result<UiTextMetrics> {
        check(request)?;
        let font = self.font(request.font)?;
        let text = plain_text(request);
        Ok(run(font, request, &text, |_, _, _| {}).0)
    }

    fn layout(&self, request: &UiTextRequest<'_>) -> Result<UiTextLayout> {
        check(request)?;
        let font = self.font(request.font)?;
        let texture = self
            .textures
            .get(&font.texture)
            .ok_or_else(|| Error::new("render.texture.unknown", &font.texture))?;
        let text = plain_text(request);
        let mut glyphs = vec![];
        let mut failure = None;
        let (metrics, carets) = run(font, request, &text, |glyph, x, y| {
            let s = glyph.source;
            if s[2] == 0 || s[3] == 0 {
                return;
            }
            if texture.width == 0
                || texture.height == 0
                || s[0].checked_add(s[2]).is_none_or(|n| n > texture.width)
                || s[1].checked_add(s[3]).is_none_or(|n| n > texture.height)
            {
                failure = Some(Error::new(
                    "asset.sprite.rectangle",
                    "Sprite rectangle exceeds texture",
                ));
                return;
            }
            let (w, h) = (texture.width as f32, texture.height as f32);
            glyphs.push(UiGlyphQuad {
                rect: Rect {
                    x: x + glyph.bearing[0] * request.scale,
                    y: y + glyph.bearing[1] * request.scale,
                    width: s[2] as f32 * request.scale,
                    height: s[3] as f32 * request.scale,
                },
                uv: [
                    s[0] as f32 / w,
                    s[1] as f32 / h,
                    (s[0] + s[2]) as f32 / w,
                    (s[1] + s[3]) as f32 / h,
                ],
                color: request.color,
                page: 0,
            });
        });
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(UiTextLayout {
            metrics,
            glyphs,
            pages: vec![texture.clone()],
            carets,
        })
    }
}

fn plain_text<'a>(request: &UiTextRequest<'a>) -> std::borrow::Cow<'a, str> {
    match request.kind {
        UiTextKind::Plain => request.text.into(),
        UiTextKind::Markup => strip_markup(request.text).into(),
    }
}

/// Remove `<...>` tags and decode the four basic entities. An unterminated
/// `<` is kept literally.
pub fn strip_markup(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(start) = rest.find(['<', '&']) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if rest.starts_with('<') {
            match rest.find('>') {
                Some(end) => rest = &rest[end + 1..],
                None => {
                    out.push_str(rest);
                    return out;
                }
            }
            continue;
        }
        let entity = [
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&amp;", '&'),
            ("&quot;", '"'),
        ]
        .into_iter()
        .find(|(name, _)| rest.starts_with(name));
        match entity {
            Some((name, c)) => {
                out.push(c);
                rest = &rest[name.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Shared bitmap line breaker; `emit(glyph, pen_x, line_top)` per drawn glyph.
fn run(
    font: &FontAtlas,
    request: &UiTextRequest<'_>,
    text: &str,
    mut emit: impl FnMut(&crate::content::FontGlyph, f32, f32),
) -> (UiTextMetrics, Vec<UiCaretStop>) {
    let scale = request.scale;
    let line = font.line_height * scale;
    // A non-positive wrap width breaks at every opportunity.
    let wrap = request.max_width.map(|w| w.max(f32::MIN_POSITIVE));
    let (mut x, mut y) = (0.0_f32, 0.0_f32);
    let mut width = 0.0_f32;
    let mut min_width = 0.0_f32;
    let mut carets = Vec::with_capacity(text.len() + 1);
    let mut index = 0usize;
    let mut breaks = 0u32;
    for word in text.split_inclusive(char::is_whitespace) {
        let word_width: f32 = word
            .chars()
            .take_while(|c| !c.is_whitespace())
            .map(|c| font.glyph(c).advance * scale)
            .sum();
        min_width = min_width.max(word_width);
        if let Some(wrap) = wrap
            && word_width > 0.0
            && x > 0.0
            && x + word_width > wrap
        {
            x = 0.0;
            y += line;
            breaks += 1;
        }
        for c in word.chars() {
            let at = index;
            index += c.len_utf8();
            if c == '\r' {
                continue;
            }
            if c == '\n' {
                carets.push(UiCaretStop {
                    index: at as u32,
                    x,
                    y,
                    height: line,
                });
                x = 0.0;
                y += line;
                breaks += 1;
                continue;
            }
            let glyph = font.glyph(c);
            let advance = glyph.advance * scale;
            // Whitespace hangs past the wrap column; the following word wraps instead.
            if let Some(wrap) = wrap
                && !c.is_whitespace()
                && x > 0.0
                && x + advance > wrap
            {
                x = 0.0;
                y += line;
                breaks += 1;
            }
            carets.push(UiCaretStop {
                index: at as u32,
                x,
                y,
                height: line,
            });
            emit(glyph, x, y);
            x += advance;
            if !c.is_whitespace() {
                width = width.max(x);
            }
        }
    }
    carets.push(UiCaretStop {
        index: text.len() as u32,
        x,
        y,
        height: line,
    });
    let lines = if text.is_empty() { 0 } else { breaks + 1 };
    (
        UiTextMetrics {
            width,
            height: lines as f32 * line,
            line_height: line,
            lines,
            min_width,
        },
        carets,
    )
}
