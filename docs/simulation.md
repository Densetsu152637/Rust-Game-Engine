# Authored simulation

`Runtime::new` installs Rapier 2D/3D, animation playback and waypoint navigation.
All projects automatically expose `engine.PhysicsBody2D`, `engine.PhysicsBody3D`,
`engine.PhysicsSettings`, `engine.AnimationPlayer`, `engine.SkinBinding` and `engine.NavigationAgent` in the component catalog,
including MCP discovery. No game registration is required. Custom values use
`{"version":1,"data":{...}}` in the entity's `components.custom` map.
The `engine.*` namespace is reserved.

`Runtime::new_with_simulation(project, Some(SimulationConfig { ... }))` configures
gravity and per-dimension body capacity. Passing `None` disables these services;
authored simulation components then cause an explicit startup error.

Native scripts and ECS Update systems run first. Core's fallible
`tick_with_post_update` boundary then runs animation, navigation, physics, and
world-transform propagation before extraction/render. Service failure returns an
error, marks the core generation failed, and prevents that frame's render. The
profiler records this boundary as `PostUpdate`. Typed hydration precedes startup.

## Physics

Shapes, materials, combine rules, named layers, `engine.PhysicsSettings`, the native
`simulation::physics` API and the feature extension seams are documented in
[Physics core](physics.md).

Example data: `{"motion":"dynamic","shape":"box","half_extents":[0.5,0.5]}`.
3D half extents and velocity have three elements; angular velocity is scalar in
2D and three-dimensional in 3D. Shapes are `box`, `ball`, Y-axis `capsule`, 3D
Y-axis `cylinder`, `convex_hull`, fixed/kinematic `trimesh` and `heightfield`, and
`compound`. Other fields: `radius`, `half_height`, shape data (`points`,
`vertices`, `indices`, `heights`, `heightfield_*`, `parts`), initial `velocity`,
`angular_velocity`, `ccd`, and `material` (`density`, `friction`, `restitution`,
combine rules, `sensor`, `layer`, `mask`, `preset`, `layer_name`). Defaults come
from the catalog. Motion is `fixed`, `dynamic`, or `kinematic`. Layer/mask
filtering uses mutual intersection. Fixed sensors detect kinematic entries/exits.
Each entity has one body and one collider (a compound collider may hold several
parts); authored joint components are not supplied. Native `engine-physics`
worlds offer fixed/revolute joints and impulses.

Dynamic bodies own world translation/rotation. Direct Transform writes do not
teleport dynamic bodies. Changing body configuration rebuilds the body with the
current pose and configured initial velocity. Fixed bodies follow authored pose
changes; kinematic bodies follow native/navigation pose writes. Scale changes
rebuild colliders. Parent transforms convert simulated world poses back to local
TRS, parents first. Positive nonsingular scales and no shear are required;
balls, capsules, cylinders and compounds require uniform world scale. 2D rotation must be about world Z.
Legacy `Collider2D` cannot coexist with either physics body on the same entity.

Delta is bounded to one second when physics is active and subdivided into steps
of at most approximately 1/60 second. There is no wall-clock accumulator or
cross-machine determinism guarantee. Contact start/stop events target each live
participant, with the other participant as source and `{dimension,other}` payload.
Names are `physics.contact.start` and `physics.contact.stop`.
Rapier evaluates contacts before integration, so a newly reached kinematic target
is detected by the next physics substep.

## Animation and imported models

Player data: `{"clip":"walk","looping":true,"speed":1,"bindings":["root","hip"]}`.
`clip` references an `animation` asset containing version 1 JSON:

```json
{"version":1,"duration":1,"joints":1,"tracks":[{"joint":0,"translation":{"times":[0,1],"values":[[0,0,0],[1,0,0]],"interpolation":"linear"}}],"events":[{"time":0.5,"name":"step"}]}
```

Rotation values are normalized XYZW quaternions; scale values have three elements.
Channels support `linear` or `step`; channels, tracks and events may be omitted.
Files are bounded to 16 MiB; clips/bindings to 4096 nodes. Empty bindings target
the owning entity for a one-node clip. Missing or duplicate bindings are errors.
Prefab instantiation remaps every binding. Changing player configuration restarts
playback. `animation.event` targets the player owner with `{name,time,clip}`.

Bindings preserve complete glTF node order, including non-joint ancestors.
`imported_skeleton` and `imported_clip` convert importer data without compressing
the hierarchy. `skin_palette` computes mesh-local skin matrices suitable for
`engine_animation::skin_vertices`. Cubic spline clips remain in model metadata
and conversion explicitly rejects them. Runtime playback updates bound node TRS.
`engine.SkinBinding` data contains `model` (a model asset ID), `mesh_node` and
`primitive` source indices, and the full node `bindings` list. The service evaluates
these node world matrices after all pose writers and deforms source primitive
positions on the CPU. It publishes `GeometryOverride(Arc<Mesh>)` for render
extraction, preserving source indices, UV0 and vertex colors. Removing SkinBinding
removes its override. The source mesh stays immutable. Lighting derives geometric
normals from the deformed triangles; authored smooth normals are not retained in
the current render vertex format.

Runtime skinning bounds source models to 64 MiB each, 256 MiB total cached source
file size, 64 models, 4096 bound nodes per mesh and one million deformed vertices
across the world. Model and palette indices, weights and matrices are validated;
bad bindings fail before rendering. Scene changes must call `simulation::reset`
after hydration so retained entities rebuild from their new scene poses.
Native animation supplies crossfades and CPU skinning; there is no authored state
machine, inverse kinematics, retargeting or morph animation.

## Navigation and ownership

Data: `{"waypoints":[[1,0,0],[2,1,0]],"speed":2}`. Waypoints are world coordinates;
the follower converts movement to local coordinates under parents. Changing
configuration replaces the path from the current world position. One
`navigation.arrived` event is emitted on completion of a nonempty path. Native
`engine-navigation::Grid` and `Graph` provide bounded path searches; game code
can convert their returned nodes into these waypoints. Alternatively an agent with
`navmesh` and `target` fields follows paths found on a baked layered polygon NavMesh
asset (see [Navigation meshes](navigation.md)). There is no crowd or dynamic obstacle
avoidance.

Animation cannot share any target with another animation, physics or navigation
writer. Navigation may drive a kinematic body, but cannot share a fixed/dynamic
body. Native code must respect the same ownership when directly writing transforms.
All simulation events use the existing bounded event queue and reach callbacks
at the beginning of the next runtime tick. Overflow returns an error.
