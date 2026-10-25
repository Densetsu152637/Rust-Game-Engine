# engine-acoustics-gpu

GPU tiers of the acoustic propagation backend (`engine_acoustics::PropagationBackend`),
stream C of [the spatial audio design record](../../docs/spatial-audio-design.md)
(sections 6 and 7.2). Both tiers trace the same stochastic energy model as the CPU
tracer in `engine-acoustics` and return the same `EnergyHistogram` type.

| Tier | Constructor | Hardware RT | Requirements |
| --- | --- | --- | --- |
| 1 | `GpuBackend::ray_query(GpuOptions)` | yes | Vulkan adapter exposing `Features::EXPERIMENTAL_RAY_QUERY`, self-test passed |
| 2 | `GpuBackend::compute(GpuOptions)` | **no**: GPU-accelerated BVH traversal in a compute shader, not hardware ray tracing | any non-software Vulkan, DX12 or Metal adapter, self-test passed |
| 3 | `engine_acoustics::CpuBackend` | no | always |

Ordinary builds need no SDK: wgpu 30.0.1 with its `vulkan`, `dx12`, `metal` and
`wgsl` features, all already in `Cargo.lock`.

## Using it

```rust
use engine_acoustics::{BackendPreference, CpuBackend};
use engine_acoustics_gpu::{GpuOptions, GraphicsApi, gpu_backends, select_backend};

// Host side: hand the GPU tiers to the service (it appends the CPU tier) ...
let backends = gpu_backends(&GpuOptions::default());
// ... or pick a tier directly, with the same order and reasons:
let selection = select_backend(
    BackendPreference::Auto,
    &GpuOptions::default(),
    GraphicsApi::Primary,
    Box::new(CpuBackend::new(4)?),
)?;
println!("{:?} {:?} {:?}", selection.active, selection.state, selection.fallback_reason);
```

- `initialize` opens the tier's own wgpu instance and device (independent of the
  renderer, works headless), then runs the self-test. Until it succeeds, every call
  returns `acoustics.backend.unavailable` and `info()` claims no adapter and no
  hardware RT.
- `sync_world` uploads the world's `GpuWorldView`. A new `revision` is a full
  rebuild; a new `dynamic_generation` at the same revision rewrites the moved
  instance records, the refitted instance BVH (tier 2) and the TLAS entries, then
  rebuilds the TLAS (tier 1). A world with nothing to hit (no instances, or only
  empty meshes) is valid: nothing is uploaded and `trace` returns the CPU's result
  for it (every ray escapes, `escaped = 1`, empty bins) without a dispatch.
- `trace` splits a request into dispatches of at most 64 job pieces and at most
  2^24 estimated segments (rays x (bounces + 1)) each, about 43 ms of compute-tier
  traversal of a 100k-triangle mesh on the RTX 5080 and about 2-3 ms on the
  ray-query tier, so one request (up to 64 jobs x 1M rays x 256 bounces, about
  1.7e10 segments) never holds the shared GPU for seconds or nears the OS GPU
  watchdog. Pieces of one job are summed as integers, so the histogram does not
  depend on the split. Each dispatch is one invocation per ray (`x` = ray, `y` =
  piece) and waits up to `readback_timeout_ms` (default 100 ms) for its readback.
- After a readback timeout the timed-out work keeps running; until it completes,
  `trace` returns `acoustics.backend.timeout` ("... nothing new was submitted")
  without submitting, so at most one submission is outstanding.
- `probe_capabilities()` lists every adapter with its ray-query availability and the
  reason when unavailable, without opening a device.

### Tier selection (`select_backend`, `tier_order`)

`Auto` and `GpuRayQuery` try ray query, then compute, then CPU; `GpuCompute` tries
compute then CPU; `Cpu` uses only the CPU tier and never creates a wgpu instance
(`wgpu_instances_created()` lets tests check this). A tier counts only after its
`initialize` (probe plus self-test) succeeds. The result is `Active` when the first
tier of the order won and `Fallback` otherwise, with `fallback_reason` listing every
skipped tier as `<tier>: <reason>` and truncated to 512 bytes on a char boundary.
When no tier works, the error lists all reasons. There is no automatic promotion:
call `initialize` again (or re-select) to re-probe.

### Errors

| Code | Fatal | When |
| --- | --- | --- |
| `acoustics.backend.unavailable` | no | capability, adapter, device creation or self-test failure; not initialized |
| `acoustics.backend.failed` | yes | GPU validation error, device loss, uncaptured GPU error, saturated or non-finite histogram |
| `acoustics.backend.timeout` | no | readback not complete within `readback_timeout_ms`, or an earlier timed-out submission still running (the service demotes after three in a row) |
| `acoustics.request.invalid` | no | jobs or settings outside section 5.3 (rays 1-1,048,576, bounces at most 256, bins 0.5-10 ms, at most 4 s), or a world the tier cannot hold (out-of-range indices or materials, storage-binding or TLAS/BLAS limits) |

A fatal error drops the device; the tier stays unusable until `initialize` succeeds again.

Fallback reasons measured on the RTX 5080 (Windows 11, driver 617.14):

- DX12 (`with_api(GraphicsApi::Dx12)`): "DX12 adapter NVIDIA GeForce RTX 5080 does
  not expose EXPERIMENTAL_RAY_QUERY: wgpu needs DXC (shader model 6.5) for DX12 ray
  query and this Cargo-only build uses FXC; ray query is validated on Vulkan only".
  The compute tier runs on DX12.
- `deny_ray_query` test hook: "EXPERIMENTAL_RAY_QUERY denied by
  GpuOptions::deny_ray_query (test hook)".
- `force_self_test_failure` test hook: "<tier> self-test step 'quad hit count' failed: ...".
- Non-Vulkan adapters that do report the feature (for example Metal) are refused with
  "ray query not validated on <backend>"; Metal and Linux Vulkan are untested.

## Self-test

Run by `initialize` on the new device before the tier is reported available; the
measurements are in `GpuBackend::self_test_report()`.

1. **Quad:** a 2 x 2 m quad (2 triangles) rotated 90 degrees about Z and moved to
   z = 5, hit by 8 x 8 parallel rays offset from every edge and the diagonal. It
   requires exactly 16 hits, |t - 5| <= 1e-4 and the expected triangle and instance
   for every hit (this checks transforms, TLAS custom data and primitive indexing);
   the first unexpected hit or missed expected hit fails the step and names the ray.
2. **Shoebox:** 4,096 rays x 64 bounces through the energy kernel in a lossless,
   fully diffuse 10 x 6 x 8 m box. It requires the mean free path within 2% of 4V/S
   = 5.106 m, no aborted rays, no escaped energy and a finite, non-zero histogram.

Measured on the RTX 5080: both tiers 16/16 hits with t error 0; mean free path
5.1142 m (compute) and 5.1150 m (ray query) against 5.1064 m.

## Kernel

One WGSL module per tier: `shaders/intersect_rayquery.wgsl` (starts with
`enable wgpu_ray_query;`, `rayQueryInitialize` with `RAY_FLAG_FORCE_OPAQUE`, cull mask
0xff) or `shaders/intersect_bvh.wgsl`, followed by `shaders/common.wgsl` (RNG,
sampling, energy model, receiver, histogram). Both shaders are validated by naga in
the ordinary `cargo test`, so a shader error fails on any machine.

- **Layouts:** the frozen `engine_acoustics::gpu_layout` structs, bound as storage
  buffers; tier 2 binds the mesh nodes followed by the instance nodes in one buffer.
  Index conventions follow `gpu_layout` (absolute child and leaf indices, instance
  leaves of one instance, `node_offset = u32::MAX` for empty meshes). Tier 1 builds
  one BLAS per distinct mesh (non-indexed `Float32x3` triangle list in the mesh's
  triangle order, `OPAQUE`, `PREFER_FAST_TRACE`; a deviation from the indexed upload
  of section 6.3 that keeps `primitive_index` equal to the mesh-relative triangle)
  and one TLAS whose instance custom data is the instance index. Normals and
  materials come from the `GpuTriangle` buffer, not from vertex return.
- **Traversal (tier 2):** two-level, fixed 64-entry stacks, near child first,
  double-sided Moller-Trumbore with `t` in `(t_min, t_max]`. More than 65,536 node
  visits or a stack overflow terminates the ray and counts it in `aborted`.
- **RNG and sampling:** identical to `engine_acoustics::rng` (PCG hash,
  `ray_state(seed, ray)`, two draws for emission, exactly three per reflection).
- **Energy model:** `common.wgsl` mirrors `engine_acoustics::trace_cpu` step for step
  (section 5.4.4): relative energy 1 per band, `e *= exp(-2 m t) * reflect` per hit
  (air coefficients are amplitude nepers per metre), diffuse with probability
  `scattering` else unnormalized specular reflection, the ray ends at the hit after
  `max_bounces` reflections or when `max e < 1e-6`, segments after at least one
  reflection deposit `e * exp(-2 m t_mid) * 3 l / r^3 / rays` into bin
  `floor((path + t_in + l/2) / c / bin_s)`, a miss adds `mean_b(e) / rays` to
  `escaped`, reflected-to-hit segments feed the mean free path, and the outgoing
  origin is offset by `ray_offset(p)`.
- **Accumulation:** every histogram cell `[job][bin][band]` and every per-job counter
  is a 64-bit fixed-point value in two `atomic<u32>` words with carry (2^40 counts per
  unit of energy, 2^16 per metre of path). Integer addition is order-independent, so
  a fixed seed is bitwise reproducible on one device and driver. This replaces the
  2^31-per-unit single word of section 6.3, which loses the -35 dB tail at 1M rays.
  The low word is rounded to nearest (with carry), not truncated. Saturation of the
  high word and non-finite energies (detected from the exponent bits, which
  compilers cannot fold away like `x != x`) set flags that become
  `acoustics.backend.failed`. A zero or non-finite hit normal falls back to +Y, as
  on the CPU.

## The single unsafe token

The crate opts out of the workspace lint table (which forbids unsafe code) and sets
`unsafe_code = "deny"`. The only `#[allow(unsafe_code)]` is
`device::experimental_features_token`, which returns
`unsafe { wgpu::ExperimentalFeatures::enabled() }` with a `// SAFETY:` comment. The
token has no memory-safety precondition of its own: it acknowledges that wgpu's
experimental APIs may contain UB bugs reachable from safe calls. It is requested
only when creating a tier-1 device, together with `EXPERIMENTAL_RAY_QUERY`, on
Vulkan, so it is active from device creation (including during the self-test's own
BLAS/TLAS builds and traces); the self-test gates every later use. wgpu is pinned to
`=30.0.1`; a wgpu upgrade must re-run the ignored hardware tests.

## Tests

`cargo test -p engine-acoustics-gpu` runs the non-GPU tests (shader validation,
tier order, bounded reasons, dispatch splitting including the 1.7e10-segment worst
case, the deny hook before any wgpu call, and the `Cpu` preference creating no
instance). Hardware tests are `#[ignore]`d:

```text
cargo test -p engine-acoustics-gpu --test gpu_tiers   -- --ignored --nocapture --test-threads=1
cargo test -p engine-acoustics-gpu --test empty_world -- --ignored --nocapture --test-threads=1
cargo test -p engine-acoustics-gpu --test bvh_parity  -- --ignored --nocapture
cargo test -p engine-acoustics-gpu --test parity      -- --ignored --nocapture --test-threads=1
cargo test -p engine-acoustics-gpu --test coexistence -- --ignored --nocapture
```

- `gpu_tiers`: capability probe, both self-tests, the deny hook falling back to
  compute, DX12's reason, a forced self-test failure falling through the tiers.
- `bvh_parity`: 100,000 seeded rays per scene against `AcousticWorld::raycast`
  (shoebox inside and outside, a 19,200-triangle box, rotated and scaled instances,
  an L-shaped room before and after moving a dynamic panel). Measured: no hit/miss
  disagreement, |dt| at most 4.4e-5 m, and at most 10 of 100,000 rays resolving a
  coplanar tie (panel base on the floor) to a different triangle.
- `empty_world`: an empty world and a world of one empty-mesh instance on both tiers
  give the CPU's histogram exactly (`escaped = 1`), the tier stays alive, and a real
  world uploads and traces afterwards.
- `parity`: design tests 16-18 against `CpuBackend` (65,536 rays x 256 bounces, 3 s
  histogram): the reference shoebox (scattering 1), the shoebox with air absorption
  [0.0002, 0.0007, 0.0025, 0.012] Np/m and a 0.25 m receiver (scattering 0.7), and
  an L-shaped room before and after moving a dynamic panel (the GPU result must
  change). Required here: T30 within 0.5%, window energies within 0.05 dB, mean
  free path within 0.2% (tighter than the design's 5% / 0.5 dB / 1%). Measured on
  the RTX 5080: T30 within 0.08% (0.31% for the compute tier in the 125 Hz band
  with air and the small receiver), windows within 0.001 dB, mean free path within
  0.03%, no aborted rays, bitwise-identical repeat runs and bitwise-identical
  results with a 2^18-segment budget (33 submissions) and the default one. Tier 2
  against tier 1 per window at most 0.47% (1-3 s window). Air absorption lowers the
  late 8 kHz energy by 2.76 dB in that room.
  Throughput (submit to decoded result, split under the default budget): 1,048,576
  rays x 64 bounces on the 99,372-triangle shoebox 5.8e9 segments/s (ray query) and
  3.6e8 (compute), on the 12-triangle shoebox 7.4e9 and 2.2e9. An independent
  review measured 9.1e9 segments/s on the 99k-triangle mesh through the ray-query
  tier. The 1 ms readback-timeout test checks that the tier survives, that traces
  are refused without submitting while the timed-out work runs (10 refusals
  observed), and that it recovers.
- `coexistence`: design test 20 at library level, a separate wgpu device rendering
  1920 x 1080 offscreen frames in the same process while the ray-query tier traces.
  Latest measurement over 3 s each: render only p95 0.536 ms; with the default
  tracer (4,096 rays x 64 bounces at 4 Hz) p95 0.542 ms (+0.006 ms); with a stress
  tracer (65,536 rays x 256 bounces back to back) p95 1.587 ms (+1.05 ms). An
  earlier run on a busier GPU measured 8.76 / 8.40 / 13.28 ms. The desktop
  measurement with the real renderer belongs to the late integration.

## Limitations

- Ray query is verified only on Windows 11, Vulkan, RTX 5080, driver 617.14. Linux
  Vulkan and Metal are unverified; DX12 ray query needs DXC and is out of scope.
- Results are reproducible per device and driver, not across machines, and agree
  with the CPU tracer statistically, not bitwise (sin, cos and sqrt are not
  correctly rounded in WGSL, and hardware intersection rounds differently).
- Equal-distance hits (coplanar overlapping triangles) may resolve to a different
  triangle than the CPU's lowest-(instance, triangle) rule.
- Each tier opens its own device on the same GPU as the renderer.
- Occlusion rays and early reflections stay on the CPU (section 6.3).
