//! Widget kinds and their authored state. Widget values (checked, slider
//! value, text, selection, scroll offset, dialog open) live in `UiRoot` so
//! they persist in snapshots and are editable through `component_set`.
use super::*;

/// Byte limit for one authored string (label text, value, markup, option).
pub const MAX_UI_TEXT: usize = 4096;
/// Byte limit for all strings of one `UiRoot`.
pub const MAX_UI_ROOT_TEXT: usize = 65536;
pub const MAX_UI_ACTION: usize = 128;
pub const MAX_DROPDOWN_OPTIONS: usize = 64;
pub const MAX_TABS: usize = 32;
pub const MAX_DIALOG_BUTTONS: usize = 4;
/// Short strings: option, tab and dialog button labels, dialog titles.
pub const MAX_UI_LABEL: usize = 256;
pub const MAX_VIRTUAL_ITEMS: u32 = 1_000_000;
/// Nodes in a virtual-list template subtree.
pub const MAX_TEMPLATE_NODES: usize = 64;
/// Replicas emitted per virtual list per layout, after overscan.
pub const MAX_VIRTUAL_VISIBLE: u32 = 256;
pub const MAX_SCROLL_OFFSET: f32 = 1.0e7;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiImageFit {
    /// Fill the content box, ignoring the source aspect ratio.
    #[default]
    Stretch,
    /// Largest centred rectangle with the source aspect ratio inside the box.
    Contain,
    /// Fill the box with the source aspect ratio, cropping the UV rectangle.
    Cover,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiScrollAxes {
    Horizontal,
    #[default]
    Vertical,
    Both,
}

impl UiScrollAxes {
    pub fn horizontal(self) -> bool {
        matches!(self, Self::Horizontal | Self::Both)
    }
    pub fn vertical(self) -> bool {
        matches!(self, Self::Vertical | Self::Both)
    }
}

const fn full_uv() -> [f32; 4] {
    [0.0, 0.0, 1.0, 1.0]
}
const fn white() -> [f32; 4] {
    [1.0; 4]
}
const fn yes() -> bool {
    true
}
const fn default_max_length() -> u32 {
    256
}
const fn default_max_visible() -> u32 {
    8
}
const fn default_bar_height() -> f32 {
    28.0
}
const fn default_overscan() -> u32 {
    1
}
const fn default_tooltip_delay() -> f32 {
    0.5
}
fn is_false(value: &bool) -> bool {
    !*value
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum UiWidget {
    #[default]
    Container,
    Label {
        text: String,
    },
    /// Rich-text markup kept as a plain stored string; the text backend
    /// interprets it (the bitmap backend strips `<...>` tags).
    RichLabel {
        markup: String,
    },
    Image {
        texture: String,
        /// Normalized source rectangle `[u, v, width, height]`.
        #[serde(default = "full_uv")]
        uv: [f32; 4],
        #[serde(default = "white")]
        tint: [f32; 4],
        #[serde(default)]
        fit: UiImageFit,
    },
    NineSlice {
        texture: String,
        /// Source insets in texture pixels: left, top, right, bottom.
        slices: [f32; 4],
        /// Destination border in logical pixels; absent uses `slices`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        border: Option<[f32; 4]>,
        #[serde(default = "yes")]
        center: bool,
        #[serde(default = "white")]
        tint: [f32; 4],
    },
    Button {
        text: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    Toggle {
        text: String,
        checked: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    Slider {
        value: f32,
        min: f32,
        max: f32,
        step: f32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    Progress {
        value: f32,
        min: f32,
        max: f32,
    },
    /// Editable text. `caret` and `anchor` are character indices; the selection
    /// is the range between them.
    TextInput {
        /// The edited value.
        #[serde(default)]
        text: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
        /// Maximum length in characters.
        #[serde(default = "default_max_length")]
        max_length: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        password: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        multiline: bool,
        #[serde(default, skip_serializing_if = "is_zero")]
        caret: u32,
        #[serde(default, skip_serializing_if = "is_zero")]
        anchor: u32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    /// Single choice; the option list opens as a popup above all UI.
    Dropdown {
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
        /// Options shown before the popup scrolls.
        #[serde(default = "default_max_visible")]
        max_visible: u32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    /// Scrolls its children; `offset` is clamped to the content extent.
    ScrollView {
        #[serde(default)]
        offset: [f32; 2],
        #[serde(default)]
        axes: UiScrollAxes,
        #[serde(default = "yes")]
        scrollbars: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    /// Selectable list whose children are the items (in vector order).
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    /// Fixed-extent items generated from one template child; only the visible
    /// range (plus overscan) is laid out and drawn.
    VirtualList {
        item_count: u32,
        item_extent: f32,
        /// ID of the only direct child, replicated per visible item.
        template: String,
        /// Scroll offset along the list axis, clamped to the content extent.
        #[serde(default)]
        scroll: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        #[serde(default = "default_overscan")]
        overscan: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        horizontal: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    /// Tab strip; child `n` (in vector order) is the panel for `labels[n]`.
    Tabs {
        labels: Vec<String>,
        #[serde(default)]
        selected: u32,
        #[serde(default = "default_bar_height")]
        header_height: f32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
    /// Modal dialog: while open it is laid out centred over the viewport, blocks
    /// input to everything beneath and traps focus. Children are its content.
    ModalDialog {
        #[serde(default)]
        title: String,
        #[serde(default)]
        open: bool,
        #[serde(default)]
        buttons: Vec<String>,
        #[serde(default = "default_bar_height")]
        title_height: f32,
        #[serde(default = "default_bar_height")]
        button_height: f32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        action: String,
    },
}

/// Hover tooltip shown after `delay` seconds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UiTooltip {
    pub text: String,
    #[serde(default = "default_tooltip_delay")]
    pub delay: f32,
}

/// Keyboard/gamepad navigation overrides. Direction targets are node IDs in
/// the same root; they win over spatial search when focusable.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiNavigation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
    /// Tab order key: ascending, ties broken by paint order.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub tab_index: i32,
    /// `false` keeps a widget pointer-only (skipped by keyboard focus).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focusable: Option<bool>,
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

impl UiWidget {
    /// Receives pointer presses.
    pub fn interactive(&self) -> bool {
        match self {
            // A rich label is a pointer target only when its markup has links.
            Self::RichLabel { markup } => markup.contains("[link="),
            _ => !matches!(
                self,
                Self::Container
                    | Self::Label { .. }
                    | Self::Image { .. }
                    | Self::NineSlice { .. }
                    | Self::Progress { .. }
            ),
        }
    }
    /// Can hold keyboard focus by default.
    pub fn focusable(&self) -> bool {
        self.interactive() && !matches!(self, Self::ScrollView { .. })
    }
    pub fn action(&self) -> &str {
        match self {
            Self::Button { action, .. }
            | Self::Toggle { action, .. }
            | Self::Slider { action, .. }
            | Self::TextInput { action, .. }
            | Self::Dropdown { action, .. }
            | Self::ScrollView { action, .. }
            | Self::List { action, .. }
            | Self::VirtualList { action, .. }
            | Self::Tabs { action, .. }
            | Self::ModalDialog { action, .. } => action,
            _ => "",
        }
    }
    /// Every authored string, for the per-root text budget.
    pub(super) fn strings(&self) -> Vec<&str> {
        match self {
            Self::Container | Self::Progress { .. } | Self::Slider { .. } => vec![],
            Self::Label { text } | Self::Button { text, .. } | Self::Toggle { text, .. } => {
                vec![text]
            }
            Self::RichLabel { markup } => vec![markup],
            Self::Image { texture, .. } | Self::NineSlice { texture, .. } => vec![texture],
            Self::TextInput {
                text, placeholder, ..
            } => vec![text, placeholder],
            Self::Dropdown {
                options,
                placeholder,
                ..
            } => options
                .iter()
                .map(String::as_str)
                .chain([placeholder.as_str()])
                .collect(),
            Self::ScrollView { .. } | Self::List { .. } => vec![],
            Self::VirtualList { template, .. } => vec![template],
            Self::Tabs { labels, .. } => labels.iter().map(String::as_str).collect(),
            Self::ModalDialog { title, buttons, .. } => buttons
                .iter()
                .map(String::as_str)
                .chain([title.as_str()])
                .collect(),
        }
    }
    pub(super) fn text_bytes(&self) -> usize {
        self.strings().iter().map(|s| s.len()).sum()
    }

    /// Shape and range checks that need no hierarchy.
    pub(super) fn validate(&self, id: &str) -> Result<()> {
        let fail = |what: &str| Err(invalid(format!("{what} on UI node {id}")));
        if self.strings().iter().any(|s| s.len() > MAX_UI_TEXT)
            || self.action().len() > MAX_UI_ACTION
        {
            return fail("Text or action too long");
        }
        let color = |c: &[f32; 4]| c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v));
        let label = |s: &String| s.len() <= MAX_UI_LABEL;
        match self {
            Self::Slider {
                value,
                min,
                max,
                step,
                ..
            } => {
                if !step.is_finite() || *step <= 0.0 || *step > max - min {
                    return Err(invalid("Slider step must be positive and within its range"));
                }
                validate_range(*value, *min, *max)?;
            }
            Self::Progress { value, min, max } => validate_range(*value, *min, *max)?,
            Self::Image {
                texture, uv, tint, ..
            } => {
                if texture.is_empty()
                    || texture.len() > 128
                    || !color(tint)
                    || !uv.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    || uv[0] + uv[2] > 1.0 + 1e-6
                    || uv[1] + uv[3] > 1.0 + 1e-6
                    || uv[2] <= 0.0
                    || uv[3] <= 0.0
                {
                    return fail("Image texture, UV rectangle or tint invalid");
                }
            }
            Self::NineSlice {
                texture,
                slices,
                border,
                tint,
                ..
            } => {
                let px = |v: &f32| v.is_finite() && (0.0..=MAX_UI_EXTENT).contains(v);
                if texture.is_empty()
                    || texture.len() > 128
                    || !color(tint)
                    || !slices.iter().all(px)
                    || !border.iter().flatten().all(px)
                {
                    return fail("Nine-slice texture, slices, border or tint invalid");
                }
            }
            Self::TextInput {
                text: value,
                placeholder,
                max_length,
                caret,
                anchor,
                multiline,
                ..
            } => {
                let chars = value.chars().count() as u32;
                if !(1..=MAX_UI_TEXT as u32).contains(max_length)
                    || placeholder.len() > MAX_UI_LABEL
                    || chars > *max_length
                    || *caret > chars
                    || *anchor > chars
                    || (!multiline && value.contains('\n'))
                    || value
                        .chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\t')
                {
                    return fail("Text input value, length limit or caret invalid");
                }
            }
            Self::Dropdown {
                options,
                selected,
                max_visible,
                placeholder,
                ..
            } => {
                if options.len() > MAX_DROPDOWN_OPTIONS
                    || !options.iter().all(label)
                    || !label(placeholder)
                    || selected.is_some_and(|s| s as usize >= options.len())
                    || !(1..=MAX_DROPDOWN_OPTIONS as u32).contains(max_visible)
                {
                    return fail("Dropdown options, selection or popup size invalid");
                }
            }
            Self::ScrollView { offset, .. } => {
                if !offset
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=MAX_SCROLL_OFFSET).contains(v))
                {
                    return fail("Scroll offset invalid");
                }
            }
            Self::VirtualList {
                item_count,
                item_extent,
                template,
                scroll: offset,
                selected,
                overscan,
                ..
            } => {
                if *item_count > MAX_VIRTUAL_ITEMS
                    || !item_extent.is_finite()
                    || !(1.0..=4096.0).contains(item_extent)
                    || template.is_empty()
                    || template.len() > 128
                    || !offset.is_finite()
                    || !(0.0..=MAX_SCROLL_OFFSET).contains(offset)
                    || selected.is_some_and(|s| s >= *item_count)
                    || *overscan > 16
                {
                    return fail(
                        "Virtual list count, extent, template, offset or selection invalid",
                    );
                }
            }
            Self::Tabs {
                labels,
                selected,
                header_height,
                ..
            } => {
                if labels.is_empty()
                    || labels.len() > MAX_TABS
                    || !labels.iter().all(label)
                    || *selected as usize >= labels.len()
                    || !header_height.is_finite()
                    || !(0.0..=256.0).contains(header_height)
                {
                    return fail("Tabs labels, selection or header height invalid");
                }
            }
            Self::ModalDialog {
                title,
                buttons,
                title_height,
                button_height,
                ..
            } if !label(title)
                || buttons.len() > MAX_DIALOG_BUTTONS
                || !buttons.iter().all(label)
                || ![title_height, button_height]
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=256.0).contains(*v)) =>
            {
                return fail("Dialog title, buttons or bar heights invalid");
            }
            // The text engine rejects a malformed known tag; catching it here
            // keeps it out of the frame. (Run-time text goes through
            // `fit_markup` instead.)
            Self::RichLabel { markup } => {
                if let Err(e) = shaped_text::validate_markup(markup) {
                    return fail(&format!("RichLabel markup invalid ({e})"));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_range(value: f32, min: f32, max: f32) -> Result<()> {
    if ![value, min, max, max - min].iter().all(|v| v.is_finite())
        || min >= max
        || value < min
        || value > max
    {
        return Err(invalid(
            "Widget range must be finite, increasing and contain its value",
        ));
    }
    Ok(())
}

impl UiTooltip {
    pub(super) fn validate(&self, id: &str) -> Result<()> {
        if self.text.len() > 1024 || !self.delay.is_finite() || !(0.0..=10.0).contains(&self.delay)
        {
            return Err(invalid(format!("Invalid tooltip on UI node {id}")));
        }
        Ok(())
    }
}

/// Visible item range `[start, end)` of a virtual list scrolled by `offset`
/// with a `viewport` main-axis extent, widened by `overscan` items each side
/// and capped at [`MAX_VIRTUAL_VISIBLE`] items.
pub fn virtual_range(
    item_count: u32,
    item_extent: f32,
    offset: f32,
    viewport: f32,
    overscan: u32,
) -> std::ops::Range<u32> {
    if item_count == 0
        || !item_extent.is_finite()
        || item_extent <= 0.0
        || !viewport.is_finite()
        || viewport <= 0.0
    {
        return 0..0;
    }
    let max = (item_count as f32 * item_extent - viewport).max(0.0);
    let offset = if offset.is_finite() {
        offset.clamp(0.0, max)
    } else {
        0.0
    };
    let first = (offset / item_extent).floor() as u32;
    let last = ((offset + viewport) / item_extent).ceil() as u32;
    let start = first.saturating_sub(overscan).min(item_count);
    let end = last.saturating_add(overscan).min(item_count);
    start..end.min(start.saturating_add(MAX_VIRTUAL_VISIBLE))
}
