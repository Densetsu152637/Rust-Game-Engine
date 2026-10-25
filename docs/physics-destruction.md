# Physics destruction

Bodies can shatter into debris. Fracturing is computed ahead of time or on demand by the
`engine-destruction` crate (Voronoi fracture of a closed mesh), triggered by contact load,
and the pieces become ordinary dynamic convex-hull bodies that inherit the body's motion.

- `crates/engine-destruction` (glam and std only): the geometry. See its
  [README](../crates/engine-destruction/README.md).
- `engine-physics`: `World::contact_impulse(id)`, `World::is_sleeping(id)` and
  `World::sleep(id)` (read-back of the solver's contact impulses and forced sleep).
- `crates/engine-project/src/simulation/physics/destruction.rs`: the authored
  `engine.Destructible3D` component, the fracture and debris runtime, events and the native
  API.

Units are meters, seconds and kilograms.

## Fracturing a mesh

`engine_destruction::fracture(&mesh, &pattern, &limits)` takes a closed triangle mesh (outward
counter-clockwise winding, every edge shared by exactly two triangles) and returns
`Fracture { fragments, seeds, source_volume, convex_source, parts }`.

1. **Seeds.** `Pattern::Uniform { count, seed }` places `count` points uniformly inside the
   solid; `Pattern::Impact { count, seed, point, radius }` puts 75% of them within `radius` of
   `point` (distance uniform in `[0, radius]` along a uniform direction, so density rises
   steeply toward the point) and the rest uniformly. A splitmix64 generator makes seeds, and
   therefore the whole output, a pure function of the inputs. Seeds closer than `1e-5` of the
   mesh diagonal to each other are rejected.
2. **Voronoi cells.** The cell of seed *i* is the mesh clipped by the bisector half-space
   `|p - s_i| <= |p - s_j|` for every other seed *j*, nearest first; clipping stops once the
   next bisector is farther than the cell's reach.
3. **Convex solids** are clipped as planar polygon faces: a polygon is cut with
   Sutherland-Hodgman, new vertices appear only where a plane crosses an edge (computed from a
   canonical endpoint order so neighbouring faces agree bit for bit), and each cut is closed
   with one cap polygon built from the on-plane points. No triangle growth, no cracks.
4. **Concave solids** are first split into convex parts along the plane of the most reflex face
   (triangle clipping with an ear-clipped cap, holes bridged), recursively, at most 32 splits;
   a mesh that needs more is refused with `Error::NonRobust` instead of approximated. The
   Voronoi cell of each seed is then cut against each part. A cell that spans two parts
   becomes two fragments, and the split faces are flagged as fracture surface. Fragments are
   therefore always convex, volumes are exact, and their hulls need no approximation.
5. **Output per fragment**: `hull_points` (collider), `mesh` (welded positions, triangles and a
   per-triangle `interior` flag that is true on every cut face, for a rock-versus-surface
   material), exact `volume`, `centroid`, `inertia` (unit density about the centroid) and
   `hull_volume`.

Limits (`Limits::default()`): 256 fragments, 65 536 vertices, 20 000 input triangles. Over a
limit, an open mesh or a bad pattern return `Error::LimitExceeded`, `InvalidMesh` or
`InvalidPattern`; nothing is silently truncated.

The 2D variant `fracture_polygon(points, &Pattern2, &limits)` fractures a convex polygon
(either winding) into convex `Fragment2` polygons with `fracture_edge` flags, area, centroid
and polar inertia. There is no authored `Destructible2D` component yet (see Deferred).

## Measured behaviour (`cargo test -p engine-destruction`)

| Check | Result |
| --- | --- |
| Unit cube, 2/8/24/64 seeds | N fragments each; total volume within 1e-4 of 1; every fragment watertight with Euler characteristic 2 and `hull_volume == volume`; volume-weighted centroid at the origin within 1e-4 |
| Single seed | whole cube; inertia 1/6 on the diagonal, 0 off it (analytic, within 1e-5) |
| Determinism | same mesh, pattern and seed give an identical `Fracture`; another seed gives other seeds |
| Impact pattern, 48 seeds, radius 0.35 | mean fragment volume 0.0037 within 0.4 m of the hit, 0.049 farther away |
| L-shaped prism (volume 3), 6/16/40/96 seeds | split into 2 convex parts (volumes 1 and 2); volume error <= 1e-4; every fragment convex, watertight, Euler 2 |
| 2D square, 2/9/40 seeds | areas sum to 4 within 1e-4; centroid at the origin; polar inertia of one piece 2.667 (analytic) |

## `engine.Destructible3D`

Requires `engine.PhysicsBody3D` on the same entity. The fractured shape is the body's `box` or
`convex_hull` (scaled by the entity's world scale), or the authored closed `mesh` for any
body shape. MCP editable through `component_set` like every catalog component.

```json
{"engine.Destructible3D":{"version":1,"data":{
  "pattern":"impact","fragment_count":24,"seed":7,"impact_radius":0.4,
  "threshold":50.0,"threshold_mode":"impulse","health":200.0,"damage_floor":2.0,
  "prefracture":false,
  "debris":{"lifetime":10.0,"sleep_velocity":0.1,"max_active":128,"recycle":"oldest",
            "pool":"default","inherit_velocity":1.0,"scatter_speed":1.0}
}}}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `pattern` | `uniform` | `uniform` or `impact` seed distribution. |
| `fragment_count` | 12 | Voronoi seeds, 2..=256 (concave meshes can yield more fragments than seeds, still at most 256). |
| `seed` | 1 | Seed generator seed. |
| `impact_point`, `impact_radius` | origin, 0.5 | Body-local impact point (used when no contact point is known) and cluster radius. |
| `mesh` | none | `{vertices, indices}` closed mesh in body-local unscaled coordinates, fractured instead of the body shape. At most 4096 vertices and 8192 triangles. |
| `threshold`, `threshold_mode` | 50, `impulse` | Fracture when the body's largest contact impulse in one physics substep reaches `threshold` N*s (`impulse`), or its largest contact force `impulse / substep` reaches it in N (`force`). |
| `health`, `damage_floor` | none, 1 | Optional damage accumulation: every substep's contact impulse above `damage_floor` adds the excess to the body's damage; reaching `health` fractures it. Resting contact (about `m g dt` per substep) must stay below the floor. |
| `prefracture` | false | Compute and cache the fragments at scene start (and whenever the component, body or scale changes) instead of at fracture time. |
| `debris.*` | see below | Fragment behaviour. |

`debris`:

| Field | Default | Meaning |
| --- | --- | --- |
| `lifetime` | 10 | Seconds before a fragment expires; 0 keeps it until recycled. |
| `sleep_velocity` | 0.1 | A fragment whose linear (m/s) and angular (rad/s) speeds stay below this for 0.25 s is forced to sleep; 0 leaves sleep to Rapier. Contact or impulses wake it. |
| `max_active`, `recycle`, `pool` | 128, `oldest`, `default` | Pool cap. All debris with the same `pool` name shares one cap (taken from the most recent fracture); the excess is recycled `oldest` first or `smallest` (volume) first. Must be at least `fragment_count`. |
| `inherit_velocity` | 1 | Fraction of the body's linear and angular velocity the fragments inherit (`v_i = v + w x r_i`). |
| `scatter_speed` | 1 | Outward speed (m/s) added along the direction from the body's centre of mass to each fragment's centroid. The mass-weighted mean is removed, so scatter adds no net linear momentum. |
| `density` | none | Fragment density in kg/m^3; omitted conserves the body's total mass (a fixed body uses its material density). |

### What happens on a fracture

1. After each physics substep the feature reads `World::contact_impulse` for every
   destructible, tracks the substep peak (impulse, force and contact point) and damage.
2. At the end of the tick, a body whose peak crossed `threshold` (cause `threshold`) or whose
   damage reached `health` (cause `damage`) fractures. `fracture_now` does the same on demand
   (cause `api`). With the `impact` pattern the seeds cluster at the contact point converted to
   body-local space; `prefracture` uses the authored `impact_point` instead.
3. The intact body's `PhysicsBody3D` is removed (the simulation drops the body at the next
   sync) and a runtime `Fractured { fragments, impulse, point }` marker is added to the entity.
   Renderers should stop drawing the intact mesh for entities with `Fractured`. Everything that
   needed the body treats a `Fractured` entity as inactive, not as an error: a joint with the
   body as an endpoint is removed (`joint_status` reads `Inactive`), a `Vehicle3D` chassis stops
   being a vehicle (`vehicle_state` returns an error until the component is removed) and a
   `CharacterController3D` body stops being a character. Scene validation stays valid and no
   event is emitted per tick.
4. One dynamic `convex_hull` body is spawned per fragment as an owned entity (`OwnedBy`
   the destructible, so it is despawned with it), registered in the entity lookup with the
   stable derived id `<destructible id>#f<fragment index>` (`...#f<index>.<n>` if that id is
   taken). Its `Transform` is the body's rotation at the fragment centroid, scale 1; its
   `DebrisFragment { owner, index }` component identifies it. The fragment bodies are
   created by the next simulation sync, so they exist from the tick after the fracture.
5. Mass: each fragment's density is chosen so its mass is the body's mass times its share of
   the total fragment volume (so mass is conserved even when a fragment's hull is larger than
   the piece). Fragments below 1e-9 m^3 cannot be physics bodies and fail the fracture with an
   explicit error, as does a fragment density outside 0.000001..=10000 kg/m^3. Every fragment is
   validated **before** the first one spawns, so a failed fracture leaves the intact body and no
   partial debris (with a 1e4 kg/m^3 body, 12 of 24 seeds are refused; before the fix seed 4
   left 12 of its 16 fragments next to the intact body). Material (friction, restitution, layers, combine rules) is copied from the
   body's resolved material.
6. The pool cap is enforced immediately; a pool smaller than a fracture recycles pieces of
   that fracture too.

### Contacts between fragments

Fragments of one fracture touch from the moment they spawn. Contacts between two fragments with
the same owner are **not** reported as `physics.contact.start`/`stop` (they still collide
physically). A 256-fragment fracture used to queue thousands of such events in its first step and
abort the tick with "Event queue capacity exceeded"; now none are queued
(`a_large_fracture_neither_aborts_the_step_nor_floods_sibling_contact_events`). Contacts with
anything else, including fragments of another fracture, are reported as usual.

Fragments are bodies like any other: when the body table is full (`SimulationConfig::body_capacity`)
the fragments that do not fit stay entities without a body (the pool and lifetime rules still
apply) and are retried every sync; see [Physics core](physics.md#pressure-and-capacity).

### Fragment lifetime

A fragment lives until its `lifetime` expires, the pool recycles it, its owner is despawned
(scene change or script teardown), or the host despawns it. Expiry and recycling despawn the
entity, remove its lookup id and render mesh, and emit `debris_expired`. The `Fractured` owner
stays (with its event bindings) after its fragments are gone.

### Events

Delivered to the destructible entity's `EventBindings`:

- `physics.destruction.fractured`: once per fracture. Payload `{dimension:3, cause:
  "threshold"|"damage"|"api", impulse, force, fragments, point, mass, linear_velocity,
  angular_velocity}` (mass and velocities are those of the intact body at the fracture).
- `physics.destruction.debris_expired`: once per fragment. Payload `{fragment: id, index,
  reason: "lifetime"|"recycled", age, volume, pool}`.

### Native API

`engine_project::simulation::physics`:

| Function | Purpose |
| --- | --- |
| `fracture_now(&mut World, Entity, impact_point: Vec3, impulse: f32) -> FractureReport` | Fracture immediately; `impact_point` is world space; `impulse` is reported in the event. The entity needs a synced body (after its first tick). |
| `fragments(&World, Entity) -> Vec<FragmentInfo>` | Live fragments (entity, index, volume, age) ordered by index. |
| `fragment_mesh(&World, fragment: Entity) -> Option<Arc<FragmentRender>>` | Render mesh relative to the fragment origin: positions, triangles, per-triangle `interior`. |
| `destructible_status(&World, Entity) -> DestructibleStatus` | Fractured, accumulated damage, health, threshold, prefractured, live fragments. |
| `set_fracture_threshold(&mut World, Entity, f32)` | Edit `threshold` (validated). |
| `debris_count(&World, Option<&str>)` | Live debris in a pool, or all. |
| `preview_fracture(&Destructible3D, &PhysicsBody3D, scale) -> Vec<f32>` | Fragment volumes without touching the world. |

Rendering: the fragment meshes live in the runtime state and are exposed through
`fragment_mesh`; the renderer hookup through `PipelineFeature` comes in a later change, and
until then the physics debug view draws the fragment hulls.

## Validation

`Destructible3D::check` bounds every value (fragment count 2..=256, finite positive
threshold, `max_active` 1..=4096 and at least `fragment_count`, debris ranges, mesh sizes).
Scene validation additionally requires the body and a box or convex-hull shape unless a `mesh`
is authored. Failures are `simulation.invalid` errors naming the component.

## Measured behaviour (`cargo test -p engine-project --test physics_destruction`)

| Check | Result |
| --- | --- |
| 1 kg box (density 1), 1.3 kg ball at 40 m/s, `threshold` 3 N*s | fractures once: event cause `threshold`, peak substep impulse 22.7 N*s, 12 fragments, total fragment mass 1.00 kg (within 1%) |
| Same scene, ball at 2 m/s | no fracture, no event, no debris |
| Fixed destructible wall, same ball | fractures once; fragment mass equals volume x material density within 1% |
| `health` 0.6, `threshold` 1e6, ball at 4 m/s | fractures once with cause `damage` |
| `fracture_now` on a box with v = (2, 1, -1), w = 3 rad/s, scatter 3 m/s | 10 fragments; total mass equals the body's 3 kg within 1%; total linear momentum 5.9999995, 2.9999995, -2.9999998 vs 6, 3, -3 (relative error 1e-7), so scatter adds no net momentum; fastest fragment moves > 1.5 m/s relative to the body |
| Impact pattern, 40 fragments, hit on a face | 24 fragments within 0.4 m of the hit, mean volume 0.0032 m^3; 16 farther, mean 0.058 m^3 |
| `prefracture` true and false, same seed | identical fragment volumes; `destructible_status.prefractured` true only when cached |
| Pool `shared`, cap 10, two boxes of 8 fragments | pool holds 10 after the second fracture and on every tick (6 `debris_expired` with reason `recycled`); `oldest` recycles the first box's pieces, `smallest` recycles pieces no larger than any survivor (0.085 vs 0.091 m^3) |
| Debris on a floor, `sleep_velocity` 0.5, `lifetime` 6 s | 10 of 10 fragments asleep 3 s after the fracture; all 10 expire after 6 s with one `debris_expired` event each and their entities despawned |
| Events | exactly one `fractured` event per fracture across 140 further ticks |
| Authored tetrahedron `mesh` on a ball body | 4 fragments, volume 1/6 within 1e-4 |
| Validation | out-of-range fields fail the component schema at load; `max_active < fragment_count`, a ball body without `mesh`, and a missing body are rejected with named errors |

engine-physics (`cargo test -p engine-physics --test destruction_support`): the contact
impulses of a 10 m/s ball landing on the ground sum to its momentum `m v` within 10%, the
reported contact point is under the ball, and `sleep` / `is_sleeping` / impulse wake behave.

## Deferred and approximations

- No authored `Destructible2D`: the crate fractures 2D polygons, but the runtime only drives
  3D bodies.
- No MCP preview tool: `preview_fracture` is native only (the MCP registry and its tool-count
  tests are a shared hotspot).
- Concave meshes needing more than 32 splitting planes are refused, not approximated.
- The fracture surface material and the renderer hookup are left to the rendering domain
  (the data is ready: `interior` flags and `fragment_mesh`).
- A fragment forced to sleep resting on another can float if that support expires first; the
  default lifetimes expire a whole fracture together.
