use super::*;

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
