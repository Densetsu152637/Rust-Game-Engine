# Physics core

Rigid-body physics has two layers:

- `crates/engine-physics`: ECS-free Rapier 0.36.1 worlds (`World2D`, `World3D`) with
  caller-assigned `u64` body IDs. See its [README](../crates/engine-physics/README.md).
- `crates/engine-project/src/simulation/physics/`: the authored components
  (`engine.PhysicsBody2D`, `engine.PhysicsBody3D`, `engine.PhysicsSettings`), the service that
  mirrors them into engine-physics every tick, and the native Rust API.

Units are meters, seconds and kilograms. 2D uses XY and radians about Z; 3D uses XYZ and
XYZW quaternions. Body behaviour, transform authority, substepping and contact events are
described in [Authored simulation](simulation.md#physics).

## Shapes

Every body owns exactly one collider. `shape` selects its geometry; the parameters live
in flat fields of the body component (and in the engine-physics `Shape` enum variants).

| `shape` | Fields | Dimensions | Notes |
| --- | --- | --- | --- |
| `box` | `half_extents` | 2D, 3D | Per-axis world scale. |
| `ball` | `radius` | 2D, 3D | Uniform world scale. |
| `capsule` | `radius`, `half_height` | 2D, 3D | Y axis; uniform world scale. |
| `cylinder` | `radius`, `half_height` | 3D | Y axis; uniform world scale. |
| `convex_hull` | `points` | 2D, 3D | Hull of 3 (2D) / 4 (3D) to 4096 points; interior points are ignored. Collinear, coplanar or zero-volume sets are rejected. Points scale per axis. |
| `trimesh` | `vertices`, `indices` | 2D, 3D | Fixed or kinematic only. 3..=65536 vertices, 1..=131072 triangles, every index < vertex count. |
| `heightfield` | `heights`, `heightfield_scale` (3D also `heightfield_rows`, `heightfield_columns`, `heightfield_holes`) | 2D, 3D | Fixed or kinematic only. See below. |
| `compound` | `parts` | 2D, 3D | 1..=64 rigid parts; uniform world scale. |

Heightfields are centred on the body origin. In 2D, `heights` (2..=1048576 samples) are
evenly spaced along X over `heightfield_scale[0]` meters and multiplied by
`heightfield_scale[1]`. In 3D, `heights` is row-major with `heightfield_rows` samples along
Z, each holding `heightfield_columns` samples along X (both at least 2, product at most
1048576 and equal to `heights.len()`); the grid spans `heightfield_scale[0]` x
`heightfield_scale[2]` meters and heights are multiplied by `heightfield_scale[1]`. 3D
heightfields fix internal edges so rolling bodies do not bump at triangle seams.
`heightfield_holes` (3D) lists removed cells by row-major index over
`(heightfield_rows - 1) x (heightfield_columns - 1)`; both triangles of a listed cell are
removed, so bodies and queries pass through it. An index outside the grid is rejected.

Compound `parts` each carry `shape` (`box`, `ball`, `capsule`, `cylinder` in 3D, or
`convex_hull`), a body-local `position` and `rotation` (radians in 2D, quaternion in 3D),
and that shape's `half_extents`, `radius`, `half_height` or `points`. Meshes, heightfields
and nested compounds are rejected. Mass, contacts and ray hits come from all parts.

Dynamic `trimesh` or `heightfield` bodies are rejected explicitly ("triangle mesh and
heightfield shapes require fixed or kinematic bodies"): Rapier gives triangle meshes
surface-only contacts and no meaningful volume, so use `convex_hull` or `compound` for
dynamic geometry. Authored mesh data is inline and bounded by the 64 KiB component limit;
larger meshes can be inserted through the engine-physics API directly. Referencing an
imported model mesh as collider source is not implemented yet.

## Materials and combine rules

`material` fields: `density`, `friction`, `restitution`, `friction_combine`,
`restitution_combine`, `sensor`, `layer`, `mask`, and optional `preset` and `layer_name`.
Combine rules are `average` (default), `min`, `max` and `multiply` and map to Rapier's
`CoefficientCombineRule`. When two touching colliders use different rules, the
higher-priority one wins: `max > multiply > min > average`.

Measured in `crates/engine-physics/tests/materials_layers.rs` (1 m box at 5 m/s on a
frictionless ground, box friction 1.0, 1.5 s): `min` and `multiply` slide 7.50 m,
`average` 2.54 m, `max` 1.04 m. A restitution-1.0 ball dropped from 3 m onto a
restitution-0 ground peaks at 0.58 m with `min` (no bounce), 1.10 m with `average`, and
2.98 m with `max`.

`preset` replaces density, friction, restitution and both combine rules with a named
`engine.PhysicsSettings` material; `sensor`, `layer` and `mask` stay inline. Unknown
presets are rejected.

## Collision layers

`layer`/`mask` are raw 32-bit membership and filter bits: a pair interacts only when each
side's layer meets the other's mask. Named layers build on this: `PhysicsSettings.layers`
declares up to 32 names, layer *i* owns membership bit *i*, and its filter is row *i* of a
symmetric collision matrix. Every pair collides until listed in `disabled_pairs`
(`[["player","debris"]]`; `[a, a]` disables self-collision). A body selects a named layer
with `material.layer_name`, which replaces its raw `layer`/`mask`; unknown names are
rejected. Disabled pairs never generate contacts or responses; enabled pairs do. Raw
values remain valid, and a raw body whose layer bits include bit *i* behaves like a member
of layer *i*. Raycast masks test collider layer bits; `physics::layer_mask` builds one
from layer names.

In engine-physics, `CollisionLayers::new(names)`, `set_collides(a, b, enabled)`,
`collides`, `groups(name) -> (layer, mask)`, `apply(material, name)` and
`query_mask(names)` provide the same behaviour.

## `engine.PhysicsSettings`

At most one per scene, on any entity (no `Transform` needed). Two settings entities, or a
settings entity while simulation services are disabled, fail scene start.

```json
{"engine.PhysicsSettings":{"version":1,"data":{
  "gravity_2d":[0,-9.81],"gravity_3d":[0,-9.81,0],"solver_iterations":4,
  "layers":["world","player","debris"],
  "disabled_pairs":[["player","debris"]],
  "materials":[{"name":"ice","friction":0,"friction_combine":"min"}]
}}}
```

- `gravity_2d`, `gravity_3d`: optional overrides of `SimulationConfig` gravity.
- `solver_iterations`: Rapier solver iterations, 1..=64 (default 4).
- `layers`, `disabled_pairs`: named layers (unique, 1..=64 chars of `[A-Za-z0-9_.-]`) and
  the matrix described above (at most 528 pairs, names must exist).
- `materials`: up to 64 named materials (`name`, `density`, `friction`, `restitution`,
  `friction_combine`, `restitution_combine`), validated with the same bounds as inline
  materials; names are unique.

- `fixed_timestep`, `max_substeps`, `interpolation`: fixed-step accumulator with a bounded
  catch-up and optional pose interpolation (all optional; see
  [Physics queries](physics-queries.md#fixed-step-and-determinism)).
- `debug`, `debug_aabbs`, `debug_max_items`: debug-view data toggle and caps.

Changing gravity or iterations applies on the next tick and wakes sleeping bodies.
Changing layers or materials rebuilds every body from its authored data at the pose and
velocity it has now (see [Authored simulation](simulation.md)); ragdoll bone bodies are rebuilt
in place too, with the new groups. The component is MCP-editable through `component_set` like
other catalog components.

### Pressure and capacity

The engine event queue holds 1024 events and the body table `SimulationConfig::body_capacity`
(default 16384 per dimension, shared by authored bodies, fragments and ragdoll bones). Reaching
a capacity limit (the event queue, the body table, a deformable's proxy limit) never fails a
step: it degrades in a bounded, counted way and says so. What still fails a step is invalid
input: a rejected authored value, an invalid native-API argument, or a solver fault that cannot
be recovered. The guarantee covers capacity, not correctness.

- **Events.** `physics::emit` (every physics event, including `physics.fluid.capacity_reached`) drops an event when the queue is full and counts it. The next step
  delivers one `physics.events.dropped` event, `{dropped, total}`, to the entity carrying
  `engine.PhysicsSettings` (broadcast when the scene has none). Example: 700 joints breaking in
  one step queue 1400 events; 1024 are delivered, 376 are dropped and counted, and the step
  succeeds (before: "Event queue capacity exceeded" aborted the step after the physics world had
  stepped but before the poses were written back). Contact events are the bulk traffic, so in
  one physics frame they may use only 7/8 of the queue (896 of 1024): the last eighth is reserved
  for the rare events (`physics.joints.broken`, `physics.destruction.fractured`, ...), which are
  dropped only when the whole queue is full. Contacts drop first and are counted like the rest.
- **Bodies.** A body, fragment or joint that does not fit is not inserted. Its entity is
  *starved* and retried at every sync, so it simulates as soon as room appears (a ragdoll needs
  room for all its bones at once and reports "physics body capacity exceeded" in its status).
  The step after entities newly became starved delivers one `physics.capacity.exceeded`,
  `{capacity, starved, new}`. Example: a capacity of 20 with 10 rocks and a 24-fragment fracture
  leaves 14 fragments without a body, one warning, and ticks that keep succeeding; despawning
  the rocks gives 10 of the 14 their bodies.
- **Solver proxies.** A cloth or soft body couples with at most 1024 nearby rigid bodies, a
  fluid volume with at most 256. With more bodies near it, the nearest ones are kept (ties by
  entity id, so the choice is deterministic) and the others are left out of that step's
  coupling: they neither collide with the deformable nor receive reactions from it. The omissions
  are counted (`PressureStatus::proxies_omitted`) and each episode is announced once with
  `physics.proxies.limited`, `{omitted, total}`; an episode ends with a step that omits nothing.
- **Refused fractures.** An impact- or damage-triggered fracture that cannot complete (for
  example a fragment density outside 0.000001..=10000 kg/m^3) is refused: the body stays intact,
  one `physics.destruction.refused` `{dimension, reason}` event is delivered, and
  `destructible_status(..).refused` holds the reason. The trigger stays off until the
  component or the body is edited, so a resting load does not retry every tick. (`fracture_now`
  still returns the error to its caller.)
- `physics::pressure_status(&World) -> PressureStatus { body_capacity, bodies_2d, bodies_3d,
  starved_bodies, events_dropped, proxies_omitted }` reads the counters.

## Native API

Native scripts and systems receive `&mut World`; `engine_project::simulation::physics`
provides:

| Function | Purpose |
| --- | --- |
| `body_state(&World, Entity) -> BodyState` | World position, rotation, linear and angular velocity, and `Dimension`. |
| `set_body_pose(&mut World, Entity, position: Vec3, rotation: Quat, velocity: Option<(Vec3, Vec3)>)` | Teleports a dynamic or kinematic body (2D needs a rotation about Z and planar velocities), wakes it, writes the `Transform` at once and restarts interpolation from the new pose. `None` keeps the velocities. Contacts and joints are not touched, so a jointed partner is pulled along by the next step. Fixed bodies move by editing their `Transform`. This is the way to move a dynamic body: plain `Transform` writes lose to the simulation. |
| `apply_impulse(&mut World, Entity, Vec3)` | Linear impulse on a dynamic body (2D requires `z == 0`). |
| `apply_angular_impulse(&mut World, Entity, Vec3)` | Angular impulse (2D uses only `z`). |
| `raycast_2d` / `raycast_3d(&World, origin, direction, max_distance, mask)` | Closest hit as `RaycastHit { entity, distance }`. |
| `layer_mask(&World, &[&str]) -> u32` | Query mask from named layers. |
| `ray_cast`, `ray_cast_all`, `shape_cast`, `shape_cast_all`, `overlap`, `project_point`, `contains_point` | Spatial queries with a `QueryFilter`; see [Physics queries](physics-queries.md). |
| `step_status`, `interpolation_alpha` | What the last frame did under `fixed_timestep`. |
| `debug_view`, `debug_view_with` | Bounded debug-view data; see [Physics queries](physics-queries.md#debug-view). |
| `body_id(Entity) -> u64`, `body_entity(u64) -> Option<Entity>` | Entity <-> engine-physics ID (`Entity::to_bits`). |
| `physics_worlds(&World) -> (&World2D, &World3D)` | Read-only engine-physics worlds for other queries. |

Bodies exist once the simulation synced them (scene start, then every tick); an entity
spawned during the current tick reports "Entity has no synced physics body" until the next
step. Errors are `simulation.invalid` with a precise message.

## Joints

Joints (fixed, revolute, prismatic, spherical, spring, distance, rope, generic 6-DoF; limits,
motors, break thresholds) are described in [Physics joints](physics-joints.md).

## Character controllers

Kinematic characters (`engine.CharacterController2D/3D`: slopes, autostep, snapping, platform
riding, pushing, gravity, jumps, ground and wall events) are described in
[Physics character controllers](physics-character.md).

## Extension seams

Later physics features (joints, character controllers, vehicles, ragdolls, destruction,
collider proxies, queries, debug views) plug in without editing shared core files.

### engine-physics (`crates/engine-physics/src`)

`world.rs` is the dimension-generic core, compiled once per dimension: `dim2.rs` and
`dim3.rs` each include it with `#[path = "world.rs"] mod world;` and supply
dimension-specific names (`Position`, `Orientation`, `BodyDesc`, `Shape`, `vector`,
`rotation`, the Rapier prelude) to `use super::*`. To add feature `foo`:

1. Create `src/features/foo.rs` (dimension-generic) or `src/features/foo/{dim2,dim3,shared}.rs`
   when descriptors differ by dimension (`dimN.rs` declares the types and includes
   `#[path = "shared.rs"] mod shared;`). `features/joints/` is the reference.
2. Register it with one item per dimension file, next to the joints line:
   `#[path = "features/foo.rs"] mod foo;` (or `features/foo/dim2.rs` / `dim3.rs`), plus a
   `pub use foo::{...};` for public types.
3. Add `impl World { ... }` blocks for the public API.
4. Keep per-world state in a `#[derive(Default)]` side table implementing the crate-internal
   `Extension` trait; get it with `self.extension::<T>()` (read) or
   `self.extension_mut::<T>() -> (&mut Core, &mut T)` (created on first use).
5. `Core` exposes `rapier: PhysicsWorld` (sets, pipelines, gravity, integration
   parameters), `entry(id) -> BodyEntry { body, collider }`, `body(id)`, `body_mut(id)`,
   `body_id(RigidBodyHandle)`, `collider_body_id(ColliderHandle)` and `capacity()`.
   Create application bodies only through `World::insert`/`remove`; Rapier
   `user_data` holds the application ID.
6. Hooks: `pre_step(&mut Core, dt)` before Rapier integrates, `post_step(&mut Core, dt)`
   after Rapier and core contact events, `body_removed(&mut Core, id)` after a body and its
   Rapier joints/colliders were removed. They run in `(order(), type name)` order;
   `order()` defaults to 100. Feature output (events) stays in the side table behind a
   `drain_*` method.

The crate allows `clippy::duplicate_mod` because sharing one source between the two
dimension modules is deliberate.

### engine-project (`crates/engine-project/src/simulation/physics/`)

`mod.rs` owns the `Physics` service and the `Feature` trait. To add feature `foo`:

1. Create `physics/foo.rs` implementing `Feature` (`name()` returns `"foo"`).
2. Add `mod foo;` in `physics/mod.rs` and ONE entry in `fn features()`; registry order is
   execution order.
3. Declare authored components in `Feature::descriptors()` (use the `engine.` namespace;
   the catalog picks them up through `simulation::component_descriptors`) and register
   their adapters in `Feature::register_components(app)`.
4. Hooks, each with a `Context { worlds, bodies, settings }` (`worlds.two`/`worlds.three`
   are the engine-physics worlds; `bodies.two`/`bodies.three` map `Entity` to the synced
   authored config and pose; `settings` is the resolved `PhysicsSettings`):
   `validate(&World, &Resolved)` every prepare/step before sync; `sync(cx, &World)` after
   bodies were mirrored; `before_substep(cx, dt)` and `after_substep(cx, &mut World, dt)`
   around each engine-physics substep; `after_step(cx, &mut World)` after dynamic poses
   were written to `Transform`s on frames that ran a step; and `after_frame(cx, &mut World,
   Frame { alpha, steps })` on **every** frame, including frames that ran no step, after the
   interpolated body poses were written. Use `after_frame` for anything another writer
   rewrites every frame (the animation service rewrites ragdoll bones) or that must follow an
   interpolated body (wheel visuals, characters); `alpha` is the interpolation blend factor
   (1 without interpolation). `Context` also carries `frame_dt` (the frame's delta, the
   animation time since the last sync) and, in `before_substep`, `progress` (`k/n` after step
   k of n, for spreading a per-frame target over the steps).
5. Emit events with `physics::emit(world, self, "event", target, source, payload)`, which
   names them `physics.<feature>.<event>` and drops them, counted, when the event queue is
   full (see [Pressure and capacity](#pressure-and-capacity)). The contacts feature (`physics/contacts.rs`)
   is the reference and emits `physics.contact.start`/`stop`.
6. Native API functions live in the feature module and are re-exported from
   `physics/mod.rs` with one `pub use foo::{...};` line; use the crate-internal
   `service`/`service_mut` helpers to reach the worlds.
7. Feature tests go in a new `crates/engine-project/tests/physics_foo.rs`; update the
   `engine-mcp` component list test when a component is added.
