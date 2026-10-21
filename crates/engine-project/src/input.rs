//! Stable binding tokens are independent of native window or gamepad integer codes.
use crate::{Error, Result};
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub type BindingMap = BTreeMap<String, Vec<String>>;
#[derive(Clone, Debug, Serialize, Deserialize, Resource, PartialEq)]
#[serde(default)]
pub struct ActionFrame {
    pub pressed: BTreeSet<String>,
    pub held: BTreeSet<String>,
    pub released: BTreeSet<String>,
    pub values: BTreeMap<String, f32>,
    pub pointer: [f32; 2],
    pub wheel: [f32; 2],
    pub focused: bool,
}
impl Default for ActionFrame {
    fn default() -> Self {
        Self {
            pressed: BTreeSet::new(),
            held: BTreeSet::new(),
            released: BTreeSet::new(),
            values: BTreeMap::new(),
            pointer: [0.0; 2],
            wheel: [0.0; 2],
            focused: true,
        }
    }
}
impl ActionFrame {
    pub fn pressed(&self, action: &str) -> bool {
        self.pressed.contains(action)
    }
    pub fn held(&self, action: &str) -> bool {
        self.held.contains(action)
    }
    pub fn released(&self, action: &str) -> bool {
        self.released.contains(action)
    }
    pub fn value(&self, action: &str) -> f32 {
        self.values.get(action).copied().unwrap_or(0.0)
    }
}
pub type InputSnapshot = ActionFrame;
#[derive(Clone, Debug)]
pub struct RawInput {
    pub down: BTreeSet<String>,
    pub axes: BTreeMap<String, f32>,
    pub pointer: [f32; 2],
    pub wheel: [f32; 2],
    pub focused: bool,
    pub gamepad_connected: bool,
}
impl Default for RawInput {
    fn default() -> Self {
        Self {
            down: BTreeSet::new(),
            axes: BTreeMap::new(),
            pointer: [0.0; 2],
            wheel: [0.0; 2],
            focused: true,
            gamepad_connected: false,
        }
    }
}
pub fn binding_registry() -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for c in ('A'..='Z').chain('0'..='9') {
        result.insert(format!("Key:{c}"));
    }
    for key in [
        "Space",
        "Apostrophe",
        "Comma",
        "Minus",
        "Period",
        "Slash",
        "Semicolon",
        "Equal",
        "LeftBracket",
        "Backslash",
        "RightBracket",
        "GraveAccent",
        "World1",
        "World2",
        "Escape",
        "Enter",
        "Tab",
        "Backspace",
        "Insert",
        "Delete",
        "Right",
        "Left",
        "Down",
        "Up",
        "PageUp",
        "PageDown",
        "Home",
        "End",
        "CapsLock",
        "ScrollLock",
        "NumLock",
        "PrintScreen",
        "Pause",
        "KeypadDecimal",
        "KeypadDivide",
        "KeypadMultiply",
        "KeypadSubtract",
        "KeypadAdd",
        "KeypadEnter",
        "KeypadEqual",
        "LeftShift",
        "LeftControl",
        "LeftAlt",
        "LeftSuper",
        "RightShift",
        "RightControl",
        "RightAlt",
        "RightSuper",
        "Menu",
    ] {
        result.insert(format!("Key:{key}"));
    }
    for n in 1..=25 {
        result.insert(format!("Key:F{n}"));
    }
    for n in 0..=9 {
        result.insert(format!("Key:Keypad{n}"));
    }
    for key in [
        "Left",
        "Right",
        "Middle",
        "4",
        "5",
        "6",
        "7",
        "8",
        "WheelUp",
        "WheelDown",
        "WheelRight",
        "WheelLeft",
    ] {
        result.insert(format!("Mouse:{key}"));
    }
    for key in [
        "A",
        "B",
        "X",
        "Y",
        "LeftBumper",
        "RightBumper",
        "Back",
        "Start",
        "Guide",
        "LeftThumb",
        "RightThumb",
        "DpadUp",
        "DpadRight",
        "DpadDown",
        "DpadLeft",
        "LeftTrigger",
        "RightTrigger",
    ] {
        result.insert(format!("Gamepad:{key}"));
    }
    for axis in ["LeftX", "LeftY", "RightX", "RightY"] {
        for direction in ["Positive", "Negative"] {
            result.insert(format!("Gamepad:{axis}{direction}"));
        }
    }
    result
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct BindingValidation {
    pub errors: Vec<String>,
    pub conflicts: BTreeMap<String, Vec<String>>,
}
pub fn validate_bindings(bindings: &BindingMap, strict: bool) -> BindingValidation {
    let mut result = BindingValidation::default();
    let registry = binding_registry();
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if bindings.len() > 256 {
        result
            .errors
            .push("At most 256 actions are supported".into());
    }
    for (action, tokens) in bindings {
        if action.is_empty() || tokens.is_empty() || tokens.len() > 16 {
            result.errors.push(format!(
                "Action {action} requires a name and 1 to 16 bindings"
            ));
        }
        let mut unique = BTreeSet::new();
        for token in tokens {
            if !registry.contains(token) {
                result.errors.push(format!("Unknown binding: {token}"));
            }
            if !unique.insert(token) {
                result.errors.push(format!("Duplicate binding: {token}"));
            } else {
                owners
                    .entry(token.clone())
                    .or_default()
                    .push(action.clone());
            }
        }
    }
    for (token, actions) in owners {
        if actions.len() > 1 {
            if strict {
                result.errors.push(format!("Binding conflict: {token}"));
            }
            result.conflicts.insert(token, actions);
        }
    }
    result
}
#[derive(Clone, Debug)]
pub struct ActionMapper {
    bindings: BindingMap,
    frame: ActionFrame,
    blocked: BTreeSet<String>,
    blocked_gamepad: BTreeSet<String>,
    deadzone: f32,
    was_focused: bool,
    was_connected: bool,
    sampled: bool,
}
impl ActionMapper {
    pub fn new(bindings: BindingMap) -> Result<Self> {
        let validation = validate_bindings(&bindings, false);
        if !validation.errors.is_empty() {
            return Err(Error::new(
                "input.bindings.invalid",
                validation.errors.join("; "),
            ));
        }
        Ok(Self {
            bindings,
            frame: ActionFrame::default(),
            blocked: BTreeSet::new(),
            blocked_gamepad: BTreeSet::new(),
            deadzone: 0.2,
            was_focused: true,
            was_connected: false,
            sampled: false,
        })
    }
    pub fn bindings(&self) -> &BindingMap {
        &self.bindings
    }
    pub fn deadzone(&self) -> f32 {
        self.deadzone
    }
    pub fn set_deadzone(&mut self, value: f32) -> Result<()> {
        if !value.is_finite() || !(0.0..1.0).contains(&value) {
            return Err(Error::new(
                "input.deadzone.invalid",
                "Deadzone must be in [0,1)",
            ));
        }
        self.deadzone = value;
        Ok(())
    }
    pub fn rebind(&mut self, bindings: BindingMap, strict: bool) -> BindingValidation {
        let result = validate_bindings(&bindings, strict);
        if result.errors.is_empty() {
            self.bindings = bindings;
            self.blocked.extend(self.frame.held.iter().cloned());
        }
        result
    }
    pub fn clear(&mut self) {
        self.frame = ActionFrame::default();
        self.blocked.clear();
        self.blocked_gamepad.clear();
        self.was_focused = true;
        self.was_connected = false;
        self.sampled = false;
    }
    pub fn sample(&mut self, input: &RawInput) -> ActionFrame {
        let clean = |v: f32| if v.is_finite() { v } else { 0.0 };
        let mut next = ActionFrame {
            focused: input.focused,
            pointer: input.pointer.map(clean),
            wheel: if input.focused {
                input.wheel.map(clean)
            } else {
                [0.0; 2]
            },
            ..Default::default()
        };
        for (action, tokens) in &self.bindings {
            let mut value = 0.0_f32;
            for token in tokens {
                let pad = token.starts_with("Gamepad:");
                if pad && !input.gamepad_connected {
                    continue;
                }
                let mut v = if let Some(wheel) = token.strip_prefix("Mouse:Wheel") {
                    match wheel {
                        "Up" => next.wheel[1].max(0.0),
                        "Down" => (-next.wheel[1]).max(0.0),
                        "Right" => next.wheel[0].max(0.0),
                        "Left" => (-next.wheel[0]).max(0.0),
                        _ => 0.0,
                    }
                } else if let Some(axis) = token.strip_prefix("Gamepad:").and_then(|s| {
                    [
                        "LeftX",
                        "LeftY",
                        "RightX",
                        "RightY",
                        "LeftTrigger",
                        "RightTrigger",
                    ]
                    .into_iter()
                    .find(|name| s.starts_with(name))
                }) {
                    let direction = if token.ends_with("Negative") {
                        -1.0
                    } else {
                        1.0
                    };
                    let raw = (clean(input.axes.get(axis).copied().unwrap_or(0.0)) * direction)
                        .clamp(0.0, 1.0);
                    if raw > self.deadzone {
                        (raw - self.deadzone) / (1.0 - self.deadzone)
                    } else {
                        0.0
                    }
                } else if input.down.contains(token) {
                    1.0
                } else {
                    0.0
                };
                if pad {
                    if self.sampled && !self.was_connected && v > 0.0 {
                        self.blocked_gamepad.insert(token.clone());
                    }
                    if self.blocked_gamepad.contains(token) {
                        if v == 0.0 {
                            self.blocked_gamepad.remove(token);
                        }
                        v = 0.0;
                    }
                }
                value = value.max(v.min(1.0));
            }
            if !input.focused || (!self.was_focused && value > 0.0) {
                if value > 0.0 {
                    self.blocked.insert(action.clone());
                }
                value = 0.0;
            } else if self.blocked.contains(action) {
                if value == 0.0 {
                    self.blocked.remove(action);
                }
                value = 0.0;
            }
            next.values.insert(action.clone(), value);
            if value > 0.0 {
                next.held.insert(action.clone());
                if !self.frame.held.contains(action) {
                    next.pressed.insert(action.clone());
                }
            }
        }
        next.released = self.frame.held.difference(&next.held).cloned().collect();
        self.sampled = true;
        self.was_focused = input.focused;
        self.was_connected = input.gamepad_connected;
        self.frame = next.clone();
        next
    }
}
/// Injected actions are one-tick pulses. Physical holds remain held until release.
pub fn merge_input(
    physical: &ActionFrame,
    injected: &BTreeSet<String>,
    previous: &mut BTreeSet<String>,
) -> ActionFrame {
    let mut result = physical.clone();
    result.held.extend(injected.iter().cloned());
    result.pressed = result.held.difference(previous).cloned().collect();
    result.released = previous.difference(&result.held).cloned().collect();
    for name in injected {
        result.values.insert(name.clone(), 1.0);
    }
    *previous = result.held.clone();
    result
}
