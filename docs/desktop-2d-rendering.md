# Desktop rendering

Rendering is split in two. `engine-project` produces a backend-neutral CPU frame
(`runtime::RenderFrame`) from the simulation, and `engine-desktop` draws that frame
with wgpu (Vulkan, DX12 or Metal backends as enabled in its `Cargo.toml`). This
replaces the earlier Vulkan-only renderer; no SPIR-V or external shader compiler is
involved, because shaders are WGSL validated with Naga. The crate README
(`crates/engine-desktop/README.md`) is the detailed contract; this page summarizes
the 2D content pipeline.

## CPU content (`engine_project::content`)

- Textures load as top-left, row-major RGBA8 with straight (non-premultiplied)
  alpha from PNG or ASCII P3 PPM. PNG decoding is bounded to 8192 per axis.
- `sprite_quad` builds a quad from a world position, size, a normalized pivot
  measured from the left/bottom, and a top-left source rectangle in texture pixels.
  Atlas bounds and all floating values are validated; the tint becomes vertex
  colour.
- `animation_frame(elapsed, fps, count, looping)` derives a frame from simulation
  time: looping wraps and one-shot animation clamps on its last frame.
- `camera_viewport` and `logical_pointer` (and `engine_desktop::CameraView`) map a
  camera's authored logical size to the framebuffer. Above 1x the fit is the
  largest whole-number scale and the viewport is centred with letterbox bars;
  below 1x a uniform fractional fit is used and pixel-perfect output is not claimed.
  A zero-sized framebuffer yields zero scale and the frame is skipped. Camera pixel
  snapping rounds the camera centre to `1/pixelsPerUnit`; simulation positions are
  unchanged. World +Y points up, UI +Y points down.
- Fonts are bitmap-atlas JSON (`schema`, `texture`, `lineHeight`, `fallback`,
  `glyphs[{codepoint, source, advance, bearing}]`). `bearing` is an offset from the
  top of the line in logical pixels and `advance` is the cursor distance.

  ```json
  {"schema":1,"texture":"asset:font-atlas","lineHeight":12,"fallback":63,
   "glyphs":[{"codepoint":63,"source":[0,0,6,10],"advance":7,"bearing":[0,1]}]}
  ```

  Font loading validates metrics, duplicate code points, the explicit fallback
  glyph and atlas bounds; the JSON is capped at 4 MiB and 65,536 glyphs, and the
  project requires the `texture` to be a declared texture asset. `text_mesh` lays
  out Unicode text with word wrapping (oversized words break by glyph), newlines
  and the fallback glyph. This is atlas coverage, not complex-script shaping.

## Render contract (`RenderFrame` to wgpu)

Draws hold immutable `Arc<Mesh>` and optional `Arc<Texture>` assets, vertex colours,
a layer, a stable ID, world or UI space, an optional logical clip, and blend and
depth state. Mesh positions are already transformed. Texture data is sRGB; vertex
colours are linear. Sampling is nearest with clamped edges.

- Draws sort by `(layer, id)`, never by resource address or ECS iteration order;
  duplicate IDs fail validation.
- Consecutive compatible draws share pipeline, texture and camera bindings without
  changing painter order. Meshes remain separate GPU buffers, so batching reduces
  binding changes rather than collapsing draw calls.
- Opaque and straight-alpha blending and independent depth test and write select
  cached pipelines. Clip rectangles become bounded framebuffer scissors.
- Reusing the same asset `Arc` between frames avoids uploads; replace it after an
  edit. Shared textures upload once and unreferenced resources are evicted after a
  frame. Validation bounds draw count, total unique mesh and texture bytes, device
  texture dimensions, indices and finite vertex data before upload.
- At most two frame submissions are in flight by default (configurable 1-8, with
  backpressure on the oldest submission) and GPU waits time out after 30 seconds.
  Validation and device-loss errors are sticky.
- Resize creates a matching depth target and a zero extent suspends presentation.
  A surface timeout skips the frame, lost or outdated surfaces reconfigure, and
  other device failures end the run with an error.

## Native extensions

Games compile into the host executable and use `engine_desktop::wgpu`, so there is
no cross-compiler Rust ABI. Implement `NativeRenderPass` and register it with
`NativeRenderPasses::register(name, pass)`, then insert the resource into
`EngineApp::world_mut()` during configuration. Passes run after built-in draws in
insertion order and receive the device, queue, command encoder, target view and the
extracted world; they may create arbitrary wgpu render, compute and transfer work.
Insert `GpuRequirements { features, limits }` before launch when a custom pipeline
needs more than baseline wgpu. `examples/native-extension` is a complete example,
including a custom WGSL pipeline.

`WindowSurface` is the reusable window and surface owner (used by hosts such as the
editor), `GpuRenderer` can draw into external views, and `OffscreenTarget` renders
through the same renderer and reads back RGBA pixels. `validate_wgsl(source)` does
bounded CPU-only Naga parsing and semantic validation with source diagnostics and no
GPU.

## Verification

Headless tests cover the CPU content, camera maths, shader validation and runner.
Pixel tests need a real adapter and are `#[ignore]`d:

```text
cargo test -p engine-desktop --test gpu_pixels -- --ignored --nocapture
```

They cover atlas orientation, sprite and UTF-8 font geometry, clipping, stable
ordering, dirty uploads, cache eviction, resizing, bounded input, an external custom
WGSL pipeline and propagation of a deliberately invalid GPU resource, and fail when
no working adapter exists. An offscreen pixel pass proves drawing but not visible
window or interactive input behavior; report those separately. See
[Testing and CI](testing.md).
