//! Retained, authored game UI in logical pixels. Independent of the editor UI.
//!
//! Module map: `style` (layout/paint style), `widget` (widget kinds), `tree`
//! (hierarchy + validation), `layout` (taffy flexbox/grid), `text` (text
//! backend contract), `navigation` (tab order + spatial focus), `text_edit`
//! (text-input event model), `interaction` (`GameUi` resource), `render`
//! (draw extraction), `schema` (authoring descriptor), `builder` (native
//! constructors) and `world` (ECS integration).
use crate::{Error, Result, ui::Rect};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

mod builder;
mod chrome;
mod interaction;
mod layout;
mod navigation;
mod render;
mod schema;
mod shaped_text;
mod style;
mod text;
mod text_edit;
mod tree;
mod widget;
mod world;

pub use interaction::{GameUi, UiInteraction, UiStateFlags, UiTickContext, UiWidgetEvent};
pub use layout::{MAX_UI_BOXES, UiBox};
pub use navigation::UiDirection;
pub use render::{UI_DIALOG_LAYER, UI_POPUP_LAYER, UiRenderState};
pub use schema::descriptor;
pub use shaped_text::{
    EngineTextBackend, SDF_DEFERRED_REASON, TextRenderState, TextRenderStatus, UiTextStats,
    validate_markup,
};
pub use style::{
    MAX_GRID_TRACKS, MAX_UI_EXTENT, UiAlign, UiEdges, UiGridFlow, UiGridPlacement, UiInset,
    UiJustify, UiLayout, UiLength, UiPosition, UiStyle, UiTrack, UiTrackSize, UiWrap,
};
pub use text::{
    BitmapTextBackend, NullTextBackend, SelectingTextBackend, UiCaretStop, UiGlyphQuad,
    UiInlineImage, UiLinkRegion, UiTextBackend, UiTextGeometry, UiTextKind, UiTextLayout,
    UiTextMetrics, UiTextRequest, strip_markup,
};
pub use text_edit::{
    MAX_TEXT_EVENT_BYTES, MAX_TEXT_EVENTS, MemoryClipboard, UiClipboard, UiEditKey,
    UiEditModifiers, UiPreedit, UiTextEvent,
};
pub use widget::{
    MAX_DIALOG_BUTTONS, MAX_DROPDOWN_OPTIONS, MAX_TABS, MAX_TEMPLATE_NODES, MAX_UI_LABEL,
    MAX_UI_ROOT_TEXT, MAX_UI_TEXT, MAX_VIRTUAL_ITEMS, MAX_VIRTUAL_VISIBLE, UiImageFit,
    UiNavigation, UiScrollAxes, UiTooltip, UiWidget, virtual_range,
};
pub use world::{extract_world, extract_world_with, register, tick_world};

/// Nodes per root (natively built trees). Authored JSON is additionally bound
/// by the 64 KiB component limit; serialization omits default values, so a
/// minimal node costs about 30 bytes and a styled text button about 180.
pub const MAX_UI_NODES: usize = 1024;
pub const MAX_UI_DEPTH: usize = 64;
/// Retained roots per world.
pub const MAX_UI_ROOTS: usize = 64;

#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiRoot {
    pub version: u32,
    pub tree: UiTree,
    pub layer: i32,
    pub visible: bool,
    pub enabled: bool,
}

impl Default for UiRoot {
    fn default() -> Self {
        Self {
            version: 1,
            tree: UiTree::default(),
            layer: 0,
            visible: true,
            enabled: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiTree {
    /// Sibling order is vector order, regardless of where their parent appears.
    pub nodes: Vec<UiNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiNode {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "is_container")]
    pub widget: UiWidget,
    #[serde(skip_serializing_if = "is_default_style")]
    pub style: UiStyle,
    #[serde(skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Reserved for the styling layer (stylesheet class selectors); validated
    /// but not interpreted by layout, interaction or rendering yet.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<UiTooltip>,
    #[serde(skip_serializing_if = "is_default_nav")]
    pub nav: UiNavigation,
}

fn is_default_nav(nav: &UiNavigation) -> bool {
    *nav == UiNavigation::default()
}
fn is_container(widget: &UiWidget) -> bool {
    *widget == UiWidget::Container
}
fn is_default_style(style: &UiStyle) -> bool {
    *style == UiStyle::DEFAULT
}
fn is_true(value: &bool) -> bool {
    *value
}

impl Default for UiNode {
    fn default() -> Self {
        Self {
            id: String::new(),
            parent: None,
            widget: UiWidget::Container,
            style: UiStyle::default(),
            visible: true,
            enabled: true,
            classes: vec![],
            tooltip: None,
            nav: UiNavigation::default(),
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("ui.tree.invalid", message)
}

fn intersection(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Rect {
        x,
        y,
        width: (a.x + a.width).min(b.x + b.width).max(x) - x,
        height: (a.y + a.height).min(b.y + b.height).max(y) - y,
    }
}

fn contains(rect: Rect, point: [f32; 2]) -> bool {
    point[0] >= rect.x
        && point[1] >= rect.y
        && point[0] < rect.x + rect.width
        && point[1] < rect.y + rect.height
}

fn viewport_rect(viewport: [u32; 2]) -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        width: viewport[0] as f32,
        height: viewport[1] as f32,
    }
}

impl UiRoot {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(invalid("Unsupported UiRoot version"));
        }
        self.tree.validate()
    }
}
