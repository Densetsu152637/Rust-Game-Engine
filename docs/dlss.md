# DLSS native integration

The optional `engine-dlss` crate performs actual NVIDIA NGX Super Resolution (SR)
on Vulkan and NGX DLSS Frame Generation (FG), including multi-frame generation.
FG produces generated frames; presenting them is the renderer's job (see
[Frame Generation](#frame-generation)).
Ordinary workspace builds do not need the NVIDIA SDK, libraries,
models, or GPU. Build availability is separate from device support and from
successful GPU evaluation. Explicit requests return errors, never a simulated
success. Hosts retain their normal rendering path when setup fails.

## Feature scope

| Feature | This slice | Required renderer data |
| --- | --- | --- |
| Super Resolution / DLAA | Native NGX execution and desktop 3D renderer integration (`Camera3D.upscaler`), tested on Windows RTX 5080 | Jittered HDR color, clip depth, current-to-previous motion, temporal reset |
| Frame Generation (2x and multi-frame) | Native NGX generation and desktop renderer/window integration (`Camera3D.frame_generation`, with or without SR), generation tested offscreen on Windows RTX 5080; on-screen pacing is a simple baseline needing human review | Backbuffer and HUD-less color, depth/motion, unjittered camera constants, reset, presentation pacing |
| Ray Reconstruction | Separate integration pending | Noisy ray-traced lighting, diffuse/specular albedo, normals/roughness, depth/motion, camera matrices |
| DLSS 5 neural rendering | Deferred by user: no SDK access | Requires NVIDIA's neural-rendering SDK/runtime/model access; do not infer its ABI |

SDK support is not automatically engine support. In particular, the built-in
raster renderer does not produce ray-traced lighting merely because a normal map
or shadow map exists. SR output alone is not generated-frame presentation.

## Build and runtime configuration

Use the official [NVIDIA DLSS SDK v310.9.16](https://github.com/NVIDIA/DLSS/tree/v310.9.16)
and [Vulkan-Headers](https://github.com/KhronosGroup/Vulkan-Headers). SDK source,
loader libraries and runtime binaries are intentionally not committed. Read the
[SDK license](https://github.com/NVIDIA/DLSS/blob/v310.9.16/LICENSE.txt) and retain
its redistribution notices when packaging the official runtime with a game.

Set `NGX_SDK_DIR` to the SDK root (containing `include/` and `lib/`) and
`VULKAN_HEADERS_DIR` to the Vulkan-Headers `include/` directory, then build with
`--features engine-dlss/ngx-vulkan`. A C++17 toolchain is required. The bridge uses
the official SDK's headers and static loader library; it does not redeclare the
proprietary SDK's structure ABI in Rust. Windows MSVC and Linux x86_64/aarch64
build paths are provided. Only Windows x86_64 execution has been validated.
macOS and other unsupported platforms retain normal rendering.

Without `NGX_SDK_DIR`, even a feature-enabled build compiles an unavailable
adapter. `build_support(Feature::SuperResolution)` explains that state. It does
not inspect a GPU or claim an active feature. Invalid explicitly supplied SDK
or Vulkan-Headers paths fail the build with a diagnostic. On Windows the
official loader needs the MSVC toolchain: other Windows target environments
(for example `gnu`) emit a cargo warning and build the unavailable adapter.

At runtime, `SessionOptions` carries the application's nonsecret project ID,
absolute existing official-plugin and log directories, output dimensions and
quality. Never store developer machine paths or credentials in an authored
scene. An NGX project ID is an application identity, not a secret key, and must
be GUID-shaped (`8-4-4-4-12` hexadecimal digits); other values are rejected
before NGX loads. The runtime directory contains `nvngx_dlss.dll` on Windows or
`libnvidia-ngx-dlss.so.310.9.16` on Linux. The Linux name is deliberately pinned
to the SDK version this bridge was built against; a different runtime version
needs that name updated in `native.rs` after it is validated. Frame generation
additionally needs `nvngx_dlssg.dll` (Windows only checked;
`libnvidia-ngx-dlssg.so.<ver>` on Linux is unvalidated). Missing or malformed
files fail before native loading. This header check is not a signature check; use only trusted
NVIDIA packages. The tested Windows runtime's Authenticode signature was valid
and belonged to NVIDIA Corporation.

## Device and lifetime contract

Call `create_device(adapter, descriptor, options)` before constructing the host
GPU context. It queries NGX's required Vulkan extensions, checks hardware
support and requests those extensions before device creation. In the tested
configuration, a normal wgpu device failed with a precise
`VK_NVX_binary_import` missing-extension error; silently trying NGX on that
device would be incorrect. `create_device_for_features` queries the union for
explicit feature requests, but does not initialize those features. Unknown
extension requirements fail explicitly pending review.

`Context::new` and the feature constructors are safe functions that enforce
their own invariants:

- the device and queue must match;
- the cloned device keeps the Vulkan device and instance alive;
- destroyed exposed textures make later evaluations fail instead of reaching
  native code;
- `Rc` confines contexts and features to the owning thread, where NGX calls and
  their submissions stay;
- one NGX runtime per process is enforced for this crate.

The constraint the crate cannot check is another NGX integration (for example
Streamline, or a second copy of this bridge) initializing NGX in the same
process or on the same device. That configuration is unsupported. Feature adapters share
cloned contexts and own distinct parameter maps/feature handles. The final
context releases NGX only after the last feature has released its resources.

`Session::new` combines context and SR creation. `Session::with_context` creates
an SR feature on an existing context. `Session::context()` exposes that shared
owner for subsequent native feature adapters. The runtime owns initialization;
feature adapters must never call NGX Init or Shutdown independently.

Each SR evaluation initializes its output through wgpu, transitions owned
textures in a tracked command buffer, then records NGX in a separate raw command
buffer. wgpu 30 forbids mixing raw and wgpu commands in one encoder. HAL resource
handles are read sequentially because wgpu's device resource lock is not
recursive. The owned resources remain alive through submission and completion.
Pre-copy, native work and post-copy are submitted together, preserving queue
ordering even if another thread submits ordinary wgpu work. Evaluation waits
for GPU completion. The additional copies and synchronous wait have a measurable
performance cost; this conservative baseline is not a claim
of improved frame rate. An NGX evaluation failure poisons that session rather
than reusing uncertain temporal state. Recreate it before retrying.

## Renderer handoff

`Session::status().render_size` is the runtime's optimal input size, not a fixed
quality ratio. Render the 3D scene to `inputs()` in this order:

1. `Rgba16Float`: linear HDR scene color, before HUD/UI and before a separate TAA.
2. `R32Float`: ordinary clip depth in `[0,1]` (not inverted or linear distance).
3. `Rg16Float`: current-to-previous pixel motion including object and camera
   motion, excluding projection jitter.

Supply projection jitter in render pixels, motion scale, positive elapsed
milliseconds, and reset after a camera cut, resize, quality change, or history
invalidation. First evaluation resets automatically. Submit producers before
`evaluate`. Its output is display-size `Rgba16Float`; composite/tonemap it before
rendering HUD/UI at display resolution.

The desktop renderer implements this handoff behind the `engine-desktop/dlss`
feature: Halton jitter, an engine depth/motion pass, history resets, a pre-HUD
composite, device preparation before the GPU context, authored/MCP/native
configuration and a reported native fallback. See
[Temporal upscaling](rendering3d.md#temporal-upscaling-dlss-super-resolution)
for its contract and limits. It uses the public `Session` API unchanged.

`Status` reports adapter identity, raw Vulkan driver version, NGX minimum driver,
compiled NGX API version, render/output sizes and successfully evaluated frame
count. A zero count means initialized, not evaluated. Runtime plugin version is
`None` when unqueried; no marketing generation is inferred from hardware or API
version. The engine's `UpscalerStatus` (runtime snapshot and MCP run reports)
preserves these distinctions: it reports `active` only for a frame whose
evaluation succeeded and counts successful evaluations.

## Validation

Default tests: `cargo test -p engine-dlss`. SDK-enabled tests:
`cargo test -p engine-dlss --features ngx-vulkan`. With `NGX_RUNTIME_DIR` pointing
to the official SR plugin directory and `NGX_LOG_DIR` an existing writable
absolute directory, run the hardware test explicitly:

```text
cargo test -p engine-dlss --features ngx-vulkan --test native_gpu -- --ignored --nocapture
```

The FG hardware test uses the same environment variables, and `NGX_RUNTIME_DIR`
must also contain `nvngx_dlssg.dll`. It renders a white 16-pixel bar that moves
24 pixels per frame, then requires the following:

- 2x: each generated bar's brightness centroid lies within 1 pixel of the
  midpoint between the previous and current positions, and its brightness stays
  within 10% of one bar's.
- Every pair, including the first one after the initial reset, is checked.
- 4x: when NGX reports multi-frame support, indices 1..=3 lie within 1 pixel of
  the 1/4, 2/4 and 3/4 positions.
- The static 8-pixel checkerboard background, away from the bar's path, is
  reproduced exactly (mean and maximum channel error 0).
- A reset returns `NoHistory` and outputs the current frame.
- A wrong-size output, an out-of-order index and a backbuffer from another
  device are rejected with `InvalidInput` before native work, without poisoning
  the generator. A destroyed output fails safely.

```text
cargo test -p engine-dlss --features ngx-vulkan --test framegen_gpu -- --ignored --nocapture
```

On 2026-10-24 the FG test passed on the RTX 5080 / driver 617.14 with the SDK's
`nvngx_dlssg.dll` 310.9.16 (see [Frame Generation](#frame-generation)).

On 2026-10-24 the SR hardware test created and evaluated the real SR feature for
three frames on NVIDIA GeForce RTX 5080 / driver 617.14. NGX selected 512x341
input for 768x512 quality output. Readback of the uniform HDR scene retained
FP16 red `0x3a00` (0.75) and green `0x2fff` (approximately 0.125). This checks
actual native GPU execution and readback, not image-quality coverage for motion,
disocclusion, transparent materials or presentation. The test also evaluates a
second feature sharing the context after the first feature is dropped, then
destroys an exposed staging input and confirms a controlled error without
another successful native evaluation. Linux/macOS execution and
full authored-game temporal behavior require their own validation.

The engine renderer tests run with the same environment:

```text
cargo test -p engine-desktop --features dlss --test upscaler -- --ignored --nocapture
```

They render engine 3D scenes through `GpuRenderer`, check `active` status,
evaluated-frame and reset counts, DLAA render size, agreement with the native
image, an unchanged HUD, and that temporal accumulation through the engine's
motion vectors beats per-frame resets on a panning scene. Results are in
[Scene rendering](rendering3d.md#verification-and-example).

## Frame Generation

`FrameGenerator` (`src/framegen.rs`, `src/framegen/native.rs`,
`native/ngx_dlssg.cpp`) drives NGX `NVSDK_NGX_Feature_FrameGeneration` through
the official `nvsdk_ngx_helpers_dlssg_vk.h` create/evaluate helpers. Like SR, it
never redeclares NVIDIA structures in Rust. The bridge passes only engine-owned
records (`FgCaps`, `FgFrame`, `FgResources` in `native/ngx_bridge.h`). The
reference is NVIDIA's *DLSS Frame Generation SDK* guide, version 310.7.0
(`doc/DLSS-FG Programming Guide.pdf` in the SDK). NVIDIA recommends Streamline
for FG, but the guide documents this direct NGX path.

### Status and evidence (2026-10-24, RTX 5080, driver 617.14)

- `create_device_for_features(.., &[SuperResolution, FrameGeneration])` creates a
  device. NGX requested no FG extension outside the reviewed allowlist.
- With the official SDK runtime (`nvngx_dlssg.dll` 310.9.16.0, Authenticode
  signed by NVIDIA Corporation) next to `nvngx_dlss.dll`, NGX reports:
  - FG available;
  - minimum driver 520.0;
  - `MultiFrameCountMax` 5.
- Measured on real NGX, with no simulated output:

  | Mode | Expected positions | Measured centroids |
  | --- | --- | --- |
  | 2x | 83.5, 107.5, 131.5 (midpoints) | 83.50, 107.50, 131.50 |
  | 4x (bar 143.5 to 167.5) | 149.5, 155.5, 161.5 | 149.50, 155.50, 161.50 |

  Each generated frame kept one bar's brightness (about 2049 of 2048), and a
  reset returned the current frame. Generation is exact only for this synthetic
  scene: it has pixel motion vectors, a static camera and no disocclusion
  complexity.
- Runtime selection matters:
  - With only the SR runtime present, NGX reports
    `FrameGeneration.Available=0` and `FeatureInitResult=0xBAD00004`
    (FeatureNotFound). `FrameGenerator::new` rejects a runtime directory
    without `nvngx_dlssg.dll` before loading NGX, and `with_context` maps
    FeatureNotFound to a precise missing-runtime error.
  - The driver-bundled fallback `nvngx_dlssg.dll` 310.2.1 creates the feature,
    but every evaluation fails with `NVSDK_NGX_Result_FAIL_InvalidParameter`
    (`0xBAD00005`). Its log reports "BackbufferExtent should be constant for
    the lifetime of the feature. (0, 0) != (256, 128)".
  - Ship the SDK's runtime, not the driver fallback.

### API

- `FrameGenerator::new(device, queue, &SessionOptions, FrameGenerationOptions)`
  initializes the shared NGX runtime. Its `runtime_dir` must contain both
  `nvngx_dlss.dll` and `nvngx_dlssg.dll`, because the shared `Context` checks SR
  availability during initialization.
- `FrameGenerator::with_context(context, &SessionOptions, options)` adds FG to an
  existing SR context. The session options must be the ones used for the
  context. It queries NGX's FG Vulkan instance and device extension
  requirements and fails with the missing extension's name when the context
  device lacks one (create it with `create_device_for_features`).
- Both create the NGX feature before returning, so `multi_frame_max` is final
  before the first pair. Both are thread-confined (`Rc` context), NGX is not
  thread-safe, and both follow the context lifetime rules above.
- `FrameGenerationOptions` holds:
  - the output (backbuffer) size;
  - the render (depth/motion) size, no larger than the output;
  - `ColorFormat::{Rgba8Unorm, Rgba16Float}`;
  - whether a HUD-less color is always supplied.

  As the guide requires, any change (resize, render-size change) needs a new
  generator. Dynamic resolution is disabled.
- `evaluate(&GenerationInputs, &GenerationFrame, &wgpu::Texture)` takes the
  caller's single-sample 2D `COPY_SRC` textures: backbuffer and HUD-less in the
  color format at output size, `R32Float` depth and `Rg16Float` motion at render
  size. It copies them into private textures and records NGX into a raw command
  buffer. It then copies the generated frame into the caller's `COPY_DST`
  output, submits and waits.
  - Input copies run in their own validated submission, together with a copy
    into the output (which generation then overwrites). Destroyed, mismatched or
    other-device textures are rejected with `InvalidInput` before native work,
    and the generator stays usable.
  - As with SR, native handles refer only to private resources.
  - A native or GPU failure poisons the generator.
- `Generated` reports how the frame should be used:
  - `Presentable`: a previous real frame exists and NGX's
    `OutputDisableInterpolation` byte was zero.
  - `NoHistory` (reset or first frame), `Disabled` (NGX disabled interpolation
    and copied the backbuffer) and `Unreported`: present only the real frame.
- The disable buffer is filled with `0xff` before every evaluation. An intact
  sentinel means NGX did not report, which yields `Unreported` rather than
  `Presentable`. With runtime 310.9.16, NGX wrote the flag on index 1 of a
  multi-frame pair and left the sentinel on indices 2..=k, so later indices
  inherit the pair's index-1 decision.
- `FrameGenerationStatus` reports:
  - adapter and raw driver version;
  - NGX minimum FG driver;
  - `MultiFrameCountMax`, re-read after feature creation;
  - sizes, format and successful evaluation count.

### Per-frame data contract

`GenerationFrame` carries:

- unjittered camera matrices (`view_to_clip`, `clip_to_view`,
  `clip_to_lens_clip`, `clip_to_prev_clip`, `prev_clip_to_clip`) as
  `float[4][4]` in row-major order assuming post-multiplication (`clip = M * v`
  with column vectors), as the guide specifies;
- camera position and unit basis vectors, near/far, FOV in radians and aspect;
- projection jitter (NGX `JitterOffset`);
- `motion_scale` (NGX `MvecScale`);
- `reset`, inverted-depth, camera-motion-included, HDR and orthographic flags;
- the multi-frame `Interpolation { index, count }`.

Motion vectors are current-to-previous screen-space motion **in pixels at the
motion texture's resolution**. `motion_scale` rescales stored values into those
units: 1 for pixel motion (the same convention as the SR input), the texture
size for UV-space motion, and a negative value flips an axis. The API reference
section and the header comment still say "normalize ... to [-1,1]". The
integration and parameter sections say pixels, and the GPU test confirms
pixels. An earlier run used `1/render_size` together with transposed
projection matrices, and its 2x centroids missed the midpoint by about 1 pixel,
with uneven 4x spacing. With pixel scale and guide-convention matrices, the
positions were exact. The two changes were not isolated, but the test camera is
static (`clip_to_prev_clip` is identity).

Motion vectors must exclude jitter (`MvecJittered` is set to 0). The bridge sets
`motionVectorsInvalidValue` to `FLT_MAX` because the guide gives no default and
the struct's zero would mark every static pixel invalid. `FLT_MAX` is not
representable in the FP16 motion texture, so no stored motion is treated as
invalid. On the synthetic scene, 0 and `FLT_MAX` produced identical output: the
static textured background was exact either way. The setting is therefore a
contract fix that this test does not observe. The guide
describes `jitterOffset` only as a "clip space jitter offset" without a unit.
The test uses zero jitter, so a jittered SR plus FG pipeline must confirm the
unit during renderer integration.

Validation rejects:

- non-finite values and zero motion scale;
- `near <= 0`, `far <= near` and a non-positive aspect;
- a perspective FOV outside (0, pi);
- out-of-range interpolation indices.

`evaluate` also enforces NVIDIA's sequencing rule (`check_sequence` in
`src/framegen.rs`):

- Each pair evaluates indices `1..=count` in order with one count, with no
  skipped index and no count change mid-pair.
- A new pair starts at index 1 once the previous pair is complete. An
  unfinished pair can be abandoned only by a new index 1 with `reset`.
- `reset` is accepted only on index 1.
- Violations are `InvalidInput` and do not poison the generator.
- A count above `multi_frame_max` is `Unavailable`.

### Presentation contract for the renderer

Generating a frame is not presenting it. Per the guide, NGX FG does not handle
timing or presentation, and this crate owns no swapchain. For each real frame N:

1. Render the scene to HUD-less color, depth and motion. Then composite UI onto
   the backbuffer, which is the image that would be presented without FG and
   includes post-processing and UI.
   - HUD-less must equal the backbuffer for every non-UI pixel.
   - Without HUD-less input, UI may smear in generated frames. Either provide
     HUD-less, or keep UI out of the backbuffer and composite it afterwards onto
     both real and generated frames.
2. Instead of presenting N, call `evaluate` with N's inputs. It generates the
   frame between N-1 and N. Keep N's backbuffer, which `evaluate` does not
   modify, until it is presented.
3. On `Presentable`, present the generated frame as soon as `evaluate`
   completes, then present N so that intervals are about equal. With
   multi-frame (`count` k ≤ `multi_frame_max`), call `evaluate` for indices
   1..=k in order with the same inputs. Present each generated frame in order,
   then N, at roughly 1/(k+1) spacing. Present the first generated frame
   without waiting for the rest. On `NoHistory` or `Disabled`, present only N.
4. Set `reset` whenever the current frame's content is unrelated to the
   previous frame: camera cuts, teleports, scene loads, history invalidation,
   and resuming after menus, pauses or cutscenes. Stop calling `evaluate` while
   no game frames are rendered. Size, format and HUD-less changes require a new
   generator, whose first evaluation resets automatically.
5. Halve motion blur for 2x, or divide it by the multiplier for multi-frame
   generation, as the guide recommends.

Holding frame N adds latency; the guide pairs FG with NVIDIA Reflex, which this
crate does not provide. The synchronous submit-and-wait and the extra copies
are a correctness baseline, not a performance claim.

### Resource states and synchronization (unverified)

The guide states resource states only for D3D12: inputs in
`NON_PIXEL_SHADER_RESOURCE`, outputs (including the disable buffer) in
`UNORDERED_ACCESS`. It specifies no Vulkan layouts and no post-evaluate state.

The adapter uses the Vulkan equivalents through wgpu's tracked transitions:
inputs as sampled resources (`RESOURCE`) and the output and disable buffer as
storage (`STORAGE_READ_WRITE`). It assumes NGX leaves these states unchanged
when it returns, then records wgpu copies after the raw command buffer in the
same submission.

No Vulkan validation layers are installed here, so these layouts, and the
barriers between wgpu's and NGX's commands, have not been checked. The
required check is a run of both GPU tests with `VK_LAYER_KHRONOS_validation`
enabled with synchronization validation
(`VK_LAYER_ENABLES=VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT`),
expecting no errors. Correct readback results do not establish correct
synchronization.

### Renderer integration

The desktop renderer implements the HUD-less/UI split, `R32Float`/`Rg16Float`
depth and motion, unjittered row-major camera constants with SR's jitter passed
in render pixels, shared reset triggers plus a no-game-frames gap, and
in-order window presentation with sleep-based even spacing. See
[Frame generation](rendering3d.md#frame-generation-dlss-frame-generation) for
the contract, evidence and limits. The jitter unit could not be confirmed:
pixels, clip units, zero and negated jitter gave identical generated frames in
the SR+FG test scene.

### Remaining for renderer integration

- presentation pacing beyond the sleep/vsync baseline (a dedicated present
  thread or swapchain timing) and human validation of on-screen pacing;
- an asynchronous lifetime to replace the per-frame wait;
- VSync and frame-rate-limit interaction, and latency reduction (Reflex);
- a context that does not require the SR runtime for FG-only use;
- native render passes with frame generation (they currently disable it);
- `OutputReal`, UI-alpha recomposition, `NotRenderingGameFrames`, HDR and
  dynamic-resolution parameters;
- Linux runtime packaging (`libnvidia-ngx-dlssg.so.<ver>`, unvalidated);
- image-quality validation (disocclusion, transparency, UI, camera motion).

## Evidence and architecture decision

Inspected on 2026-10-24:

- [DLSS release v310.9.16](https://github.com/NVIDIA/DLSS/releases/tag/v310.9.16),
  published October 16, includes public Windows/Linux SDK material.
- [NGX Vulkan API](https://github.com/NVIDIA/DLSS/blob/v310.9.16/include/nvsdk_ngx_vk.h)
  and [SR helper](https://github.com/NVIDIA/DLSS/blob/v310.9.16/include/nvsdk_ngx_helpers_vk.h)
  define capability/extension queries, resource descriptors, create/evaluate and
  release operations used by this bridge.
- [SR programming guide](https://github.com/NVIDIA/DLSS/blob/v310.9.16/doc/DLSS_Programming_Guide_Release.pdf),
  sections 2.3.1 and 5.2, requires Vulkan extension negotiation and caller-managed
  synchronization and command-buffer state. It identifies NGX as not thread safe.
- [Streamline 2.14.16 manual hooking](https://github.com/NVIDIA-RTX/Streamline/blob/main/docs/ProgrammingGuideManualHooking.md)
  requires initialization/presentation hooks. Direct NGX was selected for bounded
  integration with the existing wgpu device/resources; adopting Streamline would
  require a broader device and swapchain interception change.
- `nvsdk_ngx_defs_dlssg.h`, `nvsdk_ngx_params_dlssg.h` and
  `nvsdk_ngx_helpers_dlssg_vk.h` (v310.9.16) define FG as an NGX feature. It
  evaluates into an application output texture (`OutputInterpolated`), with an
  optional `OutputDisableInterpolation` hint buffer. Nothing in these headers
  requires a swapchain, so offscreen evaluation is a valid test, and
  presentation pacing is the application's job. The SDK's DLSS Frame
  Generation guide (310.7.0) says the feature does not handle timing or
  presentation. It recommends Streamline, but it documents the direct NGX
  integration used here.
- [Public DLSS headers](https://github.com/NVIDIA/DLSS/tree/v310.9.16/include) and
  [Streamline headers](https://github.com/NVIDIA-RTX/Streamline/tree/main/include)
  expose SR, RR and FG, but the inspected inventories contain no neural-rendering
  API. NVIDIA's [September 22 DLSS 5 developer article](https://developer.nvidia.com/blog/whats-new-for-game-developers-dlss-5-with-3d-guided-neural-rendering-nvidia-ace-updates-and-new-rtx-kit-capabilities)
  describes the feature and controls without providing that ABI. Revisit only
  when an official neural-rendering SDK and authorized runtime/model are supplied.
