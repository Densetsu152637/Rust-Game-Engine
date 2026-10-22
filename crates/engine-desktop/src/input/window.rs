//! Window events folded into [`RawInput`].

use super::key_token;
use engine_project::input::RawInput;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::PhysicalKey,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use winit::{
        dpi::PhysicalPosition,
        event::{DeviceId, TouchPhase},
    };

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
