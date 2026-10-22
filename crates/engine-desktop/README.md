# Desktop host and native rendering

`engine-desktop` hosts an `engine_project::Runtime` on the process main thread
using winit 0.30, drawing through wgpu 30 or a native Vulkan backend (see
[Render backends](#render-backends)). It does not enable winit's platform `any_thread`
escape hatch. `run(runtime, RunOptions)` returns errors from window creation,
surface acquisition, device validation/loss, custom passes, gameplay, input
initialization and audio. The host runs runtime cleanup before dropping devices.

`headless: true` runs fixed 1/60-second ticks without sleeping or creating a
window, GPU, gamepad or audio device. Its offline audio mixer advances 800 stereo
frames per tick. Injected action sets are indexed by zero-based tick; an action
stays held across consecutive entries and releases when absent. A cooperative
`shutdown` atomic flag exits through normal cleanup at a tick boundary.

Native execution creates a visible window, polls physical keyboard/mouse and
gilrs controller input, opens CPAL audio through `engine-audio`, and presents
real frames. Focus loss releases physical keyboard/mouse state. Runtime binding
changes and controller deadzone changes are applied before the next input sample.
Pointer coordinates use the current letterboxed logical viewport. Resize creates
a matching depth target; zero extent suspends presentation. Surface timeout skips
a frame, lost/outdated surfaces reconfigure, and other device failures terminate
with an error. Native runs respect the same tick bound as headless runs.

## Render backends

Graphics are split across three crates plus this host:

| Crate | Role |
| --- | --- |
| `engine-render` | Backend-neutral adapter: `RenderBackend` trait, `RenderDevice` (handles, validation, deferred destruction), `RenderCommandBuffer`, WGSL reflection and `validate_wgsl`, and a recording `headless` backend. |
| `engine-render-wgpu` | `WgpuBackend` on wgpu 30, `WgpuBackends`/`WgpuOptions` selection, and the native `GpuContext`, `GpuRequirements`, `OffscreenTarget`, `RenderTargetInfo` (re-exported by this crate). |
| `engine-vulkan` | `VulkanBackend` on ash (optional cargo feature `vulkan`, on by default). |
| `engine-desktop` | `SpriteRenderer<B: RenderBackend>` (neutral), `GpuRenderer` (wgpu-native, runs `NativeRenderPass`es), `WindowSurface` (wgpu window host), `BackendWindow` (window host for any backend), `OffscreenSprites`, `create_backend`, and the runner. |

The runner picks the backend per run. `ENGINE_RENDER_BACKEND` accepts `wgpu`
(default) or `vulkan` (case-insensitive; `vk` is an alias). Any other value fails
`run()` with an error naming the accepted values, including for headless runs, so
a typo surfaces even though a headless run never creates a device. Pass an
explicit choice with `run_with_render_backend(runtime, options, Some(kind))`,
which ignores the environment; `run(runtime, options)` is the same call with
`None`. `resolve_render_backend(option, env_value)` is the pure resolution rule.
`BackendKind::Headless` is not presentable and is rejected for native runs.

* **wgpu** (default) keeps the original native path: `WindowSurface` and
  `GpuRenderer`, so `GpuRequirements` and `NativeRenderPasses` keep working. The
  `GpuRenderer` is a `SpriteRenderer<WgpuBackend>` sharing the caller's
  `GpuContext`; native passes are encoded into their own command buffer submitted
  after the engine draws on the same queue.
* **vulkan** uses `BackendWindow` over `VulkanBackend`. Native render passes and
  non-default `GpuRequirements` are wgpu-specific by design (they hand out raw wgpu
  objects), so startup fails with "native render passes and GpuRequirements require
  the wgpu backend" instead of ignoring them. Without the `vulkan` cargo feature,
  selecting Vulkan fails with an error that names the feature.

Both backends consume the same neutral frame and produce byte-identical pixels
(`tests/backend_parity.rs`). Use `OffscreenSprites::new(kind, width, height,
limits)` for tests and tools: `render(&RenderFrame)` then `read_rgba()`.
`create_backend(kind, window, &limits)` builds a `Box<dyn RenderBackend>` for a
custom host. Within the wgpu backend, wgpu's own `WGPU_BACKEND` variable still
overrides the default graphics API choice.

## Render contract

The CPU frontend is `engine_project::runtime::RenderFrame`. Draws contain immutable
`Arc<Mesh>` and optional `Arc<Texture>` assets, vertex colours, layer, stable ID,
world/UI coordinates, logical clip, blend and depth state. Mesh positions are
already transformed by extraction. Texture data is top-left RGBA8 in sRGB;
vertex colours are linear. Atlas sampling uses nearest filtering and clamp edges.
World Y points up; UI Y points down. Camera fitting uses integer magnification
above 1x and uniform fractional fit below 1x. Camera centers snap in source pixels.

Draws sort by `(layer, id)`, not resource address or ECS iteration order. Duplicate
IDs fail validation. Consecutive compatible draws share pipeline/texture/camera
binding batches without changing painter order. Meshes remain separate GPU
buffers, so this reduces binding changes rather than claiming one draw call per
batch. Non-indexed and indexed triangle lists both draw through the same shader.
Opaque/straight-alpha blending and independent depth testing/writing select
cached pipelines. Clip rectangles become bounded framebuffer scissors.

Retain the same asset Arc between frames to avoid uploads; replace it after an
edit. Cache entries retain strong source references, preventing pointer reuse.
Shared texture Arcs upload once across all draws. Unreferenced resources are
evicted after a frame; wgpu retains submitted resource references until GPU work
finishes. Validation bounds draw count, total unique mesh/texture bytes, device
texture dimensions, indices and finite vertex data before uploading a frame.
At most two frame submissions are in flight by default; configurable limits
permit 1–8 and apply backpressure to the oldest submission. GPU waits have a
30-second timeout. Validation/device-loss errors remain sticky.

## Native extensions and editor reuse

Games compile into the same executable and use `engine_desktop::wgpu`, avoiding
independently compiled Rust object ABIs. Native passes require the wgpu backend
(see [Render backends](#render-backends)). Implement `NativeRenderPass` and register
it with `NativeRenderPasses::register(name, pass)`, then insert that resource into
`EngineApp::world_mut()` during game configuration/startup. The runner takes it
when creating the GPU. Names are unique; passes run after built-in draws in
insertion order. `prepare` receives the device and target format/extent on
registration and resize. `encode` receives the device, queue, command encoder,
target view and extracted ECS world. It may create arbitrary native wgpu render,
compute and transfer work. Use `LoadOp::Load` when compositing over engine output.
Pass failures propagate to the runner. Passes own their resources through RAII.

Insert `GpuRequirements { features, limits }` before launching when custom
pipelines require features beyond baseline wgpu. Unsupported requirements fail
device creation. No shader identifier or fixed builtin job table restricts this
extension point.

`WindowSurface` is the reusable window/surface owner for the native editor or
another host. It exposes `window`, `renderer`, `resize` and `present`.
`GpuRenderer` can also target external views directly. `OffscreenTarget` uses the
same renderer and supports real RGBA readback, including row-alignment removal.
Custom hosts must call runtime cleanup and `gpu.wait_idle()` before teardown.

`validate_wgsl(source)` performs bounded CPU-only Naga parsing and semantic
validation with source diagnostics. It requires a shader entry point. Native
capabilities are admitted at this stage; actual device feature and limit checks
remain part of pipeline creation.

## Verification

```text
cargo test -p engine-desktop
cargo test -p engine-desktop --test gpu_pixels -- --ignored --nocapture
cargo test -p engine-desktop --test backend_parity -- --ignored --nocapture
cargo test -p engine-render-wgpu -- --ignored --nocapture
cargo clippy -p engine-desktop --all-targets -- -D warnings
cargo run -p engine-desktop --example desktop_smoke -- --headless
cargo run -p engine-desktop --example desktop_smoke
ENGINE_RENDER_BACKEND=vulkan cargo run -p engine-desktop --example desktop_smoke
```

The GPU tests are explicitly ignored in ordinary headless test runs. The explicit
GPU command fails if a working adapter is absent; it never substitutes a no-op
backend or treats unavailable GPU evidence as success. Pixel tests cover textured
atlas orientation, sprite and UTF-8 font geometry, clipping, stable ordering,
dirty uploads, shared cache eviction, resizing, bounded input, an external custom
WGSL pipeline, and propagation of a deliberately invalid GPU resource.

An offscreen pixel pass proves drawing but does not prove visible window/input
interaction. Native window, interactive input and audible playback evidence must
be reported separately from ordinary unit and offscreen tests.
