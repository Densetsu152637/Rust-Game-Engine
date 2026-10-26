# Scene rendering

`Runtime::extract_scene_frame` produces immutable `SceneRenderFrame` snapshots.
The desktop path renders the 3D scene through the HDR render pipeline
([render architecture](rendering-architecture.md)): shadow maps of the scene's
lights ([lights and shadows](rendering-lights.md)), instanced depth-tested meshes
into `Rgba16Float` scene colour, registered render features, the output operator
into the target (optionally through
[DLSS Super Resolution](#temporal-upscaling-dlss-super-resolution)), then 2D
sprites/UI and overlay features, optionally with
[DLSS Frame Generation](#frame-generation-dlss-frame-generation) between
presented frames. `RenderFrame`, `Draw`, `SpriteRenderer::render`, offscreen
rendering, custom WGSL materials and native render features are available.
`Camera3D.render_features` enables or disables render features per camera
(for example `{"engine.shadows": false}`); see
[render architecture](rendering-architecture.md#authored-data-status-and-mcp).

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
opt out of casting or provide a native render feature. Transparent draws do not
cast shadows. Overlay-stage render features run after the output operator and the
2D overlay, also in an upscaled frame.

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

1. **Scene stages**: the render pipeline renders the view at the render size
   (shadow, prepass, opaque, sky and transparent stages, custom WGSL and render
   features) into its `Rgba16Float` HDR colour and `Depth32Float` depth. The
   view projection is premultiplied by a clip-space translation of
   `(2*jx/w, -2*jy/h)` for a Halton(2,3) jitter `(jx, jy)` in render pixels
   (x right, y down), with `ceil(8 * scale^2)` phases clamped to 8..256. The
   projection aspect is that of the 3D viewport (output) size.
2. **Motion**: the view requests motion vectors, so the motion prepass
   rasterizes opaque meshes with the jittered matrix into `Rg16Float`
   current-to-previous motion in render pixels, from unjittered current and
   previous camera matrices and each mesh's previous model matrix (matched by
   stable draw ID; a new ID has camera motion only).
3. **NGX inputs**: in the pipeline's host upscale hook the HDR colour is copied
   into the NGX colour input, depth is exported into `R32Float` (0..1, not
   inverted) and motion copied into the `Rg16Float` input (motion scale 1).
4. **Evaluate**: `Session::evaluate` with the jitter, measured frame time and
   reset flag; it waits for GPU completion.
5. **Finish**: the display-size output replaces the Upscale stage; HDR
   post-processing features, the output operator (by default a linear clamp,
   so values above the target range clip) and the 2D sprites/UI and overlay
   features follow at output resolution.

History resets on the first frame of a session, quality or output-size change,
any projection change, an explicit reset request, a scene revision change in
the desktop runner, and any frame rendered without DLSS in between.

### Limitations

- Transparent meshes write neither depth nor motion; they inherit the motion of
  the opaque surface behind them.
- Custom vertex displacement and CPU-skinned or geometry-override deformation
  are not in the motion vectors: such meshes report camera and model-matrix
  motion only.
- 2D world-space sprites are drawn after the 3D output and do not depth-test
  against 3D geometry (in every frame, not only upscaled ones).
- One NGX runtime may exist per process; a second DLSS renderer falls back.
- The NGX adapter copies inputs/outputs and waits for each evaluation. No frame
  rate improvement is claimed; measure before relying on one.
- Editor viewports (`OffscreenSprites`) render natively. Only Windows x86_64 on
  an RTX 5080 has been validated (before the HDR pipeline; this machine has no
  NGX SDK to re-run the SDK tests).

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
or without Super Resolution: depth and motion are exported from the render
pipeline at the render size (the 3D viewport size without SR).

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
NGX's maximum; an enabled presentation feature has not declared replay support;
and an evaluation failure, after which only real frames are presented until the
request or size changes.

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

The late-UI revision below is a source checkpoint; it has not yet been executed
against NGX. Earlier hardware observations describe the prior HUD-less/UI split.

For each real frame (sizes are the 3D viewport's output size):

1. Pin one presentation snapshot and render the native/super-resolved scene once
   with both Overlay and Composite suppressed, into SDR sRGB-view colour.
2. Export render-size depth/motion. NGX reads that UI-free image as its backbuffer
   with no separate HUD-less input (`hudless: false`).
3. Evaluate indices `1..=multiplier-1`, retaining only presentable slots.
4. `GpuRenderer::composite_presentations` applies ordered sprites/SDF, replayable
   overlay features and final filters to every generated slot and the real
   backbuffer. It snapshots aliased inputs before writes, uses separate late-view
   state and releases retained UI data on success/error.
5. Present/capture these already composed textures; presentation only blits.

An interpolation failure keeps the rendered real source in a poisoned stage,
discards generated slots, and late-composes/presents only the real output.
Completion commits history once; it does not rerender that source frame. Sticky
failure prevents further same-request evaluations; retry replaces a poisoned stage.

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

- Enabled Overlay/Composite features without replay support disable generation
  with a named reason. SDF and final colour filtering opt in; revised hardware
  FG execution remains unverified.
- No UI alpha recomposition, `OutputReal`, `NotRenderingGameFrames`, HDR or
  dynamic resolution parameters; SDR `Rgba8Unorm` only.
- Generated frames inherit the motion limits above (transparent surfaces,
  vertex displacement, CPU skinning).
- Motion blur is not implemented, so the guide's motion-blur scaling does not
  apply.
- Only Windows x86_64 on an RTX 5080 has been validated; Linux FG runtime
  packaging is unvalidated.

## Shader and native contracts

Mesh vertex locations are 0 position (`vec3<f32>`), 1 UV (`vec2<f32>`), 2 color
(`vec4<f32>`) and 3 normal (`vec3<f32>`, zero without authored normals) in a
48-byte vertex. Material shaders follow the
[material shader contract](rendering-architecture.md#material-shader-contract):
they `#import engine::material`, define `vs_main`/`fs_main` and use the engine
functions and bindings it declares (instance transforms, view, light list,
shadow visibility, the material textures at group 1 and `material`/`maps` at
group 2); declaring any other binding fails before submission with
`unsupported material binding`. The same source runs on 2D sprites with a
`MeshMaterial` (unlit, sprite implementation of `engine::material`). Invalid
WGSL, missing entry points and contract violations fail before submission. The
recording backend records sprite commands and validates 3D scenes without
drawing them. Native code adds passes as
[render features](rendering-architecture.md#feature-registration).

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
