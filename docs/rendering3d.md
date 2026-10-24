# Scene rendering

`Runtime::extract_scene_frame` produces immutable `SceneRenderFrame` snapshots.
The desktop path renders a directional shadow map, depth-tested 3D meshes
(optionally through [DLSS Super Resolution](#temporal-upscaling-dlss-super-resolution)),
then 2D sprites/UI. `RenderFrame`, `Draw`, `SpriteRenderer::render`, offscreen sprite
rendering, custom WGSL and native render callbacks remain available.

## Camera and lighting

`Camera3D` is an authored/native component. Projection is tagged `perspective`
(`fov_y_radians`, `near`, `far`) or `orthographic` (`height`, `near`, `far`).
Coordinates are right-handed, Y up, camera forward negative Z, GPU depth 0 to 1.
The camera transform must be invertible. Stable authored ID order selects the
first active camera. Without one, extraction retains the existing 2D behavior.

The camera's `lighting` object supplies one directional light: `direction` points
from the surface toward the light, `color` is linear RGB in 0..1, `intensity` is
0..100, and `ambient` is 0..1. Direction must be finite and nonzero. Default light
is `[0.4,0.7,1]`, white, intensity 3 and ambient 0.08. Native users edit the same
`Camera3D` component; MCP component inspection and edits expose the same schema.

```json
{"Camera3D":{"lighting":{"direction":[0.6,1,0.8],"intensity":3,
  "shadows":{"enabled":true,"resolution":1024,"center":[0,0,0],
             "extent":20,"depth":100,"bias":0.001}}}}
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
roughness floor to bound the specular singularity. Direct lighting is linear;
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

## Directional shadow mapping

`lighting.shadows.enabled` defaults to false. When enabled, the wgpu backend
renders opaque caster triangle geometry into a `Depth32Float` texture before
the color pass. The built-in shader projects world positions into this map and
performs a 3x3 percentage-closer filter using explicit depth comparisons and
receiver-plane depth correction for sloped surfaces. This
is an actual geometry depth pass, so moving occluders changes receiver pixels.
Shadowing affects direct lighting; the ambient term remains visible.

`resolution` must be a power of two in 64..4096 and fit the GPU limit. `center`
is the world-space shadow volume center (each coordinate within +/-1000000), `extent` its square width/height
(0.01..100000), and `depth` its length along the light direction (0.1..100000).
The volume is fixed in world space; positions outside it are lit. `bias` is a
normalized depth comparison offset in 0..0.1 (default 0.001). Adjust it with
volume depth/resolution to balance acne and detached shadows. There are no
cascades, automatic camera fitting, point/spot shadows, alpha-tested casters or
translucent shadow transmission. Transparent draws do not cast shadows.

`MeshMaterial.cast_shadows` and `receive_shadows` default to true. Custom vertex
shader displacement is not replayed in the shadow pass; such materials should
opt out of casting or provide a native implementation. Native render passes
still receive the shared device/queue and run after engine draws, including the
2D overlay of an upscaled frame.

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
inserted before launch sets `prepare_device` (for games that enable DLSS later).
Enabling DLSS at runtime on an unprepared device falls back with that reason.
`DlssConfig` also holds the nonsecret NGX `project_id` (an engine default is
used otherwise; ship your own) and optional `runtime_dir`/`log_dir`. Unset
directories use `NGX_RUNTIME_DIR`/`NGX_LOG_DIR`, else the executable's directory
and `<temp>/engine-ngx-logs` (created). Machine paths never belong in scenes.
Custom hosts call `request_upscaler_gpu(instance, surface, options, &config,
want_dlss)`, then `GpuRenderer::with_upscaler_device`; `upscaler_status()` and
`reset_upscaler_history()` expose the same state. Any preparation failure
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
normal/MR maps and linear channel handling, shadow occlusion, caster/receiver
switches, and shadow-map resizing. GPU tests fail if an adapter is unavailable.
Set `ENGINE_PBR_CAPTURE` to an absolute PNG path to retain the shadow test image.
Project rendering/import tests cover authored round trips, invalid settings and
asset kinds, texture mappings, and normal-array boundary failures.

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
