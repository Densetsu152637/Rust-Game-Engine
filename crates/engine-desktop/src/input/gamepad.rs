//! Polled gamepad state.

use engine_project::input::RawInput;

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
