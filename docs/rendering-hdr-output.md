# HDR display output

HDR output sends scene brightness above paper white to an HDR display. It needs
three things that float intermediates alone do not give: a swapchain in an HDR
colour space, an output transform that encodes for that colour space, and UI
that sits at a deliberate paper white. This page is the model, what wgpu 30
does and does not expose per backend, the encodings, the UI and frame
generation rules, the fallback and the manual check.

The default is off. Without a request the renderer runs the SDR sRGB path
exactly as before (`ViewData::linear_output` is `false`, the sRGB surface and
the unclamped-output flag are untouched).

## Model

```text
scene HDR -> post/tonemap stage -> sprites + UI  -> HDR output transform -> HDR surface
 (scene-linear)  (display-linear, 1.0 = paper white,     (roll-off to peak, scRGB or
                  unclamped Rgba16Float image)            Rec.2020 + PQ)
```

With HDR active (`GpuRenderer::enable_hdr_output`):

1. The frame renders into an engine-owned `Rgba16Float` *display-linear* image
   instead of the surface. `ViewData::linear_output` is set, so `engine.output`
   and `engine.tonemap` write the colour unclamped: no 0..1 clamp, no SDR
   tonemap operator, no sRGB encode. Exposure, bloom, grading, vignette and
   grain still run.
2. Sprites and screen-space UI draw into the same image as ordinary linear
   colour. `1.0` is paper white, so a white UI element is exactly paper white.
3. `HdrOutputPass` (`engine_render_pipeline`, WGSL `engine::hdr`) reads the
   image and writes the surface signal: roll-off to the peak, then scRGB or PQ.

The SDR tonemap operators (ACES, AgX, Neutral) are bypassed in HDR output; the
roll-off below replaces them. Scene-linear `1.0` is paper white (203 nits by
default), values above it are highlights. World-space UI (quads in the 3D
scene) is part of the scene and keeps its authored radiance.

## Capability detection

wgpu 30.0.1 can select and report surface colour spaces. Everything below is
read from the vendored sources (`~/.cargo/registry/src/*/`).

- `SurfaceConfiguration::color_space: SurfaceColorSpace`
  (`wgpu-types-30.0.1/src/surface.rs:914`; enum at `:242`; `is_hdr` at `:447`).
  `Auto` (default) never selects HDR except the linear fp16 case
  (`wgpu-core-30.0.1/src/device/surface_config.rs:24-40`).
- `SurfaceCapabilities::format_capabilities: Vec<SurfaceFormatCapabilities>`
  gives each format and its `SurfaceColorSpaces` set (`surface.rs:491`, `:506`).
  `SurfaceCapabilities::formats` lists only formats usable with `Auto`; HDR-only
  formats appear only in `format_capabilities`, so the engine reads that.
- `Surface::display_hdr_info(&Adapter) -> DisplayHdrInfo`
  (`wgpu-30.0.1/src/api/surface.rs:71`; struct `wgpu-types-30.0.1/src/surface.rs:587`)
  returns peak/SDR-white nits (Windows), EDR headroom (Apple) and a coarse
  `high_dynamic_range` flag. Every field is advisory; `None` means unknown.
- wgpu does not tonemap, gamut-map or encode: for every non-`*Srgb` surface
  the shader output must already carry the transfer function and primaries
  (`wgpu-30.0.1/src/lib.rs:74-175`, per-colour-space table at `:156-175`). The engine does that
  in `HdrOutputPass`.

What the backends advertise (and so what the engine can use):

| Backend | scRGB `Rgba16Float` + `ExtendedSrgbLinear` | HDR10 `Rgb10a2Unorm` + `Bt2100Pq` | Notes |
| --- | --- | --- | --- |
| DX12 | always advertised (`wgpu-hal-30.0.1/src/dx12/adapter.rs:1325-1354`) | always advertised on `Rgb10a2Unorm` (same lines) | Advertised regardless of the OS HDR toggle ("Windows always composites in scRGB"); the live HDR state is `display_hdr_info` (`auxil/dxgi/hdr.rs:59-107`, `coarse.high_dynamic_range`, nits from DXGI). HLG and P3 are never reported. |
| Vulkan | if the driver lists `EXTENDED_SRGB_LINEAR_EXT` for `R16G16B16A16_SFLOAT` | if the driver lists `HDR10_ST2084_EXT` for `A2B10G10R10_UNORM_PACK32` | `VK_EXT_swapchain_colorspace` is requested (`wgpu-hal-30.0.1/src/vulkan/instance.rs:280-282`); mapping in `vulkan/conv.rs:161-203`. Depends on the driver and OS HDR state. `display_hdr_info` has no data off Windows (all `None`). |
| Metal | `ExtendedSrgbLinear` enables EDR (`metal/surface.rs:279-292`) | PQ supported (`metal/surface.rs:310`) | `display_hdr_info` (headroom) must be called on the main thread (`wgpu-30.0.1/src/api/surface.rs:60-70`). The engine's scRGB unit assumption below is not validated on Metal. |
| GL / WebGPU | not used | not used | WebGPU offers only encoded `ExtendedSrgb`/`ExtendedDisplayP3` (`wgpu-30.0.1/src/backend/webgpu.rs:4145-4168`); the engine has no encoder for them. |

The engine supports two surface modes:

- **scRGB**: `Rgba16Float` + `ExtendedSrgbLinear`.
- **PQ (HDR10)**: `Rgb10a2Unorm` + `Bt2100Pq`.

HLG, encoded extended sRGB and extended Display-P3 are real wgpu colour spaces
but are not implemented here.

`select_hdr_surface` (pure, in `engine_desktop::hdr`) takes the request, the
`format_capabilities` list and the `DisplayHdrInfo`, and returns either the
format/colour space to configure or an SDR fallback with a reason:

- `Auto` prefers scRGB, then PQ; `ScRgb`/`Pq` are never substituted.
- The colour space decides, not the texel format: an `Rgba16Float` offered only
  as sRGB is not HDR.
- Because DX12 advertises HDR configurations even with Windows HDR off, a
  display that positively reports `high_dynamic_range == Some(false)` keeps the
  SDR path ("the display reports that it is not in an HDR mode"). Unknown
  (`None`) is treated as capable, since the info is advisory.
- Invalid paper white or peak, an absent configuration, or a driver that
  rejects the configuration (checked in a validation scope) all fall back to
  SDR.

`HdrStatus` (`WindowSurface::hdr_status`, `GpuRenderer::hdr_status`) reports
`requested`, `active`, `surface_format`, `color_space`, `encoding`, effective
paper white and peak, the display's reported peak, and `fallback_reason`.
`active` is true only when the configured surface is one of the two HDR modes
and the output transform is running.

## Configuration

HDR is requested at the desktop level (there is no authored display or window
setting in `engine-project`; present mode is likewise an environment variable),
so no project schema changed:

| Variable | Values | Default |
| --- | --- | --- |
| `ENGINE_HDR` | `off`, `auto` (scRGB, else PQ), `scrgb`, `pq` | `off` |
| `ENGINE_HDR_PAPER_WHITE_NITS` | 20..=1000 | 203 (ITU-R BT.2408 reference white) |
| `ENGINE_HDR_PEAK_NITS` | paper white..=10000 | 1000 |

Invalid values are errors naming the variable. `WindowSurface` reads them when
it is created; `HdrRequest` / `WindowSurface::set_hdr_request` do the same
programmatically and re-run the capability selection (use it after the
operating system toggled HDR). The peak is a cap: when the platform reports the
panel peak (Windows), the lower value is used, never below paper white.

A resize keeps the configured surface format and colour space (the stored
`SurfaceConfiguration` is only resized) and the display-linear image is
recreated at the new size on the next frame.

## Encodings

Input to the transform is display-linear Rec.709 relative to paper white
(`rgb`, `1.0` = paper white). `HdrOutputSettings::encode` is the CPU reference;
`wgsl/hdr.wgsl` mirrors it and a unit test checks that the constants and the
Rec.709 to Rec.2020 matrix rows in the WGSL are the Rust ones.

1. Clean: NaN to 0, clamp to `0..=65504`.
2. Roll-off with `headroom = peak / paper_white`: the brightest channel `m` is
   unchanged up to `start = min(0.5 * headroom, 1.0)` (so paper white is
   untouched whenever headroom >= 2) and above it follows
   `start + r * (1 - exp(-(m - start) / r))` with `r = headroom - start`. The
   curve has slope 1 at the knee, is monotonic, approaches but never exceeds
   `headroom`, and the colour is scaled by `rolloff(m) / m` so hue survives.
3. Encode:
   - **scRGB**: `rgb * paper_white / 80` (Rec.709 primaries, linear, `1.0` = 80
     nits as DXGI `RGB_FULL_G10_NONE_P709` defines it). Paper white 203 nits is
     2.5375.
   - **PQ**: Rec.709 to Rec.2020 (`post::tonemap::rec709_to_rec2020`, the same
     matrix AgX uses), times paper white in nits, then the ST 2084 inverse EOTF
     (`pq_oetf`): 100 nits 0.5081, 203 nits 0.5807, 1000 nits 0.7518, 10000 nits
     1.0.

wgpu's own text describes `1.0` in the extended spaces as "SDR white"; on
Windows scRGB `1.0` is 80 nits regardless of the SDR slider, which is what the
engine uses. On Apple EDR `1.0` follows the display's SDR white, so absolute
nits are not guaranteed there (not validated).

The Rec.709 gamut is not extended: colours stay within Rec.709 chromaticity
(no negative scRGB components, PQ output sits inside the Rec.2020 triangle).

## UI paper white

Screen-space sprites and UI composite after the scene into the display-linear
image, where `1.0` is paper white; the output transform maps that to
`paper_white_nits`. UI is therefore neither 80 nits (raw scRGB `1.0`) nor the
display peak, and it follows the paper-white setting. Blending happens in linear
light, as on the sRGB path (sRGB targets also blend in linear). SDR is
unchanged: the sRGB surface still receives hardware-encoded linear colour.

## Frame Generation

DLSS Frame Generation in this engine uses an 8-bit SDR colour path
(`Rgba8Unorm` backbuffer, `hdr = false`; `engine-dlss/src/framegen.rs:15-19`,
`framegen/native.rs:60,865`) and a late presentation composite that writes the
surface format directly. The SDK binding supports `Rgba16Float` with
`GenerationFrame::hdr`, but the desktop renderer has not been validated with
it, and it could not be tested here. HDR output therefore gates Frame
Generation off: `FrameGeneration::set_output_gate` makes `precheck` return
"frame generation is unavailable while HDR output is active ...", the camera's
`frame_generation_status` is `Fallback` with that reason, and real frames
present through the HDR path. Turning HDR off removes the gate. The DLSS SDK
bindings are unchanged.

DLSS Super Resolution is not gated: its output feeds the same unclamped output
stage.

## Fallback

Any of: HDR not requested, no matching `format_capabilities`, a display that
reports SDR, an invalid request, or a rejected configuration leaves the
surface on the first sRGB format with `Auto` colour space, the renderer on the
SDR path, and `HdrStatus::fallback_reason` set (except for "not requested").
Nothing reports HDR active unless the configured surface is `Rgba16Float` +
`ExtendedSrgbLinear` or `Rgb10a2Unorm` + `Bt2100Pq`.

## Known limits

- Post-effects that run after the output stage on the final image (depth of
  field, motion blur, spatial AA) now see display-linear unclamped values in the
  float image; they were not validated in HDR output.
- `composite_presentations` (late presentation, used by Frame Generation) and
  XR eye targets do not use the HDR path.
- The window surface path in `engine-render-wgpu/src/surface.rs` (the generic
  backend) stays sRGB-only.
- HLG, encoded extended sRGB/P3, HDR metadata (ST 2086/MaxCLL) and dynamic
  mid-run HDR toggling are not handled automatically (call `set_hdr_request`).
- No SDR-to-HDR inverse tone mapping: SDR sprites and textures are not
  expanded.

## Tests

CPU unit tests (`engine-render-pipeline` `hdr::tests`, `engine-desktop`
`hdr::tests`): PQ known code values and round trip, scRGB scale, paper-white
mapping, Rec.709 to Rec.2020, roll-off monotonic and capped at the peak, config
validation, WGSL constant agreement and naga validation, capability selection
over HDR-capable, SDR-only, float-only and SDR-display lists, environment
parsing and status.

GPU tests are offscreen readbacks and are `#[ignore]` by default (run with
`--ignored --test-threads=1` on a real adapter, like the other GPU tests):

- `engine-render-pipeline/tests/gpu_hdr.rs`: unclamped linear output through
  both output stages, SDR clamp/encode unchanged, the pass against the CPU
  encode for scRGB and PQ, paper white and peak, the full chain, format
  validation.
- `engine-desktop/tests/gpu_hdr.rs`: scene above paper white plus UI at paper
  white on a scRGB and a PQ target, SDR bit-identical when HDR is off or after
  a toggle, resize keeping the format, the Frame Generation gate.

Presenting on a real monitor is a separate manual check.

## Manual check on an HDR display

Not done as part of the implementation. Steps:

1. Turn on HDR in the operating system display settings (Windows: Settings,
   System, Display, Use HDR) and note the SDR content brightness.
2. Run a windowed example with `ENGINE_HDR=auto` (try `scrgb` and `pq` too).
3. Check `WindowSurface::hdr_status()`: `active`, the surface format and colour
   space, and the reported display peak.
4. With a scene containing bright emissive or sun highlights, confirm the
   highlights are visibly brighter than the white UI and do not clip at the UI
   white; confirm the UI white matches other SDR content at
   `ENGINE_HDR_PAPER_WHITE_NITS` (raise it to see the UI brighten).
5. Set `ENGINE_HDR_PEAK_NITS` below the panel peak and confirm highlights stop
   at that level; check no banding in dark gradients on `pq`.
6. Turn OS HDR off, call `set_hdr_request` (or restart): the status should show
   the fallback reason and the SDR image.
7. Resize and move the window between an HDR and an SDR monitor and confirm the
   format stays and the picture is correct.
