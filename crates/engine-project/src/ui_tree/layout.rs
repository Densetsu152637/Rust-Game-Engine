//! Layout on taffy (flexbox, CSS grid, absolute positioning). Legacy modes map
//! onto taffy so version-1 trees keep their rectangles: `absolute` becomes a
//! one-cell overlay grid, `row`/`column` become flex lines with `fill` as
//! `flex: 1 1 0` (zero minimum) and non-shrinking pixel/percent items.
use super::{text::UiTextRequest, tree::Hierarchy, *};
use std::borrow::Cow;
use taffy as tf;

/// Boxes per root per layout, counting virtual-list replicas.
pub const MAX_UI_BOXES: usize = 16384;
/// Space before toggle text for the check box.
pub(super) const TOGGLE_INDENT: f32 = 16.0;
/// Space after dropdown text for the arrow.
pub(super) const DROPDOWN_ARROW: f32 = 16.0;
/// Scrollbar thickness; reserved as end padding on scroll views with bars.
pub(super) const SCROLLBAR: f32 = 8.0;
/// Password inputs render every character as this mask.
pub(super) const PASSWORD_MASK: char = '*';

/// A visible node's logical-pixel rectangle and ancestor clip, in paint order.
#[derive(Clone, Debug, PartialEq)]
pub struct UiBox {
    pub node: usize,
    /// Border box after layout, scroll and `offset` translation.
    pub rect: Rect,
    /// `rect` minus resolved padding (including widget chrome such as tab
    /// headers, dialog bars and scrollbar gutters).
    pub content: Rect,
    /// Intersection of ancestor clips; `rect` itself is not included.
    pub clip: Rect,
    pub enabled: bool,
    /// Virtual-list item index when this box is a template replica.
    pub item: Option<u32>,
    /// Part of an open modal dialog laid out above the base tree.
    pub overlay: bool,
    /// Applied (clamped) scroll offset of a scroll view or virtual list.
    pub scroll: [f32; 2],
    /// Largest valid scroll offset per axis (0 when not scrollable).
    pub scroll_max: [f32; 2],
}

impl UiBox {
    pub fn hit(&self, point: [f32; 2]) -> bool {
        self.enabled && contains(self.rect, point) && contains(self.clip, point)
    }
    /// The area children are clipped to (also the visible hit area).
    pub fn visible(&self) -> Rect {
        intersection(self.rect, self.clip)
    }
}

pub(super) struct Laid {
    pub boxes: Vec<UiBox>,
    pub hierarchy: Hierarchy,
}

#[derive(Clone, Copy, PartialEq)]
enum Parent {
    Overlay,
    Flex { row: bool },
    Grid,
    Template { horizontal: bool, extent: f32 },
}

impl UiTree {
    /// Layout without fonts: text measures as empty. Rectangles of nodes with
    /// explicit sizes are identical to [`UiTree::layout_with`].
    pub fn layout(&self, viewport: [u32; 2]) -> Result<Vec<UiBox>> {
        self.layout_with(viewport, &NullTextBackend)
    }

    /// Layout measuring content-sized text through `text`.
    pub fn layout_with(&self, viewport: [u32; 2], text: &dyn UiTextBackend) -> Result<Vec<UiBox>> {
        Ok(self.laid(viewport, text)?.boxes)
    }

    pub(super) fn laid(&self, viewport: [u32; 2], text: &dyn UiTextBackend) -> Result<Laid> {
        if viewport.contains(&0) || viewport.iter().any(|v| *v > 16384) {
            return Err(invalid("Invalid UI viewport"));
        }
        let hierarchy = self.hierarchy()?;
        let mut taffy: tf::TaffyTree<usize> = tf::TaffyTree::with_capacity(self.nodes.len() + 4);
        taffy.disable_rounding();
        let mut ids = vec![None; self.nodes.len()];
        let mut builder = Builder {
            tree: self,
            hierarchy: &hierarchy,
            taffy: &mut taffy,
            ids: &mut ids,
        };
        let size = tf::Size {
            width: viewport[0] as f32,
            height: viewport[1] as f32,
        };
        let roots: Vec<usize> = hierarchy
            .roots
            .iter()
            .copied()
            .filter(|r| builder.included(*r))
            .collect();
        let base = builder.viewport(&roots, size, false)?;
        // Open dialogs leave the base flow and are laid out centred in their own root.
        let dialogs: Vec<usize> = (0..self.nodes.len())
            .filter(|i| {
                matches!(
                    self.nodes[*i].widget,
                    UiWidget::ModalDialog { open: true, .. }
                ) && self.nodes[*i].visible
                    && hierarchy.ancestors(*i).all(|a| self.nodes[a].visible)
            })
            .collect();
        let mut overlay_roots = vec![];
        for dialog in &dialogs {
            overlay_roots.push(builder.viewport(&[*dialog], size, true)?);
        }
        let space = tf::Size {
            width: tf::AvailableSpace::Definite(size.width),
            height: tf::AvailableSpace::Definite(size.height),
        };
        for root in std::iter::once(base).chain(overlay_roots) {
            taffy
                .compute_layout_with_measure(root, space, |inputs, _, context, style| {
                    tf::compute_leaf_layout(
                        inputs,
                        style,
                        |_, _| 0.0,
                        |known, available| match context {
                            Some(index) => measure(self, *index, text, known, available),
                            None => tf::Size::ZERO,
                        },
                    )
                })
                .map_err(|e| Error::new("ui.layout.failed", format!("{e:?}")))?;
        }
        let mut emitter = Emitter {
            tree: self,
            hierarchy: &hierarchy,
            taffy: &taffy,
            ids: &ids,
            boxes: Vec::with_capacity(self.nodes.len()),
        };
        let screen = viewport_rect(viewport);
        for root in roots {
            emitter.emit(root, [0.0; 2], screen, true, None, false)?;
        }
        for dialog in dialogs {
            let enabled = hierarchy.ancestors(dialog).all(|a| self.nodes[a].enabled);
            emitter.emit(dialog, [0.0; 2], screen, enabled, None, true)?;
        }
        let boxes = emitter.boxes;
        Ok(Laid { boxes, hierarchy })
    }
}

struct Builder<'a> {
    tree: &'a UiTree,
    hierarchy: &'a Hierarchy,
    taffy: &'a mut tf::TaffyTree<usize>,
    ids: &'a mut Vec<Option<tf::NodeId>>,
}

fn layout_error(e: tf::TaffyError) -> Error {
    Error::new("ui.layout.failed", format!("{e:?}"))
}

impl Builder<'_> {
    /// Visible nodes that take part in the base flow (open or closed dialogs never do).
    fn included(&self, index: usize) -> bool {
        let node = &self.tree.nodes[index];
        node.visible && !matches!(node.widget, UiWidget::ModalDialog { .. })
    }

    fn children(&self, index: usize) -> Vec<usize> {
        let children = &self.hierarchy.children[index];
        match &self.tree.nodes[index].widget {
            UiWidget::Tabs { selected, .. } => children
                .get(*selected as usize)
                .copied()
                .filter(|c| self.included(*c))
                .into_iter()
                .collect(),
            _ => children
                .iter()
                .copied()
                .filter(|c| self.included(*c))
                .collect(),
        }
    }

    fn viewport(
        &mut self,
        roots: &[usize],
        size: tf::Size<f32>,
        centred: bool,
    ) -> Result<tf::NodeId> {
        let mut children = Vec::with_capacity(roots.len());
        for root in roots {
            children.push(self.build(*root, Parent::Overlay)?);
        }
        let mut style = overlay_container();
        style.size = tf::Size {
            width: tf::Dimension::length(size.width),
            height: tf::Dimension::length(size.height),
        };
        if centred {
            style.justify_items = tf::AlignItems::CENTER;
            style.align_items = tf::AlignItems::CENTER;
        }
        self.taffy
            .new_with_children(style, &children)
            .map_err(layout_error)
    }

    fn build(&mut self, index: usize, parent: Parent) -> Result<tf::NodeId> {
        let node = &self.tree.nodes[index];
        let mode = match &node.widget {
            UiWidget::VirtualList {
                horizontal,
                item_extent,
                ..
            } => Parent::Template {
                horizontal: *horizontal,
                extent: *item_extent,
            },
            _ => match node.style.layout {
                UiLayout::Absolute => Parent::Overlay,
                UiLayout::Grid => Parent::Grid,
                layout => Parent::Flex {
                    row: layout.is_row(),
                },
            },
        };
        let mut children = vec![];
        for child in self.children(index) {
            children.push(self.build(child, mode)?);
        }
        let style = taffy_style(node, parent);
        let id = if children.is_empty() {
            self.taffy.new_leaf_with_context(style, index)
        } else {
            self.taffy.new_with_children(style, &children)
        }
        .map_err(layout_error)?;
        self.ids[index] = Some(id);
        Ok(id)
    }
}

fn overlay_container() -> tf::Style {
    let cell = tf::GridTemplateComponent::Single(tf::MinMax {
        min: tf::MinTrackSizingFunction::length(0.0),
        max: tf::MaxTrackSizingFunction::fr(1.0),
    });
    tf::Style {
        display: tf::Display::Grid,
        grid_template_columns: vec![cell.clone()],
        grid_template_rows: vec![cell],
        justify_items: tf::AlignItems::START,
        align_items: tf::AlignItems::START,
        ..Default::default()
    }
}

fn dimension(length: UiLength) -> tf::Dimension {
    match length {
        UiLength::Pixels(n) => tf::Dimension::length(n),
        UiLength::Percent(n) => tf::Dimension::percent(n),
        UiLength::Fill | UiLength::Auto => tf::Dimension::auto(),
    }
}

fn bound(length: UiLength) -> tf::LengthPercentageAuto {
    match length {
        UiLength::Pixels(n) => tf::LengthPercentageAuto::length(n),
        UiLength::Percent(n) => tf::LengthPercentageAuto::percent(n),
        UiLength::Fill | UiLength::Auto => tf::LengthPercentageAuto::auto(),
    }
}

fn align(value: UiAlign) -> tf::AlignItems {
    match value {
        UiAlign::Start => tf::AlignItems::START,
        UiAlign::End => tf::AlignItems::END,
        UiAlign::Center => tf::AlignItems::CENTER,
        UiAlign::Baseline => tf::AlignItems::BASELINE,
        UiAlign::Stretch => tf::AlignItems::STRETCH,
    }
}

fn distribute(value: UiJustify) -> tf::AlignContent {
    match value {
        UiJustify::Start => tf::AlignContent::START,
        UiJustify::End => tf::AlignContent::END,
        UiJustify::Center => tf::AlignContent::CENTER,
        UiJustify::Stretch => tf::AlignContent::STRETCH,
        UiJustify::SpaceBetween => tf::AlignContent::SPACE_BETWEEN,
        UiJustify::SpaceAround => tf::AlignContent::SPACE_AROUND,
        UiJustify::SpaceEvenly => tf::AlignContent::SPACE_EVENLY,
    }
}

fn breadth(size: UiTrackSize) -> (tf::MinTrackSizingFunction, tf::MaxTrackSizingFunction) {
    use tf::{MaxTrackSizingFunction as Max, MinTrackSizingFunction as Min};
    match size {
        UiTrackSize::Px { value } => (Min::length(value), Max::length(value)),
        UiTrackSize::Percent { value } => (Min::percent(value), Max::percent(value)),
        // A bare `fr` track is `minmax(auto, Nfr)` in CSS.
        UiTrackSize::Fr { value } => (Min::auto(), Max::fr(value)),
        UiTrackSize::Auto => (Min::auto(), Max::auto()),
        UiTrackSize::MinContent => (Min::min_content(), Max::min_content()),
        UiTrackSize::MaxContent => (Min::max_content(), Max::max_content()),
    }
}

fn track(track: UiTrack) -> tf::TrackSizingFunction {
    let (min, max) = match track {
        UiTrack::Px { value } => breadth(UiTrackSize::Px { value }),
        UiTrack::Percent { value } => breadth(UiTrackSize::Percent { value }),
        UiTrack::Fr { value } => breadth(UiTrackSize::Fr { value }),
        UiTrack::Auto => breadth(UiTrackSize::Auto),
        UiTrack::MinContent => breadth(UiTrackSize::MinContent),
        UiTrack::MaxContent => breadth(UiTrackSize::MaxContent),
        UiTrack::Minmax { min, max } => (breadth(min).0, breadth(max).1),
    };
    tf::MinMax { min, max }
}

fn placement(p: UiGridPlacement) -> tf::Line<tf::GridPlacement> {
    let line = |v: i16| tf::GridPlacement::Line(v.into());
    let start = p.start.map(line);
    let end = p.end.map(line);
    let span = p.span.map(tf::GridPlacement::Span);
    match (start, end, span) {
        (None, Some(end), Some(span)) => tf::Line { start: span, end },
        (start, end, span) => tf::Line {
            start: start.unwrap_or(tf::GridPlacement::Auto),
            end: end.or(span).unwrap_or(tf::GridPlacement::Auto),
        },
    }
}

/// Resolved padding including widget chrome, clamped to half of a pixel size
/// on each side as legacy layout did.
fn padding(node: &UiNode) -> UiEdges {
    let s = &node.style;
    let mut p = s.padding_edges();
    match &node.widget {
        UiWidget::Tabs { header_height, .. } => p.top += header_height,
        UiWidget::ModalDialog {
            title_height,
            button_height,
            buttons,
            ..
        } => {
            p.top += title_height;
            if !buttons.is_empty() {
                p.bottom += button_height;
            }
        }
        UiWidget::ScrollView {
            axes,
            scrollbars: true,
            ..
        } => {
            if axes.vertical() {
                p.right += SCROLLBAR;
            }
            if axes.horizontal() {
                p.bottom += SCROLLBAR;
            }
        }
        _ => {}
    }
    if let UiLength::Pixels(w) = s.width {
        p.left = p.left.min(w / 2.0);
        p.right = p.right.min(w / 2.0);
    }
    if let UiLength::Pixels(h) = s.height {
        p.top = p.top.min(h / 2.0);
        p.bottom = p.bottom.min(h / 2.0);
    }
    p
}

fn taffy_style(node: &UiNode, parent: Parent) -> tf::Style {
    let s = &node.style;
    let mut t = match (&node.widget, s.layout) {
        (UiWidget::VirtualList { horizontal, .. }, _) => tf::Style {
            display: tf::Display::Flex,
            flex_direction: if *horizontal {
                tf::FlexDirection::Row
            } else {
                tf::FlexDirection::Column
            },
            ..Default::default()
        },
        (_, UiLayout::Absolute) => overlay_container(),
        (_, UiLayout::Grid) => tf::Style {
            display: tf::Display::Grid,
            grid_template_columns: s
                .grid_columns
                .iter()
                .map(|t| tf::GridTemplateComponent::Single(track(*t)))
                .collect(),
            grid_template_rows: s
                .grid_rows
                .iter()
                .map(|t| tf::GridTemplateComponent::Single(track(*t)))
                .collect(),
            grid_auto_columns: s.grid_auto_columns.map(track).into_iter().collect(),
            grid_auto_rows: s.grid_auto_rows.map(track).into_iter().collect(),
            grid_auto_flow: match s.grid_auto_flow {
                UiGridFlow::Row => tf::GridAutoFlow::Row,
                UiGridFlow::Column => tf::GridAutoFlow::Column,
                UiGridFlow::RowDense => tf::GridAutoFlow::RowDense,
                UiGridFlow::ColumnDense => tf::GridAutoFlow::ColumnDense,
            },
            ..Default::default()
        },
        (_, layout) => tf::Style {
            display: tf::Display::Flex,
            flex_direction: match layout {
                UiLayout::Row => tf::FlexDirection::Row,
                UiLayout::RowReverse => tf::FlexDirection::RowReverse,
                UiLayout::ColumnReverse => tf::FlexDirection::ColumnReverse,
                _ => tf::FlexDirection::Column,
            },
            ..Default::default()
        },
    };
    // Container properties.
    if let Some(a) = s.align_items {
        t.align_items = align(a);
    }
    if let Some(a) = s.justify_items {
        t.justify_items = align(a);
    }
    if let Some(j) = s.justify {
        t.justify_content = distribute(j);
    }
    if let Some(j) = s.align_content {
        t.align_content = distribute(j);
    }
    t.flex_wrap = match s.wrap {
        UiWrap::NoWrap => tf::FlexWrap::NoWrap,
        UiWrap::Wrap => tf::FlexWrap::Wrap,
        UiWrap::WrapReverse => tf::FlexWrap::WrapReverse,
    };
    t.gap = tf::Size {
        width: tf::LengthPercentage::length(s.column_gap.unwrap_or(s.gap)),
        height: tf::LengthPercentage::length(s.row_gap.unwrap_or(s.gap)),
    };
    let p = padding(node);
    t.padding = tf::Rect {
        left: tf::LengthPercentage::length(p.left),
        right: tf::LengthPercentage::length(p.right),
        top: tf::LengthPercentage::length(p.top),
        bottom: tf::LengthPercentage::length(p.bottom),
    };
    if let UiWidget::ScrollView { axes, .. } = &node.widget {
        let scroll = |on: bool| {
            if on {
                tf::Overflow::Scroll
            } else {
                tf::Overflow::Visible
            }
        };
        t.overflow = tf::Point {
            x: scroll(axes.horizontal()),
            y: scroll(axes.vertical()),
        };
    }
    // Item properties.
    let m = s.margin;
    t.margin = tf::Rect {
        left: tf::LengthPercentageAuto::length(m.left),
        right: tf::LengthPercentageAuto::length(m.right),
        top: tf::LengthPercentageAuto::length(m.top),
        bottom: tf::LengthPercentageAuto::length(m.bottom),
    };
    t.size = tf::Size {
        width: dimension(s.width),
        height: dimension(s.height),
    };
    t.min_size = tf::Size {
        width: bound(s.min_width),
        height: bound(s.min_height),
    };
    t.max_size = tf::Size {
        width: bound(s.max_width),
        height: bound(s.max_height),
    };
    t.aspect_ratio = s.aspect_ratio;
    t.align_self = s.align_self.map(align);
    t.justify_self = s.justify_self.map(align);
    t.flex_basis = dimension(s.basis);
    let absolute = s.position == UiPosition::Absolute;
    t.position = if absolute {
        tf::Position::Absolute
    } else {
        tf::Position::Relative
    };
    if absolute {
        let side = |v: Option<f32>| {
            v.map_or(
                tf::LengthPercentageAuto::auto(),
                tf::LengthPercentageAuto::length,
            )
        };
        t.inset = tf::Rect {
            left: side(s.inset.left),
            right: side(s.inset.right),
            top: side(s.inset.top),
            bottom: side(s.inset.bottom),
        };
        if s.width == UiLength::Fill {
            t.size.width = tf::Dimension::percent(1.0);
        }
        if s.height == UiLength::Fill {
            t.size.height = tf::Dimension::percent(1.0);
        }
        return t;
    }
    let fixed = |l: UiLength| matches!(l, UiLength::Pixels(_) | UiLength::Percent(_));
    match parent {
        Parent::Overlay | Parent::Grid => {
            if parent == Parent::Overlay {
                let first = tf::Line {
                    start: tf::GridPlacement::Line(1.into()),
                    end: tf::GridPlacement::Auto,
                };
                t.grid_column = first.clone();
                t.grid_row = first;
            } else {
                t.grid_column = placement(s.grid_column);
                t.grid_row = placement(s.grid_row);
            }
            if s.width == UiLength::Fill {
                fill_cross(&mut t.justify_self, &mut t.size.width);
            }
            if s.height == UiLength::Fill {
                fill_cross(&mut t.align_self, &mut t.size.height);
            }
        }
        Parent::Flex { row } => {
            let (main, cross) = if row {
                (s.width, s.height)
            } else {
                (s.height, s.width)
            };
            if main == UiLength::Fill {
                if s.basis == UiLength::Auto {
                    t.flex_basis = tf::Dimension::length(0.0);
                }
                t.flex_grow = s.grow.unwrap_or(1.0);
                t.flex_shrink = s.shrink.unwrap_or(1.0);
                let min = if row {
                    &mut t.min_size.width
                } else {
                    &mut t.min_size.height
                };
                if min.is_auto() {
                    *min = tf::LengthPercentageAuto::length(0.0);
                }
            } else {
                t.flex_grow = s.grow.unwrap_or(0.0);
                t.flex_shrink = s.shrink.unwrap_or(if fixed(main) { 0.0 } else { 1.0 });
            }
            if cross == UiLength::Fill {
                let size = if row {
                    &mut t.size.height
                } else {
                    &mut t.size.width
                };
                fill_cross(&mut t.align_self, size);
            }
        }
        Parent::Template { horizontal, extent } => {
            let (main, cross) = if horizontal {
                (&mut t.size.width, &mut t.size.height)
            } else {
                (&mut t.size.height, &mut t.size.width)
            };
            *main = tf::Dimension::length(extent);
            *cross = tf::Dimension::auto();
            t.flex_basis = tf::Dimension::auto();
            t.flex_grow = 0.0;
            t.flex_shrink = 0.0;
            t.align_self = Some(tf::AlignItems::STRETCH);
        }
    }
    t
}

/// `fill` on an axis where the parent aligns items: stretch unless the node
/// chose its own alignment, then take the full containing size instead.
fn fill_cross(alignment: &mut Option<tf::AlignItems>, size: &mut tf::Dimension) {
    if alignment.is_some() {
        *size = tf::Dimension::percent(1.0);
    } else {
        *alignment = Some(tf::AlignItems::STRETCH);
    }
}

/// Text shown by a widget for measurement and rendering, plus horizontal
/// chrome and the minimum number of lines.
pub(super) fn display_text(node: &UiNode) -> Option<(Cow<'_, str>, UiTextKind, f32, u32)> {
    match &node.widget {
        UiWidget::Label { text } | UiWidget::Button { text, .. } => {
            Some((text.into(), UiTextKind::Plain, 0.0, 0))
        }
        UiWidget::Toggle { text, .. } => Some((text.into(), UiTextKind::Plain, TOGGLE_INDENT, 0)),
        UiWidget::RichLabel { markup } => Some((markup.into(), UiTextKind::Markup, 0.0, 0)),
        UiWidget::TextInput {
            text: value,
            placeholder,
            password,
            ..
        } => Some((
            if value.is_empty() {
                placeholder.into()
            } else if *password {
                mask(value).into()
            } else {
                value.into()
            },
            UiTextKind::Plain,
            0.0,
            1,
        )),
        UiWidget::Dropdown {
            options,
            selected,
            placeholder,
            ..
        } => Some((
            selected
                .and_then(|s| options.get(s as usize))
                .unwrap_or(placeholder)
                .into(),
            UiTextKind::Plain,
            DROPDOWN_ARROW,
            1,
        )),
        _ => None,
    }
}

pub(super) fn mask(value: &str) -> String {
    std::iter::repeat_n(PASSWORD_MASK, value.chars().count()).collect()
}

pub(super) fn text_request<'a>(
    node: &'a UiNode,
    text: &'a str,
    kind: UiTextKind,
    max_width: Option<f32>,
) -> UiTextRequest<'a> {
    UiTextRequest {
        text,
        kind,
        font: &node.style.font,
        scale: node.style.font_scale,
        color: node.style.foreground,
        max_width,
    }
}

fn measure(
    tree: &UiTree,
    index: usize,
    backend: &dyn UiTextBackend,
    known: tf::Size<Option<f32>>,
    available: tf::Size<tf::AvailableSpace>,
) -> tf::Size<f32> {
    if let (Some(width), Some(height)) = (known.width, known.height) {
        return tf::Size { width, height };
    }
    let node = &tree.nodes[index];
    let Some((text, kind, chrome, min_lines)) = display_text(node) else {
        return tf::Size::ZERO;
    };
    let wraps = !matches!(
        node.widget,
        UiWidget::TextInput {
            multiline: false,
            ..
        } | UiWidget::Dropdown { .. }
    );
    let measured = |max_width: Option<f32>| {
        backend
            .measure(&text_request(node, &text, kind, max_width))
            .unwrap_or_default()
    };
    let metrics = match available.width {
        _ if !wraps => measured(None),
        tf::AvailableSpace::Definite(w) => measured(Some((w - chrome).max(0.0))),
        tf::AvailableSpace::MaxContent => measured(None),
        tf::AvailableSpace::MinContent => {
            let widest = measured(None).min_width;
            UiTextMetrics {
                width: widest,
                ..measured(Some(widest))
            }
        }
    };
    tf::Size {
        width: known.width.unwrap_or(metrics.width + chrome),
        height: known
            .height
            .unwrap_or(metrics.height.max(min_lines as f32 * metrics.line_height)),
    }
}

struct Emitter<'a> {
    tree: &'a UiTree,
    hierarchy: &'a Hierarchy,
    taffy: &'a tf::TaffyTree<usize>,
    ids: &'a [Option<tf::NodeId>],
    boxes: Vec<UiBox>,
}

impl Emitter<'_> {
    fn layout(&self, index: usize) -> Option<&tf::Layout> {
        self.ids[index].and_then(|id| self.taffy.layout(id).ok())
    }

    fn emit(
        &mut self,
        index: usize,
        origin: [f32; 2],
        clip: Rect,
        enabled: bool,
        item: Option<u32>,
        overlay: bool,
    ) -> Result<()> {
        if self.boxes.len() >= MAX_UI_BOXES {
            return Err(Error::new(
                "ui.layout.limit",
                format!("UI layout exceeds {MAX_UI_BOXES} boxes"),
            ));
        }
        let Some(l) = self.layout(index).copied() else {
            return Ok(());
        };
        let node = &self.tree.nodes[index];
        let rect = Rect {
            x: origin[0] + l.location.x + node.style.offset[0],
            y: origin[1] + l.location.y + node.style.offset[1],
            width: l.size.width,
            height: l.size.height,
        };
        let content = Rect {
            x: rect.x + l.padding.left + l.border.left,
            y: rect.y + l.padding.top + l.border.top,
            width: (rect.width - l.padding.left - l.padding.right).max(0.0),
            height: (rect.height - l.padding.top - l.padding.bottom).max(0.0),
        };
        let enabled = enabled && node.enabled;
        let children = &self.hierarchy.children[index];
        let (scroll, scroll_max) = match &node.widget {
            UiWidget::ScrollView { offset, axes, .. } => {
                let mut extent = [0.0_f32; 2];
                for child in children {
                    if let Some(c) = self.layout(*child) {
                        extent[0] = extent[0].max(c.location.x + c.size.width + c.margin.right);
                        extent[1] = extent[1].max(c.location.y + c.size.height + c.margin.bottom);
                    }
                }
                let max = [
                    if axes.horizontal() {
                        (extent[0] + l.padding.right - l.size.width).max(0.0)
                    } else {
                        0.0
                    },
                    if axes.vertical() {
                        (extent[1] + l.padding.bottom - l.size.height).max(0.0)
                    } else {
                        0.0
                    },
                ];
                (
                    [offset[0].clamp(0.0, max[0]), offset[1].clamp(0.0, max[1])],
                    max,
                )
            }
            UiWidget::VirtualList {
                item_count,
                item_extent,
                scroll: offset,
                horizontal,
                ..
            } => {
                let viewport = if *horizontal {
                    content.width
                } else {
                    content.height
                };
                let max = (*item_count as f32 * item_extent - viewport).max(0.0);
                let applied = offset.clamp(0.0, max);
                if *horizontal {
                    ([applied, 0.0], [max, 0.0])
                } else {
                    ([0.0, applied], [0.0, max])
                }
            }
            _ => ([0.0; 2], [0.0; 2]),
        };
        let scrolls = matches!(
            node.widget,
            UiWidget::ScrollView { .. } | UiWidget::VirtualList { .. }
        );
        let child_clip = if scrolls {
            // Children never paint over scroll view chrome (gutters live in padding).
            let gutter = match &node.widget {
                UiWidget::ScrollView {
                    axes,
                    scrollbars: true,
                    ..
                } => [
                    if axes.vertical() { SCROLLBAR } else { 0.0 },
                    if axes.horizontal() { SCROLLBAR } else { 0.0 },
                ],
                _ => [0.0; 2],
            };
            intersection(
                clip,
                Rect {
                    width: (rect.width - gutter[0]).max(0.0),
                    height: (rect.height - gutter[1]).max(0.0),
                    ..rect
                },
            )
        } else if node.style.clip {
            intersection(clip, rect)
        } else {
            clip
        };
        self.boxes.push(UiBox {
            node: index,
            rect,
            content,
            clip,
            enabled,
            item,
            overlay,
            scroll,
            scroll_max,
        });
        let child_origin = [rect.x - scroll[0], rect.y - scroll[1]];
        if let UiWidget::VirtualList {
            item_count,
            item_extent,
            overscan,
            horizontal,
            ..
        } = &node.widget
        {
            let Some(template) = children.first().copied() else {
                return Ok(());
            };
            let (main, viewport) = if *horizontal {
                (scroll[0], content.width)
            } else {
                (scroll[1], content.height)
            };
            for i in virtual_range(*item_count, *item_extent, main, viewport, *overscan) {
                let shift = i as f32 * item_extent;
                let origin = if *horizontal {
                    [child_origin[0] + shift, child_origin[1]]
                } else {
                    [child_origin[0], child_origin[1] + shift]
                };
                self.emit(template, origin, child_clip, enabled, Some(i), overlay)?;
            }
            return Ok(());
        }
        for child in children {
            if !matches!(self.tree.nodes[*child].widget, UiWidget::ModalDialog { .. }) {
                self.emit(*child, child_origin, child_clip, enabled, item, overlay)?;
            }
        }
        Ok(())
    }
}
