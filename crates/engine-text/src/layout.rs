//! Paragraph layout: bidi levels, line breaking, ellipsis, visual reordering and positioning.

use std::ops::Range;

use unicode_bidi::{BidiInfo, Level};
use unicode_segmentation::UnicodeSegmentation;

use crate::shape::{self, Cluster, ClusterKind, ShapeInput, ShapedGlyph};
use crate::{Color, FaceId, FontLibrary, FontStyle, Rect, RichText, Synthesis, TextError, weight};

/// Base style applied to text outside spans and inherited by span overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: String,
    /// Font size in pixels.
    pub size: f32,
    pub color: Color,
    pub weight: u16,
    pub style: FontStyle,
}

impl TextStyle {
    pub fn new(family: impl Into<String>, size: f32) -> Self {
        Self {
            family: family.into(),
            size,
            color: Color::WHITE,
            weight: weight::REGULAR,
            style: FontStyle::Normal,
        }
    }
}

/// Horizontal alignment; `Start`/`End` follow each paragraph's resolved direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Start,
    End,
    Left,
    Right,
    Center,
}

/// Line box height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum LineHeight {
    /// Font ascent + descent + line gap (largest on the line).
    #[default]
    Normal,
    /// Multiple of the largest font size on the line (CSS unitless line-height).
    Relative(f32),
    /// Fixed pixels.
    Absolute(f32),
}

/// What happens to lines beyond `max_lines`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Drop them.
    #[default]
    Clip,
    /// Drop them and end the last kept line with `…` (or `...` when the face lacks U+2026).
    Ellipsis,
}

/// Paragraph base direction (UAX #9 rules P2/P3 for `Auto`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BaseDirection {
    #[default]
    Auto,
    Ltr,
    Rtl,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutOptions {
    /// Wrap width in pixels; `None` disables wrapping (explicit newlines still break).
    pub max_width: Option<f32>,
    pub align: TextAlign,
    pub line_height: LineHeight,
    pub max_lines: Option<usize>,
    pub overflow: Overflow,
    pub direction: BaseDirection,
    /// BCP 47 language: selects shaping language features and per-language fallback fonts.
    pub language: Option<String>,
}

/// One glyph positioned on its baseline, y-down layout pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedGlyph {
    pub face: FaceId,
    pub glyph_id: u32,
    /// Pen position (glyph origin on the baseline), offsets already applied.
    pub x: f32,
    pub y: f32,
    /// Font size in pixels.
    pub size: f32,
    pub color: Color,
    /// Index into `RichText::spans`; `None` for base-style text and for the ellipsis' own span
    /// when it inherits the base style.
    pub span: Option<usize>,
    /// Source byte range of the cluster this glyph belongs to (empty for the ellipsis).
    pub cluster: Range<usize>,
    pub synthesis: Synthesis,
    pub line: usize,
}

/// Metrics of one laid-out line.
#[derive(Clone, Debug, PartialEq)]
pub struct LineMetrics {
    /// Source bytes on this line (includes a trailing newline; ends at the cut when ellipsised).
    pub range: Range<usize>,
    pub top: f32,
    pub height: f32,
    /// Baseline y.
    pub baseline: f32,
    pub ascent: f32,
    pub descent: f32,
    /// Left x of the visible content (trailing whitespace hangs outside it).
    pub left: f32,
    /// Visible width (excludes hanging trailing whitespace).
    pub width: f32,
    /// Paragraph direction is right-to-left.
    pub rtl: bool,
    /// Ends with an explicit newline.
    pub hard_break: bool,
    pub ellipsized: bool,
}

/// Clickable area of a `[link=id]` span on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRegion {
    pub id: String,
    pub range: Range<usize>,
    pub rect: Rect,
    pub line: usize,
}

/// Space reserved for an inline image (`rect` sits on the baseline).
#[derive(Clone, Debug, PartialEq)]
pub struct InlineImageRect {
    pub key: String,
    pub span: usize,
    pub range: Range<usize>,
    pub rect: Rect,
    pub line: usize,
}

/// Underline rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoration {
    pub rect: Rect,
    pub color: Color,
    pub line: usize,
}

/// Caret-relevant cluster geometry (logical order).
#[derive(Clone, Debug)]
pub(crate) struct CaretCluster {
    pub range: Range<usize>,
    pub x0: f32,
    pub x1: f32,
    pub rtl: bool,
    pub kind: ClusterKind,
}

#[derive(Clone, Debug)]
pub(crate) struct CaretLine {
    /// Logical cluster indices into `TextLayout::carets`.
    pub clusters: Range<usize>,
    /// Same clusters in visual left-to-right order.
    pub visual: Vec<usize>,
}

/// Result of [`FontLibrary::layout`].
#[derive(Clone, Debug)]
pub struct TextLayout {
    pub glyphs: Vec<PositionedGlyph>,
    pub lines: Vec<LineMetrics>,
    /// Union of the visible line boxes.
    pub bounds: Rect,
    pub links: Vec<LinkRegion>,
    pub images: Vec<InlineImageRect>,
    pub decorations: Vec<Decoration>,
    /// Lines were dropped by `max_lines`.
    pub truncated: bool,
    pub(crate) text: String,
    /// End of laid-out text (`text.len()` unless truncated).
    pub(crate) end: usize,
    pub(crate) carets: Vec<CaretCluster>,
    pub(crate) caret_lines: Vec<CaretLine>,
    pub(crate) graphemes: Vec<usize>,
}

impl TextLayout {
    /// The laid-out source text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte index where laid-out text ends (`text().len()` unless truncated).
    pub fn laid_out_end(&self) -> usize {
        self.end
    }
}

const EPSILON: f32 = 1e-3;

struct LineBuild {
    clusters: Range<usize>,
    hard_break: bool,
    ellipsis: Option<Cluster>,
}

impl FontLibrary {
    /// Lays out rich text. All bounds in [`crate::TextLimits`] are checked first.
    pub fn layout(
        &self,
        rich: &RichText,
        style: &TextStyle,
        options: &LayoutOptions,
    ) -> Result<TextLayout, TextError> {
        let limits = self.limits();
        rich.validate(limits)?;
        limits.check_size(style.size)?;
        validate_options(options)?;
        if self.face_count() == 0 {
            return Err(TextError::NoFonts);
        }
        let text = rich.text.as_str();
        let styles = shape::resolve_styles(rich, style);
        let default_level = match options.direction {
            BaseDirection::Auto => None,
            BaseDirection::Ltr => Some(Level::ltr()),
            BaseDirection::Rtl => Some(Level::rtl()),
        };
        let bidi = BidiInfo::new(text, default_level);
        let levels: Vec<u8> = bidi.levels.iter().map(|l| l.number()).collect();
        let paragraphs: Vec<(Range<usize>, bool)> = bidi
            .paragraphs
            .iter()
            .map(|p| (p.range.clone(), p.level.is_rtl()))
            .collect();
        let fallback_rtl = paragraphs
            .last()
            .map(|p| p.1)
            .unwrap_or(options.direction == BaseDirection::Rtl);
        let shaped = shape::shape(
            self,
            &ShapeInput {
                text,
                rich,
                styles: &styles,
                levels: &levels,
                language: options.language.as_deref(),
                max_glyphs: limits.max_glyphs,
            },
        )?;
        let mut clusters = shaped.clusters;
        let mut glyphs = shaped.glyphs;

        let mut allowed = vec![false; text.len() + 1];
        for (index, opportunity) in unicode_linebreak::linebreaks(text) {
            if opportunity == unicode_linebreak::BreakOpportunity::Allowed {
                allowed[index] = true;
            }
        }
        let mut lines = break_lines(&clusters, &allowed, options.max_width);
        let mut truncated = false;
        let mut end = text.len();
        if let Some(max_lines) = options.max_lines
            && lines.len() > max_lines
        {
            truncated = true;
            lines.truncate(max_lines);
            let last = lines.len() - 1;
            let line = &mut lines[last];
            if line.clusters.end > line.clusters.start
                && clusters[line.clusters.end - 1].kind == ClusterKind::Newline
            {
                line.clusters.end -= 1;
            }
            line.hard_break = false;
            if options.overflow == Overflow::Ellipsis {
                let para_rtl = paragraph_rtl(&paragraphs, &clusters, line, fallback_rtl);
                apply_ellipsis(
                    self,
                    line,
                    &clusters,
                    &mut glyphs,
                    &styles,
                    style,
                    para_rtl,
                    options.max_width,
                )?;
            }
            end = line_end_byte(line, &clusters);
        }

        let primary = shape::primary_face(self, &styles[styles.len() - 1])?;
        let primary_metrics = self
            .face_metrics(primary.face)
            .map(|m| m.scaled(style.size))
            .ok_or(TextError::NoFonts)?;

        let mut out = TextLayout {
            glyphs: Vec::new(),
            lines: Vec::new(),
            bounds: Rect::default(),
            links: Vec::new(),
            images: Vec::new(),
            decorations: Vec::new(),
            truncated,
            text: text.to_string(),
            end,
            carets: Vec::new(),
            caret_lines: Vec::new(),
            graphemes: text[..end]
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([end])
                .collect(),
        };

        // Visual order and unaligned x of every cluster (each cluster is on one line).
        let mut placed = Vec::with_capacity(lines.len());
        let mut cluster_x: Vec<f32> = vec![0.0; clusters.len()];
        for line in &mut lines {
            let rtl = paragraph_rtl(&paragraphs, &clusters, line, fallback_rtl);
            let mut indices: Vec<usize> = line.clusters.clone().collect();
            if let Some(ellipsis) = line.ellipsis.take() {
                clusters.push(ellipsis);
                cluster_x.push(0.0);
                indices.push(clusters.len() - 1);
            }
            let visual = reorder_visual(&indices, &line_levels(&indices, &clusters, rtl));
            let mut x = 0.0;
            for &i in &visual {
                cluster_x[i] = x;
                x += clusters[i].advance;
            }
            let hanging: f32 = indices
                .iter()
                .rev()
                .take_while(|&&i| {
                    matches!(
                        clusters[i].kind,
                        ClusterKind::Whitespace | ClusterKind::Newline
                    )
                })
                .map(|&i| clusters[i].advance)
                .sum();
            placed.push(PlacedLine {
                visual,
                visible: x - hanging,
                visible_left: if rtl { hanging } else { 0.0 },
                rtl,
            });
        }
        let box_width = options
            .max_width
            .unwrap_or_else(|| placed.iter().map(|p| p.visible).fold(0.0_f32, f32::max));

        let mut top = 0.0_f32;
        let mut bounds: Option<Rect> = None;
        for (line_index, (line, place)) in lines.iter().zip(&placed).enumerate() {
            let previous = line.clusters.start.checked_sub(1).map(|i| &clusters[i]);
            let vertical = line_vertical(&place.visual, &clusters, previous).unwrap_or((
                primary_metrics.ascent,
                primary_metrics.descent,
                primary_metrics.line_gap,
                style.size,
            ));
            let (ascent, descent, gap, max_size) = vertical;
            let height = match options.line_height {
                LineHeight::Normal => ascent + descent + gap,
                LineHeight::Relative(f) => f * max_size,
                LineHeight::Absolute(px) => px,
            };
            let baseline = top + (height - (ascent + descent)) * 0.5 + ascent;
            let target_left = match (options.align, place.rtl) {
                (TextAlign::Left, _) | (TextAlign::Start, false) | (TextAlign::End, true) => 0.0,
                (TextAlign::Right, _) | (TextAlign::Start, true) | (TextAlign::End, false) => {
                    box_width - place.visible
                }
                (TextAlign::Center, _) => (box_width - place.visible) * 0.5,
            };
            let shift = target_left - place.visible_left;

            // Caret geometry: logical clusters of the line (the ellipsis is not a caret stop).
            let caret_start = out.carets.len();
            for i in line.clusters.clone() {
                let c = &clusters[i];
                out.carets.push(CaretCluster {
                    range: c.range.clone(),
                    x0: cluster_x[i] + shift,
                    x1: cluster_x[i] + shift + c.advance,
                    rtl: c.level % 2 == 1,
                    kind: c.kind,
                });
            }
            let visual_carets = place
                .visual
                .iter()
                .filter(|&&i| line.clusters.contains(&i))
                .map(|&i| caret_start + (i - line.clusters.start))
                .collect();
            out.caret_lines.push(CaretLine {
                clusters: caret_start..out.carets.len(),
                visual: visual_carets,
            });

            for &i in &place.visual {
                let c = &clusters[i];
                let s = &styles[c.style];
                let mut pen = cluster_x[i] + shift;
                for g in &glyphs[c.glyphs.clone()] {
                    out.glyphs.push(PositionedGlyph {
                        face: c.face.face,
                        glyph_id: g.glyph_id,
                        x: pen + g.x_offset,
                        y: baseline + g.y_offset,
                        size: c.size,
                        color: s.color,
                        span: s.span,
                        cluster: c.range.clone(),
                        synthesis: c.face.synthesis,
                        line: line_index,
                    });
                    pen += g.x_advance;
                }
                if c.kind == ClusterKind::Image
                    && let Some(span) = s.span
                    && let Some(image) = &rich.spans[span].style.image
                {
                    out.images.push(InlineImageRect {
                        key: image.key.clone(),
                        span,
                        range: c.range.clone(),
                        rect: Rect::new(
                            cluster_x[i] + shift,
                            baseline - c.ascent,
                            c.advance,
                            c.ascent,
                        ),
                        line: line_index,
                    });
                }
            }
            let geometry = RunGeometry {
                x: &cluster_x,
                shift,
                top,
                height,
                baseline,
                line: line_index,
            };
            emit_runs(&mut out, &place.visual, &clusters, &styles, &geometry);

            let line_range = match (line.clusters.is_empty(), line.clusters.clone().last()) {
                (false, Some(last)) => {
                    clusters[line.clusters.start].range.start..clusters[last].range.end
                }
                _ => {
                    let at = previous.map(|c| c.range.end).unwrap_or(0);
                    at..at
                }
            };
            let left = place.visible_left + shift;
            out.lines.push(LineMetrics {
                range: line_range,
                top,
                height,
                baseline,
                ascent,
                descent,
                left,
                width: place.visible,
                rtl: place.rtl,
                hard_break: line.hard_break,
                ellipsized: place
                    .visual
                    .iter()
                    .any(|&i| clusters[i].kind == ClusterKind::Ellipsis),
            });
            let line_box = Rect::new(left, top, place.visible, height);
            bounds = Some(match bounds {
                Some(b) => b.union(&line_box),
                None => line_box,
            });
            top += height;
        }
        out.bounds = bounds.unwrap_or_default();
        Ok(out)
    }
}

struct PlacedLine {
    visual: Vec<usize>,
    visible: f32,
    visible_left: f32,
    rtl: bool,
}

/// Byte where a line's content ends; for an empty line, where the previous line ended.
fn line_end_byte(line: &LineBuild, clusters: &[Cluster]) -> usize {
    match line.clusters.clone().last() {
        Some(last) => clusters[last].range.end,
        None => line
            .clusters
            .start
            .checked_sub(1)
            .map(|i| clusters[i].range.end)
            .unwrap_or(0),
    }
}

/// Embedding levels for one line after UAX #9 rule L1: trailing whitespace, newlines and the
/// ellipsis, and every tab, take the paragraph level.
fn line_levels(indices: &[usize], clusters: &[Cluster], rtl: bool) -> Vec<u8> {
    let para_level = u8::from(rtl);
    let mut levels: Vec<u8> = indices.iter().map(|&i| clusters[i].level).collect();
    for (k, &i) in indices.iter().enumerate().rev() {
        match clusters[i].kind {
            ClusterKind::Whitespace | ClusterKind::Newline | ClusterKind::Ellipsis => {
                levels[k] = para_level
            }
            _ => break,
        }
    }
    for (k, &i) in indices.iter().enumerate() {
        if clusters[i].kind == ClusterKind::Tab {
            levels[k] = para_level;
        }
    }
    levels
}

/// Largest ascent, descent, gap and font size among a line's clusters; an empty line uses
/// the preceding (newline) cluster, or `None` for the very first empty line.
fn line_vertical(
    members: &[usize],
    clusters: &[Cluster],
    previous: Option<&Cluster>,
) -> Option<(f32, f32, f32, f32)> {
    if members.is_empty() {
        return previous.map(|c| (c.ascent, c.descent, c.gap, c.size));
    }
    Some(members.iter().map(|&i| &clusters[i]).fold(
        (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32),
        |(a, d, g, s), c| {
            (
                a.max(c.ascent),
                d.max(c.descent),
                g.max(c.gap),
                s.max(c.size),
            )
        },
    ))
}

fn validate_options(options: &LayoutOptions) -> Result<(), TextError> {
    let invalid = |name, reason| Err(TextError::InvalidOption { name, reason });
    if let Some(width) = options.max_width
        && !(width.is_finite() && width >= 0.0)
    {
        return invalid("max_width", "must be finite and >= 0");
    }
    match options.line_height {
        LineHeight::Relative(v) | LineHeight::Absolute(v)
            if !(v.is_finite() && v > 0.0 && v <= 10_000.0) =>
        {
            return invalid("line_height", "must be finite and in (0, 10000]");
        }
        _ => {}
    }
    if options.max_lines == Some(0) {
        return invalid("max_lines", "must be at least 1");
    }
    if let Some(language) = &options.language
        && (language.is_empty() || language.len() > 64)
    {
        return invalid("language", "must be 1..=64 bytes");
    }
    Ok(())
}

fn paragraph_rtl(
    paragraphs: &[(Range<usize>, bool)],
    clusters: &[Cluster],
    line: &LineBuild,
    fallback: bool,
) -> bool {
    let at = clusters
        .get(line.clusters.start)
        .map(|c| c.range.start)
        .or_else(|| {
            line.clusters
                .start
                .checked_sub(1)
                .map(|i| clusters[i].range.end)
        });
    at.and_then(|at| paragraphs.iter().find(|(r, _)| r.contains(&at)))
        .map(|p| p.1)
        .unwrap_or(fallback)
}

/// Greedy UAX #14 line filling. Whitespace hangs; a word longer than the width breaks at a
/// cluster (grapheme) boundary.
fn break_lines(clusters: &[Cluster], allowed: &[bool], max_width: Option<f32>) -> Vec<LineBuild> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut width = 0.0_f32;
    let mut i = 0;
    while i < clusters.len() {
        let c = &clusters[i];
        if c.kind == ClusterKind::Newline {
            lines.push(LineBuild {
                clusters: start..i + 1,
                hard_break: true,
                ellipsis: None,
            });
            start = i + 1;
            width = 0.0;
            i += 1;
            continue;
        }
        if let Some(max) = max_width
            && c.kind != ClusterKind::Whitespace
            && i > start
            && width + c.advance > max + EPSILON
        {
            let brk = (start + 1..=i)
                .rev()
                .find(|&k| {
                    allowed
                        .get(clusters[k].range.start)
                        .copied()
                        .unwrap_or(false)
                })
                .unwrap_or(i);
            lines.push(LineBuild {
                clusters: start..brk,
                hard_break: false,
                ellipsis: None,
            });
            start = brk;
            width = clusters[brk..i].iter().map(|c| c.advance).sum();
            continue;
        }
        width += c.advance;
        i += 1;
    }
    lines.push(LineBuild {
        clusters: start..clusters.len(),
        hard_break: false,
        ellipsis: None,
    });
    lines
}

/// UAX #9 L2: reverse every maximal run at or above each level, from the highest level down
/// to the lowest odd level.
fn reorder_visual(indices: &[usize], levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = indices.to_vec();
    let mut levels = levels.to_vec();
    let Some(&max) = levels.iter().max() else {
        return order;
    };
    let min_odd = levels
        .iter()
        .copied()
        .filter(|l| l % 2 == 1)
        .min()
        .unwrap_or(max + 1);
    let mut level = max;
    while level >= min_odd && level > 0 {
        let mut k = 0;
        while k < order.len() {
            if levels[k] >= level {
                let run_start = k;
                while k < order.len() && levels[k] >= level {
                    k += 1;
                }
                order[run_start..k].reverse();
                levels[run_start..k].reverse();
            } else {
                k += 1;
            }
        }
        level -= 1;
    }
    order
}

#[allow(clippy::too_many_arguments)]
fn apply_ellipsis(
    library: &FontLibrary,
    line: &mut LineBuild,
    clusters: &[Cluster],
    glyphs: &mut Vec<ShapedGlyph>,
    styles: &[shape::ResolvedStyle],
    base: &TextStyle,
    para_rtl: bool,
    max_width: Option<f32>,
) -> Result<(), TextError> {
    let anchor = line
        .clusters
        .clone()
        .rev()
        .map(|i| &clusters[i])
        .find(|c| matches!(c.kind, ClusterKind::Text | ClusterKind::Whitespace));
    let (face, size, style, ascent, descent, gap, uo, ut) = match anchor {
        Some(c) => (
            c.face,
            c.size,
            c.style,
            c.ascent,
            c.descent,
            c.gap,
            c.underline_offset,
            c.underline_thickness,
        ),
        None => {
            let style = styles.len() - 1;
            let face = shape::primary_face(library, &styles[style])?;
            let m = library
                .face_metrics(face.face)
                .ok_or(TextError::NoFonts)?
                .scaled(base.size);
            (
                face,
                base.size,
                style,
                m.ascent,
                m.descent,
                m.line_gap,
                m.underline_offset,
                m.underline_thickness,
            )
        }
    };
    let ellipsis_text = if library.face_covers(face.face, '\u{2026}') {
        "\u{2026}"
    } else {
        "..."
    };
    let shaped = shape::shape_simple(library, face, size, ellipsis_text, para_rtl)?;
    let ellipsis_width: f32 = shaped.iter().map(|g| g.x_advance).sum();
    let visible = |range: &Range<usize>| -> f32 {
        let mut members: Vec<&Cluster> = range.clone().map(|i| &clusters[i]).collect();
        while members
            .last()
            .is_some_and(|c| matches!(c.kind, ClusterKind::Whitespace | ClusterKind::Newline))
        {
            members.pop();
        }
        members.iter().map(|c| c.advance).sum()
    };
    if let Some(max) = max_width {
        while line.clusters.end > line.clusters.start
            && visible(&line.clusters) + ellipsis_width > max + EPSILON
        {
            line.clusters.end -= 1;
        }
    }
    while line.clusters.end > line.clusters.start
        && matches!(
            clusters[line.clusters.end - 1].kind,
            ClusterKind::Whitespace | ClusterKind::Newline
        )
    {
        line.clusters.end -= 1;
    }
    let cut = line
        .clusters
        .clone()
        .last()
        .map(|i| clusters[i].range.end)
        .or_else(|| clusters.get(line.clusters.start).map(|c| c.range.start))
        .unwrap_or(0);
    let glyph_start = glyphs.len();
    glyphs.extend(shaped);
    line.ellipsis = Some(Cluster {
        range: cut..cut,
        glyphs: glyph_start..glyphs.len(),
        advance: ellipsis_width,
        level: u8::from(para_rtl),
        style,
        face,
        size,
        kind: ClusterKind::Ellipsis,
        ascent,
        descent,
        gap,
        underline_offset: uo,
        underline_thickness: ut,
    });
    Ok(())
}

/// Where one line's clusters sit: unaligned x per cluster plus the line's shift and box.
struct RunGeometry<'a> {
    x: &'a [f32],
    shift: f32,
    top: f32,
    height: f32,
    baseline: f32,
    line: usize,
}

/// Emits underline decorations and link regions for consecutive visual clusters that share
/// a span (a span split by bidi reordering yields several regions).
fn emit_runs(
    out: &mut TextLayout,
    members: &[usize],
    clusters: &[Cluster],
    styles: &[shape::ResolvedStyle],
    geometry: &RunGeometry<'_>,
) {
    let mut k = 0;
    while k < members.len() {
        let first = &clusters[members[k]];
        let style = &styles[first.style];
        let is_ellipsis = first.kind == ClusterKind::Ellipsis;
        let mut j = k + 1;
        while j < members.len()
            && clusters[members[j]].style == first.style
            && (clusters[members[j]].kind == ClusterKind::Ellipsis) == is_ellipsis
        {
            j += 1;
        }
        let run = &members[k..j];
        let last = &clusters[run[run.len() - 1]];
        let x0 = geometry.x[run[0]] + geometry.shift;
        let x1 = geometry.x[run[run.len() - 1]] + geometry.shift + last.advance;
        if style.underline && x1 > x0 {
            let offset = run
                .iter()
                .map(|&i| clusters[i].underline_offset)
                .fold(f32::MIN, f32::max);
            let thickness = run
                .iter()
                .map(|&i| clusters[i].underline_thickness)
                .fold(0.0, f32::max);
            out.decorations.push(Decoration {
                rect: Rect::new(x0, geometry.baseline + offset, x1 - x0, thickness),
                color: style.color,
                line: geometry.line,
            });
        }
        if let Some(id) = &style.link
            && x1 > x0
            && !is_ellipsis
        {
            let start = run
                .iter()
                .map(|&i| clusters[i].range.start)
                .min()
                .unwrap_or(0);
            let end = run
                .iter()
                .map(|&i| clusters[i].range.end)
                .max()
                .unwrap_or(0);
            out.links.push(LinkRegion {
                id: id.clone(),
                range: start..end,
                rect: Rect::new(x0, geometry.top, x1 - x0, geometry.height),
                line: geometry.line,
            });
        }
        k = j;
    }
}
