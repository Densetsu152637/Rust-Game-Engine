# Desktop host and native rendering

`engine-desktop` hosts an `engine_project::Runtime` on the process main thread
using winit 0.30 and wgpu 30 (see [Render backends](#render-backends)). It does not enable winit's platform `any_thread`
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

## Frame pacing

Native runs step the simulation and present the window independently. The
runtime still receives fixed 1/60-second ticks (`FIXED_DELTA`); a `FramePacer`
accumulator runs the ticks the elapsed real time buys (none, one or several per
redraw, at most `MAX_TICKS_PER_FRAME` = 5, with a single redraw adding at most
`MAX_FRAME_TIME` = 0.25 s and excess backlog dropped so there is no catch-up
spiral). Every redraw presents, at the display rate, the blend of the last two
ticks' `RenderFrame`s (`FrameHistory`, `interpolate_frame`): sprite and mesh
positions and the camera centre are blended and pixel-snapped after the blend,
while animation frames, UI and native render passes use the current tick.
Spawns, scene changes, projection changes and teleports snap rather than smear.
The blend never reaches the simulation, and headless runs, screenshots and
`RunReport` are unchanged. A key tap shorter than the gap between two ticks still
produces one press and one release (`PressLatch`). `ENGINE_PRESENT_MODE` selects
the present mode (default vsync). Details: [Desktop rendering](../../docs/desktop-2d-rendering.md#frame-pacing-and-interpolation).

## Render backends

Graphics are split across two crates plus this host:

| Crate | Role |
| --- | --- |
| `engine-render` | Thin backend-neutral adapter: the `RenderBackend` trait, `RenderDevice` (typed handles, validation, deferred destruction across frames in flight), `RenderCommandBuffer` ordering, WGSL reflection and `validate_wgsl`, and a deterministic recording `headless` backend for GPU-free tests. It exists to decouple the renderer from wgpu types and to make headless tests deterministic. |
| `engine-render-wgpu` | The only GPU backend: `WgpuBackend` on wgpu 30, `WgpuBackends`/`WgpuOptions` graphics API selection, and the native `GpuContext`, `GpuRequirements`, `OffscreenTarget`, `RenderTargetInfo` (re-exported by this crate). |
| `engine-desktop` | `SpriteRenderer<B: RenderBackend>` (converts a `RenderFrame` to adapter commands), `GpuRenderer` (a `SpriteRenderer<WgpuBackend>` sharing the caller's `GpuContext`, plus `NativeRenderPass`es), `WindowSurface`, `OffscreenSprites`, and the runner. |

wgpu itself chooses between Vulkan, DX12, Metal and GL. The runner reads
`ENGINE_RENDER_BACKEND` (`primary|vulkan|dx12|metal|gl|all`, case-insensitive;
`wgpu` and `auto` mean primary; unset or blank means primary). An unknown value
fails `run()` with an error listing the accepted values, including for headless
runs, so a typo surfaces even though a headless run never creates a device. Pass
an explicit choice with `run_with_wgpu_backends(runtime, options, Some(backends))`,
which ignores the environment; `run(runtime, options)` is the same call with
`None`. `resolve_wgpu_backends(option, env_value)` is the pure resolution rule and
`WindowSurface::with_options` takes a `WgpuOptions` (API, power preference,
`GpuRequirements`). Native render passes and `GpuRequirements` always apply, on
every graphics API.

Use `OffscreenSprites` for tests and tools: `OffscreenSprites::wgpu(backends,
width, height, limits)` renders on a real device, `OffscreenSprites::headless(..)`
on the recording backend, and `OffscreenSprites::new(Box<dyn RenderBackend>, ..)`
on any backend you hold a log for. Then `render(&RenderFrame)` and `read_rgba()`.
The same scenes produce byte-identical pixels on every wgpu API of the test
machine (`tests/backend_parity.rs`).

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
independently compiled Rust object ABIs. Implement `NativeRenderPass` and register
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

## DLSS Super Resolution

The optional `dlss` feature (forwarding to `engine-dlss/ngx-vulkan`) lets a
`Camera3D.upscaler` request render the 3D scene through NVIDIA DLSS Super
Resolution before 2D/UI. `request_upscaler_gpu` prepares the Vulkan device NGX
needs, `GpuRenderer::with_upscaler_device` uses it, and `upscaler_status()`
reports what happened. Without the feature, SDK, Vulkan or a supported NVIDIA
GPU the frame renders natively and the status states the reason. Contract,
configuration and limits: [Scene rendering](../../docs/rendering3d.md#temporal-upscaling-dlss-super-resolution).

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
cargo test -p engine-desktop --test upscaler -- --ignored --nocapture
cargo clippy -p engine-desktop --all-targets -- -D warnings
cargo run -p engine-desktop --example desktop_smoke -- --headless
cargo run -p engine-desktop --example desktop_smoke
ENGINE_RENDER_BACKEND=dx12 cargo run -p engine-desktop --example desktop_smoke
ENGINE_PRESENT_MODE=immediate cargo run -p engine-desktop --release --example desktop_smoke -- --ticks 300
```

The smoke prints the fixed-tick rate (about 60 per second) and the presented
frame rate (the display refresh with vsync; far higher with `immediate`).

The GPU tests are explicitly ignored in ordinary headless test runs. The explicit
GPU command fails if a working adapter is absent; it never substitutes a no-op
backend or treats unavailable GPU evidence as success. Pixel tests cover textured
atlas orientation, sprite and UTF-8 font geometry, clipping, stable ordering,
dirty uploads, shared cache eviction, resizing, bounded input, an external custom
WGSL pipeline, and propagation of a deliberately invalid GPU resource.

An offscreen pixel pass proves drawing but does not prove visible window/input
interaction. Native window, interactive input and audible playback evidence must
be reported separately from ordinary unit and offscreen tests.
