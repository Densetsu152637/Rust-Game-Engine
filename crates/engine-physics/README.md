# engine-physics

Independent headless 2D and 3D rigid-body worlds backed by pinned Rapier 0.36.1.
The crate has no ECS, project-format, device, or window dependency. A host owns
one world per simulation context and steps it with fixed seconds.

```rust
use engine_physics::{World3D, Material, dim3};
let mut world = World3D::new([0.0, -9.81, 0.0], 1024)?;
world.insert(7, dim3::BodyDesc::default(),
    dim3::Shape::Ball { radius: 0.5 }, Material::default())?;
world.step(1.0 / 60.0)?;
let position = world.pose(7).unwrap().position;
# Ok::<(), engine_physics::Error>(())
```

`BodyDesc` selects fixed, dynamic, or position-based kinematic bodies, initial
linear/angular velocity, rotation, and continuous collision detection. Each body
owns one collider. Shapes are boxes (half extents), balls, Y-axis capsules (half
segment height plus radius), 3D Y-axis cylinders, convex hulls of points (a convex
polygon in 2D), triangle meshes and heightfields (fixed or kinematic bodies only;
dynamic ones are rejected), and compounds of up to 64 posed convex parts. Point,
vertex, triangle, sample and part counts are capped (`MAX_HULL_POINTS`,
`MAX_MESH_VERTICES`, `MAX_MESH_TRIANGLES`, `MAX_HEIGHTFIELD_SAMPLES`,
`MAX_COMPOUND_PARTS`); degenerate hulls, out-of-range indices and non-finite values
fail. Density controls mass (`mass(id)` reports it); friction, restitution, their
`CombineRule` (average, min, max, multiply; on disagreement max > multiply > min >
average), sensor behavior, and 32-bit layer/mask pairs are configurable.
`CollisionLayers` maps up to 32 named layers and a symmetric collision matrix to
layer/mask pairs. `set_gravity`, `set_solver_iterations` (1..=64) and `velocity(id)`
expose world parameters and state.
`World2D` uses XY coordinates and radians. `World3D` uses XYZ and normalized XYZW
quaternions. Units are meters, seconds, kilograms. IDs are caller-assigned u64s;
duplicate IDs and unknown-body mutations fail.

Dynamic poses belong to physics. Use `apply_impulse` for dynamics and
`set_kinematic_target` for kinematics. Read poses after `step`; preserve prior
poses in the host for render interpolation. Valid timesteps satisfy 0 < dt <=
0.25 seconds. Worlds cap body count at their configured capacity (at most
100,000). Coordinates and velocities must be finite and bounded; dimensions
and density have explicit bounds to reject pathological simulation inputs.

`insert_joint(joint_id, body1_id, body2_id, JointDesc)` creates a real Rapier
constraint; `insert_joint_with` adds `JointOptions` (break thresholds, connected
contacts). `JointDesc` offers `Fixed`, `Revolute` (hinge, ordered limits in
[-pi, pi]), `Prismatic` (slider with linear limits), `Spherical` (3D ball with
optional swing cone and twist limits), `Spring`, `Distance` (min/max range),
`Rope` (max only) and `Generic` (every axis `Free`, `Locked` or `Limited`, in
arbitrary per-body `Frame`s). Joint IDs are independent of body IDs. Connected-body
contacts are disabled by default. Runtime edits without a rebuild: `set_joint_motor`
(position and/or velocity target, stiffness, damping, force cap, per `Dof`),
`clear_joint_motor`, `set_joint_limits`, `set_joint_break_limits`. Read-back:
`joint_state` (relative position/angles, force and torque), `joint_motor`,
`joint_limits`, `joint_bodies`, `joint_ids`. A joint whose constraint force or torque
exceeds its break threshold after a step is removed and reported by
`drain_joint_broken`; `broken_joint(id)` keeps the record. 2D has no spherical joint
(use `Revolute`). `remove_joint` removes a constraint and wakes its bodies; removing a
body automatically removes its joints and their ID mappings. Joint capacity is four
times body capacity. See [docs/physics-joints.md](../../docs/physics-joints.md).
`apply_angular_impulse` accepts a scalar in 2D or XYZ vector in 3D. Linear and
angular impulses also work immediately after insertion, before the first step.

A kinematic body becomes a character controller with `insert_character(id, CharacterDesc)`:
Rapier's `KinematicCharacterController` (slopes, sliding, snap to ground, push impulses) plus
gravity, `character_jump`, platform riding, an own autostep, `character_state(id)` and
`drain_character_events`. Drive it with `set_character_velocity`. See
[docs/physics-character.md](../../docs/physics-character.md).

`drain_events` reports sorted start/stop transitions for active solid contacts
and sensor intersections between fixed steps. Initial overlaps are reported on
the first step; removed bodies produce stop events on the next step. Events are
replaced on every step, so drain once per tick. They represent sampled contact
state, not every intermediate CCD substep. Layer/mask tests are bilateral.

`raycast` is an exact linear scan over current shapes, including sensors, and
works before the first step and immediately after removal. Its query mask tests
collider layers. Direction is normalized, distances are in world units, and
equal-distance hits choose the lower application ID. No broad-phase performance
claim is made for this query path.

This foundation does not yet expose character controllers, vehicles, shape casts, save-state serialization, or ECS
scheduling. It does not promise cross-platform bitwise determinism. New ECS
adapters should preserve legacy kinematic gameplay behavior and make rigid-body
authority opt-in.

Source layout: `world.rs` is the dimension-generic core compiled once by `dim2.rs`
and once by `dim3.rs`; features live under `src/features/` (joints are the
reference) and extend `World` through `impl World` blocks plus per-world side
tables using the crate-internal `Extension` hooks. See
[docs/physics.md](../../docs/physics.md#extension-seams) for the procedure.

Validation: `cargo test -p engine-physics` exercises real falling/settling bodies
in both dimensions, angular motion, impulses, masks, sensors, removal, queries,
kinematic targets, invalid inputs, and high-speed CCD against a thin wall.
Joint tests verify gravity-resistant fixed constraints, torque-driven hinges,
angle limits, explicit removal, and automatic cleanup in both dimensions, plus
slider drift and limits, ball anchor error and cone limits, spring period against
2*pi*sqrt(m/k), rope and distance ranges, 6-DoF locked and free axes, velocity and
position motors, and break force/torque (`tests/joints_extended.rs`). Shape
tests measure resting heights on heightfields, meshes, hulls, cylinders and
compounds plus hull/cylinder/compound masses; material tests measure sliding
distance and bounce height per combine rule; layer tests prove matrix-disabled
pairs never touch while enabled pairs do.

Spatial queries (`raycast_hit`, `raycast_all`, `cast_shape`, `cast_shape_all`, `overlap_shape`,
`project_point`, `contains_point`) take a `QueryFilter` (layer mask, excluded ids, sensor
and body-kind filters), scan the collider set so results are exact right after `insert`
and `remove`, and sort by `(distance, id)` before truncating to the caller's limit.
`debug_view(&DebugOptions)` returns a bounded `DebugView` (colliders with state colour keys,
contacts, joint anchors, optional AABBs, `wireframe(max_lines)` segments) and
`add_debug_contributor` lets other features add anchors and lines. The optional
`deterministic` cargo feature enables Rapier's `enhanced-determinism`. See
[Physics queries](../../docs/physics-queries.md).

Destruction support: `contact_impulse(id)` returns the normal contact impulse the solver
applied to a body in the last `step` (`ContactImpulse { impulse, point, other }`: summed
over its solid contacts, impulse-weighted world contact point, strongest partner), and
`is_sleeping(id)` / `sleep(id)` read and force Rapier's sleep state. They back
`engine.Destructible3D` ([Physics destruction](../../docs/physics-destruction.md)).
