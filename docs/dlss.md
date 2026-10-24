# DLSS native integration

The optional `engine-dlss` crate performs actual NVIDIA NGX Super Resolution (SR)
on Vulkan. Ordinary workspace builds do not need the NVIDIA SDK, libraries,
models, or GPU. Build availability is separate from device support and from
successful GPU evaluation. Explicit requests return errors, never a simulated
success. Hosts retain their normal rendering path when setup fails.

## Feature scope

| Feature | This slice | Required renderer data |
| --- | --- | --- |
| Super Resolution / DLAA | Native NGX execution, tested on Windows RTX 5080 | Jittered HDR color, clip depth, current-to-previous motion, temporal reset |
| Frame Generation | Separate integration pending | Presented frames, HUD-less color, depth/motion, camera constants, presentation pacing |
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

On 2026-10-24 the hardware test created and evaluated the real SR feature for
three frames on NVIDIA GeForce RTX 5080 / driver 617.14. NGX selected 512x341
input for 768x512 quality output. Readback of the uniform HDR scene retained
FP16 red `0x3a00` (0.75) and green `0x2fff` (approximately 0.125). This checks
actual native GPU execution and readback, not image-quality coverage for motion,
disocclusion, transparent materials or presentation. The test also evaluates a
second feature sharing the context after the first feature is dropped, then
destroys an exposed staging input and confirms a controlled error without
another successful native evaluation. Linux/macOS execution and
full authored-game temporal behavior require their own validation.

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
- [Public DLSS headers](https://github.com/NVIDIA/DLSS/tree/v310.9.16/include) and
  [Streamline headers](https://github.com/NVIDIA-RTX/Streamline/tree/main/include)
  expose SR, RR and FG, but the inspected inventories contain no neural-rendering
  API. NVIDIA's [September 22 DLSS 5 developer article](https://developer.nvidia.com/blog/whats-new-for-game-developers-dlss-5-with-3d-guided-neural-rendering-nvidia-ace-updates-and-new-rtx-kit-capabilities)
  describes the feature and controls without providing that ABI. Revisit only
  when an official neural-rendering SDK and authorized runtime/model are supplied.
