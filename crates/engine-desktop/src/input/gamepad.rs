//! Polled gamepad state.

use engine_project::input::RawInput;

fn stable_selection<T: PartialEq>(previous: Option<T>, selected: Option<T>) -> bool {
    selected.is_some() && (previous.is_none() || previous == selected)
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
        let previous = self.active;
        if self
            .active
            .is_none_or(|id| !self.gilrs.gamepad(id).is_connected())
        {
            self.active = self.gilrs.gamepads().next().map(|(id, _)| id);
        }
        raw.down.retain(|k| !k.starts_with("Gamepad:"));
        raw.axes.clear();
        raw.gamepad_connected = stable_selection(previous, self.active);
        // Replacing the selected controller is a disconnect/reconnect boundary,
        // even when another controller is already available in the same poll.
        if !raw.gamepad_connected {
            return;
        }
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
    use engine_project::input::{ActionBehavior, ActionMapper, InputDevices};
    use std::collections::BTreeMap;

    #[test]
    fn selected_controller_replacement_cancels_and_quarantines_until_neutral() {
        let mut mapper =
            ActionMapper::new(BTreeMap::from([("jump".into(), vec!["Gamepad:A".into()])])).unwrap();
        let mut devices = InputDevices::default();
        devices
            .set_behavior("jump", ActionBehavior::Toggle)
            .unwrap();
        let mut raw = RawInput {
            gamepad_connected: stable_selection(None, Some(1_u32)),
            ..Default::default()
        };
        devices.poll_and_merge(mapper.sample(&raw));
        raw.down.insert("Gamepad:A".into());
        assert!(devices.poll_and_merge(mapper.sample(&raw)).pressed("jump"));
        // The replacement is already connected and held, but must not inherit the latch.
        raw.gamepad_connected = stable_selection(Some(1), Some(2));
        assert!(devices.poll_and_merge(mapper.sample(&raw)).released("jump"));
        raw.gamepad_connected = stable_selection(Some(2), Some(2));
        assert!(!devices.poll_and_merge(mapper.sample(&raw)).held("jump"));
        raw.down.clear();
        devices.poll_and_merge(mapper.sample(&raw));
        raw.down.insert("Gamepad:A".into());
        assert!(devices.poll_and_merge(mapper.sample(&raw)).pressed("jump"));
    }
}
