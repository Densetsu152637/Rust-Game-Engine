# UI render features (`engine-ui-render`)

GPU features of the game UI, written against the render extension API
([rendering-architecture.md](rendering-architecture.md)). Status: implemented and
GPU-tested on Windows / RTX 5080 (Vulkan); see [Verification](#verification).

| Feature | Stage | Draws | Needs |
| --- | --- | --- | --- |
| `ui.sdf_text` | Overlay (after `engine.sprites`) | screen-UI text of SDF font families from engine-text SDF atlas pages with `GLYPH_SHADER_WGSL` | any wgpu frame (2D or 3D) |
| `ui.world` | Transparent (after `engine.transparent`) | `engine.UiWorldPanel` and `engine.WorldText` in `Camera3D` scenes: depth-tested, back-to-front, billboarded per view, SDF text | a `Camera3D` |
| `ui.color_filter` | PostProcess (after `engine.post`) | the colour-vision filter of `engine.AccessibilitySettings` | a 3D frame (2D-only frames have no post stage) |

The crate depends on `engine-render-pipeline`, `engine-project` and `engine-text`;
`engine-desktop` is only a dev-dependency (GPU tests), and the renderer needed no
change.

## Opting in

```rust
// Before the runner starts (the app is the runtime's EngineApp):
engine_ui_render::install(runtime.app_mut())?;
```

`install` takes the runtime's `UiGpuQueue` resource, creates the `PipelineFeatures`
resource when the game has none and registers the three features
(`install_with(app, UiRenderOptions { .. })` picks a subset). Games that build
`PipelineFeatures` themselves call `register(&mut features, &queue)`. The desktop
runner and `render_screenshot` already hand `PipelineFeatures` to the renderer
(`GpuRenderer::set_features`), which prepares the features on its device.

`ui.sdf_text` is an Overlay-stage feature: a registered overlay feature disables
DLSS Frame Generation ([rendering-architecture.md](rendering-architecture.md#host-integration)).
Leave it out (`sdf_text: false`) when a project uses frame generation and no SDF text.

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
renderer) routes nothing. Two consequences:

- The very first presented frame of a window is extracted before the renderer
  exists, so it still shows the 2D fallback; later frames use the features.
- Hosts that extract before preparing the features (`render_screenshot` extracts
  first) render that frame with the fallback.

Details of the routing:

- SDF output is requested from the text backend only while the screen roots are
  extracted; world panels keep coverage text at their own raster density (a panel
  is a texture-like surface rasterised 1:1 at its `pixelsPerUnit`).
- A screen root with a dialog, dropdown popup or tooltip is extracted with
  coverage text for that frame. SDF text composites above the whole sprite/UI layer
  and could not stay under the overlay (see Limits).
- SDF runs are recognised by their draw ID (`game-ui:...:sNN`, `is_sdf_draw`); the
  runtime splits them off the draw list in `extract_frame`.

## `ui.sdf_text`

Overlay is the stage where the UI composites: after tonemapping, in the target
format, after the host's sprite/UI node. The feature draws the SDF runs of
`UiGpuFrame::sdf_text` (UI-space meshes in logical pixels, sorted by layer then ID)
with the glyph shader from `engine-text` (mode 1, signed distance in alpha,
`smoothstep` over `fwidth`, so edges stay about one pixel wide at any scale). The
2D camera fit is reproduced exactly (`layout::UiFit`: integer scale above 1x,
fractional below, centred letterbox), so the text lines up with sprites; clips
become physical scissor rectangles. Pages are uploaded once per `Arc` snapshot
(the text backend republishes a page only when it changes). Colours are written
unchanged into the target, as the sprite path does.

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

Maths ([`color`]): one 3x3 matrix on **linear** sRGB (Rec. 709) RGB, applied in the
HDR PostProcess stage. Linear is chosen because the matrices are defined for linear
RGB and the HDR scene colour is linear; applying them to gamma-encoded values would
shift hues. The filter runs after `engine.post` (exposure, bloom, grading) and
before tonemapping, so it filters scene-referred colour: with the default clamp
output the result is exactly the matrix applied to the displayed linear colour, with
a nonlinear tonemapper the result differs slightly from filtering the final image.

| Mode | Matrix |
| --- | --- |
| simulate protanopia, deuteranopia, tritanopia | Machado, Oliveira and Fernandes, "A Physiologically-based Model for Simulation of Color Vision Deficiency", IEEE TVCG 15(6), 2009, severity 1.0 ([tables](https://www.inf.ufrgs.br/~oliveira/pubs_files/CVD_Simulation/CVD_Simulation.html)) |
| simulate achromatopsia | Rec. 709 luminance in all channels |
| correct (daltonize) | `I + S (I - Sim)`: the simulation error moved into the channels the deficiency keeps. `S` is the protan/deutan shift `[[0,0,0],[.7,1,0],[.7,0,1]]` after Fidaner, Lin and Dhar, "Daltonize" (2005); tritanopia uses this engine's analogue `[[1,0,.7],[0,1,.7],[0,0,0]]` (not a published constant) |
| strength `t` | `I + t (M - I)`; Machado's paper interpolates its matrices linearly between severities |

The 2D sprite/UI layer composes after tonemapping and is **not** filtered, and a
frame without a `Camera3D` has no PostProcess stage, so the filter does nothing in
2D-only games. The status says so (`requested, but ... no 3D frame has run it`).

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
(`render_features`)*, *no active `Camera3D`* and, for the filter, *requested but no 3D
frame has run it*. `EngineTextBackend::render_status()` (`TextRenderStatus`) reports
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

## Verification

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
- **SDF text above the sprite layer.** The sprite pass samples textures with a
  nearest sampler and has no distance-field shader (custom 2D materials use a
  nearest sampler too), and the Overlay stage is where the extension API composites. Layering
  relative to other sprites and UI roots is therefore not preserved (only among SDF
  runs, and dialogs/popups fall back to coverage).
- **One matrix for the filter.** Simulation, daltonization and strength are all
  linear, so the shader multiplies one CPU-tested matrix.

## Not implemented

- SDF outlines, glow and soft shadows (the glyph shader supports an edge bias;
  there is no authored outline or shadow style yet).
- Filtering the 2D sprite/UI layer and 2D-only frames (the Overlay stage cannot
  read the composited target).
- MSDF pages (engine-text generates none).
- Mipmaps for world panel textures, per-fragment ordering, interleaving with the
  built-in transparent meshes.
