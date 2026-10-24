//! Caret, hit testing, selection geometry and caret movement for text input and IME.
//!
//! Indices are UTF-8 byte offsets into [`TextLayout::text`] and are always snapped to
//! grapheme boundaries. A caret index belongs to the line whose range contains it (the start of
//! a soft-wrapped line, not the end of the previous one: downstream affinity), and is drawn at
//! the leading edge of the cluster that starts there: the left edge of a left-to-right
//! cluster, the right edge of a right-to-left one. Inside a ligature the cluster advance is
//! split evenly between its graphemes.

use glam::Vec2;
use unicode_segmentation::UnicodeSegmentation;

use crate::Rect;
use crate::layout::{CaretCluster, TextLayout};
use crate::shape::ClusterKind;

/// Caret movement. Logical moves follow byte order; visual moves follow screen order, which
/// differs from logical order inside right-to-left and mixed-direction lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaretMove {
    /// Next grapheme boundary in logical order.
    NextGrapheme,
    /// Previous grapheme boundary in logical order.
    PrevGrapheme,
    /// Start of the next word (UAX #29 word boundaries, whitespace skipped).
    NextWord,
    /// Start of the current or previous word.
    PrevWord,
    /// Logical start of the visual line.
    LineStart,
    /// Logical end of the visual line, before a trailing newline or hanging space.
    LineEnd,
    /// Nearest stop on the previous line at `x` (defaults to the current caret x; keep the
    /// original x across repeated moves for column memory).
    LineUp {
        x: Option<f32>,
    },
    /// Nearest stop on the next line at `x`.
    LineDown {
        x: Option<f32>,
    },
    /// Next stop to the left on screen; wraps to the adjacent line in paragraph order.
    VisualLeft,
    /// Next stop to the right on screen.
    VisualRight,
    TextStart,
    TextEnd,
}

impl TextLayout {
    /// Largest grapheme boundary `<= index`, clamped to the laid-out text.
    pub fn snap_index(&self, index: usize) -> usize {
        let index = index.min(self.end);
        let pos = self.graphemes.partition_point(|&g| g <= index);
        self.graphemes[pos.saturating_sub(1)]
    }

    /// Line containing a caret index.
    pub fn line_of(&self, index: usize) -> usize {
        let index = self.snap_index(index);
        self.lines
            .iter()
            .position(|l| l.range.start <= index && index < l.range.end)
            .unwrap_or_else(|| {
                // The end of the text, or an empty line that starts at `index`.
                self.lines
                    .iter()
                    .rposition(|l| l.range.start == index)
                    .unwrap_or(self.lines.len().saturating_sub(1))
            })
    }

    fn line_clusters(&self, line: usize) -> &[CaretCluster] {
        &self.carets[self.caret_lines[line].clusters.clone()]
    }

    /// Grapheme boundaries strictly inside `range` plus its start.
    fn cluster_stops(&self, cluster: &CaretCluster) -> impl Iterator<Item = usize> + '_ {
        let start = cluster.range.start;
        self.text[cluster.range.clone()]
            .grapheme_indices(true)
            .map(move |(i, _)| start + i)
    }

    /// X of offset `index` inside a cluster (`index == range.end` gives the trailing edge).
    fn edge_x(&self, cluster: &CaretCluster, index: usize) -> f32 {
        let slice = &self.text[cluster.range.clone()];
        let total = slice.graphemes(true).count().max(1);
        let before = slice
            .grapheme_indices(true)
            .take_while(|(i, _)| cluster.range.start + i < index)
            .count();
        let fraction = before as f32 / total as f32;
        let width = cluster.x1 - cluster.x0;
        if cluster.rtl {
            cluster.x1 - fraction * width
        } else {
            cluster.x0 + fraction * width
        }
    }

    fn caret_x(&self, line: usize, index: usize) -> f32 {
        let clusters = self.line_clusters(line);
        if let Some(c) = clusters.iter().find(|c| c.range.contains(&index)) {
            return self.edge_x(c, index);
        }
        match clusters.iter().rev().find(|c| c.range.end == index) {
            Some(c) if c.rtl => c.x0,
            Some(c) => c.x1,
            None => {
                let l = &self.lines[line];
                if l.rtl { l.left + l.width } else { l.left }
            }
        }
    }

    /// Caret stops (grapheme boundaries) on a line, logical order.
    fn line_stops(&self, line: usize) -> Vec<usize> {
        let mut stops: Vec<usize> = self
            .line_clusters(line)
            .iter()
            .flat_map(|c| self.cluster_stops(c))
            .collect();
        if line + 1 == self.lines.len() {
            stops.push(self.end);
        }
        if stops.is_empty() {
            stops.push(self.lines[line].range.start);
        }
        stops
    }

    /// Caret rectangle (zero width, full line height) for an index.
    pub fn caret_rect(&self, index: usize) -> Rect {
        let index = self.snap_index(index);
        let line = self.line_of(index);
        let l = &self.lines[line];
        Rect::new(self.caret_x(line, index), l.top, 0.0, l.height)
    }

    /// Byte index of the caret stop nearest to a point (lines are chosen by y, clamped).
    pub fn hit_test(&self, point: Vec2) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .position(|l| point.y < l.top + l.height)
            .or(self.lines.len().checked_sub(1))
        else {
            return 0;
        };
        self.nearest_on_line(line, point.x)
    }

    fn nearest_on_line(&self, line: usize, x: f32) -> usize {
        self.line_stops(line)
            .into_iter()
            .min_by(|a, b| {
                (self.caret_x(line, *a) - x)
                    .abs()
                    .total_cmp(&(self.caret_x(line, *b) - x).abs())
            })
            .unwrap_or(0)
    }

    /// Selection highlight rectangles for a byte range: one per contiguous visual piece per
    /// line, so mixed-direction text can yield several rectangles on one line.
    pub fn selection_rects(&self, range: std::ops::Range<usize>) -> Vec<Rect> {
        let a = self.snap_index(range.start.min(range.end));
        let b = self.snap_index(range.start.max(range.end));
        let mut rects = Vec::new();
        if a == b {
            return rects;
        }
        for (line_index, line) in self.lines.iter().enumerate() {
            if line.range.end <= a || line.range.start >= b {
                continue;
            }
            let mut pieces: Vec<(f32, f32)> = Vec::new();
            for &k in &self.caret_lines[line_index].visual {
                let c = &self.carets[k];
                let (s, e) = (a.max(c.range.start), b.min(c.range.end));
                if s >= e || c.kind == ClusterKind::Newline {
                    continue;
                }
                let (xs, xe) = (self.edge_x(c, s), self.edge_x(c, e));
                let (lo, hi) = (xs.min(xe), xs.max(xe));
                match pieces.last_mut() {
                    Some(last) if (lo - last.1).abs() < 0.01 => last.1 = hi,
                    _ => pieces.push((lo, hi)),
                }
            }
            rects.extend(
                pieces
                    .into_iter()
                    .filter(|(lo, hi)| hi > lo)
                    .map(|(lo, hi)| Rect::new(lo, line.top, hi - lo, line.height)),
            );
        }
        rects
    }

    /// Applies a caret movement and returns the new index.
    pub fn move_caret(&self, index: usize, movement: CaretMove) -> usize {
        let index = self.snap_index(index);
        let line = self.line_of(index);
        match movement {
            CaretMove::NextGrapheme => {
                let pos = self.graphemes.partition_point(|&g| g <= index);
                self.graphemes.get(pos).copied().unwrap_or(self.end)
            }
            CaretMove::PrevGrapheme => {
                let pos = self.graphemes.partition_point(|&g| g < index);
                pos.checked_sub(1).map(|p| self.graphemes[p]).unwrap_or(0)
            }
            CaretMove::NextWord => self
                .words()
                .find(|&(start, word)| start > index && !is_space(word))
                .map(|(start, _)| start)
                .unwrap_or(self.end),
            CaretMove::PrevWord => self
                .words()
                .filter(|&(start, word)| start < index && !is_space(word))
                .last()
                .map(|(start, _)| start)
                .unwrap_or(0),
            CaretMove::LineStart => self.lines[line].range.start,
            CaretMove::LineEnd => self.line_end(line),
            CaretMove::LineUp { x } => match line.checked_sub(1) {
                Some(up) => self.nearest_on_line(up, x.unwrap_or(self.caret_x(line, index))),
                None => self.lines[line].range.start,
            },
            CaretMove::LineDown { x } => {
                if line + 1 < self.lines.len() {
                    self.nearest_on_line(line + 1, x.unwrap_or(self.caret_x(line, index)))
                } else {
                    self.end
                }
            }
            CaretMove::VisualLeft => self.visual_step(line, index, false),
            CaretMove::VisualRight => self.visual_step(line, index, true),
            CaretMove::TextStart => 0,
            CaretMove::TextEnd => self.end,
        }
    }

    fn words(&self) -> impl Iterator<Item = (usize, &str)> {
        self.text[..self.end].split_word_bound_indices()
    }

    fn line_end(&self, line: usize) -> usize {
        let l = &self.lines[line];
        let last = self.line_clusters(line).last();
        match last {
            Some(c) if c.kind == ClusterKind::Newline => c.range.start,
            Some(c) if c.kind == ClusterKind::Whitespace && line + 1 < self.lines.len() => {
                c.range.start
            }
            _ => l.range.end.min(self.end),
        }
    }

    fn visual_step(&self, line: usize, index: usize, right: bool) -> usize {
        let current = self.caret_x(line, index);
        let candidate = self
            .line_stops(line)
            .into_iter()
            .filter(|&s| s != index)
            .map(|s| (s, self.caret_x(line, s)))
            .filter(|&(_, x)| {
                if right {
                    x > current + 1e-3
                } else {
                    x < current - 1e-3
                }
            })
            .min_by(|a, b| (a.1 - current).abs().total_cmp(&(b.1 - current).abs()));
        if let Some((stop, _)) = candidate {
            return stop;
        }
        // Past the edge: continue on the adjacent line in paragraph order.
        let forward = right != self.lines[line].rtl;
        let next = if forward {
            (line + 1 < self.lines.len()).then_some(line + 1)
        } else {
            line.checked_sub(1)
        };
        let Some(next) = next else {
            return index;
        };
        let stops = self.line_stops(next);
        let pick = |pick_max: bool| {
            stops
                .iter()
                .copied()
                .max_by(|&a, &b| {
                    let (xa, xb) = (self.caret_x(next, a), self.caret_x(next, b));
                    if pick_max {
                        xa.total_cmp(&xb)
                    } else {
                        xb.total_cmp(&xa)
                    }
                })
                .unwrap_or(index)
        };
        // Moving right lands on the left edge of the next line, moving left on the right edge.
        pick(!right)
    }
}

fn is_space(word: &str) -> bool {
    word.chars().all(char::is_whitespace)
}
