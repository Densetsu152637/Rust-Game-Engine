# Scene rendering

`Runtime::extract_scene_frame` produces immutable `SceneRenderFrame` snapshots.
The desktop path renders shadow maps of the scene's lights
([lights and shadows](rendering-lights.md)), depth-tested 3D meshes
(optionally through [DLSS Super Resolution](#temporal-upscaling-dlss-super-resolution)),
then 2D sprites/UI, optionally with
[DLSS Frame Generation](#frame-generation-dlss-frame-generation) between
presented frames. `RenderFrame`, `Draw`, `SpriteRenderer::render`, offscreen sprite
rendering, custom WGSL and native render callbacks remain available.

## Camera and lighting

`Camera3D` is an authored/native component. Projection is tagged `perspective`
(`fov_y_radians`, `near`, `far`) or `orthographic` (`height`, `near`, `far`).
Coordinates are right-handed, Y up, camera forward negative Z, GPU depth 0 to 1.
The camera transform must be invertible. Stable authored ID order selects the
first active camera. Without one, extraction retains the existing 2D behavior.

The camera's `lighting` object holds view-wide values only: `ambient` (0..1,
default 0.08) and `exposure` (default 1), the factor from photometric light
units to output. Lights are separate `DirectionalLight`, `PointLight`,
`SpotLight` and `AreaLight` components, with their shadows; see
[lights and shadows](rendering-lights.md), including the migration from the
former camera light. Native users edit the same components; MCP component
inspection and edits expose the same schema.

```json
{"Camera3D":{"lighting":{"ambient":0.05,"exposure":1}}}
```

## Physically based materials

`MeshMaterial` supports linear RGBA `tint`, optional base-color `texture`,
`metallic` (default 0), `roughness` (default 0.5), `normal_texture`,
`normal_scale` (default 1, range 0..10), and `metallic_roughness_texture`.
Metallic/roughness factors must be finite and in 0..1. Texture references are
catalog IDs validated as texture assets. `shader` selects custom WGSL; omitting
it selects the built-in metallic-roughness shader. `unlit` bypasses lighting.
`transparent` enables straight-alpha blending, disables depth writes, and sorts
back-to-front by object origin, with stable IDs breaking ties. Opaque meshes
write depth. The existing 2D extraction forces authored materials to unlit.

The BRDF combines GGX/Trowbridge-Reitz normal distribution, separable Smith
masking-shadowing, Schlick Fresnel, and energy-weighted Lambert diffuse. The
nonmetal reflectance at normal incidence is 0.04; metal reflectance uses base
color. Perceptual roughness is squared for the GGX alpha parameter, with a 0.045
roughness floor to bound the specular singularity. Every light of the view
(clustered for local lights) contributes through `engine::lighting`. Direct lighting is linear;
RGBA8 sRGB targets encode output. Values above the target range clip. Ambient is
a simple diffuse term, not environment-map lighting or global illumination.
There is no HDR tone mapper, IBL, clear coat, transmission or multi-scattering
compensation in this shader.

Base-color textures decode as sRGB, multiplied by linear vertex color and tint.
Metallic-roughness data uses **B for metalness and G for roughness**, multiplied
by the scalar factors. Normal maps use linear RGB mapped from 0..1 to -1..1,
positive Y convention, with `normal_scale` applied to XY. Data textures use a
linear GPU view of their original bytes, even when the same image is also used
as an sRGB base color. Sampling currently remains nearest/clamp without mips.

JSON meshes may provide `normals`, one finite vector per vertex. An empty array
uses flat geometric normals derived from transformed triangle positions. Source
normals use the inverse-transpose model transform, including nonuniform scale;
CPU skinning preserves deformed normals. Surfaces are two-sided. Tangent frames
for normal maps are reconstructed from world-position/UV derivatives; degenerate
UV triangles retain their geometric/smooth normal. Authored tangent vectors and
MikkTSpace-specific tangent seams are not consumed.

The model importer maps glTF base-color, metallic/roughness factors and textures,
normal textures/scale and source normals into renderable assets. These textures
must share UV set 0 or 1; conflicting sets are rejected rather than remapped
silently. Emissive, occlusion, alpha-mask/cutoff, sampler modes and material
extensions remain preserved in the model asset but are not implemented by this
material shader. The import warning reports these limits.

The model and texture-channel conventions follow the
[Khronos glTF 2.0 specification, materials and Appendix B](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#materials)
(accessed 2026-10-24). This renderer implements the specified base BRDF components;
it does not claim complete glTF rendering conformance.

## Shadows

Shadow maps are per light: cascaded shadow maps for directional lights, a
perspective view for spot lights and six cube faces for point and area lights,
all in one per-view shadow atlas with PCF or PCSS filtering. They are documented
with the lights in [lights and shadows](rendering-lights.md); the former
fixed-volume camera shadow (`lighting.shadows`) is superseded by cascades.

`MeshMaterial.cast_shadows` and `receive_shadows` default to true. Custom vertex
shader displacement is not replayed in the shadow pass; such materials should
opt out of casting or provide a native implementation. Transparent draws do not
cast shadows. Native render passes still receive the shared device/queue and
run after engine draws, including the 2D overlay of an upscaled frame.

## Temporal upscaling (DLSS Super Resolution)

`Camera3D.upscaler` requests a temporal upscaler for the 3D scene:

```json
{"Camera3D":{"upscaler":{"mode":"dlss_sr","quality":"quality"}}}
```

`mode` is `none` (default) or `dlss_sr`; `quality` is `ultra_performance`,
`performance`, `balanced`, `quality` (default) or `dlaa`. DLAA keeps the render
size equal to the output size; the other presets use the render size NGX
chooses. Unknown values and fields are rejected. `component_schema` documents
the object and `component_set` edits it like any other `Camera3D` field. A
request is never a claim: the scene renders natively unless an evaluation
succeeds.

### Runtime control and status

The runtime always holds an `UpscalerControl` ECS resource. Native callbacks may
set `override_settings` (it replaces the active camera's authored request while
`Some`) and `reset_history` (a one-shot request to discard temporal history,
for example after a camera cut; the desktop runner consumes it). After every
presented frame the host writes `status`:

| Field | Meaning |
| --- | --- |
| `requested` | Effective mode/quality of that frame |
| `state` | `not_rendered` (no renderer presented a 3D frame, for example headless runs), `native`, `active` (this frame's DLSS evaluation succeeded) or `fallback` |
| `fallback_reason` | Why a request rendered natively; at most 512 bytes |
| `render_size`, `output_size` | NGX input and 3D viewport sizes of an active frame |
| `evaluated_frames` | Successful evaluations by this renderer |
| `history_resets` | Evaluations submitted with reset |
| `adapter` | GPU name reported by the NGX context |

`Runtime::upscaler_status()` and `RuntimeSnapshot.upscaler` report it (absent
without an active `Camera3D`); before any presented frame the state is
`not_rendered` with the current request. `project_run_headless` returns it as
`upscaler` within the same 64 KiB bound as other telemetry, and
`render_screenshot` returns the status of its rendered image.

Fallback reasons are specific, for example: engine built without the `dlss`
feature; NGX adapter not built (no SDK at build time); a non-Vulkan graphics API
selected; DLSS device preparation failed (non-NVIDIA adapter, missing runtime
library, missing extension); the device was created without DLSS extensions;
NGX initialization, feature creation or evaluation failed. A failure for one
quality/output size is not retried every frame; changing either retries.

### Device preparation and host configuration

NGX requires Vulkan extensions at device creation, before the `GpuContext`
exists. The desktop runner and `render_screenshot` prepare a DLSS-capable device
when the first extracted frame requests `dlss_sr` or a `DlssConfig` resource
inserted before launch sets `prepare_device` (for games that enable DLSS later;
it prepares Super Resolution and Frame Generation).
Enabling DLSS at runtime on an unprepared device falls back with that reason.
`DlssConfig` also holds the nonsecret NGX `project_id` (an engine default is
used otherwise; ship your own) and optional `runtime_dir`/`log_dir`. Unset
directories use `NGX_RUNTIME_DIR`/`NGX_LOG_DIR`, else the executable's directory
and `<temp>/engine-ngx-logs` (created). Machine paths never belong in scenes.
Custom hosts call `request_upscaler_gpu(instance, surface, options, &config,
request)` with a `DlssRequest`, then `GpuRenderer::with_upscaler_device`;
`upscaler_status()` and `reset_temporal_history()` expose the same state. Any preparation failure
creates the ordinary device and keeps the reason.

Build with `--features engine-desktop/dlss` (forwarding to
`engine-dlss/ngx-vulkan`) plus the SDK variables of [DLSS](dlss.md). Ordinary
builds need neither the feature nor the SDK. Only Vulkan is supported; other
graphics APIs, macOS and non-NVIDIA GPUs fall back. The tooling does not add
the feature to generated project builds.

### Frame structure

An active frame, in queue order:

1. **Color**: the 3D scene (shadow pass, opaque and transparent draws, custom
   WGSL) renders into the NGX `Rgba16Float` input at the render size. The view
   projection is premultiplied by a clip-space translation of
   `(2*jx/w, -2*jy/h)` for a Halton(2,3) jitter `(jx, jy)` in render pixels
   (x right, y down), with `ceil(8 * scale^2)` phases clamped to 8..256. The
   projection aspect is that of the 3D viewport (output) size.
2. **Depth and motion**: opaque meshes are re-rasterized with the same jittered
   matrix by the engine motion shader into an `Rgba32Float` intermediate: clip
   depth (0..1, not inverted) and current-to-previous motion in render pixels,
   from unjittered current and previous camera matrices and each mesh's
   previous model matrix (matched by stable draw ID; a new ID has camera
   motion only). A native pass splits it into the NGX `R32Float` depth and
   `Rg16Float` motion inputs (motion scale 1, render-resolution vectors).
3. **Evaluate**: `Session::evaluate` with the jitter, measured frame time and
   reset flag; it waits for GPU completion.
4. **Composite**: the display-size output is written into the 3D viewport. As
   in the native path there is no tone mapper; values above the target range
   clip.
5. **Overlay**: 2D sprites and UI render at output resolution over the
   composite (`FrameDesc.preserve_color`), then native render passes.

History resets on the first frame of a session, quality or output-size change,
any projection change, an explicit reset request, a scene revision change in
the desktop runner, and any frame rendered without DLSS in between.

### Limitations

- Transparent meshes write neither depth nor motion; they inherit the motion of
  the opaque surface behind them.
- Custom vertex displacement and CPU-skinned or geometry-override deformation
  are not in the motion vectors: such meshes report camera and model-matrix
  motion only.
- In an upscaled frame, 2D world-space sprites are drawn after upscaling and no
  longer depth-test against 3D geometry.
- One NGX runtime may exist per process; a second DLSS renderer falls back.
- The NGX adapter copies inputs/outputs and waits for each evaluation. No frame
  rate improvement is claimed; measure before relying on one.
- Editor viewports (`OffscreenSprites`) render natively. Only Windows x86_64 on
  an RTX 5080 has been validated.

## Frame generation (DLSS Frame Generation)

`Camera3D.frame_generation` requests generated frames between rendered frames:

```json
{"Camera3D":{"frame_generation":{"mode":"dlss_fg","multiplier":2},
  "upscaler":{"mode":"dlss_sr","quality":"quality"}}}
```

`mode` is `off` (default) or `dlss_fg`; `multiplier` (2..=4, default 2) is the
number of presented frames per rendered frame, so 2 inserts one generated
frame and 4 inserts three. Scene validation rejects other multipliers, and
`component_set` edits the field. At runtime the renderer also checks NGX's
`MultiFrameCountMax` (5 on the tested RTX 5080 / driver 617.14); a larger
request falls back with that limit in the reason. Frame generation works with
or without Super Resolution: without SR, depth and motion are rendered at the
3D viewport size; with SR they are the NGX SR inputs at the render size.

### Control and status

`FrameGenerationControl` is the sibling of `UpscalerControl`:
`override_settings` (an invalid override is ignored), `reset_history`, and
`status`. Either control's `reset_history` resets both histories.
`FrameGenerationStatus` reports `requested`, `state` (`not_rendered`, `off`,
`active`, `fallback`), `fallback_reason` (at most 512 bytes), `multiplier` and
`multi_frame_max` when known, `generated_frames` (presentable frames NGX
produced), `presented_frames` (generated frames a window host presented) and
`history_resets`. Only frames returned by a successful NGX evaluation as
presentable are counted. `RuntimeSnapshot.frame_generation`,
`project_run_headless` (`frame_generation`, same 64 KiB bound) and
`render_screenshot` (`frame_generation`, `generated_frames`) report it.

Fallback reasons include those of Super Resolution plus: the device was
prepared without Frame Generation extensions; FG device preparation failed;
`FrameGenerator` creation failed (for example the FG runtime
`nvngx_dlssg.dll` is missing next to `nvngx_dlss.dll`); a multiplier above
NGX's maximum; native render passes are registered (they target the
presentation format, while generated frames use the engine's `Rgba8Unorm`
backbuffer); and an evaluation failure, after which only real frames are
presented until the request or size changes.

### Device and runtime

`DlssRequest::for_frame` asks `request_upscaler_gpu` for Super Resolution
and/or Frame Generation from the first frame, or both with
`DlssConfig.prepare_device`. Frame generation devices come from
`create_device_for_features(&[SuperResolution, FrameGeneration])`; if that
fails an SR-only device is created and FG falls back with the reason.
`FrameGenerator::with_context` shares the SR context. The shared context checks
SR availability, so frame generation needs the SR runtime as well as the FG
runtime even when `upscaler.mode` is `none`.

### Frame structure

For each real frame (sizes are those of the 3D viewport, the "output"):

1. The 3D scene renders into the HUD-less color: an `Rgba8Unorm` texture
   written through an sRGB view, so it holds the same encoded bytes as an sRGB
   swapchain. With SR, the upscaled output is composited into it; without SR,
   the scene renders at output size without jitter.
2. The engine depth/motion pass writes `R32Float` depth and `Rg16Float`
   current-to-previous pixel motion at the render size (SR inputs, or engine
   textures without SR).
3. The HUD-less color is copied into the backbuffer and 2D sprites/UI are drawn
   over it. The two images differ only where UI is drawn.
4. For indices `1..=multiplier-1`, `FrameGenerator::evaluate` generates the
   frames between the previous and this real frame, with unjittered camera
   constants in NVIDIA's row-major post-multiply convention
   (`view_to_clip`, `clip_to_prev_clip` from this and the previous
   view-projection, camera basis, near/far, FOV, aspect), motion scale 1 and
   the SR jitter in render pixels. Only `Presentable` results are kept.
5. Hosts present the generated frames in order, then the real frame.

`GpuRenderer::render_generated(frame)` renders the real frame and generates;
`generated_frame_count`, `present_generated(i, view)` and `present_real(view)`
draw them into presentation views; `capture_generated()` reads back the
generated frames followed by the real frame (sRGB RGBA8) for tests and tools.
`render_scene` draws the real frame into its target and keeps the generated
frames available for capture. `render_scene_without_generation` is the
non-generated path. `render_screenshot` with `dlss_fg` renders the
second-to-last tick first, then the final tick, and writes generated frames as
`<name>-generated-<i>.png` beside the screenshot (this path is not exercised
with an active generator by the automated tests, because tooling-built
generations do not enable the `dlss` feature).

History resets on SR's triggers (first frame, projection change, explicit
`reset_temporal_history`, scene revision change, resize or quality change via a
new generator), any frame without frame generation in between, window resume
after suspension, and a gap of more than 250 ms between real frames (pause,
menu, minimized window: no game frames). A reset frame generates nothing.

### Presentation and pacing

`WindowSurface::present_scene` presents each generated frame on its own
swapchain image, then the real frame. The first generated frame is presented
immediately; the rest and the real frame are spaced evenly over the recent
real-frame interval (`presentation_offsets`, an exponential average from
`PresentPacer`, each gap capped at 50 ms) with `thread::sleep` on the render
thread. With the default FIFO (vsync) present mode, each present also waits
for a swapchain image, so presents keep their order and real frames are
limited to about `refresh / multiplier` (expected from FIFO semantics, not
measured).

This is a simple correctness baseline. Holding the real frame until its
generated frames are shown adds at least one generated-frame interval of
latency; NVIDIA Reflex and frame-rate limiting are not integrated, and the
per-evaluation wait of the NGX adapter costs time. No frame rate or latency
improvement is claimed. Automated tests verify generation and ordering
offscreen; on-screen pacing needs a human check: run a moving scene with
`dlss_fg` in a window at vsync and at `ENGINE_PRESENT_MODE=immediate`, watch
for even motion without judder or a stuttering HUD, and compare the
`presented_frames` and real frame counts in the run report.

### Frame generation limitations

- Native render passes disable frame generation (fallback with reason).
- No UI alpha recomposition, `OutputReal`, `NotRenderingGameFrames`, HDR or
  dynamic resolution parameters; SDR `Rgba8Unorm` only.
- Generated frames inherit the motion limits above (transparent surfaces,
  vertex displacement, CPU skinning).
- Motion blur is not implemented, so the guide's motion-blur scaling does not
  apply.
- Only Windows x86_64 on an RTX 5080 has been validated; Linux FG runtime
  packaging is unvalidated.

## Shader and native contracts

Legacy vertex locations remain 0 position (`vec3<f32>`), 1 UV (`vec2<f32>`), and
2 color (`vec4<f32>`), at byte offsets 0, 12 and 20. Location 3 adds an optional
normal (`vec3<f32>`) at byte 36; the desktop upload stride is now 48 bytes.
Entry points remain `vs_main` and `fs_main`. Uniforms are supplied only when
reflected as used by the selected shader:

| Group/binding | Value | Bytes |
| --- | --- | --- |
| 0/0 | projection x view x model, column-major | 64 |
| 0/1 | model-to-world matrix | 64 |
| 0/2 | inverse-transpose model matrix | 64 |
| 2/0 | tint vec4; options vec4: unlit, metallic, roughness, receive-shadow | 32 |
| 2/1 | light VP matrix; eye, direction, radiance/ambient, shadow enable/bias vec4s | 128 |
| 2/2 | normal-map enable/scale, metallic-roughness-map enable, reserved | 16 |

Texture 1/0 and nearest clamp sampler 1/1 retain the existing contract. Linear
data textures use 1/2 and 1/3; the generated `texture_depth_2d` shadow map uses
3/0. Other resource types/uniform slots, wrong sizes, missing vertex inputs and
invalid WGSL fail before submission. `DrawCommand.shadow` exposes the neutral
caster matrix/resolution to native producers; all casters in a frame use one
resolution. The recording backend records commands but does not generate pixels.

## Verification and example

Run `cargo run -p engine-desktop --example pbr_shadows -- output.png`, or use
`--window`, for the authored scene in `examples/pbr-shadows`.

`cargo test -p engine-desktop --test scene3d -- --include-ignored --nocapture`
runs real GPU readback assertions for depth, custom WGSL/2D, material factors,
normal/MR maps and linear channel handling, cascaded shadow occlusion without
acne, caster/receiver switches, and shadow-map resizing. GPU tests fail if an
adapter is unavailable. Light and shadow measurements are in
[lights and shadows](rendering-lights.md#measured-results).
Set `ENGINE_PBR_CAPTURE` to an absolute PNG path to retain the shadow test image.
Project rendering/import tests cover authored round trips, invalid settings and
asset kinds, texture mappings, and normal-array boundary failures.

`cargo test -p engine-desktop --test frame_generation -- --ignored --nocapture`
checks on any adapter that frame generation without a prepared device presents
real frames only with a reason. With `--features dlss`, the SDK and a runtime
directory holding both `nvngx_dlss.dll` and `nvngx_dlssg.dll`, the NGX test
renders a moving engine scene (640x384) through `GpuRenderer` and inspects the
captured frames. On 2026-10-24 (RTX 5080, runtime 310.9.16, MultiFrameCountMax
5) generated frames lay between consecutive real frames: maximum centroid error
from even spacing 0.38 px for 2x object motion (step 12.5 px), 0.18 px for a
2x camera pan, 0.36 px for 4x (three frames per 25 px step) and 0.16 px for
SR (427x256 render) plus 2x. A generated SR+FG frame differed from a native
render of the midpoint state by 0.28/255 on average. HUD pixels in generated
frames matched the real HUD (within 2 levels at its edges). A cut and a 300 ms
pause each produced a reset frame without generated frames. Passing pixels,
clip units, zero or negated SR jitter as FG `JitterOffset` produced identical
generated frames on this scene, so the jitter unit is not observable here; the
renderer passes render pixels, the SR convention.

`cargo test -p engine-desktop --test upscaler -- --ignored --nocapture` checks on
any adapter that a DLSS request without a prepared device renders the native
image with a reported reason, and that a non-Vulkan selection says so. With
`--features dlss`, the SDK at build time and `NGX_RUNTIME_DIR`/`NGX_LOG_DIR`, two
more tests render engine scenes through NGX. On 2026-10-24 (RTX 5080, Windows,
640x384 output) quality mode rendered at 427x256: after eight frames the mean
absolute channel difference from the native image was 0.26/255, the red
triangle centroid was within 0.9 px, HUD pixels were identical, and DLAA rendered
at 640x384. On a panning, moving one-texel checker scene, temporal accumulation
reached about half the error of resetting history every frame (10.99 vs 21.58,
ratio 0.51). In manual experiments zero motion vectors measured a ratio of 0.63
and sign-flipped vectors 0.96, and flipping either jitter axis increased the
static error, supporting the jitter and motion sign conventions. These checks
are not an image-quality study.
