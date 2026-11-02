# Native temporal resolve

`engine-render-post-effects` provides `effects.temporal_resolve`
(`TemporalResolveFeature`), a vendor-neutral WGSL temporal resolve. At render
scale 1 it is temporal anti-aliasing; below it a temporal upscaler. It is an
engine algorithm and is **not** FSR, XeSS or DLSS, and no claim of equivalent
quality is made.

Authoring: `Camera3D.upscaler.mode = "native_temporal"` (or `"auto_temporal"`,
DLSS when it can run, else this). `quality` picks the per-axis render scale:
`dlaa` 1 (anti-aliasing only), `quality` 2/3, `balanced` 0.58, `performance`
0.5, `ultra_performance` 1/3 (`UpscalerQuality::native_render_scale`; render
size is rounded, at least one pixel). Absent or `none` leaves every pixel as it
was (a pipeline-level test compares the bits with a pipeline that never
registered the feature; the desktop test compares mode on/off/on).

## Ownership: one jitter owner, one Upscale winner

* **Jitter.** The pipeline applies `ViewData::jitter` and invents none. The host
  renderer is the only owner: when the native resolve is active it sets
  `view.jitter = jitter_offset(frame, jitter_phase_count(render, output))` from
  `engine_render_pipeline::jitter`: Halton(2,3) offsets in render pixels, each
  axis in (-0.5, 0.5), zero mean, `ceil(8 * ratio^2)` phases clamped to 8..=256
  (8 at 1:1, 32 at 0.5, 72 at 1/3). The same sequence feeds DLSS. A reset frame
  restarts the sequence at phase 0, so a cut or a re-enable renders exactly what
  a first frame renders.
* **Upscale winner.** `Upscale` is a last-enabled-wins stage. The feature is
  ordered after `engine.upscale`, so when enabled it supersedes the bilinear
  upscale (the pipeline report shows `engine.upscale` as `superseded`). A host
  that returns `HostUpscale::Upscaled` (DLSS) replaces the whole stage instead
  and the feature reports `replaced_by_host`; the desktop host never enables both.
* **The mode names the owner.** The host enables the feature, and sets its
  controls, from the effective `UpscalerSettings.mode` alone; an authored
  `render_features["effects.temporal_resolve"]` is ignored. Precedence: a runtime
  `UpscalerControl.override_settings` replaces the authored request whole;
  `dlss_sr` that cannot run renders the plain native frame (never the native
  resolve) and reports `fallback` with the reason; `auto_temporal` runs DLSS if
  it can, else the native resolve, reporting `active`, `backend =
  native_temporal` and the DLSS reason in `fallback_reason`. Under DLSS Frame
  Generation the native resolve does not run (reported as `fallback`).
* **Spatial AA** is switched off for a draw the resolve owns (the temporal
  resolve already anti-aliases) and restored when the request ends; the status
  field `spatial_aa_skipped` reports it.

## Algorithm (one fragment per output pixel, `temporal.wgsl`)

1. **Reconstruction.** The 3x3 jittered render texels around the pixel are
   weighted by `exp(-k d^2)`, `d` the distance in render pixels from the pixel
   centre to the texel's unjittered position (`texel + 0.5 - jitter`), `k` =
   `filter_sharpness` (default 5). Colour is processed as `c / (1 + luma(c))`, a
   reversible compression, so a bright HDR sample cannot dominate the
   neighbourhood or the blend. Inputs are clamped to a non-negative, bounded
   range; a NaN in the *scene* is zeroed only where the implementation preserves
   NaNs (WGSL does not require it), so that is best effort. Non-finite *history*
   (colour tap or stored distance/count) is detected exactly on the bits and
   dropped: the pixel restarts from the current frame.
2. **Reprojection.** Depth and motion come from the closest texel of the 3x3.
   Motion is the prepass's current-to-previous motion in render pixels (history
   position = pixel + motion scaled to output pixels). History is sampled with a
   Catmull-Rom filter (five bilinear taps; an exact texel load when the offset is
   below 0.002 pixel, so static content is not resampled).
3. **Disocclusion.** History is rejected outside the image, where nothing was
   accumulated, or when its stored view distance differs from the distance this
   surface had under camera motion alone (current view position moved by the
   previous-to-current camera transform) by more than 4 % plus three pixels of the
   surface's own slope (the smaller one-sided distance difference per axis, so a
   silhouette does not widen the tolerance across the edge), gated by curvature:
   an axis contributes only when its two one-sided differences have opposite
   sign (a ramp); a one-pixel ridge or valley has the same sign on both sides and
   contributes nothing, so a thin object's step to its background cannot widen
   the tolerance over the background. The slope term keeps
   history on a receding, grazing plane, where the nearest-texel read alone
   changes the distance by tens of percent. A moving
   object's own depth motion is not modelled; fast motion along the view axis
   therefore loses accumulation (more aliasing, no ghost).
4. **Clipping.** History is clipped toward the box of the 3x3 neighbourhood in
   YCoCg: mean +/- 1.25 sigma intersected with the min/max box. Below render
   scale 1 the clip is blended away with `clamp(2 (1 - render/output), 0, 1)`
   (none at 1:1, full at 0.5 and below): a feature narrower than a render pixel
   is missed by some jitter phases, and clipping to those frames' box erases it.
   Geometry changes are still rejected by the distance test; the cost is that a
   pure shading change at reduced scale is followed by the history window
   instead of being clipped.
5. **Accumulation.** `n` is the history's accumulated frame count (capped at
   `history_frames`, and at 3 for fast pixels: the cap falls from the window to
   3 as screen speed reaches 4 output pixels per frame); the new frame weighs
   `1 / (n + 1)`. The default window is `clamp(phase_count, 16, 64)` frames.
6. **Outputs.** One pass writes the resolved HDR (the Upscale output later
   stages read exactly as for the bilinear or DLSS output), the compressed
   history colour and `(view distance, count)` (`Rg32Float`) for the next frame.

## History lifecycle

History (two ping-pong slots of colour + distance per view, output size) lives
in the `TemporalControls` store shared by the host and the feature.

| Event | Result |
| --- | --- |
| First frame, `set(view, None)`, forget | no history; `set(None)` and `forget_view` free the textures |
| Camera cut (`reset_history`, `reset_temporal_history()`), projection change | `reset_history` callback discards; the frame is a fresh resolve |
| Output size change (resize) | history textures are reallocated at the new size and the frame is a fresh resolve |
| Render-only size change (render scale at constant output) | textures are kept; the host passes a reset and the pipeline resets on the target change, so the history is discarded through that forced reset |
| Scene attempt aborted or failed after encode | pending slot is dropped, `reset_next` resets the retry |
| Scene committed (`finish_scene`) | pending slot becomes the history, with its camera/projection |
| Mode switch (to DLSS, to none), 2D frame | the host calls `set(view, None)`, freeing the history |
| Several views | one entry per `ViewId`; no sharing |

`TemporalControls::status(view)` reports `history_valid`, `history_size`,
`resolved_frames` and `history_discards` (counts a valid history being thrown
away).

## Evidence (RTX 5080, Vulkan, `--ignored --test-threads=1`)

`crates/engine-render-post-effects/tests/gpu_temporal.rs` renders analytic
scenes through a real `ScenePipeline` (jittered shaders and an analytic motion
producer), 96x64 output. Errors are summed squared error against 16x16
coverage-integrated truth:

* Static diagonal edge plus 1.5-pixel stripe at 1:1: aliased 13.75, converged
  (40 frames) 4.11; 0 intermediate edge pixels become 78; steady-state
  frame-to-frame change 0.05 maximum (HDR units, white = 1).
* Render scale 0.5 (48x32 to 96x64), 96 frames, squared error against truth
  (temporal / built-in bilinear): total 16.38 / 35.42, edge region 5.83 / 8.40,
  1.5-pixel stripe (0.75 render pixels) 10.56 / 27.03. History found: before the
  clip was relaxed the stripe was 40.9 (worse than bilinear, close to an erased
  stripe at about 45) and the total 46.9 against 35.4, while the test asserted only
  the edge region; it now asserts the edge, the stripe and the total.
  The stripe is not above Nyquist for an accumulating resolve: without clipping
  it reconstructs to 10.6.
* Sharpness sweep (`sharpness_sweep_measurement`, edge region at 1:1 / 0.5): k=2
  2.57 / 10.60, k=3 1.66 / 7.90, k=5 1.08 / 5.83, k=8 0.91 / 5.02 (baselines 4.73
  / 8.40); stripe region at 1:1 / 0.5: k=2 6.29 / 17.65, k=3 4.34 / 13.91, k=5
  3.01 / 10.56, k=8 2.97 / 9.07 (baselines 9.03 / 27.03).
* One-pixel feature (`a_one_pixel_feature_does_not_inherit_the_background_history`):
  a 1-pixel object with no reported motion over a checker equals a no-history
  resolve (worst difference 0.0000); with the slope term ungated the same test
  fails (0.315). `non_finite_history_is_dropped_and_the_output_stays_finite`
  writes NaN and infinities into the committed history
  (`TemporalControls::poison_history_pixel`) and the output stays finite and
  returns within 0.0123 of the settled image; without the check the output is
  non-finite.
* Disocclusion: for a 16x16 object moving 6 output pixels per frame, the strip it
  just uncovered (excluding two pixels at the edge) equals a no-history resolve
  within 0.005 for both a smooth background (colour clipping can also help) and
  a 2-pixel checker with a grey object (colour cannot help; only the distance
  test rejects).
* Cut/reset, abort and retry, re-enable, resize and render-scale change (the
  last enforced, no escape clause) give frames equal (1e-5) to a fresh view's; two views interleaved equal the solo
  run (1e-5); forgetting one view keeps the other; a disabled feature is
  bit-identical to a pipeline without it.
* Desktop (`crates/engine-desktop/tests/gpu_temporal.rs`, 65x43 target, authored
  `Camera3D.upscaler` through `Runtime`, `GpuRenderer`, the real motion prepass
  and meshes): a static triangle edge goes from 0 to 103 intermediate-valued
  pixels at `dlaa` with a steady frame-to-frame change of 11/255; a camera cut
  (`reset_temporal_history`) reproduces the very first frame byte for byte, and
  the first half-scale frame after 1:1 frames equals a fresh renderer's first
  half-scale frame byte for byte; at
  `performance` the status reports render size 33x22 and the covered area (linear
  light) is 555.0 against 559.1 analytic (1:1: 558.5); mode off is byte-identical
  to the plain frame and re-enable reproduces the first resolve; requested DLSS
  without the SDK renders the plain native frame with a reason, `auto_temporal`
  runs the native resolve and carries that reason; a resize leaves no old history;
  spatial AA yields (`spatial_aa_skipped`) and is restored exactly; a moving mesh
  leaves a trail of 0/255 outside the edge; an authored project with a runtime
  override reports `backend = native_temporal`, then the DLSS fallback.

## Limits and residual risks

* Transparent geometry has no motion vectors or depth; sky has no motion (a
  moving camera smears only as far as clipping allows). Custom vertex
  displacement is not in the motion prepass.
* Sub-render-pixel features are kept at reduced scale only because the colour
  clip is relaxed there; the price is no colour anti-ghosting for fast lighting
  changes at reduced scale (the 1:1 clip is unchanged). There is no sharpening
  and no history-length adaptation to luminance change.
* Camera motion over a striped grazing ground plane and a wall is covered by
  `camera_motion_keeps_history_on_a_grazing_ground_plane_and_leaves_no_ghost` in
  the desktop suite (history kept while moving, no red outside the wall's
  silhouette); the slope tolerance above was added for it. Measured (test lap,
  `--nocapture`, after the changes above): ground edge pixels aliased 0, moving
  1174, held 823; fast pan wall pixels 370, ghost pixels 0; half-scale vs 1:1
  reference squared error 11.16; covered area analytic 559.1, 1:1 558.5, half 555.0.
* Disocclusion ignores an object's own motion along the view axis.
* Fixed constants (clip gamma 1.25, distance tolerance 4 %, speed cap) were set
  from the tests above, not tuned on real content.
* No performance numbers were measured; cost was not profiled. No quality
  comparison with DLSS (not run in this environment) or any other upscaler is
  claimed. HDR display output, VR eyes (no temporal resolve for XR views) and
  Frame Generation interplay are out of scope.
