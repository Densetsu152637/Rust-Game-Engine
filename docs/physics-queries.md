# Physics queries, fixed-step determinism and debug view

Three related features of the physics stack ([Physics core](physics.md)):

- **Queries**: ray casts, shape casts, overlaps and point queries over 2D and 3D bodies, with a
  shared filter, deterministic ordering and `Entity` results (`engine-physics` +
  `engine_project::simulation::physics`).
- **Fixed-step determinism**: `engine.PhysicsSettings.fixed_timestep` runs the simulation in
  whole fixed steps with a bounded catch-up, optional interpolation, and an optional
  `deterministic` cargo feature.
- **Debug view**: a bounded, render-agnostic snapshot of colliders, contacts and joint
  anchors, plus a line list a renderer or the editor can draw.

MCP exposes the first and last through `physics_query` and `physics_debug`
([MCP](mcp.md)).

## Queries

### Freshness and approach

Rapier's query pipeline is a broad-phase BVH rebuilt by `step`, so a body inserted or removed
between steps is invisible to it. The engine-physics queries therefore scan the collider set
directly, rejecting by AABB first for shape casts and overlaps, and call parry's exact
`cast_ray_and_get_normal`, `cast_shapes`, `intersection_test` and `project_point` on the
survivors. The cost is linear in the body count (the world caps it at 100 000) and results are
exact against the current state the instant a body is inserted or removed, the same guarantee
`World::raycast` always gave. At the project layer a body exists once the simulation synced it
(scene start, then each tick), so a body spawned in a tick is queryable after that tick's
sync and gone after the sync that noticed its removal.

### Hits, limits and ordering

| Query | engine-physics (`World2D`/`World3D`) | Native (`simulation::physics`, takes `Dimension`) | Hit |
| --- | --- | --- | --- |
| Closest ray | `raycast_hit(origin, dir, max, &filter)` | `ray_cast` | id/entity, `distance`, `point`, `normal` |
| All rays | `raycast_all(.., limit)` | `ray_cast_all` | sorted hits + `truncated` |
| Shape sweep | `cast_shape` / `cast_shape_all(shape, pos, rot, dir, max, &filter[, limit])` | `shape_cast` / `shape_cast_all` | `distance` (time of impact), `point`, `normal`, `shape_point`, `penetrating` |
| Overlap | `overlap_shape(shape, pos, rot, &filter, limit)` | `overlap` | id/entity, `distance` |
| Nearest point | `project_point(point, &filter)` | `project_point` | `point`, `distance`, `inside` |
| Containment | `contains_point(point, &filter, limit)` | `contains_point` | colliders with `inside` |

- Directions are normalized, so `distance` is in world units (for sweeps it equals the time of
  impact of a unit-speed shape).
- A ray starting inside a solid collider hits it at distance 0. A shape already overlapping its
  target reports `distance == 0` and `penetrating == true`; its `point`/`normal` come from the
  closest features and are approximate.
- Sweeps keep the shape's orientation fixed. Query shapes are the engine-physics `Shape`s (box,
  ball, capsule, cylinder in 3D, convex hull, ...) or the native `QueryShape`.
- Multi-hit results sort by `(distance, id)` (`f32::total_cmp`, then the body id, which is
  `Entity::to_bits()` at the project layer) **before** truncating to `limit` (1..=65536), so
  the kept hits are the nearest and ties resolve to the lowest id. `truncated` reports a cut.
  Overlap `distance` runs from the query position to each body origin; point queries use the
  distance to the surface (0 inside).
- 2D queries use XY; native callers pass `Vec3` and the Z is ignored (hit Z is 0). 2D rotations
  are about Z. A cylinder in a 2D query is an error.
- Accuracy (measured in `crates/engine-physics/tests/queries.rs`): ray hit point, normal and
  distance match box/sphere analytics within `1e-4`; a ball swept straight at a plane reports
  the analytic time of impact within `1e-4`; oblique sweeps converge by conservative
  advancement to about `2e-3`, and sweep witness points are GJK-accurate to about `2e-3`.

### `QueryFilter`

| Field | Meaning |
| --- | --- |
| `mask` | A collider is eligible when its layer membership bits intersect `mask` (default all). `layer_mask(&World, &[names])` builds one from named layers. |
| `exclude` | Bodies/entities never reported (at most 1024). |
| `sensors` | `Include` (default), `Exclude` or `Only`. |
| `kinds` | Which of fixed, dynamic and kinematic bodies are reported (default all). |

### Native example

```rust
use engine_project::simulation::physics::{self, Dimension, QueryFilter, QueryPose, QueryShape};
let filter = QueryFilter { mask: physics::layer_mask(world, &["world"])?, ..Default::default() };
if let Some(hit) = physics::ray_cast(world, Dimension::Three, eye, forward, 50., &filter)? {
    println!("{:?} at {} m, normal {}", hit.entity, hit.distance, hit.normal);
}
let ground = physics::shape_cast(world, Dimension::Three, &QueryShape::Ball { radius: 0.5 },
    QueryPose::at(feet), -Vec3::Y, 10., &filter)?;
```

## Fixed step and determinism

`engine.PhysicsSettings` gains (all additive, with the previous behaviour as the default):

| Field | Default | Range | Meaning |
| --- | --- | --- | --- |
| `fixed_timestep` | unset | 0.001..=0.1 s | When set, frame time accumulates and only whole steps of exactly this length run. |
| `max_substeps` | `8` | 1..=64 | Most fixed steps per frame (the spiral-of-death cap). |
| `interpolation` | `none` | `none`, `interpolate` | `interpolate` requires `fixed_timestep`. |

With `fixed_timestep` unset the service keeps the original behaviour (the frame delta, at most
1 s, split into substeps of about 1/60 s). With it set:

- Each frame delta (at most 10 s) is added to an `f64` accumulator; `floor(acc / h + 1e-3)` whole
  steps of `h` run and the remainder (possibly a tiny negative value from `f32` frame deltas)
  carries over. The tolerance makes `30 x 1/30 s` and `60 x 1/60 s` agree on the step count.
  Poses are therefore bitwise identical for any split of the same total time into frames, and
  two identical runs are bitwise identical (tested in `tests/physics_queries.rs`: splits of
  1 s into 60, 30, 20, 120, 3 and 6 uneven frames all give the same bits).
- At most `max_substeps` steps run per frame. Any further whole steps are dropped, never
  carried, so a long hitch cannot snowball. The frame is reported through
  `physics::step_status(&World) -> StepStatus { fixed_timestep, steps, total_steps, alpha,
  accumulator, clamped, dropped_time, clamped_frames }` and the event
  `physics.step.clamped` (payload `{dropped_seconds, max_substeps, fixed_timestep,
  clamped_frames}`) delivered to the `engine.PhysicsSettings` entity.
- `alpha` (also `physics::interpolation_alpha`) is the fraction of a step the accumulator
  holds, in 0..=1 (1 without a fixed step). With `interpolation: "interpolate"` the service
  writes dynamic body `Transform`s blended between the previous and the latest fixed step by
  `alpha` (lerp/slerp). Physics state, `body_state` and queries always use the true latest step;
  only `Transform` is blended. Kinematic and fixed bodies are not touched.
- Changing the debug toggles or stepping policy never wakes or rebuilds bodies.

### The `deterministic` cargo feature

`cargo build -p engine-physics --features deterministic` enables Rapier's
`enhanced-determinism` (`simba/libm_force` and `parry/enhanced-determinism`): floating point
maths in the physics step uses the pure-Rust `libm` instead of platform intrinsics, and
parallelism stays off. It is off by default because it costs speed.

What it guarantees: for the same engine build, the same inputs (initial bodies, order of
`insert`/`remove`/impulse calls, step sizes) produce the same results across platforms and CPU
architectures that implement IEEE-754 `f32` with round-to-nearest, as far as Rapier supports.
What it does not: it does not cover the rest of the engine (project code, other crates'
`f32` transcendental functions, the order of ECS iteration beyond what the service sorts);
different compiler versions or fast-math flags; or runs that differ in step sizes. Combine it
with `fixed_timestep` so the step sequence does not depend on the machine's frame rate. Without
the feature, runs are repeatable on one machine and build but not guaranteed across platforms.
Cross-machine bit equality was **not** verified here (single Windows x86-64 host); only that
the feature builds and the same-machine guarantees above hold.

## Debug view

`World::debug_view(&DebugOptions) -> DebugView` (engine-physics, per dimension) and
`physics::debug_view(&World)` / `debug_view_with(&World, &DebugViewOptions)` (native) return
plain data:

- `colliders`: id, `DebugShape` (ball, box, capsule, cylinder, compound parts, or the local
  bounds of hulls, meshes and heightfields), body pose, a `DebugState` colour key (`fixed`,
  `dynamic`, `kinematic`, `sleeping`, `sensor`, with `DebugState::color()` defaults) and an
  optional world AABB.
- `contacts`: world contact point, normal (first body toward second) and depth, for points at
  or below zero separation.
- `anchors`: joint anchors (every Rapier impulse joint, read from Rapier so joints created by
  any feature appear) and markers that other features add.
- `lines`: lines other features contribute.
- `totals` (uncapped counts) and `truncated`.

Everything is bounded: each of colliders, contacts, anchors and contributed lines is capped
(`DebugOptions`, at most 65536; `engine.PhysicsSettings.debug_max_items`, default 4096) and
sorted (colliders by id, contacts by ids then position, anchors by owner then position) before
cutting. `DebugView::wireframe(max_lines)` flattens the view to world-space segments with RGBA
colours (`physics::PhysicsDebugView::lines` merges both dimensions; 2D geometry lies on Z = 0).
Rendering is a separate step: a render feature can draw these segments as `Overlay`-stage
lines through `PipelineFeature` ([rendering architecture](rendering-architecture.md)), and the
editor can draw them directly.

Settings: `debug` (default false) gates the native `debug_view`; `debug_aabbs` adds AABBs;
`debug_max_items` sets the cap. `debug_view_with` ignores the toggle (tools use it).

Other physics features contribute with `World::add_debug_contributor(name, fn(&World, &mut
DebugSink))` (a duplicate `name` is ignored) and the native
`add_debug_contributor_2d/3d(&mut World, name, fn)`; the contributor pushes `anchor`s and
`line`s which the view sorts, caps and reports.

## MCP tools

Both tools are read-only, never execute game code (no native scripts are registered) and never
write files. They load a scene (default: the startup scene), start its physics headless,
advance `ticks` (0..=3600) ticks of `dt` (default 1/60 s) and answer from the result.

`physics_query` takes `kind`: `ray` (`origin`, `direction`, `max_distance`, `all`),
`shape_cast` (`shape`, `position`, `rotation`, `direction`, `max_distance`), `overlap`
(`shape`, `position`, `rotation`) or `point` (`point`, `contains`), plus `dimension`
(`"3d"` default or `"2d"`), `limit` (1..=1024) and `filter` (`layers`, `mask`, `exclude`
authored ids, `sensors`, `kinds`). `rotation` is a quaternion `[x,y,z,w]` (3D) or `[angle]` (2D); `shape` is `{type: ball|box|capsule|cylinder|convex_hull,
...}`. The result is `{dimension, ticks, count, truncated, hits: [{entity, entity_bits,
distance, ...}]}`.

`physics_debug` takes `ticks`, `dt`, `max_items` (1..=1024), `aabbs`, `contacts`, `joints`
and returns `{ticks, totals, states, line_segments, truncated, max_items, colliders, contacts,
anchors}` with authored entity ids.

Both fail with `invalid_argument` for malformed arguments and with the runtime's error for
scenes that cannot start headless (for example scenes that require registered native scripts).
