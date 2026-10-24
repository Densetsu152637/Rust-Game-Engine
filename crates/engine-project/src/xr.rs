//! Authored PC VR settings plus the plain data an XR host (crate `engine-xr`)
//! exchanges with gameplay: tracking poses, controller input, haptic requests
//! and the honest runtime status. This module never talks to OpenXR itself.
//!
//! * [`XrSettings`] (`engine.XrSettings`) is authored on the entity whose
//!   `Transform` is the tracking-space origin (the "rig"). At most one entity
//!   may have an enabled `XrSettings`.
//! * [`XrTracking`] is a resource the host updates every XR frame. Poses are in
//!   the selected reference space (metres, right-handed, +Y up, -Z forward).
//! * Controller buttons and axes reach authored actions through `XR:` binding
//!   tokens and the [`XrInputSource`] device (see [`crate::input::XR_DEVICE_ID`]).
//! * [`request_haptic`] (native) and the `engine.xr.haptic` event handler
//!   (authored `EventBindings`) queue vibration requests the host drains.
use crate::{
    ComponentDescriptor, EngineEvent, Error, Result,
    input::{DeviceDescriptor, DeviceSample, InputSource, xr_device_descriptor},
};
use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Component name of [`XrSettings`].
pub const XR_SETTINGS_COMPONENT: &str = "engine.XrSettings";
/// Event handler that turns an event payload into a haptic request.
pub const XR_HAPTIC_HANDLER: &str = "engine.xr.haptic";
/// Bound on queued, undrained haptic requests.
pub const MAX_QUEUED_HAPTICS: usize = 64;
/// Bound on the status reason carried in snapshots.
pub const MAX_STATUS_REASON: usize = 4096;

/// Tracking-space origin used for poses and the projection layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XrReferenceSpace {
    /// Floor-level origin at the centre of the guardian/chaperone bounds.
    #[default]
    Stage,
    /// Origin at the headset's position when the session started (seated).
    Local,
}

/// Which eye the desktop window mirrors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XrMirrorMode {
    None,
    #[default]
    LeftEye,
    RightEye,
}

/// Defaults applied to haptic requests that leave a field unset.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct XrHapticDefaults {
    /// When false, haptic requests are discarded.
    pub enabled: bool,
    /// Vibration amplitude in [0,1].
    pub amplitude: f32,
    /// Pulse length in milliseconds; 0 asks the runtime for its shortest pulse.
    pub duration_ms: f32,
    /// Vibration frequency in Hz; 0 lets the runtime choose.
    pub frequency_hz: f32,
}
impl Default for XrHapticDefaults {
    fn default() -> Self {
        Self {
            enabled: true,
            amplitude: 0.5,
            duration_ms: 50.0,
            frequency_hz: 0.0,
        }
    }
}

/// Authored XR configuration (`engine.XrSettings`, version 1).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct XrSettings {
    /// Request an XR session when an XR host runs this project.
    pub enabled: bool,
    pub reference_space: XrReferenceSpace,
    /// Multiplier on the runtime's recommended per-eye resolution, in [0.25, 2].
    pub render_scale: f32,
    pub mirror: XrMirrorMode,
    /// Near clip distance in metres, in [0.001, 10].
    pub near_clip: f32,
    /// Far clip distance in metres, in [0.01, 100000]; must exceed `near_clip`.
    pub far_clip: f32,
    /// Deadzone of triggers and grips, in [0, 0.95].
    pub trigger_deadzone: f32,
    /// Deadzone of thumbsticks and trackpads, in [0, 0.95].
    pub thumbstick_deadzone: f32,
    pub haptics: XrHapticDefaults,
}
impl Default for XrSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reference_space: XrReferenceSpace::Stage,
            render_scale: 1.0,
            mirror: XrMirrorMode::LeftEye,
            near_clip: 0.05,
            far_clip: 1000.0,
            trigger_deadzone: crate::input::XR_TRIGGER_DEADZONE,
            thumbstick_deadzone: crate::input::XR_THUMBSTICK_DEADZONE,
            haptics: XrHapticDefaults::default(),
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::new("xr.settings.invalid", message)
}
fn within(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}
impl XrSettings {
    /// Range and cross-field checks; the component schema checks the same
    /// ranges, so authored data and native construction agree.
    pub fn validate(&self) -> Result<()> {
        let checks = [
            (
                within(self.render_scale, 0.25, 2.0),
                "render_scale must be in [0.25, 2]",
            ),
            (
                within(self.near_clip, 0.001, 10.0),
                "near_clip must be in [0.001, 10]",
            ),
            (
                within(self.far_clip, 0.01, 100_000.0),
                "far_clip must be in [0.01, 100000]",
            ),
            (
                self.far_clip > self.near_clip,
                "far_clip must exceed near_clip",
            ),
            (
                within(self.trigger_deadzone, 0.0, 0.95),
                "trigger_deadzone must be in [0, 0.95]",
            ),
            (
                within(self.thumbstick_deadzone, 0.0, 0.95),
                "thumbstick_deadzone must be in [0, 0.95]",
            ),
            (
                within(self.haptics.amplitude, 0.0, 1.0),
                "haptics.amplitude must be in [0, 1]",
            ),
            (
                within(self.haptics.duration_ms, 0.0, 10_000.0),
                "haptics.duration_ms must be in [0, 10000]",
            ),
            (
                within(self.haptics.frequency_hz, 0.0, 1000.0),
                "haptics.frequency_hz must be in [0, 1000]",
            ),
        ];
        match checks.into_iter().find(|(ok, _)| !ok) {
            Some((_, message)) => Err(invalid(message)),
            None => Ok(()),
        }
    }
}

fn number(min: f64, max: f64, description: &str) -> serde_json::Value {
    json!({"type":"number","minimum":min,"maximum":max,"description":description})
}
/// Built-in catalog descriptor of [`XrSettings`].
pub fn descriptor() -> ComponentDescriptor {
    let haptics = json!({"type":"object","additionalProperties":false,
    "description":"Defaults for haptic requests that leave a field unset",
    "properties":{
        "enabled":{"type":"boolean","description":"Discard haptic requests when false"},
        "amplitude":number(0.0, 1.0, "Vibration amplitude"),
        "duration_ms":number(0.0, 10000.0, "Pulse length in milliseconds; 0 = runtime minimum"),
        "frequency_hz":number(0.0, 1000.0, "Vibration frequency in Hz; 0 = runtime default"),
    }});
    ComponentDescriptor {
        name: XR_SETTINGS_COMPONENT.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,
        "description":"PC VR (OpenXR) settings; the entity Transform is the tracking-space origin",
        "properties":{
            "enabled":{"type":"boolean","description":"Request an XR session when an XR host runs the project"},
            "reference_space":{"type":"string","enum":["stage","local"],"description":"stage = floor-level play-area origin; local = seated origin at session start"},
            "render_scale":number(0.25, 2.0, "Multiplier on the runtime's recommended per-eye resolution"),
            "mirror":{"type":"string","enum":["none","left_eye","right_eye"],"description":"Eye mirrored to the desktop window"},
            "near_clip":number(0.001, 10.0, "Near clip distance in metres"),
            "far_clip":number(0.01, 100000.0, "Far clip distance in metres; must exceed near_clip"),
            "trigger_deadzone":number(0.0, 0.95, "Deadzone of triggers and grips"),
            "thumbstick_deadzone":number(0.0, 0.95, "Deadzone of thumbsticks and trackpads"),
            "haptics":haptics,
        }}),
        defaults: serde_json::to_value(XrSettings::default()).expect("finite builtin defaults"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}
/// Register the typed component adapter and the `engine.xr.haptic` handler.
pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<XrSettings>(app, XR_SETTINGS_COMPONENT, 1)?;
    crate::register_event_handler(app, XR_HAPTIC_HANDLER, haptic_event)
}

/// The single enabled `XrSettings` and its rig entity, if any.
pub fn active_settings(world: &World) -> Result<Option<(Entity, XrSettings)>> {
    let mut found = None;
    for entity in world.iter_entities() {
        if let Some(settings) = entity.get::<XrSettings>().filter(|s| s.enabled) {
            if found.is_some() {
                return Err(invalid(
                    "At most one entity may have an enabled engine.XrSettings",
                ));
            }
            settings.validate()?;
            found = Some((entity.id(), *settings));
        }
    }
    Ok(found)
}

/// Honest XR availability published by the host; mirrors `engine_xr::XrStatus`.
#[derive(Resource, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XrStatusReport {
    /// Stable snake_case state, for example `runtime_not_found`, `no_headset`,
    /// `session_running`. See docs/xr.md for the full list.
    pub state: String,
    /// Precise human-readable reason (bounded to [`MAX_STATUS_REASON`] chars).
    pub reason: String,
    /// Runtime name and version once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}
impl XrStatusReport {
    pub fn new(
        state: impl Into<String>,
        reason: impl Into<String>,
        runtime: Option<String>,
    ) -> Self {
        Self {
            state: state.into(),
            reason: reason.into().chars().take(MAX_STATUS_REASON).collect(),
            runtime,
        }
    }
}
/// Publish the host's latest status for snapshots and gameplay.
pub fn publish_status(world: &mut World, report: XrStatusReport) {
    world.insert_resource(XrStatusReport::new(
        report.state,
        report.reason,
        report.runtime,
    ));
}
/// The host-published status; without a host, `not_attached` when the project
/// enables XR, else `None`.
pub fn status(world: &World) -> Option<XrStatusReport> {
    if let Some(report) = world.get_resource::<XrStatusReport>() {
        return Some(report.clone());
    }
    match active_settings(world) {
        Ok(Some(_)) => Some(XrStatusReport::new(
            "not_attached",
            "engine.XrSettings is enabled but no XR host (engine-xr session) is attached to this runtime",
            None,
        )),
        Ok(None) => None,
        Err(error) => Some(XrStatusReport::new(
            "settings_invalid",
            error.to_string(),
            None,
        )),
    }
}

/// Pose in the tracking space: metres and a unit quaternion `[x, y, z, w]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct XrPose {
    pub position: [f32; 3],
    pub orientation: [f32; 4],
    /// False when the runtime could not locate the pose this frame.
    pub tracked: bool,
}
impl Default for XrPose {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            orientation: [0.0, 0.0, 0.0, 1.0],
            tracked: false,
        }
    }
}
/// Tracked poses of one hand's controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct XrHandPose {
    /// Where the controller is held (attach held objects here).
    pub grip: XrPose,
    /// Pointing ray origin and direction (-Z) for UI and aiming.
    pub aim: XrPose,
}
/// Latest XR frame data, updated by the host before gameplay callbacks.
#[derive(Resource, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct XrTracking {
    pub head: XrPose,
    pub left: XrHandPose,
    pub right: XrHandPose,
    /// Runtime-predicted display time of the frame (OpenXR `XrTime`, ns).
    pub predicted_display_time_ns: i64,
    /// Frames published since the session started.
    pub frame: u64,
}

/// One hand's controller controls, already mapped from the runtime's
/// interaction profile. Analog values are in [0,1] (trigger/grip) or [-1,1]
/// (thumbstick); unbound controls stay neutral.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XrHandControls {
    pub trigger: f32,
    pub grip: f32,
    pub thumbstick: [f32; 2],
    pub thumbstick_click: bool,
    pub primary: bool,
    pub secondary: bool,
    pub menu: bool,
}
/// Both hands' controls plus whether the XR session is delivering input.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XrControllerState {
    /// False before the session is focused or after it ends.
    pub connected: bool,
    pub left: XrHandControls,
    pub right: XrHandControls,
}
impl XrControllerState {
    /// The input-device sample of this state (controls named `<Hand><Control>`).
    pub fn to_sample(&self) -> Result<DeviceSample> {
        if !self.connected {
            return Ok(DeviceSample::disconnected());
        }
        let mut sample = DeviceSample::connected();
        let button = |pressed: bool| if pressed { 1.0 } else { 0.0 };
        for (hand, controls) in [("Left", &self.left), ("Right", &self.right)] {
            sample.set(format!("{hand}Trigger"), controls.trigger)?;
            sample.set(format!("{hand}Grip"), controls.grip)?;
            sample.set(format!("{hand}ThumbstickX"), controls.thumbstick[0])?;
            sample.set(format!("{hand}ThumbstickY"), controls.thumbstick[1])?;
            sample.set(
                format!("{hand}ThumbstickClick"),
                button(controls.thumbstick_click),
            )?;
            sample.set(format!("{hand}Primary"), button(controls.primary))?;
            sample.set(format!("{hand}Secondary"), button(controls.secondary))?;
            sample.set(format!("{hand}Menu"), button(controls.menu))?;
        }
        Ok(sample)
    }
}
/// Shared handle the XR host writes and [`XrInputSource`] reads.
#[derive(Clone, Default)]
pub struct XrInputFeed(Arc<Mutex<XrControllerState>>);
impl XrInputFeed {
    pub fn set(&self, state: XrControllerState) {
        *self.0.lock().unwrap_or_else(|poison| poison.into_inner()) = state;
    }
    pub fn get(&self) -> XrControllerState {
        *self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}
/// Input source with ID [`crate::input::XR_DEVICE_ID`]; register it with
/// [`crate::input::register_input_source`] so `XR:` binding tokens resolve.
pub struct XrInputSource {
    feed: XrInputFeed,
    descriptor: DeviceDescriptor,
}
impl XrInputSource {
    pub fn new(feed: XrInputFeed, settings: &XrSettings) -> Self {
        Self {
            feed,
            descriptor: xr_device_descriptor(
                settings.trigger_deadzone,
                settings.thumbstick_deadzone,
            ),
        }
    }
}
impl InputSource for XrInputSource {
    fn descriptor(&self) -> DeviceDescriptor {
        self.descriptor.clone()
    }
    fn poll(&mut self) -> Result<DeviceSample> {
        self.feed.get().to_sample()
    }
}

/// Which controller(s) a haptic request addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XrHapticTarget {
    Left,
    Right,
    #[default]
    Both,
}
/// A haptic request; unset fields take the active `XrSettings.haptics` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct XrHapticRequest {
    pub target: XrHapticTarget,
    pub amplitude: Option<f32>,
    pub duration_ms: Option<f32>,
    pub frequency_hz: Option<f32>,
}
impl XrHapticRequest {
    fn validate(&self) -> Result<()> {
        let ok = |value: Option<f32>, max: f32| value.is_none_or(|v| within(v, 0.0, max));
        if ok(self.amplitude, 1.0)
            && ok(self.duration_ms, 10_000.0)
            && ok(self.frequency_hz, 1000.0)
        {
            Ok(())
        } else {
            Err(Error::new(
                "xr.haptic.invalid",
                "Haptic amplitude must be in [0,1], duration_ms in [0,10000] and frequency_hz in [0,1000]",
            ))
        }
    }
}
/// One controller's resolved vibration, ready for `engine_xr::XrSession::apply_haptic`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XrHapticPulse {
    /// `XrHapticTarget::Left` or `XrHapticTarget::Right`.
    pub hand: XrHapticTarget,
    pub amplitude: f32,
    pub duration_ms: f32,
    pub frequency_hz: f32,
}
#[derive(Resource, Default)]
struct XrHapticQueue(VecDeque<XrHapticRequest>);

/// Queue a vibration for the XR host (command API for native gameplay code).
pub fn request_haptic(world: &mut World, request: XrHapticRequest) -> Result<()> {
    request.validate()?;
    let mut queue = world.get_resource_or_insert_with(XrHapticQueue::default);
    if queue.0.len() >= MAX_QUEUED_HAPTICS {
        return Err(Error::new(
            "xr.haptic.limit",
            "At most 64 undrained haptic requests may be queued",
        ));
    }
    queue.0.push_back(request);
    Ok(())
}
/// `engine.xr.haptic` event handler: the payload is an [`XrHapticRequest`]
/// (`{}` or `null` pulses both controllers with the authored defaults).
fn haptic_event(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    let request = if event.payload.is_null() {
        XrHapticRequest::default()
    } else {
        serde_json::from_value(event.payload.clone())
            .map_err(|e| Error::new("xr.haptic.invalid", e.to_string()))?
    };
    request_haptic(world, request)
}
/// Drain queued requests into per-controller pulses using the active settings'
/// defaults. Without enabled settings the authored defaults are the
/// [`XrHapticDefaults::default`] values; disabled haptics drop the queue.
pub fn drain_haptics(world: &mut World) -> Vec<XrHapticPulse> {
    let requests: Vec<_> = world
        .get_resource_mut::<XrHapticQueue>()
        .map(|mut queue| queue.0.drain(..).collect())
        .unwrap_or_default();
    let defaults = active_settings(world)
        .ok()
        .flatten()
        .map_or_else(XrHapticDefaults::default, |(_, s)| s.haptics);
    if !defaults.enabled {
        return Vec::new();
    }
    let mut pulses = Vec::new();
    for request in requests {
        let hands: &[XrHapticTarget] = match request.target {
            XrHapticTarget::Both => &[XrHapticTarget::Left, XrHapticTarget::Right],
            XrHapticTarget::Left => &[XrHapticTarget::Left],
            XrHapticTarget::Right => &[XrHapticTarget::Right],
        };
        for hand in hands {
            pulses.push(XrHapticPulse {
                hand: *hand,
                amplitude: request.amplitude.unwrap_or(defaults.amplitude),
                duration_ms: request.duration_ms.unwrap_or(defaults.duration_ms),
                frequency_hz: request.frequency_hz.unwrap_or(defaults.frequency_hz),
            });
        }
    }
    pulses
}
