# Hardware ray tracing core (`engine-render-rt`)

`crates/engine-render-rt` is the renderer's ray-tracing foundation: capability probing,
the single audited experimental-feature token, a GPU self-test that gates the hardware
tier, BLAS/TLAS management, a reusable WGSL ray-query library and two reference compute
kernels (an RT shadow mask and a progressive path tracer). Integrating RT shadows,
reflections, AO, diffuse GI and denoising into the frame is a later step built on this
crate and the render extension API; nothing here changes the current frame.

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
- `engine-render-wgpu`'s `GpuContext::device_descriptor` does not yet carry
  `experimental_features`; the render-architecture work (C1) adds the composition hook
  through which this request is merged. Until then the crate is used with
  `RtDevice::open_standalone` (own instance, Vulkan adapter, device and failure callbacks)
  or with a host-built device as in `tests/gpu_tiers.rs`.
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

### RT shadow mask (`ShadowMaskKernel`)

`ShadowMaskKernel::new(&rt, ShadowMaskFormat::{R8Unorm, R16Float, R32Float})` then
`record(&mut encoder, &scene, &ShadowMaskTargets { world_position, world_normal, output,
width, height }, &ShadowMaskParams { direction_to_light, normal_bias, t_min, t_max, mask })`.
`R32Float` is always available; `R8Unorm`/`R16Float` need
`TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and adapter storage support
(`rt.supports_storage_format`), otherwise `new` returns `Invalid`.

Entry point `shadow_mask`, workgroup 8 x 8. Group 0: scene. Group 1: 0 world positions
`texture_2d<f32>` (`Rgba32Float`, w >= 0.5 where there is geometry), 1 world normals
`texture_2d<f32>` (xyz), both unfilterable float; 2 `texture_storage_2d<format, write>`;
3 uniform `{ direction_to_light: vec4, ray: vec4 (normal_bias, t_min, t_max), size_mask:
vec4<u32> (width, height, mask) }`. Output 1 = lit, 0 = shadowed; pixels without geometry
are 1, surfaces facing away from the light are 0. Ray origins are offset by
`normal_bias` along the normal. Depth plus inverse view-projection input is not provided;
the render pass supplies world positions.

Measured (256 x 256, 2 x 2 x 2 m box on a plane, light direction (1, 1, 0)): in all three
formats the shadowed region reads 0, the lit region 1, every shadow edge (x = -3,
z = +-1) is within 0.000 px of the analytic projection, and exactly the analytic
2,048 + 256 (facing-away row) pixels are shadowed; dispatch plus wait 0.5-0.7 ms.

### Reference path tracer (`PathTracer`)

`PathTracer::new(&rt, width, height, &[PathTracerMaterial { albedo, emissive }])`, then
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

## Tests

- `cargo test -p engine-render-rt`: validation, limits, status wording and bounding,
  request merging, layouts, the self-test scenario and mismatch reporting, and naga
  validation of every shader variant (no GPU needed).
- GPU tests (`#[ignore]`, verified on an RTX 5080 with Vulkan):
  `cargo test -p engine-render-rt --test gpu_tiers --test scene_updates --test shadow_mask
  --test path_tracer -- --ignored --nocapture --test-threads=1`.

## Limits and deferred work

- Hardware tier on Vulkan only; DX12 needs DXC (not in a Cargo-only build), Metal and Linux
  Vulkan are untested and report their reason.
- Opaque triangles only (no alpha-tested any-hit, no AABBs), no hit vertex return, no
  ray-tracing pipelines, no TLAS refit or BLAS compaction (wgpu 30).
- Mesh edits that change a mesh's size repack all shared geometry buffers (CPU copies of
  mesh data are kept for this).
- Frame integration (RT shadows, AO, reflections, diffuse GI, denoising), authored
  components and MCP control follow once the render extension API (C1) lands;
  `RayTracingStatus` already serializes for snapshots.
