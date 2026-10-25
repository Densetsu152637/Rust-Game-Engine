# Custom peripherals and accessible action input

Keyboard, mouse buttons, pointer movement, trackpad wheel scrolling and a selected
gilrs controller use the existing desktop mappings in [Desktop input](desktop-input-ui.md).
Devices that emulate those OS interfaces use the same path, including adaptive
controllers, switch interfaces and alternative mice. The engine does not bundle
drivers for every proprietary HID protocol, eye tracker or assistive device.

A native game can register an `engine_project::input::InputSource` for a device
that needs its own driver or SDK. An adapter describes named buttons and signed
axes and returns their complete state once per tick. `Runtime::tick` polls these
sources in both desktop and headless runs, before modal UI handling, native script
callbacks and scheduled gameplay systems. The adapter's actions therefore follow
the same control locks and same-callback modal capture as ordinary input.

## Register and remap

Implement `InputSource::descriptor()` and `poll()`. The descriptor contains a
stable application-selected device ID, a display label and a map of control names
to `ControlKind::Button` or `ControlKind::Axis { deadzone }`. Buttons activate
above 0.5; axes normalize the range outside their deadzone. Native drivers should
publish a bounded latest state rather than block the simulation while waiting
for an event.

Register in the external game's `configure(app)` using
`register_input_source(app.world_mut(), adapter)?`. This initializes the
`InputDevices` ECS resource if needed. Then configure its mappings, for example:

```rust,ignore
use engine_project::input::{
    ActionBehavior, AxisDirection, ControlBinding, InputDevices,
};
use std::collections::BTreeMap;

let mut devices = app.world_mut().resource_mut::<InputDevices>();
devices.set_bindings(BTreeMap::from([
    ("interact".into(), vec![ControlBinding {
        device: "adaptive-switch".into(),
        control: "primary".into(),
        direction: AxisDirection::Positive,
    }]),
]))?;
devices.set_behavior("interact", ActionBehavior::Toggle)?;
```

Device IDs and control names must match registered descriptors. Actions are the
same names consumed by gameplay and UI; a custom action can share a name with a
keyboard or controller binding. Positive/negative axis directions map to separate
actions. `set_bindings` validates atomically; currently active custom controls
must return to neutral before newly configured bindings activate. Binding maps
and behavior enums support Serde for game-owned settings storage. They are not
additional project manifest keys or a generic MCP hardware-injection endpoint.

Build a connected sample with `DeviceSample::connected()` and
`sample.set("primary", value)?`; omitted controls are neutral. Values clamp to
`[-1,1]`, and NaN/infinity are rejected. Return `DeviceSample::disconnected()`
while hardware is absent. The registration and mapping remain available for
reconnection. `unregister(id)` permanently removes the source and its mappings.

## Edges, focus and accessibility

For an in-game remapping screen built on this API, see
[Accessibility](accessibility.md#input-remapping-screen).

The runtime combines held actions from physical input and every custom device.
It computes press/release transitions from that union, so releasing or unplugging
one device cannot release an action that another source still holds. Latched
one-tick physical tap edges remain available. Values use the maximum contribution.
The desktop pointer and wheel retain their existing logical-coordinate path;
custom source axes map to actions. A custom pointing device can use OS mouse
emulation, or game code can interpret its mapped axes.

Newly connected controls already held down are quarantined until neutral. Focus
loss releases controls, clears accessibility toggles and quarantines active
controls across focus restoration. Adapters are still polled while unfocused.
No background press is replayed when focus returns.

`ActionBehavior::Hold` is the default. `Toggle` turns an action on with one press
and off with the next, without requiring a continuous hold. It applies to the
combined action, including keyboard/controller sources. Changing a behavior
clears its latch and quarantines that action until neutral. Disconnecting or
unregistering a source clears affected toggles unless another source currently
holds that action; unrelated toggles remain unchanged. Games choose which
actions should offer this option and how users select it.

The physical mapper reports binding and controller loss separately through
`ActionFrame.cancelled`; ordinary release does not cancel a toggle. The runtime
also observes `InputSettings::rebind` revisions, clearing and quarantining changed
physical actions even in hosts that supply their own action frames. Winit keyboard
and mouse input represents OS-level input; it does not identify individual keyboard
or mouse unplug events. Use an explicit native adapter when device-level disconnect
semantics are required for those peripherals. Native callers supplying custom
physical frames should populate `cancelled` when their source is lost.

## Bounds and diagnostics

There are at most 32 registered sources, 128 controls per source, 256 mapped
actions and 16 controls per action. Names are bounded to 256 bytes. Axis deadzones
must be finite and in `[0,1)`. An adapter returns one bounded complete sample per
tick; omitted controls never retain stale values.

If a poll fails or returns an undeclared control, only that source becomes neutral
and disconnected for the tick. Its latest error remains in its status until a
successful poll, capped at 1024 characters. Reconnection then requires neutral
controls. This isolates device failure without stopping other input or swallowing
the diagnostic. Native adapter panics and execution time remain the native game's
responsibility; polling must be nonblocking.

`InputDevices::snapshot()` returns the serializable
`engine_project::input::InputDevicesSnapshot`: device descriptors, connection/error
status, bindings and accessibility behaviors. Runtime snapshots include it as
`input_devices` when the resource exists. MCP `project_run_headless` returns this
field as well, capped at 64 KiB; an oversized field becomes null and is named in
`telemetry_omitted`. It contains no raw driver handles. No hardware-specific
compatibility is implied by a registered descriptor.

`cargo test -p engine-project --test input_devices` verifies fake native adapters
through ordinary runtime ticks, hotplug/focus recovery, competing held sources,
remapping, toggles, bounded/invalid samples and same-callback modal capture.
Physical assistive hardware still requires driver-specific testing.
