# Hardware ray tracing core (`engine-render-rt`)

`crates/engine-render-rt` is the renderer's ray-tracing foundation: capability probing,
the single audited experimental-feature token, a GPU self-test that gates the hardware
tier, BLAS/TLAS management, reusable WGSL ray-query and shading libraries, frame-ready
lighting kernels on G-buffer textures (RT shadows with an optional soft sun, ambient
occlusion, reflections, diffuse GI), an SVGF-style spatio-temporal denoiser for their
signals, a reference path tracer, and validated settings with per-effect status. The
kernels take and produce standalone textures; the desktop renderer runs them as the
`engine.rt.*` frame features with raster fallbacks ([Frame integration](#frame-integration)).

Ordinary builds need no SDK: wgpu `=30.0.1` with its `vulkan`, `dx12`, `metal` and `wgsl`
features, all already in `Cargo.lock`.

## Tiers and status

| Tier | When | What runs |
| --- | --- | --- |
| `Hardware` | A Vulkan adapter exposes `Features::EXPERIMENTAL_RAY_QUERY`, the device was created with the merged `RtFeatureRequest`, and the GPU self-test passed | BLAS/TLAS builds, WGSL `ray_query`, the kernels |
| `Unavailable` | Anything else | Nothing; callers use their raster fallbacks |

`RayTracingStatus { tier, cause, reason, adapter }` (serde, snake_case) is the honest
report. `reason` is at most 512 bytes, cut on a char boundary. `cause` is one of:

| `cause` | Example `reason` (measured on the RTX 5080 where shown) |
| --- | --- |
| `no_adapter` | "Microsoft Basic Render Driver is a software adapter; hardware ray tracing needs a GPU" |
| `missing_adapter_feature` | "Vulkan adapter X does not expose EXPERIMENTAL_RAY_QUERY (needs VK_KHR_ray_query and VK_KHR_acceleration_structure)" |
| `unsupported_backend` | "DX12 adapter NVIDIA GeForce RTX 5080 does not expose EXPERIMENTAL_RAY_QUERY: wgpu needs DXC (shader model 6.5) for DX12 ray query and this Cargo-only build uses FXC; ray query is validated on Vulkan only" (Metal or other backends that do expose the feature: "ray query not validated on Metal ...") |
| `device_request_failed` | "device creation with EXPERIMENTAL_RAY_QUERY failed: ..." |
| `device_missing_feature` | "the device was created without EXPERIMENTAL_RAY_QUERY; merge RtFeatureRequest::for_adapter into the device descriptor" (or missing acceleration-structure limits) |
| `self_test_failed` | "self-test step 'closest hit' failed: self-test ray 41 (origin [...], dir [...], mask 0xff, t in [0, 100]): front_face = false, expected true" - the first mismatch with measured and expected numbers |
| `device_lost` | "device lost: ..." / "GPU validation error during the scene commit: ..." / "uncaptured GPU error: ..." |

An `RtDevice` exists only after its self-test passed, so holding one is the proof of the
hardware tier. Its `status()` stays `Hardware` until a GPU failure is recorded (a
validation error in a crate-submitted command, a failed wait or map, a device-lost or
uncaptured-error callback on standalone devices, or `report_device_lost` from the host);
then it reports `Unavailable`/`device_lost` and every later operation returns
`RtError::Unavailable`. There is no automatic promotion: create a new `RtDevice` to retry.

## Token audit

The crate opts out of the workspace `unsafe_code = "forbid"` table (as `engine-dlss` and
`engine-acoustics-gpu` do) and sets `unsafe_code = "deny"` and
`unsafe_op_in_unsafe_fn = "deny"`. Exactly one function lifts the lint:
`device::experimental_features_token`, which creates `wgpu::ExperimentalFeatures::enabled()`.

- **What wgpu requires** (`wgpu-types` 30.0.1, `tokens.rs`, `# Safety`): the caller
  acknowledges that experimental APIs may contain UB-causing bugs reachable from safe calls,
  and agrees to report such bugs upstream. There is no other precondition.
- **How the crate meets it:** the token is created only by `RtFeatureRequest::for_adapter`,
  only for a Vulkan adapter exposing `EXPERIMENTAL_RAY_QUERY`, and the request adds no
  other `EXPERIMENTAL_*` feature, so a renderer that merges it gains no other experimental
  API. The token is active from device creation, so the self-test itself builds
  acceleration structures and traces under it; afterwards the self-test gates every use
  (no `RtDevice`, `RtScene`, kernel or scene bind group exists before it passes). wgpu is
  pinned to `=30.0.1`; every mesh, instance, limit and kernel input is validated at the
  crate boundary before reaching wgpu; GPU errors and device loss demote the status.
- **Reporting:** a suspected wgpu bug found through this crate is reported to
  gfx-rs/wgpu with a reproduction before any workaround lands.

## Integrating with the renderer

```rust
use engine_render_rt::{RtDevice, RtFeatureRequest};

// 1. While building the device descriptor (the device-feature composition hook):
let request = RtFeatureRequest::for_adapter(&adapter); // Err(status) when ineligible
if let Ok(request) = &request {
    request.apply(&mut descriptor); // or merge_into(&mut features, &mut limits, &mut experimental)
}
let (device, queue) = adapter.request_device(&descriptor).await?;

// 2. After device creation: run the self-test.
let rt = match RtDevice::new(&adapter, &device, &queue) {
    Ok(rt) => Some(rt),            // rt.status() is Hardware
    Err(status) => { log(status); None } // raster fallbacks
};
```

- `RtFeatureRequest { features, limits: RtLimitsRequest, experimental }` is a plain `Copy`
  value. `features` is `EXPERIMENTAL_RAY_QUERY` plus
  `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` when the adapter has it (for `R8Unorm` and
  `R16Float` storage outputs). `merge_limits` raises only the five acceleration-structure
  limits to wgpu's minimum supported values (`max_blas_primitive_count` 2^28,
  `max_blas_geometry_count` and `max_tlas_instance_count` 2^24 - 1,
  `max_acceleration_structures_per_shader_stage` 1,
  `max_buffers_and_acceleration_structures_per_shader_stage` 29) and never lowers a value.
  `RtFeatureRequest::none()` adds nothing.
- The renderer merges the request through the C1 device hook
  (`DeviceFeatureRequest::new("engine-render-rt").require(features).with_limits(limits)
  .with_experimental(experimental)`, see [Frame integration](#frame-integration));
  standalone tools use `RtDevice::open_standalone` (own instance, Vulkan adapter, device
  and failure callbacks) or a host-built device as in `tests/gpu_tiers.rs`.
- `RtDevice::new` installs no wgpu callbacks on a shared device (the renderer owns them);
  forward a loss with `rt.report_device_lost(message)`.
- `probe_adapters(backends)` lists every adapter's eligibility and reason without opening
  a device.

## Scenes: BLAS and TLAS

`RtScene::new(&rt, RtSceneLimits::for_device(rt.limits()))`, then:

| Call | Effect at the next commit |
| --- | --- |
| `add_mesh(&RtMeshData { positions, indices: RtIndices::U16/U32, normals })` | Mesh stored, shared buffers repacked, its BLAS built |
| `update_mesh(id, &data)` | Its BLAS rebuilt (reused object when vertex/index counts are unchanged; buffers rewritten in place, otherwise repacked) and the TLAS rebuilt |
| `remove_mesh(id)` | Refused while instances use it; repack |
| `add_instance(&RtInstanceDesc { mesh, transform, mask, custom_index, material_index })` | TLAS rebuilt |
| `set_instance_transform(id, t)` / `set_instance(id, &desc)` / `remove_instance(id)` | TLAS rebuilt; no BLAS work |
| `commit()` | Uploads, BLAS and TLAS builds submitted inside a validation scope (does not wait) |
| `record_commit(&mut encoder)` | Same, recorded into the renderer's encoder |

- One BLAS per mesh (`OPAQUE`, `PREFER_FAST_TRACE`, indexed `Float32x3`). `u16` indices are
  widened to `u32` in the shared index buffer, so one index format serves both the BLAS
  input and shader fetches. Positions, normals and indices of all meshes live in shared
  buffers that are both `BLAS_INPUT` and `STORAGE`; each BLAS is built from its range
  (`first_vertex`, `first_index`).
- The TLAS slot of an instance is its `InstanceId::index()` (the hit's `instance_index`);
  TLAS custom data is the instance's 24-bit `custom_index`. Capacity grows in powers of
  two; freed slots stay empty until reused. wgpu 30 has no TLAS refit
  (`ALLOW_UPDATE` builds as normal), so every instance edit is a full TLAS build.
- Handles are generation-checked: stale `MeshId`/`InstanceId`s are rejected.
- `bind_group()` is `None` before the first commit; `bind_group_generation()` increments
  whenever it is replaced (buffers or TLAS reallocated), so cached pass bind groups can
  refresh. Kernels and `cast_rays` refuse a scene with uncommitted edits.

### Limits validated at the boundary

`RtError::Invalid` (nothing reaches the GPU) for: empty positions; an index count that is
zero or not a multiple of 3; an index out of range; a non-finite position; a normal count
different from the vertex count, or a non-finite or zero-length normal; a non-finite or
singular (|det| < 1e-12) transform; `custom_index` > 2^24 - 1; and any `RtSceneLimits`
excess. Defaults (`for_device` lowers them to the device): 65,536 meshes, 2^20 instances
(and at most the device TLAS limit), 2^24 triangles and vertices per mesh (and at most the
BLAS primitive limit), 2^25 triangles and vertices in total, each shared buffer within the
device's storage-binding size (128 MiB by default, so about 11.2M triangles and vertices).
`check_device` rejects limits the device cannot honour.

### Statistics

`RtSceneStats`: `blas_count`, `instance_count`, `triangle_count` (each mesh once),
`instanced_triangle_count`, `vertex_count`, `buffer_bytes` (shared buffers plus instance
records), `blas_input_bytes` (12 per vertex + 4 per index of every live BLAS),
`tlas_capacity`, `commits` and `last_commit: RtCommitStats { blas_built, tlas_built,
geometry_repacked, build_input_bytes (rebuilt BLAS inputs + 64 per TLAS slot when the TLAS
is built), uploaded_bytes }`. wgpu 30 does not expose acceleration-structure memory sizes,
so these are input and buffer sizes, not AS sizes.

## WGSL library

`engine_render_rt::wgsl::compose(group, body)` returns `enable wgpu_ray_query;`, the
library (`src/shaders/ray_query.wgsl`) with its scene bindings at `@group(group)`, then
`body`. `wgsl::library(group)` returns the library alone; `wgsl::LIBRARY_TEMPLATE` keeps the
`RT_GROUP` placeholder. Bind `scene.bind_group()` at that group with the layout
`rt.scene_bind_group_layout()` (compute and fragment visibility):

| Binding | Resource |
| --- | --- |
| 0 | `rt_tlas: acceleration_structure` |
| 1 | `rt_instances: array<RtInstanceData>` - `{ geometry, material, custom_index, flags }` per TLAS slot |
| 2 | `rt_geometries: array<RtGeometryData>` - `{ vertex_offset, index_offset, triangle_count, flags }` per mesh (bit 0: has normals) |
| 3 | `rt_indices: array<u32>` |
| 4 | `rt_positions: array<f32>` (xyz per vertex) |
| 5 | `rt_normals: array<f32>` (xyz per vertex; zero for meshes without normals) |

Functions (all geometry is opaque; masks use the low 8 bits; `t` is in units of `dir`):

- `rt_occluded(origin, dir, t_min, t_max, mask) -> bool`: any hit in `[t_min, t_max]`,
  `RAY_FLAG_TERMINATE_ON_FIRST_HIT | RAY_FLAG_FORCE_OPAQUE`.
- `rt_closest_hit(origin, dir, t_min, t_max, mask) -> RtHit`: `hit`, `t` (`t_max` on a
  miss), `barycentrics` (weights of vertices 1 and 2), `custom_index`, `instance_index`
  (TLAS slot), `primitive_index` (triangle within the mesh), `front_face`,
  `object_to_world`, `world_to_object`.
- `rt_surface(hit, origin, dir) -> RtSurface`: `position` (`origin + dir * t`),
  `geometric_normal` (unit world normal of the object-space winding `(v1 - v0) x (v2 - v0)`,
  transformed by the inverse transpose, not face-forwarded), `shading_normal` (interpolated
  vertex normal, or the geometric normal), `material_index`, `geometry_index`.
- Helpers `rt_triangle_vertices(geometry, primitive)` and `rt_vec3(base, from_normals)`.

**Facing convention** (measured on the RTX 5080, Vulkan, and asserted by the self-test):
`front_face` is true when the triangle's vertices appear counter-clockwise from the ray
origin in object space, i.e. when the ray travels against `(v1 - v0) x (v2 - v0)`.

### Shading library

`wgsl::compose_kernel(group, body)` adds `wgsl::SHADING_LIBRARY`
(`src/shaders/shading.wgsl`, no bindings) between the ray-query library and `body`; every
kernel of this crate uses it:

- `rt_seed(pixel, width, frame, seed)` then `rt_rand() -> f32` in [0, 1) (PCG hash).
- `rt_basis(n)` (Duff et al. 2017), `rt_cosine_sample(n)` (pdf cos / pi),
  `rt_cone_sample(dir, cos_max)` (uniform in solid angle), `rt_ggx_vndf(v_local, alpha)`
  (Heitz 2018 visible-normal sample, normal = +z).
- `rt_sky(dir, zenith, horizon)` = `horizon + (zenith - horizon) * max(dir.y, 0)`.
- `rt_face_forward(n, dir)`, `rt_offset(position, bias)`, `rt_all_finite(v)`.
- `RtMaterialRecord { albedo: vec4, emissive: vec4 }` (`RtMaterial`) and
  `RtLightingRecord { sun_direction, sun_illuminance, sky_zenith, sky_horizon: vec4 }`
  (`RtLighting`).
- `rt_sun_diffuse(lighting, position, n, albedo, bias, t_max, mask)`: `albedo / pi *
  E_sun * max(n . l, 0)` times one `rt_occluded` visibility ray towards the sun centre.

## Self-test

`RtDevice::new` builds a scene through the public path (u16-indexed quad with vertex
normals, u32-indexed triangle without normals; instance 0 scaled x2, rotated 30 degrees
about X and moved to z = 5, mask 0x01, custom index 7; instance 1 at x = 10, mask 0x02,
custom index 0xABCDE; instance 2 = the quad at z = 8, mask 0x04) and casts 3,173 rays in
eight sets (all masks, single masks, `t_max` before the nearest hit, `t_min` past it,
reverse rays hitting back faces, a non-unit direction, and the second instance). Rays
within 0.02 (barycentric) of any edge are excluded so every expected answer is exact. An
f64 CPU reference gives the expected hit, instance, custom index, primitive, material,
facing, `t`, barycentrics, geometric and shading normals, position and `rt_occluded`.
Tolerances: `t` 1e-4 x max(1, t), barycentrics 1e-4, `1 - dot` of normals 1e-4, position
1e-4 x max(1, |p|). The first mismatch fails the tier with its numbers.

Measured on the RTX 5080 (Windows 11, Vulkan): 512 expected hits matched, 2,661 expected
misses, 313 mask-decided rays, 443 back-face hits; max errors `t` 7.7e-7,
barycentrics 2.1e-7, normals 1.1e-7, position 7.7e-7; about 100 ms including device
creation.

`RtDevice::cast_rays(&scene, &[ProbeRay])` runs the same probe kernel for tests and tools
(at most 2^20 rays, blocking) and returns `ProbeHit`s.

## Kernels

### Common inputs

Every lighting kernel is a compute pipeline with workgroup 8 x 8 (one invocation per
pixel), the scene at group 0 and its own bindings at group 1. Each `record` call records
one dispatch into the caller's `wgpu::CommandEncoder`. The scene must be committed,
materials must come from the same `RtDevice`, and settings, lighting and camera inputs are
validated before anything is recorded (`RtError::Invalid` otherwise).

| Input | Format | Contents |
| --- | --- | --- |
| `RtGBuffer::world_position` | `Rgba32Float` | xyz world position; w >= 0.5 where there is geometry |
| `RtGBuffer::world_normal` | `Rgba16Float` or `Rgba32Float` | xyz unit world shading normal, facing the camera |
| roughness (reflections) | any float or unorm format, `.x` | perceptual roughness in [0, 1] |
| signal (denoiser) | any float format readable by `textureLoad` | the kernel output to denoise |
| motion (denoiser) | `Rg16Float` (engine convention) or any float format, `.xy` | current-to-previous motion in pixels (y down): a surface at pixel `p` was at `p + motion` in the previous frame |

All textures of one dispatch are `width x height` and are bound as unfilterable-float
`texture_2d<f32>`. `RtMaterialTable::new(&rt, &[RtMaterial { albedo, emissive }])` holds
the Lambertian hit materials (instance `material_index`, clamped to the last material;
albedo in [0, 1], emission finite and >= 0, 1 to 65,536 materials). `RtLighting {
sun_direction, sun_illuminance, sky_zenith, sky_horizon }` is the per-frame sun (direction
towards it, irradiance on a surface facing it; zero turns it off) and the analytic sky.

### RT shadow mask (`ShadowMaskKernel`)

`ShadowMaskKernel::new(&rt, RtScalarFormat::{R8Unorm, R16Float, R32Float})` then
`record(&mut encoder, &scene, &ShadowMaskTargets { world_position, world_normal, output,
width, height }, &ShadowMaskParams { direction_to_light, normal_bias, t_min, t_max, mask,
angular_radius, samples, frame_index })`. `ShadowMaskParams::from_settings(&RtShadowSettings,
direction, frame)` maps the authored settings. `R32Float` is always available;
`R8Unorm`/`R16Float` need `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and adapter storage
support (`rt.supports_storage_format`), otherwise `new` returns `Invalid`.

Entry point `shadow_mask`. Group 1: 0 world positions, 1 world normals, 2
`texture_storage_2d<format, write>`, 3 uniform `{ direction_to_light: vec4 (xyz, w =
cos(angular_radius)), ray: vec4 (normal_bias, t_min, t_max), size_mask: vec4<u32> (width,
height, mask, samples), frame: vec4<u32> (frame, seed) }` (64 bytes).

- `angular_radius = 0` (hard): one ray towards the light gives 1 (lit) or 0 (shadowed).
- `angular_radius` in (0, 0.2] rad (soft): `samples` (1 to 64) rays are spread uniformly
  over the sun's cone and the output is the unoccluded fraction. At 1 sample per pixel the
  mask is noisy; the denoiser takes it as a `DenoiserSignal::Scalar`. Radii below about
  3.5e-4 rad round `cos` to 1 in f32 and are traced as hard shadows.
- Pixels without geometry are 1, surfaces facing away from the light centre are 0, and ray
  origins are offset by `normal_bias` along the normal.

Measured on the RTX 5080:

- Hard (256 x 256, 2 x 2 x 2 m box on a plane, light direction (1, 1, 0)): in all three
  formats the shadowed region reads 0 and the lit region 1. Every shadow edge (x = -3,
  z = +-1) is within 0.000 px of the analytic projection, and exactly the analytic
  2,048 + 256 (facing-away row) pixels are shadowed. Dispatch plus wait takes 0.5-0.7 ms.
- Soft (128 x 128 top-down floor, 2 x 1.5 x 2 m box, sun (1, 1, 0.3), 64 samples x 32
  frames): the largest difference from a CPU integration of the cone (4,096 stratified
  directions per pixel against the box) along the shadow axis is 0.018 / 0.023 / 0.029 for
  angular radius 0.02 / 0.05 / 0.1, with penumbrae 2 / 4 / 9 px wide.

### RT ambient occlusion (`AmbientOcclusionKernel`)

`AmbientOcclusionKernel::new(&rt, RtScalarFormat::R16Float)` (or `R8Unorm`/`R32Float`),
then `record(&mut encoder, &scene, &AoInputs { gbuffer, output, frame_index },
&RtAoSettings)`. Per pixel, `rays_per_pixel` cosine-weighted rays of length `radius` leave
the position offset by `normal_bias` along the normal. The output is `1 - occluded / rays`,
the cosine-weighted visibility of the hemisphere within the radius (1 = open). Background
pixels and pixels without a normal are 1.

Entry point `ambient_occlusion`. Group 1: 0 world positions, 1 world normals, 2
`texture_storage_2d<format, write>`, 3 uniform `{ ray: vec4 (radius, normal_bias), size:
vec4<u32> (width, height, rays, mask), frame: vec4<u32> (frame, seed) }` (48 bytes).

Measured on the RTX 5080 (128 x 128 `R16Float`, 64 rays x 8 frames, about 0.8 ms per frame
including readback):

| Case | GPU | CPU integral (1000 x 1000 cosine cells) |
| --- | --- | --- |
| Open plane, radius 4 | min 1.0000 (tolerance >= 0.999) | 1 |
| Crease (floor + 10 m wall), d = 0.05 m, radius 0.25 / 0.5 / 1 / 2 / 8 | 0.6257 / 0.5629 / 0.5306 / 0.5154 / 0.5035 | 0.6265 / 0.5635 / 0.5318 / 0.5159 / 0.5038 |
| Crease, worst of 8 distances per radius | max error 0.0029 (tolerance 0.01) | |
| Corner (two walls), d = 0.05 / 0.3 / 0.55 m, radius 1 | 0.2827 / 0.4660 / 0.6782 (crease: 0.5306 / 0.6861 / 0.8315) | 0.2827 / 0.4667 / 0.6779 |

The mean AO over the crease falls 0.9828 > 0.9569 > 0.9041 > 0.7984 > 0.5827 as the radius
grows from 0.25 to 8, and no pixel rises (each radius traces the same rays). The
infinite-radius limit at the wall is 0.5, half the cosine-weighted hemisphere.

### RT reflections (`ReflectionKernel`)

`ReflectionKernel::new(&rt)`, then `record(&mut encoder, &scene, &ReflectionInputs {
gbuffer, roughness, output, materials, lighting, eye, frame_index },
&RtReflectionSettings)`. Pixels with perceptual roughness `r <= roughness_cutoff` trace one
ray: the mirror direction when `r^2 < 1e-3`, otherwise a GGX visible-normal sample with
alpha = `r^2` (a sample below the surface falls back to the mirror direction). The closest
hit returns its emission plus its albedo lit by the sun (`rt_sun_diffuse`, one visibility
ray), and a miss returns the sky. There is no further bounce at the hit.

Output `Rgba16Float`:

- rgb: radiance arriving along the sampled direction. Apply the specular BRDF and Fresnel
  (for example a split-sum DFG term) when compositing.
- a: hit distance; `REFLECTION_MISS_DISTANCE` (65504) on a miss; `REFLECTION_ROUGH_FLAG`
  (-1, rgb 0) above the cutoff, where the raster fallback shades the pixel;
  `REFLECTION_NO_GEOMETRY_FLAG` (-2, rgb 0) for background pixels.

Entry point `reflections`. Group 1: 0 world positions, 1 world normals, 2 roughness, 3
`texture_storage_2d<rgba16float, write>`, 4 `array<RtMaterialRecord>`, 5 uniform (128
bytes: `RtLightingRecord`, `vec4(eye, roughness_cutoff)`, `vec4(normal_bias,
max_distance)`, `vec4<u32>(width, height, mask, material_count)`, `vec4<u32>(frame,
seed)`).

Measured on the RTX 5080 (128 x 128 mirror floor below an emissive, sun-lit box, eye
(0, 3, 6), 8 frames):

- All 7,095 mirror pixels whose CPU mirror ray passes no closer than 2 mm to a box edge
  agree with the CPU (633 reflect the box): 0 mismatches.
- Radiance is within 0.042% of `emissive + albedo * cos` on box hits and of the sky on
  misses; hit distance is within 0.092% (f16).
- All 2,032 pixels above the cutoff and the 128 background pixels carry their flags.
- 997 glossy (roughness 0.3) pixels hit the box in some but not all frames.
- Dispatch plus wait takes 0.4-0.5 ms.

### RT diffuse GI (`DiffuseGiKernel`)

`DiffuseGiKernel::new(&rt)`, then `record(&mut encoder, &scene, &GiInputs { gbuffer,
output, materials, lighting, frame_index }, &RtGiSettings)`. Per pixel, `rays_per_pixel`
cosine-weighted paths of up to `bounces` (1 to 8) segments are traced. Every hit adds its
emission plus its albedo lit by the sun (next-event estimation with one `rt_occluded` ray),
a miss adds the sky, and later segments are cosine-sampled with throughput `*= albedo`.

Output `Rgba16Float`: rgb is the cosine-weighted mean incident radiance (irradiance / pi;
multiply by the surface albedo for the diffuse indirect radiance), a is 1 on geometry and 0
elsewhere. The direct sun on the G-buffer surface itself is not included, because the
shadow-masked direct pass owns it.

Entry point `diffuse_gi`. Group 1: 0 world positions, 1 world normals, 2
`texture_storage_2d<rgba16float, write>`, 3 `array<RtMaterialRecord>`, 4 uniform (112
bytes: `RtLightingRecord`, `vec4(normal_bias, max_distance)`, `vec4<u32>(width, height,
mask, material_count)`, `vec4<u32>(frame, seed, rays, bounces)`).

Measured on the RTX 5080 (64 x 64, about 1.1 ms per frame including readback):

- Colour bleeding (white floor of albedo 0.8, red wall of albedo (0.8, 0.05, 0.05) lit by a
  sun of 3 from (1, 1, 0), grey sky 0.2, 16 rays x 2 bounces x 8 frames): R/G = 4.04 at
  0.1 m from the wall, 2.47 at 1.7 m and 1.08 at 12.7 m.
- Closed furnace (inside a 4 m box of albedo a and emission 0.25, which sun and sky cannot
  reach): the mean equals `E (1 - a^B) / (1 - a)` within 0.046% (f16 rounding; tolerance
  0.2%) for B = 1, 2, 3, 8 and a = 0.5, 0.7, 0.9 (for example 1.42383 for a = 0.9, B = 8).
  Every texel is finite and non-negative.

### Spatio-temporal denoiser (`Denoiser`)

`Denoiser::new(&rt, width, height, DenoiserSignal::{Scalar, Rgb, RgbHitDistance},
&DenoiserSettings)`, then once per frame `record(&mut encoder, &DenoiserInputs { gbuffer,
signal, motion, eye, camera_cut })`. `output()` / `output_view()` is the `Rgba16Float`
result (`STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC`, the same texture every frame);
after a successful command submission, call `finish_scene()` once to advance CPU
history. An abandoned encoder or failed scene uses `reset()` instead, so the next
record starts without stale history. `frame_count()` counts finished scenes;
`reset()` discards the history and `set_settings` applies new validated settings. The
denoiser is plain compute (no ray query); one instance serves one signal of one view.

| Signal | Filtered | Output |
| --- | --- | --- |
| `Scalar` (AO, soft shadow) | `.x` | `(v, 0, 0, 1)` |
| `Rgb` (GI) | `.rgb` | `(rgb, 1)` |
| `RgbHitDistance` (reflections) | `.rgb`; pixels with `.a < 0` (rough and background flags) are not filtered, feed no neighbour and pass through unchanged | `(rgb, input .a)` |

Each frame runs the SVGF passes (Schied et al. 2017), all with workgroup 8 x 8. Group 0
bindings 0-2 are shared: the `DnParams` uniform (80 bytes: eye, reprojection tolerances
and history cap, edge stops, `vec4<u32>(width, height, signal kind, no-history)`,
`vec4<u32>(step, min history)`), world positions and world normals.

1. `temporal` (bindings 3 signal, 4 motion, 5-7 previous history / moments / geometry,
   8-10 storage `rgba32float` integrated / moments / geometry): bilinear reprojection to
   `p + motion`. Each of the four taps is rejected when its stored view distance differs by
   more than `reprojection_depth_tolerance` x the current one, or when the cosine between
   the normals is below `reprojection_normal_threshold`; accepted taps are renormalized.
   The history length becomes `min(previous + 1, max_history_length)`. It is 1 when every
   tap is rejected, on the first frame, after `reset()` and with `camera_cut`. The signal
   and the first two luminance moments are blended with `alpha = 1 / length`, and the
   stored variance is the moment variance divided by the length (the variance of the
   accumulated mean).
2. `variance` (3 integrated, 4 moments, 5 storage): histories shorter than
   `min_history_for_temporal_variance` use an edge-aware 7 x 7 spatial luminance variance,
   scaled by `min_history / length`.
3. `atrous`, `atrous_iterations` times (same bindings, ping-pong): 5 x 5 B3-spline taps at
   steps of 1, 2, 4, ... px, weighted by `exp(-|n_p . (x_q - x_p)| / (phi_depth *
   distance_p))` (plane distance), `max(0, n_p . n_q)^phi_normal` and `exp(-|l_p - l_q| /
   (phi_luminance * sigma_p))`, where `sigma_p` is the 3 x 3 blurred standard deviation.
   The variance is filtered with the squared weights, and the first iteration's output is
   copied into the history.
4. `resolve` (3 filtered, 4 moments, 5 signal, 6 storage `rgba16float`).

The history textures (signal, moments + length, normal + view distance) are owned
ping-pong `Rgba32Float` textures. The denoiser keeps the previous G-buffer itself, so
callers pass only the current frame. Reflections are reprojected with the surface motion
(no virtual-image parallax correction), so keep their history short.

Measured on the RTX 5080 (128 x 128, default settings: 5 iterations, history 32, depth
tolerance 0.1, normal threshold 0.9, phi depth 0.05 / normal 128 / luminance 4; about
1 ms per frame including upload):

| Test | Result |
| --- | --- |
| 1-spp Bernoulli samples of a smooth target `0.5 + 0.35 sin cos`, 16 frames | RMSE 0.4680 -> 0.0247 (19.0x lower; 0.0593 after the first frame alone), residual variance 359x lower; temporal accumulation alone: 4.0x |
| Depth step (planes at 10 m and 20 m, values 0.2 / 0.8, 1-spp) | 10-90% transition 0 px (control without depth and luminance stops: 39 px) |
| Object (5 m, value 1) moving 4 px/frame over a background (10 m, value 0) for 8 frames after 16 still frames | largest ghost in the uncovered region 0.0000, moved object >= 0.9995 (control with rejection off: ghost 0.95) |
| 8 frames of 0.2, then 0.9 | 0.8999 with `camera_cut` (or `reset()`); 0.2776 without |
| AO radius 1, 1 ray per frame, against a 1,024-ray reference | RMSE 0.1389 -> 0.0181 (7.7x) |
| Soft shadow radius 0.05, 1 sample per frame, against a 1,024-sample reference | RMSE 0.0556 -> 0.0143 (3.9x) |
| Diffuse GI, 2 bounces, 1 path per frame, against a 1,024-path reference | RMSE 0.1223 -> 0.0107 (11.5x) |
| Glossy reflections, roughness 0.3, 1 ray per frame, against a 1,024-frame reference | RMSE 0.0497 -> 0.0050 (10.0x); flagged pixels and hit distances pass through unchanged |

### Reference path tracer (`PathTracer`)

`PathTracer::new(&rt, width, height, &[RtMaterial { albedo, emissive }])`, then
`record_frame(&mut encoder, &scene, &PathTracerParams { inv_view_proj, eye, sky_zenith,
sky_horizon, max_bounces, rr_start, seed, mask, t_max, jitter, ray_offset })` once per frame;
`reset()` restarts accumulation, `output()`/`output_view()` give the current
`Rgba32Float` (rgb = running mean, a = samples), `read_non_finite_samples()` reads the
discard counter.

- Lambertian albedo per material (instance `material_index`, clamped to the last
  material), two-sided emission, sky `horizon + (zenith - horizon) * max(dir.y, 0)`
  (constant when equal), cosine-weighted sampling around the face-forwarded geometric
  normal (throughput *= albedo), Russian roulette for continuations `>= rr_start` with
  survival `clamp(max(throughput), 0.05, 0.95)`, at most `max_bounces` (<= 1024) bounces.
  One sample per pixel per frame; the camera ray passes through the unprojected point at
  NDC depth 0.5, so standard and reversed depth both work.
- A non-finite sample is discarded (the mean keeps its previous value) and counted.
- Entry point `path_trace`, workgroup 8 x 8. Group 0: scene. Group 1: 0 previous
  accumulation `texture_2d<f32>`, 1 `texture_storage_2d<rgba32float, write>`, 2 materials
  `array<{ albedo: vec4, emissive: vec4 }>`, 3 uniform (160 bytes: `inv_view_proj`, `eye`,
  `sky_zenith`, `sky_horizon`, `vec4<u32>(width, height, frame, max_bounces)`,
  `vec4<u32>(rr_start, seed, mask, material_count)`, `vec4<f32>(t_max, jitter,
  ray_offset, 0)`), 4 `array<atomic<u32>, 4>` statistics.

Measured on the RTX 5080 (tolerance 2% relative per channel):

| Test | Expected | Measured |
| --- | --- | --- |
| Convex icosphere (1,280 triangles), albedo (0.25, 0.5, 0.75), uniform sky 2, 64 spp | a L = (0.5, 1.0, 1.5) | (0.50000, 1.00000, 1.50000); worst pixel 0.000%; sky pixels 2.00000 |
| Closed box, albedo (0.5, 0.5, 0.8), emission 0.5, 2 bounces, no roulette | E (1 - a^3) / (1 - a) = (0.875, 0.875, 1.22) | exact |
| Closed box, 256 bounces, roulette from 2, 256 spp | E / (1 - a) = (1.0, 1.0, 2.5) | (1.00034, 1.00034, 2.50346), max error 0.138% |
| Plane, albedo 0.6, sky zenith 2 / horizon 0.5, 256 spp | a (H + (Z - H) 2/3) = 0.9 (uniform sampling would give 0.75) | 0.90009 (0.010%) |

Every run had 0 non-finite samples, all outputs finite and non-negative, and the sample
count channel equal to the frame count. Frame cost 0.15-0.3 ms at 64-96 px square.

## Settings and status

`RtEffectsSettings { shadows, ambient_occlusion, reflections, diffuse_gi, denoiser }` are
plain serde structs (snake_case, `deny_unknown_fields`, every field optional with the
default below), intended as future authored component data. `validate()` checks every
range and names the field in its error (for example `"ambient_occlusion.radius = 0 must be
within (0, 100]"`), and each kernel validates its settings on every `record`. `enabled`
and `denoise` are for the frame integration; the kernels ignore them.

| Settings | Field | Range | Default |
| --- | --- | --- | --- |
| `RtShadowSettings` | `enabled` | | false |
| | `sun_angular_radius` (rad; 0 = hard) | [0, 0.2] | 0 |
| | `samples_per_pixel` | [1, 64] | 1 |
| | `normal_bias` (m) | [0, 1] | 0.001 |
| | `max_distance` (m) | (0, 1e6] | 10000 |
| | `denoise` (soft shadows only) | | true |
| `RtAoSettings` | `enabled` | | false |
| | `radius` (m) | (0, 100] | 1 |
| | `rays_per_pixel` | [1, 64] | 1 |
| | `normal_bias` (m) | [0, 1] | 0.001 |
| | `denoise` | | true |
| `RtReflectionSettings` | `enabled` | | false |
| | `roughness_cutoff` | [0, 1] | 0.5 |
| | `max_distance` (m; also the sun ray) | (0, 1e6] | 1000 |
| | `normal_bias` (m) | [0, 1] | 0.001 |
| | `denoise` | | true |
| `RtGiSettings` | `enabled` | | false |
| | `bounces` | [1, 8] | 1 |
| | `rays_per_pixel` | [1, 64] | 1 |
| | `max_distance` (m) | (0, 1e6] | 1000 |
| | `normal_bias` (m) | [0, 1] | 0.001 |
| | `denoise` | | true |
| `DenoiserSettings` | `atrous_iterations` | [0, 8] | 5 |
| | `max_history_length` (frames) | [1, 256] | 32 |
| | `reprojection_depth_tolerance` (relative) | (0, 1] | 0.1 |
| | `reprojection_normal_threshold` (cosine) | [-1, 1] | 0.9 |
| | `phi_depth` | (0, 10] | 0.05 |
| | `phi_normal` | [1, 1024] | 128 |
| | `phi_luminance` | (0, 1000] | 4 |
| | `min_history_for_temporal_variance` (frames) | [1, 32] | 4 |

`RtEffectsSettings::status(&RayTracingStatus) -> RtEffectsStatus` gives one
`RtEffectStatus { effect, mode, denoised, reason }` per effect (serde, snake_case):

| `mode` | When | `reason` |
| --- | --- | --- |
| `disabled` | `enabled = false` | "disabled in settings" |
| `fallback` | invalid settings | the validation error + "; using the raster fallback" |
| `fallback` | the tier is unavailable | the `RayTracingStatus` text (for example the DX12/FXC reason) + "; using the raster fallback" |
| `ray_traced` | otherwise | `None`, or a note: reflections name their roughness cutoff; "not denoised: ..." when the denoiser settings are invalid or a soft shadow has `denoise = false` |

The status describes what this crate's kernels would run for the settings on that tier. It
does not claim that a frame runs them; the desktop renderer reports what each frame actually
ran (below).

## Frame integration

The desktop renderer (`engine-desktop`, `src/renderer/rt.rs`) owns the integration;
`engine-render-pipeline` stays RT-free and only gained the generic screen-lighting inputs
described in [render architecture](rendering-architecture.md#screen-lighting-inputs).
`engine-desktop` depends on this crate unconditionally (no SDK is involved).

### Device

- `prepare_ray_tracing(&instance, surface, &mut options, &frame)` runs before device
  creation when the first frame's camera has a `RayTracing` component with any effect
  enabled: it picks the adapter, calls `RtFeatureRequest::for_adapter` and adds
  `DeviceFeatureRequest::new("engine-render-rt").require(rt.features)
  .with_limits(limits).with_experimental(rt.experimental)` (limits = `Limits::default()`
  raised by `merge_limits`) to `WgpuOptions::requirements`. The window runner
  (`WindowSurface::with_ray_tracing`), `render_screenshot` and
  `OffscreenSprites::wgpu_ray_traced` use it; an `Err` (for example the DX12/FXC reason)
  is handed to `GpuRenderer::note_ray_tracing_request` and becomes the fallback reason.
- The first frame that runs an `engine.rt.*` feature calls `RtDevice::new(&adapter,
  &device, &queue)` on the renderer's own device once (the self-test, about 100 ms) and
  forwards `GpuContext::on_device_lost` to `RtDevice::report_device_lost`. A device created
  without the request reports `device_missing_feature`; there is no retry.

### Scene sync

Before each view, `RtHost` syncs one `RtScene` from `SceneRenderFrame.meshes`: opaque draws
with vertices and indices only. One BLAS per mesh asset keyed by its `Arc<Mesh>` address
(the same identity as the GPU mesh cache; vertex normals when the mesh has one per
vertex); instance `i` is draw `i` (row-major transform from the draw's model matrix, mask
`0x02 | 0x01` for `cast_shadows`, material `i`); instances are updated in place only when
their descriptor changed, dead meshes are removed, and `commit()` runs only when the scene
is dirty. Materials: albedo = `MeshMaterial.tint.rgb`, emission = tint for `unlit`
materials, otherwise 0 (there is no authored emissive). The status carries `blas_count`,
`instance_count`, `triangle_count` and `last_blas_built` (0 on an unchanged frame).

### Features

| Feature | Stage | Ray traced | Fallback (`fallback`) |
| --- | --- | --- | --- |
| `engine.rt.shadows` | Prepass | `ShadowMaskKernel` towards the first directional light, mask 0x01 (casters); denoised only for soft shadows | the shadow maps (`shadow_maps`) |
| `engine.rt.ao` | Prepass | `AmbientOcclusionKernel`, denoised (`Scalar`) | screen-space hemisphere SSAO, 16 samples, radius = `ambient_occlusion.radius` (`ssao`) |
| `engine.rt.reflections` | Prepass | `ReflectionKernel` with a zero roughness texture (mirror rays everywhere), denoised (`RgbHitDistance`) | nothing (`none`) |
| `engine.rt.gi` | Prepass | `DiffuseGiKernel`, denoised (`Rgb`) | the constant ambient (`constant_ambient`) |

All four run after `engine.prepass.normals` and `engine.prepass.motion`, are disabled by
default, and declare `needs { normals, motion, screen_lighting }`. An enabled `RayTracing`
effect inserts `true` for its feature into the view's overrides unless
`Camera3D.render_features` already names it, so a camera can force a feature off (status:
disabled, "engine.rt.x is disabled by Camera3D.render_features") or on with the default
settings. The features are registered in the reserved namespace
(`FeatureRegistry::register_engine`) the first time a frame wants one.

Per frame and view, the first running feature reconstructs the RT G-buffer: a compute pass
writes world positions (`Rgba32Float`, w = 1 where the normal prepass covered the pixel)
from the prepass depth and the inverse jittered view-projection; normals are the C1 normal
target. Each effect then records its kernel into the Prepass encoder, denoises with its own
`Denoiser` per view (C1 motion vectors; `camera_cut = ViewData.reset_history`, which the
native path sets after `GpuRenderer::reset_temporal_history`), and a blit pass writes the
result into its screen-lighting input. The sun is the first directional light of the
frame's light list (direction and `color x intensity` in shading units); the kernels' sky
is the camera ambient (zenith = horizon), so open surfaces keep the ambient level.

### How the lit shader consumes the effects

`engine::lighting` reads the four screen-lighting textures (`engine::view` bindings 7-10)
at the fragment's pixel; every material that uses `engine_direct_lighting` and
`engine_indirect_lighting` (the built-in `lit.wgsl` does) gets the effects:

- Shadows: `screen_shadow.x` replaces the first directional light's shadow-map visibility
  where `screen_shadow.y = 1` (written by `engine.rt.shadows`), otherwise multiplies it
  (the cleared value 1 changes nothing). `receive_shadows = false` ignores both.
- AO: `screen_ao.x` multiplies the diffuse ambient / indirect term.
- GI: where `screen_indirect.a >= 0.5`, `rgb` (irradiance / pi) replaces the constant
  ambient; the term is `albedo x (1 - metallic) x incident x ao`.
- Reflections: where `screen_reflection.a >= 0` (= the roughness cutoff), surfaces with
  roughness <= cutoff add `rgb x F x (1 - roughness / cutoff)`, with
  `F = f0 + (max(1 - roughness, f0) - f0) (1 - n.v)^5` and `f0 = mix(0.04, albedo,
  metallic)`.

Without any `engine.rt.*` feature the bindings hold 1x1 neutral placeholders and the lit
result is bit-identical to before (every existing pixel test passes unchanged).

### Status

`GpuRenderer::ray_tracing_status()` (`RayTracingReport`, published to
`RayTracingControl` and `RuntimeSnapshot.ray_tracing`, and returned by `project_run_headless`
and `render_screenshot` as `ray_tracing`) reports `state`, `tier` (`hardware` /
`unavailable`), the tier `reason`, scene counts and per effect `effect`, `mode`
(`ray_traced` / `fallback` / `disabled`), `denoised`, `fallback` and `reason`.
`GpuRenderer::rt_effects_status()` returns the same as `RtEffectsStatus`. The reason starts
from `RtEffectsSettings::status` and is replaced by what the frame met: the tier text (for
example "ray tracing unavailable: the device was created without EXPERIMENTAL_RAY_QUERY
...; using the raster fallback"), a scene sync error, a kernel error, or "the scene has no
directional light to trace shadows for". Headless runs report `not_rendered` with each
enabled effect as `fallback` ("no frame has been rendered").

### Measured (RTX 5080, Vulkan, 96 x 96, `tests/ray_tracing.rs` in engine-desktop)

| Test | Result (8-bit sRGB output) |
| --- | --- |
| Floor shadowed by a 2 m box, sun (1, 1, 0), shadow maps off | RT: occluded 34, lit 163 (ray_traced, 2 BLAS, 2 instances; second frame 0 BLAS built); without RT shadows the occluded pixel reads 163 |
| Same scene, device without the RT request, shadow maps on | occluded 34, lit 163; `fallback` / `shadow_maps` with the device_missing_feature reason |
| Floor-wall crease, ambient only, radius 1 | crease 170 without AO, 141.8 with RT AO (16 rays, denoised), 146.6 with the SSAO fallback; open floor 170 in all three |
| Denoised RT AO (1 ray) | mean frame-to-frame difference 0.188 after 24 static frames, 4.436 right after a camera cut, 31.875 undenoised |
| Mirror floor (metallic 1, roughness 0) below a red unlit box | reflecting pixel (243, 63, 63), sky-only pixel (63, 63, 63); reflections off or device without RT: (0, 0, 0), status `disabled` / `fallback` (`none`) |
| White floor beside a red wall, sun (1, 1, 0), 16 paths x 2 bounces | floor next to the wall R/G 1.000 without GI, 1.179 with GI; fallback (`constant_ambient`) 1.000 |

## Tests

- `cargo test -p engine-render-rt`: validation, limits, status wording and bounding,
  request merging, layouts, the self-test scenario and mismatch reporting, settings ranges,
  serde defaults and unknown-field rejection, per-effect status, uniform layouts, and naga
  validation of every shader variant including the denoiser passes (no GPU needed).
- GPU tests (`#[ignore]`, verified on an RTX 5080 with Vulkan):
  `cargo test -p engine-render-rt --tests -- --ignored --nocapture --test-threads=1`
  (`gpu_tiers`, `scene_updates`, `shadow_mask`, `path_tracer`, `ambient_occlusion`,
  `reflections`, `diffuse_gi`, `denoiser`, `denoise_signals`).

## Limits and deferred work

- Hardware tier on Vulkan only; DX12 needs DXC (not in a Cargo-only build), Metal and Linux
  Vulkan are untested and report their reason.
- Opaque triangles only (no alpha-tested any-hit, no AABBs), no hit vertex return, no
  ray-tracing pipelines, no TLAS refit or BLAS compaction (wgpu 30).
- Mesh edits that change a mesh's size repack all shared geometry buffers (CPU copies of
  mesh data are kept for this).
- Frame integration: reflections trace mirror rays for every pixel (no roughness G-buffer;
  the lit shader applies the material roughness), so glossy lobes are not sampled; RT
  effects see opaque meshes only, with tint albedo and unlit emission (no textures);
  transparent meshes are lit without RT inputs (they read the screen inputs of the opaque
  surface behind them); custom vertex displacement is not in the BLAS. XR eye views run
  the effects with their own textures and denoisers (the scene sync repeats per view and
  builds nothing when unchanged); only the camera view writes the status.
- G-buffer input is world position (no depth plus inverse view-projection path); one
  denoiser instance per signal and view; uniform buffers and bind groups are created per
  dispatch.
- Reflections: one ray per pixel, Lambertian hit shading with the sun only (no indirect
  light at the hit), no virtual-image reprojection and no hit-distance-driven filter radius
  in the denoiser.
- GI and reflection hits use a hard sun (no angular radius); only the shadow mask has the
  soft-sun option. The sky is the analytic two-colour model.
- The denoiser's luminance stop can blur thin penumbrae (soft shadows measured a 3.9x RMSE
  reduction against 7.7-11.5x for AO, GI and reflections).
