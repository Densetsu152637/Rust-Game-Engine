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

/// A clickable rectangle of a rich label (`[link=id]...[/link]`), relative to
/// the layout origin. A link that wraps has one region per line.
#[derive(Clone, Debug, PartialEq)]
pub struct UiLinkRegion {
    pub id: String,
    pub rect: Rect,
}

/// An inline image placeholder (`[img=key width=W height=H]`), relative to the
/// layout origin. `key` names a runtime texture.
#[derive(Clone, Debug, PartialEq)]
pub struct UiInlineImage {
    pub key: String,
    pub rect: Rect,
}

/// Caret, selection and hit-test geometry of one laid-out text, in the layout's
/// own coordinates. Indices are byte offsets into the laid-out text. Backends
/// that can place carets inside bidirectional or wrapped text provide it;
/// otherwise text input falls back to [`UiTextLayout::carets`].
pub trait UiTextGeometry: std::fmt::Debug + Send + Sync {
    /// Caret rectangle (width 0, full line height) before byte `index`.
    fn caret_rect(&self, index: usize) -> Rect;
    /// One rectangle per contiguous visual piece per line.
    fn selection_rects(&self, start: usize, end: usize) -> Vec<Rect>;
    /// Nearest caret stop to `point`.
    fn hit_test(&self, point: [f32; 2]) -> usize;
    /// Caret stop one visual (wrapped) line above or below `index`.
    fn move_vertical(&self, index: usize, down: bool) -> usize;
}

#[derive(Clone, Debug, Default)]
pub struct UiTextLayout {
    pub metrics: UiTextMetrics,
    pub glyphs: Vec<UiGlyphQuad>,
    /// RGBA8 atlas pages; glyph pages use white RGB with coverage in alpha.
    pub pages: Vec<Arc<Texture>>,
    /// Ascending by `index`; may be empty for markup.
    pub carets: Vec<UiCaretStop>,
    /// Clickable link rectangles (rich text backends only).
    pub links: Vec<UiLinkRegion>,
    /// Inline image rectangles (rich text backends only).
    pub images: Vec<UiInlineImage>,
    /// Precise caret and selection geometry, when the backend has it.
    pub geometry: Option<Arc<dyn UiTextGeometry>>,
    /// [`Self::pages`] are SDF atlas pages (distance in alpha) that only the
    /// `ui.sdf_text` render feature can draw. A backend sets this only after
    /// [`UiTextBackend::set_sdf_output`] enabled SDF output.
    pub sdf: bool,
}

/// Object-safe text backend. Implementations must be deterministic for a
/// request so layout, hit testing and rendering agree, and must return errors
/// (stable `render.*`/`text.*` codes) instead of panicking.
pub trait UiTextBackend: Send + Sync {
    fn measure(&self, request: &UiTextRequest<'_>) -> Result<UiTextMetrics>;
    fn layout(&self, request: &UiTextRequest<'_>) -> Result<UiTextLayout>;
    /// Called at the start of every UI tick and every draw extraction, so
    /// backends with a glyph cache can age it. The default does nothing.
    fn begin_frame(&self) {}
    /// Let families that request SDF return SDF pages (`UiTextLayout::sdf`)
    /// instead of coverage pages. Only the draw extraction that hands the
    /// result to the `ui.sdf_text` render feature enables it. The default
    /// ignores the request (the backend has no SDF output).
    fn set_sdf_output(&self, _enabled: bool) {}
}

/// The runtime's backend: a font key that resolves to a `font_face` family
/// (see [`EngineTextBackend::resolves`]) uses the shaping engine, any other key
/// uses the bitmap font of that name. Without an engine every key is bitmap.
#[derive(Clone, Copy)]
pub struct SelectingTextBackend<'a> {
    pub engine: Option<&'a EngineTextBackend>,
    pub bitmap: BitmapTextBackend<'a>,
}

impl<'a> SelectingTextBackend<'a> {
    pub fn new(
        engine: Option<&'a EngineTextBackend>,
        fonts: &'a BTreeMap<String, FontAtlas>,
        textures: &'a BTreeMap<String, Arc<Texture>>,
    ) -> Self {
        Self {
            engine,
            bitmap: BitmapTextBackend::new(fonts, textures),
        }
    }

    /// Whether `font` is drawn by the shaping engine.
    pub fn uses_engine(&self, font: &str) -> bool {
        self.engine.is_some_and(|e| e.resolves(font))
    }

    fn pick(&self, font: &str) -> Option<&'a EngineTextBackend> {
        self.engine.filter(|e| e.resolves(font))
    }
}

impl UiTextBackend for SelectingTextBackend<'_> {
    fn measure(&self, request: &UiTextRequest<'_>) -> Result<UiTextMetrics> {
        match self.pick(request.font) {
            Some(engine) => engine.measure(request),
            None => self.bitmap.measure(request),
        }
    }
    fn layout(&self, request: &UiTextRequest<'_>) -> Result<UiTextLayout> {
        match self.pick(request.font) {
            Some(engine) => engine.layout(request),
            None => self.bitmap.layout(request),
        }
    }
    fn begin_frame(&self) {
        if let Some(engine) = self.engine {
            engine.begin_frame();
        }
    }
    fn set_sdf_output(&self, enabled: bool) {
        if let Some(engine) = self.engine {
            UiTextBackend::set_sdf_output(engine, enabled);
        }
    }
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
            ..Default::default()
        })
    }
}

fn plain_text<'a>(request: &UiTextRequest<'a>) -> std::borrow::Cow<'a, str> {
    match request.kind {
        UiTextKind::Plain => request.text.into(),
        UiTextKind::Markup => strip_markup(request.text).into(),
    }
}

/// The text a screen reader should read for rich-label markup: the bracket
/// markup of `engine-text` without its tags and with its escapes resolved,
/// and the bitmap dialect's `<...>` tags and entities removed as well (see
/// [`strip_markup`]). Unknown tags stay literal, as they do when drawn. Markup
/// that does not parse (a known tag with a bad value, unbalanced tags) falls
/// back to dropping every `[...]` group so the name never shows raw tags.
pub(crate) fn plain_markup_text(markup: &str) -> String {
    let bracketed = if markup.contains(['[', '\\']) {
        let options = engine_text::MarkupOptions {
            unknown_tags: engine_text::UnknownTagPolicy::Literal,
            ..engine_text::MarkupOptions::default()
        };
        match engine_text::RichText::from_markup(markup, &options) {
            // Inline images leave an object-replacement placeholder behind.
            Ok(rich) => rich.text.replace('\u{fffc}', ""),
            Err(_) => strip_brackets(markup),
        }
    } else {
        markup.to_owned()
    };
    strip_markup(&bracketed)
}

/// Lenient fallback for markup that does not parse: drop every `[...]` group
/// and resolve the bracket and backslash escapes.
fn strip_brackets(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut chars = markup.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.clone().next() {
                Some(n @ ('[' | ']' | '\\')) => {
                    out.push(n);
                    chars.next();
                }
                _ => out.push('\\'),
            },
            '[' => {
                let rest = chars.as_str();
                match rest.find(']') {
                    Some(end) => chars = rest[end + 1..].chars(),
                    None => out.push('['),
                }
            }
            _ => out.push(c),
        }
    }
    out
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

#[cfg(test)]
mod plain_markup_tests {
    use super::plain_markup_text;

    #[test]
    fn unparseable_markup_never_leaks_raw_tags_into_plain_text() {
        assert_eq!(plain_markup_text("[b]open [size=9999]x"), "open x");
        assert_eq!(plain_markup_text("[b]Bold[/b]"), "Bold");
    }
}
