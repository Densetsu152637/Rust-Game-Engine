//! Neutral accessibility tree of the game UI (`docs/accessibility.md`).
//!
//! The tree is plain data built from the laid-out UI: one [`AccessNode`] per
//! visible widget (plus synthetic nodes for tabs, dropdown options and dialog
//! buttons) with a role, name, description, value, states, window-space
//! bounds, supported actions and ordered children. It knows nothing about a
//! platform API; the desktop host converts it (`engine-desktop`, AccessKit).
//!
//! Node IDs are stable across ticks: [`AccessId`] is the root ID plus the node
//! ID (plus the virtual-list item or the synthetic part), hashed to a 64-bit
//! number only when a platform needs one. [`AccessTree::diff`] compares two
//! trees so a host only sends the nodes that changed.
use super::{
    chrome::{self, Popup},
    layout::Laid,
    *,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Pending assistive-technology action requests per tick.
pub const MAX_ACCESS_REQUESTS: usize = 64;
/// Bytes of a name composed from descendants.
pub const MAX_ACCESS_NAME: usize = 1024;
/// Bytes of an authored accessible description.
pub const MAX_ACCESS_DESCRIPTION: usize = 1024;
/// Outcomes retained for [`GameUi::take_access_outcomes`].
pub const MAX_ACCESS_OUTCOMES: usize = 64;

/// Semantic role, mapped from the widget kind (or `UiAccessibility::role`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRole {
    /// The application window; only the tree root has it.
    Window,
    Group,
    Label,
    Heading,
    Image,
    Button,
    Link,
    CheckBox,
    Switch,
    Slider,
    ProgressBar,
    TextInput,
    MultilineTextInput,
    PasswordInput,
    ComboBox,
    ListBox,
    ListBoxOption,
    List,
    ListItem,
    ScrollView,
    TabList,
    Tab,
    TabPanel,
    Dialog,
    Alert,
    Status,
    Region,
}

/// Live-region politeness: how eagerly changes are announced.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AccessLive {
    #[default]
    Off,
    Polite,
    Assertive,
}

/// What an assistive technology may ask a node to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessAction {
    Focus,
    /// Activate (press a button, toggle, select an item, open a dropdown).
    Click,
    Increment,
    Decrement,
    /// Set the numeric value of a slider or the text of a text input.
    SetValue,
    ScrollIntoView,
    Expand,
    Collapse,
}

/// Synthetic child of a widget node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessPart {
    /// Tab header `n` of a `Tabs` widget.
    Tab(u32),
    /// Footer button `n` of a `ModalDialog`.
    Button(u32),
    /// Option `n` of the open popup of a `Dropdown`.
    Option(u32),
    /// The option list of an open `Dropdown`.
    Popup,
}

/// Stable node identity: root ID, node ID, the virtual-list item the node
/// replicates for, and the synthetic part (tab, dialog button, option).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccessId {
    pub root: String,
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<AccessPart>,
}

impl AccessId {
    /// The window node (tree root).
    pub fn window() -> Self {
        Self {
            root: String::new(),
            node: String::new(),
            item: None,
            part: None,
        }
    }
    /// The group node of one `UiRoot`.
    pub fn root_group(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            node: String::new(),
            item: None,
            part: None,
        }
    }
    pub fn node(root: impl Into<String>, node: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            node: node.into(),
            item: None,
            part: None,
        }
    }
    pub fn with_part(mut self, part: AccessPart) -> Self {
        self.part = Some(part);
        self
    }
    pub fn with_item(mut self, item: u32) -> Self {
        self.item = Some(item);
        self
    }
    pub fn is_window(&self) -> bool {
        self.root.is_empty() && self.node.is_empty()
    }

    /// Deterministic 64-bit form (FNV-1a over the components) for platform
    /// APIs that need a number. The window is `1`; no other ID maps to `0` or
    /// `1`.
    pub fn numeric(&self) -> u64 {
        if self.is_window() {
            return 1;
        }
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        let mut feed = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(PRIME);
            }
        };
        feed(self.root.as_bytes());
        feed(&[0xff]);
        feed(self.node.as_bytes());
        feed(&[0xff]);
        match self.item {
            Some(item) => {
                feed(&[1]);
                feed(&item.to_le_bytes());
            }
            None => feed(&[0]),
        }
        match self.part {
            None => feed(&[0]),
            Some(AccessPart::Tab(n)) => {
                feed(&[1]);
                feed(&n.to_le_bytes());
            }
            Some(AccessPart::Button(n)) => {
                feed(&[2]);
                feed(&n.to_le_bytes());
            }
            Some(AccessPart::Option(n)) => {
                feed(&[3]);
                feed(&n.to_le_bytes());
            }
            Some(AccessPart::Popup) => feed(&[4]),
        }
        if hash <= 1 { hash + 2 } else { hash }
    }
}

/// Node value: text, or a number with its range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessValue {
    Text(String),
    Number {
        value: f64,
        min: f64,
        max: f64,
        /// Increment of a slider; `0` for read-only values (progress bars).
        step: f64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessStates {
    /// `Some` for checkable nodes (toggles).
    pub checked: Option<bool>,
    /// `Some` for expandable nodes (dropdowns).
    pub expanded: Option<bool>,
    /// `Some` for selectable nodes (list items, tabs, options).
    pub selected: Option<bool>,
    pub disabled: bool,
    pub focused: bool,
    pub focusable: bool,
    pub password: bool,
    pub read_only: bool,
    /// An open modal dialog.
    pub modal: bool,
}

/// Scroll position of a scroll view or virtual list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccessScroll {
    pub offset: [f32; 2],
    pub max: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessNode {
    pub id: AccessId,
    pub role: AccessRole,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Hint shown while an input is empty.
    pub placeholder: Option<String>,
    pub value: Option<AccessValue>,
    pub states: AccessStates,
    /// Border box in window logical pixels (after UI scale).
    pub bounds: Rect,
    /// Descendants outside `bounds` are clipped (scroll views, clipped nodes).
    pub clips_children: bool,
    pub actions: BTreeSet<AccessAction>,
    /// Reading order.
    pub children: Vec<AccessId>,
    pub live: AccessLive,
    /// 1-based position and set size of list items, tabs and options.
    pub position: Option<(u32, u32)>,
    pub scroll: Option<AccessScroll>,
}

impl AccessNode {
    fn new(id: AccessId, role: AccessRole) -> Self {
        Self {
            id,
            role,
            name: None,
            description: None,
            placeholder: None,
            value: None,
            states: AccessStates::default(),
            bounds: Rect::default(),
            clips_children: false,
            actions: BTreeSet::new(),
            children: vec![],
            live: AccessLive::Off,
            position: None,
            scroll: None,
        }
    }
}

/// The accessibility tree after one UI tick: nodes in pre-order (parents
/// before children, siblings in reading order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessTree {
    pub nodes: Vec<AccessNode>,
    /// The focused node; the window root when no UI node has focus.
    pub focus: AccessId,
    /// The open modal dialog that hides everything else, if any.
    pub modal: Option<AccessId>,
    /// IDs dropped because their 64-bit form collided with an earlier node
    /// (probability about 2^-64 per pair; counted, never silently merged).
    pub dropped: u32,
    #[serde(skip)]
    index: BTreeMap<AccessId, usize>,
    #[serde(skip)]
    numeric: BTreeMap<u64, usize>,
}

impl Default for AccessTree {
    /// A window with no content.
    fn default() -> Self {
        Self::assemble(
            vec![AccessNode::new(AccessId::window(), AccessRole::Window)],
            AccessId::window(),
            None,
            0,
        )
    }
}

/// Changes between two trees: only nodes whose data changed (or that are new)
/// are carried, plus the removed IDs and the focus.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessUpdate {
    pub root: AccessId,
    /// There was no previous tree: `nodes` is the whole tree.
    pub full: bool,
    pub nodes: Vec<AccessNode>,
    pub removed: Vec<AccessId>,
    pub focus: AccessId,
}

impl AccessTree {
    fn assemble(
        nodes: Vec<AccessNode>,
        focus: AccessId,
        modal: Option<AccessId>,
        dropped: u32,
    ) -> Self {
        let index = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
        let numeric = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.numeric(), i))
            .collect();
        Self {
            nodes,
            focus,
            modal,
            dropped,
            index,
            numeric,
        }
    }

    pub fn root(&self) -> &AccessNode {
        &self.nodes[0]
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn get(&self, id: &AccessId) -> Option<&AccessNode> {
        self.index.get(id).map(|i| &self.nodes[*i])
    }
    /// The node whose 64-bit ID is `numeric` (platform action requests).
    pub fn by_numeric(&self, numeric: u64) -> Option<&AccessNode> {
        self.numeric.get(&numeric).map(|i| &self.nodes[*i])
    }
    /// First node of `root` with node ID `node` and no item or part.
    pub fn find(&self, root: &str, node: &str) -> Option<&AccessNode> {
        self.get(&AccessId::node(root, node))
    }

    /// What changed since `previous` (`None` returns the whole tree).
    pub fn diff(&self, previous: Option<&AccessTree>) -> AccessUpdate {
        let Some(previous) = previous else {
            return AccessUpdate {
                root: self.nodes[0].id.clone(),
                full: true,
                nodes: self.nodes.clone(),
                removed: vec![],
                focus: self.focus.clone(),
            };
        };
        let nodes = self
            .nodes
            .iter()
            .filter(|n| previous.get(&n.id) != Some(*n))
            .cloned()
            .collect();
        let removed = previous
            .nodes
            .iter()
            .filter(|n| self.get(&n.id).is_none())
            .map(|n| n.id.clone())
            .collect();
        AccessUpdate {
            root: self.nodes[0].id.clone(),
            full: false,
            nodes,
            removed,
            focus: self.focus.clone(),
        }
    }
}

/// Authored accessibility overrides of a `UiNode` (`UiNode.accessibility`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiAccessibility {
    /// Accessible name; replaces the name derived from the widget text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Accessible description; the tooltip text is used when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Role replacing the one mapped from the widget kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<AccessRole>,
    /// Keep the node and its subtree out of the accessibility tree.
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// Announce changes of the node's name or value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<AccessLive>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl UiAccessibility {
    pub(super) fn text_bytes(&self) -> usize {
        self.label.as_ref().map_or(0, String::len)
            + self.description.as_ref().map_or(0, String::len)
    }

    pub(super) fn validate(&self, id: &str) -> Result<()> {
        let bad = |why: &str| Err(invalid(format!("{why} on UI node {id}")));
        let clean = |s: &str| !s.chars().any(|c| c.is_control() && c != '\n');
        if self
            .label
            .as_ref()
            .is_some_and(|s| s.len() > MAX_UI_LABEL || !clean(s))
        {
            return bad("Accessibility label too long or invalid");
        }
        if self
            .description
            .as_ref()
            .is_some_and(|s| s.len() > MAX_ACCESS_DESCRIPTION || !clean(s))
        {
            return bad("Accessibility description too long or invalid");
        }
        if self.role == Some(AccessRole::Window) {
            return bad("The window role cannot be assigned to a node");
        }
        Ok(())
    }
}

impl UiNode {
    /// Set the accessible name.
    pub fn with_access_label(mut self, label: impl Into<String>) -> Self {
        self.accessibility
            .get_or_insert_with(Default::default)
            .label = Some(label.into());
        self
    }
    /// Set the accessible description.
    pub fn with_access_description(mut self, description: impl Into<String>) -> Self {
        self.accessibility
            .get_or_insert_with(Default::default)
            .description = Some(description.into());
        self
    }
    /// Replace the role mapped from the widget kind.
    pub fn with_access_role(mut self, role: AccessRole) -> Self {
        self.accessibility.get_or_insert_with(Default::default).role = Some(role);
        self
    }
    /// Make changes of this node announced.
    pub fn with_access_live(mut self, live: AccessLive) -> Self {
        self.accessibility.get_or_insert_with(Default::default).live = Some(live);
        self
    }
    /// Keep the node and its subtree out of the accessibility tree.
    pub fn access_hidden(mut self) -> Self {
        self.accessibility
            .get_or_insert_with(Default::default)
            .hidden = true;
        self
    }
}

// ---------------------------------------------------------------------------
// Action requests
// ---------------------------------------------------------------------------

/// An action an assistive technology asks for. Applied on the next UI tick
/// through the same code as keyboard and pointer input, so the same widget
/// events fire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAccessAction {
    Focus,
    Click,
    Increment,
    Decrement,
    SetNumeric(f64),
    SetText(String),
    Expand,
    Collapse,
    ScrollIntoView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiAccessRequest {
    pub target: AccessId,
    pub action: UiAccessAction,
}

/// What became of a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAccessStatus {
    /// Applied (or already in the requested state).
    Applied,
    /// No such visible node.
    NotFound,
    /// The node exists but cannot take input now (disabled, or beneath a modal).
    Blocked,
    /// The widget does not support the action.
    Unsupported,
    /// The value was refused (out of range, too long, invalid characters).
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiAccessOutcome {
    pub request: UiAccessRequest,
    pub status: UiAccessStatus,
}

/// Accessibility state of a `GameUi`.
#[derive(Clone, Debug, Default)]
pub(super) struct AccessState {
    /// A platform bridge asked for the tree (an assistive technology connected).
    pub enabled: bool,
    /// The accessibility settings declare a screen reader.
    pub hint: bool,
    pub tree: Option<AccessTree>,
    pub requests: VecDeque<UiAccessRequest>,
    pub outcomes: Vec<UiAccessOutcome>,
}

// ---------------------------------------------------------------------------
// Tree construction
// ---------------------------------------------------------------------------

/// Everything the builder reads from one UI tick.
pub(super) struct AccessInput<'a> {
    pub roots: &'a [(String, UiRoot)],
    /// Laid-out visible roots in paint order: (index into `roots`, layout).
    pub frames: Vec<(usize, &'a Laid)>,
    pub focus: Option<&'a UiInteraction>,
    /// The open dropdown popup and its geometry.
    pub popup: Option<(&'a UiInteraction, Popup)>,
    /// The topmost modal dialog: (frame index, node index).
    pub modal: Option<(usize, usize)>,
    /// The (virtual) viewport the roots were laid out in.
    pub viewport: [u32; 2],
    /// UI scale: layout units to window logical pixels.
    pub scale: f32,
}

struct FrameView<'a> {
    id: &'a str,
    root: &'a UiRoot,
    laid: &'a Laid,
    /// (node, item) -> box index.
    lookup: BTreeMap<(usize, Option<u32>), usize>,
}

struct Builder<'a> {
    input: &'a AccessInput<'a>,
    nodes: BTreeMap<AccessId, AccessNode>,
    numeric: BTreeMap<u64, AccessId>,
    dropped: u32,
}

/// How the parent presents this child.
#[derive(Clone, Copy, Default)]
struct Slot {
    /// 1-based item position and set size of a list child.
    list_item: Option<(u32, u32)>,
    selected: bool,
    /// The selected panel of a `Tabs` widget, named by its tab.
    tab_panel: bool,
}

impl AccessTree {
    pub(super) fn build(input: &AccessInput<'_>) -> Self {
        let mut builder = Builder {
            input,
            nodes: BTreeMap::new(),
            numeric: BTreeMap::new(),
            dropped: 0,
        };
        let mut window = AccessNode::new(AccessId::window(), AccessRole::Window);
        window.states.focusable = true;
        let mut modal_id = None;
        let frames: Vec<usize> = match input.modal {
            Some((frame, _)) => vec![frame],
            None => (0..input.frames.len()).collect(),
        };
        for f in frames {
            let (root_index, laid) = input.frames[f];
            let (id, root) = &input.roots[root_index];
            let view = FrameView {
                id,
                root,
                laid,
                lookup: laid
                    .boxes
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(i, b)| ((b.node, b.item), i))
                    .collect(),
            };
            let mut group = AccessNode::new(AccessId::root_group(id.clone()), AccessRole::Group);
            group.states.disabled = !root.enabled;
            group.bounds = builder.scaled(viewport_rect(input.viewport));
            let mut children = vec![];
            if let Some((_, node)) = input.modal {
                if let Some(b) = view.lookup.get(&(node, None))
                    && let Some(child) = builder.node(&view, *b, Slot::default())
                {
                    modal_id = Some(child.clone());
                    children.push(child);
                }
            } else {
                for r in &laid.hierarchy.roots {
                    if matches!(root.tree.nodes[*r].widget, UiWidget::ModalDialog { .. }) {
                        continue;
                    }
                    if let Some(b) = view.lookup.get(&(*r, None))
                        && let Some(child) = builder.node(&view, *b, Slot::default())
                    {
                        children.push(child);
                    }
                }
                for (b, bounds) in laid.boxes.iter().enumerate() {
                    if bounds.overlay
                        && matches!(
                            root.tree.nodes[bounds.node].widget,
                            UiWidget::ModalDialog { .. }
                        )
                        && let Some(child) = builder.node(&view, b, Slot::default())
                    {
                        children.push(child);
                    }
                }
            }
            group.children = children;
            window.children.push(group.id.clone());
            builder.insert(group);
        }
        let window_id = window.id.clone();
        builder.insert(window);
        let focus = input
            .focus
            .map(|f| AccessId::node(f.root.clone(), f.node.clone()))
            .filter(|f| builder.nodes.contains_key(f))
            .or_else(|| modal_id.clone())
            .unwrap_or_else(AccessId::window);
        let dropped = builder.dropped;
        let mut nodes = builder.nodes;
        // The window's focus is the tree's focus ID, not a node state.
        if !focus.is_window()
            && let Some(node) = nodes.get_mut(&focus)
        {
            node.states.focused = true;
        }
        // Pre-order from the window.
        let mut ordered = Vec::with_capacity(nodes.len());
        let mut stack = vec![window_id];
        while let Some(id) = stack.pop() {
            if let Some(node) = nodes.remove(&id) {
                stack.extend(node.children.iter().rev().cloned());
                ordered.push(node);
            }
        }
        Self::assemble(ordered, focus, modal_id, dropped)
    }
}

impl Builder<'_> {
    fn scaled(&self, r: Rect) -> Rect {
        let s = self.input.scale;
        Rect {
            x: r.x * s,
            y: r.y * s,
            width: r.width * s,
            height: r.height * s,
        }
    }

    fn insert(&mut self, node: AccessNode) -> bool {
        let numeric = node.id.numeric();
        if self.numeric.get(&numeric).is_some_and(|o| *o != node.id)
            || self.nodes.contains_key(&node.id)
        {
            self.dropped += 1;
            return false;
        }
        self.numeric.insert(numeric, node.id.clone());
        self.nodes.insert(node.id.clone(), node);
        true
    }

    /// Build the subtree of box `b`; the ID of its node, or `None` when the
    /// subtree is hidden or its ID could not be registered.
    fn node(&mut self, view: &FrameView<'_>, b: usize, slot: Slot) -> Option<AccessId> {
        let bounds = &view.laid.boxes[b];
        let node = &view.root.tree.nodes[bounds.node];
        let authored = node.accessibility.as_ref();
        if authored.is_some_and(|a| a.hidden) {
            return None;
        }
        let id = AccessId {
            root: view.id.to_owned(),
            node: node.id.clone(),
            item: bounds.item,
            part: None,
        };
        let mut out = AccessNode::new(id.clone(), AccessRole::Group);
        self.describe(view, node, &mut out);
        out.bounds = self.scaled(bounds.rect);
        out.clips_children = node.style.clip
            || matches!(
                node.widget,
                UiWidget::ScrollView { .. } | UiWidget::VirtualList { .. }
            );
        if let Some(a) = authored {
            if let Some(label) = &a.label {
                out.name = Some(label.clone());
            }
            if let Some(description) = &a.description {
                out.description = Some(description.clone());
            }
            if let Some(live) = a.live {
                out.live = live;
            }
        }
        if out.name.is_none()
            && let Some(tip) = &node.tooltip
            && !tip.text.is_empty()
        {
            out.name = Some(tip.text.clone());
        } else if out.description.is_none()
            && let Some(tip) = &node.tooltip
            && !tip.text.is_empty()
        {
            out.description = Some(tip.text.clone());
        }
        let enabled = view.root.enabled && bounds.enabled;
        out.states.disabled = !enabled;
        if matches!(out.role, AccessRole::TextInput)
            || matches!(
                out.role,
                AccessRole::MultilineTextInput | AccessRole::PasswordInput
            )
        {
            out.states.read_only = !enabled;
        }
        out.states.focusable =
            enabled && node.widget.focusable() && node.nav.focusable != Some(false);
        if out.states.focusable {
            out.actions.insert(AccessAction::Focus);
        }
        if enabled {
            self.widget_actions(&node.widget, &mut out);
        }
        out.actions.insert(AccessAction::ScrollIntoView);
        out.scroll = match node.widget {
            UiWidget::ScrollView { .. } | UiWidget::VirtualList { .. } => Some(AccessScroll {
                offset: bounds.scroll,
                max: bounds.scroll_max,
            }),
            _ => None,
        };
        // Children.
        let children = self.child_boxes(view, bounds, node);
        let list_size = match &node.widget {
            UiWidget::List { .. } => view.laid.hierarchy.children[bounds.node].len() as u32,
            UiWidget::VirtualList { item_count, .. } => *item_count,
            _ => 0,
        };
        let selected_child = match &node.widget {
            UiWidget::List { selected, .. } | UiWidget::VirtualList { selected, .. } => *selected,
            _ => None,
        };
        let is_tabs = matches!(node.widget, UiWidget::Tabs { .. });
        let mut ids = vec![];
        if let UiWidget::Tabs {
            labels,
            selected,
            header_height,
            ..
        } = &node.widget
        {
            let cells = chrome::tab_cells(bounds, labels.len(), *header_height);
            for (i, label) in labels.iter().enumerate() {
                let mut tab = AccessNode::new(
                    id.clone().with_part(AccessPart::Tab(i as u32)),
                    AccessRole::Tab,
                );
                tab.name = Some(label.clone());
                tab.bounds = self.scaled(cells.get(i).copied().unwrap_or_default());
                tab.states.selected = Some(i as u32 == *selected);
                tab.states.disabled = !enabled;
                tab.position = Some((i as u32 + 1, labels.len() as u32));
                if enabled {
                    tab.actions.insert(AccessAction::Click);
                }
                let tab_id = tab.id.clone();
                if self.insert(tab) {
                    ids.push(tab_id);
                }
            }
        }
        for (position, child) in children.iter().enumerate() {
            let child_box = &view.laid.boxes[*child];
            let index = match &node.widget {
                UiWidget::VirtualList { .. } => child_box.item.unwrap_or(position as u32),
                // Position among all children, so hidden siblings keep their index.
                _ => view.laid.hierarchy.children[bounds.node]
                    .iter()
                    .position(|c| *c == child_box.node)
                    .unwrap_or(position) as u32,
            };
            let child_slot = Slot {
                list_item: (list_size > 0).then_some((index + 1, list_size)),
                selected: selected_child == Some(index),
                tab_panel: is_tabs,
            };
            if let Some(child_id) = self.node(view, *child, child_slot) {
                ids.push(child_id);
            }
        }
        if let UiWidget::ModalDialog {
            buttons,
            button_height,
            ..
        } = &node.widget
        {
            let cells = chrome::dialog_buttons(bounds, buttons.len(), *button_height);
            for (i, label) in buttons.iter().enumerate() {
                let mut button = AccessNode::new(
                    id.clone().with_part(AccessPart::Button(i as u32)),
                    AccessRole::Button,
                );
                button.name = Some(label.clone());
                button.bounds = self.scaled(cells.get(i).copied().unwrap_or_default());
                button.states.disabled = !enabled;
                button.position = Some((i as u32 + 1, buttons.len() as u32));
                if enabled {
                    button.actions.insert(AccessAction::Click);
                }
                let button_id = button.id.clone();
                if self.insert(button) {
                    ids.push(button_id);
                }
            }
        }
        if let UiWidget::Dropdown {
            options, selected, ..
        } = &node.widget
            && let Some((key, popup)) = &self.input.popup
            && key.root == view.id
            && key.node == node.id
        {
            let list_id = id.clone().with_part(AccessPart::Popup);
            let mut list = AccessNode::new(list_id.clone(), AccessRole::ListBox);
            list.bounds = self.scaled(popup.rect);
            list.name = out.name.clone();
            list.clips_children = true;
            for (i, option) in options.iter().enumerate() {
                let mut item = AccessNode::new(
                    id.clone().with_part(AccessPart::Option(i as u32)),
                    AccessRole::ListBoxOption,
                );
                item.name = Some(option.clone());
                item.bounds = self.scaled(popup.option_rect(i as u32).unwrap_or_default());
                item.states.selected = Some(*selected == Some(i as u32));
                item.position = Some((i as u32 + 1, options.len() as u32));
                item.actions.insert(AccessAction::Click);
                let item_id = item.id.clone();
                if self.insert(item) {
                    list.children.push(item_id);
                }
            }
            if self.insert(list) {
                ids.push(list_id);
            }
        }
        out.children = ids;
        // List items and tab panels are containers presented by their parent.
        if let Some((position, size)) = slot.list_item
            && matches!(
                out.role,
                AccessRole::Group | AccessRole::Label | AccessRole::Image
            )
            && authored.is_none_or(|a| a.role.is_none())
        {
            out.role = AccessRole::ListItem;
            out.position = Some((position, size));
            out.states.selected = Some(slot.selected);
            if enabled {
                out.actions.insert(AccessAction::Click);
            }
        }
        if slot.tab_panel
            && out.role == AccessRole::Group
            && authored.is_none_or(|a| a.role.is_none())
        {
            out.role = AccessRole::TabPanel;
            // Named by the selected tab.
            out.name = out.name.take().or_else(|| self.tab_name(view, bounds.node));
        }
        if matches!(
            out.role,
            AccessRole::ListItem | AccessRole::Button | AccessRole::Link
        ) && out.name.is_none()
        {
            out.name = self.content_name(&out.children);
        }
        let inserted = self.insert(out);
        inserted.then_some(id)
    }

    fn tab_name(&self, view: &FrameView<'_>, panel: usize) -> Option<String> {
        let parent = view.laid.hierarchy.parents[panel]?;
        match &view.root.tree.nodes[parent].widget {
            UiWidget::Tabs {
                labels, selected, ..
            } => labels.get(*selected as usize).cloned(),
            _ => None,
        }
    }

    /// Names of the text descendants joined with spaces, bounded.
    fn content_name(&self, children: &[AccessId]) -> Option<String> {
        let mut parts: Vec<&str> = vec![];
        let mut stack: Vec<&AccessId> = children.iter().rev().collect();
        let mut visited = 0;
        while let Some(id) = stack.pop() {
            visited += 1;
            if visited > 256 {
                break;
            }
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            if matches!(
                node.role,
                AccessRole::Label | AccessRole::Heading | AccessRole::Link | AccessRole::Button
            ) && let Some(name) = node.name.as_deref().filter(|n| !n.is_empty())
            {
                parts.push(name);
            } else {
                stack.extend(node.children.iter().rev());
            }
        }
        if parts.is_empty() {
            return None;
        }
        let mut name = parts.join(" ");
        if name.len() > MAX_ACCESS_NAME {
            let mut end = MAX_ACCESS_NAME;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
        }
        Some(name)
    }

    /// Boxes of the children of `bounds` that belong in the tree.
    fn child_boxes(&self, view: &FrameView<'_>, bounds: &UiBox, node: &UiNode) -> Vec<usize> {
        let children = &view.laid.hierarchy.children[bounds.node];
        match &node.widget {
            UiWidget::VirtualList { .. } => {
                let Some(template) = children.first() else {
                    return vec![];
                };
                view.laid
                    .boxes
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.node == *template && b.item.is_some())
                    .map(|(i, _)| i)
                    .collect()
            }
            _ => children
                .iter()
                .filter(|c| {
                    !matches!(
                        view.root.tree.nodes[**c].widget,
                        UiWidget::ModalDialog { .. }
                    )
                })
                .filter_map(|c| view.lookup.get(&(*c, bounds.item)).copied())
                .collect(),
        }
    }

    /// Role, name, value and states from the widget kind.
    fn describe(&self, view: &FrameView<'_>, node: &UiNode, out: &mut AccessNode) {
        let tip_name = |text: &str| (!text.is_empty()).then(|| text.to_owned());
        match &node.widget {
            UiWidget::Container => out.role = AccessRole::Group,
            UiWidget::Label { text } => {
                out.role = AccessRole::Label;
                out.name = tip_name(text);
            }
            UiWidget::RichLabel { markup } => {
                out.role = AccessRole::Label;
                out.name = tip_name(&strip_markup(markup));
            }
            UiWidget::Image { .. } | UiWidget::NineSlice { .. } => out.role = AccessRole::Image,
            UiWidget::Button { text, .. } => {
                out.role = AccessRole::Button;
                out.name = tip_name(text);
            }
            UiWidget::Toggle { text, checked, .. } => {
                out.role = AccessRole::CheckBox;
                out.name = tip_name(text);
                out.states.checked = Some(*checked);
            }
            UiWidget::Slider {
                value,
                min,
                max,
                step,
                ..
            } => {
                out.role = AccessRole::Slider;
                out.value = Some(AccessValue::Number {
                    value: f64::from(*value),
                    min: f64::from(*min),
                    max: f64::from(*max),
                    step: f64::from(*step),
                });
            }
            UiWidget::Progress { value, min, max } => {
                out.role = AccessRole::ProgressBar;
                out.value = Some(AccessValue::Number {
                    value: f64::from(*value),
                    min: f64::from(*min),
                    max: f64::from(*max),
                    step: 0.0,
                });
            }
            UiWidget::TextInput {
                text,
                placeholder,
                password,
                multiline,
                ..
            } => {
                out.role = if *password {
                    AccessRole::PasswordInput
                } else if *multiline {
                    AccessRole::MultilineTextInput
                } else {
                    AccessRole::TextInput
                };
                out.states.password = *password;
                // A password is never exposed.
                if !password {
                    out.value = Some(AccessValue::Text(text.clone()));
                }
                out.placeholder = tip_name(placeholder);
            }
            UiWidget::Dropdown {
                options,
                selected,
                placeholder,
                ..
            } => {
                out.role = AccessRole::ComboBox;
                let current = selected.and_then(|s| options.get(s as usize));
                out.value = current.map(|c| AccessValue::Text(c.clone()));
                out.placeholder = tip_name(placeholder);
                let open = self
                    .input
                    .popup
                    .as_ref()
                    .is_some_and(|(k, _)| k.node == node.id && k.root == view.id);
                out.states.expanded = Some(open);
            }
            UiWidget::ScrollView { .. } => out.role = AccessRole::ScrollView,
            UiWidget::List { .. } | UiWidget::VirtualList { .. } => out.role = AccessRole::List,
            UiWidget::Tabs { .. } => out.role = AccessRole::TabList,
            UiWidget::ModalDialog { title, .. } => {
                out.role = AccessRole::Dialog;
                out.name = tip_name(title);
                out.states.modal = true;
            }
        }
        if let Some(role) = node.accessibility.as_ref().and_then(|a| a.role) {
            out.role = role;
        }
    }

    fn widget_actions(&self, widget: &UiWidget, out: &mut AccessNode) {
        use AccessAction::*;
        match widget {
            UiWidget::Button { .. } | UiWidget::Toggle { .. } => {
                out.actions.insert(Click);
            }
            UiWidget::Slider { .. } => out.actions.extend([Increment, Decrement, SetValue]),
            UiWidget::TextInput { .. } => {
                out.actions.extend([SetValue, Click]);
            }
            UiWidget::Dropdown { .. } => {
                out.actions.insert(Click);
                out.actions.insert(if out.states.expanded == Some(true) {
                    Collapse
                } else {
                    Expand
                });
            }
            UiWidget::RichLabel { .. } if widget.interactive() => {
                out.actions.insert(Click);
            }
            _ => {}
        }
    }
}
