use super::*;
use bevy_ecs::prelude::World;

struct Device {
    source: Box<dyn InputSource>,
    status: DeviceStatus,
    values: BTreeMap<String, f32>,
    blocked: BTreeSet<String>,
}
impl Device {
    fn poll(&mut self, focused: bool) -> bool {
        let sample = self.source.poll().and_then(|sample| {
            sample.validate(&self.status.descriptor)?;
            Ok(sample)
        });
        let (connected, values) = match sample {
            Ok(sample) => {
                self.status.error = None;
                sample.into_parts()
            }
            Err(error) => {
                self.status.error = Some(error.to_string().chars().take(1024).collect());
                (false, BTreeMap::new())
            }
        };
        let disconnected = self.status.connected && !connected;
        self.values.clear();
        if connected {
            for (name, kind) in &self.status.descriptor.controls {
                let value = kind.normalized(values.get(name).copied().unwrap_or(0.0));
                if value == 0.0 {
                    self.blocked.remove(name);
                } else if !focused || !self.status.connected {
                    self.blocked.insert(name.clone());
                }
                self.values.insert(name.clone(), value);
            }
        } else {
            self.blocked.clear();
        }
        self.status.connected = connected;
        disconnected
    }
    fn value(&self, control: &str) -> f32 {
        if self.blocked.contains(control) {
            0.0
        } else {
            self.values.get(control).copied().unwrap_or(0.0)
        }
    }
    fn quarantine(&mut self) {
        self.blocked.extend(
            self.values
                .iter()
                .filter(|(_, value)| **value != 0.0)
                .map(|(name, _)| name.clone()),
        );
    }
}

/// Native custom-device mapping plus aggregate action edges/accessibility modes.
#[derive(Resource, Default)]
pub struct InputDevices {
    devices: BTreeMap<String, Device>,
    bindings: DeviceBindings,
    behaviors: BTreeMap<String, ActionBehavior>,
    toggled: BTreeSet<String>,
    previous_raw: BTreeSet<String>,
    previous_custom: BTreeSet<String>,
    previous_output: BTreeSet<String>,
    blocked_physical: BTreeSet<String>,
    blocked_actions: BTreeSet<String>,
    lost_actions: BTreeSet<String>,
    was_unfocused: bool,
    physical_bindings: Option<BindingMap>,
    physical_revision: Option<u64>,
    /// XR controls named by `XR:` tokens in the authored physical bindings.
    xr_bindings: DeviceBindings,
}
impl InputDevices {
    /// Also handles Runtime callers that update InputSettings without a desktop mapper.
    pub fn synchronize_physical_bindings(&mut self, bindings: &BindingMap) {
        if let Some(previous) = &self.physical_bindings {
            for action in previous.keys().chain(bindings.keys()) {
                if previous.get(action) != bindings.get(action) {
                    self.lost_actions.insert(action.clone());
                    self.blocked_physical.insert(action.clone());
                }
            }
        }
        self.physical_bindings = Some(bindings.clone());
        self.xr_bindings = super::xr::xr_device_bindings(bindings);
    }
    pub fn register(&mut self, source: impl InputSource) -> Result<()> {
        let descriptor = source.descriptor();
        descriptor.validate()?;
        if self.devices.len() >= MAX_INPUT_DEVICES || self.devices.contains_key(&descriptor.id) {
            return Err(Error::new(
                "input.device.limit",
                "At most 32 uniquely named input sources may be registered",
            ));
        }
        self.devices.insert(
            descriptor.id.clone(),
            Device {
                source: Box::new(source),
                status: DeviceStatus {
                    descriptor,
                    connected: false,
                    error: None,
                },
                values: BTreeMap::new(),
                blocked: BTreeSet::new(),
            },
        );
        Ok(())
    }
    /// Hardware hotplug uses disconnected samples. Unregister permanently removes mappings.
    pub fn unregister(&mut self, id: &str) -> bool {
        if self.devices.remove(id).is_none() {
            return false;
        }
        self.lost_actions.extend(
            self.bindings
                .iter()
                .chain(&self.xr_bindings)
                .filter(|(_, bindings)| bindings.iter().any(|binding| binding.device == id))
                .map(|(action, _)| action.clone()),
        );
        for bindings in self.bindings.values_mut() {
            bindings.retain(|binding| binding.device != id);
        }
        self.bindings.retain(|_, bindings| !bindings.is_empty());
        true
    }
    pub fn set_bindings(&mut self, bindings: DeviceBindings) -> Result<()> {
        if bindings.len() > 256 {
            return Err(Error::new(
                "input.bindings.limit",
                "At most 256 peripheral actions",
            ));
        }
        for (action, controls) in &bindings {
            if !sources::valid_name(action) || controls.is_empty() || controls.len() > 16 {
                return Err(Error::new(
                    "input.bindings.invalid",
                    "Action requires bounded name and 1..16 controls",
                ));
            }
            for control in controls {
                let kind = self
                    .devices
                    .get(&control.device)
                    .and_then(|d| d.status.descriptor.controls.get(&control.control))
                    .ok_or_else(|| {
                        Error::new(
                            "input.binding.unknown",
                            "Binding requires a registered device control",
                        )
                    })?;
                if matches!(kind, ControlKind::Button)
                    && control.direction == AxisDirection::Negative
                {
                    return Err(Error::new(
                        "input.binding.direction",
                        "A button only supports positive direction",
                    ));
                }
            }
        }
        self.bindings = bindings;
        self.toggled.clear();
        for device in self.devices.values_mut() {
            device.quarantine();
        }
        Ok(())
    }
    pub fn set_behavior(&mut self, action: &str, behavior: ActionBehavior) -> Result<()> {
        if !sources::valid_name(action)
            || (!self.behaviors.contains_key(action) && self.behaviors.len() >= 256)
        {
            return Err(Error::new(
                "input.behavior.invalid",
                "At most 256 bounded action names",
            ));
        }
        self.behaviors.insert(action.into(), behavior);
        self.toggled.remove(action);
        if self.previous_raw.contains(action) {
            self.blocked_actions.insert(action.into());
        }
        Ok(())
    }
    pub fn snapshot(&self) -> InputDevicesSnapshot {
        InputDevicesSnapshot {
            devices: self
                .devices
                .values()
                .map(|device| device.status.clone())
                .collect(),
            bindings: self.bindings.clone(),
            behaviors: self.behaviors.clone(),
        }
    }
    /// Adapter errors are isolated as neutral input, with diagnostics in snapshot().
    /// Edges are derived from the union, so releasing one source cannot release another.
    pub fn poll_and_merge(&mut self, mut physical: ActionFrame) -> ActionFrame {
        self.lost_actions.extend(physical.cancelled.iter().cloned());
        let mut disconnected = BTreeSet::new();
        for (id, device) in &mut self.devices {
            if device.poll(physical.focused && !self.was_unfocused) {
                disconnected.insert(id.clone());
            }
        }
        for (action, bindings) in self.bindings.iter().chain(&self.xr_bindings) {
            if bindings
                .iter()
                .any(|binding| disconnected.contains(&binding.device))
            {
                self.lost_actions.insert(action.clone());
            }
        }
        if !physical.focused {
            self.toggled.clear();
        }
        let clean = |value: f32| if value.is_finite() { value } else { 0.0 };
        physical.pointer = physical.pointer.map(clean);
        physical.wheel = physical.wheel.map(clean);
        physical
            .values
            .values_mut()
            .for_each(|v| *v = clean(*v).clamp(0.0, 1.0));
        if !physical.focused || self.was_unfocused {
            self.blocked_physical.extend(physical.held.iter().cloned());
        }
        self.blocked_physical
            .retain(|action| physical.held.contains(action));
        self.was_unfocused = !physical.focused;
        physical.held.retain(|a| !self.blocked_physical.contains(a));
        physical
            .pressed
            .retain(|a| !self.blocked_physical.contains(a));
        for action in &self.blocked_physical {
            physical.values.insert(action.clone(), 0.0);
        }
        let mut custom_held = BTreeSet::new();
        if physical.focused {
            for (action, bindings) in self.bindings.iter().chain(&self.xr_bindings) {
                let value = bindings
                    .iter()
                    .filter_map(|binding| {
                        self.devices.get(&binding.device).map(|device| {
                            let v = device.value(&binding.control);
                            if binding.direction == AxisDirection::Negative {
                                -v
                            } else {
                                v
                            }
                        })
                    })
                    .fold(0.0_f32, f32::max);
                let total = physical.values.entry(action.clone()).or_default();
                *total = total.max(value);
                if value > 0.0 {
                    custom_held.insert(action.clone());
                    physical.held.insert(action.clone());
                }
            }
        } else {
            physical.held.clear();
            physical.pressed.clear();
            physical.values.clear();
            physical.wheel = [0.0; 2];
        }
        self.toggled
            .retain(|action| !self.lost_actions.contains(action) || physical.held.contains(action));
        self.lost_actions.clear();
        self.blocked_actions
            .retain(|action| physical.held.contains(action));
        physical
            .held
            .retain(|action| !self.blocked_actions.contains(action));
        physical
            .pressed
            .retain(|action| !self.blocked_actions.contains(action));
        for action in &self.blocked_actions {
            physical.values.insert(action.clone(), 0.0);
        }
        let mut pressed: BTreeSet<_> = physical
            .held
            .difference(&self.previous_raw)
            .cloned()
            .collect();
        pressed.extend(physical.pressed.difference(&self.previous_custom).cloned());
        self.previous_raw = physical.held.clone();
        self.previous_custom = custom_held;
        for (action, behavior) in &self.behaviors {
            if *behavior == ActionBehavior::Toggle {
                if pressed.contains(action) && !self.toggled.remove(action) {
                    self.toggled.insert(action.clone());
                }
                physical.held.remove(action);
                physical.values.insert(
                    action.clone(),
                    if self.toggled.contains(action) {
                        1.0
                    } else {
                        0.0
                    },
                );
                if self.toggled.contains(action) {
                    physical.held.insert(action.clone());
                }
                pressed.remove(action);
            }
        }
        pressed.extend(physical.held.difference(&self.previous_output).cloned());
        let mut released: BTreeSet<_> = self
            .previous_output
            .difference(&physical.held)
            .cloned()
            .collect();
        released.extend(
            physical
                .released
                .iter()
                .filter(|a| {
                    !physical.held.contains(*a)
                        && self.behaviors.get(*a) != Some(&ActionBehavior::Toggle)
                })
                .cloned(),
        );
        physical.pressed = pressed;
        physical.released = released;
        self.previous_output = physical.held.clone();
        physical
    }
}

pub fn register_input_source(world: &mut World, source: impl InputSource) -> Result<()> {
    world.init_resource::<InputDevices>();
    world.resource_mut::<InputDevices>().register(source)
}
/// Runtime calls this before UI capture and gameplay callbacks, in every host mode.
pub fn poll_devices(world: &mut World, physical: ActionFrame) -> ActionFrame {
    world.init_resource::<InputDevices>();
    if let Some(settings) = world.get_resource::<crate::runtime::InputSettings>()
        && world.resource::<InputDevices>().physical_revision != Some(settings.revision)
    {
        let bindings = settings.bindings.clone();
        let revision = settings.revision;
        let mut devices = world.resource_mut::<InputDevices>();
        devices.synchronize_physical_bindings(&bindings);
        devices.physical_revision = Some(revision);
    }
    world
        .resource_mut::<InputDevices>()
        .poll_and_merge(physical)
}
