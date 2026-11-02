# Clouds: ray-marched volumetric cloud layer

Status: implemented to ready-to-test on branch `feat/render-r9-clouds`; GPU
readback tests written and run on Windows / RTX 5080 (Vulkan). Third and last of
the stage 9 "atmosphere, fog, clouds" slices (`pending-features-plan.md`,
dependency item 5), built on the merged [atmosphere](rendering-atmosphere.md)
and [fog](rendering-fog.md): it reuses their shared scattering helpers
(`wgsl/scatter.wgsl`), the fog's value noise and wrapped noise origin, and the
scene clock `SceneRenderFrame::scene_time`.

## What exists

One built-in feature in `engine-render-pipeline` (`clouds` module, shader
`wgsl/clouds.wgsl`, CPU reference `clouds/model.rs`), disabled by default:

| Feature | Stage | Does |
| --- | --- | --- |
| `engine.clouds` | `OpaqueEffects` | Replaces the scene colour by `colour * T' + inscatter * f` where `T` and the in-scatter come from marching the view ray through a cloud layer, limited by the pixel's scene depth (or the sky). |

The host feeds it through a cloneable `CloudControls` handle: per view,
`CloudFrame { settings, camera_position, time }`. A view without an entry draws
nothing even when the feature is enabled, and a view with the feature disabled
ignores its entry, so **absent settings give byte-identical output to a pipeline
that never registered the feature** (readback-tested, as is re-enabling restoring
the clouded image exactly, zero density and zero coverage). `register_features`
registers it; the desktop host registers it in the same registry as the
atmosphere and the fog (so the ordering below applies) and fills the handle from
`Camera3D.clouds`.

## Model

The layer is **flat**: it lies between the world altitudes `base_altitude` and
`top_altitude` (floating origin included). The atmosphere model has a planet
radius, but the cloud pass is a separate feature with its own controls and no
access to it; curvature is a documented limit (see below). With `y` the height
above the base, `h = y / thickness` in `0..1`, and `c` the point in wrapped
noise space, the extinction is

```
sigma_t = density * remap(mix(1, fbm(c), amount), coverage) * profile(h)
remap(n, k) = clamp((n - (1 - k)) / max(k, 1e-3), 0, 1)
profile(h)  = smoothstep(0, 0.1, h) * (1 - smoothstep(0.6, 1, h))
fbm(c)      = sum_i 0.5^i noise(2^i c, seed + i) / sum_i 0.5^i      i < octaves <= 4
```

`noise` is the fog's quintic value noise on a periodic 1024-cell lattice; the
cell size is `noise_scale` metres. **Coverage** raises the threshold: at `k` the
lowest `1 - k` of the noise values hold no cloud and the rest is stretched to
`0..1`, so coverage 0 is empty, 1 passes the noise through, and more coverage
never removes cloud from a point. The **height profile** rounds the base
(rises over the lowest 10 %) and thins toward the top (falls over the highest
40 %); it integrates to `0.75`, so with `amount = 0` (a constant noise field) a
ray crossing the layer has the closed form

```
T = exp(-density * thickness * 0.75 / |dy|)        (slab_transmittance)
```

used as the oracle of the density GPU test.

**Primary march.** The ray is clipped to the layer (`layer_interval`) and to
`min(scene distance, max_distance)`; the segment is split into `primary_steps`
equal midpoint samples (at most 96). Per sample with `sigma > 0` and step length
`dt`, the step transmittance is `exp(-sigma dt)` and

```
inscatter += T * source * (1 - exp(-sigma dt))        T *= exp(-sigma dt)
```

(the source is constant over a step; this is the energy-conserving form used by
the fog). **Early exit:** the loop stops when `T < 0.01` (so a nearly opaque
pixel can be up to one step above the exact transmittance). Horizontal rays only
see the layer from inside it; rays that leave it upward or point away from it
cost nothing (no samples are taken).

**Lighting.** The sun is the view's first directional light (`scatter_sun`, the
atmosphere's lookup) with irradiance `E` (lux times exposure), unattenuated by
the atmosphere. Each sample marches `light_steps` midpoint samples (at most 12)
over `light_distance` metres toward the sun and accumulates the optical depth
`tau` (Beer-Lambert self-shadowing: the base of a cloud sees the sun through the
cloud above it and is darker than its top). The source per unit extinction is

```
source = albedo * ( E * sun_intensity * visibility * sum_{k<3} a^k P(mu, c^k) exp(-b^k tau)
                  + ambient * ambient_intensity * (0.5 + 0.5 h) )
a = b = c = 1/2
P(mu, s) = 0.7 CS(mu, g s) + 0.3 CS(mu, -0.3 s)            CS = Cornette-Shanks (scatter.wgsl)
visibility = smoothstep(0, 0.05, sun.y)
```

`P` is a **dual-lobe** phase: a forward lobe with the authored `anisotropy` `g`
(the silver lining brightens clouds viewed toward the sun) and a fixed weak
backward lobe. The `k` sum is the Wrenninge **multiple-scattering
approximation**: each octave attenuates the optical depth, widens the phase and
lowers the amplitude by half; `multiple_scattering = false` keeps only `k = 0`
(single scattering). It is an artistic approximation, not a radiative-transfer
solution. The ambient term is the flat view ambient, scaled linearly from half
at the base to full at the top. A sun at or below the layer's horizon plane
(`sun.y <= 0`) contributes nothing (a flat world has no planet to occult it, so
this is the planet-shadow stand-in); an absent light means no sun term.

**Horizon fade.** `fade_distance > 0` scales the cloud opacity by
`f = exp(-entry / fade_distance)` at the distance where the ray enters the layer,
so distant clouds dissolve into the (already aerial-perspective-tinted) sky:
`T' = 1 - f (1 - T)`, in-scatter times `f`.

### Wind and the scene clock

`CloudFrame::time` is `SceneRenderFrame::scene_time`, the runtime's accumulated
tick time, and the noise is sampled at `p - wind * t`. The host folds the
camera, wind and time into one **wrapped noise origin** in `f64`
(`fog::noise_origin`, shared with the fog), so the shader only sees small
coordinates: floating-origin rebases, huge absolute positions and long runs keep
their precision. The wind therefore equals a camera moved against it, to the
bit (tested), zero wind is static at any time, and the same time gives the same
image.

## Placement decisions

- **`OpaqueEffects`, after `engine.ssr` and `engine.atmosphere.aerial` and before
  `engine.fog`.** The stage order is `engine.surface_material`, `engine.decals`,
  `engine.ssr`, `engine.atmosphere.aerial`, `engine.clouds`, `engine.fog`. The
  cloud pass is a full-screen ping-pong pass (`opaque_effect_io`) that reads
  colour and depth; its constraints are `.after(SSR).after(aerial)`, and the fog
  adds `.after(CLOUDS)`.
- **Clouds over the sky.** The atmosphere sky pass (Sky stage) has already added
  the sky radiance, planet and sun disk to far-plane pixels, so the cloud
  composite `scene * T' + inscatter` attenuates and covers the sky behind it. A
  sun disk behind thick cloud is dimmed by `T`. Because the layer is
  planet-flat and the sky model is spherical they agree near the zenith and
  diverge only toward the horizon, where the fade distance hides the join.
- **Clouds and geometry (depth).** Geometry nearer than the layer entry hides
  the clouds exactly (pixel unchanged); geometry inside the layer limits the
  march so clouds partially cover it; geometry beyond the layer (or at the far
  plane) is fully covered by them. The pixel's scene colour is the aerial-
  perspective-tinted geometry, so a distant mountain behind a cloud is tinted
  first and the cloud then covers that.
- **Aerial perspective and fog over the clouds.** The atmosphere's aerial
  perspective is not applied again to the cloud itself (the cloud pass has no
  atmosphere model); the `fade_distance` is its stand-in. The fog, in contrast,
  is applied *after* the clouds, so the fog medium nearest the camera sits in
  front of them: the fogged result is `fog(clouds(sky or geometry))`. The fog
  distance of a sky pixel is its `sky_distance`, and of geometry its depth
  distance, i.e. the fog is not integrated over the cloud distance. Both orders
  are well-formed `scene * T + S` compositions; this one matches the nearest-
  medium-last rule the fog slice adopted, and a readback test pins that the order
  is observable.
- **Water and underwater.** As for the fog, the desktop adapter sets no clouds
  for a view whose camera is under a water surface (the water pass owns the
  view), using the same `camera_underwater` test. Pinned by the desktop test
  `a_submerged_camera_gets_no_clouds`.
- **Large worlds.** Camera position is summed in `f64` (`absolute_position`);
  the camera height above the layer base and the wrapped noise origin are
  computed in `f64` before narrowing, and altitudes are absolute.
- **Ray directions and distance** share `scatter.wgsl` with the atmosphere and
  fog: the near-plane point of the (jittered) inverse projection, euclidean
  distance for perspective views. Jittered temporal views march the shifted ray
  (a readback test fails if the jitter is ignored). Orthographic views use the
  view axis as direction from the camera position, which is an approximation.

## Authored settings

`Camera3D.clouds` (absent or `null` = off; `{}` = the defaults), enabling
`engine.clouds` unless `render_features["engine.clouds"]` is `false` (an
explicit override wins, as for the fog and the atmosphere). It is a camera
setting like the fog and the atmosphere because the effect depends on the
camera position, the camera's ambient and the per-camera feature overrides, and
there is one sky per view. Unknown fields are rejected and every range is
validated when the camera loads, before any GPU work.

| Field | Default | Range | Meaning |
| --- | --- | --- | --- |
| `base_altitude`, `top_altitude` | 1500, 3500 | within +-1e7 m, top above base by 1..1e5 m | World altitudes of the layer |
| `density` | 0.004 | 0..1 /m | Extinction at full density (0 disables) |
| `coverage` | 0.5 | 0..1 | Cloud fraction (0 disables) |
| `albedo` | [1, 1, 1] | 0..1 | Scattering albedo |
| `anisotropy` | 0.6 | -0.99..0.99 | Forward lobe of the phase |
| `sun_intensity`, `ambient_intensity` | 1, 1 | 0..1000 | Light multipliers |
| `multiple_scattering` | true | bool | Wrenninge approximation |
| `noise_amount` | 1 | 0..1 | `mix(1, fbm, amount)`; 0 is a uniform layer |
| `noise_scale` | 1500 | 1..1e6 m | Base noise cell |
| `noise_octaves` | 3 | 1..4 | Fractal octaves |
| `noise_seed` | 0 | u32 | Noise pattern |
| `wind` | [0, 0, 0] | +-200 m/s per axis | Noise advection with the scene clock |
| `max_distance` | 40000 | 1..1e6 m | Farthest march distance |
| `fade_distance` | 0 | 0..1e7 m | Horizon fade (0 = off) |
| `primary_steps` | 48 | 1..96 | View-ray samples |
| `light_steps` | 6 | 1..12 | Sun-ray samples per view sample |
| `light_distance` | 800 | 1..1e5 m | Length of the sun march |

## Performance bounds

Per pixel at most `primary_steps` (96) view samples, each costing up to
`light_steps` (12) sun samples, each sample up to 4 octaves of value noise (8
hashes per octave): worst case 96 x 13 x 4 noise evaluations. The defaults are
48 x 7 x 3. Pixels whose ray misses the layer, is hidden by nearer geometry or
sees only empty (coverage-clipped) space cost one or a few noise evaluations;
the march exits early at `T < 0.01`. **The pass runs at full render
resolution.** A reduced-resolution cloud buffer with a bilateral upsample was
left out: it needs its own target management and depth-aware upsampling, and
the bounded step counts are the supported lever. Temporal reprojection
(accumulation across frames with the pass jitter) is a follow-up; the march has
no per-pixel step offset, so coarse `primary_steps` show banding on grazing
rays rather than noise.

## Limits (honest list)

- **Flat layer.** No planet curvature: the layer is a slab of constant
  altitude, so the horizon never curves away and very long grazing rays clamp at
  `max_distance`. The atmosphere's planet radius is not consulted.
- **No cloud shadows** on the ground or on geometry, no light shafts, no
  atmosphere transmittance on the sun (clouds do not redden at sunset), and the
  sun is not occulted by the planet (a sun below the horizon plane simply lights
  nothing).
- **Only the first directional light** scatters; the ambient is the flat view
  ambient, not the IBL.
- **Opaque and sky pixels only.** Transparent geometry (glass, particles, the
  water surface) is drawn after `OpaqueEffects` and is neither covered nor
  hidden by the clouds; a particle in front of a cloud looks right, one behind
  it does not.
- **Aerial perspective on the cloud itself** is approximated by
  `fade_distance` only; fog is not integrated over the cloud distance.
- **Noise** is the fog's periodic 1024-cell value noise (fractal): repeats every
  `1024 * noise_scale` metres, no curl or erosion noise, no weather map.
- One cloud layer per camera; no weather system, no volumes, no per-object
  density.
- The cutoff makes nearly opaque pixels up to one step brighter than the exact
  transmittance (below 1 % of the background).
- `Rgba16Float` quantisation applies (GPU comparisons used 1.5 to 4 percent
  tolerance against the `f64` reference).

## Parameters later slices can reuse

- `clouds::{extinction, march, composite, source, light_depth}` are the CPU
  twins of the shader; `fbm`, `profile`, `remap` and `slab_transmittance` are
  the building blocks.
- Cloud shadows can sample `extinction` along the sun direction (the same light
  march) into a shadow map; temporal accumulation can reuse the view jitter and
  `CloudUniform`; a planet-aware layer needs the atmosphere's planet radius
  passed through the controls.

## Tests

CPU (`cargo test -p engine-render-pipeline --lib clouds`, `-p engine-project
--test render_clouds`, `-p engine-desktop --lib renderer::clouds`):

- settings ranges and non-finite rejection (base at or above top, thickness
  bounds, coverage, density, steps, octaves, wind, distances), frames/controls
  independence and rejected-set safety, uniform layout, feature
  stage/order/defaults (after SSR and aerial, before fog and transparents, either
  registration order), shader composition and naga validation;
- model: the profile integrates to 0.75, coverage remap and extinction stay in
  range and are monotone in coverage, fractal noise determinism / range /
  periodicity / seed, layer intervals, the constant slab matches the closed form
  (densities and slants), coverage lowers the transmittance, geometry limits and
  hides the march, the step cap and early exit, forward scattering, sun
  intensity and horizon, self-shadowing (base darker than top, light depth falls
  with height), multiple scattering, albedo, wind equals a moved camera, zero wind
  static, the horizon fade;
- project: authored JSON to runtime to snapshot round trip, enabling and
  precedence, every range rejected, scene clock follows ticks, closed schema whose
  defaults equal the struct's; desktop adapter: position and clock mapping, field
  mapping, disabled feature, invalid settings.

GPU (RTX 5080, ignored tests): `cargo test -p engine-render-pipeline --test
gpu_clouds -- --include-ignored --test-threads=1`:

| Test | Asserts |
| --- | --- |
| `a_constant_slab_matches_the_closed_form_transmittance` | wall transmittance equals `exp(-density * 150 / dy)` within 1.5 % across densities and pitches; more density, lower transmittance; camera inside and above the layer; rays away from the layer unchanged |
| `coverage_and_noise_match_the_cpu_march_and_lower_the_transmittance` | GPU equals the CPU march (4 %) at several coverages; more coverage lowers mean transmittance; zero coverage is the plain wall; the noise is rough, seeds and octaves differ; a camera billions of metres away with a long time and a high layer through the origin still match |
| `sunlight_scatters_forward_and_follows_the_sun` | toward the sun is 1.5x brighter than away (silver lining), matches the reference, follows azimuth (mirror-symmetric) and elevation, a sun below the horizon plane lights nothing, intensity scales it, no light means none, ambient-only is albedo-tinted |
| `self_shadowing_makes_the_base_darker_than_the_top` | a camera below the layer (sees the base) is darker than one above (sees the sunlit top), both match the reference; a short light march misses the shadow; single scattering is darker than multiple |
| `geometry_occludes_the_clouds_and_clouds_cover_distant_geometry` | a wall nearer than the layer is unchanged, one beyond it is covered and matches, one inside is partial; clouds on the empty sky; rays that miss the layer stay black |
| `clouds_composite_over_sky_and_aerial_perspective_and_under_fog` | with the atmosphere sky and aerial perspective in the scene, the clouds are composited over exactly that colour, the fog is applied over the cloud result, and the opposite order would be a visibly different image |
| `wind_advects_the_clouds_with_the_scene_clock` | time moves the pattern, advecting equals moving the camera against the wind (< 5e-4), reverse wind differs, +x wind slides the pattern right and -x left, zero wind static for any time, same time same image |
| `disabled_or_unconfigured_clouds_are_identical_to_a_pipeline_without_them` | feature off, no settings, zero density and zero coverage are bit-identical to a pipeline without it; re-enabling restores the clouds |
| `resize_two_views_and_forget_view` | targets recreate on resize, two views keep their settings apart, `forget_view` drops the state |
| `jittered_views_march_the_shifted_ray` | jittered rays match the reference; an unshifted shader would miss at least 20 pixels |

`cargo test -p engine-desktop --test gpu_clouds -- --include-ignored
--test-threads=1`: authored clouds end to end (reference match over a wall and
the empty sky, a near wall hides them, rebasing, disable/enable bit-exactness,
resize), the scene clock driving the wind through real runtime ticks, and the
submerged camera getting no clouds.
