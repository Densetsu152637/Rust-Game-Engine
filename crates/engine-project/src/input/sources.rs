//! Native peripheral contracts. Samples are complete state, never unbounded event queues.
use super::*;

pub const MAX_INPUT_DEVICES: usize = 32;
pub const MAX_DEVICE_CONTROLS: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlKind {
    Button,
    Axis { deadzone: f32 },
}
impl ControlKind {
    pub(super) fn normalized(&self, value: f32) -> f32 {
        match self {
            Self::Button => {
                if value > 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Axis { deadzone } => {
                value.signum() * ((value.abs() - deadzone) / (1.0 - deadzone)).clamp(0.0, 1.0)
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeviceDescriptor {
    /// Stable application-selected ID; reconnecting hardware retains this ID.
    pub id: String,
    pub label: String,
    pub controls: BTreeMap<String, ControlKind>,
}
pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}
impl DeviceDescriptor {
    pub(super) fn validate(&self) -> Result<()> {
        if !valid_name(&self.id)
            || !valid_name(&self.label)
            || self.controls.is_empty()
            || self.controls.len() > MAX_DEVICE_CONTROLS
        {
            return Err(Error::new(
                "input.device.invalid",
                "Device requires bounded names and 1..128 controls",
            ));
        }
        for (name, kind) in &self.controls {
            if !valid_name(name)
                || matches!(kind, ControlKind::Axis { deadzone } if !deadzone.is_finite() || !(0.0..1.0).contains(deadzone))
            {
                return Err(Error::new(
                    "input.control.invalid",
                    "Control requires bounded name and finite deadzone in [0,1)",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeviceSample {
    connected: bool,
    values: BTreeMap<String, f32>,
}
impl DeviceSample {
    pub fn connected() -> Self {
        Self {
            connected: true,
            ..Default::default()
        }
    }
    pub fn disconnected() -> Self {
        Self::default()
    }
    /// Values are signed [-1,1]; omitted controls are neutral. Nonfinite input is rejected.
    pub fn set(&mut self, control: impl Into<String>, value: f32) -> Result<()> {
        let control = control.into();
        if !valid_name(&control)
            || !value.is_finite()
            || (!self.values.contains_key(&control) && self.values.len() >= MAX_DEVICE_CONTROLS)
        {
            return Err(Error::new(
                "input.sample.invalid",
                "Sample requires finite values and at most 128 bounded control names",
            ));
        }
        self.values.insert(control, value.clamp(-1.0, 1.0));
        Ok(())
    }
    pub(super) fn validate(&self, descriptor: &DeviceDescriptor) -> Result<()> {
        if self
            .values
            .keys()
            .any(|name| !descriptor.controls.contains_key(name))
        {
            return Err(Error::new(
                "input.control.unknown",
                "Sample contains an undeclared control",
            ));
        }
        Ok(())
    }
    pub(super) fn into_parts(self) -> (bool, BTreeMap<String, f32>) {
        (self.connected, self.values)
    }
}

/// A native adapter is polled once per Runtime tick, including headless ticks.
/// Implementations must be nonblocking. The engine bounds samples, not time spent
/// inside native driver code. Returned failures disconnect that source for the tick
/// and remain visible in InputDevices::snapshot; native panics are not caught.
pub trait InputSource: Send + Sync + 'static {
    fn descriptor(&self) -> DeviceDescriptor;
    fn poll(&mut self) -> Result<DeviceSample>;
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AxisDirection {
    #[default]
    Positive,
    Negative,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlBinding {
    pub device: String,
    pub control: String,
    #[serde(default)]
    pub direction: AxisDirection,
}
pub type DeviceBindings = BTreeMap<String, Vec<ControlBinding>>;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionBehavior {
    #[default]
    Hold,
    Toggle,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceStatus {
    pub descriptor: DeviceDescriptor,
    pub connected: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct InputDevicesSnapshot {
    pub devices: Vec<DeviceStatus>,
    pub bindings: DeviceBindings,
    pub behaviors: BTreeMap<String, ActionBehavior>,
}
