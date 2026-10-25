//! Input-remapping screen (`docs/accessibility.md`): a generated `UiRoot`
//! listing every action with its bindings, a "press to rebind" capture flow,
//! conflict display and reset-to-default, built on the public rebinding API
//! (`validate_bindings`, `InputSettings::rebind`, the physical `ActionMapper`).
//!
//! [`RemapScreen`] is the pure state machine and builder; [`RemapController`]
//! drives one on a `UiRoot` entity from the UI tick's widget events and the
//! raw key/button tokens a host publishes in [`RawInputTokens`].
use crate::{
    EngineEvent, Error, Result,
    input::{BindingMap, binding_registry, validate_bindings},
    runtime::{AuthoredId, InputSettings, ProjectResource},
    ui_tree::{
        AccessLive, AccessRole, UiLayout, UiLength, UiNode, UiRoot, UiScrollAxes, UiTickEvents,
        UiTree,
    },
};
use bevy_ecs::prelude::{Entity, Resource, World};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// Actions listed on one screen. Each row costs `4 + bindings` UI nodes and a
/// root holds 1024 nodes.
pub const MAX_REMAP_ACTIONS: usize = 48;
/// Longest action name the screen accepts (it becomes part of node IDs).
pub const MAX_REMAP_ACTION_NAME: usize = 48;
/// Bindings one action may carry (the input module's limit).
pub const MAX_BINDINGS_PER_ACTION: usize = 16;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("remap.invalid", message)
}

/// The binding tokens (`Key:A`, `Mouse:Left`, `Gamepad:A`) of the keys and
/// buttons currently held. A host that owns the physical mapper publishes it
/// every tick (the desktop runner does); capture reads new presses from it.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct RawInputTokens {
    pub down: BTreeSet<String>,
}

/// What to do when a new binding is already used by another action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemapConflicts {
    /// Apply the binding and show the conflict (both actions fire).
    #[default]
    Warn,
    /// Refuse the binding and say which action owns it.
    Reject,
}

#[derive(Clone, Debug)]
pub struct RemapOptions {
    /// Root ID of the generated UI (the `AuthoredId` of the entity).
    pub root_id: String,
    pub title: String,
    /// Font key for every text node (a bitmap font or `font_face` asset).
    pub font: String,
    /// Actions to list in order; empty lists every action of the defaults.
    pub actions: Vec<String>,
    /// Display names by action; the action name is the fallback.
    pub labels: BTreeMap<String, String>,
    pub conflicts: RemapConflicts,
    /// Pressing this token during capture cancels it (it cannot be bound).
    pub cancel_token: String,
    pub layer: i32,
}

impl Default for RemapOptions {
    fn default() -> Self {
        Self {
            root_id: "remap".into(),
            title: "Controls".into(),
            font: String::new(),
            actions: vec![],
            labels: BTreeMap::new(),
            conflicts: RemapConflicts::Warn,
            cancel_token: "Key:Escape".into(),
            layer: 0,
        }
    }
}

/// Which binding of an action a capture replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemapSlot {
    Replace(usize),
    Add,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemapCapture {
    pub action: String,
    pub slot: RemapSlot,
}

/// Something that happened on the screen, returned to native callers and
/// emitted as engine events (`input.rebound`, `input.rebind_conflict`,
/// `input.rebind_reset`, `input.rebind_rejected`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RemapEvent {
    CaptureStarted {
        action: String,
    },
    CaptureCancelled {
        action: String,
    },
    Rebound {
        action: String,
        previous: Option<String>,
        token: String,
        bindings: Vec<String>,
    },
    /// The new binding is shared with other actions.
    Conflict {
        token: String,
        actions: Vec<String>,
        applied: bool,
    },
    Reset {
        action: String,
        bindings: Vec<String>,
    },
    ResetAll,
    Rejected {
        reason: String,
    },
}

/// Pure model of the screen.
#[derive(Clone, Debug)]
pub struct RemapScreen {
    options: RemapOptions,
    actions: Vec<String>,
    defaults: BindingMap,
    bindings: BindingMap,
    capture: Option<RemapCapture>,
    message: String,
    conflicts: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Slot(String, usize),
    Add(String),
    Reset(String),
    ResetAll,
}

impl RemapScreen {
    /// `defaults` are what reset restores; `current` the bindings in force.
    pub fn new(defaults: BindingMap, current: BindingMap, options: RemapOptions) -> Result<Self> {
        for (label, map) in [("defaults", &defaults), ("current", &current)] {
            let check = validate_bindings(map, false);
            if !check.errors.is_empty() {
                return Err(invalid(format!(
                    "Invalid {label} bindings: {}",
                    check.errors.join("; ")
                )));
            }
        }
        let actions: Vec<String> = if options.actions.is_empty() {
            let mut names: Vec<String> = defaults.keys().chain(current.keys()).cloned().collect();
            names.sort();
            names.dedup();
            names
        } else {
            options.actions.clone()
        };
        if actions.len() > MAX_REMAP_ACTIONS {
            return Err(invalid(format!(
                "At most {MAX_REMAP_ACTIONS} actions fit on one remap screen"
            )));
        }
        for action in &actions {
            if action.is_empty() || action.len() > MAX_REMAP_ACTION_NAME {
                return Err(invalid(format!(
                    "Action name `{action}` is empty or too long"
                )));
            }
            if !current.contains_key(action) && !defaults.contains_key(action) {
                return Err(invalid(format!("Unknown action `{action}`")));
            }
        }
        if options.root_id.is_empty() || options.root_id.len() > 128 {
            return Err(invalid("Remap root ID must be 1..=128 bytes"));
        }
        let mut screen = Self {
            options,
            actions,
            defaults,
            bindings: current,
            capture: None,
            message: String::new(),
            conflicts: BTreeMap::new(),
        };
        screen.conflicts = validate_bindings(&screen.bindings, false).conflicts;
        Ok(screen)
    }

    /// Defaults from the project manifest, current from `InputSettings`.
    pub fn from_world(world: &World, options: RemapOptions) -> Result<Self> {
        let current = world
            .get_resource::<InputSettings>()
            .map(|s| s.bindings.clone())
            .ok_or_else(|| invalid("InputSettings is not available"))?;
        let defaults = world
            .get_resource::<ProjectResource>()
            .map(|p| p.0.input_bindings.clone())
            .unwrap_or_else(|| current.clone());
        Self::new(defaults, current, options)
    }

    pub fn bindings(&self) -> &BindingMap {
        &self.bindings
    }
    pub fn actions(&self) -> &[String] {
        &self.actions
    }
    pub fn capturing(&self) -> Option<&RemapCapture> {
        self.capture.as_ref()
    }
    /// Tokens used by more than one action, with their actions.
    pub fn conflicts(&self) -> &BTreeMap<String, Vec<String>> {
        &self.conflicts
    }
    /// The status line shown under the list.
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn options(&self) -> &RemapOptions {
        &self.options
    }

    fn label(&self, action: &str) -> String {
        self.options
            .labels
            .get(action)
            .cloned()
            .unwrap_or_else(|| action.to_owned())
    }

    fn tokens(&self, action: &str) -> Vec<String> {
        self.bindings.get(action).cloned().unwrap_or_default()
    }

    // ---- node IDs ----------------------------------------------------------

    fn slot_id(action: &str, slot: usize) -> String {
        format!("remap.{action}.slot.{slot}")
    }
    fn add_id(action: &str) -> String {
        format!("remap.{action}.add")
    }
    fn reset_id(action: &str) -> String {
        format!("remap.{action}.reset")
    }

    fn command(&self, node: &str) -> Option<Command> {
        if node == "remap.reset_all" {
            return Some(Command::ResetAll);
        }
        for action in &self.actions {
            if node == Self::add_id(action) {
                return Some(Command::Add(action.clone()));
            }
            if node == Self::reset_id(action) {
                return Some(Command::Reset(action.clone()));
            }
            for slot in 0..self.tokens(action).len() {
                if node == Self::slot_id(action, slot) {
                    return Some(Command::Slot(action.clone(), slot));
                }
            }
        }
        None
    }

    // ---- events in ---------------------------------------------------------

    /// A button of the generated UI was activated (`node` is its node ID).
    pub fn press(&mut self, node: &str) -> Vec<RemapEvent> {
        match self.command(node) {
            Some(Command::Slot(action, slot)) => self.start(action, RemapSlot::Replace(slot)),
            Some(Command::Add(action)) => {
                if self.tokens(&action).len() >= MAX_BINDINGS_PER_ACTION {
                    self.message = format!("{} already has the most bindings", self.label(&action));
                    return vec![RemapEvent::Rejected {
                        reason: self.message.clone(),
                    }];
                }
                self.start(action, RemapSlot::Add)
            }
            Some(Command::Reset(action)) => self.reset(&action),
            Some(Command::ResetAll) => self.reset_all(),
            None => vec![],
        }
    }

    fn start(&mut self, action: String, slot: RemapSlot) -> Vec<RemapEvent> {
        self.message = format!(
            "Press a key or button for {}. {} cancels.",
            self.label(&action),
            display_token(&self.options.cancel_token)
        );
        self.capture = Some(RemapCapture {
            action: action.clone(),
            slot,
        });
        vec![RemapEvent::CaptureStarted { action }]
    }

    /// Abandon a running capture.
    pub fn cancel(&mut self) -> Vec<RemapEvent> {
        match self.capture.take() {
            Some(capture) => {
                self.message = "Rebinding cancelled".into();
                vec![RemapEvent::CaptureCancelled {
                    action: capture.action,
                }]
            }
            None => vec![],
        }
    }

    /// Restore one action's default bindings.
    pub fn reset(&mut self, action: &str) -> Vec<RemapEvent> {
        self.capture = None;
        let Some(default) = self.defaults.get(action).cloned() else {
            self.message = format!("{} has no default", self.label(action));
            return vec![RemapEvent::Rejected {
                reason: self.message.clone(),
            }];
        };
        self.bindings.insert(action.to_owned(), default.clone());
        self.conflicts = validate_bindings(&self.bindings, false).conflicts;
        self.message = format!("{} reset to default", self.label(action));
        vec![RemapEvent::Reset {
            action: action.to_owned(),
            bindings: default,
        }]
    }

    pub fn reset_all(&mut self) -> Vec<RemapEvent> {
        self.capture = None;
        for action in self.actions.clone() {
            if let Some(default) = self.defaults.get(&action) {
                self.bindings.insert(action, default.clone());
            }
        }
        self.conflicts = validate_bindings(&self.bindings, false).conflicts;
        self.message = "All controls reset to default".into();
        vec![RemapEvent::ResetAll]
    }

    /// A key or button was pressed while capturing: bind it, or cancel when it
    /// is the cancel token. Does nothing outside a capture.
    pub fn capture_token(&mut self, token: &str) -> Vec<RemapEvent> {
        let Some(capture) = self.capture.clone() else {
            return vec![];
        };
        if token == self.options.cancel_token {
            return self.cancel();
        }
        if !binding_registry().contains(token) {
            self.message = format!("{token} cannot be used as a binding");
            self.capture = None;
            return vec![RemapEvent::Rejected {
                reason: self.message.clone(),
            }];
        }
        self.capture = None;
        let action = capture.action;
        let mut tokens = self.tokens(&action);
        let previous = match capture.slot {
            RemapSlot::Replace(i) => tokens.get(i).cloned(),
            RemapSlot::Add => None,
        };
        if previous.as_deref() == Some(token) {
            self.message = format!("{} keeps {}", self.label(&action), display_token(token));
            return vec![];
        }
        if tokens.iter().any(|t| t == token) {
            self.message = format!(
                "{} is already bound to {}",
                display_token(token),
                self.label(&action)
            );
            return vec![RemapEvent::Rejected {
                reason: self.message.clone(),
            }];
        }
        let others: Vec<String> = self
            .bindings
            .iter()
            .filter(|(name, list)| **name != action && list.iter().any(|t| t == token))
            .map(|(name, _)| name.clone())
            .collect();
        if !others.is_empty() && self.options.conflicts == RemapConflicts::Reject {
            self.message = format!(
                "{} is already used by {}",
                display_token(token),
                self.names(&others)
            );
            return vec![
                RemapEvent::Conflict {
                    token: token.to_owned(),
                    actions: others,
                    applied: false,
                },
                RemapEvent::Rejected {
                    reason: self.message.clone(),
                },
            ];
        }
        match capture.slot {
            RemapSlot::Replace(i) if i < tokens.len() => tokens[i] = token.to_owned(),
            _ => tokens.push(token.to_owned()),
        }
        let mut next = self.bindings.clone();
        next.insert(action.clone(), tokens.clone());
        let check = validate_bindings(&next, false);
        if !check.errors.is_empty() {
            self.message = check.errors.join("; ");
            return vec![RemapEvent::Rejected {
                reason: self.message.clone(),
            }];
        }
        self.bindings = next;
        self.conflicts = check.conflicts;
        self.message = format!("{} is now {}", self.label(&action), display_token(token));
        let label = self.label(&action);
        let mut events = vec![RemapEvent::Rebound {
            action,
            previous,
            token: token.to_owned(),
            bindings: tokens,
        }];
        if !others.is_empty() {
            self.message = format!(
                "{label} is now {}, also used by {}",
                display_token(token),
                self.names(&others)
            );
            events.push(RemapEvent::Conflict {
                token: token.to_owned(),
                actions: others,
                applied: true,
            });
        }
        events
    }

    fn names(&self, actions: &[String]) -> String {
        actions
            .iter()
            .map(|a| self.label(a))
            .collect::<Vec<_>>()
            .join(", ")
    }

    // ---- the generated UI --------------------------------------------------

    /// The screen as a `UiRoot` (regenerate after every change).
    pub fn root(&self) -> Result<UiRoot> {
        let font = self.options.font.clone();
        let text = move |node: UiNode| {
            let font = font.clone();
            node.with_style(move |s| s.font = font)
        };
        let fill = UiLength::Fill;
        let mut nodes = vec![];
        let mut panel = UiNode::container("remap", UiLayout::Column).with_size(fill, fill);
        panel.style.padding = 16.0;
        panel.style.gap = 8.0;
        panel.accessibility = Some(crate::ui_tree::UiAccessibility {
            label: Some(self.options.title.clone()),
            role: Some(AccessRole::Region),
            ..Default::default()
        });
        nodes.push(panel);
        nodes.push(
            text(UiNode::label("remap.title", self.options.title.clone()).child_of("remap"))
                .with_size(fill, UiLength::Pixels(28.0))
                .with_access_role(AccessRole::Heading),
        );
        let mut list = UiNode::scroll_view("remap.list", UiScrollAxes::Vertical).child_of("remap");
        list.style.width = fill;
        list.style.height = fill;
        list.style.layout = UiLayout::Column;
        list.style.gap = 6.0;
        list.accessibility = Some(crate::ui_tree::UiAccessibility {
            label: Some("Control bindings".into()),
            ..Default::default()
        });
        nodes.push(list);
        for action in &self.actions {
            let row = format!("remap.{action}.row");
            let line = format!("remap.{action}.line");
            let tokens = self.tokens(action);
            let capturing = self.capture.as_ref().filter(|c| c.action == *action);
            nodes.push(
                UiNode::container(row.clone(), UiLayout::Column)
                    .child_of("remap.list")
                    .with_size(fill, UiLength::Auto)
                    .with_style(|s| s.gap = 2.0),
            );
            nodes.push(
                UiNode::container(line.clone(), UiLayout::Row)
                    .child_of(row.clone())
                    .with_size(fill, UiLength::Pixels(36.0))
                    .with_style(|s| s.gap = 6.0),
            );
            nodes.push(
                text(UiNode::label(
                    format!("remap.{action}.name"),
                    self.label(action),
                ))
                .child_of(line.clone())
                .with_size(UiLength::Pixels(220.0), fill),
            );
            for (i, token) in tokens.iter().enumerate() {
                let active = capturing.is_some_and(|c| c.slot == RemapSlot::Replace(i));
                let shown = if active {
                    "Press a key...".to_owned()
                } else {
                    display_token(token)
                };
                let access = if active {
                    format!(
                        "{}, binding {}: waiting for a key or button",
                        self.label(action),
                        i + 1
                    )
                } else {
                    format!(
                        "{}, binding {}: {}. Activate to change",
                        self.label(action),
                        i + 1,
                        display_token(token)
                    )
                };
                nodes.push(
                    text(UiNode::button(
                        Self::slot_id(action, i),
                        shown,
                        "remap.rebind",
                    ))
                    .child_of(line.clone())
                    .with_size(UiLength::Pixels(120.0), fill)
                    .with_access_label(access),
                );
            }
            let adding = capturing.is_some_and(|c| c.slot == RemapSlot::Add);
            if tokens.len() < MAX_BINDINGS_PER_ACTION {
                nodes.push(
                    text(UiNode::button(
                        Self::add_id(action),
                        if adding { "..." } else { "+" },
                        "remap.add",
                    ))
                    .child_of(line.clone())
                    .with_size(UiLength::Pixels(36.0), fill)
                    .with_access_label(format!("Add a binding for {}", self.label(action))),
                );
            }
            nodes.push(
                text(UiNode::button(
                    Self::reset_id(action),
                    "Reset",
                    "remap.reset",
                ))
                .child_of(line)
                .with_size(UiLength::Pixels(80.0), fill)
                .with_access_label(format!("Reset {} to default", self.label(action))),
            );
            let clash: Vec<String> = tokens
                .iter()
                .filter_map(|t| {
                    let owners = self.conflicts.get(t)?;
                    let others: Vec<String> =
                        owners.iter().filter(|a| *a != action).cloned().collect();
                    (!others.is_empty()).then(|| {
                        format!("{} also used by {}", display_token(t), self.names(&others))
                    })
                })
                .collect();
            let mut conflict = text(UiNode::label(
                format!("remap.{action}.conflict"),
                if clash.is_empty() {
                    String::new()
                } else {
                    format!("Conflict: {}", clash.join("; "))
                },
            ))
            .child_of(row)
            .with_size(fill, UiLength::Pixels(22.0));
            conflict.visible = !clash.is_empty();
            conflict.style.foreground = [1.0, 0.75, 0.25, 1.0];
            nodes.push(conflict);
        }
        let mut footer = UiNode::container("remap.footer", UiLayout::Row)
            .child_of("remap")
            .with_size(fill, UiLength::Pixels(36.0));
        footer.style.gap = 8.0;
        nodes.push(footer);
        nodes.push(
            text(UiNode::button(
                "remap.reset_all",
                "Reset all",
                "remap.reset_all",
            ))
            .child_of("remap.footer")
            .with_size(UiLength::Pixels(120.0), fill)
            .with_access_label("Reset all controls to default"),
        );
        nodes.push(
            text(UiNode::label("remap.status", self.message.clone()))
                .child_of("remap.footer")
                .with_size(fill, fill)
                .with_access_role(AccessRole::Status)
                .with_access_live(AccessLive::Polite),
        );
        let root = UiRoot {
            tree: UiTree { nodes },
            layer: self.options.layer,
            ..Default::default()
        };
        root.validate()?;
        Ok(root)
    }
}

/// A binding token as people read it: `Key:Space` is `Space`, `Mouse:Left`
/// is `Mouse Left`, `Gamepad:A` is `Gamepad A`.
pub fn display_token(token: &str) -> String {
    match token.split_once(':') {
        Some(("Key", key)) => key.to_owned(),
        Some((device, control)) => format!("{device} {control}"),
        None => token.to_owned(),
    }
}

/// Drives a [`RemapScreen`] on one `UiRoot` entity.
#[derive(Debug)]
pub struct RemapController {
    pub screen: RemapScreen,
    entity: Entity,
    root_key: String,
    previous_down: BTreeSet<String>,
    shown: Option<UiRoot>,
}

impl RemapController {
    /// Create the UI on `entity` (inserting `UiRoot` and, when it has none,
    /// an `AuthoredId` named by `options.root_id`).
    pub fn new(world: &mut World, entity: Entity, options: RemapOptions) -> Result<Self> {
        let root_id = options.root_id.clone();
        let screen = RemapScreen::from_world(world, options)?;
        if world.get::<AuthoredId>(entity).is_none() {
            world.entity_mut(entity).insert(AuthoredId {
                id: root_id.clone(),
                name: root_id,
            });
        }
        let root_key = world
            .get::<AuthoredId>(entity)
            .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone());
        let mut controller = Self {
            screen,
            entity,
            root_key,
            previous_down: BTreeSet::new(),
            shown: None,
        };
        controller.publish(world)?;
        Ok(controller)
    }

    /// The root key under which the UI's widget events arrive.
    pub fn root_key(&self) -> &str {
        &self.root_key
    }

    fn publish(&mut self, world: &mut World) -> Result<()> {
        let root = self.screen.root()?;
        if self.shown.as_ref() != Some(&root) {
            world.entity_mut(self.entity).insert(root.clone());
            self.shown = Some(root);
        }
        Ok(())
    }

    /// Run once per tick after the UI tick: react to button activations, read
    /// a pressed key while capturing, apply changed bindings to
    /// `InputSettings` and refresh the UI. Returns the events of this tick,
    /// which are also emitted as engine events.
    pub fn tick(&mut self, world: &mut World) -> Result<Vec<RemapEvent>> {
        let before = self.screen.bindings().clone();
        let mut events = vec![];
        let clicked: Vec<String> = world
            .get_resource::<UiTickEvents>()
            .map(|e| {
                e.0.iter()
                    .filter(|e| e.root == self.root_key && e.name == "ui.clicked")
                    .map(|e| e.node.clone())
                    .collect()
            })
            .unwrap_or_default();
        let down = world
            .get_resource::<RawInputTokens>()
            .map(|t| t.down.clone())
            .unwrap_or_default();
        let mut started = false;
        for node in clicked {
            let was = self.screen.capturing().is_some();
            events.extend(self.screen.press(&node));
            started |= !was && self.screen.capturing().is_some();
        }
        if started {
            // The press that opened the capture must not bind itself.
            self.previous_down = down;
        } else {
            if self.screen.capturing().is_some()
                && let Some(token) = down.difference(&self.previous_down).next().cloned()
            {
                events.extend(self.screen.capture_token(&token));
            }
            self.previous_down = down;
        }
        if self.screen.bindings() != &before {
            let deadzone = world
                .get_resource::<InputSettings>()
                .map_or(0.2, |s| s.deadzone);
            if let Some(mut settings) = world.get_resource_mut::<InputSettings>() {
                settings.rebind(self.screen.bindings().clone(), deadzone)?;
            }
        }
        self.publish(world)?;
        for event in &events {
            let (name, payload) = match event {
                RemapEvent::Rebound { .. } => ("input.rebound", json!(event)),
                RemapEvent::Conflict { .. } => ("input.rebind_conflict", json!(event)),
                RemapEvent::Reset { .. } | RemapEvent::ResetAll => {
                    ("input.rebind_reset", json!(event))
                }
                RemapEvent::Rejected { .. } => ("input.rebind_rejected", json!(event)),
                RemapEvent::CaptureStarted { .. } | RemapEvent::CaptureCancelled { .. } => continue,
            };
            crate::emit_event(
                world,
                EngineEvent {
                    name: name.into(),
                    source: Some(self.entity),
                    target: Some(self.entity),
                    payload,
                },
            )?;
        }
        Ok(events)
    }
}

/// Install the resource a host publishes raw tokens into.
pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    app.world_mut().insert_resource(RawInputTokens::default());
    Ok(())
}
