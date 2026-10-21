use engine_project::input::RawInput;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
};

/// Translate physical keys to the project's stable, platform-independent tokens.
pub fn key_token(code: KeyCode) -> Option<String> {
    let debug = format!("{code:?}");
    if let Some(letter) = debug.strip_prefix("Key").filter(|s| s.len() == 1) {
        return Some(format!("Key:{letter}"));
    }
    if let Some(digit) = debug.strip_prefix("Digit") {
        return Some(format!("Key:{digit}"));
    }
    if debug.starts_with('F')
        && debug[1..]
            .parse::<u8>()
            .is_ok_and(|n| (1..=25).contains(&n))
    {
        return Some(format!("Key:{debug}"));
    }
    let name = match code {
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::ShiftLeft => "LeftShift",
        KeyCode::ShiftRight => "RightShift",
        KeyCode::ControlLeft => "LeftControl",
        KeyCode::ControlRight => "RightControl",
        KeyCode::AltLeft => "LeftAlt",
        KeyCode::AltRight => "RightAlt",
        KeyCode::SuperLeft => "LeftSuper",
        KeyCode::SuperRight => "RightSuper",
        KeyCode::BracketLeft => "LeftBracket",
        KeyCode::BracketRight => "RightBracket",
        KeyCode::Backquote => "GraveAccent",
        KeyCode::Quote => "Apostrophe",
        KeyCode::ContextMenu => "Menu",
        KeyCode::Numpad0 => "Keypad0",
        KeyCode::Numpad1 => "Keypad1",
        KeyCode::Numpad2 => "Keypad2",
        KeyCode::Numpad3 => "Keypad3",
        KeyCode::Numpad4 => "Keypad4",
        KeyCode::Numpad5 => "Keypad5",
        KeyCode::Numpad6 => "Keypad6",
        KeyCode::Numpad7 => "Keypad7",
        KeyCode::Numpad8 => "Keypad8",
        KeyCode::Numpad9 => "Keypad9",
        KeyCode::NumpadDecimal => "KeypadDecimal",
        KeyCode::NumpadDivide => "KeypadDivide",
        KeyCode::NumpadMultiply => "KeypadMultiply",
        KeyCode::NumpadSubtract => "KeypadSubtract",
        KeyCode::NumpadAdd => "KeypadAdd",
        KeyCode::NumpadEnter => "KeypadEnter",
        KeyCode::NumpadEqual => "KeypadEqual",
        KeyCode::IntlBackslash => "World1",
        KeyCode::IntlRo => "World2",
        KeyCode::Space
        | KeyCode::Comma
        | KeyCode::Minus
        | KeyCode::Period
        | KeyCode::Slash
        | KeyCode::Semicolon
        | KeyCode::Equal
        | KeyCode::Backslash
        | KeyCode::Escape
        | KeyCode::Enter
        | KeyCode::Tab
        | KeyCode::Backspace
        | KeyCode::Insert
        | KeyCode::Delete
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause => &debug,
        _ => return None,
    };
    Some(format!("Key:{name}"))
}

pub(crate) fn window_input(
    raw: &mut RawInput,
    event: &WindowEvent,
    physical_pointer: &mut [f32; 2],
) {
    match event {
        WindowEvent::KeyboardInput { event, .. } => {
            if let PhysicalKey::Code(code) = event.physical_key
                && let Some(token) = key_token(code)
            {
                set_down(raw, token, event.state.is_pressed());
            }
        }
        WindowEvent::MouseInput { state, button, .. } => {
            let name = match button {
                MouseButton::Left => "Left".into(),
                MouseButton::Right => "Right".into(),
                MouseButton::Middle => "Middle".into(),
                MouseButton::Back => "4".into(),
                MouseButton::Forward => "5".into(),
                MouseButton::Other(n) => n.to_string(),
            };
            set_down(
                raw,
                format!("Mouse:{name}"),
                *state == ElementState::Pressed,
            );
        }
        WindowEvent::CursorMoved { position, .. } => {
            *physical_pointer = [position.x as f32, position.y as f32]
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let [x, y] = match delta {
                MouseScrollDelta::LineDelta(x, y) => [*x, *y],
                MouseScrollDelta::PixelDelta(p) => [p.x as f32 / 40.0, p.y as f32 / 40.0],
            };
            raw.wheel[0] += x;
            raw.wheel[1] += y;
        }
        WindowEvent::Focused(focused) => {
            raw.focused = *focused;
            if !focused {
                raw.down.retain(|k| k.starts_with("Gamepad:"));
                raw.wheel = [0.0; 2];
            }
        }
        _ => {}
    }
}
fn set_down(raw: &mut RawInput, token: String, down: bool) {
    if down {
        raw.down.insert(token);
    } else {
        raw.down.remove(&token);
    }
}

pub(crate) struct Gamepads {
    gilrs: gilrs::Gilrs,
    active: Option<gilrs::GamepadId>,
}
impl Gamepads {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            gilrs: gilrs::Gilrs::new()
                .map_err(|e| anyhow::anyhow!("gamepad initialization: {e}"))?,
            active: None,
        })
    }
    pub fn sample(&mut self, raw: &mut RawInput) {
        while self.gilrs.next_event().is_some() {}
        if self
            .active
            .is_none_or(|id| !self.gilrs.gamepad(id).is_connected())
        {
            self.active = self.gilrs.gamepads().next().map(|(id, _)| id);
        }
        raw.down.retain(|k| !k.starts_with("Gamepad:"));
        raw.axes.clear();
        raw.gamepad_connected = self.active.is_some();
        let Some(id) = self.active else {
            return;
        };
        let pad = self.gilrs.gamepad(id);
        for (button, name) in [
            (gilrs::Button::South, "A"),
            (gilrs::Button::East, "B"),
            (gilrs::Button::West, "X"),
            (gilrs::Button::North, "Y"),
            (gilrs::Button::LeftTrigger, "LeftBumper"),
            (gilrs::Button::RightTrigger, "RightBumper"),
            (gilrs::Button::LeftTrigger2, "LeftTrigger"),
            (gilrs::Button::RightTrigger2, "RightTrigger"),
            (gilrs::Button::Select, "Back"),
            (gilrs::Button::Start, "Start"),
            (gilrs::Button::Mode, "Guide"),
            (gilrs::Button::LeftThumb, "LeftThumb"),
            (gilrs::Button::RightThumb, "RightThumb"),
            (gilrs::Button::DPadUp, "DpadUp"),
            (gilrs::Button::DPadDown, "DpadDown"),
            (gilrs::Button::DPadLeft, "DpadLeft"),
            (gilrs::Button::DPadRight, "DpadRight"),
        ] {
            if pad.is_pressed(button) {
                raw.down.insert(format!("Gamepad:{name}"));
            }
        }
        for (axis, name) in [
            (gilrs::Axis::LeftStickX, "LeftX"),
            (gilrs::Axis::LeftStickY, "LeftY"),
            (gilrs::Axis::RightStickX, "RightX"),
            (gilrs::Axis::RightStickY, "RightY"),
        ] {
            raw.axes.insert(name.into(), pad.value(axis));
        }
        // The binding mapper treats triggers as analog axes, including digital
        // controllers whose trigger values only report zero and one.
        for (button, name) in [
            (gilrs::Button::LeftTrigger2, "LeftTrigger"),
            (gilrs::Button::RightTrigger2, "RightTrigger"),
        ] {
            raw.axes.insert(
                name.into(),
                pad.button_data(button).map_or(0.0, |data| data.value()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_keys_match_authored_registry() {
        let registry = engine_project::input::binding_registry();
        for key in [
            KeyCode::KeyW,
            KeyCode::Digit8,
            KeyCode::ArrowLeft,
            KeyCode::ShiftRight,
            KeyCode::NumpadEnter,
            KeyCode::F12,
            KeyCode::BracketLeft,
        ] {
            assert!(registry.contains(&key_token(key).unwrap()));
        }
        assert_eq!(key_token(KeyCode::KeyW).as_deref(), Some("Key:W"));
    }
}
