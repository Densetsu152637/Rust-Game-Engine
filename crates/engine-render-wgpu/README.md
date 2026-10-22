# engine-render-wgpu

`engine_render::RenderBackend` on wgpu 30, plus the native GPU context shared with
`engine-desktop`.

```rust
use engine_render::{DeviceConfig, RenderDevice};
use engine_render_wgpu::{WgpuBackend, WgpuOptions};

let backend = WgpuBackend::offscreen(WgpuOptions::default());
let mut device = RenderDevice::new(backend, DeviceConfig::default())?;
```

## Constructors

* `WgpuBackend::offscreen(options)`: offscreen targets only.
* `WgpuBackend::for_window(Arc<window>, (width, height), options)`: the backend owns
  a `Fifo` swapchain (sRGB format preferred, `desired_maximum_frame_latency` =
  frames in flight). Timeout, occluded, outdated, lost and suboptimal surfaces map
  to `FrameOutcome::Skipped(..)` (or reconfigure) instead of errors.
* `WgpuBackend::from_context(GpuContext)`: share an existing device. A renderer and
  a backend can drive the same `GpuContext` (it is `Clone`). Such a backend has no
  surface; `set_external_target(view, info)` makes `RenderTarget::Surface` draw into
  a host-owned view, and `render_to_view` submits one frame into any view.

`initialize` blocks on the adapter and device request; a missing adapter or device
is `RenderError::Unavailable`.

## Error model

* **Validation errors are per call.** Resource creation (shader modules, bind group
  layouts and pipeline layouts, buffers, textures, targets, pipelines, bind groups,
  the uniform arena), readback and the whole per-frame record and submit run inside
  a wgpu `Validation` error scope (`GpuContext::validated`). The scope is a guard,
  popped on every return path including `?`. A rejected resource returns
  `RenderError::Backend` from that call only and is never written to the sticky
  log, so a following valid shader, mesh or frame succeeds. Scopes are
  thread-local and nest LIFO, which is what the inner and outer scopes of a frame
  rely on.
* **Limits are checked first.** `RenderCapabilities::limits` is filled from
  `wgpu::Device::limits()`, and `RenderDevice` rejects a shader with a bind group
  index at or above `max_bind_groups`, a mesh with a stride above
  `max_vertex_buffer_array_stride` and a uniform above
  `max_uniform_buffer_binding_size` with `RenderError::LimitExceeded` before
  anything reaches wgpu.
* **Device loss, out-of-memory and unscoped errors are sticky.** The device-lost
  callback and the uncaptured-error handler fill a sticky log read by
  `GpuContext::check_errors`. Device loss surfaces as `RenderError::DeviceLost`,
  which `RenderDevice` latches. The handler also records validation errors of a
  host's own direct use of the public `GpuContext::device` outside any scope
  (custom native passes), which `engine-desktop` relies on; the backend's own
  calls never reach it with validation errors.

## Selection

`WgpuBackends` (`Primary` (default), `Vulkan`, `Dx12`, `Metal`, `Gl`, `All`) with
`FromStr`/`Display`, `WgpuBackends::from_env()` (the `ENGINE_RENDER_BACKEND` variable:
`primary|vulkan|dx12|metal|gl|all`, with `wgpu`/`auto` as aliases of primary), and `WgpuOptions { backends, power_preference, requirements,
frames_in_flight }`. `Primary` still honours wgpu's `WGPU_BACKEND` override. The
backend reports `wgpu/<vulkan|dx12|metal|gl>` and the adapter name in its
capabilities.

## Conventions

The `engine-render` contract: sRGB RGBA8 textures and targets, y-up clip space with depth
0..1, top-left scissor and viewport, uniforms by `@group/@binding` (packed into one
per-frame arena addressed with dynamic offsets; identical contents share an offset
and distinct contents never overlap within a frame), the draw's texture at
`@group(1) @binding(0)` with a nearest/clamp sampler at `@group(1) @binding(1)` (a
1x1 white texture when absent), `BlendState::ALPHA_BLENDING` for straight alpha, a
`Depth32Float` attachment owned per target, counter-clockwise front faces, linear
clear color, pipelines cached per `(shader, vertex layout, pipeline state, target
format)`.

## Tests

```text
cargo test -p engine-render-wgpu                        # pure translation tests
cargo test -p engine-render-wgpu -- --ignored --nocapture   # real adapter
```

The ignored tests in `tests/gpu_offscreen.rs` cover the contract on a real device;
`tests/gpu_limits.rs` covers the error model (each bad input fails its own call and
the device then still renders). They fail (not skip) without an adapter.
