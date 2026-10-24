# engine-acoustics

The acoustic world and CPU propagation of the spatial-audio design
(`docs/spatial-audio-design.md`, Stream B). This crate has no ECS and no GPU code. It
builds a validated triangle world, traces sound through it on the CPU, and publishes
mixer parameters through `engine_audio::acoustic::AcousticPublisher`.

## Acoustic world

- `AcousticWorldBuilder::new(WorldLimits)` takes materials, meshes, instances,
  volumes and at most one 2D proxy, then `build()` returns an `AcousticWorld`.
  - Every `add_*` call validates its input and the section 5.3 limits before it
    changes anything. A rejected call leaves the builder as it was, and a failed
    `build` returns no world.
  - Materials must be added before anything that uses them.
- Error codes:
  - `acoustics.world.limit` for any limit, including BVH depth over 48 and footprint vertex counts.
  - `acoustics.world.invalid` for bad geometry or transforms.
  - `acoustics.world.budget` for the 64 M-operation build budget.
  - `acoustics.material.invalid` for bad materials.
  - `acoustics.proxy.invalid` for bad proxy input.
  - `acoustics.request.invalid` for bad requests.
- Degenerate triangles (area < 1e-10 m²) are dropped and counted in
  `WorldStats::dropped_degenerate`.
- `revision()` comes from a process-wide counter, so every build gets a new, larger
  value. `with_revision(n)` pins it for pipelines that count their own rebuilds.
- `set_instance_transform` moves dynamic instances and refits the instance-level
  BVH. It increments `dynamic_generation()`.
- `raycast` returns the nearest double-sided hit. `Hit::triangle` is the mesh's
  original triangle index and `Hit::normal` faces the ray.
  `raycast_brute_force` is the BVH-free reference.
- Plane clusters merge coplanar triangles of static instances per (plane,
  material). They are the image-source mirrors.

### Flat layout shared with the GPU (`gpu_layout`, `GpuWorldView`)

| Array | Contents |
| --- | --- |
| `nodes` | Per-mesh BVHs, 32-byte `BvhNode`. A node with `count == 0` is interior, with children `left_or_first` and `left_or_first + 1` as absolute indices that always follow the parent. Leaves hold 1–4 triangles, and `left_or_first` indexes `triangles` absolutely. |
| `top_nodes` | Instance-level BVH, root at index 0 (empty for an empty world). Every leaf holds exactly one instance: `count == 1`, `left_or_first` = instance index. |
| `triangles` | Object-space `GpuTriangle`s, each mesh in leaf order starting at `GpuInstance::triangle_offset`. `v0[3]` holds the material index as `f32` bits, `v1[3]` the original triangle index as bits, and `v2[3]` is 0. |
| `instances` | Row-major 3×4 `world_from_object` and `object_from_world`. `node_offset` is the mesh root, or `u32::MAX` for a mesh without triangles, which is never in `top_nodes`. `mask` holds 8 bits. |
| `materials` | `reflect = 1 - absorption`, `transmission`, `scattering`, indexed by `MaterialId`. |

- Rays are transformed into object space without renormalising, so `t` stays in
  world units.
- Ray-triangle intersection is Möller–Trumbore. Among equal `t`, the lowest
  `(instance, triangle)` wins, so traversal order never changes the result.
- Builds are deterministic: binned SAH with 16 bins, axes in x, y, z order, and stable
  partitions.
- Trees are at most `MAX_BVH_DEPTH = 48` deep, so a depth-first walk needs 49 stack
  entries per tree.

### Materials (`AcousticMaterial::preset`)

Absorption comes from UFC 3-450-01 (2022), Table C-4. The 8 kHz band is 80% of
4 kHz, following that document's rule for 8 kHz. Steel comes from the akustik.ua
"Steel decking" row. Scattering follows Zeng, Christensen and Rindel, *Applied
Acoustics* 67 (2006), Table 5. Transmission uses the mass law `TL = 20 log(f m) − 47.3`
(Long, *Architectural Acoustics*, eq. 9.21) for the documented surface densities,
capped at absorption.

The module documentation lists each row's source. `Grass` returns
`acoustics.unimplemented`, because no published table could be verified and no
preset ships without a citation.

Transmission applies per crossed surface. A closed solid box therefore applies its
transmission twice, so use single panels or author `sqrt(τ)` for thick walls.

### Volumes and the 2D proxy

- **Volumes:** `add_volume` adds `Box` solids, `Room` shells and `Mesh` volumes.
  - `Room` shells may have open faces, set as bits `-x +x -y +y -z +z`.
  - Box rooms are listed in `rooms()` and `room_at(p)`, with exact volume and
    per-face areas. Open faces count with α = 1, which gives
    `RoomInfo::sabine_rt60` and `RoomInfo::eyring_rt60`.
- **2D proxy:** `add_proxy_2d` extrudes convex footprints in `units_per_meter`
  proportions, with `s = 1 / units_per_meter`.
  - Side view maps `(x, y)` to `(x s, y s, z)`, with `z` in `±depth/2`.
  - Top down maps `(x, y)` to `(x s, h, −y s)`, with `h` in `[0, wall_height]`.
  - Shapes: rect, 12-gon circle, 14-gon capsule, convex polygon.
  - The enclosure, floor, ceiling, `open_sky` and front/back planes are described in
    the `proxy2d` module docs.
- The returned `ProxyMapping` provides:
  - `to_acoustic` and `pose_to_acoustic`.
  - `metres_per_unit`.
  - `air()`: exact air volume, reflecting area and open area, computed from a 2D
    union of the footprints.
  - `footprint_instance(i)`: static footprints share one static instance, and each
    dynamic footprint gets its own dynamic instance.
  - `rigid_motion(translation, rotation, pivot)`: the `Affine3` that moves a dynamic
    footprint without a rebuild.

### World documents

- `AcousticWorldDocument` is serde-serialisable source geometry.
- Replay order is materials, meshes, instances, volumes, proxy. `build(limits)`
  replays it through the builder, so every limit applies again and the BVH is
  rebuilt rather than trusted.
- `seal()` writes an FNV-1a 64 content hash, and a mismatch is
  `acoustics.world.invalid`.
- Loaders cap files at `MAX_DOCUMENT_BYTES` (64 MiB).

## CPU propagation

### Energy model (`trace_cpu`, `rng`), shared bit-for-bit with the GPU RNG

- **Seeding:** ray `r` of a job uses `Pcg::for_ray(job.seed, r)`, where
  `ray_state = pcg(pcg(lo ^ pcg(hi)) ^ r)` and each draw is `state = pcg(state)`.
- **Emission:** 2 draws pick a uniform sphere direction. The ray starts with relative
  energy 1 per band, and deposits are scaled by `1 / rays`.
- **Reflection:** every reflection draws exactly 3 values, `u_scatter, u1, u2`.
  - Scattering is cosine-weighted with the Duff orthonormal basis when
    `u_scatter < scattering`. Otherwise the reflection is specular.
  - The ray restarts from `hit + n * ray_offset(hit)`.
- **Air:** energy decays as `exp(-2 m l)`, because `air_np_per_m` is an amplitude
  attenuation in nepers.
- **Receiver deposit:** applies to chords inside the receiver sphere after at least
  one reflection. Each deposits `e * exp(-2 m t_mid) * 4π l / V_rcv / rays` into bin
  `floor(((path + t_in + l/2) / c) / bin_s)`. This is normalised to the direct
  sound at 1 m.
- **Termination:** a ray ends after `max_bounces` reflections, or when
  `max_b e_b < 1e-6` of its initial energy.
- **Escapes:** a ray that hits nothing adds `mean_b e_b / rays` to `escaped`.
- **Mean free path:** averaged over reflection-to-hit segments.
- **Reduction:** chunks of 1024 rays are reduced in `f64` in chunk order, so results
  are bitwise independent of the thread count.

### Direct path, early reflections and fitting

- **Occlusion:** `occlusion_rays` rays from the listener to Fibonacci points on the
  source sphere.
  - Each ray multiplies the transmission of up to 4 crossed surfaces. A ray that
    crosses more is fully blocked.
  - The direct gain is `sqrt(mean factor)`, as an amplitude.
- **Image sources:** ray-guided discovery, using at most 64 planes from `er_rays`
  Fibonacci rays.
  - Paths are validated backwards with segment raycasts: each segment must hit its
    expected cluster, and the final segment to the source must be clear.
  - A path's amplitude is
    `sqrt(Π (1−α)(1−s)) · exp(−m L) / max(L, 1 m)`.
  - The `max_reflections` strongest paths are kept, sorted by `(delay, planes)`.
  - Directions are in the listener frame: `x` right, `y` up, `−z` forward.
- **Fitting:** `fit_reverb` uses Schroeder backward integration, with T30, then T20,
  then EDT (ISO 3382-1 ranges).
  - `level` is the late (`≥ late_start_s`) energy relative to the direct sound at
    1 m, as an amplitude.
  - `predelay_s` is the first non-empty bin.
- **Estimates:** `sabine_rt60` and `eyring_rt60` use `4mV` with `m = 2 · air_np_per_m`.
- **Impulse responses:** `synthesize_ir` produces deterministic LR4 band-noise IRs
  that carry the histogram energy per band.

### Return types kept from Stream 0

`AcousticMaterial::preset`, `sabine_rt60`, `eyring_rt60`, `fit_reverb` and
`synthesize_ir` keep the Stream 0 `Result` return types (stream D calls them).
Invalid input is an error rather than a fabricated number.

## Service (`AcousticsService`)

- `ServiceConfig.backends` are host-supplied GPU tiers, and the CPU tier is appended
  automatically.
- **Tier selection:** `Auto` and `GpuRayQuery` try `GpuRayQuery`, then
  `GpuCompute`, then `Cpu`. `GpuCompute` tries `GpuCompute`, then `Cpu`. A `Cpu`
  preference never initializes the GPU backends.
- **Failures:** each `initialize` error becomes part of `fallback_reason`, truncated
  to 512 bytes.
  - A fatal trace error or an invalid histogram demotes immediately.
  - Non-fatal errors (for example `acoustics.backend.timeout`) keep the last late
    field. Three consecutive ones demote.
  - There is no automatic promotion. `set_preference` re-probes every tier.
- **Threaded mode:** a worker runs at `update_hz` and also wakes on every
  submit, dynamic update or world change.
  - `submit`, `latest` (which uses `try_lock`), `status`, `set_world` and
    `update_dynamic` take short locks only.
  - The late field is traced at most `reverb_update_hz` times per second.
  - The ray count halves on a budget overrun and recovers by ×1.25 when an update
    takes under half the budget.
- **Synchronous mode:** `step_blocking` runs one update in the caller. The late field
  is traced every `round(update_hz / reverb_update_hz)` steps, and nothing adapts.
  Results are bitwise reproducible except `stats.elapsed_us` and the
  `budget_exceeded` flag, which depend on wall-clock time.
- **Per update:**
  - Up to 64 sources are traced, ranked by `priority / distance`.
  - Direct paths and image sources are computed for each traced source.
  - The late field is a listener-to-listener job, plus up to 4 hero jobs.
- **Publishing:** each update publishes one `AcousticFrame`, with voices sorted by
  `VoiceHandle` and up to 16 taps. `direction` is `Some` only when the world has a
  2D proxy. The `ParamHook` runs before the swap.
- **Additions to the frozen contract:**
  - `SourceRequest::voice: Option<VoiceHandle>`. engine-audio cannot rebuild a
    handle from `u64`. `None` traces without publishing, for queries.
  - `drain_events() -> Vec<ServiceEvent>`, for backend changes, failures and
    warnings.
  - `set_convolution_rate(Option<u32>)`, which synthesizes and prepares
    listener and hero kernels at the output rate.
- The service, `AcousticWorld` and `ProxyMapping` are `Send + Sync`.

## Reference fixtures (`reference`)

- `reference_shoebox(scattering)` builds the section 8 room. It is 10 × 6 × 8 m,
  absorption `[0.05, 0.1, 0.2, 0.4]`.
- Supporting fixtures:
  - `reference_trace_settings(length_s)`: 1 ms bins, c = 343 m/s, no air.
  - `reference_job(rays, seed)`: a listener job at `SHOEBOX_LISTENER` with a 1 m
    receiver and 256 reflections.
  - `cpu_reference_histogram(world, settings, job, threads)`.
  - `window_energy(h, t0, t1)`.
  - `histogram_csv(h)`.
  - `box_mesh` and `shoebox_world`.
  - The `SHOEBOX_*` analytic constants.
- `cargo run -p engine-acoustics --release --example shoebox -- out.csv` prints T30
  against Eyring and writes the CSV.

Measured on this machine (Windows 11, release build, 8 threads): 65,536 rays × 256
reflections take 0.61 s, about 21 M segments/s on the 12-triangle shoebox. The T30
errors against Eyring are +0.2%, +2.1%, +4.6% and +9.1% per band. The mean free
path is 5.1087 m, against 4V/S = 5.1064 m.

The excess decay time at high absorption is expected for diffuse reflection: it
comes from the variance of the path lengths, which Eyring ignores. The default
4,096 rays × 64 reflections is around 0.26 M segments, which exceeds the 4 ms budget
on 4 threads. Threaded mode then halves the ray count until the update fits.

## Tests

`cargo test -p engine-acoustics` runs the following:

| File | What it checks |
| --- | --- |
| `bvh.rs` | Test 9 |
| `limits.rs` | Test 10 |
| `image_sources.rs` | Test 11 |
| `reverb.rs` | Tests 12 and 13 |
| `proxy2d.rs` | Test 15, library half |
| `service.rs` | Tests 16 (CPU), 19 and 8 (service half) |
| `world.rs` | Rooms, documents, IR energy and the test 14 direct-path half |
