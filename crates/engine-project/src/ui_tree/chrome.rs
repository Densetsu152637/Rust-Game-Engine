//! Widget chrome geometry shared by hit testing and rendering, so what a
//! widget draws is exactly what it reacts to.
use super::{layout::SCROLLBAR, *};

/// Dropdown option popup placed below its anchor (above when it would leave
/// the viewport and fits there).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Popup {
    pub rect: Rect,
    pub item_height: f32,
    pub first: u32,
    pub visible: u32,
    pub count: u32,
}

impl Popup {
    pub fn new(
        anchor: Rect,
        count: usize,
        max_visible: u32,
        first: u32,
        viewport: [u32; 2],
    ) -> Self {
        let count = count as u32;
        let visible = count.min(max_visible);
        let item_height = anchor.height.max(1.0);
        let height = item_height * visible as f32;
        let mut y = anchor.y + anchor.height;
        if y + height > viewport[1] as f32 && anchor.y - height >= 0.0 {
            y = anchor.y - height;
        }
        Self {
            rect: Rect {
                x: anchor.x,
                y,
                width: anchor.width,
                height,
            },
            item_height,
            first: first.min(count - visible),
            visible,
            count,
        }
    }
    /// Rectangle of option `index` when it is scrolled into view.
    pub fn option_rect(&self, index: u32) -> Option<Rect> {
        (index >= self.first && index < self.first + self.visible).then(|| Rect {
            y: self.rect.y + (index - self.first) as f32 * self.item_height,
            height: self.item_height,
            ..self.rect
        })
    }
    pub fn option_at(&self, point: [f32; 2]) -> Option<u32> {
        if !contains(self.rect, point) {
            return None;
        }
        let slot = ((point[1] - self.rect.y) / self.item_height) as u32;
        Some((self.first + slot).min(self.count.saturating_sub(1)))
    }
    /// First visible option that keeps `index` in view.
    pub fn reveal(&self, index: u32) -> u32 {
        if index < self.first {
            index
        } else if index >= self.first + self.visible {
            index + 1 - self.visible
        } else {
            self.first
        }
    }
}

/// Track and thumb per axis (`[horizontal, vertical]`) of a scroll view whose
/// content overflows on that axis.
pub(super) fn scrollbars(bounds: &UiBox, node: &UiNode) -> [Option<(Rect, Rect)>; 2] {
    let UiWidget::ScrollView {
        axes,
        scrollbars: true,
        ..
    } = &node.widget
    else {
        return [None, None];
    };
    let r = bounds.rect;
    let gutter = |on: bool| if on { SCROLLBAR } else { 0.0 };
    let bar = |axis: usize| -> Option<(Rect, Rect)> {
        let max = bounds.scroll_max[axis];
        if max <= 0.0 {
            return None;
        }
        let (track, viewport) = if axis == 1 {
            let h = (r.height - gutter(axes.horizontal())).max(0.0);
            (
                Rect {
                    x: r.x + r.width - SCROLLBAR,
                    y: r.y,
                    width: SCROLLBAR,
                    height: h,
                },
                h,
            )
        } else {
            let w = (r.width - gutter(axes.vertical())).max(0.0);
            (
                Rect {
                    x: r.x,
                    y: r.y + r.height - SCROLLBAR,
                    width: w,
                    height: SCROLLBAR,
                },
                w,
            )
        };
        let length = if axis == 1 { track.height } else { track.width };
        let thumb = (length * viewport / (viewport + max)).max(16.0).min(length);
        let start = (length - thumb) * (bounds.scroll[axis] / max);
        let thumb = if axis == 1 {
            Rect {
                y: track.y + start,
                height: thumb,
                ..track
            }
        } else {
            Rect {
                x: track.x + start,
                width: thumb,
                ..track
            }
        };
        Some((track, thumb))
    };
    [
        if axes.horizontal() { bar(0) } else { None },
        if axes.vertical() { bar(1) } else { None },
    ]
}

fn cells(row: Rect, count: usize, inset: f32) -> Vec<Rect> {
    if count == 0 {
        return vec![];
    }
    let width = row.width / count as f32;
    (0..count)
        .map(|i| Rect {
            x: row.x + i as f32 * width + inset,
            y: row.y + inset,
            width: (width - 2.0 * inset).max(0.0),
            height: (row.height - 2.0 * inset).max(0.0),
        })
        .collect()
}

/// Tab header cells, one per label, across the header strip.
pub(super) fn tab_cells(bounds: &UiBox, count: usize, header_height: f32) -> Vec<Rect> {
    let r = bounds.rect;
    cells(
        Rect {
            height: header_height.min(r.height),
            ..r
        },
        count,
        0.0,
    )
}

pub(super) fn dialog_title(bounds: &UiBox, title_height: f32) -> Rect {
    Rect {
        height: title_height.min(bounds.rect.height),
        ..bounds.rect
    }
}

/// Dialog footer buttons, equal width with a 4 px inset.
pub(super) fn dialog_buttons(bounds: &UiBox, count: usize, button_height: f32) -> Vec<Rect> {
    let r = bounds.rect;
    let height = button_height.min(r.height);
    cells(
        Rect {
            y: r.y + r.height - height,
            height,
            ..r
        },
        count,
        4.0,
    )
}

/// Laid-out text of a `TextInput`, scrolled so the caret stays visible.
pub(super) struct InputView {
    pub layout: UiTextLayout,
    /// Absolute top-left of the text.
    pub origin: [f32; 2],
    /// Absolute caret rectangle.
    pub caret: Rect,
    pub placeholder: bool,
    pub selection: Vec<Rect>,
    pub preedit: Vec<Rect>,
    password: bool,
    shown: String,
    composing: bool,
}

fn byte_of(text: &str, chars: u32) -> usize {
    text.char_indices()
        .nth(chars as usize)
        .map_or(text.len(), |(b, _)| b)
}

fn stop_at(layout: &UiTextLayout, byte: usize) -> Option<UiCaretStop> {
    let i = layout
        .carets
        .partition_point(|s| (s.index as usize) <= byte);
    i.checked_sub(1).map(|i| layout.carets[i])
}

/// One rectangle per laid-out character in the byte range.
fn range_rects(layout: &UiTextLayout, start: usize, end: usize, underline: bool) -> Vec<Rect> {
    let stops = &layout.carets;
    stops
        .iter()
        .enumerate()
        .filter(|(_, s)| (start..end).contains(&(s.index as usize)))
        .map(|(i, s)| {
            let width = stops
                .get(i + 1)
                .filter(|t| t.y == s.y)
                .map_or(4.0, |t| t.x - s.x);
            if underline {
                Rect {
                    x: s.x,
                    y: s.y + s.height - 1.0,
                    width,
                    height: 1.0,
                }
            } else {
                Rect {
                    x: s.x,
                    y: s.y,
                    width,
                    height: s.height,
                }
            }
        })
        .collect()
}

fn offset(mut rect: Rect, by: [f32; 2]) -> Rect {
    rect.x += by[0];
    rect.y += by[1];
    rect
}

pub(super) fn input_view(
    node: &UiNode,
    bounds: &UiBox,
    text: &dyn UiTextBackend,
    preedit: Option<&UiPreedit>,
) -> Result<Option<InputView>> {
    let UiWidget::TextInput {
        text: value,
        placeholder,
        password,
        multiline,
        caret,
        anchor,
        ..
    } = &node.widget
    else {
        return Ok(None);
    };
    let content = bounds.content;
    let preedit = preedit.filter(|p| !p.text.is_empty());
    let shown = if *password {
        layout::mask(value)
    } else {
        value.clone()
    };
    let caret_byte = byte_of(&shown, *caret);
    let placeholder_shown = value.is_empty() && preedit.is_none();
    let display = match (placeholder_shown, preedit) {
        (true, _) => placeholder.clone(),
        (false, Some(p)) => format!("{}{}{}", &shown[..caret_byte], p.text, &shown[caret_byte..]),
        (false, None) => shown.clone(),
    };
    let request = layout::text_request(
        node,
        &display,
        UiTextKind::Plain,
        multiline.then_some(content.width),
    );
    let laid = text.layout(&request)?;
    let insert = preedit.map_or(0, |p| p.text.len());
    let display_byte = |chars: u32| {
        let b = byte_of(&shown, chars);
        if b > caret_byte { b + insert } else { b }
    };
    let caret_at = if placeholder_shown {
        0
    } else {
        caret_byte + preedit.map_or(0, |p| p.cursor.map_or(p.text.len(), |c| c[1]))
    };
    let line = laid.metrics.line_height.max(1.0);
    let stop = stop_at(&laid, caret_at).unwrap_or(UiCaretStop {
        index: 0,
        x: 0.0,
        y: 0.0,
        height: line,
    });
    let height = if stop.height > 0.0 { stop.height } else { line };
    let shift = if *multiline {
        [0.0, (stop.y + height - content.height).max(0.0)]
    } else {
        [(stop.x + 2.0 - content.width).max(0.0), 0.0]
    };
    let origin = [content.x - shift[0], content.y - shift[1]];
    let selection = if placeholder_shown || caret == anchor {
        vec![]
    } else {
        let (a, b) = ((*caret).min(*anchor), (*caret).max(*anchor));
        range_rects(&laid, display_byte(a), display_byte(b), false)
    };
    let underline = preedit.map_or(vec![], |p| {
        range_rects(&laid, caret_byte, caret_byte + p.text.len(), true)
    });
    Ok(Some(InputView {
        caret: offset(
            Rect {
                x: stop.x,
                y: stop.y,
                width: 2.0,
                height,
            },
            origin,
        ),
        selection: selection.into_iter().map(|r| offset(r, origin)).collect(),
        preedit: underline.into_iter().map(|r| offset(r, origin)).collect(),
        layout: laid,
        origin,
        placeholder: placeholder_shown,
        password: *password,
        shown,
        composing: preedit.is_some(),
    }))
}

impl InputView {
    /// Character index nearest to `point`, or `None` while composing.
    pub fn caret_at(&self, point: [f32; 2]) -> Option<u32> {
        if self.composing {
            return None;
        }
        if self.placeholder {
            return Some(0);
        }
        let local = [point[0] - self.origin[0], point[1] - self.origin[1]];
        let stops = &self.layout.carets;
        // The last line starting at or above the pointer (else the first line).
        let first = stops.first()?.y;
        let line_y = stops
            .iter()
            .map(|s| s.y)
            .filter(|y| *y <= local[1])
            .fold(first, f32::max);
        let stop = stops
            .iter()
            .filter(|s| s.y == line_y)
            .min_by(|a, b| (a.x - local[0]).abs().total_cmp(&(b.x - local[0]).abs()))?;
        let byte = (stop.index as usize).min(self.shown.len());
        Some(if self.password {
            byte as u32
        } else {
            self.shown[..byte].chars().count() as u32
        })
    }
}
