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
fixed or revolute constraint. Joint IDs are independent of body IDs. Fixed
joints lock local anchors together and align the local rotation frames;
revolute joints keep their local anchors together while allowing hinge rotation.
The 2D hinge rotates about Z. The 3D hinge takes a nonzero axis, normalized and
interpreted in both bodies' local frames. Optional hinge limits are ordered
radians in [-pi, pi]. Connected-body contacts are disabled. `remove_joint`
removes a constraint and wakes its bodies; removing a body automatically removes
its joints and their ID mappings. Joint capacity is four times body capacity.
`apply_angular_impulse` accepts a scalar in 2D or XYZ vector in 3D. Linear and
angular impulses also work immediately after insertion, before the first step.

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

This foundation does not yet expose joint motors, arbitrary joint frames,
character controllers, vehicles, shape casts, save-state serialization, or ECS
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
angle limits, explicit removal, and automatic cleanup in both dimensions. Shape
tests measure resting heights on heightfields, meshes, hulls, cylinders and
compounds plus hull/cylinder/compound masses; material tests measure sliding
distance and bounce height per combine rule; layer tests prove matrix-disabled
pairs never touch while enabled pairs do.
