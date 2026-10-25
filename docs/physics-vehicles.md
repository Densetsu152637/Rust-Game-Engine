# Raycast vehicles

3D raycast vehicles in `engine-physics` (`World3D`, see its
[README](../crates/engine-physics/README.md)) and as the authored component
`engine.Vehicle3D`. A vehicle is a **dynamic chassis body** plus 2 to 16 wheels. There are no
wheel bodies or colliders: each wheel probes the world with a ray (or a sphere cast) and a
spring, damper, slip-based tyre and drivetrain model turn the result into forces on the
chassis. The suspension casts use Rapier 0.36.1's broad phase; the tyre and drivetrain models
are ours. Units are meters, seconds, kilograms, newtons and radians. General physics concepts
are in [Physics core](physics.md).

There is no 2D vehicle: a top-down or side-view variant was not built (deferred, see the end).

## Conventions

* Everything in a description is in the **chassis frame**. `VehicleDesc::forward` (default
  +Z) is the driving direction; "up" is the opposite of the mean suspension direction
  (default -Y).
* A wheel rolls forward for positive spin when `axle x up == forward`. With forward +Z and up
  +Y the axle is +X (the vehicle's left). Validation rejects a wheel whose `axle x up` is not
  within 60 degrees of `forward`.
* Steering `+1` turns **right** (toward `forward x up`); the steer angle rotates the wheel
  heading about up by `-angle`.
* Speed is the chassis velocity along `forward`; negative while reversing.
* Scene queries use the broad phase built by the previous `step`, so colliders inserted since
  then are invisible until the next step (step the world once after building a scene). Sphere
  casts lose millimetres to centimetres of accuracy on colliders tens of kilometres wide; use
  rays (default) on huge ground boxes.

## Authored component

`engine.Vehicle3D` needs a **dynamic** `engine.PhysicsBody3D` on the same entity (the chassis;
its collider, density and therefore mass and inertia are the vehicle's). Validation (every
prepare and step) rejects a non-dynamic body, a `CharacterController3D` on the same entity,
unknown wheel entities and any invalid wheel, tyre, engine, gearbox or anti-roll setting.
The defaults describe a 1500 kg car with a 2.6 m wheelbase and 1.6 m track (front steering,
rear drive); give the chassis box half extents `[0.9, 0.3, 2.0]` and density `347` for it.

```json
{"engine.PhysicsBody3D":{"version":1,"data":{"half_extents":[0.9,0.3,2.0],"material":{"density":347}}},
 "engine.Vehicle3D":{"version":1,"data":{
   "throttle":1.0,
   "differential":{"kind":"limited_slip","preload":50,"viscous":10,"max_torque":400},
   "input":{"throttle":"gas","brake":"brake","steer_left":"left","steer_right":"right"}}}}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `wheels` | 4 wheels | 2 to 16 [wheel](#wheels) objects. |
| `tyre` | see below | [Tyre model](#tyre-model) settings shared by all wheels. |
| `engine` | 900-6500 rpm | `torque_curve` `[rpm, N m]` samples (ascending, 2..=64), `idle_rpm`, `max_rpm`, `engine_brake` (N m at `max_rpm`). |
| `gearbox` | automatic, 5 gears | `mode` `automatic`/`manual`, `forward` ratios (1..=12), `reverse`, `final_drive`, `efficiency`, `up_shift_rpm`, `down_shift_rpm`, `shift_time`. |
| `differential` | open | `kind` `open`/`locked`/`limited_slip` plus `preload`, `viscous`, `max_torque` (limited slip). |
| `steering` | 0.55 rad | `max_angle`, `reduction_speed`, `min_factor`, `rate`. |
| `anti_roll` | none | Up to 8 bars `{wheel_a, wheel_b, stiffness}`. |
| `suspension_cast` | `ray` | `ray` or `sphere`. |
| `forward` | +Z | Driving direction in the chassis frame. |
| `drag` / `downforce` | 0.4 / 0 | N per (m/s)^2: `0.5 rho Cd A`; downforce acts along chassis up. |
| `rolling_resistance` | 0.012 | Force of `coefficient * load` against each wheel's motion. |
| `spawn_rolling` | false | Start wheels spinning at the chassis speed (a vehicle spawned with a velocity). |
| `input` | see [controls](#controls-and-input-mapping) | Gameplay action names. |
| `throttle`, `brake`, `handbrake`, `steer` | 0 | Direct controls: pedals `0..1`, steer `-1..1`. |
| `shift_up`, `shift_down` | 0 | Increment to request one gear up/down. |

Every field is editable with `component_set` (MCP) or a scene edit; changes take effect at the
next physics sync and rebuild the model in place (wheel speeds and gear are kept while the
wheel count is unchanged). `entity_references` cover `/wheels/<i>/entity`, so prefab
instantiation remaps wheel visual ids.

### Wheels

| Field | Default | Meaning |
| --- | --- | --- |
| `mount` | 0 | Top of the suspension (chassis frame). |
| `suspension_dir` | -Y | Direction the wheel extends toward the ground. |
| `axle` | +X | Spin axis (orthogonalized against up). |
| `radius` | 0.34 | Wheel radius (`0.02..=2`). |
| `rest_length` | 0.3 | Unloaded spring length; ray length is `rest_length + radius`. |
| `max_travel` | 0.2 | Compression travel; shortest length is `rest_length - max_travel`. |
| `stiffness` | 35000 | Spring rate N/m. |
| `damping_compression`, `damping_relaxation` | 3000, 4000 | Damper N s/m while compressing / extending. |
| `max_force` | 60000 | Largest suspension force (N). |
| `inertia` | 1.5 | Wheel plus tyre inertia about the axle (kg m^2). |
| `steer`, `drive`, `brake`, `handbrake` | flags | Which controls act on the wheel (`brake` defaults true). |
| `brake_torque`, `handbrake_torque` | 2500, 3500 | Torque at full pedal (N m). |
| `entity` | none | Entity id that receives the [wheel pose](#wheel-visuals). |

## Suspension

For each wheel the chassis frame mount point is cast along the suspension direction. The
wheel centre sits `L = hit_distance - radius` from the mount (a sphere cast uses the sweep
distance directly), clamped to `[rest_length - max_travel, rest_length]`, and the spring is
compressed by `c = rest_length - L` (unclamped, so a bottomed-out wheel keeps pushing harder).

```
F = clamp( k c + c_d * v_c  + anti_roll , 0, max_force )
v_c = (v_mount - v_ground) . dir        (closing speed along the suspension)
c_d = damping_compression while v_c > 0, damping_relaxation otherwise
```

The damper term is limited to what cancels the closing speed within one step
(`|v_c| / (dt * invM_eff * 4)`), which keeps stiff dampers stable at low frame rates. The force
acts on the chassis at the wheel centre along the suspension axis. `F` is also the tyre normal
load. Anti-roll bars add `stiffness * (c_a - c_b)` to wheel a and subtract it from wheel b
while both touch the ground. If the ground under a wheel is a **dynamic** application body the
equal and opposite impulse is applied to it at the contact point.

The rays exclude the chassis body and sensors and use the chassis collider's collision groups.
The chassis receives the forces as Rapier user forces, which are reset and rewritten every
step (a vehicle owns the user force and torque of its chassis; `apply_impulse` still works).
Vehicle chassis bodies never sleep.

## Tyre model

`TyreDesc::forces(load, surface_scale, forward, side, surface)` is a pure function:

* Patch speeds `forward`/`side` are the ground-relative contact velocity along the wheel
  heading and axle; `surface = omega r` is the tread speed.
* Slip ratio `kappa = (surface - forward) / max(|forward|, low_speed)`; slip angle
  `alpha = atan2(side, max(|forward|, low_speed))`. `low_speed` (default 1 m/s) regularizes
  both near standstill.
* Pacejka magic formula per axis, `y(x) = sin(C atan(B x - E (B x - atan(B x))))`, with
  `(B, C, E)` = longitudinal `(10, 1.35, 0)` and lateral `(8, 1.3, -0.5)` by default. `y` peaks
  at about 1; `PacejkaAxis::peak_slip()` returns where.
* Peak force `D_x = mu_eff * load`, `D_y = lateral_scale * D_x`, with load sensitivity
  `mu_eff = friction * clamp(1 - s (load / nominal_load - 1), 0.25, 2) * surface_scale`
  (`s = load_sensitivity`) and the surface scale `collider friction /
  reference_surface_friction` of the ground (the default `0.5/0.5` makes an engine default
  material grip 1x; ice at 0.1 grips 0.2x).
* `F_x0 = D_x y_x(kappa)`, `F_y0 = -D_y y_y(alpha)`.
* **Friction ellipse**: with `u = (F_x0/D_x)^2 + (F_y0/D_y)^2`, both forces are scaled by
  `1/sqrt(u)` when `u > 1`, so `(F_x/D_x)^2 + (F_y/D_y)^2 <= 1`. Slip in one direction costs grip
  in the other.
* Stability clamps: each component is limited to the force that would cancel the slip within
  the step, using the chassis effective inverse mass at the contact (shared over the wheels in
  contact) and, longitudinally, the wheel inertia. This keeps explicit integration stable at
  low speed without a separate stick model.

Rolling resistance `crr * load * tanh(v / 0.05)` is added along the heading. There is no
static friction: a vehicle on a slope with the brakes off rolls (hold the handbrake).

## Drivetrain

* **Engine**: `torque = curve(rpm) * pedal * limiter(rpm)`; the limiter fades to zero over the
  last 3% below `max_rpm`. Closed-throttle engine braking applies `engine_brake * rpm/max_rpm *
  (1 - pedal)` once the wheels drive the engine above idle. The engine is slaved to the wheels
  (`rpm = ratio * driven wheel speed`, clamped to idle..max*1.02): there is no clutch slip or
  engine inertia, so launches use the idle-rpm torque. In neutral, or while shifting, the
  engine free-revs toward `idle + 0.6 pedal (max - idle)`.
* **Gearbox**: gears `-1` reverse, `0` neutral, `1..=N`. `overall ratio = gear * final_drive`
  (negative in reverse); wheel torque `= crank torque * ratio * efficiency`. Automatic mode
  selects gear 1 when the throttle is pressed in neutral/reverse, reverse when the brake is held
  below 0.5 m/s with no throttle (in reverse the brake pedal accelerates and the throttle
  brakes), upshifts at `up_shift_rpm` and downshifts at `down_shift_rpm` (at least 0.5 s apart).
  A shift between forward gears cuts torque for `shift_time`. Manual mode only changes gear on
  `shift_up`, `shift_down` or `set_gear`. Gear changes emit events.
* **Differential** over all `drive` wheels: `open` gives each the same torque (a wheel on ice
  spins until the rev limiter, the other keeps only what the ice can carry);
  `locked` gives them one speed (they share the summed inertia, torque follows the grip);
  `limited_slip` is an open differential plus a clutch pack that moves
  `min(preload + viscous |omega - mean|, max_torque)` from the faster to the slower wheel.
* **Brakes**: torque `pedal * brake_torque` (+ `handbrake * handbrake_torque`) opposes the
  wheel and never reverses it (a wheel stops at zero speed). Locked wheels slide on the tail
  of the tyre curve.
* Aerodynamic drag `-drag |v| v` and downforce `downforce |v|^2` along chassis up act at the
  centre of mass.

## Steering

The target angle is `input * max_angle * f`, `f = 1 - (1 - min_factor) min(|speed| /
reduction_speed, 1)` (`reduction_speed = 0` disables the reduction), and the actual angle moves
toward it at `rate` rad/s. Only `steer` wheels turn; all of them take the same angle (no
Ackermann geometry).

## Controls and input mapping

Direct controls (`throttle`, `brake`, `handbrake` `0..1`, `steer` `-1..1`) are plain component
fields for native scripts, behaviours and MCP. The mapped gameplay actions come from the
`ActionFrame` resource (the same frame scripts read; it is empty while UI capture or control
locks are active). `input` holds the action names (empty = unbound):
`throttle`, `brake`, `handbrake`, `steer_left`, `steer_right`, `shift_up`, `shift_down` (defaults are the same
words). A pedal or steering action contributes its analog `values` entry when it has one and
`1` while merely held; action values are `0..1`, so a stick axis is bound to a positive and a
negative action. Effective pedals are `max(direct, mapped)`; effective steer is
`clamp(direct + right - left)`. Shift actions act on the press.

## Events

All are delivered to the chassis entity (bind `EventBindings` handlers):

| Event | Payload |
| --- | --- |
| `physics.vehicles.gear_changed` | `from`, `to`, `gear` |
| `physics.vehicles.wheel_contact` | `wheel` index, `phase` `start`/`stop`, `ground` (authored id or null) |
| `physics.vehicles.airborne` | none; no wheel touched the ground for 0.1 s |
| `physics.vehicles.landed` | `impact_speed` (m/s) |

At most 65536 events are buffered between drains; extra events are counted in
`dropped_vehicle_events`.

## Wheel visuals

`wheels[i].entity` names an entity (usually a child of the chassis) that receives the wheel
pose after each step: the chassis world pose composed with the wheel's simulated
chassis-frame position (mount plus suspension travel) and rotation (steer then spin about the
axle). A wheel mesh whose rolling axis is the chassis X axis can be used directly. The pose is
computed from the chassis `Transform`, so it also holds with interpolated stepping. The same
data is readable without visuals through `vehicle_wheel_pose` / `WheelReadout`.

The wheel entities are rewritten on **every** frame (the per-frame `after_frame` hook), from the
chassis `Transform` as it is at the end of the frame. With `fixed_timestep` below the frame time
and `interpolation` on, the chassis `Transform` is interpolated and moves on frames that ran no
physics step; a wheel visual that is not a child of the chassis used to lag by up to one step
there. Measured at 144 Hz frames, 60 Hz steps and 5.8 m/s: 87 of 150 frames ran no step; the
visuals were up to 0.0778 m behind the chassis before and are within 5.4e-7 m now. A chassis
that fractures (`Fractured`) stops being a vehicle and its wheel visuals stop being written.

## Native API

engine-physics (`World3D`): `insert_vehicle`, `remove_vehicle`, `is_vehicle`, `vehicle_desc`,
`set_vehicle_desc`, `set_vehicle_controls`, `vehicle_controls`, `vehicle_shift_up/down`,
`set_vehicle_gear`, `vehicle_roll_start`, `vehicle_state`, `vehicle_wheel_state(s)`,
`vehicle_ids`, `drain_vehicle_events`, `dropped_vehicle_events`. `TyreDesc`, `EngineDesc`,
`GearboxDesc`, `DifferentialDesc` and `split_torque` expose the pure models.

engine-project (`simulation::physics`): `set_vehicle_controls(world, entity, throttle, brake,
steer, handbrake)`, `vehicle_shift_up/down`, `vehicle_set_gear`, `vehicle_state` (a
`VehicleReadout` with speed, lateral speed, rpm, engine torque, gear, shifting, steer angle,
contact count, airborne and per-wheel `WheelReadout`), `vehicle_wheels` and
`vehicle_wheel_pose` (world position and rotation of a wheel by index).
`WheelReadout` holds contact state, suspension length/velocity/force, slip ratio and angle,
tyre forces, spin and steer, contact point/normal, ground entity and surface friction, and the
wheel pose in world and chassis space.

The debug view (`World::add_debug_contributor`, registered on the first `insert_vehicle`)
draws each suspension ray (green in contact, yellow in air), the contact normal (grey), the
suspension force (blue) and the tyre force (red) at the contact points.

## Determinism

Vehicles iterate in id order and use only f32 arithmetic with no randomness, so fixed-step runs
are bitwise identical (two runs and frame splits are compared in `tests/physics_vehicles.rs`).
The `deterministic` crate feature applies as in [Physics queries](physics-queries.md).

## Measured behaviour

From `crates/engine-physics/tests/vehicles.rs` (1500 kg car, 60 Hz, dev profile):

| Check | Result |
| --- | --- |
| Rest compression `m g / (k n)` | 0.105 m predicted; rest height within 5 mm and drift under 1e-2 m over 5 s (ray and sphere casts) |
| Full throttle | monotonic speed; upshifts 1-2 at 5785 rpm and 2-3 at 5796 rpm (configured 5800); top speed 46.6 m/s vs 47.8 m/s analytic (drag/gearing balance), -2.50% (test bound 5%) |
| Braking from 19.8 m/s | 21.3 m of 18.2 m from `v^2 / (2 mu g)` (locked wheels slide on the curve tail, +17.25%); the test accepts 0.97..1.2 times the estimate (it can never beat the friction limit) |
| Steering 0.2 rad | turn radius 12.97 m vs 12.87 m from `sqrt((L / tan(delta))^2 + (L/2)^2)`, +0.60% (test bound 2%) |
| Split mu, 4 s first gear | open: wheel on ice 120.6 rad/s vs 10.5 rad/s on grip, 7.7 m; locked: 109.5 rad/s on both, 25.7 m |
| Drop from 3 m | no NaN, peak suspension force 24.2 kN under the 60 kN limit |

## Limits and deferred work

* 2D vehicles (top-down or side view) are not implemented; the model is 3D only.
* No Ackermann steering, clutch or engine inertia, static (stick) friction, tyre temperature
  or wear, per-axle differentials, or suspension geometry beyond a straight axis.
* Sphere casts are costlier than rays (not benchmarked) and inaccurate on tens-of-kilometre colliders.
* The ground under a wheel is read from the previous step's broad phase; a collider moved or
  inserted this tick is seen one step later.
