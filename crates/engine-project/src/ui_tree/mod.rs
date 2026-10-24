//! Retained, authored game UI in logical pixels. Independent of the editor UI.
use crate::{Error, Result, ui::Rect};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

mod interaction;
mod layout;
mod render;
mod schema;
mod world;

pub use interaction::{GameUi, UiInteraction, UiWidgetEvent};
pub use layout::UiBox;
pub use schema::descriptor;
pub use world::{extract_world, register, tick_world};

pub const MAX_UI_NODES: usize = 1024;
pub const MAX_UI_DEPTH: usize = 64;

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
    pub widget: UiWidget,
    pub style: UiStyle,
    pub visible: bool,
    pub enabled: bool,
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
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiWidget {
    #[default]
    Container,
    Label {
        text: String,
    },
    Button {
        text: String,
        #[serde(default)]
        action: String,
    },
    Toggle {
        text: String,
        checked: bool,
        #[serde(default)]
        action: String,
    },
    Slider {
        value: f32,
        min: f32,
        max: f32,
        step: f32,
        #[serde(default)]
        action: String,
    },
    Progress {
        value: f32,
        min: f32,
        max: f32,
    },
}

impl UiWidget {
    pub fn interactive(&self) -> bool {
        matches!(
            self,
            Self::Button { .. } | Self::Toggle { .. } | Self::Slider { .. }
        )
    }
    fn text(&self) -> Option<&str> {
        match self {
            Self::Label { text } | Self::Button { text, .. } | Self::Toggle { text, .. } => {
                Some(text)
            }
            _ => None,
        }
    }
    fn action(&self) -> &str {
        match self {
            Self::Button { action, .. }
            | Self::Toggle { action, .. }
            | Self::Slider { action, .. } => action,
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "unit", content = "value", rename_all = "snake_case")]
pub enum UiLength {
    Pixels(f32),
    /// Fraction of the parent's available content dimension, in 0..=1.
    Percent(f32),
    /// Remaining main-axis space, shared equally between fill siblings.
    #[default]
    Fill,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiLayout {
    #[default]
    Absolute,
    Row,
    Column,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiStyle {
    pub width: UiLength,
    pub height: UiLength,
    /// Offset from the allocated top-left corner (also in rows and columns).
    pub offset: [f32; 2],
    pub layout: UiLayout,
    pub padding: f32,
    pub gap: f32,
    pub clip: bool,
    pub background: [f32; 4],
    pub foreground: [f32; 4],
    pub accent: [f32; 4],
    pub font: String,
    pub font_scale: f32,
}

impl Default for UiStyle {
    fn default() -> Self {
        Self {
            width: UiLength::Pixels(120.0),
            height: UiLength::Pixels(28.0),
            offset: [0.0; 2],
            layout: UiLayout::Absolute,
            padding: 4.0,
            gap: 4.0,
            clip: true,
            background: [0.1, 0.12, 0.16, 1.0],
            foreground: [1.0; 4],
            accent: [0.25, 0.6, 1.0, 1.0],
            font: String::new(),
            font_scale: 1.0,
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

impl UiRoot {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(invalid("Unsupported UiRoot version"));
        }
        self.tree.validate()
    }
}
