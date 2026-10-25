# Physics joints

Joints constrain two rigid bodies. They are solved by Rapier 0.36.1 impulse joints and exist
in both worlds: `engine-physics` (`World2D`/`World3D`, see its
[README](../crates/engine-physics/README.md)) and the authored components
`engine.PhysicsJoint2D` / `engine.PhysicsJoint3D`. Units are meters, seconds, kilograms and
radians; forces are N and N*m. General physics concepts are in [Physics core](physics.md).

## Joint kinds

| `kind` | Constrains | Parameters |
| --- | --- | --- |
| `fixed` | All relative motion. | `anchor_a`, `anchor_b` |
| `revolute` | Hinge about Z (2D) or `axis` (3D). | `axis` (3D), `limits` (radians, ordered, within -pi..=pi) |
| `prismatic` | Slider along `axis`. | `axis`, `limits` (meters, ordered) |
| `spherical` | Ball-and-socket (3D only). | `axis` (twist axis), `swing_limit` (cone angle, 0..=pi), `twist_limits` |
| `spring` | Spring-damper on the anchor distance. | `rest_length`, `stiffness` (N/m), `damping` |
| `distance` | Anchor distance within a range. | `min_distance`, `max_distance` |
| `rope` | Anchor distance at most `max_distance`; slack allowed. | `max_distance` |
| `generic` | Per-axis 6-DoF (3-DoF in 2D). | `axes`, `rotation_a`, `rotation_b` |

2D has one rotation axis, so a 2D `spherical` joint is rejected with "spherical joints are 3D
only; use revolute in 2D" (the 2D schema does not list the kind). A `rope` is a `distance` with
minimum 0. Rapier's coupled distance limit natively supports only a maximum, so a positive
`min_distance` is enforced by an engine-physics hard stop: a velocity-level motor that arms when
the bodies are closing in and the next step would reach the minimum, and stays armed while it
pushes the bodies apart (a load presses them together). The stop is **one-sided**: when gravity
or another force pulls the bodies apart at rest at the minimum, the stop (which would only hold
them together) is released and they separate. Measured: a vertical joint (min 0.8 m, max 1.2 m)
released at its minimum under separating gravity reaches 1.2000 m in 2 s (before: stuck at
0.8000 m); the same joint holds a pressing load at 0.8000 m with a closest approach of 0.7996 m
over 3 s. The `lin_x` motor of such a joint is reserved (motors are rejected on it).

Anchors (`anchor_a`, `anchor_b`) are positions in each body's local space; they are not scaled
by `Transform` scale. `generic` joints use the anchors and `rotation_a`/`rotation_b` (radians in
2D, XYZW quaternion in 3D) as the two joint frames. In a frame, `lin_x`/`lin_y`/`lin_z` are
translations and `ang_x`/`ang_y`/`ang_z` rotations about the frame axes (2D: `lin_x`, `lin_y`,
`ang`). `axes` lists only the non-default degrees of freedom: each entry is `{"axis","mode"}`
with `mode` `free`, `locked` or `limited` (plus `min`/`max`; angular limits within -pi..=pi). An
unlisted axis is locked. `axes` is rejected on other kinds.

## Motors

`motors` (at most one per axis, six entries) drive a degree of freedom of the joint:

```json
{"engine.PhysicsJoint3D":{"version":1,"data":{
  "kind":"revolute","body_b":"base","axis":[0,0,1],
  "motors":[{"axis":"ang_x","target_velocity":2.0,"damping":50.0}],
  "break_force":4000.0
}}}
```

- Velocity motor: `target_velocity` with `damping` as the gain (`stiffness` 0).
- Position motor: `target_position` with `stiffness` and `damping`.
- `max_force` caps the motor (N or N*m; omitted is unlimited). `force_based: false` (default)
  scales stiffness and damping with mass (Rapier acceleration model); `true` makes them
  absolute forces.
- Motors drive body B relative to body A: a positive target velocity turns body B
  counter-clockwise relative to A, so A itself turns the other way when B is fixed.
- A hinge is driven on `ang_x` (3D, about the hinge `axis`) or `ang` (2D); a slider, spring,
  rope and distance on `lin_x`; a spherical joint on `ang_x`/`ang_y`/`ang_z`; a generic joint on
  its non-locked axes. Motors on locked axes are rejected ("motor axis is locked for this joint
  kind").

Motor and break-threshold edits are applied to the live constraint without rebuilding it.
Any other change (kind, anchors, limits, axes, references) re-creates the constraint from the
authored data.

## Breakable joints

`break_force` (N) and `break_torque` (N*m) are thresholds on the magnitude of the constraint
load after each step. Rapier writes back the impulses of its constraint rows; the engine sums
the locked, limit and motor rows per axis, divides by the solver substep, and reports

- `force`: the force transmitted through the center of mass of the dynamic body (frame-1 axes),
- `torque`: the moment about the joint anchor (the angular rows plus the lever arm of the
  force; frame-1 axes).

For a body of mass m hanging from a fixed joint the force is m*g (measured within 5%). When
both bodies are dynamic the torque is evaluated about the second body's center of mass. When
either threshold is exceeded the joint is removed and a `JointBroken { id, first, second,
force, torque }` record is queued. The record stays queryable (`broken_joint`) until the joint
is removed or re-created. engine-physics keeps the last 1024 records and evicts the **oldest**
break first (not the lowest id; measured: after 1200 breaks in three waves the 1024 newest
remain). An authored joint stays broken even after its record was evicted: the project
remembers the break, so the next sync does not re-create the joint (before, evicted joints came
back and broke again every tick). Once evicted, `joint_status` reads `Inactive` instead of
`Broken`.

## Authored component

`engine.PhysicsJoint3D` / `engine.PhysicsJoint2D`. Defaults come from the catalog; all edits
work through MCP `component_set` / `scene_edit`.

| Field | Meaning |
| --- | --- |
| `kind` | See above (default `fixed`). |
| `body_a` | Entity id of the first body; omitted means the entity carrying the component. |
| `body_b` | Entity id of the second body. Required. |
| `anchor_a`, `anchor_b`, `rotation_a`, `rotation_b`, `axis` | Frames and axis, body-local. |
| `limits`, `swing_limit`, `twist_limits` | Kind-specific limits. |
| `rest_length`, `stiffness`, `damping` | Spring. |
| `min_distance`, `max_distance` | Distance and rope. |
| `axes` | `generic` degrees of freedom. |
| `motors` | See above. |
| `break_force`, `break_torque` | Break thresholds. |
| `collide_connected` | Whether the two bodies still collide (default `false`). |

The component goes on the entity that owns body A (then `body_a` is omitted) or on a standalone
entity with explicit `body_a` and `body_b`. `body_a` and `body_b` are descriptor
`entity_references`: scenes reject unknown ids and prefab instantiation remaps them like other
references. Both bodies must carry `engine.PhysicsBody2D` (for a 2D joint) or
`engine.PhysicsBody3D` (3D); a dimension mismatch, a missing `body_b`, a joint to itself,
non-finite or unordered values, oversized `axes`/`motors`, and motors on locked axes all fail
scene validation with a precise message. An id that no longer resolves at runtime (the entity
was despawned) leaves the joint inactive instead of failing the tick. A joint to a body that
**fractured** (`engine.Destructible3D` removed its `PhysicsBody3D` and added `Fractured`, on
either endpoint, or on the joint's own entity) is inactive the same way: the sync removes the
constraint, `joint_status` reads `Inactive`, it is not reported as broken, no event is emitted
and the scene stays valid (before, validation failed on every tick from then on). If a body is rebuilt
(its component changed, or a fixed body moved) the joint is re-created from the authored data;
a joint that broke stays broken until its component is edited, also across a period in which one
of its bodies is missing (starved, or a fractured body that returns): the break is remembered, so
the joint is not re-created intact when the body comes back.

Event `physics.joints.broken` is delivered to the joint entity and both body entities:

```json
{"dimension":3,"joint":"crate","force":51.4,"torque":0.0,"body_a":"crate","body_b":"ceiling"}
```

## Native API

`engine_project::simulation::physics`:

| Function | Purpose |
| --- | --- |
| `add_joint_2d` / `add_joint_3d(&mut World, Entity, PhysicsJointND)` | Validate and attach a joint component. |
| `remove_joint(&mut World, Entity, Dimension)` | Remove the component (the constraint goes on the next sync). |
| `set_joint_motor` / `clear_joint_motor` | Update one axis motor in place. |
| `set_joint_break_limits` | Replace the break thresholds (editing a broken joint re-creates it). |
| `joint_status(&World, Entity, Dimension)` | `Active`, `Broken { force, torque }` or `Inactive`. |
| `joint_state(&World, Entity, Dimension)` | `JointReadout`: relative position, angles, force and torque. |
| `broken_joints(&World)` | Every broken joint by entity, with its load. |

Changes made through the API are applied at the next simulation sync (the next tick).

## Measured behaviour

From `crates/engine-physics/tests/joints_extended.rs` and
`crates/engine-project/tests/physics_joints.rs` (all pass):

- Prismatic: off-axis drift below 1e-3 under gravity and sideways kicks; stops within 0.02 m of
  its limits in 2D and 3D; limits edited at runtime.
- Spherical: anchor separation below 1e-3 for 3 s of swinging; a 0.5 rad cone is reached
  (above 0.45) and not exceeded by more than 0.03 rad; twist limits hold.
- Spring: period within 5% of 2*pi*sqrt(m/k) for m = 1.5 kg, k = 60 N/m in 2D and 3D; damping
  settles the mass.
- Rope: max stretch 1.5 m + 0.02; free fall through the slack; distance min/max hold.
- Generic 6-DoF: locked axes stay within 1e-3; free and limited axes move; frames rotate the free
  axis.
- Motors: velocity motor reaches 2.5 rad/s within 0.1; position motors converge to the target
  angle within 0.03 rad and retarget in place; `max_force` bounds acceleration.
- Breaking: a 5 kg load holds under a 100 N threshold; a 20 kg load breaks it and reports a
  force within 10% of m*g; a cantilever load breaks on torque within 15% of m*g*L.

## Limitations

- Rapier's coupled linear limit has no native minimum; see the distance note above.
- A joint reports the load of the last step only; breaking is evaluated per physics substep.
- Sleeping bodies keep their joints; editing a motor or limit wakes both bodies.
- Joint anchors ignore `Transform` scale.
- A joint that does not fit in the joint table (four per body slot) is skipped and counted with
  the starved bodies in `pressure_status`; see [Physics core](physics.md#pressure-and-capacity).
