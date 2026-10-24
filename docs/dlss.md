# DLSS native integration

The optional `engine-dlss` crate performs actual NVIDIA NGX Super Resolution (SR)
on Vulkan. It also contains an NGX DLSS Frame Generation (FG) adapter whose GPU
evaluation is not yet verified (see [Frame Generation](#frame-generation)).
Ordinary workspace builds do not need the NVIDIA SDK, libraries,
models, or GPU. Build availability is separate from device support and from
successful GPU evaluation. Explicit requests return errors, never a simulated
success. Hosts retain their normal rendering path when setup fails.

## Feature scope

| Feature | This slice | Required renderer data |
| --- | --- | --- |
| Super Resolution / DLAA | Native NGX execution, tested on Windows RTX 5080 | Jittered HDR color, clip depth, current-to-previous motion, temporal reset |
| Frame Generation | NGX adapter built; capability query and feature creation verified on RTX 5080; evaluation **not verified** (blocked on the SDK-matched runtime) | Backbuffer and HUD-less color, depth/motion, unjittered camera constants, reset, presentation pacing |
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
build paths fail the build with a diagnostic.

At runtime, `SessionOptions` carries the application's nonsecret project ID,
absolute existing official-plugin and log directories, output dimensions and
quality. Never store developer machine paths or credentials in an authored
scene. An NGX project ID is an application identity, not a secret key. The
runtime directory contains `nvngx_dlss.dll` on Windows or
`libnvidia-ngx-dlss.so.310.9.16` on Linux. Missing or malformed files fail before
native loading. This header check is not a signature check; use only trusted
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

`Context::new` is the sole unsafe construction boundary. Its device and queue
must match, its Vulkan instance must remain alive, and callers must not destroy
the device or exposed textures while context clones/features exist. Keep all
queue submissions and NGX calls on the owning thread. One process runtime is
allowed, and `Rc` makes context/features thread-confined. Feature adapters share
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
rendering HUD/UI at display resolution. The native evaluation is real, but the
scene split/history/MRT integration belongs to the renderer's subsequent slice.

`Status` reports adapter identity, raw Vulkan driver version, NGX minimum driver,
compiled NGX API version, render/output sizes and successfully evaluated frame
count. A zero count means initialized, not evaluated. Runtime plugin version is
`None` when unqueried; no marketing generation is inferred from hardware or API
version. Shared scene/runtime/MCP status should preserve these distinctions.

## Validation

Default tests: `cargo test -p engine-dlss`. SDK-enabled tests:
`cargo test -p engine-dlss --features ngx-vulkan`. With `NGX_RUNTIME_DIR` pointing
to the official SR plugin directory and `NGX_LOG_DIR` an existing writable
absolute directory, run the hardware test explicitly:

```text
cargo test -p engine-dlss --features ngx-vulkan --test native_gpu -- --ignored --nocapture
```

The FG hardware test uses the same environment variables, and `NGX_RUNTIME_DIR`
must also contain `nvngx_dlssg.dll`. It renders a 16-pixel bar moving 24 pixels
per frame and requires the generated bar's brightness centroid to lie strictly
between its previous and current positions. When NGX reports multi-frame
support, it also checks that the generated positions advance in order. Finally
it checks the error for a destroyed output:

```text
cargo test -p engine-dlss --features ngx-vulkan --test framegen_gpu -- --ignored --nocapture
```

It currently fails at the first evaluation, as described in
[Frame Generation](#frame-generation).

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

## Frame Generation

`FrameGenerator` (`src/framegen.rs`, `src/framegen/native.rs`,
`native/ngx_dlssg.cpp`) drives NGX `NVSDK_NGX_Feature_FrameGeneration` through
the official `nvsdk_ngx_helpers_dlssg_vk.h` create/evaluate helpers. Like SR, it
never redeclares NVIDIA structures in Rust. The bridge passes only engine-owned
records (`FgCaps`, `FgFrame`, `FgResources` in `native/ngx_bridge.h`).

### Status and evidence (2026-10-24, RTX 5080, driver 617.14)

- `create_device_for_features(.., &[SuperResolution, FrameGeneration])` creates a
  device. NGX requested no FG extension outside the reviewed allowlist.
- With only the SR runtime present, NGX reports `FrameGeneration.Available=0`
  and `FeatureInitResult=0xBAD00004` (FeatureNotFound). `FrameGenerator::new`
  rejects a runtime directory without `nvngx_dlssg.dll` before loading NGX.
  `with_context` maps FeatureNotFound to a precise missing-runtime error.
- The installed driver package contains a WHQL-signed `nvngx_dlssg.dll` 310.2.1.
  NGX logs it as the "dlssg driver fallback snippet". With a local copy beside
  the SR runtime, NGX reports FG available (minimum driver 520.0, multi-frame
  maximum 1 before creation), and feature creation succeeds.
- **Every evaluation then fails** with `NVSDK_NGX_Result_FAIL_InvalidParameter`
  (`0xBAD00005`). The snippet log reports `cached size cannot be zero` and
  `BackbufferExtent should be constant for the lifetime of the feature. (0, 0) !=
  (256, 128)`, then re-initializes on every call. None of these changed the
  result: creating on the context device (`CreateFeature1`), creating in a prior
  submission, tagging resources before creation, supplying HUD-less color,
  retrying evaluation, or setting every extent parameter the public header
  defines.
- The cause is unresolved. Either the 310.2.1 driver fallback snippet cannot
  serve the 310.9.16 header contract, or creation has a requirement documented
  only in NVIDIA's FG guide. The local SDK copy has neither the SDK-matched
  runtime nor the guide. The next inputs needed (downloading needs user
  approval) are `lib/Windows_x86_64/rel/nvngx_dlssg.dll` and
  `doc/DLSS-FG Programming Guide.pdf` from
  [NVIDIA/DLSS v310.9.16](https://github.com/NVIDIA/DLSS/tree/v310.9.16).

FG therefore produces no verified generated frame. Do not enable it in a
shipping renderer until `tests/framegen_gpu.rs` passes.

### API

- `FrameGenerator::new(device, queue, &SessionOptions, FrameGenerationOptions)`
  initializes the shared NGX runtime. Its `runtime_dir` must contain both
  `nvngx_dlss.dll` and `nvngx_dlssg.dll`, because the shared `Context` checks SR
  availability during initialization. `FrameGenerator::with_context` adds FG to
  an existing SR context. Both are thread-confined (`Rc` context) and follow
  the context lifetime rules above.
- `FrameGenerationOptions` holds:
  - the output (backbuffer) size;
  - the render (depth/motion) size, no larger than the output;
  - `ColorFormat::{Rgba8Unorm, Rgba16Float}`;
  - whether a HUD-less color is always supplied.

  Any change requires a new generator. Dynamic resolution is disabled.
- `evaluate(&GenerationInputs, &GenerationFrame, &wgpu::Texture)` takes the
  caller's single-sample 2D `COPY_SRC` textures: backbuffer and HUD-less in the
  color format at output size, `R32Float` depth and `Rg16Float` motion at render
  size. It copies them into private textures and records NGX into a raw command
  buffer. It then copies the generated frame into the caller's `COPY_DST`
  output, submits and waits.
  - The first call creates the NGX feature in its own submission.
  - As with SR, native handles refer only to private resources. Destroyed or
    mismatched caller textures are rejected with `InvalidInput` before native
    work.
  - A native or GPU failure poisons the generator.
- `Generated::Presentable` is returned only when a previous real frame exists
  and NGX's `OutputDisableInterpolation` buffer was not set. `NoHistory` (reset
  or first frame) and `Disabled` both mean "present only the real frame".
- `FrameGenerationStatus` reports the adapter, raw driver version, NGX minimum
  FG driver, multi-frame maximum (refreshed after feature creation), sizes,
  format and successful evaluation count.

### Per-frame data contract

`GenerationFrame` carries:

- unjittered camera matrices (`view_to_clip`, `clip_to_view`,
  `clip_to_lens_clip`, `clip_to_prev_clip`, `prev_clip_to_clip`), copied
  verbatim as NVIDIA `float[4][4]` rows;
- camera position and basis, near/far, vertical FOV and aspect;
- projection jitter (NGX `JitterOffset`);
- the motion scale that converts stored current-to-previous motion to
  render-extent normalized units (`1/render_size` for pixel motion);
- `reset`, inverted-depth, camera-motion-included, HDR and orthographic flags;
- the multi-frame `Interpolation { index, count }`.

Validation rejects non-finite values, zero motion scale, `near <= 0`,
`far <= near`, non-positive aspect, a perspective FOV outside (0, pi), and
out-of-range or out-of-order interpolation indices. The header calls the jitter
"clip space". Its unit and the matrix vector convention must be confirmed
against NVIDIA's FG guide during integration.

### Presentation contract for the renderer

Generating a frame is not presenting it, and this crate owns no swapchain. For
each real frame N:

1. Render the scene to HUD-less color, depth and motion, then composite UI onto
   the backbuffer. Without HUD-less input, the backbuffer must contain no UI,
   and UI must be composited afterwards onto both real and generated frames.
2. Call `evaluate` with frame N's inputs. It generates the frame between N-1
   and N, so real frame N is held until the generated frame is presented.
3. On `Presentable`, present the generated frame, then real frame N. Pace them
   evenly: the generated frame goes about half a real-frame interval after N-1
   (with `count` k, index i goes at i/(k+1)). For multi-frame generation, call
   `evaluate` for indices 1..=k in order with the same inputs. On `NoHistory`
   or `Disabled`, present only N.
4. Set `reset` after a camera cut or teleport, a scene load, history
   invalidation, or when FG resumes after menus or pauses. Stop calling
   `evaluate` while no game frames are rendered. Size, format and HUD-less
   changes require a new generator, whose first evaluation resets
   automatically.

Holding frame N adds roughly one real-frame interval of latency, and this crate
provides no latency-reduction mechanism. The synchronous submit-and-wait and the
extra copies are a correctness baseline, not a performance claim.

### Remaining for renderer integration

To schedule after SR lands:

- a verified FG evaluation (blocked, see above);
- swapchain pacing and present ordering;
- the HUD-less/UI split;
- jittered camera constants for SR versus unjittered ones for FG;
- depth and motion copies into `R32Float`/`Rg16Float`;
- an asynchronous lifetime to replace the per-frame wait;
- frame-rate and VSync interaction;
- a context that does not require the SR runtime for FG-only use;
- UI recomposition, `NotRenderingGameFrames` and dynamic-resolution parameters;
- Linux runtime packaging;
- image-quality validation (disocclusion, transparency, UI).

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
  presentation pacing is the application's job. They do not show that
  Streamline is required.
- [Public DLSS headers](https://github.com/NVIDIA/DLSS/tree/v310.9.16/include) and
  [Streamline headers](https://github.com/NVIDIA-RTX/Streamline/tree/main/include)
  expose SR, RR and FG, but the inspected inventories contain no neural-rendering
  API. NVIDIA's [September 22 DLSS 5 developer article](https://developer.nvidia.com/blog/whats-new-for-game-developers-dlss-5-with-3d-guided-neural-rendering-nvidia-ace-updates-and-new-rtx-kit-capabilities)
  describes the feature and controls without providing that ABI. Revisit only
  when an official neural-rendering SDK and authorized runtime/model are supplied.
