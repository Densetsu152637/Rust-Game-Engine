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

/// Most distance evaluations (pixels times candidate segments, plus one per segment per
/// bitmap row) one SDF glyph may cost. Typical glyphs use well under one percent of this.
pub const MAX_SDF_WORK: u64 = 100_000_000;
/// Most (cell, segment) pairs the SDF search grid may hold (bounds its memory).
const MAX_SDF_GRID_ENTRIES: u64 = 8_000_000;
/// Edge of a search-grid cell in pixels.
const SDF_CELL: u32 = 8;

/// Segments that can be nearest to some pixel of each grid cell, plus the cost of searching
/// them.
struct SdfGrid {
    cells_x: u32,
    /// Start of each cell's run in `entries` (`cells + 1` offsets).
    starts: Vec<u32>,
    entries: Vec<u32>,
}

impl SdfGrid {
    /// Buckets `lines` (in outline coordinates) into cells of the `width` x `height` bitmap at
    /// `origin`. A segment is listed in every cell with a pixel centre within `reach` of its
    /// bounding box, so a pixel whose nearest segment is closer than `reach` finds it in its own
    /// cell. Fails with `GlyphTooExpensive` before allocating more than the grid budget.
    fn build(
        lines: &[(Vec2, Vec2)],
        origin: Vec2,
        width: u32,
        height: u32,
        reach: f32,
    ) -> Result<Self, TextError> {
        let too_expensive = |work: u64| TextError::GlyphTooExpensive {
            work,
            max: MAX_SDF_WORK,
        };
        let cells_x = width.div_ceil(SDF_CELL);
        let cells_y = height.div_ceil(SDF_CELL);
        // Pixels of one cell (edge cells are smaller).
        let cell_pixels = |cx: u32, cy: u32| -> u64 {
            let w = SDF_CELL.min(width - cx * SDF_CELL);
            let h = SDF_CELL.min(height - cy * SDF_CELL);
            u64::from(w) * u64::from(h)
        };
        let span = |lo: f32, hi: f32, cells: u32| -> Option<(u32, u32)> {
            // Pixel centres sit at index + 0.5; one pixel of slack keeps the bound conservative.
            let first = (lo - reach - 1.5).floor().max(0.0);
            let last = (hi + reach + 0.5)
                .floor()
                .min((cells * SDF_CELL - 1) as f32);
            (first <= last).then(|| (first as u32 / SDF_CELL, last as u32 / SDF_CELL))
        };
        let ranges: Vec<Option<(u32, u32, u32, u32)>> = lines
            .iter()
            .map(|&(a, b)| {
                let (a, b) = (a - origin, b - origin);
                let xs = span(a.x.min(b.x), a.x.max(b.x), cells_x)?;
                let ys = span(a.y.min(b.y), a.y.max(b.y), cells_y)?;
                Some((xs.0, xs.1, ys.0, ys.1))
            })
            .collect();
        // Count first so the cost is known before anything large is allocated.
        let mut counts = vec![0u32; (cells_x * cells_y) as usize];
        let mut work = u64::from(height) * lines.len() as u64;
        let mut pairs = 0u64;
        for &(x0, x1, y0, y1) in ranges.iter().flatten() {
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    counts[(cy * cells_x + cx) as usize] += 1;
                    work += cell_pixels(cx, cy);
                    pairs += 1;
                }
            }
            if work > MAX_SDF_WORK || pairs > MAX_SDF_GRID_ENTRIES {
                return Err(too_expensive(work));
            }
        }
        let mut starts = Vec::with_capacity(counts.len() + 1);
        let mut total = 0u32;
        starts.push(0);
        for &c in &counts {
            total += c;
            starts.push(total);
        }
        let mut fill = starts.clone();
        let mut entries = vec![0u32; total as usize];
        for (i, range) in ranges.iter().enumerate() {
            let Some((x0, x1, y0, y1)) = *range else {
                continue;
            };
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    let cell = (cy * cells_x + cx) as usize;
                    entries[fill[cell] as usize] = i as u32;
                    fill[cell] += 1;
                }
            }
        }
        Ok(Self {
            cells_x,
            starts,
            entries,
        })
    }

    fn cell(&self, x: u32, y: u32) -> &[u32] {
        let cell = ((y / SDF_CELL) * self.cells_x + x / SDF_CELL) as usize;
        &self.entries[self.starts[cell] as usize..self.starts[cell + 1] as usize]
    }
}

/// True signed distance field: alpha = 0.5 + d / (2 * spread), inside positive, edge at 128.
/// Inside/outside uses the non-zero winding rule on the flattened outline; `bold` grows the
/// shape by that many pixels.
///
/// Only distances up to `spread + |bold| + 1` can change a pixel (everything beyond saturates
/// to 0 or 255), so each pixel searches only the segments bucketed near it. The cost is
/// bounded: a glyph that would need more than [`MAX_SDF_WORK`] evaluations fails with
/// `text.atlas.glyph_too_expensive` before any pixel is computed.
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
    let reach = spread + bold.abs() + 1.0;
    let grid = SdfGrid::build(
        &lines,
        Vec2::new(left as f32, top as f32),
        width,
        height,
        reach,
    )?;
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
        // Winding number to the left of the pixel centre, advanced along the row.
        let (mut next, mut winding) = (0usize, 0i32);
        for x in 0..width {
            let p = Vec2::new(left as f32 + x as f32 + 0.5, py);
            while let Some(&(cx, w)) = crossings.get(next) {
                if cx >= p.x {
                    break;
                }
                winding += w;
                next += 1;
            }
            let d2 = grid
                .cell(x, y)
                .iter()
                .map(|&i| {
                    let (a, b) = lines[i as usize];
                    distance_to_segment(p, a, b)
                })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn outline_of(segments: Vec<Segment>) -> Outline {
        let mut min = Vec2::splat(f32::MAX);
        let mut max = Vec2::splat(f32::MIN);
        for s in &segments {
            let points: Vec<Vec2> = match *s {
                Segment::Line(a, b) => vec![a, b],
                Segment::Quad(a, b, c) => vec![a, b, c],
                Segment::Cubic(a, b, c, d) => vec![a, b, c, d],
            };
            for p in points {
                min = min.min(p);
                max = max.max(p);
            }
        }
        Outline { segments, min, max }
    }

    /// Two nested rounded contours (a ring with a hole) built from quads and lines.
    fn ring(radius: f32) -> Outline {
        let mut segments = Vec::new();
        for (r, wind) in [(radius, 1.0f32), (radius * 0.5, -1.0)] {
            let n = 12;
            let at = |k: usize| {
                let t = wind * k as f32 / n as f32 * std::f32::consts::TAU;
                Vec2::new(r * t.cos(), r * t.sin())
            };
            for k in 0..n {
                let (a, c) = (at(k), at(k + 1));
                let mid = (a + c) * 0.5;
                // Control point pushed outwards so the chord bulges like a circle arc.
                segments.push(Segment::Quad(a, mid * (1.0 + 0.1), c));
            }
        }
        segments.push(Segment::Line(
            Vec2::new(-r_edge(radius), 0.0),
            Vec2::new(-r_edge(radius), 1.0),
        ));
        outline_of(segments)
    }

    fn r_edge(radius: f32) -> f32 {
        radius * 1.2
    }

    /// The original exhaustive algorithm, kept as the reference for the grid search.
    fn reference_sdf(outline: &Outline, spread: f32, bold: f32, max_dim: u32) -> GlyphBitmap {
        let margin = spread.ceil() + 1.0 + bold;
        let (left, top, width, height) = bitmap_box(
            outline.min,
            outline.max,
            margin,
            Vec2::splat(margin),
            max_dim,
        )
        .unwrap();
        let lines = flatten(&outline.segments, 0.2).unwrap();
        let mut alpha = vec![0u8; (width * height) as usize];
        for y in 0..height {
            let py = top as f32 + y as f32 + 0.5;
            let mut crossings: Vec<(f32, i32)> = Vec::new();
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
                let signed = if winding != 0 { d2.sqrt() } else { -d2.sqrt() } + bold;
                let value = (0.5 + signed / (2.0 * spread)).clamp(0.0, 1.0);
                alpha[(y * width + x) as usize] = (value * 255.0).round() as u8;
            }
        }
        GlyphBitmap {
            width,
            height,
            left,
            top,
            alpha,
        }
    }

    #[test]
    fn grid_search_matches_the_exhaustive_distance_field() {
        for (radius, spread, bold) in [(30.0, 4.0, 0.0), (45.0, 8.0, 1.5), (20.0, 2.0, 0.5)] {
            let outline = ring(radius);
            let fast = rasterize_sdf(&outline, spread, bold, 1000).unwrap();
            let slow = reference_sdf(&outline, spread, bold, 1000);
            assert_eq!(
                (fast.width, fast.height, fast.left, fast.top),
                (slow.width, slow.height, slow.left, slow.top)
            );
            assert_eq!(fast.alpha, slow.alpha, "radius {radius} spread {spread}");
            // The field is not trivially saturated.
            assert!(fast.alpha.iter().any(|&a| a > 128));
            assert!(fast.alpha.iter().any(|&a| a > 0 && a < 128));
        }
    }

    #[test]
    fn a_glyph_with_unbounded_distance_cost_fails_fast_with_a_typed_error() {
        // 16 384 segments, each spanning the whole bitmap: naive cost is ~1000 * 1000 * 65 536.
        let segments: Vec<Segment> = (0..MAX_OUTLINE_SEGMENTS)
            .map(|i| {
                let i = i as f32;
                Segment::Line(
                    Vec2::new(0.0, (i * 7.0) % 1000.0),
                    Vec2::new(1000.0, (i * 13.0) % 1000.0),
                )
            })
            .collect();
        let outline = outline_of(segments);
        let started = Instant::now();
        let error = rasterize_sdf(&outline, 8.0, 0.0, 1018)
            .err()
            .expect("must fail");
        assert!(
            matches!(error, TextError::GlyphTooExpensive { .. }),
            "{error:?}"
        );
        assert_eq!(error.code(), "text.atlas.glyph_too_expensive");
        assert!(
            started.elapsed().as_secs_f32() < 2.0,
            "rejected in {:?}",
            started.elapsed()
        );
    }
}
