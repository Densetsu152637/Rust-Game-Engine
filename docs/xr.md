# PC VR (OpenXR)

The `engine-xr` crate runs PC VR through OpenXR on Windows and Linux: runtime
discovery, an honest availability status, the session lifecycle, action-based
controller input, head/hand tracking, haptics, and per-eye swapchains wrapped as
`wgpu::Texture`s on a Vulkan device created through `XR_KHR_vulkan_enable2`.
Authored settings, tracking data, controller input and haptic requests live in
`engine-project` (`crates/engine-project/src/xr.rs`) so gameplay never touches
OpenXR.

Ordinary `cargo build --workspace` needs no OpenXR SDK, loader DLL, CMake or
headset: the runtime is found and loaded at run time.

## Status of each part

| Part | State |
| --- | --- |
| Runtime discovery, built-in negotiation, `XrStatus` | Implemented; unit-tested, and run against the installed Meta (Oculus) runtime without a headset (see [Observed status](#observed-status-on-the-development-machine)) |
| Session lifecycle state machine | Implemented; transitions unit-tested; live transitions need a headset |
| Per-frame wait/begin/end, per-eye views, projection math | Implemented; projection math unit-tested; live loop needs a headset |
| Action sets, four interaction profiles, `XR:` binding tokens | Implemented; binding tables and engine input mapping unit-tested |
| Head, grip and aim poses (`XrTracking`) | Implemented; needs a headset to observe |
| Haptics (native, `request_haptic`, `engine.xr.haptic` event) | Implemented; queueing/resolution tested; vibration needs a headset |
| Vulkan binding, wgpu device and swapchain textures | Implemented through wgpu-hal; unverified until a headset session runs |
| Test pattern renderer and desktop mirror blit | Implemented; pixel-checked on an ordinary Vulkan device (ignored GPU tests) |
| Scene rendering in the headset | Deferred: the desktop renderer cannot yet render into external per-eye targets (see [Renderer integration](#renderer-integration)) |
| macOS | Unsupported (no mainstream OpenXR runtime); reported as `platform_unsupported` |

## Setup

1. Install a PC VR runtime and make it the active OpenXR runtime: Meta Quest Link
   (Oculus app: Settings > General > OpenXR Runtime), SteamVR (Settings >
   OpenXR > Set SteamVR as OpenXR runtime), or Monado on Linux.
2. Have a Vulkan driver installed (`vulkan-1.dll` / `libvulkan.so.1`).
3. Add `engine.XrSettings` to the entity whose `Transform` is the tracking-space
   origin (the "rig"). With MCP:
   `component_set {"scene":"scenes/start.json","expected_revision":"<revision>","entity":"rig","component":"engine.XrSettings","value":{"version":1,"data":{"reference_space":"stage"}}}`.
4. Bind controller inputs in the project's `inputBindings`, for example
   `"grab": ["XR:RightGrip", "Key:G"]`.

No API layers are loaded by default (see [Runtime discovery](#runtime-discovery)).

## Runtime discovery

`engine_xr::manifest::resolve` follows the Khronos loader's active-runtime
rules; the first existing file wins, and an existing but invalid file is
reported (not skipped):

1. `XR_RUNTIME_JSON`, when set and non-empty. Exclusive on every platform.
2. Windows: `HKEY_LOCAL_MACHINE\SOFTWARE\Khronos\OpenXR\1` value `ActiveRuntime`
   (64-bit registry view, environment strings expanded).
3. Linux: `$XDG_CONFIG_HOME/openxr/1/` (default `$HOME/.config/openxr/1/`), then
   each `$XDG_CONFIG_DIRS` entry (default `/etc/xdg`) as `<dir>/openxr/1/`, then
   `/etc/openxr/1/`. Each directory is checked for `active_runtime.<arch>.json`
   before `active_runtime.json`.
4. macOS and other platforms: `platform_unsupported` unless `XR_RUNTIME_JSON` is set.

Manifests are read with a 64 KiB bound and must contain
`file_format_version` (`1.x`) and `runtime.library_path` (at most 4096 bytes).
`runtime.name` and a renamed `functions.xrNegotiateLoaderRuntimeInterface` are
honoured. An absolute `library_path` is used as is, a relative path containing a
separator resolves against the manifest's directory and must exist, and a bare
file name uses the operating system's library search.

### Loader decision

| Option | Result |
| --- | --- |
| Bundle or statically build the Khronos loader (`openxr` features `static`/`linked`) | Rejected: needs CMake (absent here) or a shipped DLL, and violates "builds need no SDK" |
| `openxr::Entry::load()` (system `openxr_loader`) | Not the default: no loader is installed on typical machines (none here), and loading by bare name searches the working directory |
| Built-in negotiator (chosen) | Load the runtime named by the active manifest, call `xrNegotiateLoaderRuntimeInterface` (loader interface 1, API 1.0.0 to 1.1023.4095), then `openxr::Entry::from_get_instance_proc_addr` |

The built-in path **bypasses OpenXR API layers** (validation, overlays,
capture tools). To use them, install the Khronos loader and select it
explicitly with `LoaderMode::KhronosLoader(<absolute path to openxr_loader>)`;
a path is required to avoid DLL planting. On Windows the runtime library is
loaded like the Khronos loader does: plain `LoadLibrary`, then a retry that also
searches the library's own directory for its dependencies. The `openxr` crate is
pinned at `=0.22.0` with default features off.

## Status meanings

`XrStatus` (Rust) and `XrStatusReport` (snapshot telemetry, field `xr` of
`RuntimeSnapshot` and of the MCP `run` result) use these states. Every reason
names what was checked and the exact `XrResult`.

| State | Meaning |
| --- | --- |
| `platform_unsupported` | No OpenXR runtime convention on this OS (macOS) and no `XR_RUNTIME_JSON` |
| `runtime_not_found` | No manifest at any location; the reason lists every location and why (`key or value not present`, `file not found`) |
| `manifest_invalid` | Manifest unreadable, above 64 KiB, malformed, or its library file is missing |
| `runtime_load_failed` | The runtime (or loader) library failed to load, lacks the negotiation export, or negotiation failed |
| `instance_creation_failed` | `xrEnumerateInstanceExtensionProperties`/`xrCreateInstance` failed, e.g. `XR_ERROR_RUNTIME_UNAVAILABLE` when the runtime service is stopped |
| `no_headset` | `xrGetSystem(HEAD_MOUNTED_DISPLAY)` failed, normally `XR_ERROR_FORM_FACTOR_UNAVAILABLE` (headset off or disconnected) |
| `graphics_binding_unavailable` | No `XR_KHR_vulkan_enable2`, Vulkan below 1.1, no usable Vulkan device, or no RGBA8/BGRA8 swapchain format |
| `system_ready` | Headset and Vulkan binding available; no session yet (end state of `engine_xr::probe`) |
| `session_idle` | Session created (`IDLE`/`READY`); waiting for the runtime |
| `session_running` | `SYNCHRONIZED`, `VISIBLE` or `FOCUSED`; frames are submitted |
| `session_stopping` | Runtime requested a stop; `xrEndSession` follows |
| `session_exiting` | Session ends and must be dropped |
| `session_lost` | `LOSS_PENDING`, instance loss, or a call returned `XR_ERROR_SESSION_LOST`/`XR_ERROR_INSTANCE_LOST` |
| `not_attached` | Snapshot only: `engine.XrSettings` is enabled but no XR host published a status |
| `settings_invalid` | Snapshot only: more than one enabled `engine.XrSettings`, or invalid values |

### Observed status on the development machine

Windows 11, RTX 5080, Meta (Oculus) runtime registered as the active runtime,
`OVRService` stopped, no headset connected, no `openxr_loader.dll`
(`installed_runtime_without_a_headset_reports_an_honest_status`, 2026-10-25):

```text
instance_creation_failed -> Oculus OpenXR was found but OpenXR instance creation
failed with XR_ERROR_RUNTIME_UNAVAILABLE (-51): the loader was unable to find or
load a runtime (the runtime library loaded; its service is likely not running,
e.g. the Oculus OVRService or SteamVR)
```

The registry lookup, manifest parse (`.\LibOVRRTImpl64_1.dll` relative to the
manifest), library load and `xrNegotiateLoaderRuntimeInterface` all succeeded;
`xrCreateInstance` is where the runtime declined. The trailing "loader" wording
is the OpenXR specification's generic text for that result code. With the
service running and no headset, `no_headset` (`XR_ERROR_FORM_FACTOR_UNAVAILABLE`)
is the expected state; that was not observed here.

## Session lifecycle and frame loop

`XrSession::create(instance, system, config)` creates the Vulkan device through
the runtime, the session, the action set and the reference spaces. The host
calls `poll_events()` every frame; `SessionLifecycle` maps runtime states to
actions (READY: `xrBeginSession`; STOPPING: `xrEndSession`; EXITING: drop the
session; LOSS_PENDING or instance loss: `session_lost`). `request_exit()` calls
`xrRequestExitSession` while running, or finishes immediately otherwise.

`frame(renderer, mirror)` does, while the session runs: `xrWaitFrame`,
`xrBeginFrame`, action sync and pose location at the predicted display time,
`xrLocateViews`, per-eye `acquire`/`wait`, `XrViewRenderer::render_views`, the
optional mirror blit, a pass that leaves each image in colour-attachment layout,
`release`, and `xrEndFrame` with one projection layer. With `should_render`
false, invalid view orientation, or a renderer error, the frame is ended with no
layers (renderer errors are then returned).

Reference space: `stage` (floor-level play area) falls back to `local` when the
runtime lacks STAGE; the fallback is listed in `XrSession::notes()`.

Per-eye views (`XrView`): pose, fov, projection and view matrix. Conventions
match the desktop camera: right-handed, metres, +Y up, looking down -Z, depth 0
at near and 1 at far (not reversed), the same matrix as
`glam::camera::rh::proj::directx::perspective` for a symmetric fov. Swapchains
use the runtime's recommended size times `render_scale`, clamped to its maximum.

## Input

One action set `engine` with hand subaction paths. Suggested bindings:

| Action | khr/simple_controller | oculus/touch_controller | valve/index_controller | htc/vive_controller |
| --- | --- | --- | --- | --- |
| Trigger | `select/click` | `trigger/value` | `trigger/value` | `trigger/value` |
| Grip | - | `squeeze/value` | `squeeze/value` | `squeeze/click` |
| Thumbstick | - | `thumbstick` | `thumbstick` | `trackpad` |
| ThumbstickClick | - | `thumbstick/click` | `thumbstick/click` | `trackpad/click` |
| Primary | - | left `x/click`, right `a/click` | `a/click` | - |
| Secondary | - | left `y/click`, right `b/click` | `b/click` | - |
| Menu | `menu/click` | left `menu/click` | - | `menu/click` |
| Grip pose, aim pose, haptic | `grip/pose`, `aim/pose`, `output/haptic` on every profile |

A profile the runtime rejects is recorded in `XrSession::notes()`; the others
still apply.

Authored actions bind `XR:` tokens next to keys and gamepad buttons:
`XR:{Left|Right}{Trigger|Grip|ThumbstickClick|Primary|Secondary|Menu}` and
`XR:{Left|Right}{ThumbstickX|ThumbstickY}{Positive|Negative}` (20 tokens).
`XrWorldBridge::install` registers an input source with device ID `xr`
(controls `<Hand><Control>`, trigger/grip deadzone `trigger_deadzone`, stick
deadzone `thumbstick_deadzone`); `InputDevices` resolves the tokens against it
with the usual edge, quarantine and disconnect handling. Poses are in the
`XrTracking` resource (`head`, `left`/`right` `grip` and `aim`, each with
`tracked`), in the reference space; compose with the rig entity's `Transform`
for world space.

## Haptics

- Native: `XrSession::apply_haptic(&XrHapticPulse)` (amplitude 0..1, duration
  in ms with 0 meaning the runtime minimum, frequency in Hz with 0 meaning
  runtime default).
- Gameplay command: `engine_project::xr::request_haptic(world, XrHapticRequest)`
  (bounded queue of 64; unset fields take `XrSettings.haptics`).
- Authored event: bind any event to the handler `engine.xr.haptic` in
  `EventBindings`; the payload is an `XrHapticRequest`
  (`{"target":"right","amplitude":0.8}`; empty payload pulses both hands).
- The host drains the queue with `XrWorldBridge::apply_haptics`.
  `haptics.enabled = false` discards requests.

## Authored settings (`engine.XrSettings`, version 1)

| Field | Default | Range |
| --- | --- | --- |
| `enabled` | `true` | request a session when an XR host runs the project |
| `reference_space` | `stage` | `stage`, `local` |
| `render_scale` | `1.0` | 0.25..2 |
| `mirror` | `left_eye` | `none`, `left_eye`, `right_eye` |
| `near_clip` | `0.05` | 0.001..10 m |
| `far_clip` | `1000` | 0.01..100000 m, greater than `near_clip` |
| `trigger_deadzone` | `0.05` | 0..0.95 |
| `thumbstick_deadzone` | `0.15` | 0..0.95 |
| `haptics.enabled` / `amplitude` / `duration_ms` / `frequency_hz` | `true` / `0.5` / `50` / `0` | bool / 0..1 / 0..10000 / 0..1000 |

Unknown fields are rejected, and so is more than one enabled `XrSettings` in a
scene at run time. The component is edited with the generic MCP `component_set`
and documented by `component_schema`.

## Renderer integration

`engine-xr` exposes rendering only through `XrViewRenderer`
(`render_views(&XrFrameContext)`): the XR device and queue, the predicted
display time, and per eye an `XrRenderTarget` (view data, swapchain
`wgpu::Texture` and view, format). `TestPattern` implements it; `MirrorBlit`
copies an eye into a texture created on the XR device.

The desktop renderer (`engine-desktop`) is not wired yet because it lacks two
things owned by the rendering domain (program contract 1):

1. Adopting an external device: create the desktop `GpuContext` and window
   surface from the XR session's `wgpu::Instance`/`Adapter`/`Device`/`Queue`
   (`XrSession::wgpu_instance()`, `adapter()`, `device()`, `queue()`) instead of
   requesting its own, so the mirror and scene share one device.
2. Rendering a scene frame into an external target with explicit matrices:
   for each eye, `color: &wgpu::TextureView`, `format`, `extent`, `view` (world
   to eye: `XrView::view * rig_world.inverse()`) and `projection`
   (`XrView::projection`), with the renderer owning a depth buffer per extent;
   no swapchain present, upscaler or frame generation for these views.

Until then a host can run a session with `TestPattern` (the headset test does).

## Manual headset test

Prerequisites: a PC VR headset on Windows (Quest via Link/Air Link, Rift, Index,
Vive) or Linux (Monado/SteamVR), its runtime set as the active OpenXR runtime,
and a Vulkan GPU.

1. Start the runtime (Oculus app with Link active, or SteamVR) and put the
   headset on (proximity sensor covered) so the session can reach FOCUSED.
2. Hold both controllers.
3. From the repository root run (one cargo job, as the CI resource rules require):

   ```text
   cargo test -p engine-xr --test runtime -- --ignored --nocapture headset_session_renders_tracks_and_vibrates
   ```
4. Expected: the headset shows an orange/dark checkerboard in the left eye and a
   cyan/dark checkerboard in the right eye (64-pixel cells) for 180 focused
   frames; both controllers vibrate for 200 ms; the test then requests exit and
   passes. It asserts FOCUSED was reached, both eyes were rendered with distinct
   poses for 180 frames, the head pose was tracked and non-identity, both
   haptic pulses were accepted, and the session finished.
5. Record the printed system, notes and tracking lines with the result.

Without a headset, run the honest-status probe instead:

```text
cargo test -p engine-xr --test runtime -- --ignored --nocapture installed_runtime_without_a_headset
```

GPU helper checks (no headset; any Vulkan GPU):

```text
cargo test -p engine-xr --test gpu -- --ignored --test-threads=1
```

## Limitations

- Scene rendering in the headset waits for the renderer integration above.
- The built-in negotiator loads no API layers.
- Vulkan only (`XR_KHR_vulkan_enable2`); no D3D11/D3D12 or OpenGL bindings.
- One projection layer, one swapchain per eye, no multiview, no depth
  submission (`XR_KHR_composition_layer_depth`), no foveation or quad layers.
- No hand-joint tracking (`XR_EXT_hand_tracking`), eye tracking or passthrough.
- Swapchains are sized once per session; changing `render_scale` needs a new session.
- The Vulkan binding, swapchain wrapping, frame loop, poses and haptics have not
  run against a headset on the development machine (none connected).
