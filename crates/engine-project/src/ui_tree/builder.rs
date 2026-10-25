//! Native Rust constructors for common widgets. Each returns a plain
//! [`UiNode`]; chain the `with_*`/`child_of` helpers and push into a tree.
use super::*;

impl UiNode {
    pub fn new(id: impl Into<String>, widget: UiWidget) -> Self {
        Self {
            id: id.into(),
            widget,
            ..Default::default()
        }
    }
    pub fn container(id: impl Into<String>, layout: UiLayout) -> Self {
        let mut node = Self::new(id, UiWidget::Container);
        node.style.layout = layout;
        node
    }
    /// Grid container with explicit column and row tracks.
    pub fn grid(id: impl Into<String>, columns: Vec<UiTrack>, rows: Vec<UiTrack>) -> Self {
        let mut node = Self::container(id, UiLayout::Grid);
        node.style.grid_columns = columns;
        node.style.grid_rows = rows;
        node
    }
    pub fn label(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(id, UiWidget::Label { text: text.into() })
    }
    pub fn rich_label(id: impl Into<String>, markup: impl Into<String>) -> Self {
        Self::new(
            id,
            UiWidget::RichLabel {
                markup: markup.into(),
            },
        )
    }
    pub fn image(id: impl Into<String>, texture: impl Into<String>, fit: UiImageFit) -> Self {
        Self::new(
            id,
            UiWidget::Image {
                texture: texture.into(),
                uv: [0.0, 0.0, 1.0, 1.0],
                tint: [1.0; 4],
                fit,
            },
        )
    }
    pub fn nine_slice(id: impl Into<String>, texture: impl Into<String>, slices: [f32; 4]) -> Self {
        Self::new(
            id,
            UiWidget::NineSlice {
                texture: texture.into(),
                slices,
                border: None,
                center: true,
                tint: [1.0; 4],
            },
        )
    }
    pub fn button(
        id: impl Into<String>,
        text: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            UiWidget::Button {
                text: text.into(),
                action: action.into(),
            },
        )
    }
    pub fn toggle(id: impl Into<String>, text: impl Into<String>, checked: bool) -> Self {
        Self::new(
            id,
            UiWidget::Toggle {
                text: text.into(),
                checked,
                action: String::new(),
            },
        )
    }
    pub fn slider(id: impl Into<String>, value: f32, min: f32, max: f32, step: f32) -> Self {
        Self::new(
            id,
            UiWidget::Slider {
                value,
                min,
                max,
                step,
                action: String::new(),
            },
        )
    }
    pub fn progress(id: impl Into<String>, value: f32, min: f32, max: f32) -> Self {
        Self::new(id, UiWidget::Progress { value, min, max })
    }
    pub fn text_input(
        id: impl Into<String>,
        placeholder: impl Into<String>,
        max_length: u32,
    ) -> Self {
        Self::new(
            id,
            UiWidget::TextInput {
                text: String::new(),
                placeholder: placeholder.into(),
                max_length,
                password: false,
                multiline: false,
                caret: 0,
                anchor: 0,
                action: String::new(),
            },
        )
    }
    pub fn dropdown(id: impl Into<String>, options: Vec<String>, selected: Option<u32>) -> Self {
        Self::new(
            id,
            UiWidget::Dropdown {
                options,
                selected,
                placeholder: String::new(),
                max_visible: 8,
                action: String::new(),
            },
        )
    }
    pub fn scroll_view(id: impl Into<String>, axes: UiScrollAxes) -> Self {
        Self::new(
            id,
            UiWidget::ScrollView {
                offset: [0.0; 2],
                axes,
                scrollbars: true,
                action: String::new(),
            },
        )
    }
    /// Vertical selectable list; its children are the items.
    pub fn list(id: impl Into<String>) -> Self {
        let mut node = Self::new(
            id,
            UiWidget::List {
                selected: None,
                action: String::new(),
            },
        );
        node.style.layout = UiLayout::Column;
        node
    }
    /// Vertical virtual list; add the template node as its only child.
    pub fn virtual_list(
        id: impl Into<String>,
        item_count: u32,
        item_extent: f32,
        template: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            UiWidget::VirtualList {
                item_count,
                item_extent,
                template: template.into(),
                scroll: 0.0,
                selected: None,
                overscan: 1,
                horizontal: false,
                action: String::new(),
            },
        )
    }
    pub fn tabs(id: impl Into<String>, labels: Vec<String>) -> Self {
        Self::new(
            id,
            UiWidget::Tabs {
                labels,
                selected: 0,
                header_height: 28.0,
                action: String::new(),
            },
        )
    }
    pub fn dialog(id: impl Into<String>, title: impl Into<String>, buttons: Vec<String>) -> Self {
        Self::new(
            id,
            UiWidget::ModalDialog {
                title: title.into(),
                open: false,
                buttons,
                title_height: 28.0,
                button_height: 28.0,
                action: String::new(),
            },
        )
    }

    pub fn child_of(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }
    pub fn with_size(mut self, width: UiLength, height: UiLength) -> Self {
        self.style.width = width;
        self.style.height = height;
        self
    }
    pub fn with_style(mut self, edit: impl FnOnce(&mut UiStyle)) -> Self {
        edit(&mut self.style);
        self
    }
    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        let action = action.into();
        match &mut self.widget {
            UiWidget::Button { action: a, .. }
            | UiWidget::Toggle { action: a, .. }
            | UiWidget::Slider { action: a, .. }
            | UiWidget::TextInput { action: a, .. }
            | UiWidget::Dropdown { action: a, .. }
            | UiWidget::ScrollView { action: a, .. }
            | UiWidget::List { action: a, .. }
            | UiWidget::VirtualList { action: a, .. }
            | UiWidget::Tabs { action: a, .. }
            | UiWidget::ModalDialog { action: a, .. } => *a = action,
            _ => {}
        }
        self
    }
    pub fn with_tooltip(mut self, text: impl Into<String>, delay: f32) -> Self {
        self.tooltip = Some(UiTooltip {
            text: text.into(),
            delay,
        });
        self
    }
}

impl UiTree {
    /// Append a node, returning the tree for chaining. Validation happens in
    /// [`UiTree::validate`] / layout.
    pub fn with(mut self, node: UiNode) -> Self {
        self.nodes.push(node);
        self
    }
}
