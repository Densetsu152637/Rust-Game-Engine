# Skeleton ragdolls

`engine.Ragdoll` turns an animated skeleton into physics bodies. Every listed bone entity gets
one Rapier body (a capsule, box or ball between its joint and its child joint) and a joint to its
parent bone. Three modes and a blend weight decide how the simulated pose reaches the bone
`Transform`s, so the same skeleton can play animation, go limp, or be driven by physics toward the
animation. General physics concepts are in [Physics core](physics.md); the joint solver is
described in [Physics joints](physics-joints.md). Units are meters, seconds, kilograms, radians.

## Modes

| `mode` | Bones | Pose written to the skeleton |
| --- | --- | --- |
| `animated` (default) | Kinematic, placed on the animation pose each tick (hit detection, pushing bodies). | Nothing; animation owns the pose. |
| `ragdoll` | Dynamic, limp; only the joint limits and contacts act. | `blend` of animation and physics. |
| `powered` | Dynamic; joint motors drive every bone toward the animation pose (active ragdoll). | Same. |

- `blend` (0..=1, default 1) is the weight of the physics pose. Local rotations are blended with
  slerp and local positions with lerp, bone by bone, relative to the parent bone. Blend 0 writes
  exactly the animation pose.
- `strength` (0..=10) scales every motor in `powered` mode; each bone has its own `strength`
  multiplier. Zero strength releases the motors, so the skeleton collapses like `ragdoll`.
- Switching from `animated` to a dynamic mode builds the bodies on the current animation pose and
  gives them the animation velocity (`inherit_velocity`, default true): the linear and angular
  velocity of each bone is the animation pose change between the previous and the current frame
  divided by that frame's delta, so it does not depend on how many physics steps the frame ran
  (with a 60 Hz `fixed_timestep` and 144 Hz frames it is the same 1 m/s on every frame, within
  0.08). Switching between `ragdoll` and `powered` keeps the bodies.
- Get-up: setting `mode` to `animated` while the bodies are dynamic fades the physics pose out
  over `blend_out_seconds` (0 = immediate) with the simulation running, then makes the bodies
  kinematic. `ragdoll_blend_out(seconds)` starts the same fade from code and sets the component
  mode to `animated` when it ends. At the end of the fade the written pose is exactly the
  animation pose. Selecting a dynamic mode again cancels a fade.
- `pin_root` keeps bones without a parent bone kinematic on the animation (the rest is dynamic),
  which gives a powered character that stays in place. Without it the root body is free and a
  powered ragdoll falls over; there is no balance controller.

## Animation ownership

The animation service keeps writing the bone `Transform`s first each tick; the ragdoll feature
then reads them as the animation pose and writes the blended result after the physics step. The
blended pose is written on **every** frame, including frames that ran no physics step (with a
`fixed_timestep` below the frame time most frames), through the per-frame `after_frame` feature
hook; otherwise the animation pose would show through on those frames and the skeleton would
flicker between animation and physics. With `engine.PhysicsSettings.interpolation` set to
`interpolate` the written physics pose is blended between the previous and the latest step by
`step_status().alpha` (measured: within 1.2e-7 m of the lerp of the two step poses); otherwise it
is the latest step. `ragdoll_status` and `ragdoll_bone_state` always report the latest step. A
`Transform` that differs from what the ragdoll wrote last is treated as a new animation pose
(an `AnimationPlayer`, or a script writing the transform); an unchanged one keeps the previous
pose. So the rule in [Authored simulation](simulation.md#navigation-and-ownership) that an
animation target cannot also carry `engine.PhysicsBody*` or `NavigationAgent` still holds:
the ragdoll bodies are internal engine-physics bodies, not authored bodies, and `engine.Ragdoll`
is the one sanctioned way to share bones between animation and physics. A bone may belong to one
ragdoll and may not carry a physics body or navigation agent (rejected by scene validation).
Skinning evaluates after the physics step, so the skin follows the blended pose.

## Authored component

`engine.Ragdoll` goes on the skeleton root or any entity with `Transform`. All edits work through
MCP `component_set` / `scene_edit`. Bone references are descriptor `entity_references`
(`/bones/0/bone` .. `/bones/63/bone`), so prefab instantiation remaps them and scenes reject
unknown ids.

| Field | Meaning |
| --- | --- |
| `mode`, `blend`, `strength`, `blend_out_seconds` | See above. |
| `pin_root`, `self_collision`, `inherit_velocity` | Root pinning; whether the bones of this ragdoll collide with each other (default false: the bones share a contact exclusion group, so no pair of them touches and none reports contact events; joint neighbours never touch either way); velocity hand-over. |
| `density` (kg/m^3, default 1000), `friction`, `restitution`, `layer_name` | Bone collider material; `layer_name` references an `engine.PhysicsSettings` layer. The bones always keep the authored groups (the named layer, or the default membership 1 with an all-ones mask), so they collide with floors on named layers exactly as the layer matrix says. |
| `motor_stiffness`, `motor_damping` | Joint gains per kg m^2 of the bone subtree (defaults 1000 and 80 per second squared and second). |
| `settle_speed`, `settle_seconds` | Rest detection (below). |
| `bones` | Up to 64 bone definitions. |

A bone definition (`bones[i]`):

| Field | Meaning |
| --- | --- |
| `bone` | Authored id of the bone entity (needs `Transform`). Its position is the joint position. |
| `shape` | `capsule` (default), `box` or `ball`, in a frame whose Y axis runs along the bone. |
| `length`, `radius`, `half_extents` | Collider size in meters. Defaults: length = distance to the farthest child bone (a leaf continues the parent direction at half its length, at least 0.08); radius = 0.2 of the length within 0.02..=0.12. |
| `direction` | Bone-local direction override. |
| `mass` / `density` | Total kg (wins) or kg/m^3 (default: the ragdoll `density`). |
| `joint` | `spherical` (default), `hinge`, `generic`, `fixed` to the parent bone. A root bone has no joint. |
| `swing_limit`, `twist_limits` | Spherical: swing about the two axes perpendicular to the bone (radians, effective maximum 1.5) and twist about the bone. |
| `axis`, `hinge_limits` | Hinge axis (bone-local) and range. |
| `angular_limits` | Generic: ranges about the bone-local X, Y, Z axes (`[-pi, pi]` is free). |
| `strength` | Motor multiplier for this bone. |

Bones must form a tree through their entity parents: a bone's parent bone is its direct entity
parent (a non-bone entity between two bones is rejected; list it as a bone). Bones and all
entities above them need unit world scale. The rest pose used for joint frames and limits is the
pose the bones had when the ragdoll was created (the scene pose); build the ragdoll while the
skeleton is in its bind pose. Bodies count against the 3D `body_capacity` of the simulation.

`ragdoll_from_hierarchy(world, root)` builds a definition for every entity below `root` that has
a `Transform` (at most 64): auto capsules between joints, hinges (about an axis perpendicular to
the bone) for names containing knee, elbow, forearm, shin, calf and lower arm/leg, tighter cones
for spine, neck, head, hands and feet, and ball joints elsewhere.

## Events

Delivered to the entity carrying `engine.Ragdoll`:

- `physics.ragdoll.mode_changed`: `{"from":"ragdoll","to":"animated"}`; sent when the simulated
  mode changes (after a blend-out the change is reported when the fade ends).
- `physics.ragdoll.settled`: `{"mode":"powered","seconds":0.52}`; sent once when every dynamic
  bone has stayed below `settle_speed` m/s and `3 * settle_speed` rad/s for `settle_seconds`.
  A hit, impulse or mode change re-arms it.

## Native API

`engine_project::simulation::physics`:

| Function | Purpose |
| --- | --- |
| `add_ragdoll(&mut World, Entity, Ragdoll)` | Validate and attach the component. |
| `ragdoll_from_hierarchy(&World, root)` | Default definition from the bone hierarchy. |
| `ragdoll_set_mode` / `ragdoll_set_blend` / `ragdoll_set_strength` | Edit the live controls on the component. |
| `ragdoll_blend_out(&mut World, Entity, seconds)` | Get-up fade, then `animated`. |
| `ragdoll_bone_impulse(&World, ragdoll, bone, impulse, point)` | World-space impulse (N*s) at a point or the bone center; applied at the next sync, dynamic bones only. |
| `ragdoll_hit(.., HitReaction { strength_scale, recover_seconds })` | The same plus a temporary motor-strength drop that recovers linearly. |
| `ragdoll_status(&World, Entity)` | `RagdollStatus`: simulated mode, effective blend, fading, settled, mean pose error (rad), hit scale, per-bone `BoneReadout` (world position/rotation, velocities, dynamic flag, pose error). |
| `ragdoll_bone_state`, `ragdoll_joint_state` | One bone; the engine-physics `JointState` (relative position, XYZ Euler angles, loads) of the joint to its parent. |

Changes are applied at the next simulation sync (the next tick); read-outs reflect the last step.

## Settings changes, capacity and kinematic bones

- Changing `engine.PhysicsSettings` layers or materials rebuilds the bone bodies in place at the
  next sync with the new groups and materials, keeping each bone's pose and velocity.
- Kinematic bone bodies (every bone in `animated` mode, the pinned root otherwise) reach their
  animation pose in equal parts over the physics steps of a frame, not in the first step.
- All bones need room in the body table at once (`SimulationConfig::body_capacity`, shared with
  every other body and with debris). A ragdoll that does not fit is not built: `ragdoll_status`
  reports `active: false` with the reason "physics body capacity exceeded", the owner counts in
  `pressure_status().starved_bodies`, `physics.capacity.exceeded` is delivered once, and the
  build is retried at every sync.

## Measured behaviour

From `crates/engine-project/tests/physics_ragdoll.rs` (all pass) on a 11-bone humanoid:

- Build: body count equals bone count (+ floor); every joint anchor error below 1e-3 m;
  animated bodies sit on the bone joints within 1e-4 m.
- Limp ragdoll on a floor: falls and emits `settled` once; every bone stays above the floor;
  joint limits are soft constraints: exceeded by at most 0.44 rad during the landing impact and
  by 0 at rest (measured with `solver_iterations` 16).
- Powered with `pin_root` and a posed arm and thigh: mean pose error 0.020 rad at full strength,
  0.57 rad at zero strength.
- Blend 0.5: the written local rotation equals the slerp midpoint within 1e-4 (and position the
  lerp midpoint).
- Mode switch from moving animation: every bone inherits the 1 m/s animation velocity within
  0.05 (and none with `inherit_velocity` false).
- Blend-out: weight 0.5 at the middle of a 1 s fade; at the end every bone `Transform` equals the
  animation pose within 1e-5, the bodies are kinematic and one `mode_changed` is delivered.
- Hit: impulse gives the bone velocity; `hit_scale` drops to the reaction value and recovers to 1.
- With an `AnimationPlayer` on the same bones the powered ragdoll tracks the clip.
- Two runs with `fixed_timestep` are bitwise identical (transforms and body positions).
- 60 Hz steps at 144 Hz frames (`the_ragdoll_pose_is_rewritten_on_frames_without_a_physics_step`):
  64 of the 110 checked frames run no step; before the per-frame write the animated arm differed
  from the ragdoll pose by 2.4 rad on those frames, now by 2.4e-7 rad on every frame.
- A named-layer floor holds a default ragdoll (before: bones fell 43.6 m through it); no contact
  event is ever reported between two bones.

## Limitations

- Joint limits and motors use Rapier generic joints. A spherical joint is a twist limit plus two
  independent swing limits (an angle box, not a cone) because Rapier applies no motors to the
  coupled axes of a cone, and multi-axis Euler limits are soft: raise
  `engine.PhysicsSettings.solver_iterations` (16 recommended) for stiffer limits.
- Motor targets are XYZ Euler angles of the animation relative rotation; joints far beyond a
  1.5 rad swing are clamped to the limits.
- No balance or root-motion controller; a free powered ragdoll falls. No skinning-aware collider
  fitting beyond the capsule/box/ball per bone, no cloth on bones.
- Bones need unit scale and direct bone-to-bone parenting; at most 64 bones.
- The ragdoll feature reads animation as "any `Transform` it did not write"; a script that writes
  exactly the last ragdoll output is indistinguishable from no write.
