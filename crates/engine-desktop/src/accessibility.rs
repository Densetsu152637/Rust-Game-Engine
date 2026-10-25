//! Screen-reader bridge: the neutral accessibility tree of the game UI
//! (`engine_project::ui_tree::AccessTree`) converted to AccessKit and handed
//! to the platform through `accesskit_winit` (UI Automation on Windows,
//! NSAccessibility on macOS, AT-SPI on Linux).
//!
//! The conversion ([`access_node`], [`access_tree_update`],
//! [`access_action_request`]) is pure so it is tested without a window. The
//! [`AccessBridge`] owns the adapter: the platform tells it when an assistive
//! technology connects, the bridge then turns the tree on in the `GameUi`
//! (`GameUi::set_accessibility`), sends only the nodes that changed, and
//! forwards the technology's action requests back as `UiAccessRequest`s.
use crate::CameraView;
use accesskit::{
    Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, Affine,
    DeactivationHandler, Live, Node, NodeId, Rect, Role, Toggled, TreeId, TreeInfo, TreeUpdate,
};
use accesskit_winit::Adapter;
use bevy_ecs::world::World;
use engine_project::{
    accessibility::{AccessBridgeStatus, AccessibilityControl},
    ui_tree::{
        AccessAction, AccessId, AccessLive, AccessNode, AccessRole, AccessTree, AccessUpdate,
        AccessValue, GameUi, UiAccessAction, UiAccessRequest,
    },
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

/// Platform requests held between ticks; further ones are dropped.
const MAX_PENDING_REQUESTS: usize = 128;

/// The AccessKit role of a neutral role.
pub fn access_role(role: AccessRole) -> Role {
    match role {
        AccessRole::Window => Role::Window,
        AccessRole::Group => Role::Group,
        AccessRole::Label => Role::Label,
        AccessRole::Heading => Role::Heading,
        AccessRole::Image => Role::Image,
        AccessRole::Button => Role::Button,
        AccessRole::Link => Role::Link,
        AccessRole::CheckBox => Role::CheckBox,
        AccessRole::Switch => Role::Switch,
        AccessRole::Slider => Role::Slider,
        AccessRole::ProgressBar => Role::ProgressIndicator,
        AccessRole::TextInput => Role::TextInput,
        AccessRole::MultilineTextInput => Role::MultilineTextInput,
        AccessRole::PasswordInput => Role::PasswordInput,
        AccessRole::ComboBox => Role::ComboBox,
        AccessRole::ListBox => Role::ListBox,
        AccessRole::ListBoxOption => Role::ListBoxOption,
        AccessRole::List => Role::List,
        AccessRole::ListItem => Role::ListItem,
        AccessRole::ScrollView => Role::ScrollView,
        AccessRole::TabList => Role::TabList,
        AccessRole::Tab => Role::Tab,
        AccessRole::TabPanel => Role::TabPanel,
        AccessRole::Dialog => Role::Dialog,
        AccessRole::Alert => Role::Alert,
        AccessRole::Status => Role::Status,
        AccessRole::Region => Role::Region,
    }
}

/// The AccessKit ID of a neutral ID.
pub fn access_node_id(id: &AccessId) -> NodeId {
    NodeId(id.numeric())
}

fn access_action(action: AccessAction) -> Action {
    match action {
        AccessAction::Focus => Action::Focus,
        AccessAction::Click => Action::Click,
        AccessAction::Increment => Action::Increment,
        AccessAction::Decrement => Action::Decrement,
        AccessAction::SetValue => Action::SetValue,
        AccessAction::ScrollIntoView => Action::ScrollIntoView,
        AccessAction::Expand => Action::Expand,
        AccessAction::Collapse => Action::Collapse,
    }
}

/// One neutral node as an AccessKit node. `tree` supplies the set size that
/// AccessKit stores on the container of list items, tabs and options.
pub fn access_node(node: &AccessNode, tree: &AccessTree) -> Node {
    // An unnamed group is a layout container, which assistive technology skips.
    let role = match (node.role, &node.name) {
        (AccessRole::Group, None) => Role::GenericContainer,
        (role, _) => access_role(role),
    };
    let mut out = Node::new(role);
    if let Some(name) = &node.name {
        if node.role == AccessRole::Label {
            // Static text carries its text as the value.
            out.set_value(name.as_str());
        } else {
            out.set_label(name.as_str());
        }
    }
    if let Some(description) = &node.description {
        out.set_description(description.as_str());
    }
    if let Some(placeholder) = &node.placeholder {
        out.set_placeholder(placeholder.as_str());
    }
    match &node.value {
        Some(AccessValue::Text(text)) => out.set_value(text.as_str()),
        Some(AccessValue::Number {
            value,
            min,
            max,
            step,
        }) => {
            out.set_numeric_value(*value);
            out.set_min_numeric_value(*min);
            out.set_max_numeric_value(*max);
            if *step > 0.0 {
                out.set_numeric_value_step(*step);
            }
        }
        None => {}
    }
    if let Some(checked) = node.states.checked {
        out.set_toggled(Toggled::from(checked));
    }
    if let Some(expanded) = node.states.expanded {
        out.set_expanded(expanded);
    }
    if let Some(selected) = node.states.selected {
        out.set_selected(selected);
    }
    if node.states.disabled {
        out.set_disabled();
    }
    if node.states.read_only {
        out.set_read_only();
    }
    if node.states.modal {
        out.set_modal();
    }
    out.set_bounds(Rect {
        x0: f64::from(node.bounds.x),
        y0: f64::from(node.bounds.y),
        x1: f64::from(node.bounds.x + node.bounds.width),
        y1: f64::from(node.bounds.y + node.bounds.height),
    });
    if node.clips_children {
        out.set_clips_children();
    }
    for action in &node.actions {
        out.add_action(access_action(*action));
    }
    if node.live != AccessLive::Off {
        out.set_live(match node.live {
            AccessLive::Polite => Live::Polite,
            _ => Live::Assertive,
        });
    }
    if let Some((position, _)) = node.position {
        out.set_position_in_set(position.saturating_sub(1) as usize);
    }
    if let Some(size) = node
        .children
        .iter()
        .find_map(|c| tree.get(c).and_then(|c| c.position))
        .map(|(_, size)| size)
    {
        out.set_size_of_set(size as usize);
    }
    if let Some(scroll) = node.scroll {
        out.set_scroll_x(f64::from(scroll.offset[0]));
        out.set_scroll_x_min(0.0);
        out.set_scroll_x_max(f64::from(scroll.max[0]));
        out.set_scroll_y(f64::from(scroll.offset[1]));
        out.set_scroll_y_min(0.0);
        out.set_scroll_y_max(f64::from(scroll.max[1]));
    }
    out.set_children(node.children.iter().map(access_node_id).collect::<Vec<_>>());
    out
}

/// What the window root needs besides the UI tree: the window title and the
/// map from UI logical pixels to window pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessWindow {
    pub title: String,
    /// Affine coefficients `[a, b, c, d, e, f]` (see `accesskit::Affine`).
    pub transform: [f64; 6],
}

impl AccessWindow {
    /// The camera's fit of the logical UI into the window: uniform scale plus
    /// the letterbox offset.
    pub fn from_camera(title: impl Into<String>, camera: &CameraView) -> Self {
        Self {
            title: title.into(),
            transform: [
                camera.scale,
                0.0,
                0.0,
                camera.scale,
                f64::from(camera.viewport.x),
                f64::from(camera.viewport.y),
            ],
        }
    }
}

/// The AccessKit update for `update`: only the changed nodes (the window root
/// is always included when `refresh_root`, because its title or transform
/// changed), the focus, and the tree info on a full update.
pub fn access_tree_update(
    update: &AccessUpdate,
    tree: &AccessTree,
    window: &AccessWindow,
    refresh_root: bool,
) -> TreeUpdate {
    let mut nodes: Vec<(NodeId, Node)> = update
        .nodes
        .iter()
        .map(|n| (access_node_id(&n.id), access_node(n, tree)))
        .collect();
    if refresh_root && !update.nodes.iter().any(|n| n.id == update.root) {
        nodes.push((access_node_id(&update.root), access_node(tree.root(), tree)));
    }
    for (id, node) in &mut nodes {
        if *id == access_node_id(&update.root) {
            node.set_label(window.title.as_str());
            node.set_transform(Affine::new(window.transform));
        }
    }
    TreeUpdate {
        nodes,
        tree: update
            .full
            .then(|| TreeInfo::new(access_node_id(&update.root))),
        tree_id: TreeId::ROOT,
        focus: access_node_id(&update.focus),
    }
}

/// The UI request for a platform action request on a node of `tree`, or
/// `None` when the node is gone or the action is not one the UI supports.
pub fn access_action_request(
    request: &ActionRequest,
    tree: &AccessTree,
) -> Option<UiAccessRequest> {
    let node = tree.by_numeric(request.target_node.0)?;
    let action = match (request.action, &request.data) {
        (Action::Click, _) => UiAccessAction::Click,
        (Action::Focus, _) => UiAccessAction::Focus,
        (Action::Increment, _) => UiAccessAction::Increment,
        (Action::Decrement, _) => UiAccessAction::Decrement,
        (Action::Expand, _) => UiAccessAction::Expand,
        (Action::Collapse, _) => UiAccessAction::Collapse,
        (Action::ScrollIntoView, _) => UiAccessAction::ScrollIntoView,
        (Action::SetValue, Some(ActionData::Value(text))) => {
            UiAccessAction::SetText(text.to_string())
        }
        (Action::SetValue, Some(ActionData::NumericValue(value))) => {
            UiAccessAction::SetNumeric(*value)
        }
        _ => return None,
    };
    Some(UiAccessRequest {
        target: node.id.clone(),
        action,
    })
}

/// State shared with the platform's handler threads.
#[derive(Default)]
struct Shared {
    /// An assistive technology is connected.
    active: AtomicBool,
    requests: Mutex<Vec<ActionRequest>>,
}

struct Activation(Arc<Shared>);
struct Actions(Arc<Shared>);
struct Deactivation(Arc<Shared>);

impl ActivationHandler for Activation {
    /// Lazy start: nothing is built until a technology asks. The tree is sent
    /// from the next tick (`AccessBridge::sync`), which is the "no later than
    /// the next display refresh" the contract requires.
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0.active.store(true, Ordering::Release);
        None
    }
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        if let Ok(mut pending) = self.0.requests.lock()
            && pending.len() < MAX_PENDING_REQUESTS
        {
            pending.push(request);
        }
    }
}

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}

/// The adapter of one window and what was last sent through it.
pub(crate) struct AccessBridge {
    adapter: Adapter,
    shared: Arc<Shared>,
    /// The tree the platform currently has; `None` forces a full update.
    sent: Option<AccessTree>,
    window: Option<AccessWindow>,
    connected: bool,
}

impl AccessBridge {
    /// Create the adapter. The window must not have been shown yet.
    pub fn new(event_loop: &ActiveEventLoop, window: &Window) -> Self {
        let shared = Arc::new(Shared::default());
        let adapter = Adapter::with_direct_handlers(
            event_loop,
            window,
            Activation(shared.clone()),
            Actions(shared.clone()),
            Deactivation(shared.clone()),
        );
        Self {
            adapter,
            shared,
            sent: None,
            window: None,
            connected: false,
        }
    }

    /// Forward a window event; call before the application handles it.
    pub fn handle(&mut self, window: &Window, event: &WindowEvent) {
        self.adapter.process_event(window, event);
    }

    /// Run once per tick after the UI tick: follow the platform's connection
    /// state, forward its action requests into the UI, and send the changes.
    pub fn sync(&mut self, world: &mut World, camera: &CameraView, title: &str) {
        let active = self.shared.active.load(Ordering::Acquire);
        if active != self.connected {
            self.connected = active;
            self.sent = None;
            self.window = None;
        }
        let pending = std::mem::take(
            &mut *self
                .shared
                .requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        if let Some(mut control) = world.get_resource_mut::<AccessibilityControl>() {
            control.bridge = if active {
                AccessBridgeStatus::Connected
            } else {
                AccessBridgeStatus::Idle
            };
        }
        let Some(mut ui) = world.get_resource_mut::<GameUi>() else {
            return;
        };
        ui.set_accessibility(active);
        let Some(tree) = ui.accessibility_tree().cloned() else {
            return;
        };
        for request in &pending {
            if let Some(request) = access_action_request(request, &tree) {
                // A full queue drops the request; the technology asks again.
                let _ = ui.request_access(request);
            }
        }
        if !active {
            return;
        }
        let wanted = AccessWindow::from_camera(title, camera);
        let refresh_root = self.window.as_ref() != Some(&wanted);
        let update = tree.diff(self.sent.as_ref());
        if update.full || refresh_root || !update.nodes.is_empty() || !update.removed.is_empty() {
            self.adapter
                .update_if_active(|| access_tree_update(&update, &tree, &wanted, refresh_root));
        } else {
            // Focus is part of every update; send it when only it moved.
            let moved = self.sent.as_ref().is_none_or(|s| s.focus != tree.focus);
            if moved {
                self.adapter
                    .update_if_active(|| access_tree_update(&update, &tree, &wanted, false));
            }
        }
        self.window = Some(wanted);
        self.sent = Some(tree);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        ActionFrame,
        ui_tree::{GameUi, UiAccessAction, UiImageFit, UiLength, UiNode, UiRoot, UiTree, UiWidget},
    };

    fn at(node: UiNode, x: f32, y: f32, w: f32, h: f32) -> UiNode {
        node.with_size(UiLength::Pixels(w), UiLength::Pixels(h))
            .with_style(|s| {
                s.offset = [x, y];
                s.padding = 0.0;
            })
    }

    fn root() -> UiRoot {
        let mut dialog = at(
            UiNode::dialog("confirm", "Quit?", vec!["Yes".into()]),
            0.0,
            0.0,
            200.0,
            100.0,
        );
        if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
            *open = false;
        }
        UiRoot {
            tree: UiTree {
                nodes: vec![
                    at(UiNode::label("title", "Settings"), 0.0, 0.0, 100.0, 20.0),
                    at(UiNode::button("play", "Play", "go"), 10.0, 30.0, 80.0, 20.0),
                    at(UiNode::toggle("mute", "Mute", true), 10.0, 60.0, 80.0, 20.0),
                    at(
                        UiNode::slider("volume", 0.5, 0.0, 1.0, 0.25),
                        10.0,
                        90.0,
                        100.0,
                        20.0,
                    )
                    .with_access_label("Volume"),
                    at(
                        UiNode::image("logo", "tex", UiImageFit::Stretch),
                        10.0,
                        120.0,
                        20.0,
                        20.0,
                    ),
                    dialog,
                ],
            },
            ..Default::default()
        }
    }

    fn tick(ui: &mut GameUi, roots: &mut [(String, UiRoot)], input: &ActionFrame) {
        ui.tick(roots, [640, 360], input).unwrap();
    }

    fn built() -> (GameUi, [(String, UiRoot); 1]) {
        let mut roots = [("hud".to_string(), root())];
        let mut ui = GameUi::default();
        ui.set_accessibility(true);
        tick(&mut ui, &mut roots, &ActionFrame::default());
        (ui, roots)
    }

    fn window() -> AccessWindow {
        AccessWindow {
            title: "Game".into(),
            transform: [2.0, 0.0, 0.0, 2.0, 10.0, 20.0],
        }
    }

    #[test]
    fn every_neutral_role_maps_to_an_accesskit_role() {
        let roles = [
            (AccessRole::Window, Role::Window),
            (AccessRole::Group, Role::Group),
            (AccessRole::Label, Role::Label),
            (AccessRole::Heading, Role::Heading),
            (AccessRole::Image, Role::Image),
            (AccessRole::Button, Role::Button),
            (AccessRole::Link, Role::Link),
            (AccessRole::CheckBox, Role::CheckBox),
            (AccessRole::Switch, Role::Switch),
            (AccessRole::Slider, Role::Slider),
            (AccessRole::ProgressBar, Role::ProgressIndicator),
            (AccessRole::TextInput, Role::TextInput),
            (AccessRole::MultilineTextInput, Role::MultilineTextInput),
            (AccessRole::PasswordInput, Role::PasswordInput),
            (AccessRole::ComboBox, Role::ComboBox),
            (AccessRole::ListBox, Role::ListBox),
            (AccessRole::ListBoxOption, Role::ListBoxOption),
            (AccessRole::List, Role::List),
            (AccessRole::ListItem, Role::ListItem),
            (AccessRole::ScrollView, Role::ScrollView),
            (AccessRole::TabList, Role::TabList),
            (AccessRole::Tab, Role::Tab),
            (AccessRole::TabPanel, Role::TabPanel),
            (AccessRole::Dialog, Role::Dialog),
            (AccessRole::Alert, Role::Alert),
            (AccessRole::Status, Role::Status),
            (AccessRole::Region, Role::Region),
        ];
        for (neutral, expected) in roles {
            assert_eq!(access_role(neutral), expected, "{neutral:?}");
        }
    }

    #[test]
    fn a_full_update_has_one_node_per_neutral_node_with_roles_and_focus() {
        let (ui, _roots) = built();
        let tree = ui.accessibility_tree().unwrap();
        let update = tree.diff(None);
        let ak = access_tree_update(&update, tree, &window(), false);
        // window, the root group, title, play, mute, volume, logo (the closed dialog is absent)
        assert_eq!(ak.nodes.len(), 7);
        assert_eq!(ak.nodes.len(), tree.len());
        assert_eq!(
            ak.tree.as_ref().unwrap().root,
            access_node_id(&AccessId::window())
        );
        assert_eq!(ak.tree_id, TreeId::ROOT);
        // No UI node has focus: the focus is the window.
        assert_eq!(ak.focus, access_node_id(&AccessId::window()));

        let by_id = |node: &str| {
            let id = access_node_id(&AccessId::node("hud", node));
            &ak.nodes.iter().find(|(i, _)| *i == id).unwrap().1
        };
        assert_eq!(by_id("title").role(), Role::Label);
        assert_eq!(by_id("title").value(), Some("Settings"));
        let play = by_id("play");
        assert_eq!((play.role(), play.label()), (Role::Button, Some("Play")));
        assert!(play.supports_action(Action::Click) && play.supports_action(Action::Focus));
        let mute = by_id("mute");
        assert_eq!(
            (mute.role(), mute.toggled()),
            (Role::CheckBox, Some(Toggled::True))
        );
        let volume = by_id("volume");
        assert_eq!(
            (volume.role(), volume.label()),
            (Role::Slider, Some("Volume"))
        );
        assert_eq!(volume.numeric_value(), Some(0.5));
        assert_eq!(
            (
                volume.min_numeric_value(),
                volume.max_numeric_value(),
                volume.numeric_value_step()
            ),
            (Some(0.0), Some(1.0), Some(0.25))
        );
        assert!(
            volume.supports_action(Action::Increment) && volume.supports_action(Action::SetValue)
        );
        let bounds = volume.bounds().unwrap();
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            (10.0, 90.0, 110.0, 110.0)
        );
        assert_eq!(by_id("logo").role(), Role::Image);

        // The window carries the title and the logical-to-window transform.
        let (_, root) = ak
            .nodes
            .iter()
            .find(|(i, _)| *i == access_node_id(&AccessId::window()))
            .unwrap();
        assert_eq!((root.role(), root.label()), (Role::Window, Some("Game")));
        assert_eq!(
            root.transform(),
            Some(&Affine::new([2.0, 0.0, 0.0, 2.0, 10.0, 20.0]))
        );
        assert_eq!(root.children().len(), 1);
        // The unnamed group of the UI root is a plain container.
        let group = ak
            .nodes
            .iter()
            .find(|(i, _)| *i == access_node_id(&AccessId::root_group("hud")))
            .unwrap();
        assert_eq!(group.1.role(), Role::GenericContainer);
        assert_eq!(group.1.children().len(), 5);
    }

    #[test]
    fn focus_follows_the_ui_focus() {
        let (mut ui, mut roots) = built();
        let press = ActionFrame {
            pressed: ["ui_next".into()].into(),
            ..Default::default()
        };
        tick(&mut ui, &mut roots, &press);
        let tree = ui.accessibility_tree().unwrap();
        let ak = access_tree_update(&tree.diff(None), tree, &window(), false);
        assert_eq!(ak.focus, access_node_id(&AccessId::node("hud", "play")));
    }

    #[test]
    fn an_incremental_update_carries_only_the_changed_node() {
        let (mut ui, mut roots) = built();
        let first = ui.accessibility_tree().unwrap().clone();
        ui.request_access(UiAccessRequest {
            target: AccessId::node("hud", "volume"),
            action: UiAccessAction::Increment,
        })
        .unwrap();
        tick(&mut ui, &mut roots, &ActionFrame::default());
        let tree = ui.accessibility_tree().unwrap();
        let update = tree.diff(Some(&first));
        let ak = access_tree_update(&update, tree, &window(), false);
        assert_eq!(ak.nodes.len(), 1, "only the slider changed");
        assert_eq!(
            ak.nodes[0].0,
            access_node_id(&AccessId::node("hud", "volume"))
        );
        assert_eq!(ak.nodes[0].1.numeric_value(), Some(0.75));
        assert!(
            ak.tree.is_none(),
            "tree info is sent once, with the full update"
        );
        assert_eq!(ak.focus, access_node_id(&AccessId::node("hud", "volume")));
        // A changed window title or transform refreshes the root as well.
        let with_root = access_tree_update(&update, tree, &window(), true);
        assert_eq!(with_root.nodes.len(), 2);
    }

    #[test]
    fn platform_actions_become_ui_requests() {
        let (ui, _roots) = built();
        let tree = ui.accessibility_tree().unwrap();
        let target = |node: &str| access_node_id(&AccessId::node("hud", node));
        let request = |action, node: &str, data| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: target(node),
            data,
        };
        let convert = |r: ActionRequest| access_action_request(&r, tree);
        assert_eq!(
            convert(request(Action::Click, "play", None)),
            Some(UiAccessRequest {
                target: AccessId::node("hud", "play"),
                action: UiAccessAction::Click
            })
        );
        assert_eq!(
            convert(request(Action::Increment, "volume", None)).map(|r| r.action),
            Some(UiAccessAction::Increment)
        );
        assert_eq!(
            convert(request(
                Action::SetValue,
                "volume",
                Some(ActionData::NumericValue(0.25))
            ))
            .map(|r| r.action),
            Some(UiAccessAction::SetNumeric(0.25))
        );
        assert_eq!(
            convert(request(
                Action::SetValue,
                "title",
                Some(ActionData::Value("hi".into()))
            ))
            .map(|r| r.action),
            Some(UiAccessAction::SetText("hi".into()))
        );
        // Unsupported actions, unknown nodes and a SetValue without data are dropped.
        assert_eq!(
            convert(request(Action::ShowContextMenu, "play", None)),
            None
        );
        assert_eq!(convert(request(Action::SetValue, "volume", None)), None);
        assert_eq!(convert(request(Action::Click, "missing", None)), None);
    }

    #[test]
    fn requests_round_trip_into_widget_events() {
        let (mut ui, mut roots) = built();
        let tree = ui.accessibility_tree().unwrap().clone();
        let click = ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: access_node_id(&AccessId::node("hud", "play")),
            data: None,
        };
        ui.request_access(access_action_request(&click, &tree).unwrap())
            .unwrap();
        let events = ui
            .tick(&mut roots, [640, 360], &ActionFrame::default())
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "ui.clicked");
        assert_eq!(events[0].action, "go");
    }

    #[test]
    fn list_items_carry_their_position_and_the_container_the_set_size() {
        let nodes = vec![
            at(UiNode::list("list"), 0.0, 0.0, 100.0, 80.0),
            {
                let mut n = UiNode::label("a", "Alpha");
                n.parent = Some("list".into());
                n
            },
            {
                let mut n = UiNode::label("b", "Beta");
                n.parent = Some("list".into());
                n
            },
        ];
        let mut roots = [(
            "hud".to_string(),
            UiRoot {
                tree: UiTree { nodes },
                ..Default::default()
            },
        )];
        let mut ui = GameUi::default();
        ui.set_accessibility(true);
        tick(&mut ui, &mut roots, &ActionFrame::default());
        let tree = ui.accessibility_tree().unwrap();
        let list = access_node(tree.find("hud", "list").unwrap(), tree);
        assert_eq!((list.role(), list.size_of_set()), (Role::List, Some(2)));
        let beta = access_node(tree.find("hud", "b").unwrap(), tree);
        assert_eq!(
            (beta.role(), beta.position_in_set()),
            (Role::ListItem, Some(1))
        );
    }
}
