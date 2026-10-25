//! Glyph outlines and CPU rasterisation into coverage masks and signed distance fields.

use ab_glyph_rasterizer::{Rasterizer, point};
use glam::Vec2;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::TextError;

/// Most outline segments accepted for one glyph (after curve flattening for SDF).
pub const MAX_OUTLINE_SEGMENTS: usize = 16_384;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Segment {
    Line(Vec2, Vec2),
    Quad(Vec2, Vec2, Vec2),
    Cubic(Vec2, Vec2, Vec2, Vec2),
}

/// Glyph outline in pixels, y-down, origin at the pen position.
pub(crate) struct Outline {
    pub segments: Vec<Segment>,
    pub min: Vec2,
    pub max: Vec2,
}

struct Pen {
    skew: f32,
    segments: Vec<Segment>,
    start: Vec2,
    current: Vec2,
    overflow: bool,
}

impl Pen {
    fn map(&self, x: f32, y: f32) -> Vec2 {
        Vec2::new(x + y * self.skew, -y)
    }

    fn push(&mut self, segment: Segment) {
        if self.segments.len() >= MAX_OUTLINE_SEGMENTS {
            self.overflow = true;
        } else {
            self.segments.push(segment);
        }
    }

    fn close_contour(&mut self) {
        if self.current != self.start {
            self.push(Segment::Line(self.current, self.start));
            self.current = self.start;
        }
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_contour();
        self.start = self.map(x, y);
        self.current = self.start;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.push(Segment::Line(self.current, p));
        self.current = p;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.map(cx0, cy0), self.map(x, y));
        self.push(Segment::Quad(self.current, c, p));
        self.current = p;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.map(cx0, cy0), self.map(cx1, cy1), self.map(x, y));
        self.push(Segment::Cubic(self.current, c0, c1, p));
        self.current = p;
    }

    fn close(&mut self) {
        self.close_contour();
    }
}

/// Extracts the outline of `glyph` at `size_px`; `Ok(None)` for empty glyphs (spaces).
pub(crate) fn glyph_outline(
    font: &FontRef<'_>,
    glyph: u32,
    size_px: f32,
    skew: f32,
) -> Result<Option<Outline>, TextError> {
    let glyphs = font.outline_glyphs();
    let Some(outline) = glyphs.get(GlyphId::new(glyph)) else {
        return Ok(None);
    };
    let mut pen = Pen {
        skew,
        segments: Vec::new(),
        start: Vec2::ZERO,
        current: Vec2::ZERO,
        overflow: false,
    };
    let settings = DrawSettings::unhinted(Size::new(size_px), LocationRef::default());
    if outline.draw(settings, &mut pen).is_err() {
        // A malformed glyph renders as empty rather than failing the whole text.
        return Ok(None);
    }
    pen.close_contour();
    if pen.overflow {
        return Err(TextError::GlyphTooComplex {
            segments: MAX_OUTLINE_SEGMENTS + 1,
            max: MAX_OUTLINE_SEGMENTS,
        });
    }
    let mut points = pen.segments.iter().flat_map(|s| match *s {
        Segment::Line(a, b) => vec![a, b],
        Segment::Quad(a, b, c) => vec![a, b, c],
        Segment::Cubic(a, b, c, d) => vec![a, b, c, d],
    });
    let Some(first) = points.next() else {
        return Ok(None);
    };
    let (min, max) = points.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
    if !(min.is_finite() && max.is_finite()) {
        return Ok(None);
    }
    Ok(Some(Outline {
        segments: pen.segments,
        min,
        max,
    }))
}

/// Single-channel bitmap plus its placement relative to the pen (y-down pixels).
pub(crate) struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub alpha: Vec<u8>,
}

fn bitmap_box(
    min: Vec2,
    max: Vec2,
    margin_lo: f32,
    margin_hi: Vec2,
    max_dim: u32,
) -> Result<(i32, i32, u32, u32), TextError> {
    let x0 = (min.x - margin_lo).floor();
    let y0 = (min.y - margin_lo).floor();
    let x1 = (max.x + margin_hi.x).ceil();
    let y1 = (max.y + margin_hi.y).ceil();
    let (w, h) = (x1 - x0, y1 - y0);
    if !(w.is_finite() && h.is_finite()) || w > max_dim as f32 || h > max_dim as f32 {
        return Err(TextError::GlyphTooLarge {
            width: w.clamp(0.0, u32::MAX as f32) as u32,
            height: h.clamp(0.0, u32::MAX as f32) as u32,
            page: max_dim,
        });
    }
    Ok((x0 as i32, y0 as i32, (w as u32).max(1), (h as u32).max(1)))
}

/// Anti-aliased coverage at the outline's exact pixel size, with a `pad`-pixel empty border.
/// Synthetic bold double-strikes the outline `bold` pixels to the right.
pub(crate) fn rasterize_coverage(
    outline: &Outline,
    bold: f32,
    pad: u32,
    max_dim: u32,
) -> Result<GlyphBitmap, TextError> {
    let pad = pad as f32;
    let (left, top, width, height) = bitmap_box(
        outline.min,
        outline.max,
        pad,
        Vec2::new(pad + bold, pad),
        max_dim,
    )?;
    let mut raster = Rasterizer::new(width as usize, height as usize);
    let strikes: &[f32] = if bold > 0.0 { &[0.0, bold] } else { &[0.0] };
    for &dx in strikes {
        let at = |p: Vec2| point(p.x - left as f32 + dx, p.y - top as f32);
        for segment in &outline.segments {
            match *segment {
                Segment::Line(a, b) => raster.draw_line(at(a), at(b)),
                Segment::Quad(a, b, c) => raster.draw_quad(at(a), at(b), at(c)),
                Segment::Cubic(a, b, c, d) => raster.draw_cubic(at(a), at(b), at(c), at(d)),
            }
        }
    }
    let mut alpha = vec![0u8; (width * height) as usize];
    raster.for_each_pixel_2d(|x, y, a| {
        let i = (y * width + x) as usize;
        if let Some(px) = alpha.get_mut(i) {
            *px = (a.abs().min(1.0) * 255.0).round() as u8;
        }
    });
    // The rasteriser's accumulation can leak a tiny value into the padding; the border is
    // guaranteed empty so neighbouring glyphs never bleed under bilinear sampling.
    clear_border(&mut alpha, width, height, pad as u32);
    Ok(GlyphBitmap {
        width,
        height,
        left,
        top,
        alpha,
    })
}

fn clear_border(alpha: &mut [u8], width: u32, height: u32, pad: u32) {
    for y in 0..height {
        for x in 0..width {
            if x < pad
                || y < pad
                || x >= width.saturating_sub(pad)
                || y >= height.saturating_sub(pad)
            {
                alpha[(y * width + x) as usize] = 0;
            }
        }
    }
}

fn flatten(segments: &[Segment], tolerance: f32) -> Result<Vec<(Vec2, Vec2)>, TextError> {
    let mut lines = Vec::with_capacity(segments.len() * 4);
    let steps = |control_len: f32| ((control_len / tolerance).sqrt().ceil() as usize).clamp(1, 32);
    for segment in segments {
        match *segment {
            Segment::Line(a, b) => lines.push((a, b)),
            Segment::Quad(a, b, c) => {
                let n = steps((a - 2.0 * b + c).length());
                let mut prev = a;
                for k in 1..=n {
                    let t = k as f32 / n as f32;
                    let p = a * (1.0 - t) * (1.0 - t) + b * 2.0 * t * (1.0 - t) + c * t * t;
                    lines.push((prev, p));
                    prev = p;
                }
            }
            Segment::Cubic(a, b, c, d) => {
                let n = steps((a - 2.0 * b + c).length().max((b - 2.0 * c + d).length()) * 1.5);
                let mut prev = a;
                for k in 1..=n {
                    let t = k as f32 / n as f32;
                    let u = 1.0 - t;
                    let p =
                        a * u * u * u + b * 3.0 * u * u * t + c * 3.0 * u * t * t + d * t * t * t;
                    lines.push((prev, p));
                    prev = p;
                }
            }
        }
        if lines.len() > MAX_OUTLINE_SEGMENTS * 4 {
            return Err(TextError::GlyphTooComplex {
                segments: lines.len(),
                max: MAX_OUTLINE_SEGMENTS * 4,
            });
        }
    }
    Ok(lines)
}

fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    let t = if len2 > 0.0 {
        ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + ab * t)).length_squared()
}

/// True signed distance field: alpha = 0.5 + d / (2 * spread), inside positive, edge at 128.
/// Inside/outside uses the non-zero winding rule on the flattened outline; `bold` grows the
/// shape by that many pixels.
pub(crate) fn rasterize_sdf(
    outline: &Outline,
    spread: f32,
    bold: f32,
    max_dim: u32,
) -> Result<GlyphBitmap, TextError> {
    let margin = spread.ceil() + 1.0 + bold;
    let (left, top, width, height) = bitmap_box(
        outline.min,
        outline.max,
        margin,
        Vec2::splat(margin),
        max_dim,
    )?;
    let lines = flatten(&outline.segments, 0.2)?;
    let mut alpha = vec![0u8; (width * height) as usize];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for y in 0..height {
        let py = top as f32 + y as f32 + 0.5;
        crossings.clear();
        for &(a, b) in &lines {
            if (a.y <= py) != (b.y <= py) {
                let t = (py - a.y) / (b.y - a.y);
                crossings.push((a.x + t * (b.x - a.x), if b.y > a.y { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        for x in 0..width {
            let p = Vec2::new(left as f32 + x as f32 + 0.5, py);
            let winding: i32 = crossings
                .iter()
                .take_while(|c| c.0 < p.x)
                .map(|c| c.1)
                .sum();
            let d2 = lines
                .iter()
                .map(|&(a, b)| distance_to_segment(p, a, b))
                .fold(f32::INFINITY, f32::min);
            let d = d2.sqrt();
            let signed = if winding != 0 { d } else { -d } + bold;
            let value = (0.5 + signed / (2.0 * spread)).clamp(0.0, 1.0);
            alpha[(y * width + x) as usize] = (value * 255.0).round() as u8;
        }
    }
    Ok(GlyphBitmap {
        width,
        height,
        left,
        top,
        alpha,
    })
}
