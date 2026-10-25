# Desktop rendering

Rendering is split in two. `engine-project` produces a backend-neutral CPU frame
(`runtime::RenderFrame`) from the simulation, and `engine-desktop` draws that frame
through the thin `engine-render` adapter onto wgpu (which selects Vulkan, DX12, Metal
or GL). Shaders are authored in WGSL and validated with Naga
(`engine_render::validate_wgsl`); wgpu consumes WGSL directly. The crate README
(`crates/engine-desktop/README.md`) is the detailed contract; this page summarizes
the 2D content pipeline. For integrated 3D cameras, materials and custom shader draws, see [Scene rendering](rendering3d.md).

## Crates and graphics API selection

| Crate | Role |
| --- | --- |
| `engine-render` | `RenderBackend` trait, `RenderDevice` (handles, deferred destruction), command buffer, WGSL reflection, deterministic recording backend. |
| `engine-render-wgpu` | `WgpuBackend`, `GpuContext`/`GpuRequirements`, `OffscreenTarget`, `WgpuBackends`. |
| `engine-desktop` | `SpriteRenderer<B>`, wgpu-native `GpuRenderer`, `WindowSurface`, runner. |

The adapter exists to keep the renderer independent of wgpu types and to make
headless tests deterministic (the recording backend), not to host several GPU
implementations. Choose the wgpu graphics API with
`ENGINE_RENDER_BACKEND=primary|vulkan|dx12|metal|gl|all` (default primary; `wgpu`
and `auto` are aliases of primary; an invalid value fails startup, even for
headless runs) or `engine_desktop::run_with_wgpu_backends`. Conventions are the
same on every API: sRGB RGBA8 targets and textures, y-up clip space, top-left
scissor and viewport, straight alpha, depth `LessEqual`/`Always` with a write
flag, a backend-owned depth attachment, and nearest/clamp sampling. The
`backend_parity` tests render the sprite scenes on each API available on the
machine and assert exact, byte-identical pixels. Overlay render features and
`GpuRequirements` work on every API.

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

## Render contract (`RenderFrame` to the adapter)

Draws hold immutable `Arc<Mesh>` and optional `Arc<Texture>` assets, vertex colours,
a layer, a stable ID, world or UI space, an optional logical clip, and blend and
depth state. Mesh positions are already transformed. Texture data is sRGB; vertex
colours are linear. Sampling is nearest with clamped edges. Two fields serve
presentation interpolation: `motion` (the entity position the mesh was baked
from; extraction fills it, renderers ignore it) and `offset` (a world-space
translation the renderer adds to that draw's camera matrix; extraction leaves it
zero, and non-zero offsets are rejected on UI draws). See
[Frame pacing and interpolation](#frame-pacing-and-interpolation).

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

## Frame pacing and interpolation

The visible runner (`engine_desktop::run`) separates simulation from
presentation. Headless and deterministic runs (`headless: true`, `--headless`, the
MCP `project_run_headless` and `render_screenshot` tools) are unchanged: exactly
`RunOptions::ticks` fixed 1/60-second ticks with the injected input of each tick,
the same `RunReport`, and screenshots render the current tick with no blending.

**Fixed-step simulation.** `Runtime::tick` always receives `FIXED_DELTA` (1/60 s).
Each redraw `FramePacer::advance` banks the real time since the previous redraw
and returns how many ticks to run and an `alpha` in `[0, 1]`:

| Constant | Value | Meaning |
| --- | --- | --- |
| `FIXED_DELTA` | 1/60 s | One simulation tick. |
| `MAX_FRAME_TIME` | 0.25 s | A redraw adds at most this much time (window drag, debugger pause). |
| `MAX_TICKS_PER_FRAME` | 5 | At most this many ticks per redraw. |

When the cap is hit the excess whole ticks are dropped, keeping only the sub-tick
remainder, so a slow machine runs slower than real time instead of falling
further behind (no death spiral), and a stall never fast-forwards the game. A
display slower than 60 Hz runs several ticks per redraw; a faster one runs a tick
on some redraws and none on the rest. Non-finite or negative frame times count as
zero.

**Presentation at the display rate.** Every redraw presents, including those that
ran no tick, so frames follow the display (vsync, `Fifo`, by default; each
redraw blocks acquiring the swapchain image, so the loop neither spins nor needs
a timer). After a tick the runner extracts a `RenderFrame` and keeps the last two
in `FrameHistory` (only the final two ticks of a multi-tick redraw are
extracted). The drawn frame is `interpolate_frame(previous, current, alpha)`: the
simulation clock is `alpha` of the way from the previous tick to the current one,
so the picture lags the simulation by up to one tick and moves smoothly at any
refresh rate. The final frame of a bounded run is drawn at `alpha = 1`.
`ENGINE_PRESENT_MODE=fifo|mailbox|immediate|auto` selects the present mode
(unsupported modes fall back to `fifo`; the non-vsync modes exist to measure
frame rates above the display refresh).

**What blends.** Extraction records, per sprite and mesh draw, the entity's
unsnapped position (`Draw::motion`). Interpolation matches draws by stable ID and
blends that position, applying the result as `Draw::offset`, a world-space
translation added to the draw's camera matrix. The mesh is the current tick's
mesh, never rebuilt, so the GPU copy is reused and blending costs no uploads.
Consequently:

- Sprite animation frames, UVs and tint are the current tick's. UI draws have no
  motion and are not interpolated. Render features receive the current
  tick's render world unblended.
- Pixel snapping applies after the blend: a snapping sprite is drawn at the pixel
  grid position of the blended position (`snap_to_pixels`, the function
  extraction uses), so it steps from pixel to pixel. The camera centre blends
  too, and `CameraView` snaps the blended centre.
- A draw snaps to the current tick when it cannot be a smooth move: no previous
  draw with that ID (spawn), a scene transition (`Runtime::scene_revision`
  changed between the two ticks), a camera projection change, a non-finite
  position, or a jump of more than one screen (camera logical size over pixels
  per unit) in one tick, which is treated as a teleport.
- `alpha = 1` returns the current frame itself, so output is bit-identical to an
  uninterpolated frame.
- Interpolation reads two finished frames and produces a third. It never touches
  the runtime, so simulation, collision, scripts and saves never see a blended
  value.

**Input across uneven ticks.** Input is sampled by level once per tick and edges
come from comparing consecutive ticks, so a key held across redraws with no tick,
or across several ticks in one redraw, presses once (on the first tick) and
releases once. A tap that begins and ends between two ticks would be invisible to
level sampling; `PressLatch` remembers key and mouse-button presses until the
next tick sees them, producing one press and one release. The mouse wheel delta
goes to the first tick that samples it. Gamepad state is polled per tick (a
gamepad tap shorter than a tick interval can still be missed).

**While nothing can be shown** (minimized, occluded, or the swapchain yields no
image), redraws are not requested back to back. The loop waits for the next tick
deadline and keeps simulating at real-time speed, as before, without presenting;
the first successful present resumes display-rate redraws.

## Native extensions

Games compile into the host executable and use `engine_desktop::wgpu`, so there is
no cross-compiler Rust ABI. Implement `render_pipeline::PipelineFeature` (an
`Overlay`-stage feature draws after the engine's output and sprites; other stages
reach the HDR scene, see [render architecture](rendering-architecture.md)) and
register it with `PipelineFeatures::register(feature)`, then insert the resource
into `EngineApp::world_mut()` during configuration. Features receive the device,
queue, command encoder, view data, frame textures, output target and the
extracted world; they may create arbitrary wgpu render, compute and transfer work.
Add a `DeviceFeatureRequest` to the `GpuRequirements` resource before launch when
a custom pipeline needs more than baseline wgpu. `examples/native-extension` is a complete example,
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
cargo test -p engine-desktop --test backend_parity -- --ignored --nocapture
cargo test -p engine-render-wgpu -- --ignored --nocapture
```

Frame pacing (`FramePacer`: exact multiples, fractional accumulation, the tick
cap and debt dropping, huge stalls, zero-length frames), interpolation (blend,
snap on a missing previous draw, scene change, teleport, pixel snap after the
blend, `alpha = 1` equivalence), the input latch (edges across zero, one and many
ticks per redraw) and the offset-through-matrix submission are pure and covered
by headless unit and `interpolated_presentation` tests. `gpu_pixels` has an
ignored test proving an interpolated sprite draws the same pixels as one baked
at the blended position.

`backend_parity` has deterministic tests that assert the recorded command stream
without a GPU, and ignored tests that render the same scenes on each wgpu API
(primary, Vulkan, DX12, GL), skipping an API with no adapter and comparing the
readbacks of those that ran.

They cover atlas orientation, sprite and UTF-8 font geometry, clipping, stable
ordering, dirty uploads, cache eviction, resizing, bounded input, an external custom
WGSL pipeline and propagation of a deliberately invalid GPU resource, and fail when
no working adapter exists. An offscreen pixel pass proves drawing but not visible
window or interactive input behavior; report those separately. See
[Testing and CI](testing.md).
