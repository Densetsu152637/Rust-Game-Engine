# Physics character controllers

Kinematic character controllers for 2D and 3D, in `engine-physics` (`World2D`/`World3D`,
see its [README](../crates/engine-physics/README.md)) and as the authored components
`engine.CharacterController2D` / `engine.CharacterController3D`. They are built on Rapier
0.36.1's `KinematicCharacterController` (shape-cast movement, sliding, slopes, ground
snapping and collision impulses). Units are meters, seconds, kilograms and radians. General
physics concepts are in [Physics core](physics.md).

## What is Rapier's and what is ours

| Feature | Source |
| --- | --- |
| Shape-cast movement, sliding along walls, skin `offset` | Rapier |
| Walk-up limit `max_slope_climb_angle`, slide threshold `min_slope_slide_angle` | Rapier |
| Snap to ground, grounded detection | Rapier |
| Pushing dynamic bodies (`solve_character_collision_impulses`) | Rapier |
| Gravity, jumps, vertical speed, landing impact speed | ours |
| Moving platforms (inherits the ground body's motion) | ours |
| Autostep over ledges | ours (own up/forward/down sweep) |
| Ground body and normal, wall contact, grounded/wall events | ours |

Autostep is our own because Rapier's `CharacterAutostep` did not step reliably in our
tests (a capsule resting at its skin gap never stepped over a 0.2 m ledge in 3D, cause not
isolated) and its 2D wall test uses a signed angle (`Vec2::angle_to`), so walls facing -X are
not walls. Rapier does not expose a maximum-iterations setting (it is fixed at 20), so there
is none here.

## Authored component

The entity needs a **kinematic** `engine.PhysicsBody2D/3D` whose collider is the character
shape: `box`, `ball`, `capsule`, `cylinder` (3D) or `convex_hull`. A Y-axis capsule is the
usual choice. The controller does not create its own collider. Validation (every prepare and
step) rejects a controller whose entity has no body of the same dimension, a non-kinematic
body, a trimesh/heightfield/compound shape, both a 2D and a 3D controller, or a
`NavigationAgent` on the same entity. Navigation writes `Transform` positions directly and
would fight the controller; feed its direction into `desired_velocity` from a script instead.

```json
{"engine.PhysicsBody3D":{"version":1,"data":{"motion":"kinematic","shape":"capsule","half_height":0.5,"radius":0.3}},
 "engine.CharacterController3D":{"version":1,"data":{
   "desired_velocity":[2,0,0],"max_slope_climb_angle":0.7854,"autostep":{"max_height":0.3,"min_width":0.1},
   "push_mass":60,"jump_speed":5}}}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `up` | +Y | World up (normalized). |
| `offset` | 0.01 | Skin gap in meters, `0.0001..=1`. |
| `slide` | true | Slide along obstacles instead of stopping. |
| `max_slope_climb_angle` | pi/4 | Steepest walkable slope (`0..=pi/2`). |
| `min_slope_slide_angle` | pi/4 | A grounded character slides down slopes steeper than this. |
| `autostep` | absent | Object enables ledge stepping: `max_height`, `min_width`, `include_dynamic_bodies`. |
| `snap_to_ground` | 0.2 | Snap distance in meters, `0` disables. |
| `push_mass` | 0 | Mass (kg) used when pushing dynamic bodies; `0` disables pushing. |
| `gravity_scale` | 1 | Multiplier on world gravity along `up`; `0` flies. |
| `desired_velocity` | 0 | World-space velocity (m/s) applied every physics step. 2D uses 2 components. |
| `jump_speed` / `jump` | 5 / 0 | Increment `jump` to jump at `jump_speed` while grounded. |
| `landing_min_speed` | 1 | Smallest touchdown speed that emits `physics.character.landed`. |

All fields are editable through MCP `component_set`. The controller applies `desired_velocity`
in every physics step, so with `fixed_timestep` movement is bitwise reproducible and
independent of how frames are split. The simulated position is written back to `Transform`
after each frame's steps; fixed-step interpolation does not smooth characters.

Pushing: Rapier transfers the character's velocity into dynamic bodies it presses against,
scaled by `m_body * m / (m_body + m)` with `m = push_mass`, so a heavier character pushes more.

### Slopes, sliding and steps

A grounded character on a walkable slope (below `min_slope_slide_angle`) stands still: it is
held by snapping and no gravity push is applied. On a steeper slope gravity applies and it
slides. A slope steeper than `max_slope_climb_angle` cannot be walked up. Autostep triggers
when a grounded character is blocked by a wall: it casts up by `max_height`, forward by
`min_width` (must be free) and down (must land on walkable ground within the raised height),
then raises the character by the step height. `min_width` should be at least the character
radius for a round shape to settle on the step.

## Events

Delivered to the character entity (bind them with `EventBindings`):

| Event | Payload |
| --- | --- |
| `physics.character.grounded` | `dimension`, `impact_speed` (m/s downward at touchdown) |
| `physics.character.landed` | same; only when `impact_speed >= landing_min_speed` |
| `physics.character.left_ground` | `dimension` |
| `physics.character.wall` | `dimension`, `phase` (`start`/`stop`), `normal` (facing the character) |

Wall contact is held for 50 ms after the last hit so a character pressed into a wall does not
flicker. A character is grounded when it touches walkable ground and is not moving upward.

## Native API

`engine_project::simulation::physics`:

- `set_character_velocity(world, entity, Vec3)` writes `desired_velocity`;
- `character_jump(world, entity)` increments `jump`; `character_impulse(world, entity, speed)`
  sets the vertical speed (m/s along `up`) on the next step even in the air;
- `character_state(world, entity) -> CharacterReadout` (`grounded`, `on_slope`, `sliding`,
  `ground_normal`, `ground_entity`, `wall_contact`, `wall_normal`, `translation`, `velocity`,
  `ground_velocity`, `vertical_speed`).

`engine-physics`: `insert_character(id, CharacterDesc)`, `remove_character`,
`set_character_desc`, `set_character_velocity`, `move_character(id, translation, dt)` (a
velocity of `translation / dt`), `character_jump`, `character_state`,
`drain_character_events` (bounded by `MAX_CHARACTER_EVENTS`; overflow is counted by
`dropped_character_events`), `is_character`, `character_ids`. `CharacterDesc::validate`
rejects non-finite or out-of-range settings.

## Moving platforms

The ground found after each move is remembered as a body and a body-local point. On the next
step the character receives that point's displacement: exactly `next_position - position`
for kinematic platforms (the displacement of the ground point, so rotation is included; only
translation is covered by tests) and
`velocity * dt` for dynamic ones. The character's own movement is computed relative to the
platform, so `state.velocity` is the world velocity and `ground_velocity` the inherited part.
The character's yaw does not follow a rotating platform. Platform motion is not checked
against obstacles, so a platform can carry a character into a wall.

## Limits and notes

- Scene queries use the broad phase built by the previous `step`: colliders inserted since
  then are invisible until the next step, and a kinematic collider's cull box is one step old.
- Rapier adds a kinematic parent's velocity itself while resolving a hit, so a character
  pressed into a moving kinematic platform by a wall may receive that motion twice.
- In 2D Rapier's slope handling has no horizontal tangent and can drop all horizontal
  movement when float noise tilts a floor contact normal; the controller avoids the trigger
  by not pushing into walkable ground (see the notes in `features/character.rs`).
- Debug view: a contributor named `character` adds an anchor per character plus a green
  ground-normal line (grounded) and a red wall-normal line (wall contact).
