//! `GameUi`: focus, pointer capture, hover/tooltips, popups, modal stacking,
//! keyboard/gamepad navigation and text editing over retained roots.
use super::{
    accessibility::{AccessInput, AccessState},
    chrome::{self, Popup},
    layout::Laid,
    navigation::{self, UiDirection},
    text_edit::{Edit, MemoryClipboard, UiClipboard, UiEditKey, UiPreedit, UiTextEvent},
    *,
};
use crate::input::ActionFrame;
use bevy_ecs::prelude::Resource;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

/// Wheel notch distance in logical pixels.
const WHEEL_STEP: f32 = 40.0;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UiInteraction {
    pub root: String,
    pub node: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UiWidgetEvent {
    pub root: String,
    pub node: String,
    pub name: String,
    pub action: String,
    pub value: Value,
}

/// Per-node interaction flags, recomputed every tick, for styling and
/// accessibility layers. Nodes with no flag set are not stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UiStateFlags(pub u8);

impl UiStateFlags {
    pub const HOVERED: Self = Self(1);
    pub const PRESSED: Self = Self(2);
    pub const FOCUSED: Self = Self(4);
    pub const DISABLED: Self = Self(8);
    pub const CHECKED: Self = Self(16);
    /// Dropdown popup or modal dialog open.
    pub const OPEN: Self = Self(32);
    /// Selected list item, dropdown option row or tab panel owner.
    pub const SELECTED: Self = Self(64);

    pub fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
    pub fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn hovered(self) -> bool {
        self.contains(Self::HOVERED)
    }
    pub fn pressed(self) -> bool {
        self.contains(Self::PRESSED)
    }
    pub fn focused(self) -> bool {
        self.contains(Self::FOCUSED)
    }
    pub fn disabled(self) -> bool {
        self.contains(Self::DISABLED)
    }
    pub fn checked(self) -> bool {
        self.contains(Self::CHECKED)
    }
    pub fn open(self) -> bool {
        self.contains(Self::OPEN)
    }
}

/// Per-tick inputs other than actions.
#[derive(Clone, Copy)]
pub struct UiTickContext<'a> {
    pub viewport: [u32; 2],
    /// Seconds since the previous tick (tooltip delays).
    pub dt: f32,
    pub text: &'a dyn UiTextBackend,
}

#[derive(Clone, Debug, PartialEq)]
enum Capture {
    /// Pointer press on a widget, held.
    Press(UiInteraction),
    Slider(UiInteraction),
    /// Content drag on a scroll view.
    Drag {
        key: UiInteraction,
        pointer: [f32; 2],
        offset: [f32; 2],
    },
    /// Scrollbar thumb drag on one axis.
    Thumb {
        key: UiInteraction,
        axis: usize,
        pointer: [f32; 2],
        offset: [f32; 2],
    },
    /// Text selection drag.
    Select(UiInteraction),
}

impl Capture {
    fn key(&self) -> &UiInteraction {
        match self {
            Self::Press(k) | Self::Slider(k) | Self::Select(k) => k,
            Self::Drag { key, .. } | Self::Thumb { key, .. } => key,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PopupState {
    pub key: UiInteraction,
    pub highlight: u32,
    pub first: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TooltipState {
    pub key: UiInteraction,
    pub text: String,
    pub at: [f32; 2],
}

#[derive(Clone)]
struct Clipboard(Arc<dyn UiClipboard>);

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UiClipboard")
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self(Arc::new(MemoryClipboard::default()))
    }
}

/// Retains interaction state; widget values live in `UiRoot`.
#[derive(Clone, Debug, Default, Resource)]
pub struct GameUi {
    pub(crate) focus: Option<UiInteraction>,
    captured: Option<Capture>,
    hovered: Option<UiInteraction>,
    hover_owner: Option<UiInteraction>,
    hover_time: f32,
    pub(super) tooltip: Option<TooltipState>,
    pub(super) popup: Option<PopupState>,
    /// Highlighted button of the active modal dialog.
    pub(super) dialog_choice: Option<(UiInteraction, u32)>,
    queue: VecDeque<UiTextEvent>,
    pub(super) preedit: Option<(UiInteraction, UiPreedit)>,
    editing: Option<UiInteraction>,
    ime_area: Option<Rect>,
    clipboard: Clipboard,
    states: BTreeMap<UiInteraction, UiStateFlags>,
    /// Keyboard navigation moved focus this tick: scroll it into view.
    reveal_focus: bool,
    /// UI scale of the last tick (layout units to window pixels); 0 means 1.
    ui_scale: f32,
    /// Accessibility tree, assistive-technology requests and their outcomes.
    access: AccessState,
    /// Per-item texts of virtual-list templates per root, set before each
    /// tick so the accessibility tree names the rows as they are drawn.
    item_texts: BTreeMap<String, UiItemTexts>,
}

struct Target {
    frame: usize,
    root: usize,
    node: usize,
    key: UiInteraction,
    bounds: UiBox,
    focusable: bool,
    tab_index: i32,
}

struct RootFrame {
    root: usize,
    laid: Laid,
}

struct Frame {
    roots: Vec<RootFrame>,
    targets: Vec<Target>,
    /// Global paint order of every box: (frame, box).
    paint: Vec<(usize, usize)>,
    /// Target index of the topmost open dialog.
    modal: Option<usize>,
    viewport: [u32; 2],
}

impl Frame {
    fn build(roots: &[(String, UiRoot)], ctx: &UiTickContext<'_>) -> Result<Self> {
        if roots.len() > MAX_UI_ROOTS {
            return Err(invalid("UI exceeds 64 roots"));
        }
        let mut order: Vec<_> = (0..roots.len()).collect();
        order.sort_by(|a, b| {
            (roots[*a].1.layer, &roots[*a].0).cmp(&(roots[*b].1.layer, &roots[*b].0))
        });
        let mut ids = std::collections::BTreeSet::new();
        let mut frames = vec![];
        for index in order {
            let (id, root) = &roots[index];
            if !ids.insert(id) {
                return Err(invalid("Duplicate UI root ID"));
            }
            root.validate()?;
            if root.visible {
                frames.push(RootFrame {
                    root: index,
                    laid: root.tree.laid(ctx.viewport, ctx.text)?,
                });
            }
        }
        let mut paint = vec![];
        for overlay in [false, true] {
            for (f, frame) in frames.iter().enumerate() {
                for (b, bounds) in frame.laid.boxes.iter().enumerate() {
                    if bounds.overlay == overlay {
                        paint.push((f, b));
                    }
                }
            }
        }
        let mut targets = vec![];
        for (f, b) in &paint {
            let frame = &frames[*f];
            let (id, root) = &roots[frame.root];
            let bounds = &frame.laid.boxes[*b];
            let node = &root.tree.nodes[bounds.node];
            let visible = bounds.visible();
            // A focusable widget scrolled out of a scroll view stays a target,
            // so keyboard navigation can reach it (focus then scrolls it into
            // view). It has no visible area, so pointer hits cannot land on it.
            let scrolled_out = node.widget.focusable()
                && node.nav.focusable != Some(false)
                && bounds.rect.width > 0.0
                && bounds.rect.height > 0.0
                && bounds.clip.width > 0.0
                && bounds.clip.height > 0.0
                && frame.laid.hierarchy.ancestors(bounds.node).any(|a| {
                    matches!(
                        root.tree.nodes[a].widget,
                        UiWidget::ScrollView { .. } | UiWidget::VirtualList { .. }
                    )
                });
            if root.enabled
                && bounds.enabled
                && bounds.item.is_none()
                && node.widget.interactive()
                && ((visible.width > 0.0 && visible.height > 0.0) || scrolled_out)
            {
                targets.push(Target {
                    frame: *f,
                    root: frame.root,
                    node: bounds.node,
                    key: UiInteraction {
                        root: id.clone(),
                        node: node.id.clone(),
                    },
                    bounds: bounds.clone(),
                    focusable: node.widget.focusable() && node.nav.focusable != Some(false),
                    tab_index: node.nav.tab_index,
                });
            }
        }
        let modal = targets.iter().rposition(|t| {
            t.bounds.overlay
                && matches!(
                    roots[t.root].1.tree.nodes[t.node].widget,
                    UiWidget::ModalDialog { .. }
                )
        });
        Ok(Self {
            roots: frames,
            targets,
            paint,
            modal,
            viewport: ctx.viewport,
        })
    }

    /// Whether target `t` may receive input while the modal (if any) is open.
    fn allowed(&self, t: usize) -> bool {
        let Some(m) = self.modal else {
            return true;
        };
        let (target, modal) = (&self.targets[t], &self.targets[m]);
        target.frame == modal.frame
            && self.roots[modal.frame]
                .laid
                .hierarchy
                .contains(modal.node, target.node)
    }

    fn box_allowed(&self, frame: usize, node: usize) -> bool {
        let Some(m) = self.modal else {
            return true;
        };
        let modal = &self.targets[m];
        frame == modal.frame && self.roots[frame].laid.hierarchy.contains(modal.node, node)
    }

    fn find(&self, key: &UiInteraction) -> Option<usize> {
        self.targets.iter().position(|t| &t.key == key)
    }

    fn hit(&self, point: [f32; 2]) -> Option<usize> {
        (0..self.targets.len())
            .rev()
            .find(|t| self.allowed(*t) && self.targets[*t].bounds.hit(point))
    }
}

fn node<'a>(roots: &'a [(String, UiRoot)], target: &Target) -> &'a UiNode {
    &roots[target.root].1.tree.nodes[target.node]
}

fn node_mut<'a>(roots: &'a mut [(String, UiRoot)], target: &Target) -> &'a mut UiNode {
    &mut roots[target.root].1.tree.nodes[target.node]
}

fn event(target: &Target, node: &UiNode, name: &str, value: Value) -> UiWidgetEvent {
    UiWidgetEvent {
        root: target.key.root.clone(),
        node: node.id.clone(),
        name: name.into(),
        action: node.widget.action().to_owned(),
        value,
    }
}

impl GameUi {
    pub fn focus(&self) -> Option<&UiInteraction> {
        self.focus.as_ref()
    }

    /// Flags of a node after the last tick.
    pub fn state(&self, root: &str, node: &str) -> UiStateFlags {
        self.states
            .get(&UiInteraction {
                root: root.into(),
                node: node.into(),
            })
            .copied()
            .unwrap_or_default()
    }

    /// Every node with at least one flag set after the last tick.
    pub fn states(&self) -> &BTreeMap<UiInteraction, UiStateFlags> {
        &self.states
    }

    pub fn hovered(&self) -> Option<&UiInteraction> {
        self.hovered.as_ref()
    }

    /// Dropdown whose popup is open.
    pub fn open_popup(&self) -> Option<&UiInteraction> {
        self.popup.as_ref().map(|p| &p.key)
    }

    /// Visible tooltip: owner, text and anchor point.
    pub fn tooltip(&self) -> Option<(&UiInteraction, &str, [f32; 2])> {
        let scale = self.scale();
        self.tooltip
            .as_ref()
            .map(|t| (&t.key, t.text.as_str(), [t.at[0] * scale, t.at[1] * scale]))
    }

    /// Focused text input, when the platform should enable IME/text entry.
    pub fn text_input_focus(&self) -> Option<&UiInteraction> {
        self.editing.as_ref()
    }

    /// Absolute caret rectangle of the focused text input (IME candidate placement).
    pub fn ime_area(&self) -> Option<Rect> {
        let scale = self.scale();
        self.ime_area.map(|r| Rect {
            x: r.x * scale,
            y: r.y * scale,
            width: r.width * scale,
            height: r.height * scale,
        })
    }

    /// UI scale applied to the last tick (1 without accessibility settings).
    pub fn scale(&self) -> f32 {
        if self.ui_scale > 0.0 {
            self.ui_scale
        } else {
            1.0
        }
    }

    pub(super) fn set_scale(&mut self, scale: f32) {
        self.ui_scale = scale;
    }

    /// Per-item texts of virtual-list templates per root for the next tick.
    pub(super) fn set_item_texts(&mut self, texts: BTreeMap<String, UiItemTexts>) {
        self.item_texts = texts;
    }

    pub fn preedit(&self) -> Option<&UiPreedit> {
        self.preedit.as_ref().map(|(_, p)| p)
    }

    /// Replace the clipboard used by copy, cut and paste.
    pub fn set_clipboard(&mut self, clipboard: Arc<dyn UiClipboard>) {
        self.clipboard = Clipboard(clipboard);
    }

    /// Queue a text-input event for the next tick. Events reach the focused
    /// `TextInput`; they are discarded when none is focused.
    pub fn push_text_event(&mut self, event: UiTextEvent) -> Result<()> {
        event.validate()?;
        if self.queue.len() >= text_edit::MAX_TEXT_EVENTS {
            return Err(Error::new(
                "ui.text_event.overflow",
                "More than 256 text events queued in one tick",
            ));
        }
        self.queue.push_back(event);
        Ok(())
    }

    pub fn clear(&mut self) {
        let clipboard = self.clipboard.clone();
        let access = AccessState {
            enabled: self.access.enabled,
            hint: self.access.hint,
            ..Default::default()
        };
        *self = Self {
            clipboard,
            access,
            ..Default::default()
        };
    }

    /// Pause interaction for a modal panel or lost window focus. Focus is
    /// retained; pointer capture, popups, tooltips and IME composition are
    /// cancelled, so a later release cannot activate a widget.
    pub fn suspend(&mut self) {
        self.captured = None;
        self.popup = None;
        self.tooltip = None;
        self.hover_owner = None;
        self.hover_time = 0.0;
        self.hovered = None;
        self.preedit = None;
        self.queue.clear();
    }

    /// Headless tick without fonts or elapsed time (text measures as empty,
    /// tooltips never become visible).
    pub fn tick(
        &mut self,
        roots: &mut [(String, UiRoot)],
        viewport: [u32; 2],
        input: &ActionFrame,
    ) -> Result<Vec<UiWidgetEvent>> {
        self.tick_with(
            roots,
            input,
            &UiTickContext {
                viewport,
                dt: 0.0,
                text: &NullTextBackend,
            },
        )
    }

    /// Root IDs must be unique. Roots are painted/hit-tested by (layer, root ID);
    /// open dialogs paint and hit-test above all roots, dropdown popups above
    /// those. Actions: ui_click (pointer press/hold), ui_confirm, ui_cancel,
    /// ui_next/ui_previous (tab order), ui_up/ui_down/ui_left/ui_right
    /// (spatial navigation unless the focused widget consumes the direction).
    pub fn tick_with(
        &mut self,
        roots: &mut [(String, UiRoot)],
        input: &ActionFrame,
        ctx: &UiTickContext<'_>,
    ) -> Result<Vec<UiWidgetEvent>> {
        let frame = Frame::build(roots, ctx)?;
        self.retain(&frame, roots);
        let mut events = vec![];
        self.apply_access(roots, &frame, &mut events);
        if !input.focused {
            self.suspend();
        } else {
            self.step(roots, &frame, input, ctx, &mut events);
        }
        self.queue.clear();
        if std::mem::take(&mut self.reveal_focus)
            && let Some(focus) = self.focus.clone()
        {
            self.scroll_into_view(roots, &frame, &focus.root, &focus.node, None, &mut events);
        }
        self.refresh(roots, &frame, input, ctx);
        self.build_access(roots, &frame);
        Ok(events)
    }

    /// Drop state referring to targets that no longer exist.
    fn retain(&mut self, frame: &Frame, roots: &[(String, UiRoot)]) {
        let live = |key: &UiInteraction| frame.find(key).is_some();
        if self.focus.as_ref().is_some_and(|k| !live(k)) {
            self.focus = None;
        }
        if self.captured.as_ref().is_some_and(|c| !live(c.key())) {
            self.captured = None;
        }
        if self.popup.as_ref().is_some_and(|p| {
            frame.find(&p.key).is_none_or(|t| {
                !matches!(
                    node(roots, &frame.targets[t]).widget,
                    UiWidget::Dropdown { .. }
                )
            })
        }) {
            self.popup = None;
        }
        if self
            .preedit
            .as_ref()
            .is_some_and(|(k, _)| Some(k) != self.focus.as_ref())
        {
            self.preedit = None;
        }
    }

    fn step(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        input: &ActionFrame,
        ctx: &UiTickContext<'_>,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        let pointer = input.pointer;
        let pointer_ok = pointer.iter().all(|v| v.is_finite());
        // Focus trap: an open modal owns focus.
        if let Some(m) = frame.modal {
            let key = &frame.targets[m].key;
            if self
                .focus
                .as_ref()
                .is_none_or(|f| frame.find(f).is_none_or(|t| !frame.allowed(t)))
            {
                self.focus = Some(key.clone());
            }
            if self.dialog_choice.as_ref().is_none_or(|(k, _)| k != key) {
                self.dialog_choice = Some((key.clone(), 0));
            }
            if self
                .popup
                .as_ref()
                .is_some_and(|p| frame.find(&p.key).is_none_or(|t| !frame.allowed(t)))
            {
                self.popup = None;
            }
        } else {
            self.dialog_choice = None;
        }
        self.hover(roots, frame, pointer, pointer_ok, input, ctx.dt);
        if self.popup.is_some() {
            self.popup_input(roots, frame, input, events);
            return;
        }
        let navigated = self.navigate(roots, frame, input);
        if input.pressed("ui_click") && pointer_ok {
            self.press(roots, frame, pointer, ctx, events);
        } else if input.held("ui_click") && pointer_ok {
            self.drag(roots, frame, pointer, ctx, events);
        } else if !input.held("ui_click") {
            self.captured = None;
        }
        if input.wheel != [0.0; 2] && pointer_ok && input.wheel.iter().all(|v| v.is_finite()) {
            self.wheel(roots, frame, pointer, input.wheel, events);
        }
        // At most one activation per widget per tick when keyboard and pointer coincide.
        if !input.pressed("ui_click") && !input.held("ui_click") {
            self.keyboard(roots, frame, input, navigated, ctx, events);
        }
        self.edit_text(roots, frame, ctx, events);
    }

    fn hover(
        &mut self,
        roots: &[(String, UiRoot)],
        frame: &Frame,
        pointer: [f32; 2],
        pointer_ok: bool,
        input: &ActionFrame,
        dt: f32,
    ) {
        let in_popup = self.popup.as_ref().is_some_and(|p| {
            frame.find(&p.key).is_some_and(|t| {
                let target = &frame.targets[t];
                popup_geometry(roots, target, p, frame.viewport)
                    .is_some_and(|g| contains(g.rect, pointer))
            })
        });
        self.hovered = (pointer_ok && !in_popup)
            .then(|| frame.hit(pointer))
            .flatten()
            .map(|t| frame.targets[t].key.clone());
        // Tooltip owner: topmost box with a tooltip under the pointer.
        let owner = (pointer_ok && !in_popup)
            .then(|| {
                frame.paint.iter().rev().find_map(|(f, b)| {
                    let rf = &frame.roots[*f];
                    let bounds = &rf.laid.boxes[*b];
                    let (id, root) = &roots[rf.root];
                    let node = &root.tree.nodes[bounds.node];
                    let tooltip = node.tooltip.as_ref()?;
                    (bounds.item.is_none()
                        && contains(bounds.rect, pointer)
                        && contains(bounds.clip, pointer)
                        && frame.box_allowed(*f, bounds.node))
                    .then(|| {
                        (
                            UiInteraction {
                                root: id.clone(),
                                node: node.id.clone(),
                            },
                            tooltip.clone(),
                        )
                    })
                })
            })
            .flatten();
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match owner {
            Some((key, tooltip)) if Some(&key) == self.hover_owner.as_ref() => {
                if input.pressed("ui_click") {
                    self.hover_time = 0.0;
                    self.tooltip = None;
                } else {
                    self.hover_time += dt;
                }
                if self.hover_time >= tooltip.delay && !tooltip.text.is_empty() {
                    let at = self.tooltip.as_ref().map_or(pointer, |t| t.at);
                    self.tooltip = Some(TooltipState {
                        key,
                        text: tooltip.text,
                        at,
                    });
                }
            }
            Some((key, _)) => {
                self.hover_owner = Some(key);
                self.hover_time = 0.0;
                self.tooltip = None;
            }
            None => {
                self.hover_owner = None;
                self.hover_time = 0.0;
                self.tooltip = None;
            }
        }
    }

    fn popup_input(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        input: &ActionFrame,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        let Some(state) = self.popup.clone() else {
            return;
        };
        let Some(t) = frame.find(&state.key) else {
            self.popup = None;
            return;
        };
        let target = &frame.targets[t];
        let Some(geometry) = popup_geometry(roots, target, &state, frame.viewport) else {
            self.popup = None;
            return;
        };
        let mut state = state;
        let mut choose = None;
        let mut close = false;
        let count = geometry.count;
        if input.pressed("ui_click") {
            self.captured = None;
            match geometry.option_at(input.pointer) {
                Some(index) => choose = Some(index),
                None => close = true,
            }
        }
        if input.wheel[1].is_finite()
            && input.wheel[1] != 0.0
            && contains(geometry.rect, input.pointer)
        {
            let lines = -input.wheel[1].round() as i64;
            state.first =
                (state.first as i64 + lines).clamp(0, (count - geometry.visible) as i64) as u32;
        }
        if count > 0 {
            if input.pressed("ui_down") {
                state.highlight = (state.highlight + 1).min(count - 1);
            }
            if input.pressed("ui_up") {
                state.highlight = state.highlight.saturating_sub(1);
            }
            if input.pressed("ui_down") || input.pressed("ui_up") {
                state.first = Popup {
                    first: state.first,
                    ..geometry
                }
                .reveal(state.highlight);
            }
            if input.pressed("ui_confirm") {
                choose = Some(state.highlight);
            }
        }
        if input.pressed("ui_cancel") {
            close = true;
        }
        if let Some(index) = choose {
            let node = node_mut(roots, target);
            if let UiWidget::Dropdown { selected, .. } = &mut node.widget
                && *selected != Some(index)
            {
                *selected = Some(index);
                let node = &*node;
                events.push(event(target, node, "ui.selection_changed", json!(index)));
            }
            close = true;
        }
        self.popup = (!close).then_some(state);
    }

    /// Move focus; returns true when focus moved (the keys are then spent).
    fn navigate(&mut self, roots: &[(String, UiRoot)], frame: &Frame, input: &ActionFrame) -> bool {
        let before = self.focus.clone();
        self.move_focus(roots, frame, input);
        let moved = self.focus != before;
        self.reveal_focus |= moved;
        moved
    }

    fn move_focus(&mut self, roots: &[(String, UiRoot)], frame: &Frame, input: &ActionFrame) {
        let candidates: Vec<usize> = (0..frame.targets.len())
            .filter(|t| frame.targets[*t].focusable && frame.allowed(*t))
            .collect();
        let order: Vec<(i32, usize)> = candidates
            .iter()
            .map(|t| (frame.targets[*t].tab_index, *t))
            .collect();
        let current = self.focus.as_ref().and_then(|f| frame.find(f));
        let next = input.pressed("ui_next");
        let previous = input.pressed("ui_previous");
        if next || previous {
            let position = current.and_then(|c| candidates.iter().position(|t| *t == c));
            if let Some(i) = navigation::tab_step(&order, position, previous && !next) {
                self.focus = Some(frame.targets[candidates[i]].key.clone());
            }
            return;
        }
        let direction = [
            ("ui_up", UiDirection::Up),
            ("ui_down", UiDirection::Down),
            ("ui_left", UiDirection::Left),
            ("ui_right", UiDirection::Right),
        ]
        .into_iter()
        .find(|(action, _)| input.pressed(action))
        .map(|(_, d)| d);
        let Some(direction) = direction else {
            return;
        };
        let Some(current) = current.filter(|c| candidates.contains(c)) else {
            let back = matches!(direction, UiDirection::Up | UiDirection::Left);
            if let Some(i) = navigation::tab_step(&order, None, back) {
                self.focus = Some(frame.targets[candidates[i]].key.clone());
            }
            return;
        };
        let target = &frame.targets[current];
        let node = node(roots, target);
        let children = frame.roots[target.frame].laid.hierarchy.children[target.node].len();
        if consumes(node, direction, children) {
            return;
        }
        if let Some(id) = direction.target(&node.nav)
            && let Some(t) = candidates
                .iter()
                .copied()
                .find(|t| frame.targets[*t].root == target.root && frame.targets[*t].key.node == id)
        {
            self.focus = Some(frame.targets[t].key.clone());
            return;
        }
        let from = target.bounds.visible();
        let others = candidates
            .iter()
            .copied()
            .filter(|t| *t != current)
            .map(|t| (t, frame.targets[t].bounds.visible()));
        if let Some(t) = navigation::spatial(from, others, direction) {
            self.focus = Some(frame.targets[t].key.clone());
        }
    }

    fn press(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        pointer: [f32; 2],
        ctx: &UiTickContext<'_>,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        self.captured = None;
        let Some(t) = frame.hit(pointer) else {
            // Clicks outside an open modal are swallowed and keep its focus.
            if frame.modal.is_none() {
                self.focus = None;
            }
            return;
        };
        let target = &frame.targets[t];
        let key = target.key.clone();
        if target.focusable {
            self.focus = Some(key.clone());
        } else if !matches!(node(roots, target).widget, UiWidget::ScrollView { .. }) {
            self.focus = None;
        }
        self.captured = Some(Capture::Press(key.clone()));
        let bounds = &target.bounds;
        match &node(roots, target).widget {
            UiWidget::Slider { .. } => {
                self.captured = Some(Capture::Slider(key));
                events.extend(activate(roots, target, Some(pointer), 0));
            }
            UiWidget::TextInput { .. } => {
                self.captured = Some(Capture::Select(key.clone()));
                self.place_caret(roots, target, pointer, ctx, false);
            }
            UiWidget::Dropdown {
                selected, options, ..
            } => {
                if !options.is_empty() {
                    let highlight = selected.unwrap_or(0);
                    self.popup = Some(PopupState {
                        key,
                        highlight,
                        first: highlight,
                    });
                }
            }
            UiWidget::ScrollView { offset, .. } => {
                let bars = chrome::scrollbars(bounds, node(roots, target));
                let thumb = (0..2)
                    .find(|axis| bars[*axis].is_some_and(|(track, _)| contains(track, pointer)));
                self.captured = Some(match thumb {
                    Some(axis) => Capture::Thumb {
                        key,
                        axis,
                        pointer,
                        offset: bounds.scroll,
                    },
                    None => Capture::Drag {
                        key,
                        pointer,
                        offset: [
                            offset[0].min(bounds.scroll_max[0]),
                            offset[1].min(bounds.scroll_max[1]),
                        ],
                    },
                });
            }
            UiWidget::List { .. } => {
                let laid = &frame.roots[target.frame].laid;
                let children = &laid.hierarchy.children[target.node];
                let hit = laid.boxes.iter().find(|b| {
                    children.contains(&b.node)
                        && contains(b.rect, pointer)
                        && contains(b.clip, pointer)
                });
                if let Some(child) = hit.and_then(|b| children.iter().position(|c| *c == b.node)) {
                    events.extend(select(roots, target, child as u32, None));
                }
            }
            UiWidget::VirtualList {
                item_extent,
                item_count,
                horizontal,
                ..
            } => {
                let along = if *horizontal {
                    pointer[0] - bounds.content.x + bounds.scroll[0]
                } else {
                    pointer[1] - bounds.content.y + bounds.scroll[1]
                };
                let index = (along / item_extent).floor();
                if index >= 0.0 && (index as u32) < *item_count {
                    events.extend(select(roots, target, index as u32, None));
                }
            }
            UiWidget::Tabs {
                labels,
                header_height,
                ..
            } => {
                let cells = chrome::tab_cells(bounds, labels.len(), *header_height);
                if let Some(i) = cells.iter().position(|c| contains(*c, pointer)) {
                    events.extend(select(roots, target, i as u32, None));
                }
            }
            UiWidget::ModalDialog {
                buttons,
                button_height,
                ..
            } => {
                let cells = chrome::dialog_buttons(bounds, buttons.len(), *button_height);
                if let Some(i) = cells.iter().position(|c| contains(*c, pointer)) {
                    events.extend(close_dialog(roots, target, i as i64));
                }
            }
            UiWidget::RichLabel { .. } => {
                let origin = target.bounds.content;
                let hit = link_regions(roots, target, ctx).into_iter().find(|l| {
                    contains(
                        Rect {
                            x: l.rect.x + origin.x,
                            y: l.rect.y + origin.y,
                            ..l.rect
                        },
                        pointer,
                    )
                });
                if let Some(link) = hit {
                    events.push(link_event(roots, target, &link.id));
                }
            }
            _ => events.extend(activate(roots, target, Some(pointer), 0)),
        }
    }

    fn drag(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        pointer: [f32; 2],
        ctx: &UiTickContext<'_>,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        let Some(capture) = self.captured.clone() else {
            return;
        };
        let Some(t) = frame.find(capture.key()) else {
            return;
        };
        let target = &frame.targets[t];
        match capture {
            Capture::Slider(_) => events.extend(activate(roots, target, Some(pointer), 0)),
            Capture::Select(_) => self.place_caret(roots, target, pointer, ctx, true),
            Capture::Drag {
                pointer: start,
                offset,
                ..
            } => {
                let to = [
                    offset[0] - (pointer[0] - start[0]),
                    offset[1] - (pointer[1] - start[1]),
                ];
                events.extend(scroll_to(roots, target, to));
            }
            Capture::Thumb {
                axis,
                pointer: start,
                offset,
                ..
            } => {
                let bars = chrome::scrollbars(&target.bounds, node(roots, target));
                if let Some((track, thumb)) = bars[axis] {
                    let (length, size) = if axis == 1 {
                        (track.height, thumb.height)
                    } else {
                        (track.width, thumb.width)
                    };
                    let travel = (length - size).max(1.0);
                    let mut to = offset;
                    to[axis] +=
                        (pointer[axis] - start[axis]) * target.bounds.scroll_max[axis] / travel;
                    events.extend(scroll_to(roots, target, to));
                }
            }
            Capture::Press(_) => {}
        }
    }

    fn wheel(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        pointer: [f32; 2],
        wheel: [f32; 2],
        events: &mut Vec<UiWidgetEvent>,
    ) {
        let delta = [wheel[0] * WHEEL_STEP, -wheel[1] * WHEEL_STEP];
        // Innermost scrollable under the pointer that can still move.
        for t in (0..frame.targets.len()).rev() {
            let target = &frame.targets[t];
            if !frame.allowed(t) || !target.bounds.hit(pointer) {
                continue;
            }
            let max = target.bounds.scroll_max;
            let scroll = target.bounds.scroll;
            let mut to = scroll;
            match &node(roots, target).widget {
                UiWidget::ScrollView { axes, .. } => {
                    if axes.horizontal() && !axes.vertical() {
                        to[0] += if delta[0] != 0.0 { delta[0] } else { delta[1] };
                    } else {
                        to[0] += delta[0];
                        to[1] += delta[1];
                    }
                }
                UiWidget::VirtualList { horizontal, .. } => {
                    let axis = usize::from(!*horizontal);
                    to[axis] += if delta[axis] != 0.0 {
                        delta[axis]
                    } else {
                        delta[1 - axis]
                    };
                }
                _ => continue,
            }
            let to = [to[0].clamp(0.0, max[0]), to[1].clamp(0.0, max[1])];
            if to != scroll {
                events.extend(scroll_to(roots, target, to));
                return;
            }
        }
    }

    fn keyboard(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        input: &ActionFrame,
        navigated: bool,
        ctx: &UiTickContext<'_>,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        if input.pressed("ui_cancel")
            && let Some(m) = frame.modal
        {
            events.extend(close_dialog(roots, &frame.targets[m], -1));
            return;
        }
        let Some(t) = self.focus.as_ref().and_then(|f| frame.find(f)) else {
            return;
        };
        if !frame.allowed(t) {
            return;
        }
        let target = &frame.targets[t];
        let axis = |plus: &str, minus: &str| {
            if navigated {
                0
            } else {
                i32::from(input.pressed(plus)) - i32::from(input.pressed(minus))
            }
        };
        let horizontal = axis("ui_right", "ui_left");
        let vertical = axis("ui_down", "ui_up");
        let confirm = input.pressed("ui_confirm");
        if input.pressed("ui_cancel")
            && matches!(node(roots, target).widget, UiWidget::TextInput { .. })
        {
            // Gamepad equivalent of the Escape edit key: leave the input.
            self.focus = None;
            self.preedit = None;
            return;
        }
        match node(roots, target).widget.clone() {
            UiWidget::Dropdown {
                selected, options, ..
            } if confirm && !options.is_empty() => {
                let highlight = selected.unwrap_or(0);
                self.popup = Some(PopupState {
                    key: target.key.clone(),
                    highlight,
                    first: highlight,
                });
            }
            UiWidget::Tabs {
                selected, labels, ..
            } if horizontal != 0 => {
                let next = (selected as i64 + horizontal as i64).clamp(0, labels.len() as i64 - 1);
                events.extend(select(roots, target, next as u32, None));
            }
            UiWidget::ModalDialog { buttons, .. } => {
                let choice = self
                    .dialog_choice
                    .as_ref()
                    .filter(|(k, _)| *k == target.key)
                    .map_or(0, |(_, c)| *c);
                if confirm && !buttons.is_empty() {
                    events.extend(close_dialog(roots, target, choice as i64));
                } else if horizontal != 0 && !buttons.is_empty() {
                    let next =
                        (choice as i64 + horizontal as i64).clamp(0, buttons.len() as i64 - 1);
                    self.dialog_choice = Some((target.key.clone(), next as u32));
                }
            }
            UiWidget::List { selected, .. } if vertical != 0 => {
                let count = frame.roots[target.frame].laid.hierarchy.children[target.node].len();
                if let Some(next) = step_selection(selected, vertical, count as u32) {
                    events.extend(select(roots, target, next, None));
                }
            }
            UiWidget::VirtualList {
                selected,
                item_count,
                horizontal: along_x,
                item_extent,
                ..
            } => {
                let delta = if along_x { horizontal } else { vertical };
                if delta != 0
                    && let Some(next) = step_selection(selected, delta, item_count)
                {
                    let b = &target.bounds;
                    let (scroll, viewport) = if along_x {
                        (b.scroll[0], b.content.width)
                    } else {
                        (b.scroll[1], b.content.height)
                    };
                    let start = next as f32 * item_extent;
                    let reveal = if start < scroll {
                        start
                    } else if start + item_extent > scroll + viewport {
                        start + item_extent - viewport
                    } else {
                        scroll
                    };
                    events.extend(select(roots, target, next, Some(reveal)));
                }
            }
            UiWidget::RichLabel { .. } => {
                if confirm && let Some(link) = link_regions(roots, target, ctx).into_iter().next() {
                    events.push(link_event(roots, target, &link.id));
                }
            }
            _ if horizontal != 0 || confirm => {
                events.extend(activate(roots, target, None, horizontal));
            }
            _ => {}
        }
    }

    fn place_caret(
        &mut self,
        roots: &mut [(String, UiRoot)],
        target: &Target,
        pointer: [f32; 2],
        ctx: &UiTickContext<'_>,
        extend: bool,
    ) {
        let preedit = self
            .preedit
            .as_ref()
            .filter(|(k, _)| *k == target.key)
            .map(|(_, p)| p);
        let index = chrome::input_view(node(roots, target), &target.bounds, ctx.text, preedit)
            .ok()
            .flatten()
            .and_then(|view| view.caret_at(pointer));
        if let Some(index) = index
            && let UiWidget::TextInput { caret, anchor, .. } = &mut node_mut(roots, target).widget
        {
            *caret = index;
            if !extend {
                *anchor = index;
            }
        }
    }

    fn edit_text(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        ctx: &UiTickContext<'_>,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        let Some(t) = self.focus.as_ref().and_then(|f| frame.find(f)) else {
            return;
        };
        let target = &frame.targets[t];
        if self.queue.is_empty() || !frame.allowed(t) {
            return;
        }
        let clipboard = self.clipboard.0.clone();
        let mut changed = false;
        let mut submitted = false;
        let mut cancelled = false;
        let queue = std::mem::take(&mut self.queue);
        let (font, scale, color) = {
            let style = &node(roots, target).style;
            (style.font.clone(), style.font_scale, style.foreground)
        };
        let width = target.bounds.content.width;
        {
            let node = node_mut(roots, target);
            let UiWidget::TextInput {
                text: value,
                caret,
                anchor,
                max_length,
                multiline,
                password,
                ..
            } = &mut node.widget
            else {
                return;
            };
            let mut edit = Edit {
                value,
                caret,
                anchor,
                max_length: *max_length,
                multiline: *multiline,
                password: *password,
            };
            for item in &queue {
                match item {
                    UiTextEvent::Preedit(p) => {
                        self.preedit =
                            (!p.text.is_empty()).then(|| (target.key.clone(), p.clone()));
                        continue;
                    }
                    UiTextEvent::Commit(_) => self.preedit = None,
                    _ => {}
                }
                // Wrapped inputs move by visual line when the backend has geometry.
                if let UiTextEvent::Key {
                    key: key @ (UiEditKey::Up | UiEditKey::Down),
                    modifiers,
                } = item
                    && edit.multiline
                    && let Some(to) = chrome::vertical_caret(
                        ctx.text,
                        &font,
                        scale,
                        color,
                        edit.value,
                        *edit.caret,
                        edit.password,
                        width,
                        *key == UiEditKey::Down,
                    )
                {
                    *edit.caret = to;
                    if !modifiers.shift {
                        *edit.anchor = to;
                    }
                    continue;
                }
                let outcome = edit.apply(item, clipboard.as_ref());
                changed |= outcome.changed;
                if outcome.submitted {
                    submitted = true;
                }
                if outcome.cancelled {
                    cancelled = true;
                    break;
                }
            }
        }
        let node = node(roots, target);
        let UiWidget::TextInput { text: value, .. } = &node.widget else {
            return;
        };
        if changed {
            events.push(event(target, node, "ui.text_changed", json!(value)));
        }
        if submitted {
            events.push(event(target, node, "ui.text_submitted", json!(value)));
        }
        if cancelled {
            self.focus = None;
            self.preedit = None;
        }
    }

    /// Recompute per-node flags, the editing target and the IME area.
    fn refresh(
        &mut self,
        roots: &[(String, UiRoot)],
        frame: &Frame,
        input: &ActionFrame,
        ctx: &UiTickContext<'_>,
    ) {
        let mut states = BTreeMap::new();
        for rf in &frame.roots {
            let (id, root) = &roots[rf.root];
            for bounds in rf.laid.boxes.iter().filter(|b| b.item.is_none()) {
                let node = &root.tree.nodes[bounds.node];
                let mut flags = UiStateFlags::default();
                if !root.enabled || !bounds.enabled {
                    flags.insert(UiStateFlags::DISABLED);
                }
                match &node.widget {
                    UiWidget::Toggle { checked: true, .. } => flags.insert(UiStateFlags::CHECKED),
                    UiWidget::ModalDialog { open: true, .. } => flags.insert(UiStateFlags::OPEN),
                    _ => {}
                }
                if let Some(parent) = rf.laid.hierarchy.parents[bounds.node]
                    && let UiWidget::List {
                        selected: Some(s), ..
                    } = &root.tree.nodes[parent].widget
                    && rf.laid.hierarchy.children[parent].get(*s as usize) == Some(&bounds.node)
                {
                    flags.insert(UiStateFlags::SELECTED);
                }
                if !flags.is_empty() {
                    states.insert(
                        UiInteraction {
                            root: id.clone(),
                            node: node.id.clone(),
                        },
                        flags,
                    );
                }
            }
        }
        let mut mark = |key: &UiInteraction, flag: UiStateFlags| {
            states.entry(key.clone()).or_default().insert(flag);
        };
        if let Some(key) = &self.hovered {
            mark(key, UiStateFlags::HOVERED);
        }
        if let Some(key) = &self.focus {
            mark(key, UiStateFlags::FOCUSED);
        }
        if let Some(popup) = &self.popup {
            mark(&popup.key, UiStateFlags::OPEN);
        }
        if let Some(capture) = &self.captured
            && input.held("ui_click") | input.pressed("ui_click")
            && self.hovered.as_ref() == Some(capture.key())
        {
            mark(capture.key(), UiStateFlags::PRESSED);
        }
        self.states = states;
        self.editing = None;
        self.ime_area = None;
        if let Some(t) = self.focus.as_ref().and_then(|f| frame.find(f)) {
            let target = &frame.targets[t];
            let node = node(roots, target);
            if matches!(node.widget, UiWidget::TextInput { .. }) {
                self.editing = Some(target.key.clone());
                let preedit = self.preedit.as_ref().map(|(_, p)| p);
                self.ime_area = chrome::input_view(node, &target.bounds, ctx.text, preedit)
                    .ok()
                    .flatten()
                    .map(|v| v.caret);
            }
        }
    }

    /// Interaction state needed to draw one root.
    pub fn render_state(&self, root: &str) -> UiRenderState {
        let mine = |k: &UiInteraction| (k.root == root).then(|| k.node.clone());
        UiRenderState {
            focus: self.focus.as_ref().and_then(mine),
            pressed: self
                .states
                .iter()
                .find(|(k, f)| k.root == root && f.pressed())
                .map(|(k, _)| k.node.clone()),
            popup: self
                .popup
                .as_ref()
                .and_then(|p| mine(&p.key).map(|n| (n, p.highlight, p.first))),
            preedit: self
                .preedit
                .as_ref()
                .and_then(|(k, p)| mine(k).map(|n| (n, p.clone()))),
            tooltip: self
                .tooltip
                .as_ref()
                .and_then(|t| mine(&t.key).map(|n| (n, t.text.clone(), t.at))),
            dialog_choice: self
                .dialog_choice
                .as_ref()
                .and_then(|(k, c)| mine(k).map(|n| (n, *c))),
            ..Default::default()
        }
    }
}

/// Whether the focused widget handles a direction itself instead of moving
/// focus. Lists keep vertical keys until their first/last item is selected.
fn consumes(node: &UiNode, direction: UiDirection, children: usize) -> bool {
    let horizontal = matches!(direction, UiDirection::Left | UiDirection::Right);
    let back = matches!(direction, UiDirection::Up | UiDirection::Left);
    let within = |selected: Option<u32>, count: u32| match selected {
        None => !back && count > 0,
        Some(s) if back => s > 0,
        Some(s) => s + 1 < count,
    };
    match &node.widget {
        UiWidget::TextInput { .. } => true,
        UiWidget::Slider { .. } | UiWidget::Tabs { .. } | UiWidget::ModalDialog { .. } => {
            horizontal
        }
        UiWidget::List { selected, .. } => !horizontal && within(*selected, children as u32),
        UiWidget::VirtualList {
            selected,
            item_count,
            horizontal: along_x,
            ..
        } => horizontal == *along_x && within(*selected, *item_count),
        _ => false,
    }
}

fn step_selection(selected: Option<u32>, delta: i32, count: u32) -> Option<u32> {
    if count == 0 {
        return None;
    }
    let next = match selected {
        None => 0,
        Some(s) => (s as i64 + delta as i64).clamp(0, count as i64 - 1) as u32,
    };
    (Some(next) != selected).then_some(next)
}

fn popup_geometry(
    roots: &[(String, UiRoot)],
    target: &Target,
    state: &PopupState,
    viewport: [u32; 2],
) -> Option<Popup> {
    match &node(roots, target).widget {
        UiWidget::Dropdown {
            options,
            max_visible,
            ..
        } if !options.is_empty() => Some(Popup::new(
            target.bounds.rect,
            options.len(),
            *max_visible,
            state.first,
            viewport,
        )),
        _ => None,
    }
}

/// Select index `index` on a list, virtual list or tabs widget.
fn select(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    index: u32,
    reveal: Option<f32>,
) -> Option<UiWidgetEvent> {
    let node = node_mut(roots, target);
    let name = match &mut node.widget {
        UiWidget::List { selected, .. } => {
            let changed = *selected != Some(index);
            *selected = Some(index);
            changed.then_some("ui.selection_changed")
        }
        UiWidget::VirtualList {
            selected,
            scroll: offset,
            ..
        } => {
            if let Some(reveal) = reveal {
                *offset = reveal.max(0.0);
            }
            let changed = *selected != Some(index);
            *selected = Some(index);
            changed.then_some("ui.selection_changed")
        }
        UiWidget::Tabs { selected, .. } => {
            let changed = *selected != index;
            *selected = index;
            changed.then_some("ui.tab_changed")
        }
        _ => None,
    }?;
    Some(event(target, node, name, json!(index)))
}

fn scroll_to(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    to: [f32; 2],
) -> Option<UiWidgetEvent> {
    let max = target.bounds.scroll_max;
    let to = [to[0].clamp(0.0, max[0]), to[1].clamp(0.0, max[1])];
    let node = node_mut(roots, target);
    let value = match &mut node.widget {
        UiWidget::ScrollView { offset, .. } => {
            if *offset == to {
                return None;
            }
            *offset = to;
            json!(to)
        }
        UiWidget::VirtualList {
            scroll: offset,
            horizontal,
            ..
        } => {
            let along = if *horizontal { to[0] } else { to[1] };
            if *offset == along {
                return None;
            }
            *offset = along;
            json!(along)
        }
        _ => return None,
    };
    Some(event(target, node, "ui.scrolled", value))
}

/// Close a modal dialog with button `result` (-1 = cancelled).
fn close_dialog(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    result: i64,
) -> Option<UiWidgetEvent> {
    let node = node_mut(roots, target);
    let UiWidget::ModalDialog { open, .. } = &mut node.widget else {
        return None;
    };
    *open = false;
    Some(event(target, node, "ui.dialog_result", json!(result)))
}

fn activate(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    pointer: Option<[f32; 2]>,
    direction: i32,
) -> Option<UiWidgetEvent> {
    let node = node_mut(roots, target);
    let content = target.bounds.content;
    let (name, value) = match &mut node.widget {
        UiWidget::Button { .. } if direction == 0 => ("ui.clicked", Value::Null),
        UiWidget::Toggle { checked, .. } if direction == 0 => {
            *checked = !*checked;
            ("ui.value_changed", Value::Bool(*checked))
        }
        UiWidget::Slider {
            value,
            min,
            max,
            step,
            ..
        } => {
            let previous = *value;
            let candidate = if let Some(pointer) = pointer {
                if !pointer.iter().all(|v| v.is_finite()) || content.width <= 0.0 {
                    return None;
                }
                *min + ((pointer[0] - content.x) / content.width).clamp(0.0, 1.0) * (*max - *min)
            } else if direction != 0 {
                *value + direction as f32 * *step
            } else {
                return None;
            };
            *value = quantize(candidate, *min, *max, *step);
            if *value == previous {
                return None;
            }
            ("ui.value_changed", json!(*value))
        }
        _ => return None,
    };
    let node = &*node;
    Some(event(target, node, name, value))
}

/// `candidate` snapped to the slider step grid and clamped to its range.
fn quantize(candidate: f32, min: f32, max: f32, step: f32) -> f32 {
    // Calculate in f64 so a tiny positive step cannot overflow quantization.
    let steps = ((candidate as f64 - min as f64) / step as f64).round();
    if candidate >= max {
        max
    } else if candidate <= min {
        min
    } else {
        (steps * step as f64 + min as f64).clamp(min as f64, max as f64) as f32
    }
}

/// Link regions of a rich label, relative to its content box. Empty for
/// backends without rich text (the bitmap backend) and for unlinked text.
fn link_regions(
    roots: &[(String, UiRoot)],
    target: &Target,
    ctx: &UiTickContext<'_>,
) -> Vec<UiLinkRegion> {
    let node = node(roots, target);
    let Some((content, kind, _, _)) = layout::display_text(node) else {
        return vec![];
    };
    let request = layout::text_request(node, &content, kind, Some(target.bounds.content.width));
    ctx.text
        .layout(&request)
        .map(|l| l.links)
        .unwrap_or_default()
}

fn link_event(roots: &[(String, UiRoot)], target: &Target, id: &str) -> UiWidgetEvent {
    event(target, node(roots, target), "ui.link_activated", json!(id))
}

mod access;
