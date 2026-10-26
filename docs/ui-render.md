# UI render features (`engine-ui-render`)

GPU features of the game UI, written against the render extension API
([rendering-architecture.md](rendering-architecture.md)). The ordered overlay,
final-composite and late Frame Generation changes are a source checkpoint:
build, Clippy and new CPU/GPU regression execution are pending. Earlier GPU
measurements below describe the previous implementation.

| Feature | Stage | Draws | Needs |
| --- | --- | --- | --- |
| `ui.sdf_text` | Overlay (merged with `engine.sprites` in paint order) | screen-UI text of SDF font families from engine-text SDF atlas pages with `GLYPH_SHADER_WGSL` | any wgpu frame (2D or 3D) |
| `ui.world` | Transparent (after `engine.transparent`) | `engine.UiWorldPanel` and `engine.WorldText` in `Camera3D` scenes: depth-tested, back-to-front, billboarded per view, SDF text | a `Camera3D` |
| `ui.color_filter` | Composite (after all overlays) | the colour-vision filter of `engine.AccessibilitySettings` | 2D and 3D presentation, including late generated/real outputs |

The crate depends on `engine-render-pipeline`, `engine-project` and `engine-text`;
`engine-desktop` is only a dev-dependency (GPU tests). The desktop host depends
on this crate to install default UI features; GPU feature code does not depend
on the desktop host.

## Defaults and explicit options

```rust
// Override the defaults before the runner starts:
engine_ui_render::install_with(runtime.app_mut(), UiRenderOptions {
    sdf_text: true, world_ui: true, color_filter: true,
})?;
```

The desktop window runner and screenshot host call
`engine_desktop::install_default_ui_features` before extraction, prepare the
features on their renderer, then re-extract the presented frame after readiness.
Explicit `install_with` options, including all false, override defaults. A game
that registered its own UI subset keeps it. Other existing feature names are
preserved. A standalone `GpuRenderer` can use `install` / `register` explicitly.

`install` uses the runtime's `UiGpuQueue` and `PipelineFeatures` resource;
`register` accepts those handles directly. Screen SDF and final filtering declare
replayable presentation support and can accompany Frame Generation. An enabled
custom Overlay/Composite feature without that capability still gives a precise
FG fallback reason; disabling that feature for the camera removes the blocker.

## Routing rule (no double drawing)

`engine-project` extracts UI draws on the CPU (fonts, layout, localisation, atlas
pages live there). Draw extraction decides, once per frame, what leaves the 2D
`Draw` list ([`UiGpuQueue::route`]):

| Content | Goes to the feature when | Otherwise |
| --- | --- | --- |
| Screen UI text of a family with `atlas: "sdf"` | `ui.sdf_text` is **ready** (registered and prepared on a renderer's device) and not disabled for the active camera | coverage atlas, `DrawSpace::Ui` draws |
| `UiWorldPanel`, `WorldText` | `ui.world` is ready, **a `Camera3D` is active** and the feature is not disabled | `DrawSpace::World` meshes for the 2D camera (today's behaviour) |

Routed content is *not* emitted as 2D draws; the feature draws it from the frame
published into the queue. A feature is disabled for a camera with
`Camera3D.render_features` (`{"ui.world": false}`); extraction reads the same map,
so disabling a feature sends its content back to the 2D path instead of hiding it.
A feature that is registered but not prepared (a 2D-only or non-wgpu backend such
as `OffscreenSprites::headless`, or before the first frame has created the
renderer) routes nothing. Hosts that extract before preparing features keep coverage for that snapshot;
they must re-extract after readiness to route SDF. Default window and screenshot
hosts perform that handoff before the final presented image.

Details of the routing:

- SDF output is requested from the text backend only while the screen roots are
  extracted; world panels keep coverage text at their own raster density (a panel
  is a texture-like surface rasterised 1:1 at its `pixelsPerUnit`).
- Dialogs, dropdowns and tooltips retain SDF when ready. Their authored layers
  place them above ordinary roots in the same painter stream.
- SDF runs are recognised by their draw ID (`game-ui:...:sNN`, `is_sdf_draw`); the
  runtime splits them off the draw list in `extract_frame`.

## `ui.sdf_text`

Overlay runs after tonemapping in the target format. The SDF feature supplies
ordered items alongside the host's sprite commands, using the existing UI
producer's root/preorder/paint IDs. Its `:sNN` distance-field marker is normalized
to the original text slot `:2NN` for ordering only; cache/draw identity stays
unchanged. Background/image geometry stays below text, and caret/foreground,
dialog, popup and tooltip geometry stays above it. Items merge by world/UI,
layer and paint ID; only contiguous runs from the same producer are batched.

A producer retains its frame while all its batches encode. The glyph shader from
`engine-text` uses mode 1, signed distance in alpha and `fwidth` smoothing. The
2D camera fit (`layout::UiFit`) and physical scissor still match sprites. Atlas
pages upload once per `Arc` snapshot. Telemetry reports once per encoded output,
including an empty output, rather than once per interleaved batch.

SDF atlas: `AtlasConfig::sdf()` (48 px base, 6 px spread), created on first use
next to the coverage atlas. One bitmap serves every size; quads are the base
bitmap scaled by `size / 48`. Only the families (or `WorldText.atlas`) that
request `sdf` use it.

## `ui.world`

Per frame the runtime publishes one item per panel or text: item-local meshes
(x right, y up, world units, z 0), the entity's world matrix and a billboard mode.
For each view the feature:

1. **Sorts** items back to front by the depth of their bounds centre along the view
   direction (`layout::order_items`), then layer, then ID; draws inside an item keep
   their layer/ID order.
2. **Places** each item in the vertex shader: `world_from_local`, turned toward the
   camera of *that view* from `engine_view.world_from_view` / `camera_position`
   (`placed()` in `world.wgsl`, mirrored by `layout::billboard_matrix`):
   `full` faces the camera position, `yaw` rotates about world Y only (both match
   `UiWorldPanel::placement`), `screen` (used by `WorldText.billboard`) aligns
   with the camera's right and up axes. Position and per-axis scale are kept.
3. **Rasterises** with `engine_view.clip_from_world` (the jittered view-projection,
   so it composes with temporal upscalers) and **depth-tests** against the scene
   depth (`LessEqual`, no depth write) in the Transparent stage, so opaque
   geometry in front hides it.
4. **Blends** straight alpha into linear HDR colour. Vertex colours are sRGB and are
   converted to linear; image textures use their sRGB view; SDF pages use the
   distance in alpha. A white UI colour is diffuse white 1.0 (not lit).

Views of kind `Probe` are skipped; camera, editor-viewport and XR-eye views draw,
each with its own camera for billboards.

Limits: no mip-mapping of panel textures; per-item (not per-fragment) sorting, so
intersecting blended items can show ordering artifacts; items are drawn after the
built-in transparent meshes (they are not interleaved by depth with them); no depth
bias (place panels slightly off a wall to avoid z-fighting); world UI is not
interpolated between ticks; pointer picking is unchanged (`UiWorldPanel::pick`).

## Colour-vision filter (`ui.color_filter`)

Settings: `engine.AccessibilitySettings` fields `colorFilter` (`none`,
`protanopia`, `deuteranopia`, `tritanopia`, `achromatopsia`), `colorFilterMode`
(`simulate`, `correct`) and `colorFilterStrength` (0 to 1); native API
`AccessibilityControl::set_color_filter(kind, mode, strength)`. Validation rejects
unknown values, a strength outside 0..=1 and `correct` with `achromatopsia` (there
is nothing to correct). See [accessibility.md](accessibility.md#colour-vision-filter).

Maths ([`color`]): one 3x3 matrix on linear sRGB (Rec. 709) RGB. The
Composite stage reads the fully composed display image after tonemapping,
sprites and SDF, and writes a distinct destination. Both textures match output
size and format; an sRGB source view decodes on sampling and an sRGB destination
encodes on write. Raw non-sRGB targets retain their producer's numerical encoding
contract; encoded 8-bit content should use an sRGB view for linear filtering.

| Mode | Matrix |
| --- | --- |
| simulate protanopia, deuteranopia, tritanopia | Machado, Oliveira and Fernandes, "A Physiologically-based Model for Simulation of Color Vision Deficiency", IEEE TVCG 15(6), 2009, severity 1.0 ([tables](https://www.inf.ufrgs.br/~oliveira/pubs_files/CVD_Simulation/CVD_Simulation.html)) |
| simulate achromatopsia | Rec. 709 luminance in all channels |
| correct (daltonize) | `I + S (I - Sim)`: the simulation error moved into the channels the deficiency keeps. `S` is the protan/deutan shift `[[0,0,0],[.7,1,0],[.7,0,1]]` after Fidaner, Lin and Dhar, "Daltonize" (2005); tritanopia uses this engine's analogue `[[1,0,.7],[0,1,.7],[0,0,0]]` (not a published constant) |
| strength `t` | `I + t (M - I)`; Machado's paper interpolates its matrices linearly between severities |

The filter includes the 2D sprite/UI layer and pure-2D frames. Inactive settings
allocate no final-composite scratch and encode no filter pass. Active scratch
textures are owned per pipeline view and recreated on size/format changes;
callers need no sampled/copy-source swapchain usage.

## Late presentation and Frame Generation

The scene is rendered and tonemapped with both Overlay and Composite suppressed.
NGX receives UI-free SDR colour (`hudless: false` and no separate HUD-less input).
After evaluation, `GpuRenderer::composite_presentations` runs the same ordered
UI and final filters on every presentable generated output and the real output.
Source and target may alias: the source is first sampled into an owned scratch
image. Captures read those composed textures; presenting them only blits, so UI
is not applied twice.

One presentation sequence pins text and colour-filter data before scene/NGX work
and retains it for every output, even if extraction publishes a newer UI frame.
The snapshot is released on success/error. Participants are resolved with the
camera's effective feature overrides, then begun in execution order and ended
in reverse order exactly once (including a partially failing begin). Disabled
features acquire no snapshot. Known capability, blocker and sticky-failure
checks run before snapshot acquisition, so an unavailable FG request can fall
back natively without calling presentation hooks. Scene-only features, such as
the lazily installed RT prepasses, may be appended while presentation is pinned;
new Overlay/Composite registration is rejected until the sequence ends.
Late output uses separate overlay-only view state and does not advance the
scene's temporal history.

If interpolation fails after the scene rendered, its valid real colour remains
available in a poisoned stage. Only the real output is late-composited/presented;
scene rendering/history is not repeated for that source. Sticky failure blocks
further same-request evaluations; a retry replaces a poisoned generator.

The new integration has not been executed against NGX in this source checkpoint.
Synthetic GPU regressions exercise the same late-composition API, but they do
not establish hardware FG image quality or pacing.

The follow-up repair checkpoint adds unexecuted regressions in
`engine-render-pipeline/tests/gpu_pipeline.rs` for enabled participants, begin
failure cleanup, output failure cleanup and stable lazy scene registration.
Ignored native-adapter tests in `engine-desktop` exercise the actual `RtHost`
preparation on its first scene and on later activation during a pinned sequence,
and unavailable-FG native fallback with a failing presentation hook. Another
actual-host test renders a UI-free real source, injects interpolation failure at
the shared completion boundary, checks once-only alpha pixels, one scene-pass
execution and retained scene camera history. It bypasses NGX evaluation and
therefore provides no evidence that hardware generation ran. These tests, and
all earlier UI checkpoint tests, still require compilation and GPU execution
under the manager's build slot.

## Status and telemetry

`UiGpuQueue::status()` (also `Runtime::ui_render_status()` and
`RuntimeSnapshot.ui_render`) reports each feature, in the style of
`UpscalerStatus`:

```json
{"features":[
  {"name":"ui.sdf_text","state":"gpu","frames":412,"last_draws":3,"last_items":3},
  {"name":"ui.world","state":"fallback","reason":"no active Camera3D: world UI keeps the 2D camera path (DrawSpace::World meshes)","frames":0,"last_draws":0,"last_items":0},
  {"name":"ui.color_filter","state":"gpu","frames":0,"last_draws":0,"last_items":0}]}
```

`state` is `gpu` or `fallback`; the reason distinguishes *not registered*,
*registered but not prepared by a wgpu renderer*, *disabled for the camera
(`render_features`)*, *no active `Camera3D`* and, for the filter, *requested but no final presentation
has run it*. `EngineTextBackend::render_status()` (`TextRenderStatus`) reports
SDF text per path: `screen` (`ui.sdf_text`, used by screen text of SDF families) and
`world` (`ui.world`, used by `engine.WorldText` that asked for SDF). `state` is `sdf` when
every path in use is on the GPU and `sdf_fallback` otherwise, with the reasons of the
failing paths in use joined in `fallback_reason`; a path nothing asked for is still listed
in `screen` / `world` but does not make the state a fallback (a game with only screen SDF
text and no `Camera3D` reports `sdf`, with `world.state` `fallback` and its reason). The
report also carries `sdf_gpu_layouts`. The headless run report
(`project_run_headless`, `render_screenshot`) carries `ui_render` as an object with the
`features` array above (`name`, `state`, `reason`, `frames`, `last_draws`, `last_items`),
or `null` when no status was published; it does not carry `TextRenderStatus`.

## Source-checkpoint validation plan

Not executed for this revision yet:

- Package CPU tests, project `ui_gpu` tests, strict scoped Clippy and formatting.
- `gpu_ui`: actual SDF glyph between background/translucent foreground in both
  2D/3D; final filtering of those pixels; two aliased synthetic real/generated
  targets at three sizes/formats; once-only alpha/filtering; publication between
  outputs; retained text/filter snapshots and their release.
- Existing pipeline/light/post/UI GPU regressions and DLSS-feature compilation.
- Actual NGX FG remains a separate hardware check with SDK/runtime prerequisites.

## Verification

Historical observations below predate the source-only presentation revision.

Commands (GPU tests: RTX 5080, Vulkan, `--test-threads=1`):

- `cargo test -p engine-ui-render` and `-p engine-project`: filter matrices against
  the published values and identity at strength 0; billboard bases equal
  `UiWorldPanel::placement`; back-to-front order; the 2D camera fit and scissor;
  SDF layouts (one bitmap for every size, quads scaled, SDF pages carry a distance
  gradient); the routing rule and the 2D skip; the published frame; status
  reporting; settings accepted and rejected through the component descriptor.
- `cargo test -p engine-ui-render --test gpu_ui -- --ignored --test-threads=1`:
  measured on the reference machine (set `UI_RENDER_PNG_DIR` to keep the images).

| Check | Result |
| --- | --- |
| SDF "I" stem edge, 10% to 90% of peak, at a 2x and a 4x camera fit | 1.59 px and 0.83 px (coverage text magnified by the fit: 2.44 px and 4.44 px) |
| Panel behind an opaque quad (left half) / in front of it | left pixel red (occluded) / green (visible); panel pixels 3080 vs 6160 |
| Nearer panel wins regardless of entity order | pass for both orders |
| Edge-on (80 degrees) panel vs `billboard: full` | 25 vs 110 of 128 pixels on the middle row |
| World text depth test | 486 white glyph pixels clear, 0 behind a wall |
| Colour filter, red and green unlit patches (protan, deutan, tritan, achromat, half strength, daltonized) | within 3/255 per channel of the CPU reference, e.g. protanopia red -> (109, 95, 0), green -> (255, 229, 0) |

## Decision record

- **Data through a shared queue, not the render world.** The extracted render world
  holds only components registered for extraction, and the features cannot reach
  the runtime (fonts, layout, localisation, atlas). A small `Arc` queue in
  `engine-project`, filled by draw extraction and read by the features, keeps the
  GPU crate free of layout logic and `engine-project` free of GPU types.
- **Routing by readiness.** Routing keyed on registration alone would lose text on
  backends that never prepare features; keying on prepared-on-a-device keeps the
  fallback exact. The cost is the one-frame start-up handoff.
- **A shared painter stream.** Sprite and SDF producers publish compatible
  ordering keys while retaining their own shaders/resources. Existing whole-pass
  extensions keep their behaviour; constraints around the combined stream use
  `engine.sprites`.
- **Final display filtering.** The Composite stage reads an owned image after UI,
  including 2D-only and late generated/real outputs, without sampling a swapchain.
- **One matrix for the filter.** Simulation, daltonization and strength are all
  linear, so the shader multiplies one CPU-tested matrix.

## Not implemented

- SDF outlines, glow and soft shadows (the glyph shader supports an edge bias;
  there is no authored outline or shadow style yet).
- MSDF pages (engine-text generates none).
- Mipmaps for world panel textures, per-fragment ordering, interleaving with the
  built-in transparent meshes.
