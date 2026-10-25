//! Typed text and IME: winit keyboard and IME events translated into the game
//! UI's [`UiTextEvent`]s, plus the window-side IME state (allowed flag and
//! candidate-window anchor).
//!
//! The translation functions are pure so they can be tested without a window.
//! Events reach the UI only while a `TextInput` is focused
//! (`GameUi::text_input_focus`); the UI applies them on its next tick.
use crate::CameraView;
use engine_project::ui::Rect;
use engine_project::ui_tree::{
    GameUi, MAX_TEXT_EVENT_BYTES, UiEditKey, UiEditModifiers, UiPreedit, UiTextEvent,
};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{Ime, WindowEvent},
    keyboard::{Key, NamedKey},
    window::Window,
};

/// Keyboard modifiers relevant to editing shortcuts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// Command (macOS) / Windows key.
    pub logo: bool,
}

/// Modifier tracking for [`translate_window_event`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextInputState {
    pub modifiers: TextModifiers,
    /// Command shortcuts use Cmd and word motion uses Option, as on macOS.
    pub macos: bool,
}

impl TextInputState {
    /// State with the shortcut conventions of the platform this build targets.
    pub fn native() -> Self {
        Self {
            modifiers: TextModifiers::default(),
            macos: cfg!(target_os = "macos"),
        }
    }
}

/// One keyboard press, decomposed from a winit `KeyEvent` (whose constructor is
/// private to winit, so the pure translation takes these parts).
#[derive(Clone, Copy, Debug)]
pub struct KeyPress<'a> {
    pub logical: &'a Key,
    /// `KeyEvent::text`: what the key produced with all modifiers applied.
    pub text: Option<&'a str>,
    pub pressed: bool,
}

/// `text` truncated to the event size limit on a character boundary.
fn bounded(text: &str) -> &str {
    let mut end = text.len().min(MAX_TEXT_EVENT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Printable characters only: control characters (Enter's `\r`, Tab, Escape,
/// Backspace's `\u{8}`, Ctrl chords) never reach the UI as typed text.
fn printable(text: &str) -> String {
    bounded(&text.chars().filter(|c| !c.is_control()).collect::<String>()).to_owned()
}

fn editing_key(key: &Key, modifiers: &TextModifiers, macos: bool) -> Option<UiEditKey> {
    let command = if macos {
        modifiers.logo
    } else {
        modifiers.control
    };
    Some(match key {
        Key::Named(NamedKey::Backspace) => UiEditKey::Backspace,
        Key::Named(NamedKey::Delete) if modifiers.shift => UiEditKey::Cut,
        Key::Named(NamedKey::Delete) => UiEditKey::Delete,
        Key::Named(NamedKey::ArrowLeft) => UiEditKey::Left,
        Key::Named(NamedKey::ArrowRight) => UiEditKey::Right,
        Key::Named(NamedKey::ArrowUp) => UiEditKey::Up,
        Key::Named(NamedKey::ArrowDown) => UiEditKey::Down,
        Key::Named(NamedKey::Home) => UiEditKey::Home,
        Key::Named(NamedKey::End) => UiEditKey::End,
        Key::Named(NamedKey::Enter) => UiEditKey::Enter,
        Key::Named(NamedKey::Escape) => UiEditKey::Escape,
        Key::Named(NamedKey::Insert) if modifiers.shift => UiEditKey::Paste,
        Key::Named(NamedKey::Insert) if modifiers.control => UiEditKey::Copy,
        Key::Character(c) if command => match c.to_ascii_lowercase().as_str() {
            "a" => UiEditKey::SelectAll,
            "c" => UiEditKey::Copy,
            "x" => UiEditKey::Cut,
            "v" => UiEditKey::Paste,
            _ => return None,
        },
        _ => return None,
    })
}

/// Translate one key press into text events: an editing key (with shift and
/// word modifiers) or the printable text the key produced. Releases and
/// Ctrl/Cmd chords that are not editing shortcuts produce nothing; AltGr
/// (Ctrl+Alt on Windows) still types.
pub fn translate_key(
    press: &KeyPress<'_>,
    modifiers: &TextModifiers,
    macos: bool,
) -> Vec<UiTextEvent> {
    if !press.pressed {
        return vec![];
    }
    if let Some(key) = editing_key(press.logical, modifiers, macos) {
        let word = if macos {
            modifiers.alt
        } else {
            modifiers.control
        };
        return vec![UiTextEvent::Key {
            key,
            modifiers: UiEditModifiers {
                shift: modifiers.shift,
                word,
            },
        }];
    }
    let chord = if macos {
        modifiers.logo
    } else {
        modifiers.control && !modifiers.alt
    };
    if chord {
        return vec![];
    }
    match press.text.map(printable) {
        Some(text) if !text.is_empty() => vec![UiTextEvent::Insert(text)],
        _ => vec![],
    }
}

/// Translate an IME event. A preedit with an invalid cursor range keeps its
/// text but drops the cursor; `Disabled` clears any composition.
pub fn translate_ime(ime: &Ime) -> Vec<UiTextEvent> {
    match ime {
        Ime::Enabled => vec![],
        Ime::Disabled => vec![UiTextEvent::Preedit(UiPreedit::default())],
        Ime::Preedit(text, cursor) => {
            let shown = bounded(text);
            let cursor = cursor
                .filter(|(a, b)| a <= b && *b <= shown.len())
                .filter(|(a, b)| shown.is_char_boundary(*a) && shown.is_char_boundary(*b))
                .map(|(a, b)| [a, b]);
            vec![UiTextEvent::Preedit(UiPreedit {
                text: shown.to_owned(),
                cursor,
            })]
        }
        Ime::Commit(text) => {
            let text = printable(text);
            if text.is_empty() {
                vec![]
            } else {
                vec![UiTextEvent::Commit(text)]
            }
        }
    }
}

/// Translate a window event, tracking modifier state in `state`.
pub fn translate_window_event(event: &WindowEvent, state: &mut TextInputState) -> Vec<UiTextEvent> {
    match event {
        WindowEvent::ModifiersChanged(modifiers) => {
            let s = modifiers.state();
            state.modifiers = TextModifiers {
                shift: s.shift_key(),
                control: s.control_key(),
                alt: s.alt_key(),
                logo: s.super_key(),
            };
            vec![]
        }
        WindowEvent::KeyboardInput { event, .. } => translate_key(
            &KeyPress {
                logical: &event.logical_key,
                text: event.text.as_deref(),
                pressed: event.state.is_pressed(),
            },
            &state.modifiers,
            state.macos,
        ),
        WindowEvent::Ime(ime) => translate_ime(ime),
        _ => vec![],
    }
}

/// Where the platform should anchor its IME candidate window, in physical
/// window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImeCursorArea {
    pub position: [f64; 2],
    pub size: [f64; 2],
}

/// Map the UI's caret rectangle (logical UI pixels) through the camera fit to
/// physical window pixels.
pub fn ime_cursor_area(area: Rect, camera: &CameraView) -> ImeCursorArea {
    ImeCursorArea {
        position: [
            f64::from(camera.viewport.x) + f64::from(area.x) * camera.scale,
            f64::from(camera.viewport.y) + f64::from(area.y) * camera.scale,
        ],
        size: [
            (f64::from(area.width) * camera.scale).max(1.0),
            (f64::from(area.height) * camera.scale).max(1.0),
        ],
    }
}

/// Window-side text input state: modifier tracking and the IME flags last
/// sent to the window, so the platform is only told about changes.
#[derive(Debug)]
pub(crate) struct TextBridge {
    state: TextInputState,
    ime_allowed: bool,
    area: Option<ImeCursorArea>,
}

impl Default for TextBridge {
    fn default() -> Self {
        Self {
            state: TextInputState::native(),
            ime_allowed: false,
            area: None,
        }
    }
}

impl TextBridge {
    /// Forward a window event to the UI. Typed text and IME events are queued
    /// only while a text input is focused; a rejected event (a full queue) is
    /// dropped, never fatal.
    pub fn handle(&mut self, event: &WindowEvent, ui: &mut GameUi) {
        let events = translate_window_event(event, &mut self.state);
        if events.is_empty() || ui.text_input_focus().is_none() {
            return;
        }
        for event in events {
            if ui.push_text_event(event).is_err() {
                break;
            }
        }
    }

    /// Enable or disable the IME and move its candidate window to follow the
    /// focused input's caret. Call after the UI tick.
    pub fn sync(&mut self, window: &Window, ui: &GameUi, camera: &CameraView) {
        let focused = ui.text_input_focus().is_some();
        if focused != self.ime_allowed {
            window.set_ime_allowed(focused);
            self.ime_allowed = focused;
            self.area = None;
        }
        let area = focused
            .then(|| ui.ime_area())
            .flatten()
            .map(|area| ime_cursor_area(area, camera));
        if let Some(area) = area
            && self.area != Some(area)
        {
            window.set_ime_cursor_area(
                PhysicalPosition::new(area.position[0], area.position[1]),
                PhysicalSize::new(area.size[0], area.size[1]),
            );
            self.area = Some(area);
        }
    }
}

/// The system clipboard behind the `system-clipboard` cargo feature (arboard,
/// MIT OR Apache-2.0). Without the feature the UI keeps its process-local
/// in-memory clipboard.
#[cfg(feature = "system-clipboard")]
pub mod clipboard {
    use engine_project::ui_tree::UiClipboard;
    use std::sync::{Arc, Mutex};

    pub struct SystemClipboard(Mutex<arboard::Clipboard>);

    impl UiClipboard for SystemClipboard {
        fn get(&self) -> Option<String> {
            let text = self.0.lock().ok()?.get_text().ok()?;
            (!text.is_empty()).then_some(text)
        }
        fn set(&self, text: &str) {
            if let Ok(mut clipboard) = self.0.lock() {
                let _ = clipboard.set_text(text.to_owned());
            }
        }
    }

    /// `None` when the platform clipboard cannot be opened (the in-memory
    /// clipboard stays in place).
    pub fn system_clipboard() -> Option<Arc<dyn UiClipboard>> {
        let clipboard = arboard::Clipboard::new().ok()?;
        Some(Arc::new(SystemClipboard(Mutex::new(clipboard))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::Camera2D;
    use winit::{event::Modifiers, keyboard::ModifiersState};

    const NONE: TextModifiers = TextModifiers {
        shift: false,
        control: false,
        alt: false,
        logo: false,
    };

    fn character(c: &str) -> Key {
        Key::Character(c.into())
    }
    fn press(
        key: &Key,
        text: Option<&str>,
        modifiers: TextModifiers,
        macos: bool,
    ) -> Vec<UiTextEvent> {
        translate_key(
            &KeyPress {
                logical: key,
                text,
                pressed: true,
            },
            &modifiers,
            macos,
        )
    }
    fn insert(text: &str) -> Vec<UiTextEvent> {
        vec![UiTextEvent::Insert(text.into())]
    }
    fn edit(key: UiEditKey, shift: bool, word: bool) -> Vec<UiTextEvent> {
        vec![UiTextEvent::Key {
            key,
            modifiers: UiEditModifiers { shift, word },
        }]
    }

    #[test]
    fn printable_text_becomes_insert_events() {
        assert_eq!(press(&character("a"), Some("a"), NONE, false), insert("a"));
        assert_eq!(
            press(&character("\u{e9}"), Some("\u{e9}"), NONE, false),
            insert("\u{e9}")
        );
        assert_eq!(
            press(
                &character("A"),
                Some("A"),
                TextModifiers {
                    shift: true,
                    ..NONE
                },
                false
            ),
            insert("A")
        );
        // Multi-character key output (dead keys, compose) arrives whole.
        assert_eq!(
            press(&character("^"), Some("^e"), NONE, false),
            insert("^e")
        );
        assert_eq!(
            press(&Key::Named(NamedKey::Space), Some(" "), NONE, false),
            insert(" ")
        );
    }

    #[test]
    fn control_characters_and_missing_text_are_filtered() {
        for text in ["\r", "\u{8}", "\t", "\u{1b}", "\u{1}", "\u{7f}", "\n"] {
            assert!(
                press(&Key::Named(NamedKey::Tab), Some(text), NONE, false).is_empty(),
                "{text:?}"
            );
        }
        assert!(press(&Key::Named(NamedKey::Shift), None, NONE, false).is_empty());
        // Control characters inside otherwise printable text are dropped.
        assert_eq!(
            press(&character("a"), Some("a\u{7}b"), NONE, false),
            insert("ab")
        );
        // Releases never type.
        let release = KeyPress {
            logical: &character("a"),
            text: Some("a"),
            pressed: false,
        };
        assert!(translate_key(&release, &NONE, false).is_empty());
    }

    #[test]
    fn oversized_text_is_cut_on_a_character_boundary() {
        let long = "\u{e9}".repeat(MAX_TEXT_EVENT_BYTES);
        let events = press(&character("x"), Some(&long), NONE, false);
        let [UiTextEvent::Insert(text)] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert!(text.len() <= MAX_TEXT_EVENT_BYTES && text.chars().all(|c| c == '\u{e9}'));
        assert!(!text.is_empty());
    }

    #[test]
    fn editing_keys_carry_shift_and_word_modifiers() {
        let cases = [
            (NamedKey::Backspace, UiEditKey::Backspace),
            (NamedKey::Delete, UiEditKey::Delete),
            (NamedKey::ArrowLeft, UiEditKey::Left),
            (NamedKey::ArrowRight, UiEditKey::Right),
            (NamedKey::ArrowUp, UiEditKey::Up),
            (NamedKey::ArrowDown, UiEditKey::Down),
            (NamedKey::Home, UiEditKey::Home),
            (NamedKey::End, UiEditKey::End),
            (NamedKey::Enter, UiEditKey::Enter),
            (NamedKey::Escape, UiEditKey::Escape),
        ];
        for (named, expected) in cases {
            let key = Key::Named(named);
            assert_eq!(
                press(&key, None, NONE, false),
                edit(expected, false, false),
                "{named:?}"
            );
            // Enter and Backspace also carry control text that must not be typed.
            assert_eq!(
                press(&key, Some("\r"), NONE, false),
                edit(expected, false, false)
            );
        }
        let shifted = TextModifiers {
            shift: true,
            ..NONE
        };
        assert_eq!(
            press(&Key::Named(NamedKey::ArrowRight), None, shifted, false),
            edit(UiEditKey::Right, true, false)
        );
        // Word motion: Ctrl on Windows/Linux, Option on macOS.
        let control = TextModifiers {
            control: true,
            ..NONE
        };
        assert_eq!(
            press(&Key::Named(NamedKey::Backspace), None, control, false),
            edit(UiEditKey::Backspace, false, true)
        );
        let option = TextModifiers { alt: true, ..NONE };
        assert_eq!(
            press(&Key::Named(NamedKey::ArrowLeft), None, option, true),
            edit(UiEditKey::Left, false, true)
        );
        assert_eq!(
            press(&Key::Named(NamedKey::ArrowLeft), None, option, false),
            edit(UiEditKey::Left, false, false)
        );
    }

    #[test]
    fn clipboard_and_select_all_shortcuts_follow_the_platform() {
        let control = TextModifiers {
            control: true,
            ..NONE
        };
        let logo = TextModifiers { logo: true, ..NONE };
        for (letter, expected) in [
            ("a", UiEditKey::SelectAll),
            ("c", UiEditKey::Copy),
            ("x", UiEditKey::Cut),
            ("v", UiEditKey::Paste),
            ("V", UiEditKey::Paste),
        ] {
            assert_eq!(
                press(&character(letter), Some("\u{1}"), control, false),
                edit(expected, false, true),
                "ctrl+{letter}"
            );
            assert_eq!(
                press(&character(letter), Some(letter), logo, true),
                edit(expected, false, false),
                "cmd+{letter}"
            );
        }
        // The wrong chord for the platform is not a shortcut and types nothing.
        assert!(
            press(&character("c"), Some("c"), logo, false)
                .iter()
                .all(|e| matches!(e, UiTextEvent::Insert(_)))
        );
        assert!(press(&character("c"), Some("\u{3}"), control, true).is_empty());
        assert!(press(&character("z"), Some("\u{1a}"), control, false).is_empty());
        // Legacy chords.
        assert_eq!(
            press(
                &Key::Named(NamedKey::Insert),
                None,
                TextModifiers {
                    shift: true,
                    ..NONE
                },
                false
            ),
            edit(UiEditKey::Paste, true, false)
        );
        assert_eq!(
            press(
                &Key::Named(NamedKey::Delete),
                None,
                TextModifiers {
                    shift: true,
                    ..NONE
                },
                false
            ),
            edit(UiEditKey::Cut, true, false)
        );
    }

    #[test]
    fn altgr_still_types_but_other_chords_do_not() {
        let altgr = TextModifiers {
            control: true,
            alt: true,
            ..NONE
        };
        assert_eq!(press(&character("@"), Some("@"), altgr, false), insert("@"));
        let alt = TextModifiers { alt: true, ..NONE };
        assert_eq!(
            press(&character("e"), Some("\u{e9}"), alt, true),
            insert("\u{e9}")
        );
        assert!(
            press(
                &character("e"),
                Some("e"),
                TextModifiers { logo: true, ..NONE },
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn ime_events_translate_to_preedit_and_commit() {
        assert!(translate_ime(&Ime::Enabled).is_empty());
        assert_eq!(
            translate_ime(&Ime::Preedit("\u{306b}\u{307b}".into(), Some((3, 6)))),
            vec![UiTextEvent::Preedit(UiPreedit {
                text: "\u{306b}\u{307b}".into(),
                cursor: Some([3, 6]),
            })]
        );
        // No cursor, or a cursor off a character boundary or out of range, keeps the text only.
        for cursor in [None, Some((1, 3)), Some((0, 99)), Some((6, 3))] {
            assert_eq!(
                translate_ime(&Ime::Preedit("\u{306b}\u{307b}".into(), cursor)),
                vec![UiTextEvent::Preedit(UiPreedit {
                    text: "\u{306b}\u{307b}".into(),
                    cursor: None,
                })],
                "{cursor:?}"
            );
        }
        // An empty preedit clears the composition.
        assert_eq!(
            translate_ime(&Ime::Preedit(String::new(), None)),
            vec![UiTextEvent::Preedit(UiPreedit::default())]
        );
        assert_eq!(
            translate_ime(&Ime::Commit("\u{65e5}\u{672c}".into())),
            vec![UiTextEvent::Commit("\u{65e5}\u{672c}".into())]
        );
        assert_eq!(
            translate_ime(&Ime::Disabled),
            vec![UiTextEvent::Preedit(UiPreedit::default())]
        );
        assert!(translate_ime(&Ime::Commit("\u{7}".into())).is_empty());
        // Every produced event is accepted by the UI queue.
        let mut ui = GameUi::default();
        for event in translate_ime(&Ime::Preedit("x".repeat(5000), Some((2, 4)))) {
            ui.push_text_event(event).unwrap();
        }
    }

    #[test]
    fn window_events_track_modifiers_and_route_ime() {
        let mut state = TextInputState {
            macos: false,
            ..Default::default()
        };
        let event = WindowEvent::ModifiersChanged(Modifiers::from(ModifiersState::CONTROL));
        assert!(translate_window_event(&event, &mut state).is_empty());
        assert!(state.modifiers.control && !state.modifiers.shift);
        let event = WindowEvent::ModifiersChanged(Modifiers::from(
            ModifiersState::SHIFT | ModifiersState::ALT,
        ));
        translate_window_event(&event, &mut state);
        assert_eq!(
            state.modifiers,
            TextModifiers {
                shift: true,
                alt: true,
                ..NONE
            }
        );
        let ime = WindowEvent::Ime(Ime::Commit("x".into()));
        assert_eq!(
            translate_window_event(&ime, &mut state),
            vec![UiTextEvent::Commit("x".into())]
        );
        assert!(translate_window_event(&WindowEvent::Focused(true), &mut state).is_empty());
    }

    #[test]
    fn ime_area_maps_logical_ui_pixels_to_physical_window_pixels() {
        // 320x180 logical camera in a 1280x800 window: 4x scale, letterboxed vertically.
        let camera = CameraView::new(&Camera2D::default(), 1280, 800).unwrap();
        assert_eq!(camera.scale, 4.0);
        let area = ime_cursor_area(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 2.0,
                height: 8.0,
            },
            &camera,
        );
        assert_eq!(area.position[0], 40.0);
        assert_eq!(area.position[1], f64::from(camera.viewport.y) + 80.0);
        assert_eq!(area.size, [8.0, 32.0]);
        // A zero-width caret still yields a non-empty anchor.
        let thin = ime_cursor_area(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            &camera,
        );
        assert_eq!(thin.size, [1.0, 1.0]);
    }

    #[test]
    fn bridge_forwards_events_only_while_an_input_is_focused() {
        use engine_project::{
            ActionFrame,
            ui_tree::{UiLength, UiNode, UiRoot, UiTree, UiWidget},
        };
        let node = UiNode::text_input("name", "", 32)
            .with_size(UiLength::Pixels(100.0), UiLength::Pixels(20.0));
        let mut roots = [(
            "hud".to_string(),
            UiRoot {
                tree: UiTree { nodes: vec![node] },
                ..Default::default()
            },
        )];
        let value = |roots: &[(String, UiRoot)]| match &roots[0].1.tree.nodes[0].widget {
            UiWidget::TextInput { text, .. } => text.clone(),
            _ => unreachable!(),
        };
        let mut bridge = TextBridge::default();
        let mut ui = GameUi::default();
        let commit = WindowEvent::Ime(Ime::Commit("hi".into()));
        let idle = ActionFrame::default();
        // Nothing focused: the typed text is not queued.
        ui.tick(&mut roots, [200, 100], &idle).unwrap();
        bridge.handle(&commit, &mut ui);
        ui.tick(&mut roots, [200, 100], &idle).unwrap();
        assert_eq!(value(&roots), "");
        // Click to focus, then the same event edits the input on the next tick.
        let click = ActionFrame {
            pressed: ["ui_click".to_string()].into(),
            pointer: [5.0, 5.0],
            ..Default::default()
        };
        ui.tick(&mut roots, [200, 100], &click).unwrap();
        assert!(ui.text_input_focus().is_some());
        bridge.handle(&commit, &mut ui);
        ui.tick(&mut roots, [200, 100], &idle).unwrap();
        assert_eq!(value(&roots), "hi");
        // A queue that overflows drops the rest instead of failing.
        for _ in 0..400 {
            bridge.handle(&commit, &mut ui);
        }
    }
}
