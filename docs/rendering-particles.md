# GPU particles

`engine.particles` simulates fixed-capacity particle emitters on the GPU and
draws them as camera-facing billboards, blended into the HDR scene and
depth-tested against scene depth. The simulation advances **once per scene
tick**; drawing never advances it.

The pass is a native feature of `engine-render-pipeline`
([rendering-architecture.md](rendering-architecture.md)); the authored path is
the `ParticleEmitter` component ([project-format.md](project-format.md)). The
fluid proxies of `engine-physics-render` are a separate system and are not
affected.

## Tick contract

The simulation clock is explicit and independent of rendering:

- `ScenePipeline::advance_particles(ParticleTick { id, dt }, &emitters)` is the
  only call that simulates. It validates everything first, records and submits
  its own compute work, and returns a `ParticleTickReport`.
- A `tick.id` equal to the last simulated one is a **no-op**
  (`advanced: false`, counted in `ParticleReport::repeated_ticks`), even if the
  emitter list differs. Any other id advances by `tick.dt`. Ids are opaque;
  hosts pass a monotonically increasing scene tick number. `dt` must be finite
  in `(0, 0.25]`; longer steps are rejected by the pipeline (the desktop
  adapter clamps them).
- `render_view` (and every view, eye, probe or shadow pass built on it) only
  reads the pools. Rendering the same tick for any number of views, redraws
  between ticks, `GpuRenderer::render_eyes`, and DLSS Frame Generation
  presentations (`render_generated`, `present_generated`, `present_real`) never
  simulate. Observable through `ScenePipeline::particle_report()`:
  `simulated_ticks` counts simulated ticks only, `repeated_ticks` the ignored
  repeats, and each emitter reports its own `ticks` and `spawn_attempts`.
- `emitters` is the complete live set of the tick. An id that disappears frees
  its buffers; a new id starts an empty pool; a changed `capacity` restarts the
  emitter (empty pool, fresh counters, same id).
- Multiple scene ticks between two presented frames each call
  `advance_particles`; `DesktopRunner` does so right after every
  `Runtime::tick`, using `Runtime::extract_particles()` (tick id = scene ticks
  completed, `dt` = that tick's step). A host that renders without this call
  draws empty pools.

## Simulation

Each emitter owns three GPU buffers: a pool of `capacity` 32-byte slots, a
16-byte counter block, and the 176-byte emitter uniform written every tick. A
tick records, per emitter, in one compute pass:

1. `integrate`: for every live slot, `age += dt`; at `age >= lifetime` the slot
   is zeroed (killed); otherwise
   `v = (v + gravity * dt) * max(1 - drag * dt, 0)` then `p += v * dt`
   (semi-implicit Euler). Survivors are counted into `alive`.
2. `emit`: `count` spawns; spawn `k` of the tick targets ring slot
   `(head + k) % capacity`. A free slot receives a new particle (age 0, not
   integrated until the next tick); a slot that is still alive **drops** the
   spawn (`dropped`). The spawn count is
   `floor(carry + spawn_rate * dt)` with the fraction carried to the next tick.

A slot is alive exactly when `lifetime > 0`. The counters are
`alive` (recomputed each tick), `spawned`, `dropped` and `killed` (cumulative,
wrapping at 2^32).

### Determinism and the CPU reference

The spawn plan (count, ring head, spawn index) is computed on the CPU by
`EmitterCursor::plan`, shared by the GPU path and by `ParticleReference`, so
both see identical plans. Per-spawn randomness is a stateless PCG hash,
`unit_random(seed, n, stream)` with `n` the spawn index and stream 0 to 6
(lifetime, speed, launch cone u and v, spawn offset x, y, z); the hash and the
`h >> 8` scaling by 2^-24 are bit-identical in WGSL and Rust. The launch
direction is `normalize(axis + tan(spread) * (u * tangent + v * bitangent))`
with `u`, `v` uniform in `[-1, 1]` (a square-section cone: no trigonometry on
the GPU, so no sin/cos accuracy gap); the basis comes from the CPU.

`ParticleReference::step` performs the same step in `f32` on the CPU. Counters,
slot liveness, spawn indices and slots are exact. Positions, velocities, ages
and lifetimes agree within `REFERENCE_TOLERANCE` (1e-3 relative to `max(1, |x|)`)
because the GPU may fuse multiply-adds and rounds `normalize` differently. The
tests read the pools back with `ScenePipeline::read_particles` and compare
against the reference after N seeded ticks.

### Overflow and limits

Capacity is fixed per emitter and spawns beyond it are dropped and counted,
never grown:

- one tick asks for more spawns than `capacity`: the excess is dropped without a
  slot attempt (`ParticleEmitterReport::rate_limited`);
- a spawn whose ring slot is still alive is dropped (`ParticleCounters::dropped`).
  The ring advances per attempt, so a long-lived particle ahead of the head can
  drop a spawn while other slots are free (fragmentation); with equal lifetimes
  the ring is in age order and this does not occur. `alive` never exceeds
  `capacity`.
- `ParticleReadback::dropped_total()` is the sum of both. Every requested spawn
  is either spawned or counted as dropped.

| Limit | Value |
| --- | --- |
| Live emitters | 16 (`MAX_EMITTERS`) |
| Particles per emitter | 65 536 (`MAX_PARTICLES_PER_EMITTER`) |
| Sum of capacities | 262 144 (`MAX_TOTAL_PARTICLES`) |
| Tick step | `0 < dt <= 0.25` s (`MAX_TICK_DT`) |
| Spawn rate | `0..=1e6` per second |
| Lifetime | `0 < min <= max <= 3600` s |

Validation (`ParticleEmitter::validate`, `ParticleTick::validate`,
`validate_tick`) is pure and runs before any state or GPU change: non-finite
values, zero or excess capacity, negative rates, lifetimes, speeds, sizes and
drag, inverted ranges, a zero direction, duplicate ids, and the emitter and
total budgets are all errors.

## Placement and blending

`engine.particles` runs in the `Transparent` stage, after `engine.transparent`
(relative to other post-transparent features by registration order). It is
disabled by default and turned on for a view while any emitter has spawned a
particle, unless the view sets `engine.particles` to `false`
(`Camera3D.render_features["engine.particles"]`). With no spawned particles the
pass does not run and the frame is bit-identical to a particle-free frame.

Drawing is one instanced draw of 6 vertices per emitter straight from the pool
buffer; no CPU round trip. The instance count is the **high-water slot count**
`min(capacity, spawn attempts)`, known to the CPU, rather than the GPU alive
count, because the ring leaves holes: dead slots inside that range collapse
outside the clip volume in the vertex shader. (A draw from the alive count would
need an order-preserving GPU compaction; the alive count stays available for
diagnostics through the counters.) Each billboard is a camera-facing disc (the
fragment shader discards outside the unit circle) whose half size and colour
interpolate linearly between birth and death over `age / lifetime`.

- Depth: the scene depth is loaded and tested `LessEqual`; particles never write
  depth, so nearer opaque geometry (and anything drawn earlier in the stage)
  occludes them.
- `alpha` blend: premultiplied "over" (`One`, `OneMinusSrcAlpha`). Overlapping
  particles blend in **slot order** (emitters in id order, slots in index order);
  there is no depth sort, so intersecting translucent particles at different
  depths can blend out of order. This is deterministic frame to frame.
- `additive` blend: `rgb * alpha` added to the scene; order independent, so
  correct without sorting.
- Particles are drawn at render resolution before the `Upscale` stage, so they are
  part of the image DLSS or the native upscaler consumes, and they use the
  jittered view-projection like all scene geometry.

## Authored settings

The `ParticleEmitter` component sits on an entity with a `Transform`. The entity
world position is the spawn centre; the launch axis is the component's local
`direction` (default +Y) rotated into world space. Fields: `capacity`,
`spawn_rate`, `lifetime` `[min, max]`, `speed` `[min, max]`, `spread` (cone half
angle, radians), `spawn_extent` (half size of the spawn box), `gravity` (world),
`drag`, `size` `[birth, death]` (billboard half size in metres), `start_color` and
`end_color` (linear RGBA, rgb up to 1000 HDR, alpha 0..1), `blend` (`alpha` or
`additive`), `seed`, `enabled`. Defaults, ranges and a valid example are in
`component_schema ParticleEmitter`. The entity's authored ID is the emitter's
identity across ticks. More than 16 enabled emitters or more than 262 144 total
capacity fail extraction with `render.particles.limit`.

## Known limits

- Particles live in world space once spawned: they do not follow a moving emitter,
  and a floating-origin rebase does not translate live pools (they displace for at
  most their lifetime).
- No collisions with scene depth, no soft-particle fade, no textures or flipbooks,
  no per-particle random size or colour, no lighting or shadows on particles.
- A multi-tick burst before a frame renders every tick (one compute submission per
  tick); only the pool state after the last tick is drawn.
- Counters wrap at 2^32 and spawn indices at 2^32 (the random stream repeats).
- The GPU tests (`gpu_particles`, `gpu_authored_particles`) passed in the stage 10 gate
  at `c0ab566`; no performance number is recorded.

## Degenerate emitter transforms

An emitter whose world transform makes the native description invalid (a
zero-scaled entity, the usual way to hide one, gives a zero launch direction) is
**skipped, not fatal**: `GpuRenderer::advance_particles` validates each native
emitter, drops the failing ones from the tick's set (freeing their pools) and lists
their ids in `GpuRenderer::skipped_particle_emitters()`. Every other emitter
simulates. Skipping was chosen over falling back to the authored direction because a
hidden emitter should stay hidden. `ScenePipeline::advance_particles` itself stays
strict for direct callers.
