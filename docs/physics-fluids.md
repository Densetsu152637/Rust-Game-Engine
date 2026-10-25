# Particle fluids

`crates/engine-fluids` simulates 3D particle fluids on the CPU. It depends only on `glam` and
`std` and has no ECS, project-format, renderer or rigid-body dependency, so a host adapter
creates, updates, steps and reads fluid volumes by stable `u64` ids. Units are meters, seconds
and kilograms. See the crate [README](../crates/engine-fluids/README.md) for a code sample.

The authored `engine.FluidVolume` component (MCP-editable through `component_set`), its
`physics.fluid.capacity_reached` event, rigid coupling and rendering are wired in
`engine-project` and `engine-physics-render`: see
[Authored component](#authored-component-enginefluidvolume). 2D fluids are deferred (see
[Limits](#limits-and-deferred-work)).

## Solver choice

The solver is Position Based Fluids (PBF, Macklin & Mueller 2013) rather than weakly
compressible SPH. Reasons:

- WCSPH needs a stiff equation of state whose artificial sound speed bounds the timestep
  (CFL), which costs many small steps per frame for low compression. PBF enforces
  incompressibility as a position constraint and is stable at game steps (1/60 s with 2
  substeps by default).
- Position-level collision projection makes rigid-body proxies and the box domain simple and
  robust, and the impulse each proxy receives falls out of the position corrections exactly.
- It matches the position-based approach of the sibling `engine-xpbd` crate.

Per step, emitters run first, then `substeps` fixed substeps of `dt / substeps`:

1. Apply gravity and predict positions.
2. Build CSR neighbour lists once on the predicted positions with the spatial hash grid.
3. Repeat `iterations` times (Jacobi): compute densities (poly6) and lambdas for the
   unilateral constraint `C = max(rho / rho0 - 1, 0)` with relaxation `epsilon`, compute position
   corrections with the spiky gradient and the tensile-instability term
   `s_corr = -k (W(r) / W(dq * h))^4`, then resolve proxy and domain collisions.
4. Derive velocities from the position change, apply wall restitution, vorticity confinement
   (optional), symmetric XSPH viscosity and the speed clamp.

After the substeps particles age, expire (lifetime) or are removed by kill regions, and a
diagnostic pass computes end-of-step densities, colour-field normals and surface flags on the
final positions.

Implementation details that differ from the paper:

- Particle mass is calibrated so a cubic lattice at the rest spacing `s = 2 * particle_radius`
  has exactly the rest density. Lattice fills therefore start at rest.
- `epsilon = relaxation * D_rest` and `s_corr` is scaled by `1 / D_rest`, where `D_rest` is the
  lambda denominator of a rest-lattice particle. Both parameters are dimensionless and do not
  depend on particle size or rest density.
- The constraint is unilateral (only compression is corrected), so free-surface particles do
  not clump.
- Domain walls contribute the tabulated density of virtual lattice layers behind each wall
  plane (64 samples over one kernel radius, averaged over lateral offsets) to both the density
  and the constraint gradient. Particles next to a wall therefore sit at rest density instead
  of being pulled toward it, and walls are not reported as free surface.
- XSPH uses the symmetric weight `m / rho0`, so viscosity conserves momentum.

### Spatial grid and determinism

Cells have the kernel radius as edge length. Cell coordinates hash into a power-of-two table
(at least twice the particle count) and particles are counting-sorted by bucket; the 27-cell
query deduplicates buckets and visits them in sorted order. No hash-map iteration order
reaches the solver or its outputs: volumes, emitters and kill regions live in `BTreeMap`s,
proxies are sorted by id, particles keep spawn order (removal is a stable compaction) and
every particle carries a monotonically assigned `u64` id. Identical inputs produce bitwise
identical positions, velocities and densities on the same build and platform (test
`identical_runs_are_bitwise_identical`). The solver is single-threaded.

## Parameters and units

`FluidParams` (defaults in parentheses; `FluidParams::validate` enforces the ranges):

| Field | Meaning | Range (default) |
| --- | --- | --- |
| `particle_radius` | Collision radius (m); rest spacing is twice this | `1e-3..=10` (0.025) |
| `kernel_scale` | Kernel radius `h` / rest spacing | `1.5..=3.0` (2.0) |
| `rest_density` | kg/m^3 | `1e-3..=1e5`, must be > 0 (1000) |
| `gravity` | m/s^2 | finite, length <= 1000 (0, -9.81, 0) |
| `substeps` | Fixed substeps per step | `1..=16` (2) |
| `iterations` | Constraint iterations per substep | `1..=32` (4) |
| `relaxation` | Constraint softening relative to `D_rest` | `1e-6..=10` (0.1) |
| `tensile_k`, `tensile_dq` | Tensile correction strength and reference distance (fraction of `h`) | `0..=1` (0.1), `0.05..=0.5` (0.2) |
| `viscosity` | XSPH blend per substep | `0..=1` (0.01) |
| `vorticity` | Vorticity confinement strength (m/s) | `0..=100` (0, off) |
| `max_speed` | Speed clamp (m/s) | `(0, 1000]` (50) |
| `surface_threshold` | Surface flag threshold on the length of `h * grad(colour)` | `(0, 10]` (0.6) |

`Domain` is a closed axis-aligned box (`min`, `max`, every axis at least two spacings wide,
coordinates within 1e6 m) with `restitution` (`0..=1`, default 0) and Coulomb `friction`
(`0..=10`, default 0.1). Particle centres stay at least one particle radius inside.
`VolumeDesc::capacity` is `1..=MAX_PARTICLES` (1,048,576). A step accepts `0 < dt <= 0.1` s.

Changing parameters with `set_params` keeps particle state and recomputes mass, kernel and wall
tables; `set_domain` and `set_capacity` (not below the current count) validate first. Every
rejected call leaves the volume unchanged.

## Emitters and sinks

- `fill(region, velocity, lifetime)`: cubic lattice at the rest spacing, centred in a box or
  sphere region, clipped to the domain.
- `spawn(positions, velocity, lifetime)`: explicit positions inside the domain.
- `set_emitter(id, EmitterDesc)`: `EmitterKind::Rate` emits `per_second` particles at
  SplitMix64-seeded positions in a region (fractional particles carry over in an `f64`
  accumulator, so 120/s at 60 Hz is exactly 2 per step); `EmitterKind::Nozzle` emits a disc of
  lattice points every time the stream advances one spacing (a 5 cm-radius nozzle at 5 cm
  spacing and 1.5 m/s emits 5 particles every second step). Each emitter has an optional
  lifetime and an `enabled` flag; updating an emitter keeps its accumulator. Up to 64 emitters.
- `set_kill_volume(id, region)`: particles inside are removed after each step (up to 64).
- Lifetimes: particles whose age reaches their lifetime are removed after the step.

Capacity is explicit: `spawn` and `fill` are all-or-nothing and return
`Error::CapacityExceeded { requested, available }`. Emitters spawn what fits and report the
rest in `StepReport::dropped_at_capacity`; `capacity_reached` says the volume is full.
`StepReport` also counts `spawned`, `killed`, `expired` and `neighbor_overflow`.

## Rigid-body proxies (two-way coupling)

The host supplies `ColliderProxy` values every step (the type is kept identical to
`engine-xpbd`): `id`, pose at the start of the step, linear and angular velocity, `shape`
(`Sphere`, Y-axis `Capsule`, `Box`, world-space `Plane` whose pose is ignored) and Coulomb
`friction` (`0..=10`). Within the step each proxy moves with its constant velocities; particle
centres are projected to at least one particle radius outside the shape after every iteration,
with Coulomb position friction relative to the proxy surface velocity. Up to 256 proxies with
unique ids; non-finite values, non-positive sizes, zero plane normals and rotations far from
unit length (outside 0.5..=2) are rejected, others are normalized.

`StepReport::proxy_impulses` (and `WorldStepReport::proxy_impulses`, summed over volumes)
lists, sorted by id, the linear impulse (N s) the fluid applied to every supplied proxy and the
angular impulse about the proxy position (N m s, zero for planes). Apply `linear / dt` as a
force for buoyancy and drag. Because PBF corrections between particles are pairwise
antisymmetric and XSPH is symmetric, the impulse equals the negative fluid momentum change when
no domain wall, vorticity or speed clamp is involved (measured relative error about 3e-6).

Proxies have no boundary density, so particles touching a proxy are slightly under-dense and
the measured buoyancy is below Archimedes' value (see below).

## Rendering outputs

Rendering is a later step through the rendering domain's `PipelineFeature` API
([render architecture](rendering-architecture.md)), starting with debug points; this crate
draws nothing. It provides plain data:

- `render_output()` / slice accessors: ids, positions, velocities, end-of-step densities
  (kg/m^3, including wall boundary density), outward colour-field normals and surface flags
  (length of `h * grad(colour)` above `surface_threshold`). Near a side wall the free-surface normal tilts
  toward the wall because the virtual wall layers extend above the surface.
- `sample_density_grid(min, max, dims)`: normalized density `rho / rho0` (without wall
  density) on a regular grid, x fastest, at most `MAX_GRID_SAMPLES` (4,194,304) samples. The
  0.5 iso-surface follows the free surface within one spacing (test
  `settled_pool_reports_surface_normals_and_density_grid`) and is the input for a later
  marching-cubes mesh.
- `stats()`: particle and surface counts, mean, interior-mean and max density, kinetic energy,
  max speed.

## Measured behaviour

Debug-build tests (`cargo test -p engine-fluids`) use 5 cm spacing, default parameters and
1/60 s steps:

| Test | Result |
| --- | --- |
| Dam break, 480 particles, 4 s | interior mean density 993.7 kg/m^3 (-0.6 %), max 1006.2, kinetic energy 2.04 J at 1 s to 0.0029 J at 4 s, no particle outside the domain |
| Hydrostatic column, 576 particles, 3 s | surface 0.796 m vs 0.800 m expected, density 1012.4 at 0.05-0.3 m vs 1003.6 at 0.45-0.65 m |
| Column release, viscosity 0 vs 0.8, 0.5 s | front 1.067 m vs 0.936 m, centroid 0.414 m vs 0.360 m |
| Fixed sphere (r = 0.1 m) under 1152 falling particles, 1.5 s | worst clearance -3e-8 m (no penetration) |
| Box proxy pushing 216 particles at 1 m/s, zero gravity | fluid gained 19.88 N s, proxy impulse -19.88 N s |
| Submerged sphere (r = 0.1 m) in a 0.5 m pool | 27.5 N up vs Archimedes 41.1 N (0.67); 0 N in air |

### Stability (release sweep)

`cargo test -p engine-fluids --release --test perf -- --ignored --nocapture` runs a 1 m
column (20 layers, 960 particles) collapsing in a 1 m tank for 4 s at 1/60 s steps:

| Substeps x iterations | Interior mean density | Peak density during collapse |
| --- | --- | --- |
| 1 x 2 | 1037-1080 | 1585-1618 |
| 1 x 4 | 1015-1038 | 1250-1293 |
| 2 x 2 | 1007-1019 | 1108-1129 |
| 2 x 4 (default) | 1000-1006 | 1046-1073 |
| 2 x 8 | 992-995 | 1020-1032 |
| 4 x 4 | 971-972 | 1013-1022 |

Ranges span `relaxation` 0.01, 0.1 and 1.0 (larger relaxation is softer and settles denser).
No configuration diverged (residual speeds stayed below 0.25 m/s). Guidance: keep the substep
length at or below about 1/120 s for 5 cm particles and use at least 4 iterations; one
substep compresses deep columns by several percent. The tensile correction is a position
correction per iteration, so with 4 or more substeps it expands the fluid by about 3 %;
lower `tensile_k` (for example 0.05) when raising substeps.

### Performance

Release build, this machine, single thread, default parameters: a 10,000-particle dam break
(5 cm spacing) takes 36.5 ms mean and 41.5 ms max per 1/60 s step (2 substeps x 4
iterations), so it is slower than real time at this size; a 100 x 48 x 50 density grid
(240,000 samples) takes 39 ms.

## Authored component: `engine.FluidVolume`

Add `engine.FluidVolume` to an entity with a `Transform`. The domain, fills, emitters and
kill volumes are entity-local and axis aligned: the entity's translation and scale place them
(they follow the entity every tick) and its rotation is ignored.

| Field | Meaning |
| --- | --- |
| `domain_min`, `domain_max`, `restitution`, `wall_friction` | The closed box and its walls. |
| `particle_radius`, `kernel_scale`, `rest_density`, `viscosity`, `vorticity`, `substeps`, `iterations`, `relaxation`, `tensile_k`, `tensile_dq`, `max_speed`, `surface_threshold` | The `FluidParams` described above; all live-editable. |
| `gravity_scale` | Multiplier on scene gravity. |
| `capacity` | Most particles (1..=131,072); emitters stop at capacity. |
| `fills` | Up to 8 `{shape: box/sphere, center, half_extents / radius, velocity}` lattice fills, run at creation and again when the list is edited. |
| `emitters` | Up to 16: `kind: rate` (`center`, `half_extents`, `per_second`, `velocity`, `seed`) or `nozzle` (`position`, `direction`, `nozzle_radius`, `speed`), plus `lifetime` and `enabled`. |
| `kill_volumes` | Up to 16 regions that remove particles. |
| `two_way`, `coupling_scale`, `collision_mask` | See below. |

Event `physics.fluid.capacity_reached` (target: the volume entity; payload `capacity`,
`particles`, `dropped`) fires once each time an emitter fills the volume; it re-arms when
the volume drops below capacity again. At most 32 volumes per scene. The fluid's reaction
impulse (buoyancy and drag) on a dynamic body is applied as returned by the step.

### Rigid coupling

Every engine-physics substep the feature turns the 3D rigid bodies near the object into
collider proxies (current pose, linear and angular velocity, material friction), steps the
solver with the same fixed `dt`, and applies the reported reaction impulse (linear, plus the
angular part about the body origin) to **dynamic** bodies, multiplied by `coupling_scale`.
`two_way: false` keeps the one-way collision. Bodies are used when `material.layer &
collision_mask != 0` (sensors never collide).

| Body shape | Proxy |
| --- | --- |
| box, ball, capsule | exact |
| cylinder | local bounding box (`radius`, `half_height`) |
| convex hull | local bounding box of the hull points |
| compound | local bounding box of the parts' bounding spheres |
| trimesh, heightfield | **not coupled** (no simple plane/box path exists; put a fixed box collider under them) |

Only bodies within the object's bounds plus 0.25 m are passed to the solver; more than
1024 (cloth, soft body) or 256 (fluid) nearby proxies keep the nearest ones (ties by entity id)
and leave the others out of that step's coupling; the omissions are counted
(`PressureStatus::proxies_omitted`) and announced once per episode with `physics.proxies.limited`
([Physics](physics.md#pressure-and-capacity)). The step never fails for this.

### Transforms and render data

Particles simulate in **world space**. The entity `Transform` is only the creation anchor
(the mesh is placed with its world pose when the object is built); it is not written back
and moving it later does not move the object. Structural edits (mesh, size, resolution,
pins, density) rebuild the object at the entity's current pose; material, wind, damping,
gravity scale, substeps and coupling edits apply live.

After each frame in which the simulation stepped, the feature publishes the `PhysicsRenderData`
world resource (`engine_project::simulation::physics::PhysicsRenderData`) for a render feature
to read from the extracted world (see [render architecture](rendering-architecture.md)):

```rust
pub struct PhysicsRenderData {
    pub frame: u64,                                    // bumped on every refresh
    pub cloths: BTreeMap<Entity, ClothRender>,         // { positions, normals, triangles: Arc<[[u32; 3]]> }
    pub soft_bodies: BTreeMap<Entity, SoftBodyRender>, // { positions, normals, triangles (surface), volume, rest_volume }
    pub fluids: BTreeMap<Entity, FluidRender>,         // { positions, velocities, densities, surface, particle_radius,
                                                       //   rest_density, capacity, domain_min, domain_max }
}
```

`deformable_debug_segments(&World, max_segments)` returns cloth and soft-body edges and
fluid particles (a short velocity tick, brighter on the surface) as world-space segments,
bounded and only when `engine.PhysicsSettings.debug` is on. The engine-physics debug
contributor hook takes a stateless function pointer, so these segments are not merged into
`debug_view`.

## Limits and deferred work

- CPU only and single-threaded. GPU acceleration (compute-shader PBF) and multi-threaded
  Jacobi passes are deferred; the Jacobi structure keeps per-particle work independent, so
  threading can stay deterministic.
- 3D only. A 2D variant through the same core is deferred.
- Wall boundary density assumes flat walls; corners and edges count overlapping virtual
  layers twice (slight extra repulsion). Proxies use projection only and have no boundary
  density.
- Proxies receive impulses but are not moved by the fluid; the host applies the impulse to
  its rigid body. Proxy motion within a step uses constant velocities.
- Neighbour lists are capped at `MAX_NEIGHBORS` (128) per particle; overflow (only under
  extreme compression) is counted in `StepReport::neighbor_overflow` and breaks exact
  momentum symmetry.
- No surface tension, multiphase or foam/spray models; the colour-field surface flag is a
  heuristic.
- Particles draw as sphere impostors through `physics.fluid` ([Physics rendering](physics-rendering.md));
  a screen-space fluid surface is deferred there.
