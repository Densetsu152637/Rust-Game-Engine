//! Steam Input as an engine input source.
//!
//! [`SteamInputSource`] implements `engine_project::input::InputSource`. Register it with
//! `register_input_source(world, source)`; the engine then polls it once per tick, and the
//! controls it declares bind to engine actions with `InputDevices::set_bindings`:
//!
//! - a digital action `jump` is the button control `jump`;
//! - an analog action `move` is the axis controls `move.x` and `move.y` (Steam reports
//!   `y` positive for up/forward; bind `AxisDirection::Negative` for the other way).
//!
//! Every poll: `RunFrame`, list controllers (raising connect/disconnect events and giving
//! new controllers the active action set), then read each declared action on each
//! controller. Several controllers merge: a button is down if any controller holds it,
//! an axis takes the largest magnitude.

use std::collections::BTreeSet;
use std::path::PathBuf;

use engine_project::input::{
    ControlKind, DeviceDescriptor, DeviceSample, InputSource, MAX_DEVICE_CONTROLS,
};

use crate::error::{SteamError, SteamResult};
use crate::session::{SessionState, SteamSession};
use crate::types::{ActionGlyph, ControllerId, InputActionKind, SteamEvent};

/// The device id [`SteamInputSource`] registers under (bindings name it).
pub const STEAM_INPUT_DEVICE_ID: &str = "steam_input";

/// Dead zone declared for analog controls. Steam applies the player's own dead zone in
/// its controller configuration, so the engine adds only a small one.
const AXIS_DEADZONE: f32 = 0.05;

/// Which Steam Input actions the source exposes. Names must match the action manifest
/// (`game_actions_<appid>.vdf`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteamInputLayout {
    digital: Vec<String>,
    analog: Vec<String>,
    manifest: Option<PathBuf>,
}

impl SteamInputLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exposes a digital (button) action as the control `name`.
    pub fn digital(mut self, name: &str) -> Self {
        self.digital.push(name.to_owned());
        self
    }

    /// Exposes an analog action as the controls `name.x` and `name.y`.
    pub fn analog(mut self, name: &str) -> Self {
        self.analog.push(name.to_owned());
        self
    }

    /// Loads a local action manifest before Steam Input starts. For development only: a
    /// shipped game gets its manifest from the app's Steamworks configuration.
    pub fn with_manifest(mut self, path: impl Into<PathBuf>) -> Self {
        self.manifest = Some(path.into());
        self
    }

    /// The control name of an analog action's axis (`axis` is `"x"` or `"y"`).
    pub fn analog_control(action: &str, axis: &str) -> String {
        format!("{action}.{axis}")
    }

    fn validate(&self) -> SteamResult<()> {
        let mut controls = BTreeSet::new();
        let mut insert = |name: String| -> SteamResult<()> {
            if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                return Err(SteamError::InvalidName(name));
            }
            if controls.insert(name.clone()) {
                Ok(())
            } else {
                Err(SteamError::Rejected(format!("duplicate control {name:?}")))
            }
        };
        for name in &self.digital {
            insert(name.clone())?;
        }
        for name in &self.analog {
            insert(Self::analog_control(name, "x"))?;
            insert(Self::analog_control(name, "y"))?;
        }
        if controls.is_empty() || controls.len() > MAX_DEVICE_CONTROLS {
            return Err(SteamError::Rejected(format!(
                "a Steam Input layout needs 1..={MAX_DEVICE_CONTROLS} controls"
            )));
        }
        Ok(())
    }
}

fn engine_error(error: SteamError) -> engine_project::Error {
    engine_project::Error::new("steam.input", error.to_string())
}

/// Feeds Steam Input actions to the engine's `InputDevices`.
pub struct SteamInputSource {
    session: SteamSession,
    layout: SteamInputLayout,
}

impl SteamInputSource {
    /// Validates the layout. Steam need not be running yet: until it is, the device
    /// polls as disconnected.
    pub fn new(session: SteamSession, layout: SteamInputLayout) -> SteamResult<Self> {
        layout.validate()?;
        Ok(Self { session, layout })
    }
}

impl InputSource for SteamInputSource {
    fn descriptor(&self) -> DeviceDescriptor {
        let mut controls = std::collections::BTreeMap::new();
        for name in &self.layout.digital {
            controls.insert(name.clone(), ControlKind::Button);
        }
        for name in &self.layout.analog {
            for axis in ["x", "y"] {
                controls.insert(
                    SteamInputLayout::analog_control(name, axis),
                    ControlKind::Axis {
                        deadzone: AXIS_DEADZONE,
                    },
                );
            }
        }
        DeviceDescriptor {
            id: STEAM_INPUT_DEVICE_ID.into(),
            label: "Steam Input".into(),
            controls,
        }
    }

    fn poll(&mut self) -> engine_project::Result<DeviceSample> {
        let mut state = self.session.lock();
        let SessionState {
            backend,
            input,
            pending,
            ..
        } = &mut *state;
        match backend.input_init(self.layout.manifest.as_deref()) {
            Ok(()) => {}
            // Without Steam the device is simply absent.
            Err(SteamError::Unavailable(_)) => return Ok(DeviceSample::disconnected()),
            Err(error) => return Err(engine_error(error)),
        }
        backend.input_run_frame().map_err(engine_error)?;
        let controllers = backend.controllers().map_err(engine_error)?;

        let current: BTreeSet<ControllerId> = controllers.iter().copied().collect();
        let gone: Vec<_> = input.known.difference(&current).copied().collect();
        let new: Vec<_> = current.difference(&input.known).copied().collect();
        // Record the controllers first: whatever happens below, each change is announced
        // exactly once.
        input.known = current;
        for controller in gone {
            pending.push(SteamEvent::ControllerDisconnected(controller));
        }
        let mut set_error = None;
        for controller in new {
            pending.push(SteamEvent::ControllerConnected(controller));
            if let Some(set) = input.action_set.clone()
                && let Err(error) = backend.activate_action_set(controller, &set)
            {
                // A set Steam does not know (a typo requested before the first poll) is
                // dropped instead of failing every tick; the failure is reported once.
                input.action_set = None;
                set_error.get_or_insert(error);
            }
        }
        if let Some(error) = set_error {
            return Err(engine_error(error));
        }
        if controllers.is_empty() {
            return Ok(DeviceSample::disconnected());
        }

        let mut sample = DeviceSample::connected();
        let invalid = |name: &str| {
            engine_project::Error::new("steam.input", format!("invalid control {name:?}"))
        };
        for name in &self.layout.digital {
            let mut down = false;
            for controller in &controllers {
                let data = backend
                    .input_digital(*controller, name)
                    .map_err(engine_error)?;
                down |= data.active && data.down;
            }
            if down {
                sample.set(name.clone(), 1.0).map_err(|_| invalid(name))?;
            }
        }
        for name in &self.layout.analog {
            let (mut x, mut y) = (0.0_f32, 0.0_f32);
            for controller in &controllers {
                let data = backend
                    .input_analog(*controller, name)
                    .map_err(engine_error)?;
                if data.active {
                    if data.x.abs() > x.abs() {
                        x = data.x;
                    }
                    if data.y.abs() > y.abs() {
                        y = data.y;
                    }
                }
            }
            for (axis, value) in [("x", x), ("y", y)] {
                if value != 0.0 && value.is_finite() {
                    sample
                        .set(SteamInputLayout::analog_control(name, axis), value)
                        .map_err(|_| invalid(name))?;
                }
            }
        }
        Ok(sample)
    }
}

/// What the session remembers about Steam Input between polls.
#[derive(Debug, Default)]
pub(crate) struct InputState {
    /// The action set new and existing controllers should have active.
    pub(crate) action_set: Option<String>,
    /// Controllers seen by the last poll, for connect/disconnect events.
    pub(crate) known: BTreeSet<ControllerId>,
}

impl SteamSession {
    /// Controllers Steam Input currently reports (empty until Steam Input is initialised
    /// by the first [`SteamInputSource`] poll).
    pub fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
        self.with_backend(|backend| backend.controllers())
    }

    /// Makes `set` the active action set on every connected controller, and on any
    /// controller that connects later. Action sets switch between contexts such as
    /// "menu" and "gameplay". An unknown set name is rejected and changes nothing.
    pub fn set_action_set(&self, set: &str) -> SteamResult<()> {
        let mut state = self.lock();
        let SessionState { backend, input, .. } = &mut *state;
        match backend.controllers() {
            Ok(controllers) => {
                for controller in controllers {
                    backend.activate_action_set(controller, set)?;
                }
            }
            Err(error @ SteamError::Unavailable(_)) => return Err(error),
            // Steam Input not started yet: the first poll applies the set.
            Err(_) => {}
        }
        input.action_set = Some(set.to_owned());
        Ok(())
    }

    /// The action set last requested with [`SteamSession::set_action_set`].
    pub fn action_set(&self) -> Option<String> {
        self.lock().input.action_set.clone()
    }

    /// The controls (names and glyph image paths) `action` is bound to in `set` on
    /// `controller`, for on-screen prompts that match the player's hardware.
    pub fn action_glyphs(
        &self,
        controller: ControllerId,
        set: &str,
        action: &str,
        kind: InputActionKind,
    ) -> SteamResult<Vec<ActionGlyph>> {
        self.with_backend(|backend| backend.input_glyphs(controller, set, action, kind))
    }
}
