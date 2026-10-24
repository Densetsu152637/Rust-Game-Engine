//! Itemisation (style, bidi level, script, face) and shaping into logical clusters.

use std::collections::HashMap;
use std::ops::Range;

use skrifa::charmap::Charmap;
use skrifa::instance::{LocationRef, Size};
use skrifa::{GlyphId, MetadataProvider};
use unicode_script::{Script as UScript, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

use crate::font::{FaceMatch, SYNTHETIC_BOLD_EM};
use crate::{Color, FaceId, FontLibrary, FontStyle, RichText, TextError, weight};

/// Fully resolved style of one span (or of the base style).
#[derive(Clone, Debug)]
pub(crate) struct ResolvedStyle {
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub style: FontStyle,
    pub color: Color,
    pub underline: bool,
    pub link: Option<String>,
    pub image: Option<(f32, f32)>,
    /// Index into `RichText::spans`, `None` for the base style.
    pub span: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClusterKind {
    Text,
    /// Breaking whitespace that hangs past the line edge.
    Whitespace,
    Newline,
    Tab,
    Image,
    Ellipsis,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ShapedGlyph {
    pub glyph_id: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    /// Y-down offset from the baseline.
    pub y_offset: f32,
}

/// Smallest unit of layout and caret placement: one or more whole graphemes shaped together.
#[derive(Clone, Debug)]
pub(crate) struct Cluster {
    pub range: Range<usize>,
    pub glyphs: Range<usize>,
    pub advance: f32,
    pub level: u8,
    pub style: usize,
    pub face: FaceMatch,
    pub size: f32,
    pub kind: ClusterKind,
    pub ascent: f32,
    pub descent: f32,
    pub gap: f32,
    pub underline_offset: f32,
    pub underline_thickness: f32,
}

pub(crate) struct Shaped {
    pub clusters: Vec<Cluster>,
    pub glyphs: Vec<ShapedGlyph>,
}

pub(crate) fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

pub(crate) fn is_hanging_space(c: char) -> bool {
    c.is_whitespace() && !is_newline(c) && !matches!(c, '\u{A0}' | '\u{2007}' | '\u{202F}' | '\t')
}

/// Characters that never decide face coverage (joiners, selectors, bidi controls).
fn is_ignorable(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{E0000}'..='\u{E01EF}'
        | '\u{FEFF}')
}

fn real_script(c: char) -> Option<UScript> {
    match c.script() {
        UScript::Common | UScript::Inherited | UScript::Unknown => None,
        s => Some(s),
    }
}

/// One memoised `(weight, style)` request for a family and its result.
type MatchEntry = ((u16, FontStyle), Option<FaceMatch>);

/// Per-layout caches: character maps per face and family matches per request.
pub(crate) struct CoverageCache<'a> {
    library: &'a FontLibrary,
    maps: HashMap<FaceId, Option<Charmap<'a>>>,
    matches: HashMap<String, Vec<MatchEntry>>,
}

impl<'a> CoverageCache<'a> {
    pub fn new(library: &'a FontLibrary) -> Self {
        Self {
            library,
            maps: HashMap::new(),
            matches: HashMap::new(),
        }
    }

    fn charmap(&mut self, face: FaceId) -> Option<&Charmap<'a>> {
        let library = self.library;
        self.maps
            .entry(face)
            .or_insert_with(|| {
                let face = library.face(face)?;
                face.font_ref().ok().map(|font| font.charmap())
            })
            .as_ref()
    }

    pub fn covers(&mut self, face: FaceId, text: &str) -> bool {
        let Some(map) = self.charmap(face) else {
            return false;
        };
        text.chars()
            .filter(|c| !is_ignorable(*c))
            .all(|c| map.map(c).is_some())
    }

    pub fn glyph(&mut self, face: FaceId, c: char) -> Option<GlyphId> {
        self.charmap(face)?.map(c)
    }

    /// Memoised [`FontLibrary::match_face`].
    pub fn match_face(&mut self, family: &str, weight: u16, style: FontStyle) -> Option<FaceMatch> {
        let request = (weight, style);
        if let Some(found) = self
            .matches
            .get(family)
            .and_then(|list| list.iter().find(|(r, _)| *r == request))
        {
            return found.1;
        }
        let found = self.library.match_face(family, weight, style);
        self.matches
            .entry(family.to_string())
            .or_default()
            .push((request, found));
        found
    }
}

struct Grapheme {
    range: Range<usize>,
    style: usize,
    level: u8,
    script: Option<UScript>,
    kind: ClusterKind,
    face: FaceMatch,
}

/// Chooses the face for one grapheme: requested family, then language, script and default
/// fallbacks; the first face whose cmap covers every significant character wins. When none
/// covers it, the requested family's face is used (the shaper emits `.notdef`).
pub(crate) fn choose_face(
    library: &FontLibrary,
    cache: &mut CoverageCache<'_>,
    style: &ResolvedStyle,
    language: Option<&str>,
    script: Option<UScript>,
    text: &str,
) -> Result<FaceMatch, TextError> {
    let primary = cache.match_face(&style.family, style.weight, style.style);
    if let Some(found) = primary
        && cache.covers(found.face, text)
    {
        return Ok(found);
    }
    let mut first = primary;
    let script_tag = script.map(|s| s.short_name()).unwrap_or("Zyyy");
    for family in library.fallback().chain(language, script_tag) {
        let Some(found) = cache.match_face(family, style.weight, style.style) else {
            continue;
        };
        first.get_or_insert(found);
        if cache.covers(found.face, text) {
            return Ok(found);
        }
    }
    first
        .or_else(|| library.any_face())
        .ok_or(TextError::NoFonts)
}

pub(crate) fn primary_face(
    library: &FontLibrary,
    style: &ResolvedStyle,
) -> Result<FaceMatch, TextError> {
    library
        .match_face(&style.family, style.weight, style.style)
        .or_else(|| library.any_face())
        .ok_or(TextError::NoFonts)
}

pub(crate) fn resolve_styles(rich: &RichText, base: &crate::TextStyle) -> Vec<ResolvedStyle> {
    let base_resolved = ResolvedStyle {
        family: base.family.clone(),
        size: base.size,
        weight: base.weight,
        style: base.style,
        color: base.color,
        underline: false,
        link: None,
        image: None,
        span: None,
    };
    let mut out: Vec<ResolvedStyle> = rich
        .spans
        .iter()
        .enumerate()
        .map(|(i, span)| {
            let s = &span.style;
            ResolvedStyle {
                family: s.family.clone().unwrap_or_else(|| base.family.clone()),
                size: s.size.unwrap_or(base.size),
                weight: if s.bold {
                    base.weight.max(weight::BOLD)
                } else {
                    base.weight
                },
                style: if s.italic {
                    FontStyle::Italic
                } else {
                    base.style
                },
                color: s.color.unwrap_or(base.color),
                underline: s.underline,
                link: s.link.clone(),
                image: s.image.as_ref().map(|im| (im.width, im.height)),
                span: Some(i),
            }
        })
        .collect();
    out.push(base_resolved);
    out
}

/// Scaled vertical metrics of a face.
fn scaled_metrics(library: &FontLibrary, face: FaceId, size: f32) -> crate::FaceMetrics {
    library
        .face_metrics(face)
        .map(|m| m.scaled(size))
        .unwrap_or(crate::FaceMetrics {
            units_per_em: 1.0,
            ascent: size * 0.8,
            descent: size * 0.2,
            line_gap: 0.0,
            underline_offset: size * 0.1,
            underline_thickness: size * 0.05,
        })
}

pub(crate) struct ShapeInput<'t> {
    pub text: &'t str,
    pub rich: &'t RichText,
    pub styles: &'t [ResolvedStyle],
    pub levels: &'t [u8],
    pub language: Option<&'t str>,
    pub max_glyphs: usize,
}

pub(crate) fn shape(library: &FontLibrary, input: &ShapeInput<'_>) -> Result<Shaped, TextError> {
    let text = input.text;
    let base_style = input.styles.len() - 1;
    let mut cache = CoverageCache::new(library);
    let mut graphemes: Vec<Grapheme> = Vec::new();
    for (start, g) in text.grapheme_indices(true) {
        let style = input.rich.span_at(start).unwrap_or(base_style);
        let first = g.chars().next().unwrap_or(' ');
        let kind = if input.styles[style].image.is_some() && first == crate::OBJECT_REPLACEMENT {
            ClusterKind::Image
        } else if is_newline(first) {
            ClusterKind::Newline
        } else if first == '\t' {
            ClusterKind::Tab
        } else if g.chars().all(is_hanging_space) {
            ClusterKind::Whitespace
        } else {
            ClusterKind::Text
        };
        graphemes.push(Grapheme {
            range: start..start + g.len(),
            style,
            level: input.levels.get(start).copied().unwrap_or(0),
            script: g.chars().find_map(real_script),
            kind,
            face: FaceMatch {
                face: FaceId(0),
                synthesis: Default::default(),
            },
        });
    }
    resolve_common_scripts(&mut graphemes);
    for g in &mut graphemes {
        let style = &input.styles[g.style];
        g.face = match g.kind {
            ClusterKind::Text | ClusterKind::Whitespace => choose_face(
                library,
                &mut cache,
                style,
                input.language,
                g.script,
                &text[g.range.clone()],
            )?,
            _ => primary_face(library, style)?,
        };
    }

    let mut shaped = Shaped {
        clusters: Vec::new(),
        glyphs: Vec::new(),
    };
    let mut i = 0;
    while i < graphemes.len() {
        let g = &graphemes[i];
        let style = &input.styles[g.style];
        if !matches!(g.kind, ClusterKind::Text | ClusterKind::Whitespace) {
            let metrics = scaled_metrics(library, g.face.face, style.size);
            let (advance, ascent, descent) = match g.kind {
                ClusterKind::Tab => {
                    let space = space_advance(library, &mut cache, g.face.face, style.size);
                    (space * 4.0, metrics.ascent, metrics.descent)
                }
                ClusterKind::Image => {
                    let (w, h) = style.image.unwrap_or((0.0, 0.0));
                    (w, h, 0.0)
                }
                _ => (0.0, metrics.ascent, metrics.descent),
            };
            shaped.clusters.push(Cluster {
                range: g.range.clone(),
                glyphs: shaped.glyphs.len()..shaped.glyphs.len(),
                advance,
                level: g.level,
                style: g.style,
                face: g.face,
                size: style.size,
                kind: g.kind,
                ascent,
                descent,
                gap: if g.kind == ClusterKind::Image {
                    0.0
                } else {
                    metrics.line_gap
                },
                underline_offset: metrics.underline_offset,
                underline_thickness: metrics.underline_thickness,
            });
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < graphemes.len() {
            let n = &graphemes[j];
            let same = matches!(n.kind, ClusterKind::Text | ClusterKind::Whitespace)
                && n.face == g.face
                && input.styles[n.style].size.to_bits() == style.size.to_bits()
                && n.level == g.level
                && n.script == g.script;
            if !same {
                break;
            }
            j += 1;
        }
        let run = graphemes[i].range.start..graphemes[j - 1].range.end;
        shape_run(library, input, &graphemes[i..j], run, &mut shaped)?;
        if shaped.glyphs.len() > input.max_glyphs {
            return Err(TextError::TooManyGlyphs {
                max: input.max_glyphs,
            });
        }
        i = j;
    }
    Ok(shaped)
}

/// Common/Inherited graphemes take the script of the preceding run (or the following one at
/// the start of the text) so punctuation and digits shape and fall back with their context.
fn resolve_common_scripts(graphemes: &mut [Grapheme]) {
    let mut last = None;
    for g in graphemes.iter_mut() {
        match g.script {
            Some(s) => last = Some(s),
            None => g.script = last,
        }
    }
    let mut next = None;
    for g in graphemes.iter_mut().rev() {
        match g.script {
            Some(s) => next = Some(s),
            None => g.script = next,
        }
    }
}

fn space_advance(
    library: &FontLibrary,
    cache: &mut CoverageCache<'_>,
    face: FaceId,
    size: f32,
) -> f32 {
    let advance = cache.glyph(face, ' ').and_then(|gid| {
        let font = library.face(face)?.font_ref().ok()?;
        font.glyph_metrics(Size::new(size), LocationRef::default())
            .advance_width(gid)
    });
    advance.unwrap_or(size * 0.25)
}

fn shape_run(
    library: &FontLibrary,
    input: &ShapeInput<'_>,
    graphemes: &[Grapheme],
    run: Range<usize>,
    shaped: &mut Shaped,
) -> Result<(), TextError> {
    let text = input.text;
    let first = &graphemes[0];
    let style = &input.styles[first.style];
    let face = library.face(first.face.face).ok_or(TextError::NoFonts)?;
    let font = face.font_ref()?;
    let shaper = face.shaper_data.shaper(&font).build();
    let rtl = first.level % 2 == 1;

    let mut buffer = harfrust::UnicodeBuffer::new();
    for (offset, ch) in text[run.clone()].char_indices() {
        buffer.add(ch, (run.start + offset) as u32);
    }
    let context_start = floor_char_boundary(text, run.start.saturating_sub(32));
    let context_end = ceil_char_boundary(text, (run.end + 32).min(text.len()));
    let pre = &text[context_start..run.start];
    let post = &text[run.end..context_end];
    let pre = pre.rsplit(is_newline).next().unwrap_or("");
    let post = post.split(is_newline).next().unwrap_or("");
    buffer.set_pre_context(pre);
    buffer.set_post_context(post);
    let mut flags = harfrust::BufferFlags::empty();
    if text[..run.start].chars().next_back().is_none_or(is_newline) {
        flags |= harfrust::BufferFlags::BEGINNING_OF_TEXT;
    }
    if text[run.end..].chars().next().is_none_or(is_newline) {
        flags |= harfrust::BufferFlags::END_OF_TEXT;
    }
    buffer.set_flags(flags);
    buffer.set_direction(if rtl {
        harfrust::Direction::RightToLeft
    } else {
        harfrust::Direction::LeftToRight
    });
    if let Some(script) = first.script.and_then(|s| {
        let tag = s.short_name().as_bytes();
        let tag: [u8; 4] = tag.try_into().ok()?;
        harfrust::Script::from_iso15924_tag(harfrust::Tag::new(&tag))
    }) {
        buffer.set_script(script);
    }
    if let Some(language) = input
        .language
        .and_then(|l| l.parse::<harfrust::Language>().ok())
    {
        buffer.set_language(language);
    }
    buffer.guess_segment_properties();
    let output = shaper.shape(buffer, harfrust::ShapeOptions::new());

    let scale = style.size / face.metrics.units_per_em;
    let bold_extra = if first.face.synthesis.bold {
        style.size * SYNTHETIC_BOLD_EM
    } else {
        0.0
    };
    // Group glyphs by cluster value; harfrust emits RTL runs in visual (reversed) order.
    let mut groups: Vec<(usize, Vec<ShapedGlyph>)> = Vec::new();
    for (info, pos) in output.glyph_infos().iter().zip(output.glyph_positions()) {
        let glyph = ShapedGlyph {
            glyph_id: info.glyph_id,
            x_advance: pos.x_advance as f32 * scale
                + if pos.x_advance != 0 { bold_extra } else { 0.0 },
            x_offset: pos.x_offset as f32 * scale,
            y_offset: -(pos.y_offset as f32) * scale,
        };
        let cluster = info.cluster as usize;
        match groups.last_mut() {
            Some((start, glyphs)) if *start == cluster => glyphs.push(glyph),
            _ => groups.push((cluster, vec![glyph])),
        }
    }
    if rtl {
        groups.reverse();
    }
    // Defensive normalisation: clusters must be strictly increasing and inside the run.
    groups.sort_by_key(|(start, _)| *start);
    let mut merged: Vec<(usize, Vec<ShapedGlyph>)> = Vec::with_capacity(groups.len());
    for (start, glyphs) in groups {
        let start = start.clamp(run.start, run.end);
        match merged.last_mut() {
            Some((s, g)) if *s == start => {
                if rtl {
                    let mut glyphs = glyphs;
                    glyphs.append(g);
                    *g = glyphs;
                } else {
                    g.extend(glyphs);
                }
            }
            _ => merged.push((start, glyphs)),
        }
    }
    if let Some(first_group) = merged.first_mut() {
        first_group.0 = run.start;
    }
    let metrics = face.metrics.scaled(style.size);
    for (k, (start, glyphs)) in merged.iter().enumerate() {
        let end = merged.get(k + 1).map(|(s, _)| *s).unwrap_or(run.end);
        // The span style of a cluster is the style of the grapheme it starts in.
        let grapheme = graphemes
            .partition_point(|g| g.range.end <= *start)
            .min(graphemes.len() - 1);
        let style_index = graphemes[grapheme].style;
        let kind = if text[*start..end].chars().all(is_hanging_space) {
            ClusterKind::Whitespace
        } else {
            ClusterKind::Text
        };
        let glyph_start = shaped.glyphs.len();
        shaped.glyphs.extend_from_slice(glyphs);
        shaped.clusters.push(Cluster {
            range: *start..end,
            glyphs: glyph_start..shaped.glyphs.len(),
            advance: glyphs.iter().map(|g| g.x_advance).sum(),
            level: first.level,
            style: style_index,
            face: first.face,
            size: style.size,
            kind,
            ascent: metrics.ascent,
            descent: metrics.descent,
            gap: metrics.line_gap,
            underline_offset: metrics.underline_offset,
            underline_thickness: metrics.underline_thickness,
        });
    }
    Ok(())
}

/// Shapes a standalone string (used for the ellipsis) with one face, LTR or RTL.
pub(crate) fn shape_simple(
    library: &FontLibrary,
    face: FaceMatch,
    size: f32,
    text: &str,
    rtl: bool,
) -> Result<Vec<ShapedGlyph>, TextError> {
    let face_data = library.face(face.face).ok_or(TextError::NoFonts)?;
    let font = face_data.font_ref()?;
    let shaper = face_data.shaper_data.shaper(&font).build();
    let mut buffer = harfrust::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(if rtl {
        harfrust::Direction::RightToLeft
    } else {
        harfrust::Direction::LeftToRight
    });
    buffer.guess_segment_properties();
    let output = shaper.shape(buffer, harfrust::ShapeOptions::new());
    let scale = size / face_data.metrics.units_per_em;
    let bold_extra = if face.synthesis.bold {
        size * SYNTHETIC_BOLD_EM
    } else {
        0.0
    };
    Ok(output
        .glyph_infos()
        .iter()
        .zip(output.glyph_positions())
        .map(|(info, pos)| ShapedGlyph {
            glyph_id: info.glyph_id,
            x_advance: pos.x_advance as f32 * scale + bold_extra,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: -(pos.y_offset as f32) * scale,
        })
        .collect())
}

pub(crate) fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub(crate) fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hanging_space_excludes_no_break_spaces_and_newlines() {
        assert!(is_hanging_space(' '));
        assert!(is_hanging_space('\u{3000}'));
        assert!(!is_hanging_space('\u{A0}'));
        assert!(!is_hanging_space('\n'));
        assert!(!is_hanging_space('\t'));
    }

    #[test]
    fn char_boundary_helpers_stay_on_boundaries() {
        let text = "aé€";
        assert_eq!(floor_char_boundary(text, 2), 1);
        assert_eq!(ceil_char_boundary(text, 2), 3);
        assert_eq!(ceil_char_boundary(text, 99), text.len());
    }
}
