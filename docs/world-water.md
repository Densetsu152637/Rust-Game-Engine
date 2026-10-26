# Ocean, lake and river water

Surface water is a pure model crate plus a runtime service, a GPU feature and MCP tools:

| Part | Where | What |
| --- | --- | --- |
| Model | [`engine-water`](../crates/engine-water/README.md) | Gerstner sets, the FFT ocean, rivers, displacement inversion, optics, underwater fog, shoreline attenuation, buoyancy impulses |
| Scene integration | `engine_project::world::water` (`crates/engine-project/src/world/water/`) | `engine.WaterBody`, `engine.Buoyancy`, the water clock, terrain ground, CPU fallback meshes, the GPU render set, native queries |
| GPU rendering | [`engine-world-render`](../crates/engine-world-render/README.md) | `world.water` and `world.water.underwater` |
| Authoring | `crates/engine-mcp/src/tools/water.rs` | `water_sample`, `water_spectrum`; bodies are edited with `component_set` |

This is surface (height-field) water. Particle fluids and their rigid coupling are the
physics domain's [`engine.FluidVolume`](physics-fluids.md); the two are independent.

## Frames and units

A water body's local frame comes from its entity `Transform`: local `y = 0` is the mean
surface, `+Y` is up, horizontal positions are local `(x, z)`. Keep water transforms upright
(rotation about Y only, unit scale) so surface heights and buoyancy stay vertical. Metres,
seconds; gravity `g = 9.81 m/s^2`; densities are in the units of `engine.PhysicsBody3D`
material densities (water defaults to 1, so a body of density 0.5 floats half submerged).

## Waves

### Gerstner sets (`engine_water::gerstner`)

Up to 16 waves `{direction_degrees, wavelength (0.05..=5000 m), amplitude (<= wavelength /
14, the Stokes limit H/L = 1/7), steepness (0..=1), phase}`. With `k = 2 pi / L`,
`w = sqrt(g k tanh(k h))` (deep water `sqrt(g k)`; `h` = `waves.depth`, 0 = deep) and
`theta = k (d . x0) - w t + phi`, the rest point `x0` moves to

```text
x = x0 + sum q_i A_i d_i cos(theta_i),   y = sum A_i sin(theta_i)
```

with `q_i A_i = c s_i / k_i` and `c = min(1, 0.9 / sum s_i)`. Hence
`L = sum q_i k_i A_i <= 0.9` bounds the Lipschitz constant of the horizontal displacement:
its Jacobian determinant is at least `(1 - L)^2 = 0.01`, so the surface never loops or
folds (measured minimum 0.30 for a 4-wave set at the bound). The set reports the applied
`steepness_scale`; the runtime adds a status note when it is below 1. Normals come from the
exact tangents `dP/dx0 x dP/dz0`; orbital velocity is `dP/dt`.

### FFT ocean (`engine_water::spectrum`, `engine_water::ocean`)

A Tessendorf patch of `resolution` (16..512, power of two) samples over `patch_length` metres,
seeded (`seed`, SplitMix64 + Box-Muller, platform independent) and periodic in X and Z.

- **Spectra.** Phillips: `P(k) = a (alpha / 2 pi) k^-4 exp(-1/(k L)^2) (k_hat . w_hat)^2
  exp(-k^2 l^2)`, `alpha = 0.0081`, `L = V^2 / g`; continuum variance `a alpha L^2 / 4`.
  JONSWAP: `S(w) = alpha g^2 w^-5 exp(-5/4 (w_p/w)^4) gamma^r` with
  `alpha = 0.076 (V^2/(F g))^0.22`, `w_p = 22 (g^2/(V F))^(1/3)`, `sigma = 0.07 / 0.09`,
  spread `(2/pi) cos^2` over the downwind half plane, `P(k) = S(w) (dw/dk) D(theta) / k`.
- **Amplitudes.** `h0(k) = (xi_r + i xi_i) / 2 sqrt(P(k)) dk` (`dk = 2 pi / L`), Nyquist and
  zero modes empty; `h(k, t) = h0(k) e^(-i w t) + conj(h0(-k)) e^(i w t)` (Hermitian, travels
  along `+k`). Expected variance `sum P(k) dk^2`. `loop_period > 0` quantizes `w` to
  multiples of `2 pi / loop_period` so the surface repeats.
- **Frame.** Four complex inverse FFTs (in-house radix-2, `engine_water::fft`), each packing
  two real fields: height, choppy displacement `lambda IFFT(i k_hat h)`, slopes `IFFT(i k h)`
  and the displacement Jacobian `lambda IFFT(-k k^T/|k| h)`. `choppiness` is `lambda`
  (0..=2). Where the Jacobian determinant drops to 0 the surface folds (crest foam).
- **Statistics.** `OceanSpectrum::stats`: variance, significant height `4 sqrt(variance)`,
  continuum variance, peak wavelength/period (Phillips peak `k = sqrt(2/3) / L`), mean-square
  slope and a 16-bin energy histogram over log wavelength.

### Height at a world position

Displaced surfaces are sampled by inverting the horizontal displacement:
`engine_water::invert_horizontal` solves `x0 + D(x0) = p` with a bounded fixed-point
iteration on the Newton map `x0 <- x0 - J^-1 r` (plain `x0 <- p - D(x0)` when a Newton step
does not reduce the residual or `J` is near singular). It stops at `|r| <= 1e-4 m`
(`INVERSE_TOLERANCE`) or after 16 iterations (`INVERSE_MAX_ITERATIONS`) and returns the
best iterate; for a contraction with constant `L` the rest-position error is at most
`|r| / (1 - L)`. Measured: 4000 queries on a Gerstner set at the 0.9 bound all converge,
maximum residual 9.9993e-5 m, mean 2.4 iterations, at most 3; 2500 queries on a choppy
(`lambda = 1`) 64^2 Phillips patch all converge (maximum 9.98e-5 m). In folded FFT regions the
inverse is not unique and the best iterate is returned.

## Water bodies

`WaterSurface` (engine-water) is the queryable surface: `shape` (ocean: unbounded, lake:
`|x| <= half_extent.x, |z| <= half_extent.y`, river), `waves` (flat, Gerstner, an FFT frame),
`optics`, `shoreline` and the wave `time`. `sample(p, ground)` returns height, normal,
velocity (river flow plus Gerstner orbital velocity), crest foam, the Jacobian determinant,
the inversion residual and the shoreline attenuation; `underwater(point, ground)` returns
depth below the surface, fog colour and density and the per-channel sunlight transmittance
`exp(-absorption depth)`.

### Rivers (`engine_water::river`)

2..=256 centreline points (local; `y` is the surface height there), one width/depth or one
per point, `flow_speed` at the source and `subdivisions` (1..=64) per Catmull-Rom span. The
discharge is fixed by the first section, `Q = flow_speed width_0 depth_0`; the mean speed is
`Q / (width depth)` at every section and the cross profile is `1.5 u_mean (1 - (2n/width)^2)`
along the 3D tangent. Integrating the profile over 37 sections of a river whose width
(3..9 m) and depth (0.6..2 m) vary gives `Q` within 0.1 %. `locate(p)` returns arc length,
signed lateral offset, height, width, depth, flow and whether `p` is between the banks;
`ribbon(across)` is the surface mesh.

### Shoreline attenuation and foam

With a ground height `g` under a point (water-local), `depth = 0 - g` (rivers: centreline
height - g) gives `amplitude = min + (1 - min) smoothstep(0, fade_depth, depth)` (applied to
every wave amplitude and the horizontal displacement) and shore foam
`1 - smoothstep(0, foam_depth, depth)`. Without ground (`NoGround`) water is open.
`GroundHeight` is implemented for closures `Fn(f32, f32) -> Option<f32>`. Crest foam is
`clamp((foam_threshold - det) / foam_threshold, 0, 1)` from the horizontal Jacobian
determinant.

## `engine.WaterBody`

Requires `Transform`; editable with `component_set` (schema-validated, unknown fields
rejected; scene validation also runs every numeric check, so bad edits are refused).

| Field | Default | Meaning |
| --- | --- | --- |
| `kind` | `ocean` | `ocean`, `lake` or `river` |
| `half_extent` | `[50, 50]` | lake bounds / drawn ocean area (0.5..=100000 m) |
| `waves.model` | `gerstner` | `none`, `gerstner` or `fft` |
| `waves.gerstner` | 3 waves (18 m, 9 m, 4 m) | up to 16 `GerstnerWave`s |
| `waves.fft` | 64^2, 128 m, Phillips, 10 m/s, choppiness 1, seed 1 | `OceanSettings` (see above; `fetch`, `peak_enhancement` for JONSWAP, `small_wave_cutoff`, `depth`, `loop_period`) |
| `waves.depth` | 0 | Gerstner dispersion depth (0 = deep) |
| `waves.time_scale` | 1 | wave seconds per simulated second (0..=10) |
| `river` | 2-point 20 m river | `points`, `widths`, `depths`, `flow_speed`, `subdivisions` (`kind: river`) |
| `optics` | blue-green | `shallow_color`, `deep_color`, `absorption` (per m, RGB), `fog_color`, `fog_density`, `foam_color`, `foam_threshold`, `foam_intensity`, `roughness`, `refraction` |
| `shoreline` | enabled, fade 4 m, foam 0.75 m, min 0, `use_terrain` | attenuation over `engine.Terrain` heights |
| `density` | 1 | fluid density (body-density units) |
| `render_mode` | `auto` | `auto`, `cpu`, `gpu` (see below) |
| `grid_resolution` | 64 | drawn grid cells per side (8..=256) |
| `visible` | true | drawing only; queries and buoyancy are unaffected |
| `cast_shadows` | false | opt-in opaque depth-only shadows from the displaced surface |

### Runtime (`world::water` hooks)

- `pre_simulation` (`step_water`), after terrain and PCG: advances `WaterClock`
  (`time += dt`), rebuilds bodies whose component changed (`WaterStatus.revision`), evaluates
  the FFT frame at `time * time_scale`, snapshots terrain heightfields as the shoreline
  ground, then applies buoyancy (below).
- `post_transforms` (`update_water_render`): render paths, CPU meshes, `WaterRenderSet`,
  and `UnderwaterView` (underwater state of the active `Camera3D`).

At most 64 water bodies (the first 64 by authored id; the rest are skipped with a reason in
`water::failure`, logged once, and the tick continues); a body with a singular transform or an
invalid wave setup is skipped the same way. At most 1024 buoyant bodies per tick. Generated
surface meshes carry `WaterMesh { owner }`, authored id `water:<owner id>/surface` and `Parent`
= the water entity; a body without an authored id registered in the entity lookup gets no CPU
mesh (a note in `WaterStatus.notes`) because its parent link would dangle.

### Clock precision

`WaterClock.time` is f64 and stays f64 until the waves reduce it to a phase: each Gerstner
wave's phase is `(phase - omega t) mod 2 pi` computed in f64 (`GerstnerSet::at_time`), and the
FFT frame reduces `t` modulo `loop_period` when the spectrum loops (`OceanSpectrum::frame_f64`;
mode phases of a non-looping spectrum are `omega t` mod 2 pi in f64, limited by the f32 `omega`
of each mode, about `omega t * 6e-8` rad: 4e-3 rad at 10 h). Tested after ten hours: the
surface equals the surface at `t mod period` within 1e-4 m for a Gerstner wave and for a
looping FFT ocean.

### Origin shifts

Wave phases are body-local, so the surface needs nothing from `WorldOrigin`: a rebase moves the
water root and everything standing on it by the same delta and the waves continue unchanged.
Buoyancy reads body positions and the surface in the same local frame. Tested: with and
without a rebase of (1024, 0, -512) a floating box ends at the same absolute position (5 mm)
and the surface height and normal at the same absolute points agree (1e-3).

### Render path rule (no double draw)

As for terrain: a body draws through the GPU feature when `render_mode` is `gpu`, or `auto`
while the `WaterGpuRenderer` marker resource exists (`engine_world_render::install` inserts
it). Otherwise it has one CPU surface mesh entity: `MeshRenderer` + `GeometryOverride` (the
grid over `half_extent` displaced exactly like the model, vertex colours shallow/deep by
shoreline attenuation plus crest and shore foam, per-vertex normals; rivers: the ribbon) +
a transparent `MeshMaterial` (tint alpha 0.75, the optics roughness, authored `cast_shadows`). A
GPU-path body has no mesh entity and a CPU-path body is not in `WaterRenderSet`. A `gpu` body
without the renderer draws nothing and its status says so. The CPU fallback has no
refraction, absorption or underwater fog (`UnderwaterView` still reports the camera state for
game code).

### Native API

`water_at(world, point)` / `water_at_filtered(world, point, Some(id))` (world-space
`WaterQuery`: body, surface height, normal, velocity, depth, underwater fog, crest and shore
foam, amplitude, residual; the highest covering surface wins), `underwater_at`,
`surface(world, entity)` (the current `WaterSurface` for custom queries), `status`,
`buoyancy_status`, `sample_scene` (headless sampling used by MCP), `WaterClock`,
`UnderwaterView`, `WaterRenderSet`.

## `engine.Buoyancy`

On an entity with a dynamic `engine.PhysicsBody3D`; forces go through the physics impulse API
(`simulation::physics::apply_impulse` / `apply_angular_impulse`) in `pre_simulation`, before
the step.

| Field | Default | Meaning |
| --- | --- | --- |
| `points` | `[]` | up to 64 body-local sample points; empty: a 2 x 2 x 2 lattice of cell centres over the collider bounds (box, ball, capsule, cylinder) |
| `volume` | 0 | displaced volume when submerged (m^3); 0: the collider's volume |
| `point_height` | 0 | vertical slab per point; 0: the lattice cell height (authored points: half the collider height, else 0.5 m) |
| `linear_drag`, `angular_drag`, `flow_drag` | 1, 1, 1 | per second (0..=100) |
| `water` | `""` | only float in this body (authored id); empty: any |

Each point stands for `volume / N` over a vertical slab `point_height`; its submerged fraction
is `clamp((surface - y)/height + 1/2, 0, 1)` (exact for an upright box with lattice points).
Per tick: buoyancy `-rho V_i f_i g dt` at the point (with its torque about the body origin);
linear drag `m a (u - v)`, `a = linear_drag f_mean dt`, toward the submerged-weighted water
velocity `u`, plus horizontal `flow_drag`; both clamped so their sum is at most 1 (never
overshooting `u`, stable for any `dt`); angular drag per point on `w x r` with
`angular_drag dt <= 1/4`. The body velocity for drag is the mean velocity of the last tick
(position difference): the velocity read before the step carries the previous substeps'
gravity and would bias drag (it biased the floating height by ~5 mm).

Measured (`crates/engine-project/tests/water.rs`, 0.02 s ticks):

- a 1 m box of density 0.5 dropped from 1 m into calm water settles at centre height
  -0.0011 m, submerged fraction 0.501 (tolerance 0.03), buoyant force 4.915 N
  (weight 4.905 N), moving 3.9 mm in the last 0.5 s, upright;
- a density-3 box sinks (y = -13.9 m after 3 s, fully submerged, buoyancy 9.81 N);
- a floating box in a river (8 m wide, 2 m deep, 1 m/s mean) drifts to x = 7.99 m in 6 s at
  1.494 m/s, the local flow 1.494 m/s (tolerance 5 %), staying on the centreline;
- a camera 3 m under a lake reports depth 3.0000, the authored fog colour and density.

## GPU rendering

`engine_world_render::enable(app)` (or `install` + `register`) adds both features with the
terrain ones; `engine_world_render::water::register` alone returns a
`WaterRenderStatusHandle` (views, bodies, draw calls, texture uploads, underwater views,
last underwater body, notes).

- **`world.water`** (`Transparent`, before `engine.transparent`): copies HDR colour and depth,
  then draws each body once (shared unit grid, rivers their ribbon). The vertex shader mirrors
  `GerstnerSet::evaluate` (up to 16 waves in a uniform) or samples the uploaded FFT frame
  (`Rgba32Float` displacement + determinant and normal textures, periodic bilinear
  `textureLoad`, re-uploaded when the frame time changes) scaled by the per-vertex shoreline
  attenuation (an `Rg32Float` grid computed on the CPU from the terrain ground). The fragment
  shader refracts the copied scene colour (offset by the surface slope, rejected when the
  offset sample is in front of the water), absorbs it with `exp(-absorption thickness)` using
  the thickness from the copied depth, adds in-scattered shallow-to-deep colour, a Schlick
  Fresnel (F0 = 0.02) reflection of the ambient sky plus the first directional light's
  highlight, and foam (vertex crest/shore foam and scene depth within `foam_depth` below the
  surface). It writes depth, draws both faces (from below: the refracted world above).
- **`world.water.underwater`** (`Transparent`, after `engine.transparent` and `world.water`):
  when the view camera is under a surface (CPU `WaterSurface::underwater`, no shoreline ground)
  it re-copies colour and depth and fogs every pixel by its view distance `d`:
  `mix(fog_color, scene exp(-absorption d), exp(-fog_density d))`. It runs in the
  Transparent stage instead of PostProcess because it needs render-size depth and the
  pre-upscale HDR colour (temporal upscalers then see fogged frames).
- **`world.water.shadow`** (`Shadow`, after `engine.shadows`): when `cast_shadows` is true,
  draws the same displaced grid or river ribbon into every allocated shared shadow tile.
  Its wave time, ocean frame and shoreline attenuation match the colour surface. The atlas
  stores only depth, so enabling this treats the water as an opaque shadow blocker; it does
  not represent coloured transmission or partial absorption. The default is off for
  transparent water.

Verified on this machine (RTX 5080, Vulkan,
`cargo test -p engine-world-render --test gpu_water -- --ignored --test-threads=1`, 160 x 120):

- a Gerstner lake covers 59.6 % of the view, rows past the far shore stay background, one
  draw call; 1.3 s of wave time changes 7558 pixels;
- over a ground ramp, shallow water (ground 1.5 m down) is 2.22x the luminance of deep water
  (7.5 m), deep water is blue-shifted (`[48, 143, 169]` over grey `[203, 203, 203]`) and the
  shoreline (0.4 m) foams brightest (`[226, 236, 241]`);
- a camera 2 m under the surface sees a white wall 15 m away as `[36, 103, 119]` against the
  analytic `[36.4, 102.7, 119.5]` (tolerance 3 levels), open water as the fog colour
  `[39, 97, 111]` (expected `[38.7, 97.2, 111.3]`); above the water the wall is white and the
  status reports no underwater view;
- an FFT ocean (64^2) uploads one frame per new frame time (2 uploads for 2 frames), covers
  64.3 % and changes 5298 pixels over 2 s.

The opt-in shadow path has a separate ignored GPU readback for a native receiver with
`cast_shadows` disabled, enabled, and at a later wave time; it has not yet been run on the
integrated revision.

### Limits

- Water writes no motion vectors and is not in the normals prepass.
- Refraction and absorption see only what was drawn before `world.water` (opaque geometry);
  one water body does not refract another.
- The sky reflection is the view's ambient colour (no environment map); RT reflections are
  not consumed.
- The GPU underwater check ignores shoreline ground (the CPU `UnderwaterView` includes it); rivers get no per-vertex shoreline grid on the GPU (scene-depth foam still applies).
- The ocean grid covers `half_extent` around the entity; it does not follow the camera.
- Buoyancy samples world-vertical slabs: a strongly rotated body's submerged volume is
  approximate; inertia is not read from physics (angular drag uses the point masses).
- Water bodies assume upright, unit-scale transforms.

## MCP tools

| Tool | Access | Effect |
| --- | --- | --- |
| `water_sample` | read | runs a scene headless for 1..=3600 ticks (no game code) and reports every body and the water at up to 256 world points: covering body, surface height, depth, underwater fog, normal, flow, crest/shore foam, amplitude |
| `water_spectrum` | read | FFT statistics (variance, Hs, continuum variance, peak, slope, energy histogram, frame height stats) or Gerstner dispersion, steepness scale and Lipschitz bound, from an authored body, inline `ocean` settings or inline `gerstner` waves |

Bodies and buoyancy are authored with `component_set` (`engine.WaterBody`,
`engine.Buoyancy`).

## Verification

- `cargo test -p engine-water`: FFT round trip (error 5.0e-7 at N = 4096) and DFT agreement;
  dispersion `w^2 = g k tanh(k h)` and a measured crest speed of 4.9980 m/s against 4.9981
  m/s; steepness bound; inversion error; Parseval (relative 1e-4) at four times; Phillips and
  JONSWAP discrete variance within 1.1 % of the continuum and seed-averaged realizations
  within 1.4 %; FFT vs direct synthesis 9.5e-7 m; tile periodicity 2.8e-6 m; bit-identical
  determinism; mode phases advancing at `w` (deep and 4 m depth, five modes, to 5 digits);
  loop period; river continuity; shoreline, underwater, buoyancy balance and drag clamping.
- `cargo test -p engine-project --test water`: the runtime cases above, CPU mesh vertices
  equal to the model, GPU-renderer takeover, terrain shoreline, FFT tiling, validation.
- `cargo test -p engine-world-render --test water_extraction` and the ignored `gpu_water`.
- `cargo test -p engine-mcp --test water`: `component_set` editability and rejection,
  `water_sample` (a raft floating at the lake surface, confirmed by `physics_query`),
  `water_spectrum`.
