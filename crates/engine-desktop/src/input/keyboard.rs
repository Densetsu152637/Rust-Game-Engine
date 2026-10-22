//! Physical key translation.

use winit::keyboard::KeyCode;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

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
}
