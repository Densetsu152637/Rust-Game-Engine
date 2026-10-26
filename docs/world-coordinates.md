# Large-world coordinates

Positions stay precise far from the origin (tested at 50 km, 1,000 km and 10,000 km) through
**floating-origin rebasing**: the simulation, physics and renderers keep working in f32 near
a local origin, and the absolute position of that origin is an f64 offset that moves with the
player. Code: `engine_project::world::coordinates` (crates/engine-project/src/world/coordinates).

Contents: [Model](#model) - [Authoring](#authoring) - [Native API](#native-api) -
[What a shift does](#what-a-shift-does) - [Subsystems](#subsystems) -
[Events and telemetry](#events-and-telemetry) - [Rendering](#rendering) -
[Decision record](#decision-record) - [Limits](#limits) - [Verification](#verification).

## Model

- `WorldOrigin { offset: [f64; 3] }` (a resource in every runtime world) is the absolute
  position, in metres, of the local origin. Every `Transform` / `WorldTransform` is f32 and
  local: **absolute = offset + local**.
- A *shift* (rebase) moves the origin by `delta = new offset - old offset` and moves every
  root `Transform` by `-delta`, so absolute positions do not change.
- An f32 has 24 significant bits: about 0.24 mm resolution at 2 km, 3.9 mm at 50 km, 6 cm at
  1,000 km and 1 m at 10,000 km. Rebasing keeps everything near the anchor (camera or
  player) within the threshold of the origin, where the resolution is sub-millimetre.

## Authoring

### `engine.LargeWorld`

Rebasing settings, at most one per scene (on any entity). Without it nothing rebases
automatically; `rebase_now` still works.

| Field | Default | Meaning |
| ----- | ------- | ------- |
| `enabled` | `true` | rebase automatically |
| `threshold` | `2048` | metres from the origin, on any axis, at which the anchor triggers a rebase (1..=1e6) |
| `snap` | `512` | grid in metres the new origin snaps to: a power of two from 1 to 65536, at most `threshold` |
| `anchor` | `""` | authored id of the anchor entity; empty uses the active `Camera3D` with the lowest id |

```json
"custom": {"engine.LargeWorld": {"version": 1, "data": {"threshold": 4096, "snap": 1024, "anchor": "player"}}}
```

### `engine.AbsolutePosition`

The f64 absolute position of a **root** entity (no `parent`; requires `Transform`):
`{"position": [x, y, z]}`, each within +-1e12 m.

```json
{"id": "tower", "name": "tower", "components": {
  "Transform": {"position": [0, 0, 0]}, "MeshRenderer": {"mesh": "tower"},
  "custom": {"engine.AbsolutePosition": {"version": 1, "data": {"position": [12345678.25, 40.0, -9876543.5]}}}}}
```

When the component is inserted (scene start, streamed cells, PCG prefabs, native inserts) an
ECS insert hook sets the local `Transform` position to `position - WorldOrigin.offset`,
computed in f64 and rounded once. The authored `Transform` position is ignored; rotation and
scale are kept. On every shift the local position is recomputed from the f64 value instead
of shifting the f32, so the entity never accumulates rounding. When the entity moves
(physics, scripts, navigation), `position` follows `offset + local` (synced in
`post_transforms` and on every read).

Scene validation (`Project::validate_scene`: load, `scene_edit`, `component_set`) rejects a
second `engine.LargeWorld`, `snap > threshold`, a snap that is not a power of two, an
out-of-range threshold or position, and `engine.AbsolutePosition` on a child.

Both components are editable through MCP `component_set` (closed schemas, tested in
crates/engine-mcp/tests/coordinates.rs).

## Native API

`engine_project::world::coordinates`:

| Function | Purpose |
| -------- | ------- |
| `origin(world) -> [f64; 3]` | current offset |
| `absolute_position(world, entity) -> Result<[f64; 3]>` | exact `engine.AbsolutePosition` while the entity has not moved since it was applied, else `offset + world position` |
| `set_absolute_position(world, entity, [f64; 3])` | places the entity; a dynamic or kinematic body is teleported with its velocities (`set_body_pose`); a root also gets or updates `engine.AbsolutePosition` |
| `rebase_now(world, new_offset) -> Result<ShiftReport>` | shifts now (`new_offset` used as given; snap it to keep repeated shifts exact) |
| `status(world)`, `snapshot(world)` | `WorldOriginStatus` |
| `WorldOrigin::absolute(local)`, `WorldOrigin::local(absolute)` | conversions |

`ShiftReport { previous, offset, delta, roots, bodies }`.

## What a shift does

The coordinates service runs **first** in the world `pre_simulation` hook (before streaming,
terrain, PCG, water, foliage and `simulation::step`). Each tick:

1. **Reconcile**: the first tick latches the offset written before it as the initial origin.
   If `WorldOrigin` was written directly afterwards, the world follows the write with one
   shift (`cause: external`); if that shift is refused the write is reverted and reported.
2. **Threshold**: with an enabled `engine.LargeWorld` and a resolved anchor, if any axis of
   `anchor absolute - offset` exceeds `threshold`, the new offset is the anchor's absolute
   position rounded to a multiple of `snap` on every axis, and one shift runs. At most one
   shift per tick.

A shift from `from` to `to`:

1. refuses (nothing changes) if `|to|` exceeds 1e12 m or ragdoll/deformable physics exists
   ([limits](#limits));
2. records the pose and velocity of every simulated dynamic and kinematic body;
3. moves every root `Transform` by `-delta` in f64 (rounded once); a root whose
   `engine.AbsolutePosition` is still exact gets `position - to` instead. Children keep their
   parent-relative transforms;
4. teleports every dynamic and kinematic body with `physics::set_body_pose` to its pose minus
   `delta`, **keeping linear and angular velocity**; fixed bodies follow their `Transform`
   (the physics sync rebuilds them in place);
5. updates `engine.AbsolutePosition` values, sets `WorldOrigin.offset = to`, refreshes world
   transforms, updates `WorldOriginStatus` and emits `world.origin_shifted`.

Because every offset produced by the threshold rule is a multiple of a power-of-two `snap`
below 1e12 m, offsets and deltas are exact in f64 (no drift however many shifts run), and
entities near the anchor move toward the origin, where their f32 value is exact.

### Invariants

- **Single owner**: only this service shifts entity transforms. Feature services never shift
  entities; nothing is shifted twice.
- absolute = `WorldOrigin.offset + local` holds for every entity before and after a shift
  (exact for `engine.AbsolutePosition` roots and entities moving toward the origin; within
  half an f32 step of the new local value otherwise).
- A shift never changes a velocity, rotation, scale or parent-relative transform.
- Services that cache world-space data outside entity transforms record the offset their
  data is expressed in and re-express it by `cached - current` when they see a different
  `WorldOrigin.offset`. Services that need absolute positions (cell grids, noise, wave
  phases) compute `offset + local` in f64.

## Subsystems

| Subsystem | Behaviour across a shift |
| --------- | ------------------------ |
| Physics (rigid) | Bodies are teleported through the public API (`set_body_pose`); velocities, rotations and joint frames are unchanged and contacts are found again in the same step. A resting box at 50 km moved 0 m and peaked at 1.6e-6 m/s through three shifts. Fixed bodies (terrain collider tiles) are rebuilt by the physics sync. Character controllers see the `Transform` write as a teleport (their ground state restarts; not separately tested). Every body is woken. |
| Navigation | Meshes, `target` and `waypoints` are absolute; queries use `offset + local`; each follower walks in the frame of the origin it was planned in and converts when writing its `Transform` (simulation/navigation.rs). |
| Streaming | Cell roots and HLOD proxies are root entities and shift with everything else; streaming reads the offset only to convert between absolute and local positions (its own rebase step was removed). |
| Terrain | Chunks and collider tiles are children of the terrain entity; the GPU render set uses the terrain's world matrix every frame. The PCG surface is re-registered with the new matrix, but a matrix that moved by exactly the origin delta (a pure translation) does not bump `PcgVolume.regenerate`: the ground is unchanged in absolute terms. Navigation input is absolute (`offset + local translation`). |
| Foliage | The instance field is translated into the new frame and the CPU clusters rebuilt; the moved terrain matrix is not treated as a ground change, so nothing regenerates ([foliage](world-foliage.md#origin-shifts)). |
| PCG | Output entities are roots and shift. Volumes on a terrain surface are not regenerated by a pure translation; noise is sampled at `offset + local`, so any regeneration lands at the same absolute places (tested: the same instances within 2 mm and `regenerate` unchanged). |
| Water | Wave phases are body-local (`k . x_local - w t`), so the surface moves with the body root and is shift-invariant; there is no absolute phase to rebase and no own shifting. Tested: a floating box and the Gerstner surface are unchanged in absolute terms across a rebase ([water](world-water.md#origin-shifts)). |
| 2D, audio, acoustics, UI | Read transforms each tick; nothing cached. |

## Events and telemetry

`world.origin_shifted` (untargeted, delivered with the next tick's events) carries
`delta`, `offset`, `previous` (f64 arrays), `shifts` and `cause` (`threshold`, `api`,
`external`).

`WorldOriginStatus` (resource, `status(world)`; also `RuntimeSnapshot::world_origin` once the
world uses an `engine.LargeWorld`, a nonzero offset or a shift; under 4 KiB):
`offset`, `shifts`, `last_delta`, `last_shift_tick`, `last_cause`, `settings` (entity id),
`anchor`, `anchor_distance`, `blocked` (why a due shift did not run) and at most 8
`diagnostics` (also written to the runtime log).

Systems that cache world positions watch `WorldOrigin.offset` (or `shifts`) and re-express
their data by the difference, as navigation and foliage do.

## Rendering

The renderer receives f32 local matrices only: `CameraInstance3D.world` and every
`MeshDraw3D.model` come from `WorldTransform`, which is local. With rebasing the camera stays
within `threshold` of the origin, so view-space positions (`inverse(camera) * model`) keep
f32 precision wherever the world is. Tested on CPU extraction (`extract_scene_frame`): a
scene authored at 1,000 km and at 10,000 km through `engine.AbsolutePosition` produces
view-space matrices within **6.1e-5** of the same scene at the origin (tolerance 1e-4);
without rebasing the difference is 5.3e-2 and 0.82. A shift moves the camera and the scene
by the same delta, so view-space positions, and therefore motion vectors computed from
previous and current view-projection times model, are unchanged across a shift. That
last property follows from the math and is not GPU-verified.

## Decision record

Requirement: positions safe beyond about 10 km for simulation, physics, navigation,
streaming and rendering, with minimal edits to transforms and the physics and render
boundaries.

| Option | Verdict |
| ------ | ------- |
| **A. Floating origin: f64 offset, f32 locals, one service shifts all roots (chosen)** | Keeps every f32 consumer (rapier, navigation, renderers, terrain/foliage/PCG) unchanged; one owner of shifting; exact repeated shifts on a power-of-two grid; f64 authoring through one optional component. |
| B. Keep per-feature rebasing (streaming shifted only its own roots) | Rejected: nothing else moved, two owners could shift the same entity twice, no physics or navigation story. Streaming's step was removed. |
| C. f64 `Transform`/`WorldTransform` everywhere plus camera-relative rendering | Rejected: changes the schema of every scene, every consumer of transforms, the render boundary and the physics boundary (rapier is built for f32); large migration for the same precision near the player. |
| D. f64 physics (rapier `f64` build) | Rejected: physics-owned, slower, and does not help navigation or rendering. |
| E. Per-tick camera-relative extraction only | Rejected: fixes rendering but not physics, navigation or simulation drift. |

Tradeoffs: a shift costs one pass over root entities and bodies, rebuilds fixed bodies and
re-registers terrain surfaces (PCG volumes on them do not regenerate for a pure translation); it happens only when the
anchor crosses the threshold. Revisit when a physics shift hook exists (lift the
ragdoll/deformable restriction), or if an f64 transform hierarchy is ever needed for
editing worlds larger than 1e12 m.

## Limits

- **Ragdolls, cloth, soft bodies and fluids block rebasing.** Their bones and particles live
  in physics-owned world space that the public physics API cannot translate. While any
  `engine.Ragdoll`, `Cloth`, `SoftBody` or `FluidVolume` exists, automatic rebasing waits
  (`blocked` names the entity) and `rebase_now` fails with `world.origin.unsupported`.
  Lifting this needs a physics-domain `shift_origin` hook.
- Entities without `engine.AbsolutePosition` that end up farther from the origin after a
  shift are rounded to the f32 step at their new distance (the agent in the navigation test
  moved 512 m away: 1.3e-5 m). Use `engine.AbsolutePosition` for static content far from the
  start, or keep the anchor near the content (the threshold rule does).
- Custom `PcgSurfaceRegistry` surfaces written in local coordinates must be re-registered on
  `world.origin_shifted`. (Generated foliage hashes absolute cells, so regenerating it under
  another origin gives the same forest.)
- Fixed bodies cannot be teleported (`set_body_pose` accepts dynamic and kinematic bodies
  only), so every terrain collider tile is rebuilt by the physics sync after a shift: one
  heightfield construction per tile, a one-off cost per shift that grows with the tile count
  (up to 1024 per terrain). A cheaper path needs a fixed-body pose hook in the physics crate.
- A due shift that cannot run (an anchor beyond 1e12 m, ragdoll or deformable physics) leaves
  the origin where it is: `WorldOriginStatus.blocked` names the reason, a diagnostic is
  logged and the tick succeeds.
- Navigation meshes, foliage and terrain assets are f32 data; content authored at 1e7 m in
  those formats has their f32 resolution there.
- The first rebase happens in the first tick; a frame rendered before the first tick sees the
  authored (unrebased) locals.
- Interpolated dynamic bodies show their latest simulated pose for one frame after a shift.

## Verification

crates/engine-project/tests/coordinates.rs (13 tests, numbers printed with `--nocapture`):

- A ball thrown at 50 km with rebasing (threshold 4 m, snap 4 m: 9 shifts in 3 s) matches the
  same flight near the origin within **4.3e-5 m** (tolerance 1e-3 m) with identical
  velocities; without rebasing the error is **1.35 m** (control, asserted >= 1e-2 m).
- An `engine.AbsolutePosition` entity at 1.2e7 m keeps its absolute position exactly (0 m,
  tolerance 1e-6 m) through 100 shifts; an entity shifted toward is exact.
- A shift keeps linear and angular velocity bit-identical and moves the pose by exactly
  `-delta`; a kinematic body is teleported, not swept (speed 0 after the next step).
- A resting box at 50 km through three shifts: moved 0 m, peak speed 1.6e-6 m/s.
- A navigation agent keeps walking around the obstacle across a shift, arrives at the
  absolute target and re-paths from the shifted frame.
- Terrain chunks, foliage instances and clusters and PCG output keep their absolute places
  (within 1 mm at mm rounding) and foliage does not regenerate; a body rests on the terrain
  collider through the shift; the camera anchor rebases at most once per tick on the grid; a
  direct `WorldOrigin` write is reconciled by exactly one shift; ragdoll/deformable physics
  blocks with a reason; invalid settings are rejected.
- crates/engine-project/tests/streaming.rs: streamed content is shifted exactly once.
- View-space render matrices at 1e6 and 1e7 m match the origin scene within 6.1e-5.
## Cinematic capture integration

The existing root-shift loop sends actual before/after positions to active cinematic
translation captures. Captures retain their authored key origin and f64 baseline, recognize
legitimate rebases without releasing ownership, and project subsequent root samples into the
applied frame. Child-local tracks do not shift their keys. This notification neither advances
cinematic clocks nor changes World-only origin shifting. Root capture acquisition latches the
applied origin through the coordinates service, just as a native coordinates API call does.
See [cinematic sequences](cinematics.md#floating-origins-and-local-frames) for frame lifetime,
external-write conflicts and f32 precision limits.

Cinematic root captures trust an AbsolutePosition baseline only while its freshness value
matches the local Transform. Existing exact-position state is then guarded and synchronized
with each owned root write and baseline restoration; foreign writes or component removal
release neither field back to the sequence. Rebase notifications include the actual exact-
position snapshots so ordinary service freshness updates do not look like external writes.
