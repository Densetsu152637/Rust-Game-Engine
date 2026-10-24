# Spatial audio and acoustic propagation: decision record and implementation plan

Task `advanced-engine-20261024/spatial-audio-design`, 2026-10-24. Status: **implemented and integrated; this
record keeps the original decisions and marks where the build deviated.** The text is
the design as proposed. The sections "Accepted deviations" (§6.3), "Integration
record" (§11) and the user guide [acoustics.md](acoustics.md) describe what exists;
where they disagree with an older sentence, they win. Measured facts are the §6.1
probes, the per-crate READMEs and the test results they cite.

User request: spatial audio that bounces and is damped by an acoustic world ("v_world"),
which is either the real 3D polygons of the scene or a 3D world synthesized in the
background from a 2D game with matching proportions; modern spatial-audio methods plus
ray-traced audio, with the modern method as the fallback when GPU hardware acceleration
is unavailable. Standing constraints: Cargo-only builds with no extra SDKs; desktop
Windows/Linux/macOS; everything configurable through MCP and native Rust
callbacks/components/events; no claims of unverified features.

## 1. Decision summary

| Topic | Decision |
| --- | --- |
| Acoustic world | Two-level triangle world (per-mesh BVH plus an instance level, mirroring BLAS/TLAS) with per-triangle acoustic materials in 4 bands (125/500/2k/8k Hz centres). Fed by render meshes, a 2D→3D proxy synthesizer and explicit acoustic volumes. New crate `engine-acoustics`. |
| Modern CPU pipeline | Geometric-acoustics hybrid: volumetric BVH occlusion with per-band transmission, ISO 9613-1 air absorption, delay-line Doppler, ray-guided image-source early reflections (order 2), stochastic ray-traced energy histogram that is fitted to a per-band FDN late reverb (Schroeder/T30), with Sabine/Eyring analytic paths for explicit rooms. Binaural output via the analytic Brown–Duda spherical-head model (no dataset), or the existing equal-power stereo law. |
| GPU ray tracing | The same stochastic energy tracer runs on the GPU. Tiers, in order: **hardware ray query** (wgpu 30 `EXPERIMENTAL_RAY_QUERY`, Vulkan) → **GPU compute BVH traversal** (GPU-accelerated, not hardware RT) → **CPU**. Output is per-band energy-time histograms, consumed as FDN parameters or, optionally, as synthesized impulse responses through uniform partitioned FFT convolution. New crate `engine-acoustics-gpu`. |
| Ray-query verdict | **Usable in wgpu 30.0.1 on Vulkan, verified on this RTX 5080** by a probe that built a BLAS/TLAS and traced rays from a WGSL compute shader with exact hit results. It requires `unsafe { wgpu::ExperimentalFeatures::enabled() }`. DX12 does not expose it here, and Metal is untested. See §6.1. |
| Real-time rule | The audio callback never waits for propagation. Parameters reach the mixer through a std-only `Mutex::try_lock` triple buffer (no `unsafe`, no allocation, no new dependency) and are ramped over about 21 ms. |
| Compatibility | `VoiceAcoustics::default()` (disabled) plus the `StereoPan` renderer must leave mixer output **bit-identical** to today's, so all existing engine-audio tests stay unchanged. |
| Dependencies | No new external crates. The FFT, BVH, HRTF and FDN are written in-house. wgpu/pollster/bytemuck are already in `Cargo.lock`. |

## 2. Current state (read from source, 2026-10-24)

**`crates/engine-audio`** (deps: cpal 0.18.2, hound 3.5.1; workspace lints, so `unsafe_code = "forbid"`):

- `lib.rs`: `AudioEngine` owns an optional `cpal::Stream` plus `AudioControl { inner: Arc<Mutex<Mixer>> }`. `AudioEngine::offline` mixes deterministically through `mix(&mut [f32])` (interleaved stereo; rejected while a device stream is live). `AudioConfig { sample_rate (8–192 kHz), max_voices (1–1024, default 64) }`. `Bus { Master, Music, Effects, Dialogue }`. `SpatialOptions { enabled, position, min_distance, max_distance, rolloff, attenuation: Attenuation { None, Linear, Inverse } }` is `Copy`, and tests build it as a full struct literal in about 60 places. `Listener { position, forward (-Z default), up }`, right-handed with Y up. `PlayOptions { looped, bus, gain, owner, pitch, spatial }` has `validate()`. Error type: `AudioError { code: &'static str, message }`.
- `mixer.rs`: `Mixer::mix` computes `spatial_gains(spatial, listener)` **once per voice per mix call**: distance law, then an equal-power pan from the listener right vector. It then linearly interpolates the clip at `step = clip_rate / out_rate * pitch`, downmixes stereo clips to mono when spatial, sums into the output and clamps to [-1, 1]. No delay lines, filters, HRTF, occlusion or reverb.
- `device.rs`: the CPAL callback calls `callback_control.lock()` (a **blocking** mutex shared with every control call) and mixes in 2048-sample stack scratch blocks. The README states the mixer does not claim hard real-time behaviour, and that clip retirement may free memory inside the callback.
- `sources.rs`: `AudioSources` gives one reusable voice per `SourceHandle` (≤1024 sources and ≤1024 observers), with event-observer playback.
- Tests: `tests/{audio,mix_math,spatial,voices,wav_decode}.rs` (2,790 lines), including exact gain checks.

**`crates/engine-project/src/audio.rs`** (ECS adapter): the `RuntimeAudio` resource holds the control and the loaded clips. Per entity, the `EntityAudio` component owns `AudioSources` plus `attachments: SourceHandle → (Entity, PlayOptions)`. `audio::source/configure/listener` resolve `world_transform(entity).position()`. `synchronize(world)` runs every tick after `refresh_world_transforms` (`runtime/mod.rs` line 284 and `runtime/scene.rs` line 130). It re-`configure`s **every attached source every tick** with its new position, and removes sources whose attachment died. There is **no authored audio component**: playback is created by native scripts. The listener is native-only as well.

**Scene geometry sources:**

- 3D: the `MeshRenderer { mesh: asset id, texture }` component plus `Transform`/`WorldTransform { matrix: [[f32;4];4] }`. The `content::Mesh { vertices: Vec<Vertex{position,uv,color}>, indices: Vec<u32>, normals }` loader takes JSON or `CGMESH 1`, up to 1,000,000 vertices and 3,000,000 indices. `GeometryOverride(Arc<Mesh>)` is per-entity evaluated (skinned or deformed) geometry. glTF import (`model_import.rs` → `engine_assets::import_gltf`, `ImportLimits`) generates `mesh` assets and a prefab of `Transform + MeshRenderer` entities. `MeshMaterial` is visual-only.
- 2D: `Collider2D { size, offset, trigger, layer, mask }` (axis-aligned boxes; `gameplay2d::World` holds ≤1024 bodies with coordinates ≤1e6) and `engine.PhysicsBody2D { shape: box|ball|capsule, half_extents, radius, half_height, material.sensor, … }` (rapier2d 0.36.1, rotation from `Transform`). `SpriteRenderer` is visual-only. **No tilemap component exists in the repository**, so the proxy synthesizer is specified over generic 2D footprints that a future tilemap can feed.
- Built-in components use the `engine.*` namespace in `Components.custom`. Their descriptors come from `simulation::component_descriptors()` and are chained in `ComponentCatalog::new` (`catalog.rs` line 45). Adapters are registered with `register_component::<T>(app, name, 1)`, and validation is JSON-schema plus typed. MCP `component_set` accepts any catalog name (`engine-mcp/src/tools/edit.rs` line 420), so new `engine.Acoustic*` components become MCP-editable with no MCP code.

**Precedents reused:**

- NavMesh baking (`engine-navigation`, `docs/navigation.md`): explicit world-space input, double precision where needed, a work budget (4M operations), hard limits (65,536 vertices, 32,768 triangles, 4,096 obstacles), assets that store **source data rather than trusted topology**, an MCP `navigation_bake` that never overwrites and returns a manifest entry for the caller to register, and a read-only `navigation_query`.
- The `UpscalerStatus`/`UpscalerControl` resource (`rendering3d.rs`): a request is not a claim, `state` plus a bounded `fallback_reason` (512 bytes), and host-written status.
- `RuntimeSnapshot` (`runtime/scene.rs`) carries `upscaler`, `network` and `input_devices`. `engine-tooling/src/mcp_tools/run.rs` (`project_run_headless`) caps each telemetry object at 64 KiB and lists oversized ones in `telemetry_omitted`.
- `engine-dlss` opts out of workspace lints and confines its `unsafe` to one documented module. This is the precedent for the single experimental-feature token (§6.1).
- `engine-render-wgpu::GpuContext` (`context.rs`): `InstanceDescriptor::new_without_display_handle()`, `PollType::Wait { timeout }`, and an uncaptured-error sink.

## 3. Requirements and acceptance criteria

1. Any scene can produce a validated acoustic world from 3D meshes, a 2D proxy or explicit volumes, within bounded limits and work budgets. Invalid input returns coded errors and is never silently accepted.
2. Sound is occluded and transmitted per band through geometry, reflects (early reflections) and reverberates (late field) according to geometry and materials, with distance, air-absorption and Doppler effects.
3. Ray-traced propagation runs on the GPU with hardware ray queries when available, otherwise on GPU compute, otherwise on the CPU pipeline. The status reports the active tier and the exact fallback reason.
4. The audio callback never blocks on propagation, and parameter changes are click-free.
5. Behaviour is configurable through authored components (MCP `component_set`), MCP inspect/query/bake tools, native Rust APIs (backends, parameter hooks, DSP effects) and engine events.
6. Measurable tests (§8) pass, and GPU claims are only made from tests run on real hardware.

## 4. Decision record

### 4.1 Alternatives compared (same requirements)

| Option | Verdict | Reason |
| --- | --- | --- |
| Keep the current pan and attenuation, adding only a low-pass "occlusion" | Rejected | Has no geometry or reverb, so it does not meet the request. |
| Steam Audio / Resonance / Wwise-style middleware via FFI | Rejected | Needs prebuilt native SDKs (violates Cargo-only) and bundles datasets. |
| Pure-Rust audio crates (kira spatial, fundsp, `hrtf` + SOFA, rustfft/realfft) | Deferred | None is in the local registry, so adding one needs a download and user permission. `hrtf` needs a measured dataset. In-house replacements are small. Revisit if permission is granted (§10). |
| parry3d (already in `Cargo.lock` via rapier3d) for CPU ray casts | Rejected for the core | Its BVH cannot be uploaded as the GPU layout. One flat BVH shared by CPU and WGSL keeps the CPU/GPU parity tests meaningful and avoids coupling to the physics version. |
| Wave-based simulation (FDTD/ARD) or precomputed probe grids | Rejected for v1 | Heavy bakes and memory, and poor fit for dynamic and 2D-proxy worlds. Revisit for low-frequency accuracy needs. |
| Raw Vulkan via `ash` for ray tracing (as engine-dlss uses for NGX) | Rejected | Large `unsafe` surface and duplicate device management. wgpu 30 already exposes BLAS/TLAS/ray query. |
| **Geometric hybrid (ISM early + stochastic ray-traced late) with parametric FDN or convolution rendering, the same algorithm on CPU and GPU** | **Selected** | This is the standard hybrid method (Vorländer, *Auralization*, 2008). It scales from CPU to GPU by ray budget, works for both 3D and proxy worlds, and gives testable physics (Sabine/Eyring, image-source counts). |

### 4.2 Tradeoffs and revisit conditions

- Geometric acoustics has no edge diffraction. Volumetric occlusion gives soft shadow edges but no sound around corners. Revisit (UTD/BTM edges or portal graphs) if occlusion over corridors is reported as wrong.
- The analytic HRTF is non-individualized: front/back confusion is likely and elevation cues are weak. Revisit with a measured SOFA dataset if the user permits the download.
- Listener-centric late reverb (one FDN for all sources) is approximate for sources in another room. Per-pair "hero" convolution covers up to 4 important sources.
- The mixer keeps its blocking control mutex (existing behaviour). Revisit with a lock-free command queue if underruns are measured.

## 5. Acoustic world ("v_world") and the modern CPU pipeline

### 5.1 Data model (engine-acoustics, data-oriented)

- **Bands:** `BANDS = 4` with centres 125, 500, 2000 and 8000 Hz and crossovers 250, 1000 and 4000 Hz (geometric means). These are defined once in `engine_audio::acoustic` and re-exported.
- **Materials:** `AcousticMaterial { absorption: [f32; 4], scattering: f32, transmission: [f32; 4] }`. All values are energy coefficients in [0, 1], and each band must satisfy `transmission[b] ≤ absorption[b]` (transmitted energy is part of the non-reflected energy). Reflected energy is `1 − absorption`. Transmission applies to occluded direct paths only in v1. Presets live in a data table: `default, concrete, brick, plaster, wood, glass, metal, carpet, curtain, grass, water, absorber (α=1), reflector (α=0, tests)`. Each preset row cites its published source in code (implementer duty), and no preset ships without a citation.
- **Meshes:** `positions: Vec<[f32;3]>`, `triangles: Vec<[u32;3]>`, per-triangle `MaterialId(u16)`. Meshes are double-sided (rays never cull); normals face the incoming ray.
- **Instances:** `(MeshId, 3×4 row-major affine, Mobility { Static, Dynamic }, mask: u8)`. Static instances never move after build. Dynamic instances update their transform each tick and trigger an instance-level refit (CPU) or a TLAS rebuild (GPU).
- **BVH:** per mesh, a binned SAH build into a flat array of 32-byte nodes (layout frozen in §7.2), plus a top-level BVH over instance AABBs. Builds are deterministic: stable partitioning with ties broken by triangle index.
- **Plane clusters:** coplanar triangles of static meshes are merged per (quantized plane, material) at build time and drive the image-source method (§5.4.3).
- **Revision:** `u64`, incremented on every rebuild. Results carry the revision they were computed against.

### 5.2 Geometry sources

**(a) Authored and imported 3D meshes.** Each entity with `MeshRenderer` (or `GeometryOverride`'s undeformed base mesh) and a world transform contributes when `AcousticEnvironment.geometry.render_meshes` is true (default) and it is not marked `engine.AcousticGeometry { mode: "exclude" }`. `AcousticGeometry.mesh` can substitute a simplified acoustic mesh asset, which is recommended for dense render meshes. Degenerate triangles (area < 1e-10 m²) are dropped and counted in `WorldStats.dropped_degenerate`, never silently. Skinned and deforming geometry uses the base mesh in v1 (non-goal: deforming acoustics).

**(b) 2D→3D proxy synthesizer.** `AcousticEnvironment.proxy2d` turns 2D footprints into a 3D proxy world in acoustic metres:

- Footprints: `Collider2D` (non-trigger), `PhysicsBody2D` (non-sensor; box, ball as a 12-gon, capsule as two semicircle 6-gons plus a rectangle, rotation from the transform) and `AcousticVolume { space: "proxy2d" }`. The input API is a generic `Footprint2D` list, so a future tilemap only needs to emit footprints (greedy rectangle merging of solid cells is recommended to bound triangles).
- `plane: "side_view"` (platformer): the 2D point (x, y) maps to acoustic (x·s, y·s, z), where `s = 1 / units_per_meter`. Each footprint is extruded over z ∈ [−depth/2, +depth/2]. Optional front and back planes at ±depth/2 (`enclose_depth`, `side_material`) close the slice. Sources and the listener sit at z = 0.
- `plane: "top_down"`: (x, y) maps to (x·s, h, −y·s). Footprints are extruded over [0, wall_height]. There is a floor at 0, a ceiling at `wall_height` unless `open_sky`, and sources and the listener sit at `ear_height`. 2D "up" (+y) maps to acoustic forward (−Z).
- Enclosure: `"none" | "auto"` (level AABB of the footprints plus `margin_m`) or an explicit rectangle. Bounding faces receive `floor_material`/`ceiling_material`/`material`. `open_sky: true` removes the top (side view) or ceiling (top-down) face, so rays escape and the reverb falls.
- "Proportions": `units_per_meter` (0.001–1000) is the only scale knob, giving world units per acoustic metre. Guidance: set it so a character's authored height maps to about 1.8 m.
- The output is `ProxyMapping` (`to_acoustic(point)` and `listener_to_acoustic(pose)`). When a proxy is active, all propagation runs in proxy space and the mixer receives listener-frame directions from propagation (§7.3), so 2D panning is consistent with the proxy.

**(c) Explicit acoustic-only volumes.** `engine.AcousticVolume { shape: "box" (half_extents) | "mesh" (asset), kind: "solid" | "room", open_faces, space: "world" | "proxy2d", mobility }` uses the entity transform and its `engine.AcousticMaterial`. `room` creates an inward box shell with optional open faces. A `room` volume also supplies an analytic Eyring RT60 (§5.4.5) when the environment's reverb source is `"volumes"`, which costs no tracing.

### 5.3 Validation and limits (initial; Stream B may tighten them with measurements, and only root may loosen them)

| Limit | Value |
| --- | --- |
| Triangles per mesh / total static / total dynamic | 262,144 / 524,288 / 65,536 |
| Instances total / dynamic | 16,384 / 1,024 |
| Materials per world | 1,024 (`u16` ids) |
| Volumes / proxy footprints | 4,096 / 8,192 (polygons ≤ 32 vertices, convex) |
| Coordinate magnitude | ≤ 100,000 acoustic metres, all finite |
| BVH build work budget | 64 M operations, then `acoustics.world.budget` |
| Plane clusters | 65,536 total; ≤ 64 discovered per source for image sources |
| Request | ≤ 256 sources submitted, ≤ 64 traced per update (ranked by `priority / distance`) |
| Rays per job | CPU ≤ 65,536; GPU ≤ 1,048,576; bounces ≤ 256 |
| Histogram | bin 0.5–10 ms (default 1 ms), length ≤ 4 s (default 2 s) |
| Reflection taps / convolution voices | ≤ 16 per voice (default 8) / ≤ 4 |
| Events per tick | ≤ 32 acoustics events |

Error codes are prefixed `acoustics.` (`world.invalid`, `world.limit`, `world.budget`, `material.invalid`, `proxy.invalid`, `request.invalid`, `backend.unavailable`, `backend.failed`, `backend.timeout`, `unimplemented`).

### 5.4 Modern CPU pipeline

The propagation worker (§7.4) computes the direct path, early reflections and late reverb. The DSP in engine-audio renders them.

#### 5.4.1 Direct path

- **Distance attenuation:** unchanged. The existing `Attenuation` law runs in mixer world units.
- **Propagation delay and Doppler (mixer):** the mixer computes delay = distance · metres_per_unit / c from the per-tick positions it already receives through `configure`. It reads from a per-voice fractional delay line (cubic Hermite interpolation). The delay target is ramped linearly across each block, so constant velocity produces a constant Doppler shift, f' = f·c/(c − v_radial), for source and listener motion combined. `doppler` (0–4) scales the delay rate relative to its value at play time. The buffer capacity comes from `max_delay_s` (default 0.5 s, about 171 m) and is allocated when the voice is created on the control thread.
- **Air absorption (mixer):** per band, gain = exp(−m_b · d_m), with m_b in Np/m from ISO 9613-1 (environment temperature and humidity at the band centres) or explicit dB/m values.
- **Volumetric occlusion and transmission (worker):** cast `occlusion_rays` (default 16) from the listener to deterministic Fibonacci-sphere points on a sphere of radius `radius_m` around the source. The visible fraction is v. Blocked rays multiply the transmission of up to 4 crossed surfaces. The direct band gain is `v + (1 − v) · mean(T)`, as energy, and is applied as an amplitude after a square root.
- **Spatializer (mixer):**
  - `StereoPan` (default for speakers) is the existing equal-power law.
  - `Binaural` is the Brown & Duda (1998) structural model with no dataset. ITD follows Woodworth, Δt = (a/c)(θ + sin θ), with a = 0.0875 m and implemented as fractional per-ear delays. Head shadow is a one-pole/one-zero filter per ear with α(θ) = (1 + α_min/2) + (1 − α_min/2)·cos(θ/θ_min · 180°), α_min = 0.1, θ_min = 150°. An optional elevation cue uses Brown–Duda pinna echoes (`pinna_echoes`, default on).

#### 5.4.2 Band filtering

`BandFilter` applies four band gains. Its contract is ±1 dB of the requested gain at the band centres and ±0.5 dB flat sum at unity, measured on rendered noise. The implementation is free; an LR4 crossover tree or a shelf/peak cascade are both acceptable. It is reused for occlusion, air absorption and reflections.

#### 5.4.3 Early reflections: ray-guided image sources (worker)

1. Cast `er_rays` (default 256) specularly from the source. Record the plane clusters hit within `reflection_order` (default 2; ≤ 3 on CPU) bounces. Keep at most 64 planes.
2. Enumerate plane sequences up to that order without consecutive repeats. Build exact image sources and validate each path by segment raycasts: each reflection point must hit the expected cluster, and the final segment must be unobstructed.
3. Each path's gain per band is Π(1 − α)(1 − s) per reflection × 1/length (amplitude). Output the delay relative to the direct path and the listener-frame arrival direction.
4. Keep the `max_reflections` strongest paths (default 8). Sort deterministically by (delay, plane sequence).

For a shoebox this method yields exactly 4n² + 2 image sources of order n (6 for n = 1 and 18 for n = 2), which becomes a test.

The mixer renders each tap from the voice delay line at its delay, panned or ITD-spatialized individually. The summed early reflections pass through one `BandFilter` with energy-weighted mean band gains, a documented approximation.

#### 5.4.4 Late field: stochastic ray tracing (the energy model shared by CPU and GPU)

- Emit `rays` directions uniformly on the sphere from the emitter, each with per-band energy 1/rays. The RNG is the PCG hash on wrapping `u32`: `s = v*747796405 + 2891336453; w = ((s >> ((s >> 28) + 4)) ^ s) * 277803737; out = (w >> 22) ^ w`. It is seeded per (seed, job, ray), and the code is identical on the CPU and in WGSL.
- At each hit, e_b *= 1 − α_b. With probability `scattering`, the next direction is cosine-weighted about the normal facing the ray; otherwise it is the specular reflection. Each segment applies e_b *= exp(−2 m_b · ℓ) (as built: `air_np_per_m` is an amplitude coefficient, so energy decays at twice the rate; see §11). The ray terminates after `max_bounces` or when max_b e_b < 1e-6 (−60 dB). It never uses Russian roulette, which keeps it deterministic.
- **Receiver:** a sphere of radius `receiver_radius_m` (default 1.0). For each segment crossing it after at least one reflection, deposit e_b · 4π · ℓ_in / V_rcv into the bin at time (path length to entry + ℓ_in/2)/c. This normalizes energy relative to the direct sound at 1 m. Direct (zero-bounce) energy is excluded because the mixer renders it.
- Also track `escaped` energy (rays with no hit, for open-sky detection) and the mean segment length (≈ 4V/S, the mean free path).
- The listener-centric late field uses a job from the listener to itself (reciprocity). Per-source pair jobs are traced only for hero sources.

#### 5.4.5 Fitting and analytic estimates

- Schroeder backward integration runs per band. RT60 is taken from T30 (−5 to −35 dB), falling back to T20 and then EDT. The fit also produces the late level (energy after `late_start_s`, default 80 ms) and the predelay. The output is `ReverbParams` with `rt60`, `level`, `predelay_s`, `mean_free_path_m` and `escaped`.
- Analytic estimates are available for volumes and tests. Sabine: T = 0.161·V/(Σ S_i α_i + 8mV). Eyring: T = 0.161·V/(−S·ln(1 − ᾱ) + 8mV) (with the amplitude coefficient m in Np/m, the energy coefficient is 2m, so the air term is 4·(2m)·V = 8mV; the first draft's 4mV omitted the factor 2).

#### 5.4.6 Late reverb rendering (mixer)

- **Parametric (default):** an 8-line FDN (Householder feedback) whose delay lengths spread around the mean free path divided by c, with mutually prime sample counts. Per-line band decay is g_b = 10^(−3·L/(RT60_b·fs)). There are orthogonal stereo output taps and an output EQ set from `level`. Each voice feeds it with a scalar `reverb_send`. Parameter changes cross-fade over about 50 ms.
- **Convolution (optional):** the worker synthesizes a stereo IR from the histogram (per-band deterministic noise, band-filtered and shaped by sqrt of bin energy, with independent noise per ear) and prepares the frequency-domain partitions off-thread (`ConvolutionKernel::prepare`). The mixer runs uniform partitioned FFT convolution with internal block `CONVOLUTION_BLOCK = 256` (adds 256 samples of latency to the wet path), using an in-house radix-2 real FFT and a one-block cross-fade on kernel swaps. It is used for the listener-centric field and for up to 4 hero voices.

#### 5.4.7 Rates and budgets (estimates until Stream B measures them)

- Occlusion and early reflections update at `update_hz` (default 15).
- The late field updates at `reverb_update_hz` (default 4).
- CPU defaults: 4,096 reverb rays × 64 bounces.
- In threaded mode, the time budget (default 4 ms per update) adapts the ray count downward and reports `budget_overruns`. In synchronous mode nothing adapts.

## 6. GPU ray-traced propagation

### 6.1 Verdict and evidence

**Source evidence (local registry, wgpu 30.0.1, wgpu-types 30.0.1, wgpu-hal 30.0.1, naga 30.0.1):**

- `wgpu-types/src/features.rs`: `EXPERIMENTAL_RAY_QUERY = 1 << 32` ("Allows for the creation of ray-tracing queries within shaders", native-only). It belongs to `all_experimental_mask()`. BLAS vertex format is `Float32x3` when ray query is enabled.
- `wgpu-types/src/tokens.rs`: experimental features require `DeviceDescriptor.experimental_features = unsafe { ExperimentalFeatures::enabled() }`. The `# Safety` text says the user acknowledges "there may be UB-containing bugs in these apis". This conflicts with the workspace `unsafe_code = "forbid"`, so it needs a crate-level exception (§10, question 2).
- `wgpu/src/api/device.rs`: `create_blas(&CreateBlasDescriptor, BlasGeometrySizeDescriptors)` and `create_tlas(&CreateTlasDescriptor)`. `command_encoder.rs`: `build_acceleration_structures(blas_entries, tlases)`. `tlas.rs`: `IndexMut<usize>` for `TlasInstance::new(&blas, [f32;12], custom_data, mask)` and `as_binding()`. `BufferUsages::BLAS_INPUT`. `Limits::using_minimum_supported_acceleration_structure_values()`.
- naga WGSL: `enable wgpu_ray_query;`, the types `acceleration_structure` and `ray_query`, and the functions `rayQueryInitialize(&rq, acc, RayDesc(flags, cull_mask, tmin, tmax, origin, dir))`, `rayQueryProceed` and `rayQueryGetCommittedIntersection`. `RayIntersection` has `kind, t, instance_custom_data, instance_index, sbt_record_offset, geometry_index, primitive_index, barycentrics, front_face, object_to_world, world_to_object`.
- Backends in wgpu-hal:
  - Vulkan: `VK_KHR_ray_query` plus acceleration structures (`vulkan/adapter.rs` lines 955–975).
  - DX12: needs raytracing tier 1.1 **and shader model ≥ 6.5** (`dx12/adapter.rs` lines 610–620). With the default `Dx12Compiler::Auto` and no `dxcompiler.dll` or `static-dxc` feature, wgpu falls back to FXC, which caps at SM 5.1.
  - Metal: `supports_raytracing`.

**Hardware evidence (probe built offline in a scratch crate outside the repo and then deleted; Windows 11, RTX 5080, NVIDIA driver 617.14):**

- Vulkan adapter: `ray_query=true rt_pipelines=true vertex_return=true as_binding_array=true`. Limits: BLAS primitives 536,870,911, TLAS instances 16,777,215.
- Functional test: one quad BLAS and a one-instance TLAS, with 64 rays from a WGSL compute shader using `rayQueryProceed`. Result: **40 hits, all at t = 5.0 with triangle kind, and 24 misses**, which matches the analytic count. No validation errors.
- Throughput test: 2^20 rays × 32 specular bounces in a 10×6×8 m shoebox (12 triangles) gave **33.5 M hit segments in 1.94 ms** (submit + wait, best of 5), about 17 G segments/s for this trivial scene. Mean free path was 5.130 m against 4V/S = 5.106 m (0.5%). This upper bound does not predict real scenes.
- DX12 adapter on the same GPU: `ray_query=false`, consistent with the FXC/SM 5.1 path above.

**Conclusion:** the hardware ray-query tier is feasible and verified only for wgpu 30.0.1 + Vulkan + RTX 5080 on Windows. Linux Vulkan is expected to behave the same but is unverified. Metal is unverified. v1 enables the ray-query tier **only on Vulkan adapters**. Other backends use the compute tier and report the reason `ray query not validated on <backend>`. DX12 hardware ray tracing would require DXC (an external DLL or a new download), which the Cargo-only rule excludes.

### 6.2 Tiers and fallback (explicit, no fake success)

| Tier | `BackendKind` | Hardware RT | Requirements |
| --- | --- | --- | --- |
| 1 | `GpuRayQuery` | yes | Vulkan adapter with `EXPERIMENTAL_RAY_QUERY`, experimental token, self-test passed |
| 2 | `GpuCompute` | no ("GPU-accelerated BVH traversal, not hardware ray tracing") | Any wgpu primary-backend adapter with compute and storage buffers, self-test passed |
| 3 | `Cpu` | no | Always available |

- `BackendPreference::{Auto, Cpu, GpuCompute, GpuRayQuery}`. `Auto` (default) tries 1 → 2 → 3. An explicit preference tries that tier and then the lower tiers. `Cpu` never creates a wgpu instance.
- **Self-test before a tier is marked available:** the probe's quad test (exact hit count and t) plus a 4,096-ray shoebox whose mean free path is within 2% of 4V/S. Any failure records the failing step in `fallback_reason`.
- Runtime failures:
  - A device-lost error, a validation error or a non-finite result is **fatal** and demotes immediately.
  - A readback timeout (default 100 ms) is non-fatal, but three consecutive timeouts demote.
  - Demotion emits `acoustics.backend_changed`. The CPU tier continues with the last good parameters, so audio only experiences parameter smoothing.
  - There is no automatic promotion. `AcousticControl.retry_gpu` or a scene reload re-runs the probes.
- `AcousticState::{Disabled, Starting, Active, Fallback, Failed}`. `Active` means the requested tier is running and has produced a validated result. `Fallback` means a lower tier is active, with a reason. `Failed` means no tier works, for example because the world is invalid: the mixer is reset to dry (neutral) parameters and the error is reported.

### 6.3 GPU design (engine-acoustics-gpu)

- **Own device:** the crate creates its own `wgpu::Instance` (Vulkan for tier 1, primary backends for tier 2) and requests a HighPerformance adapter, optionally filtered by `adapter_name_contains`. It is independent of the renderer's device: no shared resources are needed, it works headless, and it keeps engine-desktop untouched until integration. Cost: a second device on the same GPU. GPU time is budgeted (§8 measures the effect on render frame time).
- **Exactly one `#[allow(unsafe_code)]` function** creates `ExperimentalFeatures::enabled()`, with a `// SAFETY:` comment citing wgpu's contract. Like engine-dlss, the crate opts out of the workspace lint table, but it sets `unsafe_code = "deny"` rather than leaving unsafe unrestricted. Tier 2 needs no unsafe code.
- **Upload:**
  - One BLAS per `AcousticMesh`: `Float32x3` positions, `Uint32` indices, `OPAQUE`, `PREFER_FAST_TRACE`.
  - One TLAS with `TlasInstance::new(&blas, transform 3x4 row-major, custom_data = instance index (24 bits), mask)`. Dynamic transforms rewrite `tlas[i]` and rebuild the TLAS each update.
  - Storage buffers: `GpuTriangle` (positions plus material, for normals and materials without relying on `EXPERIMENTAL_RAY_HIT_VERTEX_RETURN`), `GpuInstance` (triangle offset, normal matrix) and `GpuMaterial`. Tier 2 also uploads the flat `BvhNode` arrays and the instance-level BVH, the same buffers the CPU uses.
- **Kernel:** one WGSL module is composed from `common.wgsl` (RNG, sampling, energy model, receiver, histogram deposit) plus either `intersect_rayquery.wgsl` or `intersect_bvh.wgsl` (fixed 64-entry stack; an iteration cap counts `aborted` rays and reports them). One invocation handles one ray, and jobs are batched in one dispatch (≤ 64 jobs). The histogram is `array<atomic<u32>>` [job][bin][band], fixed point with full scale 2^31 per unit of total ray energy, using integer atomics. That makes the sums order-independent and **deterministic for a fixed seed**. Saturation is detected and reported as an error.
- **Readback:** `map_async` plus `poll(PollType::Wait { timeout })` on the worker thread only. Output histograms use the same `EnergyHistogram` type as the CPU, so fitting, IR synthesis and DSP are shared.
- **Accepted deviations (stream C, approved by root; measured details in [the crate README](../crates/engine-acoustics-gpu/README.md)):**
  - **Histogram accumulation:** each cell and per-job counter is a 64-bit fixed-point value held in two `atomic<u32>` words with carry, at 2^40 counts per unit of energy (and 2^16 per metre of path), instead of one `u32` at full scale 2^31. The single word loses the −35 dB tail at 1M rays. Integer addition stays order-independent, so a fixed seed is still bitwise reproducible on one device and driver; saturation of the high word is detected and reported as `acoustics.backend.failed`.
  - **BLAS upload:** one non-indexed `Float32x3` triangle list per distinct mesh, in the mesh's triangle order, instead of the indexed upload above. This keeps `primitive_index` equal to the mesh-relative triangle, so normals and materials still come from the `GpuTriangle` buffer without vertex return.
  - **`GraphicsApi` selector:** `GpuBackend::with_api(GraphicsApi::{Primary, Vulkan, Dx12, Metal})` chooses which wgpu backends a tier may open (`Primary` by default; tier 1 still requires Vulkan). It exists so the forced-DX12 fallback reason of test 19 is real evidence, and `select_backend(preference, options, api, cpu)` reuses the service's tier order and reason format.
  - **Test 20 status:** measured at library level only. A separate wgpu device rendering 1080p frames while the ray-query tier traces gave p95 8.76 ms without the tracer, 8.40 ms with the default tracer (4,096 rays × 64 bounces at 4 Hz; within noise, median +0.10 ms) and 13.28 ms with a stress tracer (65,536 rays × 256 bounces back to back). The `engine-desktop` test `the_acoustics_device_coexists_with_a_live_renderer_device` (ignored, run on the RTX 5080) shows both devices live in one process and both working. The 95th-percentile frame time of the real windowed desktop renderer with the tracer at defaults has **not** been measured, so the "≤ 1 ms" criterion is unconfirmed there; the CPU tier can be forced with `--acoustics-backend cpu`.
- **Not on the GPU in v1:** occlusion rays and early reflections stay on the CPU. They are few rays, sensitive to latency, and a GPU readback would add a frame of latency. Revisit when more than 64 traced sources are needed.

## 7. Interfaces

### 7.1 Crate layout and dependency direction

```text
engine-audio (DSP, mixer, acoustic parameter types; no geometry, no ECS)
   ^
engine-acoustics (world, BVH, proxy2d, materials, CPU tracer, fitting, IR synthesis, service)
   ^                         ^
engine-acoustics-gpu (wgpu tiers 1 and 2; implements PropagationBackend)
   ^                         ^
engine-project (components, collection, sync, events, status; CPU service by default)
   ^
engine-desktop (host: injects GPU backends)   engine-mcp / engine-tooling (tools, telemetry)
```

engine-project does not depend on wgpu. The host injects GPU backends as `Box<dyn PropagationBackend>`.

### 7.2 Frozen contracts (Stream 0 lands these first; changes need root approval)

`engine_audio::acoustic` (new module `crates/engine-audio/src/acoustic.rs`):

```rust
pub const BANDS: usize = 4;
pub const BAND_CENTERS_HZ: [f32; BANDS] = [125.0, 500.0, 2000.0, 8000.0];
pub const BAND_EDGES_HZ: [f32; BANDS - 1] = [250.0, 1000.0, 4000.0];
pub type BandGains = [f32; BANDS];
pub const MAX_REFLECTION_TAPS: usize = 16;
pub const MAX_ACOUSTIC_VOICES: usize = 256;
pub const MAX_CONVOLUTION_VOICES: usize = 4;
pub const CONVOLUTION_BLOCK: usize = 256;

/// New field `PlayOptions::acoustics`. Default (enabled: false) is the legacy path, bit-identical output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceAcoustics {
    pub enabled: bool,
    pub propagation_delay: bool, // delay line: latency + Doppler (default true)
    pub doppler: f32,            // 0..=4, default 1; requires propagation_delay
    pub max_delay_s: f32,        // 0.01..=2.0, default 0.5
    pub air_absorption: bool,    // default true
    pub metres_per_unit: f32,    // 0.001..=1000, default 1
    pub reverb_send: f32,        // 0..=1, default 1
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpatialRenderer { #[default] StereoPan, Binaural }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReverbRenderer { Off, #[default] Parametric, Convolution }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BinauralSettings { pub head_radius_m: f32, pub pinna_echoes: bool }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerEnvironment {
    pub renderer: SpatialRenderer,
    pub reverb: ReverbRenderer,
    pub binaural: BinauralSettings,
    pub speed_of_sound: f32,        // 300..=400 m/s, default 343
    pub air_np_per_m: BandGains,    // >= 0
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReflectionTap { pub delay_s: f32, pub gains: BandGains, pub direction: [f32; 3] } // listener frame
pub struct VoicePropagation { // no Clone: owns an optional kernel box
    pub voice: VoiceHandle,
    pub direct: BandGains,                 // occlusion x transmission (amplitude)
    pub direction: Option<[f32; 3]>,       // listener-frame override (proxy space); None = mixer geometry
    pub taps: [ReflectionTap; MAX_REFLECTION_TAPS],
    pub tap_count: u8,
    pub reverb_send: f32,
    pub kernel: Option<Box<ConvolutionKernel>>, // hero voices only
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReverbParams {
    pub enabled: bool, pub rt60_s: BandGains, pub level: BandGains,
    pub predelay_s: f32, pub mean_free_path_m: f32,
}
/// One published parameter set. Capacities are reserved at construction; the audio thread never allocates or frees one.
pub struct AcousticFrame {
    pub sequence: u64,
    pub voices: Vec<VoicePropagation>,     // sorted by voice, len <= MAX_ACOUSTIC_VOICES
    pub reverb: ReverbParams,
    pub listener_kernel: Option<Box<ConvolutionKernel>>,
}
pub struct StereoImpulseResponse { pub sample_rate: u32, pub left: Vec<f32>, pub right: Vec<f32> } // <= 4 s
pub struct ConvolutionKernel { /* frequency-domain partitions for CONVOLUTION_BLOCK */ }
impl ConvolutionKernel { pub fn prepare(ir: &StereoImpulseResponse, output_rate: u32) -> Result<Self>; }

/// Single writer, obtained once from `AudioControl::acoustic_publisher()` (a second call is an error).
pub struct AcousticPublisher { /* Arc<ParamSlot>, staging Box<AcousticFrame> */ }
impl AcousticPublisher {
    /// Clears and fills the staging frame, then swaps it into the slot under a short lock.
    pub fn publish(&mut self, fill: impl FnOnce(&mut AcousticFrame) -> Result<()>) -> Result<u64>;
    pub fn stats(&self) -> PublisherStats; // published, consumed, audio_try_lock_misses
    pub fn reset_to_dry(&mut self) -> Result<u64>;
}

/// Native per-bus effect. `process` runs on the audio thread: it must not block or allocate.
pub trait DspEffect: Send + 'static {
    fn process(&mut self, frames: &mut [[f32; 2]], context: &EffectContext);
    fn reset(&mut self) {}
}
pub struct EffectContext { pub sample_rate: u32, pub bus: Bus, pub frame_cursor: u64 }
// New AudioControl methods:
//   set_environment(MixerEnvironment) -> Result<()>
//   set_bus_effect(Bus, Option<Box<dyn DspEffect>>) -> Result<Option<Box<dyn DspEffect>>>  (old effect is returned, dropped by the caller)
//   acoustic_publisher() -> Result<AcousticPublisher>
```

**`ParamSlot` protocol** (std-only, no `unsafe`): the slot is `Mutex<Box<AcousticFrame>>` plus an `AtomicU64` generation.

- The writer fills staging, locks, swaps staging with the slot, increments the generation and unlocks. Staging now holds an older frame, which it reuses.
- At the start of `mix`, if the generation changed, the audio thread calls `try_lock()`. On success it performs two swaps (slot ↔ fading, then active ↔ fading): the new frame becomes active, the previous one stays for cross-fades, and the oldest returns to the writer. Old kernels are therefore freed on the worker, never on the audio thread. On `WouldBlock` it keeps the current parameters and increments `audio_try_lock_misses`.

`engine_acoustics` (new crate, `crates/engine-acoustics`), public surface:

```rust
pub use engine_audio::acoustic::{BANDS, BandGains};
pub struct AcousticMaterial { pub absorption: BandGains, pub scattering: f32, pub transmission: BandGains }
impl AcousticMaterial { pub fn validate(&self) -> Result<()>; pub fn preset(p: MaterialPreset) -> Self; }
pub enum MaterialPreset { Default, Concrete, Brick, Plaster, Wood, Glass, Metal, Carpet, Curtain, Grass, Water, Absorber, Reflector }
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct MaterialId(pub u16);
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct MeshId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct InstanceId(pub u32);
pub enum Mobility { Static, Dynamic }
pub type Affine3 = [f32; 12]; // row-major 3x4, same layout as wgpu::TlasInstance::transform
pub struct AcousticMesh { pub positions: Vec<[f32; 3]>, pub triangles: Vec<[u32; 3]>, pub materials: Vec<MaterialId> /* len 1 (uniform) or triangles.len() */ }
pub enum Footprint2D { Rect { center: [f32; 2], half_extents: [f32; 2], rotation: f32 }, Circle { center: [f32; 2], radius: f32 },
                       Capsule { center: [f32; 2], half_height: f32, radius: f32, rotation: f32 }, Polygon { points: Vec<[f32; 2]> } }
pub struct Footprint { pub shape: Footprint2D, pub material: MaterialId, pub mobility: Mobility }
pub enum ProxyPlane { SideView, TopDown }
pub enum Enclosure { None, Auto { margin_m: f32 }, Bounds { min: [f32; 2], max: [f32; 2] } }
pub struct Proxy2DSettings { pub plane: ProxyPlane, pub units_per_meter: f32, pub depth_m: f32, pub enclose_depth: bool,
    pub wall_height_m: f32, pub ear_height_m: f32, pub floor: bool, pub ceiling: bool, pub open_sky: bool, pub enclosure: Enclosure,
    pub material: MaterialId, pub floor_material: MaterialId, pub ceiling_material: MaterialId, pub side_material: MaterialId }
pub struct ProxyMapping { /* scale, plane, heights */ }
impl ProxyMapping { pub fn to_acoustic(&self, world: [f32; 3]) -> [f32; 3]; pub fn pose_to_acoustic(&self, p: ListenerPose) -> ListenerPose; pub fn metres_per_unit(&self) -> f32; }
pub enum VolumeShape { Box { half_extents: [f32; 3] }, Mesh(MeshId) }
pub enum VolumeKind { Solid, Room { open_faces: u8 /* bit per face: -X,+X,-Y,+Y,-Z,+Z */ } }
pub struct AcousticVolume { pub shape: VolumeShape, pub kind: VolumeKind, pub material: MaterialId, pub mobility: Mobility }
pub struct WorldLimits { /* §5.3 values; Default = documented limits */ }
pub struct AcousticWorldBuilder { /* … */ }
impl AcousticWorldBuilder {
    pub fn new(limits: WorldLimits) -> Self;
    pub fn add_material(&mut self, m: AcousticMaterial) -> Result<MaterialId>;
    pub fn add_mesh(&mut self, mesh: AcousticMesh) -> Result<MeshId>;
    pub fn add_instance(&mut self, mesh: MeshId, transform: Affine3, mobility: Mobility, mask: u8) -> Result<InstanceId>;
    pub fn add_volume(&mut self, v: AcousticVolume, transform: Affine3) -> Result<InstanceId>;
    pub fn add_proxy_2d(&mut self, footprints: &[Footprint], settings: &Proxy2DSettings) -> Result<ProxyMapping>;
    pub fn build(self) -> Result<AcousticWorld>;
}
pub struct AcousticWorld { /* SoA meshes, flat BVHs, instances, plane clusters, revision */ }
impl AcousticWorld {
    pub fn revision(&self) -> u64;
    pub fn stats(&self) -> WorldStats; // triangles, instances, materials, dropped_degenerate, bvh_nodes, plane_clusters, bounds
    pub fn set_instance_transform(&mut self, id: InstanceId, t: Affine3) -> Result<()>; // Dynamic only
    pub fn raycast(&self, origin: [f32; 3], dir: [f32; 3], t_max: f32, mask: u8) -> Option<Hit>;
    pub fn gpu_view(&self) -> GpuWorldView<'_>; // borrowed slices of the layouts below
}
pub struct Hit { pub t: f32, pub instance: InstanceId, pub triangle: u32, pub material: MaterialId, pub normal: [f32; 3] }

pub mod gpu_layout { // #[repr(C)], bytemuck Pod; identical for CPU traversal and WGSL
    pub struct BvhNode { pub min: [f32; 3], pub left_or_first: u32, pub max: [f32; 3], pub count: u32 } // 32 B; count == 0 is interior (children left, left+1)
    pub struct GpuTriangle { pub v0: [f32; 4], pub v1: [f32; 4], pub v2: [f32; 4] } // 48 B; v0.w = material id as f32 bits
    pub struct GpuInstance { pub world_from_object: [f32; 12], pub object_from_world: [f32; 12], pub node_offset: u32, pub triangle_offset: u32, pub mask: u32, pub _pad: u32 }
    pub struct GpuMaterial { pub reflect: [f32; 4] /* 1 - absorption */, pub transmission: [f32; 4], pub scattering: f32, pub _pad: [f32; 3] }
}

pub struct ListenerPose { pub position: [f32; 3], pub forward: [f32; 3], pub up: [f32; 3] } // acoustic space
pub struct SourceRequest { pub id: u64 /* VoiceHandle::get */, pub position: [f32; 3], pub radius_m: f32,
    pub occlusion: bool, pub reflections: bool, pub reverb: bool, pub hero: bool, pub priority: f32 }
pub struct PropagationSettings { pub occlusion_rays: u16, pub reflection_order: u8, pub max_reflections: u8, pub er_rays: u16,
    pub reverb_rays: u32, pub max_bounces: u16, pub histogram_bin_s: f32, pub histogram_length_s: f32, pub late_start_s: f32,
    pub receiver_radius_m: f32, pub speed_of_sound: f32, pub air_np_per_m: BandGains, pub seed: u64, pub time_budget_us: u32 }
pub struct PropagationRequest { pub sequence: u64, pub listener: ListenerPose, pub sources: Vec<SourceRequest>, pub settings: PropagationSettings }

/// What a backend traces: the stochastic energy model of §5.4.4.
pub struct EnergyJob { pub emitter: [f32; 3], pub receiver: [f32; 3], pub receiver_radius_m: f32, pub rays: u32, pub max_bounces: u16, pub seed: u64 }
pub struct TraceSettings { pub bin_s: f32, pub bins: u32, pub speed_of_sound: f32, pub air_np_per_m: BandGains }
pub struct EnergyHistogram { pub bin_s: f32, pub bins: Vec<BandGains>, pub rays: u32, pub escaped: f32, pub aborted: u32, pub mean_free_path_m: f32 }
pub struct TraceStats { pub rays: u64, pub segments: u64, pub elapsed_us: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)] pub enum BackendKind { Cpu, GpuCompute, GpuRayQuery }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)] pub enum BackendPreference { #[default] Auto, Cpu, GpuCompute, GpuRayQuery }
pub struct BackendInfo { pub kind: BackendKind, pub hardware_rt: bool, pub adapter: Option<String>, pub driver: Option<String> }
pub struct BackendError { pub code: &'static str, pub message: String, pub fatal: bool }

pub trait PropagationBackend: Send {
    fn info(&self) -> BackendInfo;
    /// Capability probe plus self-test (§6.2). Err means the tier is unavailable; the message becomes the fallback reason.
    fn initialize(&mut self) -> Result<(), BackendError>;
    /// Upload or refresh geometry. A changed revision means a full rebuild; dynamic transforms are an update.
    fn sync_world(&mut self, world: &AcousticWorld) -> Result<(), BackendError>;
    /// Runs on the propagation worker only. Fills `out[i]` for `jobs[i]`.
    fn trace(&mut self, world: &AcousticWorld, settings: &TraceSettings, jobs: &[EnergyJob], out: &mut Vec<EnergyHistogram>) -> Result<TraceStats, BackendError>;
}
pub struct CpuBackend { /* threads: 1..=8, deterministic chunking with ordered reduction */ }

pub struct DirectPath { pub visible_fraction: f32, pub gains: BandGains, pub distance_m: f32, pub direction: [f32; 3] }
pub struct ReflectionPath { pub order: u8, pub delay_s: f32, pub length_m: f32, pub gains: BandGains, pub direction: [f32; 3] }
pub struct SourceResult { pub id: u64, pub direct: DirectPath, pub reflections: Vec<ReflectionPath>, pub histogram: Option<EnergyHistogram> }
pub struct PropagationResult { pub sequence: u64, pub world_revision: u64, pub backend: BackendKind,
    pub listener_field: Option<EnergyHistogram>, pub reverb: ReverbParams, pub sources: Vec<SourceResult>, pub stats: TraceStats, pub budget_exceeded: bool }

pub fn sabine_rt60(volume_m3: f32, surfaces: &[(f32 /*area*/, BandGains /*absorption*/)], air_np_per_m: BandGains) -> BandGains;
pub fn eyring_rt60(volume_m3: f32, surfaces: &[(f32, BandGains)], air_np_per_m: BandGains) -> BandGains;
pub fn fit_reverb(h: &EnergyHistogram, late_start_s: f32) -> ReverbParams; // Schroeder; T30 > T20 > EDT
pub fn synthesize_ir(h: &EnergyHistogram, sample_rate: u32, seed: u64) -> StereoImpulseResponse;

pub enum ServiceMode { Threaded, Synchronous }
pub struct ServiceConfig { pub preference: BackendPreference, pub mode: ServiceMode, pub update_hz: f32, pub reverb_update_hz: f32,
    pub settings: PropagationSettings, pub backends: Vec<Box<dyn PropagationBackend>> /* host-supplied GPU tiers; CPU appended automatically */ }
pub type ParamHook = Box<dyn FnMut(&PropagationResult, &mut AcousticFrame) + Send>;
pub struct AcousticsService { /* worker thread, latest-wins request/world slots, publisher */ }
impl AcousticsService {
    pub fn new(config: ServiceConfig, publisher: AcousticPublisher) -> Result<Self>;
    pub fn set_world(&mut self, world: AcousticWorld);                         // non-blocking hand-off
    pub fn update_dynamic(&mut self, updates: &[(InstanceId, Affine3)]) -> Result<()>;
    pub fn submit(&mut self, request: PropagationRequest) -> Result<()>;       // latest wins; never waits for tracing
    pub fn latest(&mut self) -> Option<PropagationSummary>;                    // try_lock; None if busy or no news
    pub fn step_blocking(&mut self) -> Result<&PropagationResult>;              // Synchronous mode only
    pub fn status(&self) -> AcousticStatus;
    pub fn set_preference(&mut self, p: BackendPreference) -> Result<()>;      // re-probes
    pub fn set_param_hook(&mut self, hook: Option<ParamHook>);                 // runs on the worker
    pub fn shutdown(&mut self);                                                 // joins the worker, publishes dry
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AcousticStatus {
    pub requested: BackendPreference, pub active: Option<BackendKind>, pub state: AcousticState,
    pub fallback_reason: Option<String>, // <= 512 bytes, char-boundary truncated
    pub adapter: Option<String>, pub hardware_rt: bool,
    pub world: WorldStats, pub updates: u64, pub last_update_us: u32, pub max_update_us: u32, pub budget_overruns: u64,
    pub rays_last_update: u64, pub sources_traced: u32, pub sources_skipped: u32,
    pub publishes: u64, pub audio_try_lock_misses: u64, pub gpu_timeouts: u64, pub aborted_rays: u64,
    pub sources: Vec<SourceStatus>, // <= 32 entries: id, visible_fraction, occluded, reflections
}
```

`engine_acoustics_gpu` (new crate): `GpuBackend::ray_query(GpuOptions) -> GpuBackend`, `GpuBackend::compute(GpuOptions)`, `probe_capabilities() -> Vec<AdapterCapability>` (name, backend, ray_query flag, reason), and `GpuOptions { adapter_name_contains: Option<String>, readback_timeout_ms: u32, deny_ray_query: bool /* test hook */, force_self_test_failure: bool /* test hook, cfg(test)/doc-hidden */ }`.

### 7.3 Mixer integration (engine-audio)

- Per voice, when `acoustics.enabled`: the path is source → mono → delay line (Doppler and latency) → direct (distance × `direct` × air) → `BandFilter` → spatializer. Taps are read from the same delay line. The reverb send feeds the FDN or convolution bus. The bus and master effects run, then the output is clamped.
- When acoustics are disabled and there are no effects, the **legacy summation code path is retained unchanged**, which preserves bit-identical output and the existing exact tests.
- Gains ramp linearly over `PARAM_RAMP_FRAMES = 1024`. When `propagation_delay` is false, delays are slew-limited (|d delay/dt| ≤ 0.05) to avoid pitch artefacts.
- Voice DSP state (`Option<Box<VoiceDsp>>`) is allocated in `play`/`configure` on the control thread. Retired voice DSP boxes move to a preallocated `retired` list that control calls drain, so they are never freed in the callback.
- `max_acoustic_voices` (default 32): voices beyond the cap fall back to the legacy path, are counted and are reported.

### 7.4 Threading model

| Thread | Work | Blocking allowed |
| --- | --- | --- |
| Game (runtime tick) | `acoustics::synchronize`: collect dirty geometry, map poses, `submit`, read `latest`, emit events, write `AcousticControl.status` | Short locks only (no waiting on tracing) |
| Propagation worker (one per service) | World rebuild/refit, backend `sync_world`/`trace`, early reflections, fitting, IR synthesis, kernel preparation, `publish` | Yes (GPU poll with timeout) |
| Audio callback | `mix`: `try_lock` the parameter slot, DSP | Never waits on propagation (the existing mixer mutex behaviour is unchanged) |
| GPU queue | Acoustics device only | N/A |

In threaded mode the worker runs at `update_hz` and also wakes on submit. Synchronous mode (headless runs, tests, MCP) runs `step_blocking` in the caller's thread every `round(tick_hz / update_hz)` ticks, before that tick's `mix`.

### 7.5 Determinism

- CPU tracing is bitwise reproducible for a fixed seed on the same machine and binary: deterministic BVH, PCG RNG per (seed, job, ray), ordered reduction and no time-based adaptation in synchronous mode.
- GPU histograms are reproducible for a fixed seed because of integer atomics. CPU and GPU agree within tolerance (§8), not bitwise.
- Results are not promised identical across machines (floating-point and driver differences).

### 7.6 ECS integration (engine-project; new `src/acoustics.rs` with submodules)

Components are registered like `engine.PhysicsBody2D`: descriptors are chained in `ComponentCatalog::new` and adapters registered at runtime start. All are version 1, and JSON shapes are schema-validated plus typed `validate()`:

```jsonc
// engine.AcousticEnvironment: at most one per scene (validation error otherwise); no Transform required
{"enabled":true,"backend":"auto","renderer":"binaural","reverb":"parametric","reverb_source":"traced", // traced|volumes|off
 "update_hz":15,"reverb_update_hz":4,"speed_of_sound":343,"air":{"temperature_c":20,"humidity_percent":50}, // or {"absorption_db_per_m":[..4]} or null
 "quality":{"occlusion_rays":16,"reflection_order":2,"max_reflections":8,"er_rays":256,"reverb_rays":4096,"max_bounces":64,
            "histogram_length_s":2.0,"histogram_bin_ms":1.0,"late_start_ms":80,"receiver_radius_m":1.0,"time_budget_ms":4},
 "seed":1,"geometry":{"render_meshes":true,"asset":null,"default_material":"default","mask":255},
 "proxy2d":null /* or {"plane":"side_view","units_per_meter":1,"depth_m":8,"enclose_depth":true,"wall_height_m":3,"ear_height_m":1.6,
   "floor":true,"ceiling":true,"open_sky":false,"enclosure":"auto","margin_m":0,"colliders":true,"physics_bodies":true,
   "material":"default","floor_material":"wood","ceiling_material":"plaster","side_material":"plaster"} */}
// engine.AcousticMaterial (requires Transform): preset plus optional explicit overrides
{"preset":"concrete","absorption":[0.01,0.02,0.02,0.03],"scattering":0.1,"transmission":[0.002,0.001,0.0005,0.0002]}
// engine.AcousticGeometry (requires Transform)
{"mode":"static","mesh":null,"mask":1}            // static|dynamic|exclude
// engine.AcousticVolume (requires Transform)
{"shape":"box","half_extents":[5,3,4],"kind":"room","open_faces":["+y"],"space":"world","mobility":"static"}
// engine.AcousticSource (requires Transform): applied by audio::source/configure to voices owned by this entity
{"enabled":true,"occlusion":true,"reflections":true,"reverb_send":1,"doppler":1,"propagation_delay":true,
 "air_absorption":true,"radius_m":0.25,"priority":1,"hero":false}
```

- Merge rule: when the owner has `engine.AcousticSource`, its values set `PlayOptions.acoustics` for sources created or configured through `engine_project::audio`, so the component wins. Without it, native `PlayOptions.acoustics` is used as given.
- Collection (`acoustics/collect.rs`) reads `MeshRenderer` (or `AcousticGeometry.mesh`) × `WorldTransform`, `Collider2D`/`PhysicsBody2D` footprints, `AcousticVolume` and `AcousticMaterial`. Static changes trigger a debounced full rebuild (≥ 0.5 s) and emit `acoustics.world_rebuilt`. Dynamic transforms go through `update_dynamic` every tick. Scene replacement rebuilds.
- Sync (`acoustics/sync.rs`) runs after `audio::synchronize` in `Runtime::tick` and after scene loads. It builds `SourceRequest`s from `EntityAudio` attachments (`SourceHandle → VoiceHandle` via `AudioSources::voice`), maps poses through `ProxyMapping` when the proxy is active, and calls `submit` and `latest`.
- Resources:
  - `AcousticControl { override_backend: Option<BackendPreference>, retry_gpu: bool, paused: bool, rebuild_world: bool, status: AcousticStatus }`. Callbacks write the controls and the runtime writes `status`, following the `UpscalerControl` pattern.
  - `RuntimeAcoustics` holds the service (private).
- Host API: `Runtime::set_acoustics(backends: Vec<Box<dyn PropagationBackend>>, mode: ServiceMode) -> Result<()>`, called before start and after `set_audio`. When a scene has `AcousticEnvironment` and the host configured nothing, the runtime creates a CPU-only synchronous service, so headless and MCP runs work without a GPU.
- Native hooks: `engine_project::acoustics::set_param_hook(world, ParamHook)`, `register_event_handler` for the events below, `AudioControl::set_bus_effect` via `RuntimeAudio.control`, and a custom `PropagationBackend` through `set_acoustics`.
- Events (payloads ≤ 1 KiB, ≤ 32 per tick, delivered through the existing `EventBindings` and `register_event_handler`):
  - `acoustics.backend_changed {from, to, reason}`
  - `acoustics.occlusion_changed {source: entity, occluded, visible_fraction}`, with hysteresis: enter when visible < 0.3, exit when > 0.7
  - `acoustics.world_rebuilt {revision, triangles, dropped_degenerate}`
  - `acoustics.failed {code, message}`
- Snapshot: `RuntimeSnapshot.acoustics: Option<AcousticStatus>` (absent without an environment).
- Assets: a new manifest asset kind `"acoustics"` holds an `AcousticWorldDocument` (JSON ≤ 64 MiB: version, materials, meshes, instances, proxy settings, source scene and content hash). It stores source geometry, not a trusted BVH, and the BVH is rebuilt and validated at load (the NavMesh precedent). `geometry.asset` uses it instead of live static collection, while dynamic entities stay live.

### 7.7 MCP

- `acoustics_inspect` (read): `{scene?, include_entities?}` returns world stats, bounds, materials, proxy summary, warnings (for example "closed room escapes 12% of rays: geometry leak") and ≤ 256 contributing entities. It does not execute game code.
- `acoustics_query` (read, always CPU, deterministic): `{scene?, listener:{position,forward?,up?}, sources:[{position,radius_m?}] ≤16, quality?}` returns per source the visible fraction, direct gains in dB per band, distance, ≤ 16 reflection paths (order, delay in ms, gains in dB, direction) and the reverb (traced RT60 per band, mean free path, escaped fraction). It also returns Sabine and Eyring estimates when the listener is inside a `room` volume.
- `acoustics_bake` (write): `{scene?, output, include:{meshes,proxy2d,volumes}}` writes a new `AcousticWorldDocument`. It never overwrites and returns `{path, triangles, hash, asset}` for the caller to add to `project.json`, the same flow as `navigation_bake`.
- Materials and settings are edited through the existing `component_set` on the `engine.Acoustic*` components, with no new code. `component_schema` describes them through the catalog.
- `project_run_headless` gains an `acoustics` telemetry object (`RuntimeSnapshot.acoustics`, capped at 64 KiB, with a `telemetry_omitted` entry when oversized).
- Tool bodies are thin. The logic lives in `engine_project::acoustics::{inspect_scene, query_scene, bake_scene}` so native code and tests share it.

## 8. Verification plan (measurable acceptance)

Reference numbers: shoebox 10×6×8 m, V = 480 m³, S = 376 m², 4V/S = 5.106 m. With uniform α per band = [0.05, 0.1, 0.2, 0.4] and no air, Eyring RT60 = [4.007, 1.951, 0.921, 0.402] s and Sabine RT60 = [4.111, 2.055, 1.028, 0.514] s.

| # | Test (owner) | Pass criterion |
| --- | --- | --- |
| 1 | Legacy invariance (A) | All existing engine-audio tests pass unmodified. With acoustics disabled, output is bit-identical to a pre-change golden render of a mixed scene. |
| 2 | Band filter (A) | Requested band gains are within ±1 dB at the centres; the unity sum is flat to ±0.5 dB (rendered white noise, FFT analysis). |
| 3 | FDN RT60 (A) | An impulse through an FDN with target RT60 [2.0, 1.5, 1.0, 0.5] s measures T30 per octave band within ±10%. |
| 4 | Binaural (A) | A source at +90° gives a right-ear lead of 0.656 ± 0.05 ms (cross-correlation; Woodworth) and an ILD at 8 kHz ≥ 6 dB louder on the right. The ILD at 125 Hz is < 3 dB. A source in front gives \|ITD\| < 0.02 ms. Mirroring the source mirrors the signs. |
| 5 | Doppler (A) | A 1 kHz source at 20 m/s with positions updated every 256 frames measures 1061.9 Hz approaching and 944.9 Hz receding, each ±0.5%, by zero-crossing count over 100 ms windows away from closest approach. |
| 6 | Convolution (A) | Partitioned FFT convolution matches direct time-domain convolution with max relative error ≤ 1e-5. A kernel swap has no step above the cross-fade bound. |
| 7 | Click-free updates (A) | A step change in direct gain from 1 to 0.1 produces no sample-to-sample jump larger than (Δgain / `PARAM_RAMP_FRAMES`) × peak + ε. |
| 8 | Audio never blocked (A and B) | A publisher stalled while holding the slot lock (test hook) does not stop 200 `mix` calls of 512 frames, each finishing in < 5 ms with stale parameters; `audio_try_lock_misses` > 0. A backend sleeping 500 ms in `trace` has the same effect on mix timing, and the parameters publish afterwards. |
| 9 | BVH correctness (B) | 100,000 seeded random rays against BVH and brute force give identical nearest hits (triangle and t within 1e-5) on a shoebox, a 50k-triangle random soup and instanced transformed meshes. |
| 10 | Limits and validation (B, D) | Each limit in §5.3 and each invalid material, proxy or request field returns its `acoustics.*` code. Degenerate triangles are counted. No partial world is published. |
| 11 | Image sources (B) | Shoebox order 1 gives 6 paths and order 2 gives 18 (4n² + 2), with delays equal to analytic image distances / c within 1e-4 s. An occluded image path is rejected. |
| 12 | Analytic estimates (B) | `sabine_rt60` and `eyring_rt60` reproduce the reference numbers within 0.1%. |
| 13 | Traced RT60 (B) | CPU tracing with 65,536 rays, scattering 1.0 and a shoebox 3 s histogram measures T30 per band within ±10% of Eyring (bands where Eyring ≤ 2.5 s; ±15% for 4.0 s). The mean free path is within 2% of 4V/S. |
| 14 | Occluder (B and A) | In free field (absorber room), a 4×4 m wall with transmission [0.3, 0.1, 0.03, 0.01] between source and listener 10 m apart gives per-band direct levels of −5.2, −10, −15.2 and −20 dB ±1.5 dB (render plus band analysis). The HF/LF energy ratio falls by ≥ 10 dB. Moving the source until half the occlusion rays clear the edge gives an intermediate visible fraction of 0.5 ± 0.15. |
| 15 | 2D proxy (B, then D end-to-end) | A side-view room of 4 `Collider2D` boxes (inner 10×6 units, depth 8, `units_per_meter` 1) gives a proxy with V and S equal to the shoebox within 0.1% and traced RT60 within 5% of test 13. With `units_per_meter` 0.5 (dimensions doubled), the RT60 ratio is 2.0 ± 10% (Eyring α = 0.2 gives 1.842 s against 0.921 s). Removing the ceiling collider (`open_sky: true`) raises `escaped` above 0.3 and lowers late level by ≥ 6 dB. Top-down with `ceiling: false` behaves the same way. |
| 16 | Determinism (B and C) | Two synchronous CPU runs with the same seed give bitwise-identical `PropagationResult` and mixer output. Two GPU runs with the same seed give bitwise-identical histograms. |
| 17 | GPU tier 1 (C, RTX 5080, ignored by default) | Self-test passes and the status reports `GpuRayQuery`, `hardware_rt: true` and the adapter/driver. Shoebox histograms against CPU (65,536 rays): T30 per band within 5%, early (0–50 ms) and late window energies within ±0.5 dB, mean free path within 1%. An L-shaped room with a dynamic occluder gives the same tolerances after moving the occluder (TLAS update). Throughput (segments/s) is recorded for the shoebox and a 100k-triangle mesh. |
| 18 | GPU tier 2 (C) | Same tolerances as test 17 against CPU, and tier-2 histograms against tier-1 histograms with the same seeds within 1% per window. `aborted == 0` on the test scenes. |
| 19 | Forced fallback (C and B) | Preferring `GpuRayQuery` with `deny_ray_query` gives `Fallback`, active `GpuCompute`, and a reason containing "EXPERIMENTAL_RAY_QUERY". Forcing the DX12 backend gives a reason naming DX12 (real evidence on this machine). Forced self-test failure gives `Cpu` with the reason naming the failing step. An injected timeout three times demotes, emits `acoustics.backend_changed`, and keeps mixing with no gap. A `Cpu` preference never creates a wgpu instance. |
| 20 | GPU and render coexistence (C, then root) | With the desktop renderer active and the GPU tracer at defaults, the 95th-percentile frame time increases by ≤ 1 ms (measured, recorded). |
| 21 | ECS integration (D) | Authored components hydrate and validate. A second `AcousticEnvironment` is rejected. Moving a dynamic occluder emits `acoustics.occlusion_changed` once per crossing (hysteresis). Entity removal removes its geometry. `RuntimeSnapshot.acoustics` is populated and bounded. A headless run is deterministic. |
| 22 | MCP (D, late) | `acoustics_inspect` gives counts on fixture projects. Two identical `acoustics_query` calls return identical JSON. `acoustics_bake` writes a new file, refuses overwrite, rejects traversal and protected paths, and the result loads as an `"acoustics"` asset. `component_set` rejects out-of-range absorption and transmission > absorption. Tool listings and access levels are updated. |
| 23 | DSP cost (A, measured) | 32 binaural acoustic voices with 8 taps each, plus the FDN, at 48 kHz release build render 1 s of audio in ≤ 150 ms on this machine (recorded). |

Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, plus the ignored hardware tests on the RTX 5080. Untested platforms (Linux Vulkan ray query, macOS) stay explicitly unverified in docs and status.

## 9. Work split

### 9.1 Order

1. **Stream 0, contracts skeleton (lands on `main` first; one worker, root reviews).** Create `crates/engine-acoustics` (Cargo.toml, `src/lib.rs` with every §7.2 type and signature; non-trivial functions return `acoustics.unimplemented` and never fake results) and `crates/engine-acoustics-gpu` (empty crate, lints, `GpuBackend::initialize` returning `backend.unavailable: not implemented`). In engine-audio: `src/acoustic.rs` with all §7.2 types, `ParamSlot` and `AcousticPublisher` implemented (small and needed by all streams), the `PlayOptions.acoustics` field with validation, and the mixer holding the slot without applying it. Acceptance: workspace builds; existing tests unchanged and passing; a publisher round-trip unit test. Expected size is a few hundred lines.
2. **Streams A, B, C and D in parallel worktrees branched from Stream 0.**
3. **Integration by root:** merge A, B, C and D, reconcile `Cargo.lock`, run the gates.
4. **Late integration**, after the in-flight DLSS work in `crates/engine-desktop` and the engine-mcp and engine-tooling test fixes land (uncommitted in the shared checkout on 2026-10-24): the MCP tool registration and tests, the run-report telemetry, and the desktop host wiring.

### 9.2 Streams (owned paths are disjoint)

| Stream | Owns | Depends on | Acceptance |
| --- | --- | --- | --- |
| **A: Audio DSP** | `crates/engine-audio/**` (after Stream 0): `src/mixer.rs`, `src/acoustic.rs`, new `src/dsp/{band,delay,hrtf,fdn,fft,convolver,effect}.rs`, `tests/acoustic_*.rs`, `README.md` | Stream 0 | Tests 1–8 and 23; `cargo test -p engine-audio`; clippy clean |
| **B: World and CPU propagation** | `crates/engine-acoustics/**`: `world.rs`, `material.rs` (cited presets), `bvh.rs`, `planes.rs`, `proxy2d.rs`, `volume.rs`, `rng.rs`, `trace_cpu.rs`, `direct.rs`, `image_source.rs`, `estimate.rs`, `fit.rs`, `ir.rs`, `service.rs`, `document.rs` (`AcousticWorldDocument`), `tests/`, `examples/shoebox.rs`, `README.md` | Stream 0; lands `bvh.rs` and `rng.rs` early as milestone 1 for Stream C | Tests 8 (service half), 9–13, 15 (library half), 16 (CPU), 19 (state machine with fake backends) |
| **C: GPU propagation** | `crates/engine-acoustics-gpu/**`: `lib.rs`, `device.rs` (sole unsafe token), `capability.rs`, `upload.rs`, `backend.rs`, `shaders/{common,intersect_rayquery,intersect_bvh}.wgsl`, `tests/`, `README.md` | Stream 0 layouts; B milestone 1 for parity (until then, a one-leaf BVH from `gpu_layout` types) | Tests 16 (GPU), 17–19, plus test 20 measured once D and the desktop wiring exist. `cargo build` succeeds and CPU-safe tests pass on machines without Vulkan ray tracing. |
| **D: Engine integration** | `crates/engine-project/src/acoustics.rs` and `src/acoustics/**`, plus small hooks in `src/catalog.rs` (descriptor chain), `src/runtime/mod.rs` (`set_acoustics`, tick hook), `src/runtime/scene.rs` (snapshot, reload), `src/schema/project.rs` (asset kind `"acoustics"` and validation), `src/audio.rs` (`AcousticSource` merge, voice mapping), `crates/engine-project/Cargo.toml`, `tests/acoustics.rs`; `docs/acoustics.md` (user guide); `docs/project-format.md` | Stream 0; B for real behaviour (codes against the frozen API meanwhile) | Tests 10 (component validation), 15 (end-to-end), 21; existing engine-project tests pass |
| **Late (D worker or root)** | `crates/engine-mcp/src/tools/acoustics.rs`, one line each in `tools/mod.rs`; `crates/engine-mcp/tests/acoustics.rs` plus tool-count fixtures; `crates/engine-tooling/src/mcp_tools/run.rs` and `tests.rs` (`acoustics` telemetry); `crates/engine-desktop/src/runner.rs` (construct the service with `GpuBackend::ray_query`, `compute` and CPU; `--acoustics-backend` option; synchronous mode in headless), `crates/engine-desktop/Cargo.toml`; `docs/mcp.md`, `docs/architecture.md`, `docs/advanced-engine-plan.md` status row | Other worker's DLSS, engine-mcp and engine-tooling test changes merged; A–D merged | Test 22; test 20 measured; `cargo test -p engine-mcp -p engine-tooling -p engine-desktop` |

Each stream documents its crate in that crate's `README.md` from the final code. Stream D writes the user-facing `docs/acoustics.md` and links this decision record. Contract changes to §7.2 go through root as a single commit that all streams rebase onto.

## 10. Risks, non-goals and open questions

### 10.1 Risks

1. **wgpu experimental ray query.** wgpu warns of possible UB bugs, and the API may break on upgrades. Mitigations: a self-test gate, Vulkan-only enablement, device-lost and timeout demotion, tier 2 always available, and a version-pinned wgpu (`=30.0.1`). A wgpu upgrade must re-run tests 17–19.
2. **Unsafe policy.** The token needs a crate-level lint exception (engine-dlss precedent). This is root or user approval (question 2).
3. **CPU cost and lock contention in the mixer.** The existing blocking mixer mutex is shared with per-tick `configure` calls. Mitigations: an acoustic voice cap, measured cost (test 23), and a documented revisit condition for a lock-free command path.
4. **Geometry quality.** Render meshes may be dense or leaky. Mitigations: acoustic mesh overrides, double-sided handling, escape-fraction leak warnings in inspect and status, and limits.
5. **Perceptual quality.** The analytic HRTF, the listener-centric reverb and the band-averaged reflection filter are approximations. No claim of perceptual parity is made, and listening checks are manual and separate.
6. **GPU contention with rendering.** A second device on the same GPU is budgeted and measured (test 20). The CPU tier can be forced.
7. **2D proxy plausibility.** Proportions depend on `units_per_meter`, so the user guide must explain choosing it, and the inspect tool reports the resulting room dimensions.

### 10.2 Explicit non-goals (v1)

- Parity with commercial middleware (Steam Audio, Wwise Reflect, Project Acoustics, Dolby, and so on).
- Wave-based simulation.
- Measured or dataset HRTFs, which are deferred unless the user permits a download.
- Edge diffraction (UTD/BTM) and portal or pathfinding propagation.
- Ambisonics and multichannel speaker layouts.
- DX12 and Metal hardware ray query.
- Skinned or deforming acoustic geometry.
- Per-band per-voice reverb sends.
- Head tracking.
- Hard real-time guarantees for the existing mixer mutex.
- Mobile, web and consoles.

### 10.3 Open questions for the user

1. May the default `Auto` preference use wgpu's **experimental** ray query (verified working here, but wgpu documents possible UB bugs), or should hardware ray tracing be opt-in, with `Auto` starting at the compute tier?
2. Approve a crate-level exception to `unsafe_code = "forbid"` in `engine-acoustics-gpu`, limited to one audited function that creates `ExperimentalFeatures::enabled()`, as engine-dlss does for NGX?
3. Should the default spatial renderer be binaural (headphones) or the existing stereo speaker panning?
4. Permission to download crates later? Candidates are `realfft`/`rustfft` for faster convolution, and `sofar` plus a SOFA HRTF dataset (whose license must be checked) for measured binaural. The plan works without them.
5. Audio playback and the listener are native-script-only today. Should authored `engine.AudioEmitter` and `engine.AudioListener` components be added, so MCP-only projects can place sounds? This is outside this design's scope unless requested.

## 11. Integration record

What the integration added or corrected relative to the design above (all root-approved).

**Air absorption convention.** `air_np_per_m` is an *amplitude* attenuation in nepers per metre. The mixer applies `exp(−m d)` to amplitude, so the tracer decays energy as `exp(−2 m ℓ)` and Sabine/Eyring use an air term of `8mV`. The first draft wrote `e *= exp(−m ℓ)` and `4mV`, each off by a factor of 2. The component's `air` field maps through `engine_audio::acoustic::iso9613_air_np_per_m` (valid −20 to 50 °C, 0 to 100 % humidity, 50 to 120 kPa) or `db_per_m_to_np_per_m`.

**Contract additions (stream B).**
- `SourceRequest.voice: Option<VoiceHandle>`, with `id == voice.get()`. engine-audio cannot rebuild a handle from a `u64`; `None` traces a source without publishing (queries).
- `AcousticsService::drain_events()` returns `ServiceEvent::{BackendChanged, Failed, Warning}`, which engine-project maps to `acoustics.backend_changed`, `acoustics.failed` and the runtime warnings.
- `AcousticsService::set_convolution_rate(Option<u32>)`, called with the mixer output rate when `reverb` is `convolution`.
- World and builder methods `with_revision`, `dynamic_generation`, `rooms`, `room_at`, `proxy`, `plane_clusters`, `ProxyMapping::{air, footprint_instance, rigid_motion}`, and serde derives on the input types. `WorldStats.triangles` counts triangles across instances.
- `AcousticWorldDocument` (`seal`, `compute_hash`, `build(limits)`), the `"acoustics"` asset kind payload.
- `SourceStatus.occluded` means visible fraction < 0.5; the 0.3/0.7 hysteresis of `acoustics.occlusion_changed` lives in engine-project.
- `AcousticMaterial::preset`, `sabine_rt60`, `eyring_rt60`, `fit_reverb` and `synthesize_ir` keep the Stream 0 `Result` returns; `preset(Grass)` returns `acoustics.unimplemented` because no published table could be verified.
- Integration: `AcousticStatus.mixer: MixerStats` carries `AudioControl::acoustic_stats()`; `ReverbParams.level` is an energy (stream A divides it by the FDN's own energy), so `fit_reverb` returns the late energy rather than its square root.

**Conventions shared with stream A.** Listener-frame directions are unit vectors from the listener toward the apparent source (+X right, +Y up, −Z forward). Tap gains are absolute amplitudes that already include 1/length, and `direct` multiplies the mixer's distance law. Every publish lists every acoustic voice (a missing one reverts to neutral) and re-supplies its kernels. Acoustics apply only to voices with `spatial.enabled`; a voice with acoustics disabled keeps the legacy bit-identical path. The default renderer is binaural.

**MCP and tooling.** `acoustics_inspect`, `acoustics_query` and `acoustics_bake` live in `engine-mcp` over `engine_project::acoustics::{inspect_scene, query_scene, bake_scene}`; the `acoustics` telemetry object is part of `project_run_headless` and `render_screenshot`. `engine-desktop` supplies `gpu_backends()` through `Runtime::set_acoustics` and exposes `--acoustics-backend auto|ray_query|compute|cpu`. The runtime does not yet build its world from `geometry.asset`; a baked document is validated and loadable by native code and tools.

**Known limitations.**
- The default CPU budget does not fit 4,096 rays × 64 bounces in 4 ms on a few threads; the threaded service halves the ray count until the update fits and recovers by ×1.25.
- Closed solids apply their transmission twice (once per crossed surface); use single panels or author the square root for thick walls.
- A closed box leaks about 1.2e-5 of rays through its edges (ray offsets at shared edges).
- Moving 2D bodies rebuild the proxy (debounced to 0.5 s) instead of using `footprint_instance` and `rigid_motion`.
