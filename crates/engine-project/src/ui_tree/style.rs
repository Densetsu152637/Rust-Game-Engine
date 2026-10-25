//! Authored layout and paint style. Fields map onto taffy flexbox/grid styles in
//! `layout.rs`; every field has a serde default so version-1 JSON keeps loading.
use super::*;

/// Upper bound for authored lengths, offsets and spacing in logical pixels.
pub const MAX_UI_EXTENT: f32 = 16384.0;
/// Upper bound for explicit grid tracks per axis and for grid line indices.
pub const MAX_GRID_TRACKS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "unit", content = "value", rename_all = "snake_case")]
pub enum UiLength {
    Pixels(f32),
    /// Fraction of the parent's content box dimension, in 0..=1.
    Percent(f32),
    /// Take the parent's remaining space: flex `1 1 0` with a zero minimum on a
    /// row/column main axis, stretch on a cross axis, stretch in overlay/grid cells.
    #[default]
    Fill,
    /// Content size (measured text, children); CSS `auto`.
    Auto,
}

/// How a node arranges its children.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiLayout {
    /// Overlay: every child is placed at the content-box top-left (plus its
    /// offset/margin); children do not affect each other.
    #[default]
    Absolute,
    Row,
    Column,
    RowReverse,
    ColumnReverse,
    /// CSS grid using `gridColumns`/`gridRows` tracks.
    Grid,
}

impl UiLayout {
    pub(super) fn is_row(self) -> bool {
        matches!(self, Self::Row | Self::RowReverse)
    }
}

/// Whether a node takes part in its parent's flow.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiPosition {
    #[default]
    Flow,
    /// Out of flow; placed by `inset` against the parent's padding box (CSS absolute).
    Absolute,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiEdges {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl UiEdges {
    pub const fn uniform(value: f32) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }
    fn values(&self) -> [f32; 4] {
        [self.left, self.top, self.right, self.bottom]
    }
}

/// Absolute-position insets; an absent side is `auto`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiInset {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

/// Main-axis distribution (`justify`) and multi-line distribution (`alignContent`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiJustify {
    Start,
    End,
    Center,
    Stretch,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Cross-axis alignment for `alignItems`/`alignSelf`/`justifyItems`/`justifySelf`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiAlign {
    Start,
    End,
    Center,
    Baseline,
    Stretch,
}

/// A grid track breadth (also a `minmax` bound).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiTrackSize {
    Px { value: f32 },
    Percent { value: f32 },
    Fr { value: f32 },
    Auto,
    MinContent,
    MaxContent,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiTrack {
    Px { value: f32 },
    Percent { value: f32 },
    Fr { value: f32 },
    Auto,
    MinContent,
    MaxContent,
    Minmax { min: UiTrackSize, max: UiTrackSize },
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiGridFlow {
    #[default]
    Row,
    Column,
    RowDense,
    ColumnDense,
}

/// Grid line placement: 1-based (negative counts from the end) start/end lines
/// and/or a span. `{ "start": 2, "span": 3 }` is CSS `2 / span 3`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct UiGridPlacement {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<u16>,
}

/// Style of one node. Every field defaults (see [`UiStyle::DEFAULT`]) and is
/// omitted from serialized JSON while it equals its default, which keeps
/// snapshots of large trees inside the 64 KiB component limit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiStyle {
    #[serde(skip_serializing_if = "default_width")]
    pub width: UiLength,
    #[serde(skip_serializing_if = "default_height")]
    pub height: UiLength,
    /// `auto` means no minimum. `fill` is rejected.
    #[serde(skip_serializing_if = "is_auto")]
    pub min_width: UiLength,
    #[serde(skip_serializing_if = "is_auto")]
    pub min_height: UiLength,
    /// `auto` means no maximum. `fill` is rejected.
    #[serde(skip_serializing_if = "is_auto")]
    pub max_width: UiLength,
    #[serde(skip_serializing_if = "is_auto")]
    pub max_height: UiLength,
    /// Width divided by height, applied when one dimension is content sized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<f32>,
    /// Translation applied after layout to the node and its subtree; it never
    /// moves siblings (in every layout mode).
    #[serde(skip_serializing_if = "is_zero2")]
    pub offset: [f32; 2],
    #[serde(skip_serializing_if = "is_default")]
    pub layout: UiLayout,
    #[serde(skip_serializing_if = "is_default")]
    pub position: UiPosition,
    #[serde(skip_serializing_if = "is_default")]
    pub inset: UiInset,
    #[serde(skip_serializing_if = "is_default")]
    pub margin: UiEdges,
    /// Uniform padding; `paddingEdges` overrides it per side when present.
    #[serde(skip_serializing_if = "default_padding")]
    pub padding: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding_edges: Option<UiEdges>,
    /// Gap between rows and columns; `rowGap`/`columnGap` override one axis.
    #[serde(skip_serializing_if = "default_gap")]
    pub gap: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_gap: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_gap: Option<f32>,
    #[serde(skip_serializing_if = "is_default")]
    pub wrap: UiWrap,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justify: Option<UiJustify>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align_items: Option<UiAlign>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align_self: Option<UiAlign>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align_content: Option<UiJustify>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justify_items: Option<UiAlign>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justify_self: Option<UiAlign>,
    /// Flex grow; absent means 1 for `fill` main sizes and 0 otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grow: Option<f32>,
    /// Flex shrink; absent means 0 for pixel/percent main sizes (legacy rows
    /// overflow instead of squashing) and 1 for `fill`/`auto`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shrink: Option<f32>,
    /// Flex basis; `auto` uses the main size. `fill` is rejected.
    #[serde(skip_serializing_if = "is_auto")]
    pub basis: UiLength,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub grid_columns: Vec<UiTrack>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub grid_rows: Vec<UiTrack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid_auto_columns: Option<UiTrack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid_auto_rows: Option<UiTrack>,
    #[serde(skip_serializing_if = "is_default")]
    pub grid_auto_flow: UiGridFlow,
    #[serde(skip_serializing_if = "is_default")]
    pub grid_column: UiGridPlacement,
    #[serde(skip_serializing_if = "is_default")]
    pub grid_row: UiGridPlacement,
    #[serde(skip_serializing_if = "default_clip")]
    pub clip: bool,
    #[serde(skip_serializing_if = "default_background")]
    pub background: [f32; 4],
    #[serde(skip_serializing_if = "default_foreground")]
    pub foreground: [f32; 4],
    #[serde(skip_serializing_if = "default_accent")]
    pub accent: [f32; 4],
    #[serde(skip_serializing_if = "String::is_empty")]
    pub font: String,
    #[serde(skip_serializing_if = "default_font_scale")]
    pub font_scale: f32,
}

impl UiStyle {
    pub const DEFAULT: Self = Self {
        width: UiLength::Pixels(120.0),
        height: UiLength::Pixels(28.0),
        min_width: UiLength::Auto,
        min_height: UiLength::Auto,
        max_width: UiLength::Auto,
        max_height: UiLength::Auto,
        aspect_ratio: None,
        offset: [0.0; 2],
        layout: UiLayout::Absolute,
        position: UiPosition::Flow,
        inset: UiInset {
            left: None,
            top: None,
            right: None,
            bottom: None,
        },
        margin: UiEdges::uniform(0.0),
        padding: 4.0,
        padding_edges: None,
        gap: 4.0,
        row_gap: None,
        column_gap: None,
        wrap: UiWrap::NoWrap,
        justify: None,
        align_items: None,
        align_self: None,
        align_content: None,
        justify_items: None,
        justify_self: None,
        grow: None,
        shrink: None,
        basis: UiLength::Auto,
        grid_columns: Vec::new(),
        grid_rows: Vec::new(),
        grid_auto_columns: None,
        grid_auto_rows: None,
        grid_auto_flow: UiGridFlow::Row,
        grid_column: UiGridPlacement {
            start: None,
            end: None,
            span: None,
        },
        grid_row: UiGridPlacement {
            start: None,
            end: None,
            span: None,
        },
        clip: true,
        background: [0.1, 0.12, 0.16, 1.0],
        foreground: [1.0; 4],
        accent: [0.25, 0.6, 1.0, 1.0],
        font: String::new(),
        font_scale: 1.0,
    };

    /// Resolved per-side padding.
    pub fn padding_edges(&self) -> UiEdges {
        self.padding_edges.unwrap_or(UiEdges::uniform(self.padding))
    }
}

impl Default for UiStyle {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}
fn is_auto(value: &UiLength) -> bool {
    *value == UiLength::Auto
}
fn is_zero2(value: &[f32; 2]) -> bool {
    *value == [0.0; 2]
}
macro_rules! default_check {
    ($($name:ident: $field:ident: $ty:ty),* $(,)?) => {
        $(fn $name(value: &$ty) -> bool { *value == UiStyle::DEFAULT.$field })*
    };
}
default_check!(
    default_width: width: UiLength,
    default_height: height: UiLength,
    default_padding: padding: f32,
    default_gap: gap: f32,
    default_clip: clip: bool,
    default_background: background: [f32; 4],
    default_foreground: foreground: [f32; 4],
    default_accent: accent: [f32; 4],
    default_font_scale: font_scale: f32,
);

fn extent(value: f32) -> bool {
    value.is_finite() && (0.0..=MAX_UI_EXTENT).contains(&value)
}
fn signed_extent(value: f32) -> bool {
    value.is_finite() && value.abs() <= MAX_UI_EXTENT
}
fn valid_length(length: UiLength, allow_fill: bool) -> bool {
    match length {
        UiLength::Fill => allow_fill,
        UiLength::Auto => true,
        UiLength::Pixels(n) => extent(n),
        UiLength::Percent(n) => n.is_finite() && (0.0..=1.0).contains(&n),
    }
}
fn valid_breadth(size: UiTrackSize, as_min: bool) -> bool {
    match size {
        UiTrackSize::Px { value } => extent(value),
        UiTrackSize::Percent { value } => value.is_finite() && (0.0..=1.0).contains(&value),
        // CSS forbids flexible minimums.
        UiTrackSize::Fr { value } => !as_min && value.is_finite() && value > 0.0 && value <= 1024.0,
        UiTrackSize::Auto | UiTrackSize::MinContent | UiTrackSize::MaxContent => true,
    }
}
fn valid_track(track: UiTrack) -> bool {
    match track {
        UiTrack::Minmax { min, max } => valid_breadth(min, true) && valid_breadth(max, false),
        UiTrack::Px { value } => valid_breadth(UiTrackSize::Px { value }, false),
        UiTrack::Percent { value } => valid_breadth(UiTrackSize::Percent { value }, false),
        UiTrack::Fr { value } => valid_breadth(UiTrackSize::Fr { value }, false),
        UiTrack::Auto | UiTrack::MinContent | UiTrack::MaxContent => true,
    }
}
fn valid_placement(p: UiGridPlacement) -> bool {
    let line = |v: Option<i16>| {
        v.is_none_or(|v| v != 0 && v.unsigned_abs() as usize <= MAX_GRID_TRACKS + 1)
    };
    line(p.start)
        && line(p.end)
        && p.span
            .is_none_or(|s| s >= 1 && s as usize <= MAX_GRID_TRACKS)
        && !(p.start.is_some() && p.end.is_some() && p.span.is_some())
}

impl UiStyle {
    /// Check ranges; `id` names the node in the error message.
    pub(super) fn validate(&self, id: &str) -> Result<()> {
        let s = self;
        let colors = [s.background, s.foreground, s.accent];
        let ok = valid_length(s.width, true)
            && valid_length(s.height, true)
            && [
                s.min_width,
                s.min_height,
                s.max_width,
                s.max_height,
                s.basis,
            ]
            .into_iter()
            .all(|l| valid_length(l, false))
            && s.aspect_ratio
                .is_none_or(|r| r.is_finite() && (0.001..=1000.0).contains(&r))
            && s.offset.into_iter().all(signed_extent)
            && [s.inset.left, s.inset.top, s.inset.right, s.inset.bottom]
                .into_iter()
                .flatten()
                .all(signed_extent)
            && s.margin.values().into_iter().all(signed_extent)
            && extent(s.padding)
            && s.padding_edges
                .is_none_or(|p| p.values().into_iter().all(extent))
            && extent(s.gap)
            && [s.row_gap, s.column_gap].into_iter().flatten().all(extent)
            && [s.grow, s.shrink]
                .into_iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=1024.0).contains(&v))
            && s.grid_columns.len() <= MAX_GRID_TRACKS
            && s.grid_rows.len() <= MAX_GRID_TRACKS
            && s.grid_columns
                .iter()
                .chain(&s.grid_rows)
                .chain(s.grid_auto_columns.iter())
                .chain(s.grid_auto_rows.iter())
                .all(|t| valid_track(*t))
            && valid_placement(s.grid_column)
            && valid_placement(s.grid_row)
            && s.font_scale.is_finite()
            && (0.01..=16.0).contains(&s.font_scale)
            && s.font.len() <= 128
            && colors
                .iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v));
        if ok {
            Ok(())
        } else {
            Err(invalid(format!(
                "Invalid style metrics, track, placement or color on UI node {id}"
            )))
        }
    }
}
