# HDR post-processing

The post stack turns the HDR scene colour into the displayed image: exposure,
bloom and colour grading in HDR, then tonemapping, vignette and film grain on
the way to the target. It is built on the render extension API
([rendering-architecture.md](rendering-architecture.md)) as two built-in
features:

| Feature | Stage | Work |
| --- | --- | --- |
| `engine.post` | `PostProcess` | auto-exposure histogram, exposure scale, bloom chain, HDR grading (one ping-pong pass at output size) |
| `engine.tonemap` | `Tonemap` (after `engine.output`, so it supersedes it) | tonemap operator, vignette, film grain, sRGB encode for non-sRGB targets |

Both are disabled by default and switched on for a view that has
`ViewData::post = Some(PostSettings)` (an explicit `FeatureOverrides` entry for
either name wins). A view without settings runs exactly the old linear-clamp
`engine.output`, so existing images do not change. Depth of field and motion
blur are separate optional HDR features documented in
[rendering-depth-motion.md](rendering-depth-motion.md). Not in this release:
SSAO, SSR, TAA/FXAA.

## Order of operations

1. Exposure scale: `scene * scale` (off: 1, manual: `2^(compensation - ev100)`,
   auto: the adapted histogram scale times `2^compensation`).
2. Bloom (computed from the exposed scene, added in HDR).
3. Grading, in this order: white balance, exposure offset, contrast, saturation,
   hue shift, gain and lift, gamma, optional 3D LUT. All in scene-linear HDR.
4. Tonemap (display-linear 0..1).
5. Vignette, then film grain, in display-linear space.
6. sRGB encode when the target format is not an sRGB view (sRGB targets
   encode on write, as before).

`ScenePipeline::read_hdr` returns the HDR colour after steps 1-3.

## Tonemap operators

`TonemapOperator` / authored `tonemap`:

| Value | Curve |
| --- | --- |
| `none` | clamp to 0..1 (the default appearance) |
| `aces` | Narkowicz ACES fit, per channel: `x(2.51x+0.03)/(x(2.43x+0.59)+0.14)`, no extra pre-scale |
| `agx` | AgX-style: Rec.2020 inset, log2 over -12.47..4.03 EV, a polynomial sigmoid, outset, `pow 2.2` decode (greys stay grey within 0.5%) |
| `agx_punchy` | `agx` with a power 1.35 and saturation 1.4 look |
| `neutral` | Khronos PBR Neutral (start compression 0.76, desaturation 0.15) |

The CPU reference lives in `engine_render_pipeline::post::tonemap` and the
WGSL in the `engine::tonemap` module; the GPU test compares them.

## Exposure and `Camera3D.lighting.exposure`

Two separate scale factors exist and they multiply:

* `Camera3D.lighting.exposure` converts photometric light units (lux, lumens)
  to shading units when the lights are submitted. It is part of lighting, it
  is always applied, and the post stack never replaces it.
* `PostProcess.exposure` scales the finished HDR scene afterwards:
  * `off` (default): nothing; only `lighting.exposure` applies.
  * `manual`: `2^(compensation - ev100)`. Note this is the plain `2^-EV100`
    scale, without the 1/1.2 lens constant of the full photographic formula;
    use `lighting.exposure` for the photometric conversion (for example
    `1/(1.2*2^EV100)` there) and keep post EV100 at 0, or leave
    `lighting.exposure` at 1 and set the full range here.
  * `auto`: see below. Total scene scale = `lighting.exposure` (on the lights)
    x auto scale.

Pick one place to express the camera's exposure; if both are set the effects
simply multiply.

### Auto-exposure

A compute pass builds a 64-bin histogram of `log2(luminance)` of the exposed
HDR colour between `min_ev` and `max_ev` (bin 0 holds black pixels and is
ignored). The mass between `low_percentile` and `high_percentile` is averaged;
the target scale maps that average to middle grey: `0.18 / 2^average * 2^compensation`.
The current EV moves toward the target exponentially with the view's
`delta_time`: `ev += (target - ev) * (1 - e^(-speed * dt))`, with `speed_up`
(1/s) when the scene gets darker (the scale rises) and `speed_down` when it
gets brighter. The remaining gap after `t` seconds is `e^(-speed * t)`: at the
default `speed_up` 3 it is 5% after 1 s and 0.25% after 2 s (measured, below).
A camera cut (`ViewData::reset_history`) snaps to the target at once; the first
frame of a view does too. The desktop host measures `delta_time` from the wall
clock (capped at 0.25 s). `ScenePipeline::auto_exposure(view)` reads the state
back (blocking; for tests and tools). The histogram range bounds the
reachable EV: a scene outside `min_ev..max_ev` saturates at the end bins.

## Bloom

Threshold with a soft knee (`knee` x `threshold` wide) on the exposed
luminance, then a dual-filter mip chain in `Rgba16Float` (half resolution
first, up to 8 mips; the prefilter uses four bilinear taps, downsample a
five-tap dual filter, upsample an eight-tap tent). Each upsample is added into
the finer mip with weight `scatter`; the finest mip, scaled by `intensity` and
`tint`, is added to the scene. Larger `scatter` widens the halo. Content below
the threshold is untouched.

## Grading and 3D LUT

Parameters: `temperature`, `tint` (a linear white-balance model normalised to
keep white's luminance, not a chromatic adaptation transform),
`exposure_offset` (stops), `contrast` (power about the 0.18 pivot),
`saturation` (0 is grey), `lift`, `gamma`, `gain`, `hue_shift_degrees`
(rotation about the grey axis), `lut` and `lut_intensity`.

A `.cube` LUT is an asset of kind `lut` (add it to the manifest like any asset;
the `lut` field of `grading` holds its id). `engine_render_pipeline::Lut3d::parse_cube`
validates it at the renderer boundary: `LUT_3D_SIZE` 2..=64, exactly `size^3`
finite triples, `DOMAIN_MIN < DOMAIN_MAX`, at most 16 MiB; 1D LUTs and unknown
keywords are rejected. The runtime reads the file (bounded, UTF-8) into the
frame; the renderer parses it, caches it by content, uploads an `Rgba16Float`
3D texture and samples it trilinearly in a reversible `x/(1+x)` compressed
domain so HDR values pass through (an identity LUT is a no-op within fp16
precision). A LUT that fails to parse is reported in `PostStatus.lut_error` and
not applied; grading otherwise still runs. A volume can set a LUT but cannot
clear one (`null` is the same as unset).

## Vignette and grain

Vignette: distance from `center` (aspect-corrected, 1 at the farthest corner),
`smoothstep(1 - smoothness, 1, d)`, blended toward `color` by `intensity`: a
black vignette of intensity 0.5 halves the corners. Grain: per-pixel hashed
zero-mean uniform noise in [-1, 1] scaled by `intensity * lerp(1, 1 - luma,
response)`, re-seeded every frame by a per-view frame counter.

## Authored data

Registered in `crates/engine-project` (`post.rs`), documented by
`component_schema`, validated on load and edit, editable with `component_set`
(unknown fields and out-of-range values are rejected).

* `PostProcess` on the camera entity: `tonemap`, `exposure`, `bloom`,
  `grading`, `vignette`, `grain` (full field lists and ranges in
  `component_schema`). No `PostProcess` and no influencing volume means the
  default output.
* `PostProcessVolume`: `enabled`, `shape` (`global`, `box {size}`,
  `sphere {radius}`; non-global shapes need Transform and are scaled by it),
  `priority` (-1000..1000), `blend_distance` (metres), `weight` (0..1), plus the
  same groups as `PostProcess` with **every field optional**: an unset field is
  not overridden.

### Blending

`engine_project::post::resolve_post(camera, volumes, position)`:

1. The base is the camera's `PostProcess` (or the defaults) - the global layer.
2. Enabled volumes apply in ascending `priority`, ties by entity ID, so higher
   priorities win. Each has an influence = `weight` x distance factor, where the
   factor is 1 inside the shape and `1 - d / blend_distance` outside (0 beyond;
   `blend_distance` 0 is a hard edge; `d` is world metres).
3. Continuous values interpolate `base + (override - base) * influence`;
   booleans, enums, the tonemap mode and the LUT id switch at an influence of
   0.5. Volumes with zero influence are skipped.
4. If there is no camera `PostProcess` and no influencing volume the result is
   `None` and the view keeps the default output.

The desktop renderer evaluates it per view at that view's camera position
(`ViewData::camera_position`), so XR eyes and extra views each get their own
result.

## Status

`RuntimeSnapshot.post` (`Runtime::post_status`) is small: `state`
(`not_rendered` until a frame was drawn; before it, the authored data resolved
at the camera), `active`, `volumes` (enabled volumes in the scene),
`active_volumes` (`id`, `priority`, `influence`, lowest priority first), the
effective `tonemap` and `exposure_mode`, flags `bloom`/`grading`/`vignette`/
`grain`, and `lut`/`lut_error`. The measured auto-exposure EV is not part of
the snapshot (reading it blocks the GPU); use `ScenePipeline::auto_exposure`.

## Native API

`ViewData::post: Option<PostSettings>` (`PostSettings`, `ExposureSettings`,
`BloomSettings`, `GradingSettings`, `VignetteSettings`, `GrainSettings`,
`Lut3d`); `PostSettings::validate` rejects non-finite and out-of-range values
before anything is encoded. `names::POST` / `names::TONEMAP` are the feature
names.

## Measured results

RTX 5080, Vulkan, `--ignored --test-threads=1`
(`crates/engine-render-pipeline/tests/gpu_post.rs`, 64x64 views):

* Tonemap, input 4.0: `none` 1.0 (clamp), ACES 0.9734 (reference 0.97342),
  AgX 0.863 (0.863 / 0.861 / 0.861 reference), neutral 0.982 (0.983); input
  0.18 -> ACES 0.2664 (0.2669), AgX 0.2159, neutral 0.141 (0.140). All within
  8-bit quantisation of the CPU reference.
* Auto-exposure: scene 8.0 -> EV -5.474 (ideal -5.474), scene 0.02 -> EV 3.193
  (ideal 3.170, bin width 0.22 EV); switching bright to dark rose monotonically
  and the remaining gap was 0.0498 after 1 s and 0.0025 after 2 s at 3/s (model
  0.0498 / 0.0025); the exposed dark pixel was 0.1829 (target 0.18); a camera cut
  snapped to the target.
* Bloom: a 50.0 pixel with threshold 1, intensity 1, scatter 0.7: centre 52.97,
  neighbours at 1/2/4/8/16 px = 2.27 / 0.83 / 0.135 / 0.098 / 0.019 (monotonic);
  scatter 0.2 at 16 px 0.001; a uniform 0.5 scene (below threshold) deviated by 0.
* Grading: saturation 0 on (0.8, 0.4, 0.1) gave grey 0.4631 (luma 0.4634);
  contrast 2: 0.36 -> 0.7202 (0.72), 0.09 -> 0.0450 (0.045); identity 17^3 LUT
  returned 4.0039 for 4.0 and 0.2998 for 0.3; manual EV100 2 on 0.8 -> 0.19995.
* Vignette (intensity 0.5): corner / centre = 0.4974 (expected 0.5). Grain
  (0.2): mean 0.4990 / 0.4988 over two frames, 98% of pixels changed between
  frames, standard deviation 0.1151 (uniform-noise model 0.1155).
* Desktop pixel test (`crates/engine-desktop/tests/gpu_post.rs`): a camera
  inside a local box volume with manual EV100 1 rendered 137 (sRGB of 0.25), the
  same scene outside the volume 188, identical to the scene with no post data.

## Limits

* One `PostSettings` per view; `engine.post` skips the pass entirely when only
  tonemap, vignette or grain are set (no extra HDR copy).
* Auto-exposure samples every pixel of the output-size HDR colour; no metering
  mask or centre weighting.
* The LUT is applied in HDR before tonemapping, not on the display image.
* AgX is the common polynomial approximation with its own constants, not
  Blender's exact transform.
