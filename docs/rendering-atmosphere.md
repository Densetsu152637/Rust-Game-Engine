# Atmosphere: sky and aerial perspective

Status: implemented and GPU-tested on Windows / RTX 5080 (Vulkan). First of the
stage 9 "atmosphere, fog, clouds" slices (`pending-features-plan.md`, dependency
item 5). It covers the **sky, the planet below the horizon, the sun disk and
aerial perspective on opaque meshes**. Fog and clouds are separate, later
slices; this document marks the parameters they can build on.

## What exists

Two built-in features in `engine-render-pipeline` (`atmosphere` module, shaders
`wgsl/atmosphere*.wgsl`), both disabled by default:

| Feature | Stage | Does |
| --- | --- | --- |
| `engine.atmosphere` | `Sky` | Adds single-scattered sky radiance, the Lambertian planet and the sun disk to pixels whose depth is still at the far plane. |
| `engine.atmosphere.aerial` | `OpaqueEffects` | Replaces opaque scene colour by `colour * transmittance + in-scatter` along the pixel's view distance. |

The host feeds them through a cloneable `AtmosphereControls` handle: per view,
`AtmosphereFrame { settings, camera_altitude }`. A view without an entry draws
nothing even if its features are enabled, and a view with the features disabled
ignores its entry, so **absent settings give byte-identical output to a pipeline
that never registered the features** (checked by a readback test). The desktop
host registers the features at pipeline creation and fills the handle from
`Camera3D.atmosphere`; `register_features(&mut registry, &controls)` does the
same for any other host.

The sun is the view's **first directional light** (`engine_lights[0]` when
`counts.y > 0`, the same convention as `engine_screen_shadow`). Direction,
colour and intensity come straight from the light list in shading units
(`lux * lighting.exposure`), so the sky is in the same unit system as lit
surfaces and needs no separate calibration. With no directional light both
passes do nothing (the sky stays at the background, geometry is unchanged).

## Model

Single scattering in a spherical shell, evaluated per pixel by numerical
integration (no precomputed tables):

- Medium: Rayleigh scattering (per-channel `beta_R`, density `exp(-h/H_R)`), Mie
  scattering and absorption (`beta_M`, `exp(-h/H_M)`, Cornette-Shanks phase with
  anisotropy `g`) and ozone absorption (a tent-shaped layer: density
  `max(0, 1 - |h - centre| / width)`). Defaults are Earth-like (Hillaire 2020:
  Rayleigh `[5.802, 13.558, 33.1]e-6`, `H_R = 8 km`, Mie `3.996e-6` scattering and
  `0.444e-6` absorption, `H_M = 1.2 km`, `g = 0.8`, ozone `[0.65, 1.881, 0.085]e-6`
  at 25 km, planet radius 6360 km, top 6460 km).
- View ray: `L = integral T(0..t) * sigma_s(t) * P(cos) * E_sun * T_sun(t) dt`
  with `P = beta_R rho_R P_R + beta_M rho_M P_M`. `T_sun` is the transmittance
  from the sample to the top of the atmosphere along the sun direction, 0 if the
  planet blocks that ray (the terminator is a hard threshold; the sun disk's
  angular size is not integrated into the shadow).
- 32 view steps (sky) or 16 (aerial), 10 sun steps. Step edges are spaced
  quadratically so the dense low atmosphere gets the small steps; inside a step
  the source is constant and extinction exponential, which removes the usual
  dark/bright banding of plain Riemann sums.
- Ground: a ray that reaches the planet adds `T * albedo / pi * E_sun * N.L *
  T_sun` (Lambertian, sun-lit side only; no ground-reflected light re-enters the
  atmosphere).
- Sun disk: radiance `E_sun / solid_angle * T` inside the angular radius, only
  for rays that do not hit the planet. The soft 20 % limb's coverage is divided
  by `SUN_DISK_NORMALISATION` = 0.812 (the fraction of the full disk's solid
  angle it covers, single-sourced into the shader), so the disk
  carries the full attenuated sun irradiance (a unit test integrates it). Clamped
  to 60 000 so `Rgba16Float` never receives infinity.

### Precision

Planetary radii are far beyond what an `f32` can subtract: the shader never
computes `|P| - R`. With `u = |P|^2 - R^2 = a(2R + a) + 2(R + a) d.y t + t^2`
(`a` the camera altitude, `d.y` the ray's vertical cosine) the altitude is
`u / (|P| + R)`. Ray/sphere intersections use `(H - h)(2R + H + h)` for the
shell. `atmosphere/model.rs` is the same algorithm in `f64`; it is the oracle of
the unit tests and the GPU comparisons (a test checks the formulation against the
direct radius difference).

### Placement decisions

- **Sky in the Sky stage, additive.** The pass rasterizes at depth 1.0 with a
  `LessEqual` test, so only pixels still at the far plane are touched, and adds
  into the HDR colour. The background is not attenuated, so a custom Sky feature
  such as stars shows through only if it is registered **earlier** (features of one
  stage run in registration order unless constrained) and itself writes or adds;
  a non-additive sky registered later overwrites the atmosphere. Depth is not
  written.
- **Aerial perspective in `OpaqueEffects`.** It needs a per-channel transmittance
  multiply of existing colour, which fixed-function blending cannot do without
  dual-source blending, so it is a ping-pong full-screen pass
  (`opaque_effect_io`) that reads colour and depth. Far-plane pixels pass through
  untouched. It therefore runs after `Sky` and before `Transparent`.
- **Aerial perspective after decals and SSR.** `engine.atmosphere.aerial` is
  constrained `.after("engine.ssr")` (and `engine.decals` is `.before("engine.ssr")`),
  so the `OpaqueEffects` order is `engine.surface_material`, `engine.decals`,
  `engine.ssr`, `engine.atmosphere.aerial`. Decals therefore change what SSR
  reflects, and the combined image, reflections included, is then attenuated and
  tinted by the view distance of each reflecting surface pixel. The reflected
  ray's own path is not integrated: the reflection is fogged as part of the
  reflecting surface, not by its own (longer) travelled distance. Pinned by the
  registration-order unit test and by `aerial_perspective_fogs_the_reflection_after_ssr`
  in `engine-render-pipeline/tests/gpu_ssr.rs` (transmittance of the reflected
  radiance below one and bluer light attenuated more, with SSR and aerial both on).
- **Distance** is the euclidean view-space distance of the depth sample
  (orthographic views: the view-axis depth), times `aerial_perspective_scale`,
  clipped to the atmosphere exit and to the planet.
  The planet clip starts to apply as soon as the scaled path reaches the surface:
  a downward ray (vertical cosine `d.y < 0`) from altitude `a` is cut at roughly
  `a / |d.y|` metres, so at low altitude a geometry distance times
  `aerial_perspective_scale` beyond that gets no extra tint (a camera 3 m up looking
  45 degrees down stops accumulating after about 4 m, however large the scale).
  Rays at or above the horizon are only clipped by the atmosphere exit.
- **Large worlds.** The shader only needs the camera's **absolute altitude** (a
  horizontal move does not change a locally spherical sky). The host computes it
  as `applied_origin.y + camera.y` in `f64` (`absolute_altitude`) so floating
  origin rebasing never changes the sky; altitudes below the surface clamp to the
  surface. World `y = 0` is sea level / planet surface, 1 world unit is 1 metre.
- **Ray directions** come from the near-plane point of the (jittered) inverse
  projection, which stays exact for any far plane; orthographic views look
  along view `-Z`.

## Authored settings

`Camera3D.atmosphere` (absent or `null` = off; see `project-format.md`). Setting
it turns both features on for that camera's view; `render_features`
`engine.atmosphere` / `engine.atmosphere.aerial` = `false` disable a pass
individually and win over the automatic enable. It lives on the camera, like the
other per-view effects, so editor viewports and split screens can differ.

| Field | Default | Range | Meaning |
| --- | --- | --- | --- |
| `planet_radius` | 6 360 000 | 1e3..=1e8 m | surface at world `y = 0` |
| `atmosphere_radius` | 6 460 000 | 100 m..=1e7 m above the planet | top of the shell |
| `rayleigh_scattering` | `[5.802e-6, 13.558e-6, 33.1e-6]` | 0..=0.01 /m per channel | |
| `rayleigh_scale_height` | 8000 | 1..=1e6 m | |
| `mie_scattering`, `mie_absorption` | 3.996e-6, 0.444e-6 | 0..=0.01 /m | |
| `mie_scale_height` | 1200 | 1..=1e6 m | |
| `mie_anisotropy` | 0.8 | -0.99..=0.99 | forward peak around the sun |
| `ozone_absorption` | `[0.65e-6, 1.881e-6, 0.085e-6]` | 0..=0.01 /m | |
| `ozone_center_height`, `ozone_width` | 25 000, 15 000 | 0..=1e7, 1..=1e7 m | |
| `ground_albedo` | `[0.3; 3]` | 0..=1 | planet seen below the horizon |
| `sun_angular_diameter` | 0.0093 | 0..=0.5 rad | 0 hides the disk |
| `intensity` | 1 | 0..=1000 | multiplies the sun irradiance; 0 skips both passes |
| `aerial_perspective_scale` | 1 | 0..=1000 | multiplies geometry distance; 0 disables aerial perspective |

Non-finite or out-of-range values fail validation (`camera3d.atmosphere.invalid`)
before any GPU work, in `Camera3D::validate`, again in the desktop adapter and in
`AtmosphereControls::set`; a rejected set leaves the previous state. The schema
(`describe_component` for `Camera3D` over MCP) documents every field.

## Aerial perspective and other surfaces

- Opaque built-in meshes (and anything else written to depth in `Opaque`) get
  aerial perspective. **Transparent geometry does not**: `Transparent` runs after
  `OpaqueEffects`, so a distant window or particle is not tinted. Fog/volume work
  that applies to blended surfaces needs a view-bound atmosphere sampled by the
  material; that is future work.
- Direct sunlight on surfaces is **not** reddened by atmospheric extinction (the
  light list is unchanged), so a sunset sky is red while sun-lit meshes stay the
  light's colour; authors set the directional light colour themselves. Likewise
  the sky does not feed ambient or reflections.
- Pixels showing the sky receive no aerial perspective; opaque geometry that is
  exactly at the far plane is treated as sky.

### IBL environment: out of scope

The atmosphere does not generate an environment map. Image-based lighting keeps
using the camera's `lighting.indirect` asset or ambient (`rendering-indirect.md`).
Capturing the sky into a probe or diffuse SH/specular cubemap would be a
separate slice (it needs a cubemap render of this pass plus the prefiltering that
the indirect set already has); the pass is written against `engine::view`
so a cube-face view could drive it later.

## Limits (honest list)

- **Single scattering only.** No multiple-scattering LUT: the sky is darker than
  a full model (measured here: a clear-sky zenith at 45 degrees sun elevation is
  about 0.11 of the radiance of a white diffuse wall facing the sun), and twilight
  and the shadowed side of the planet have no fill light; the ground below the
  horizon is black when the sun is below it.
- No precomputed transmittance/sky-view/aerial-perspective LUTs: cost is per
  pixel `32 * 10` density evaluations for sky and `16 * 10` for geometry. No
  performance numbers have been taken; LUTs are the planned optimisation.
- The sun is the first directional light only; moon, extra suns and local lights
  are not scattered. No cloud shadows, fog, volumetric light shafts or planet
  shadow penumbra.
- Planet curvature is assumed (a flat game world is dressed with a spherical sky);
  geometry beyond the planet horizon is clamped to the horizon path length.
- The camera altitude is per view; a camera moving far horizontally does not move
  around the planet.
- `Rgba16Float` quantisation applies (GPU comparisons used 4 to 6 percent
  tolerance against the `f64` reference; observed differences were below 1 percent
  except at the sun disk).

## Parameters later slices can reuse

- `AtmosphereFrame` already carries altitude and the full medium; fog can use the
  same `exp(-h/H)` density machinery and `atm_extinction`/`atm_march`.
- Wind and cloud layers would need time and a world-space density source; the
  per-view `time` already exists in `engine::view`.
- `aerial_perspective_scale` is the stylisation knob a height-fog slice can share.

## Tests

CPU (`cargo test -p engine-render-pipeline --lib atmosphere`, `-p engine-project
--test render_atmosphere`, `-p engine-desktop --lib renderer::atmosphere`):

- settings ranges and non-finite rejection, controls independence and
  rejected-set safety, uniform layout, feature stage/order/defaults, shader
  composition and naga validation;
- model: height formulation vs direct radius, phase normalisation, vertical
  optical depth vs the exponential integral, transmittance monotonic in length
  and blue removed first, atmosphere/planet intersections, planet blocking the
  sun, noon zenith bluer than the horizon, low sun red, Mie forward peak, sun
  disk energy, albedo and density scaling, aerial growth and convergence to the
  sky colour;
- project: authored JSON to runtime to snapshot round trip, feature enabling
  and precedence, closed schema; desktop adapter: altitude sums the applied
  origin in `f64`, disabled/absent clears the view, invalid settings fail early.

GPU (ignored, real adapter, `-- --ignored --test-threads=1`):

- `engine-render-pipeline/tests/gpu_atmosphere.rs`: eight sky pixels within 4 %
  of the reference; feature off, no settings and no sun all identical to a
  pipeline without the features; zenith bluer than the horizon; sunset redder
  toward the sun than away and than noon; brightest pixel is the sun disk
  (200 times the sky, bounded) and the sky beside it matches the Mie peak, no disk
  at diameter 0; Rayleigh x3 and intensity x4 scale as predicted; ground albedo
  affects only below the horizon; 30 km altitude and orbit match the reference;
  an opaque wall at 300 m, 3 km and 30 km matches `colour*T + L` and fades toward
  the sky colour, aerial off or scale 0 leaves it untouched, scale 50 tints it
  further; geometry hides the sky; two views of different size and sun, resize,
  and `forget_view` releasing the settings; orthographic views are finite.
- `engine-desktop/tests/gpu_atmosphere.rs`: authored project JSON (camera,
  directional light, far wall mesh) through the runtime and `GpuRenderer`: sky and
  wall against the reference, sRGB output bluish, floating-origin equivalence
  (camera height vs applied origin give identical pixels), a 1e9 m origin stays
  finite, disabling via `render_features` restores the plain image and
  re-enabling restores the atmosphere exactly, the preview status lists both
  features. The `Camera2D` logical size fixes the 3D viewport, so true size
  changes are covered by the pipeline-level resize test.
