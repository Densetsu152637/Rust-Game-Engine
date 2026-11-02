# Fog: height fog with wind-advected noise

Status: implemented to ready-to-test on branch `feat/render-r9-fog`; GPU readback
tests written and run on Windows / RTX 5080 (Vulkan). Second of the stage 9
"atmosphere, fog, clouds" slices (`pending-features-plan.md`, dependency item
5), built on the merged [atmosphere](rendering-atmosphere.md). Clouds are the
next slice and can reuse the pieces listed under
[Parameters later slices can reuse](#parameters-later-slices-can-reuse).

## What exists

One built-in feature in `engine-render-pipeline` (`fog` module, shader
`wgsl/fog.wgsl`, shared helpers `wgsl/scatter.wgsl`), disabled by default:

| Feature | Stage | Does |
| --- | --- | --- |
| `engine.fog` | `OpaqueEffects` | Replaces the scene colour by `colour * T + source * (1 - T)` along each pixel's view ray, for opaque geometry (to its depth) and for sky pixels (to `sky_distance`). |

The host feeds it through a cloneable `FogControls` handle: per view,
`FogFrame { settings, camera_position, time }`. A view without an entry draws
nothing even when the feature is enabled, and a view with the feature disabled
ignores its entry, so **absent settings give byte-identical output to a pipeline
that never registered the feature** (checked by a readback test, as is
re-enabling restoring the fogged image exactly). `register_features(&mut
registry, &controls)` registers it; the desktop host puts it in the same
registry as the atmosphere (so the ordering below applies) and fills the handle
from `Camera3D.fog`.

## Model

A homogeneous-in-colour participating medium with extinction

```
sigma_t(p) = density * exp(-falloff * (y - base)) * m(p)
m(p)       = 1 + amount * (2 n(p - wind * t) - 1)       n in [0, 1], m in 1 -+ amount
```

`y` is the **absolute** world height (floating origin included), `n` is 3D
value noise (quintic interpolation) on a lattice of `noise_scale` metre cells,
and `t` the scene clock. Scattering is `albedo * sigma_t` (the rest of the
extinction is absorption), with a Cornette-Shanks phase of asymmetry `g` for the
sun and an isotropic term for the ambient light.

Single scattering along a view ray of length `D` collapses to a closed form,
because the source term does not vary along the ray (no shadowing of the sun
inside the fog, see limits):

```
tau    = integral_0^D sigma_t ds                          (optical depth)
T      = exp(-tau)
source = albedo * (sun_intensity * E_sun * P_CS(cos, g) + ambient_intensity * L_ambient)
L      = L_scene * T + source * (1 - T)
```

- `E_sun` is the irradiance of the view's **first directional light** in shading
  units (lux times lighting exposure), exactly what the atmosphere uses (`scatter_sun`
  in `scatter.wgsl`; no sun means no sun term). `P_CS` is the same Cornette-Shanks
  function as the atmosphere's Mie phase (`scatter_cornette_shanks`, shared by both
  shaders; CPU twin `atmosphere::mie_phase`). `L_ambient` is the view's ambient
  radiance (`Camera3D.lighting.ambient`, `engine_view.ambient`), scattered isotropically
  (`phase * 4 pi = 1`). The in-scatter is clamped to the atmosphere's
  `MAX_RADIANCE` (60 000) so `Rgba16Float` never receives infinity.
- **Pure height fog is exact.** For a ray from height `h0` above the base with
  vertical cosine `dy` and `x = falloff * dy * D`:
  `tau = density * (e^{-falloff h0} - e^{-falloff (h0 + dy D)}) / (falloff dy)`,
  written as `density * D * e^{-falloff h0} * (1 - x/2 + x^2/6 - x^3/24 + x^4/120)`
  for `|x| < 0.01` (the exponential difference would cancel there; zero falloff
  is Beer-Lambert). Exponents are clamped to 80 so `exp` stays finite in `f32`;
  beyond that the optical depth is astronomically large and `T` is exactly 0.
  `fog::height_optical_depth` is the same function in `f64` and is the oracle of the
  GPU density test.
- **Noise.** The first `noise_distance` metres of the ray are cut into
  `noise_steps` equal midpoint segments; each segment contributes its analytic
  height integral times `m(p_mid)`. The remainder out to `D` is the analytic
  height integral with `m = 1` (the noise has mean 1, so far fog keeps its mean
  density). With `noise_amount = 0` no noise is evaluated at all. Cost per pixel is
  at most 64 value-noise evaluations (8 hashes each); there is no per-pixel
  dithering, so coarse steps of a long `noise_distance` show as banding: keep
  `noise_distance / noise_steps` below about half a noise cell.
- **Determinism.** The lattice hash is an integer PCG-style hash of the cell,
  wrapped to a period of 1024 cells per axis, seeded by `noise_seed`
  (`fog::cell_value` is the bit-identical CPU twin). The result depends only on
  settings, absolute camera position and scene time.

### Wind and the scene clock

The pattern is advected by sampling the noise at `p - wind * t`. The host
(`fog::noise_origin`, in `f64`) folds the absolute camera position, the wind and the
time into one **wrapped noise-space camera origin** (cells, modulo the lattice
period) per frame; the shader only adds the small camera-relative ray offset. So
neither a large floating origin nor a long run time ever degrades precision, and
rebasing the origin cannot move the fog. Two exact symmetries are tested:

- advecting for `dt` seconds equals moving the camera by `-wind * dt` (a readback
  test renders both and compares the whole image);
- zero wind is static for any time, bit for bit.

`t` is the **scene clock**: `SceneRenderFrame::scene_time`, filled from the
runtime's accumulated tick `dt` (`Runtime::extract_scene_frame`). It never reads wall
time, so a paused or single-stepped runtime holds the fog still, and re-rendering
the same frame later gives the same image (tested end to end with a sleep in
between).

## Placement decisions

- **`OpaqueEffects`, after `engine.ssr` and `engine.atmosphere.aerial`.** The
  stage order is `engine.surface_material`, `engine.decals`, `engine.ssr`,
  `engine.atmosphere.aerial`, `engine.fog`. The fog is a full-screen ping-pong
  pass (`opaque_effect_io`) that reads colour and depth, like aerial perspective,
  and is constrained `.after("engine.ssr").after("engine.atmosphere.aerial")`.
  Fog last treats it as the medium nearest the camera: the thin atmosphere's tint
  is applied to the surface first and the surface-plus-tint is then seen through
  the fog. For two media in series along the same ray this is the physically
  correct composition order for the near medium (the far medium's in-scatter is
  attenuated by the near one), and because both operations are
  `scene * T + S` they remain well-behaved in either order; no evidence argued
  for the reverse, and the registration-order unit test pins this one. The
  constraint is ignored (as the framework does for unknown names) when the
  atmosphere is not registered. Register both in one registry (the desktop host
  does) so the constraint is applied across them.
- **SSR reflections are fogged as part of the reflecting surface.** SSR runs
  earlier, so the reflected radiance is already in the scene colour of the
  reflecting pixel and is attenuated by that pixel's view distance. The reflected
  ray's own, longer path is not integrated (same documented limit as aerial
  perspective). Decals are applied before SSR and are fogged with the surface.
- **Sky pixels (depth at the far plane)** are fogged over `sky_distance` metres
  along their view ray, `0` leaving them untouched. The default 10 km gives a
  horizon haze that matches fogged distant terrain (otherwise terrain fogged to
  the fog colour would meet an unfogged sky in a visible seam). Set it near the
  camera's far plane if the terrain fades out at the far plane; the sky is the
  fog colour at the horizon (ray crosses the full layer) and clears upward because
  the integral converges to `density e^{-falloff h0} / (falloff dy)`. With the
  atmosphere's sky enabled the fog multiplies the atmosphere's sky colour by
  `T` and adds the in-scatter, i.e. the fog sits between the camera and the sky.
- **Water and underwater fog: no double fogging.** `world.water.underwater`
  (`Transparent` stage) fogs *every* pixel by its distance with the water's own
  colour/density when the camera is below a water surface, and runs after
  `OpaqueEffects`. If the air fog also ran, the whole view would be fogged by
  both media. The desktop adapter therefore **does not set the fog for a view
  whose camera is under a water surface**, using exactly the test the underwater
  pass uses (`engine_world_render::water::camera_underwater` over `WaterRenderSet`),
  and only while `world.water.underwater` is enabled in that view (otherwise
  nothing else would fog the view). Above the surface the air fog applies to
  opaque geometry, including the lake bed seen through the water (the water
  pass refracts the already fogged opaque colour). The water *surface* itself is
  transparent geometry and is not fogged (see limits). Pinned by
  `renderer::fog::tests::a_submerged_camera_gets_no_air_fog` and the desktop
  readback `a_submerged_camera_is_not_fogged_twice`.
- **Large worlds.** The host sums the applied origin and the origin-relative
  camera in `f64` per axis (`fog::absolute_position`); height above the base and
  the wrapped noise origin are computed in `f64` before narrowing.
- **Ray directions and distance** share `scatter.wgsl` with the aerial pass: the
  near-plane point of the (jittered) inverse projection, euclidean distance for
  perspective views and view-axis depth for orthographic ones. Jittered temporal
  views therefore fog along the shifted ray (a readback test fails if the jitter
  is ignored).

## Authored settings

`Camera3D.fog` (absent or `null` = off; `{}` = defaults, pure height fog; see
[project-format.md](project-format.md)). Setting it turns `engine.fog` on for that
camera's view; `render_features["engine.fog"] = false` disables it and wins over
the automatic enable. It lives on the camera like the other per-view effects.

| Field | Default | Range | Meaning |
| --- | --- | --- | --- |
| `density` | 0.01 | 0..=10 /m | extinction at `base_height`; 0 disables the fog |
| `height_falloff` | 0.1 | 0..=1 /m | exponential decay with height; 0 is uniform fog |
| `base_height` | 0 | +-1e7 m | world height where the density equals `density` (below it the density grows) |
| `albedo` | `[0.8, 0.85, 0.9]` | 0..=1 per channel | fog colour under white light |
| `anisotropy` | 0.3 | -0.99..=0.99 | Cornette-Shanks asymmetry; positive: bright toward the sun |
| `sun_intensity` | 1 | 0..=1000 | multiplies the sun in-scatter |
| `ambient_intensity` | 1 | 0..=1000 | multiplies the ambient in-scatter |
| `sky_distance` | 10 000 | 0..=1e6 m | distance integrated for sky pixels; 0 leaves the sky |
| `noise_amount` | 0 | 0..=1 | noise strength `1 -+ amount`; 0 = no noise |
| `noise_scale` | 20 | 0.5..=1e5 m | noise cell size |
| `noise_distance` | 300 | 1..=1e5 m | distance marched with noise; beyond it plain height fog |
| `noise_steps` | 16 | 1..=64 | noise segments per pixel |
| `noise_seed` | 0 | `u32` | selects the noise pattern |
| `wind` | `[0, 0, 0]` | +-200 m/s per axis | advects the noise; 0 is static |

Non-finite or out-of-range values fail validation (`camera3d.fog.invalid`) before
any GPU work, in `Camera3D::validate`, again in `FogFrame::new` /
`FogControls::set` (a rejected set leaves the previous state) and in the desktop
adapter. The schema (`describe_component` for `Camera3D` over MCP) documents
every field. The wind is a pure authored velocity; scripts that want gusts
change `Camera3D.fog.wind` between ticks.

## Limits (honest list)

- **No shadowing inside the fog.** The sun in-scatter is unshadowed: no light
  shafts, no shadow-map sampling along the ray (a froxel / volumetric-shadow
  grid is the standard follow-up and was deliberately left out), no planet
  shadow, and the sun is not attenuated by the atmosphere or the fog layer
  above the sample. The source term is therefore constant along the ray, which
  is what makes the closed form exact.
- **Only the first directional light** scatters; point/spot/area lights do not
  light the fog. The ambient term is the flat view ambient, not the IBL.
- **Opaque and sky pixels only.** Transparent geometry (glass, particles, the
  water surface, the second SSR hop) is drawn after `OpaqueEffects` and is not
  fogged, as with aerial perspective; a distant window next to fogged terrain
  looks clear.
- **Reflections** (SSR) are fogged by the reflecting surface's distance, not by
  the reflected ray's longer path.
- Noise is a single value-noise octave on a periodic lattice (1024 cells): fine
  for drifting banks, repeats every `1024 * noise_scale` metres. No
  per-pixel dithering of the step offset (banding for coarse steps). Fog density is
  not shared with other systems (no volumes, no per-object density, no fog
  volumes); there is one fog per camera.
- Unbounded exponential density below `base_height`: a camera far below the base
  sees (correctly) near-opaque fog.
- The fog is a camera effect, so editor viewports and split screens can differ.
- `Rgba16Float` quantisation applies (GPU comparisons used 1 to 3 percent
  tolerance against the `f64` reference).

## Parameters later slices can reuse

- `scatter.wgsl` (Cornette-Shanks phase, pixel ray, view distance, sun lookup) is
  shared by the atmosphere and the fog, and is what a cloud pass should import.
- `fog::value_noise` / `fog.wgsl::fog_noise`, the wrapped `noise_origin` and the
  scene-clock plumbing (`SceneRenderFrame::scene_time`) are the wind-advected
  density source clouds need; only the height-layer shape and a march with
  self-shadowing differ.
- `FogFrame` carries absolute camera position and time, so a cloud layer at a
  world altitude needs no new floating-origin handling.

## Tests

CPU (`cargo test -p engine-render-pipeline --lib fog`, `-p engine-project --test
render_fog`, `-p engine-desktop --lib renderer::fog`):

- settings ranges and non-finite rejection, frames/controls independence and
  rejected-set safety, absolute position summed in `f64` and rebasing invariance,
  uniform layout, feature stage/order/defaults (after SSR and aerial perspective,
  before transparents), shader composition and naga validation;
- model: closed form against a 200 000-step numeric integral (up, down, level,
  below the base, uniform), continuity across the series switch, finite and
  non-negative in extreme geometry, noise determinism / range / mean / periodicity /
  seed, the noise origin (wrapped, rebasing invariant, wind equals a moved camera),
  modulation bounds and the noise distance, fogged-pixel blend and sun
  anisotropy;
- project: authored JSON to runtime to snapshot round trip, feature enabling and
  precedence, scene clock follows ticks, closed schema whose defaults equal the
  struct's, every range rejected; desktop adapter: position and clock mapping,
  disabled feature, invalid settings, submerged camera.

GPU (RTX 5080, ignored tests): `cargo test -p engine-render-pipeline --test
gpu_fog -- --include-ignored --test-threads=1`:

| Test | Asserts |
| --- | --- |
| `density_matches_the_analytic_height_transmittance` | wall transmittance equals the closed form within 1 % across densities, falloffs, looking up and down; more density gives lower transmittance; uniform fog is Beer-Lambert |
| `base_height_and_camera_altitude_move_the_fog_layer` | a higher camera sees thinner fog; raising the base restores it; an applied origin counts toward altitude |
| `in_scatter_is_brighter_toward_the_sun_and_follows_it` | in-scatter toward the sun is more than 5x that away from it, follows azimuth and elevation, mirror-symmetric, matches the reference, intensity scales it, no light means none |
| `ambient_and_albedo_tint_the_in_scatter` | ambient-only in-scatter is albedo-tinted and scales with `ambient_intensity` |
| `farther_surfaces_are_more_fogged_and_near_ones_are_untouched` | fog grows with wall distance, a wall 1.5 m away is unchanged within 1 %, all match the reference |
| `sky_pixels_are_fogged_only_up_to_the_sky_distance` | `sky_distance` 0 leaves the sky exactly black, otherwise sky pixels match the reference, denser toward the ground, longer distance fogs more, geometry hides the sky |
| `orthographic_views_fog_along_the_view_axis` | parallel rays, view-axis distance, uniform finite image |
| `noise_matches_the_cpu_march_and_varies_across_the_view` | GPU equals the CPU march (3 %), roughness 3x the smooth fog, plain height fog beyond the noise distance, other seed differs, a camera billions of metres from the origin with a long time still matches |
| `wind_advects_the_noise_with_the_scene_clock` | time changes the image, advecting equals moving the camera against the wind (< 2e-4), reverse wind differs, +x wind slides the pattern right and -x left, zero wind static for any time, same time same image |
| `disabled_or_unconfigured_fog_is_identical_to_a_pipeline_without_it` | feature off, no settings, zero density are bit-identical to a pipeline without it; re-enabling restores the fog |
| `resize_two_views_and_forget_view` | targets recreate on resize, two views keep their settings apart, `forget_view` drops the state |
| `jittered_views_fog_along_the_shifted_ray` | jittered rays and distances match the reference; an unshifted shader would miss at least 20 pixels |

`cargo test -p engine-desktop --test gpu_fog -- --include-ignored --test-threads=1`:
authored fog end to end (reference match, sky, rebasing, disable/enable
bit-exactness, resize), the scene clock driving the wind through real runtime
ticks, and the submerged camera getting no air fog.
