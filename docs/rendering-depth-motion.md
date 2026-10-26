# Depth of field and motion blur feature slice

`engine-render-post-effects` supplies two executable HDR `PostProcess` features:
`effects.dof` and `effects.motion_blur`. They are disabled by default. The host
registers `DepthOfFieldFeature::new(controls.clone())` and
`MotionBlurFeature::new(controls.clone())` with `ScenePipeline`, sets each
view's `FeatureOverrides` entry to `true`, and updates its `PostEffects`
control handle with `set_dof` and `set_motion_blur`. `None` removes a view's
settings. A feature with no settings, zero radius, or zero shutter does not
request a post destination. The host adapter for authored project components
and editor controls is an integration task; this crate does not change those
schemas.

Both features read the current HDR colour at the output size and call
`post_process_io()` for a distinct output-size destination. They run after
the built-in `engine.post` if it is enabled, before tonemapping. The descriptor
orders depth of field before motion blur when both run. Each feature works per
view; there is no history texture. Motion blur declares `FeatureNeeds::motion`
so the pipeline allocates and runs the native motion prepass.

## Depth of field

The depth texture is `Depth32Float` at render size, with WebGPU non-reversed
depth in 0..1. The shader maps each output pixel centre to its render pixel,
then multiplies clip coordinates by the inverse of the view's unjittered
`clip_from_view` matrix. Its focus measure is the absolute view-space Z
distance. This supports finite perspective and orthographic projections.
Projections with a nonfinite inverse, an infinite far clip plane, or a depth
mapping whose view-space Z distance fails to increase from near to far are
rejected before requesting a post destination.

The circle radius is `min(abs(distance - focus_distance) / focus_range, 1) *
max_radius_pixels`. A 16-tap, two-ring gather blurs pixels with radius at least
0.75 output pixels. Samples whose view-space Z differs from the centre by
more than `edge_tolerance` are rejected, so a foreground silhouette does not
mix its colour into a background centre or vice versa. The centre contributes
unconditionally. The result is a bounded screen-space approximation, not a
physical lens model: foreground silhouettes remain relatively hard, no
separate occluded colour layer exists, and highlights do not produce lens
bokeh shapes.

`focus_distance` and `focus_range` must be finite and positive.
`max_radius_pixels` must be finite within 0..24. `edge_tolerance` must be
finite and nonnegative. Radius zero leaves HDR colour untouched.

## Motion blur

The native prepass writes current-to-previous motion in **render pixels**:
positive X is right and positive Y is down. The shader maps each output pixel
centre to the render-size motion texture and multiplies the vector by
`output_size / render_size` independently for X and Y. It then applies
`shutter_scale` and clamps the path length to `max_radius_pixels`. Nine
symmetric taps, including the centre, gather along that path. Depth separation
beyond `edge_tolerance` rejects samples across silhouettes. Zero motion and
`ViewData::reset_history` return the source pixel exactly. A camera cut must
set `reset_history`; otherwise previous-frame camera motion remains eligible.

`shutter_scale` is finite within 0..2, `max_radius_pixels` is finite within
0..24, and `edge_tolerance` is finite and nonnegative. Zero shutter or radius
skips the pass. Motion from custom vertex displacement and transparent geometry
is outside the native prepass contract; those pixels use whatever the prepass
provides, typically zero. This blur is spatial and uses no temporal history.

## Validation and limits

The crate's tests cover parameter rejection, per-view independence, and
GPU readbacks from an actual pipeline frame with depth and motion producers.
The ignored GPU suite requires a native adapter and is run with
`cargo test -p engine-render-post-effects --test gpu_effects -- --ignored
--test-threads=1`. Per-view resize rebuilds no effect-owned textures; each
frame uses its current target dimensions and projection. The two fullscreen
pipelines are device-owned, and only one short-lived uniform buffer is made
for an active pass. Quality and cost at high resolution have not been profiled.
