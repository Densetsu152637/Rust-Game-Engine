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

#[cfg(test)]
mod translation_tests {
    use super::*;
    use std::collections::BTreeSet;
    use winit::{
        dpi::PhysicalPosition,
        event::{DeviceId, TouchPhase},
    };

    macro_rules! token_cases {
        ($($name:ident: $code:ident => $token:expr;)*) => {$(
            #[test]
            fn $name() {
                assert_eq!(key_token(KeyCode::$code).as_deref(), $token);
            }
        )*};
    }

    token_cases! {
        letter_a_maps_to_key_a: KeyA => Some("Key:A");
        letter_z_maps_to_key_z: KeyZ => Some("Key:Z");
        digit_zero_maps_to_key_zero: Digit0 => Some("Key:0");
        digit_nine_maps_to_key_nine: Digit9 => Some("Key:9");
        f1_maps_to_key_f1: F1 => Some("Key:F1");
        f12_maps_to_key_f12: F12 => Some("Key:F12");
        f25_is_the_last_function_key: F25 => Some("Key:F25");
        f26_is_outside_the_registry: F26 => None;
        f35_is_outside_the_registry: F35 => None;
        arrow_up_drops_the_arrow_prefix: ArrowUp => Some("Key:Up");
        arrow_down_drops_the_arrow_prefix: ArrowDown => Some("Key:Down");
        arrow_left_drops_the_arrow_prefix: ArrowLeft => Some("Key:Left");
        arrow_right_drops_the_arrow_prefix: ArrowRight => Some("Key:Right");
        left_shift_is_spelled_out: ShiftLeft => Some("Key:LeftShift");
        right_control_is_spelled_out: ControlRight => Some("Key:RightControl");
        left_alt_is_spelled_out: AltLeft => Some("Key:LeftAlt");
        right_super_is_spelled_out: SuperRight => Some("Key:RightSuper");
        left_bracket_is_swapped_to_prefix_order: BracketLeft => Some("Key:LeftBracket");
        right_bracket_is_swapped_to_prefix_order: BracketRight => Some("Key:RightBracket");
        backquote_is_grave_accent: Backquote => Some("Key:GraveAccent");
        quote_is_apostrophe: Quote => Some("Key:Apostrophe");
        context_menu_is_menu: ContextMenu => Some("Key:Menu");
        numpad_zero_is_keypad_zero: Numpad0 => Some("Key:Keypad0");
        numpad_seven_is_keypad_seven: Numpad7 => Some("Key:Keypad7");
        numpad_decimal_is_keypad_decimal: NumpadDecimal => Some("Key:KeypadDecimal");
        numpad_divide_is_keypad_divide: NumpadDivide => Some("Key:KeypadDivide");
        numpad_multiply_is_keypad_multiply: NumpadMultiply => Some("Key:KeypadMultiply");
        numpad_subtract_is_keypad_subtract: NumpadSubtract => Some("Key:KeypadSubtract");
        numpad_add_is_keypad_add: NumpadAdd => Some("Key:KeypadAdd");
        numpad_enter_is_keypad_enter: NumpadEnter => Some("Key:KeypadEnter");
        numpad_equal_is_keypad_equal: NumpadEqual => Some("Key:KeypadEqual");
        intl_backslash_is_world_one: IntlBackslash => Some("Key:World1");
        intl_ro_is_world_two: IntlRo => Some("Key:World2");
        space_keeps_its_name: Space => Some("Key:Space");
        escape_keeps_its_name: Escape => Some("Key:Escape");
        enter_keeps_its_name: Enter => Some("Key:Enter");
        page_down_keeps_its_name: PageDown => Some("Key:PageDown");
        print_screen_keeps_its_name: PrintScreen => Some("Key:PrintScreen");
        caps_lock_keeps_its_name: CapsLock => Some("Key:CapsLock");
        backslash_keeps_its_name: Backslash => Some("Key:Backslash");
        pause_keeps_its_name: Pause => Some("Key:Pause");
        intl_yen_has_no_token: IntlYen => None;
        fn_key_has_no_token: Fn => None;
        fn_lock_has_no_token: FnLock => None;
        find_key_is_not_mistaken_for_a_function_key: Find => None;
        help_has_no_token: Help => None;
        numpad_comma_has_no_token: NumpadComma => None;
        numpad_backspace_has_no_token: NumpadBackspace => None;
        media_key_has_no_token: MediaPlayPause => None;
        browser_key_has_no_token: BrowserBack => None;
        power_has_no_token: Power => None;
        meta_has_no_token: Meta => None;
        kana_mode_has_no_token: KanaMode => None;
    }

    /// Every winit 0.30 `KeyCode` variant.
    const ALL_KEYS: &[KeyCode] = &[
        KeyCode::Backquote,
        KeyCode::Backslash,
        KeyCode::BracketLeft,
        KeyCode::BracketRight,
        KeyCode::Comma,
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Equal,
        KeyCode::IntlBackslash,
        KeyCode::IntlRo,
        KeyCode::IntlYen,
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyC,
        KeyCode::KeyD,
        KeyCode::KeyE,
        KeyCode::KeyF,
        KeyCode::KeyG,
        KeyCode::KeyH,
        KeyCode::KeyI,
        KeyCode::KeyJ,
        KeyCode::KeyK,
        KeyCode::KeyL,
        KeyCode::KeyM,
        KeyCode::KeyN,
        KeyCode::KeyO,
        KeyCode::KeyP,
        KeyCode::KeyQ,
        KeyCode::KeyR,
        KeyCode::KeyS,
        KeyCode::KeyT,
        KeyCode::KeyU,
        KeyCode::KeyV,
        KeyCode::KeyW,
        KeyCode::KeyX,
        KeyCode::KeyY,
        KeyCode::KeyZ,
        KeyCode::Minus,
        KeyCode::Period,
        KeyCode::Quote,
        KeyCode::Semicolon,
        KeyCode::Slash,
        KeyCode::AltLeft,
        KeyCode::AltRight,
        KeyCode::Backspace,
        KeyCode::CapsLock,
        KeyCode::ContextMenu,
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::Enter,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
        KeyCode::ShiftLeft,
        KeyCode::ShiftRight,
        KeyCode::Space,
        KeyCode::Tab,
        KeyCode::Convert,
        KeyCode::KanaMode,
        KeyCode::Lang1,
        KeyCode::Lang2,
        KeyCode::Lang3,
        KeyCode::Lang4,
        KeyCode::Lang5,
        KeyCode::NonConvert,
        KeyCode::Delete,
        KeyCode::End,
        KeyCode::Help,
        KeyCode::Home,
        KeyCode::Insert,
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::ArrowDown,
        KeyCode::ArrowLeft,
        KeyCode::ArrowRight,
        KeyCode::ArrowUp,
        KeyCode::NumLock,
        KeyCode::Numpad0,
        KeyCode::Numpad1,
        KeyCode::Numpad2,
        KeyCode::Numpad3,
        KeyCode::Numpad4,
        KeyCode::Numpad5,
        KeyCode::Numpad6,
        KeyCode::Numpad7,
        KeyCode::Numpad8,
        KeyCode::Numpad9,
        KeyCode::NumpadAdd,
        KeyCode::NumpadBackspace,
        KeyCode::NumpadClear,
        KeyCode::NumpadClearEntry,
        KeyCode::NumpadComma,
        KeyCode::NumpadDecimal,
        KeyCode::NumpadDivide,
        KeyCode::NumpadEnter,
        KeyCode::NumpadEqual,
        KeyCode::NumpadHash,
        KeyCode::NumpadMemoryAdd,
        KeyCode::NumpadMemoryClear,
        KeyCode::NumpadMemoryRecall,
        KeyCode::NumpadMemoryStore,
        KeyCode::NumpadMemorySubtract,
        KeyCode::NumpadMultiply,
        KeyCode::NumpadParenLeft,
        KeyCode::NumpadParenRight,
        KeyCode::NumpadStar,
        KeyCode::NumpadSubtract,
        KeyCode::Escape,
        KeyCode::Fn,
        KeyCode::FnLock,
        KeyCode::PrintScreen,
        KeyCode::ScrollLock,
        KeyCode::Pause,
        KeyCode::BrowserBack,
        KeyCode::BrowserFavorites,
        KeyCode::BrowserForward,
        KeyCode::BrowserHome,
        KeyCode::BrowserRefresh,
        KeyCode::BrowserSearch,
        KeyCode::BrowserStop,
        KeyCode::Eject,
        KeyCode::LaunchApp1,
        KeyCode::LaunchApp2,
        KeyCode::LaunchMail,
        KeyCode::MediaPlayPause,
        KeyCode::MediaSelect,
        KeyCode::MediaStop,
        KeyCode::MediaTrackNext,
        KeyCode::MediaTrackPrevious,
        KeyCode::Power,
        KeyCode::Sleep,
        KeyCode::AudioVolumeDown,
        KeyCode::AudioVolumeMute,
        KeyCode::AudioVolumeUp,
        KeyCode::WakeUp,
        KeyCode::Meta,
        KeyCode::Hyper,
        KeyCode::Turbo,
        KeyCode::Abort,
        KeyCode::Resume,
        KeyCode::Suspend,
        KeyCode::Again,
        KeyCode::Copy,
        KeyCode::Cut,
        KeyCode::Find,
        KeyCode::Open,
        KeyCode::Paste,
        KeyCode::Props,
        KeyCode::Select,
        KeyCode::Undo,
        KeyCode::Hiragana,
        KeyCode::Katakana,
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
        KeyCode::F13,
        KeyCode::F14,
        KeyCode::F15,
        KeyCode::F16,
        KeyCode::F17,
        KeyCode::F18,
        KeyCode::F19,
        KeyCode::F20,
        KeyCode::F21,
        KeyCode::F22,
        KeyCode::F23,
        KeyCode::F24,
        KeyCode::F25,
        KeyCode::F26,
        KeyCode::F27,
        KeyCode::F28,
        KeyCode::F29,
        KeyCode::F30,
        KeyCode::F31,
        KeyCode::F32,
        KeyCode::F33,
        KeyCode::F34,
        KeyCode::F35,
    ];

    #[test]
    fn every_emitted_token_is_a_registered_binding() {
        let registry = engine_project::input::binding_registry();
        for code in ALL_KEYS {
            if let Some(token) = key_token(*code) {
                assert!(
                    registry.contains(&token),
                    "{code:?} -> unregistered {token}"
                );
            }
        }
    }

    #[test]
    fn no_two_physical_keys_share_a_token() {
        let mut seen = BTreeSet::new();
        for code in ALL_KEYS {
            if let Some(token) = key_token(*code) {
                assert!(seen.insert(token.clone()), "{code:?} duplicates {token}");
            }
        }
    }

    #[test]
    fn every_registered_keyboard_token_has_a_physical_key() {
        let produced: BTreeSet<_> = ALL_KEYS.iter().filter_map(|c| key_token(*c)).collect();
        let missing: Vec<_> = engine_project::input::binding_registry()
            .into_iter()
            .filter(|t| t.starts_with("Key:") && !produced.contains(t))
            .collect();
        assert!(
            missing.is_empty(),
            "unreachable keyboard bindings: {missing:?}"
        );
    }

    #[test]
    fn key_table_is_complete_for_unmapped_keys() {
        let unmapped = ALL_KEYS.iter().filter(|c| key_token(**c).is_none()).count();
        // 25 function keys + 26 letters + 10 digits + ... are mapped; the rest are media/IME keys.
        assert!(unmapped > 60, "{unmapped}");
        assert!(ALL_KEYS.len() > 190);
    }

    fn id() -> DeviceId {
        DeviceId::dummy()
    }
    fn click(button: MouseButton, state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: id(),
            state,
            button,
        }
    }
    fn apply(raw: &mut RawInput, event: &WindowEvent) -> [f32; 2] {
        let mut pointer = [-1.0; 2];
        window_input(raw, event, &mut pointer);
        pointer
    }

    macro_rules! mouse_cases {
        ($($name:ident: $button:expr => $token:expr;)*) => {$(
            #[test]
            fn $name() {
                let mut raw = RawInput::default();
                apply(&mut raw, &click($button, ElementState::Pressed));
                assert_eq!(raw.down, BTreeSet::from([$token.to_string()]));
                apply(&mut raw, &click($button, ElementState::Released));
                assert!(raw.down.is_empty());
            }
        )*};
    }

    mouse_cases! {
        left_button_press_and_release: MouseButton::Left => "Mouse:Left";
        right_button_press_and_release: MouseButton::Right => "Mouse:Right";
        middle_button_press_and_release: MouseButton::Middle => "Mouse:Middle";
        back_button_is_mouse_four: MouseButton::Back => "Mouse:4";
        forward_button_is_mouse_five: MouseButton::Forward => "Mouse:5";
        other_button_uses_its_number: MouseButton::Other(7) => "Mouse:7";
    }

    #[test]
    fn mouse_buttons_are_tracked_independently() {
        let mut raw = RawInput::default();
        apply(&mut raw, &click(MouseButton::Left, ElementState::Pressed));
        apply(&mut raw, &click(MouseButton::Right, ElementState::Pressed));
        apply(&mut raw, &click(MouseButton::Left, ElementState::Released));
        assert_eq!(raw.down, BTreeSet::from(["Mouse:Right".to_string()]));
    }

    #[test]
    fn repeated_press_is_idempotent_and_spurious_release_is_harmless() {
        let mut raw = RawInput::default();
        apply(&mut raw, &click(MouseButton::Left, ElementState::Released));
        assert!(raw.down.is_empty());
        apply(&mut raw, &click(MouseButton::Left, ElementState::Pressed));
        apply(&mut raw, &click(MouseButton::Left, ElementState::Pressed));
        assert_eq!(raw.down.len(), 1);
    }

    #[test]
    fn cursor_motion_updates_only_the_physical_pointer() {
        let mut raw = RawInput::default();
        let event = WindowEvent::CursorMoved {
            device_id: id(),
            position: PhysicalPosition::new(123.5, 45.25),
        };
        assert_eq!(apply(&mut raw, &event), [123.5, 45.25]);
        assert_eq!(raw.pointer, [0.0; 2], "logical pointer is derived per tick");
        assert!(raw.down.is_empty());
    }

    #[test]
    fn cursor_outside_the_window_keeps_negative_coordinates() {
        let mut raw = RawInput::default();
        let event = WindowEvent::CursorMoved {
            device_id: id(),
            position: PhysicalPosition::new(-12.0, -3.5),
        };
        assert_eq!(apply(&mut raw, &event), [-12.0, -3.5]);
    }

    fn wheel(delta: MouseScrollDelta) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: id(),
            delta,
            phase: TouchPhase::Moved,
        }
    }

    #[test]
    fn line_wheel_deltas_are_passed_through() {
        let mut raw = RawInput::default();
        apply(&mut raw, &wheel(MouseScrollDelta::LineDelta(1.0, -2.0)));
        assert_eq!(raw.wheel, [1.0, -2.0]);
    }

    #[test]
    fn pixel_wheel_deltas_are_forty_pixels_per_line() {
        let mut raw = RawInput::default();
        apply(
            &mut raw,
            &wheel(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                80.0, -20.0,
            ))),
        );
        assert_eq!(raw.wheel, [2.0, -0.5]);
    }

    #[test]
    fn wheel_deltas_accumulate_until_sampled() {
        let mut raw = RawInput::default();
        apply(&mut raw, &wheel(MouseScrollDelta::LineDelta(0.0, 1.0)));
        apply(&mut raw, &wheel(MouseScrollDelta::LineDelta(0.5, 1.0)));
        apply(
            &mut raw,
            &wheel(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                0.0, 40.0,
            ))),
        );
        assert_eq!(raw.wheel, [0.5, 3.0]);
    }

    #[test]
    fn losing_focus_releases_keyboard_and_mouse_but_keeps_gamepad_state() {
        let mut raw = RawInput::default();
        raw.down.extend([
            "Key:W".to_string(),
            "Mouse:Left".to_string(),
            "Gamepad:A".to_string(),
        ]);
        raw.wheel = [3.0, 4.0];
        apply(&mut raw, &WindowEvent::Focused(false));
        assert_eq!(raw.down, BTreeSet::from(["Gamepad:A".to_string()]));
        assert_eq!(raw.wheel, [0.0, 0.0]);
        assert!(!raw.focused);
    }

    #[test]
    fn regaining_focus_only_sets_the_flag() {
        let mut raw = RawInput {
            focused: false,
            ..Default::default()
        };
        raw.down.insert("Key:W".into());
        raw.wheel = [1.0, 1.0];
        apply(&mut raw, &WindowEvent::Focused(true));
        assert!(raw.focused);
        assert_eq!(raw.down.len(), 1);
        assert_eq!(raw.wheel, [1.0, 1.0]);
    }

    #[test]
    fn unrelated_window_events_leave_input_untouched() {
        let mut raw = RawInput::default();
        raw.down.insert("Key:A".into());
        raw.wheel = [1.0, 2.0];
        let before = (raw.down.clone(), raw.wheel, raw.focused);
        for event in [
            WindowEvent::Occluded(true),
            WindowEvent::CloseRequested,
            WindowEvent::Destroyed,
        ] {
            assert_eq!(apply(&mut raw, &event), [-1.0, -1.0]);
        }
        assert_eq!((raw.down.clone(), raw.wheel, raw.focused), before);
    }

    #[test]
    fn set_down_adds_and_removes_tokens() {
        let mut raw = RawInput::default();
        set_down(&mut raw, "Key:Q".into(), true);
        set_down(&mut raw, "Key:Q".into(), true);
        assert_eq!(raw.down.len(), 1);
        set_down(&mut raw, "Key:Q".into(), false);
        set_down(&mut raw, "Key:Q".into(), false);
        assert!(raw.down.is_empty());
    }
}
