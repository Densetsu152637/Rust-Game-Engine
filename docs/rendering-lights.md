# Lights, clustered shading and shadows

The 3D pipeline (`engine-render-pipeline`) shades any number of directional
lights plus point, spot and rectangular area lights. Local lights are culled per
view on the CPU and assigned to a froxel grid by a compute pass, so each
fragment loops only over the lights that can reach it. Every light can cast
shadows into a paged shadow atlas: cascaded maps for directional lights, one
perspective view for spot lights and six cube faces for point and area lights,
filtered with PCF or PCSS.

## Authored components

Lights are ECS components on scene entities, validated by the scene schema,
documented by `component_schema` and edited with MCP `component_set` (or
natively as `engine_project::lights::*` components). Unknown fields are rejected.

| Component | Placement | Intensity unit | Other fields (default) |
| --- | --- | --- | --- |
| `DirectionalLight` | Travels along the entity's local -Z (world rotation; identity without `Transform`) | `lux` (3), 0..200000 | `shadows`: cascades |
| `PointLight` | Entity world position; requires `Transform` | `lumens` (800), 0..1e8 | `range` (10) |
| `SpotLight` | Position, cone along local -Z; requires `Transform` | `lumens` (800) | `range` (10), `inner_angle_radians` (0.4), `outer_angle_radians` (0.55, at most pi/2) |
| `AreaLight` | Rectangle centred on the entity in its local XY plane (width along X, height along Y), emitting along local -Z (both sides when `two_sided`); requires `Transform` | `lumens` (800) | `range` (10, above half the diagonal), `width` (1), `height` (1), `two_sided` (false) |

Every light also has `version` (1), `enabled` (true) and linear RGB `color`
(0..1, default white). Several light components may share an entity. Light sizes
are metres: the transform's scale does not resize area lights.

```json
{"id": "sun", "name": "Sun", "components": {
  "Transform": {"position": [0, 10, 0], "rotation": [-0.399593, 0.239756, 0, 0.884784]},
  "DirectionalLight": {"lux": 3, "shadows": {"enabled": true, "cascades": 2,
                                             "max_distance": 30}}}}
```

A rotation of `[-0.7071068, 0, 0, 0.7071068]` (-90 degrees about X) points a
light straight down. Natively, `LightInstance3D::directional_toward(id,
toward, light)` builds a directional light from a vector pointing at the light.

### Units, exposure and falloff

`Camera3D.lighting` keeps the view-wide values: `ambient` (0..1, default 0.08,
already exposed) and `exposure` (1e-8..1000, default 1). The renderer converts
photometric values to *shading units* by multiplying them by `exposure`:

| Light | Shading intensity |
| --- | --- |
| Directional | `lux x exposure` (illuminance on a surface facing it) |
| Point, spot | `lumens / (4 pi) x exposure` (candela); spot lights keep the point conversion, so narrowing the cone does not brighten its axis |
| Area | `lumens / (pi x width x height) x exposure` (luminance; halved for two-sided lights) |

The built-in material multiplies irradiance by the BRDF, so at the default
exposure 1 a 1 lux light on a white Lambertian surface gives `1/pi` before the
output stage clamps to 0..1. The default 3 lux directional light reproduces the
former camera light. Physically based scenes set `exposure = 1 / (1.2 x
2^EV100)` (`photometric::exposure_from_ev100`); direct sunlight (100000 lux) at
EV100 15 maps to about 2.5.

Local lights use a windowed inverse-square falloff,
`E = I x (1 - (d/range)^4)^2 / max(d^2, 0.0001) x cos`, which is exactly zero at
and beyond `range` and within 3% of pure inverse square for `d <= 0.35 range`
(measured below). Spot lights multiply it by
`smoothstep(cos outer, cos inner, cos angle)`: full intensity inside the inner
cone, zero outside the outer cone, C1-smooth in between.

### Area lights

Diffuse lighting from an area light is the exact Lambertian integral over the
rectangle: the polygon is clipped to the receiver's upper hemisphere and the
edge integrals are summed (Lambert's polygon formula), then multiplied by the
range window of the distance to the centre. Specular lighting uses a
representative-point approximation: the closest point of the rectangle to the
reflection ray drives the GGX lobe, scaled by the exact irradiance and an energy
normalisation `(alpha / alpha')^2` with `alpha' = alpha + r / (2 d)` (r the
radius of the disc of equal area). No LTC tables are used, so there are no table
assets or licences involved; glossy reflections of large area lights are an
approximation rather than a reference integral.

## Shadows

| Field | Local lights (`LightShadows`) | Directional (`DirectionalShadows`) |
| --- | --- | --- |
| `enabled` | false | false |
| `resolution` | 512 (per face or view), power of two 64..4096 | 2048 per cascade |
| `depth_bias`, `normal_bias` | 1, 1.5 shadow texels (0..16) | same |
| `filter` | `pcf` (`hard`, `pcf`, `pcss`) | same |
| `pcf_radius`, `pcss_max_radius` | 1 (0..4), 16 (1..32) texels | same |
| PCSS source size | `source_radius` 0.05 m (0.001..10) | `angular_diameter_radians` 0.0093 (sun) |
| `near` | 0.05 m projection near plane | - |
| Cascades | - | `cascades` 4 (1..4), `split_lambda` 0.75, `max_distance` 100 m, `cascade_blend` 0.1 (0..0.5), `caster_extension` 100 m |

- **Cascaded shadow maps.** Split depths follow the practical scheme
  `lambda x log + (1 - lambda) x uniform` between the camera near plane and
  `min(max_distance, camera far)`. Each cascade is an orthographic view around
  the bounding sphere of its camera frustum slice; the sphere depends only on
  the projection, and its centre is snapped to whole shadow texels in light
  space, so the shadow texel grid stays fixed in world space while the camera
  moves (shadow edges do not shimmer under translation; rotating the light
  still changes them). Casters up to `caster_extension` beyond the sphere
  toward the light are rendered. The last `cascade_blend` fraction of each
  cascade is blended into the next one, and the last cascade fades out.
  Surfaces beyond `max_distance` are unshadowed.
- **Spot lights** render one perspective view of the outer cone (widened by
  the filter margin). Cones wider than 160 degrees are shadowed only within 160
  degrees.
- **Point and area lights** render six 90-degree cube faces (a 6-face atlas
  layout, widened by the filter margin to keep filter taps on-face). Area light
  shadows are cast from the rectangle's centre: an approximation that is good
  for small emitters; PCSS with `source_radius` near the light's size softens
  them.
- **Biases** are in shadow texels at the receiver: the receiver is offset by
  `normal_bias` along its normal and compared `depth_bias` closer to the
  light (linear depth), so the same values work at every resolution and
  distance.
- **Filters.** `hard`: one bilinear hardware comparison. `pcf`: a
  `(2 pcf_radius + 1)^2` grid of bilinear comparisons one texel apart.
  `pcss`: a 16-tap blocker search (radius from the source size and the
  receiver depth, at most `pcss_max_radius`), the average blocker depth gives
  the penumbra (`source_radius x (receiver - blocker) / blocker` for local
  lights, `tan(angular_diameter / 2) x (receiver - blocker)` for directional
  ones), then a 32-tap Vogel-disk filter of that radius. Filtering is selected
  per light.

### Shadow atlas

All shadow views of a view live in one `Depth32Float` texture array whose
layers are atlas pages (`ShadowAtlasConfig`: `page_size` 4096, `max_pages` 4 by
default; `ScenePipeline::set_shadow_atlas`). Views are power-of-two squares
packed largest first along a Morton curve, so pages fill without gaps. Lights
are admitted by priority (directional lights first, then local lights nearest
to the camera); a light whose views do not fit the remaining texels, or would
exceed `MAX_SHADOWS` (256) views, renders without shadows and the status says
why. Pages are allocated on demand and kept (grow-only) per view.

## Clustering

`engine.lights.clusters` (Prepare stage, enabled by default) builds a 16 x 9 x
24 froxel grid per view: 16 x 9 screen tiles and 24 logarithmic depth slices
between the camera near plane and the farthest extent of any uploaded local
light (at most the camera far plane), using the jittered view-from-clip matrix
of `engine::view`. One compute invocation per froxel tests every local light's
range sphere against the froxel's view-space bounds, plus the outer cone of spot
lights (against the froxel's bounding sphere) and the emitting half-space of
one-sided area lights, and stores up to 256 light indices in ascending order.
Workgroups of 64 froxels load the lights in view space into shared memory in
batches of 64.
Longer lists are truncated and counted. Disabling the feature in a view
(`render_features: {"engine.lights.clusters": false}`) shades every uploaded
local light per fragment instead (the all-lights path).

Before upload the CPU culls local lights whose range sphere is outside the view
frustum or that are black, and keeps at most `MAX_LIGHTS` (4096) lights per view
(directional lights first, then the nearest local lights). Light counts are
those of the current frame; cluster statistics come from a non-blocking
readback, so they describe an earlier frame of the view. With the device feature `TIMESTAMP_QUERY` enabled (an
optional `DeviceFeatureRequest`), the readback includes the cluster pass's GPU
time.

## WGSL contract

`engine::view` (group 0) carries the light data every feature can read:

| Binding | Value |
| --- | --- |
| 0 | `engine_view: EngineView` (uniform) |
| 1 | `engine_lights: EngineLightList`: `counts` (x lights, y directional lights, which come first) and `lights: array<EngineLight>` |
| 2 | `engine_shadows: array<EngineShadow>` |
| 3 | `engine_shadow_atlas: texture_depth_2d_array` (a 1x1 placeholder in the Shadow stage) |
| 4 | `engine_clusters: EngineClusterGrid`: header and per-cluster light counts |
| 5 | `engine_cluster_lights: array<u32>`: cluster `c`'s indices start at `c x dims.w` |
| 6 | `engine_shadow_sampler: sampler_comparison` (linear, less-equal) |

`EngineLight` (80 bytes): `color`, `intensity` (shading units), `position`,
`range`, `direction` (directional: toward the light; spot/area: the direction
of travel), `kind` (0 directional, 1 point, 2 spot, 3 area), `tangent` (area
width axis), `shadow_index` (first `EngineShadow` or -1), `params`
(directional: cascade count and blend; spot: cos inner, cos outer; area: half
width, half height, two-sided).

`EngineShadow` (128 bytes): `clip_from_world`, `rect` (atlas uv offset, uv
size, layer), `bias` (depth bias, normal bias, world texel size at unit distance
or absolute, perspective flag), `sampling` (mode, PCF radius, PCSS source size,
PCSS max radius) and `range` (projection near/far, cascade split near/far in
camera view depth). Cascades of a light are consecutive; cube faces follow in
+X, -X, +Y, -Y, +Z, -Z order.

`EngineClusterGrid`: `dims` (16, 9, 24, 256), `depth` (near, far, slice scale
and bias: `slice = log(view depth) x scale + bias`), `tile` (clusters per render
pixel in x and y), `params` (x: 0 clustered or 1 all lights; y first local
light; z light count), then `counts`.

### Lighting in a material

`#import engine::material` brings in `engine::lighting` (and `engine::brdf`). A
material fills an `EngineSurface` (albedo, metallic, unit normal, perceptual
roughness) and calls

```wgsl
let lit = engine_direct_lighting(surface, input.world, v, input.position.xy, receive_shadows);
```

with the fragment's world position, view direction and `@builtin(position)`;
the built-in `lit.wgsl` is the reference. Lower-level functions for custom
shading models: `engine_light_incident(light, world, n, v)` (direction and
irradiance without shadows), `engine_light_visibility(light, world, n)`,
`engine_light_contribution(...)`, `engine_light_contribution_screen(...)` (with a
screen-space visibility), `engine_indirect_lighting(surface, v, frag)` (the ambient /
indirect term with the screen-lighting inputs), `engine_brdf(surface, v, l)`,
`engine_cascade_index(light, world)` (debug views), `engine_cluster_index(frag,
depth)`, `engine_view_depth(world)`, `engine_range_window` and
`engine_distance_falloff`. Every function is valid in non-uniform control flow
(no derivatives). The sprite implementation of `engine::material` provides
`engine::brdf` and unlit stand-ins for `engine_direct_lighting`,
`engine_light_contribution`, `engine_light_incident`, `engine_light_visibility`,
`engine_cascade_index` and `engine_view_depth`, so a material using those
compiles for both targets.

## Native API

`engine_render_pipeline` exports `Light`/`LightShape`/`LightList` (shading
units), `ShadowSettings`/`ShadowFilter`/`CascadeSettings`, `ShadowAtlasConfig`,
`cascade_splits`, the `shadows` module (cascade, spot and cube views, atlas
packing), `photometric` conversions and the GPU layouts `GpuLight` and
`GpuShadow`. `ScenePipeline::render_view` takes the `LightList` in
`SceneFrame.lights`; `ViewReport.lighting` (`LightingReport`) describes the
frame. The desktop host converts authored components with the camera's
exposure.

## Status

`LightingStatus` (`Runtime::lighting_status()`, `RuntimeSnapshot.lighting`,
`GpuRenderer::lighting_status()`) reports for the active camera view: `state`
(`not_rendered` before any 3D frame, with the `authored` count of enabled
lights), `submitted`, `culled`, `over_budget`, `directional`, `local`,
`cluster_mode` (`clustered` or `all_lights`), `clustered`, `cluster_dims`,
`cluster_overflowed` and `cluster_longest_list` (latest readback),
`cluster_gpu_ms` (with timestamp queries), `shadows_enabled`,
`shadowed_lights`, `shadow_views`, `shadow_dropped`, atlas page size, pages,
used and capacity texels, and at most four `overflow` reasons (for example
`2 shadowed lights did not fit the shadow atlas (1 pages of 1024^2 texels)`).

## Migration from `Camera3D.lighting`

`Camera3D.lighting` no longer holds a light: `direction`, `color`,
`intensity` and the fixed-volume `shadows` (`center`, `extent`, `depth`,
`bias`) were removed and are rejected as unknown fields. Author a
`DirectionalLight` entity instead (`lux` replaces `intensity` at exposure 1;
cascades replace the fixed volume). `ambient` stays; `exposure` is new.

## Measured results

`cargo test -p engine-render-pipeline --test gpu_lights -- --ignored
--test-threads=1 --nocapture` on an RTX 5080 (Vulkan), 2026-10-25:

| Check | Result |
| --- | --- |
| Point light (1 m above the floor, range 6 m) | Max error 0.09% vs the windowed inverse-square formula; within 3.0% of pure inverse square for d <= 0.35 range; monotonic; exactly 0 at all 33 pixels beyond range |
| Spot light (inner 0.3, outer 0.5 rad) | Factor 1 inside the inner cone (40 px), 0 outside the outer cone (58 px), max deviation from smoothstep 0.0008 between (30 px) |
| Area light (2 m x 1 m at 1.5 m) | Max error 0.094% vs numerical integration over 256 floor samples; elongated along the rectangle's width |
| 1024 point lights, 640x360 | Clustered and all-lights images identical (max difference 0); 992 clustered, 32 culled; longest list 98, 0 clusters over capacity; cluster pass 0.076 ms GPU (timestamp queries); frame 1.07 ms clustered vs 1.90 ms all lights (wall time with a GPU wait, debug build) |
| 4096 point lights, 640x360 | Cluster pass 0.300 ms; frame 2.61 ms clustered vs 6.37 ms all lights; longest list 377, so 20 clusters were truncated (reported) |
| Cascades (4, 60 m) | A shadow at view depth 3.0 m used cascade 0 and one at 42.2 m cascade 3 (splits 4.14, 9.35, 20.35); both fully shadowed, lit neighbours 1.0 |
| Cascade stability | A 0.0625 m camera move (0.70 of a 0.0895 m shadow texel) changed 0 of 1090 shadow-edge pixels; with snapping disabled the same move changed 1365 pixels (max 0.16) |
| Spot and point shadows | Visibility 0 at the projected occluder centre (spot; point -Y and +X faces), 1.0 beside it |
| PCSS (spot 6 m up, 0.3 m source) | 10-90% penumbra 0.031, 0.086, 0.188 m for blockers 0.5, 1, 2 m above the receiver; PCF stays 0.031 m |
| Atlas overflow | 4 point lights x 6 x 256^2 in one 1024^2 page: 2 shadowed, 2 dropped with a reason |

## Limits

- No image-based lighting; the ambient term is a constant unless the ray-traced GI,
  AO or reflection features write the screen-lighting inputs
  ([rendering-raytracing.md](rendering-raytracing.md#frame-integration)).
- Area light specular is a representative-point approximation; area shadows
  come from the centre. Disc and sphere lights are not implemented.
- Every shadow view draws every opaque caster (no per-view caster culling) and
  shadow maps are re-rendered every frame (no caching of static lights).
- The cluster pass tests every local light against every froxel, in batches
  of 64 lights shared per workgroup (0.08 ms for 1024 and 0.30 ms for 4096
  lights on an RTX 5080); a froxel holds at most 256 lights.
- Each view (camera, editor viewport, XR eye) owns its own atlas pages.
- Transparent meshes do not cast shadows; custom vertex displacement is not
  replayed in shadow passes.
- Render features in the Shadow stage see a placeholder atlas and do not get
  the atlas rectangles of the built-in shadow views.
