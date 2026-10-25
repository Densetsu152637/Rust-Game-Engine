//! Retained, authored game UI in logical pixels. Independent of the editor UI.
//!
//! Module map: `style` (layout/paint style), `widget` (widget kinds), `tree`
//! (hierarchy + validation), `layout` (taffy flexbox/grid), `text` (text
//! backend contract), `navigation` (tab order + spatial focus), `text_edit`
//! (text-input event model), `interaction` (`GameUi` resource), `render`
//! (draw extraction), `schema` (authoring descriptor), `builder` (native
//! constructors) and `world` (ECS integration). The styling layer
//! (`docs/ui-styling.md`) adds `selector`, `stylesheet` (themes and cascade),
//! `animation` (transitions, keyframes), `resolve` (the one style-resolution
//! step), `binding` (data binding), `world_panel` (world-space panels) and
//! `style_schema` (their descriptors). The authoring layer
//! (`docs/ui-authoring.md`) adds `document` (edit operations, undo, selection),
//! `diff`, `inspect` (layout inspection), `raster` (CPU rasterizer) and
//! `preview` (headless rendering). `gpu` is the hand-off to the UI render
//! features of `engine-ui-render` (`docs/ui-render.md`). `localize` applies message references and
//! per-locale asset overrides (`docs/localization.md`).
use crate::{Error, Result, ui::Rect};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

mod accessibility;
mod animation;
mod binding;
mod builder;
mod chrome;
mod diff;
mod document;
mod gpu;
mod inspect;
mod interaction;
mod layout;
mod localize;
mod navigation;
mod preview;
mod raster;
mod render;
mod resolve;
mod schema;
mod selector;
mod shaped_text;
mod style;
mod style_schema;
mod stylesheet;
mod text;
mod text_edit;
mod tree;
mod widget;
mod world;
mod world_panel;

pub use accessibility::{
    AccessAction, AccessId, AccessLive, AccessNode, AccessPart, AccessRole, AccessScroll,
    AccessStates, AccessTree, AccessUpdate, AccessValue, MAX_ACCESS_DESCRIPTION, MAX_ACCESS_NAME,
    MAX_ACCESS_OUTCOMES, MAX_ACCESS_REQUESTS, UiAccessAction, UiAccessOutcome, UiAccessRequest,
    UiAccessStatus, UiAccessibility,
};
pub use animation::{
    Easing, InfiniteTag, MAX_ANIMATION_ITERATIONS, MAX_KEYFRAMES, MAX_MOTION_SECONDS,
    MAX_RULE_TRANSITIONS, UiAnimProperty, UiAnimation, UiAnimationDirection, UiAnimationPhase,
    UiIterations, UiKeyframe, UiKeyframes, UiTransition, UiValue,
};
pub use binding::{
    MAX_BIND_ARGS, MAX_BIND_FORMAT, MAX_BIND_PATH, MAX_BIND_SEGMENTS, MAX_DATA_BYTES,
    MAX_DATA_DEPTH, MAX_DATA_ITEMS, MAX_DATA_STRING, MAX_UI_BINDINGS, UiBindMode, UiBindProperty,
    UiBinding, UiBindingState, UiBindingStats, UiData, UiDataContext, UiItemTextSource,
    UiItemTexts, UiPathSegment, UiStringResolver, parse_path,
};
pub use diff::{UiDiff, UiNodeChange, diff_ops, diff_roots};
pub use document::{
    DEFAULT_UI_HISTORY, MAX_UI_HISTORY, MAX_UI_OPS, MAX_UI_PATCH_BYTES, UI_WIDGET_KINDS, UiChange,
    UiChangeKind, UiDocument, UiOp, UiOpReport, apply_ops,
};
pub use gpu::{
    FEATURE_COLOR_FILTER, FEATURE_SDF_TEXT, FEATURE_WORLD, UiFeatureReport, UiFeatureState,
    UiFeatureStatus, UiGpuFeature, UiGpuFrame, UiGpuQueue, UiGpuRouting, UiRenderStatus,
    WorldBillboard, WorldUiDraw, WorldUiItem, WorldUiKind, is_sdf_draw,
};
pub use inspect::{
    MAX_UI_ISSUES, UiHitMode, UiInspection, UiIssue, UiIssueSeverity, UiNodeLayout, inspect_layout,
    state_flag, state_names,
};
pub use interaction::{GameUi, UiInteraction, UiStateFlags, UiTickContext, UiWidgetEvent};
pub use layout::{MAX_UI_BOXES, UiBox};
pub use localize::localize_root;
pub use navigation::UiDirection;
pub use preview::{
    UiPreview, UiPreviewAssets, UiPreviewOptions, UiViewOptions, effective_root,
    referenced_textures, render_preview,
};
pub use raster::{
    MAX_RASTER_DIMENSION, MAX_RASTER_DRAWS, MAX_RASTER_FILL, MAX_RASTER_TRIANGLES, RasterImage,
    RasterOptions, UiSampling, painted_bounds, rasterize,
};
pub use render::{UI_DIALOG_LAYER, UI_POPUP_LAYER, UiRenderState};
pub use resolve::{UiStyleRuntime, UiStyleSettings};
pub use schema::descriptor;
pub use selector::{MAX_SELECTOR_COMPOUNDS, MAX_SELECTOR_LEN, MAX_SELECTOR_LIST};
pub(crate) use shaped_text::PlacedText;
pub use shaped_text::{
    EngineTextBackend, SdfPathStatus, TextRenderState, TextRenderStatus, UiTextStats,
    validate_markup,
};
pub use style::{
    MAX_GRID_TRACKS, MAX_UI_EXTENT, UiAlign, UiEdges, UiGridFlow, UiGridPlacement, UiInset,
    UiJustify, UiLayout, UiLength, UiPosition, UiStyle, UiTrack, UiTrackSize, UiWrap,
};
pub use style_schema::{descriptors, validate_component};
pub use stylesheet::{
    MAX_COMPILED_RULES, MAX_THEME_COST, MAX_THEME_DEPTH, MAX_THEME_KEYFRAMES, MAX_THEME_RULES,
    MAX_THEME_TOKENS, UiComposedTheme, UiRule, UiStyleDelta, UiTheme, UiThemeSet, UiToken,
    UiTokens,
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
pub use world::{
    UiExtraction, UiTickEvents, extract_world, extract_world_routed, extract_world_with, register,
    reset_world, tick_world,
};
pub use world_panel::{
    MAX_PANEL_PIXELS, PANEL_Z_STEP, UiBillboard, UiWorldPanel, UiWorldPanels, UiWorldPointer,
    camera2d_ray, camera3d_ray, clip_quads, world_mesh,
};

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
    /// Theme name for this root; unset uses `UiStyleSettings::active_theme`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Reduced-motion override for this root; unset follows the global setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduced_motion: Option<bool>,
    /// Data bindings of widget properties to the `UiDataContext`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<UiBinding>,
}

impl Default for UiRoot {
    fn default() -> Self {
        Self {
            version: 1,
            tree: UiTree::default(),
            layer: 0,
            visible: true,
            enabled: true,
            theme: None,
            reduced_motion: None,
            bindings: vec![],
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
    /// Class names matched by `.class` stylesheet selectors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<UiTooltip>,
    #[serde(skip_serializing_if = "is_default_nav")]
    pub nav: UiNavigation,
    /// Accessible name, description, role, live region and hiding overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<UiAccessibility>,
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
            accessibility: None,
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
        if self
            .theme
            .as_deref()
            .is_some_and(|t| !stylesheet::valid_name(t))
        {
            return Err(invalid("Invalid theme name"));
        }
        let hierarchy = self.tree.hierarchy()?;
        binding::validate_all(&self.bindings, &self.tree.nodes, &hierarchy)
    }
}
