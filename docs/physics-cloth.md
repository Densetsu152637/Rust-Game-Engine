# Cloth (XPBD)

`crates/engine-xpbd` simulates triangle-mesh cloth and tetrahedral soft bodies with
extended position-based dynamics. This page covers the shared world, the collider proxies
and rigid-body coupling, and cloth. Soft bodies are described in
[Soft bodies](physics-softbody.md). The crate is ECS-free and depends only on `glam`; the
crate [README](../crates/engine-xpbd/README.md) has a short example.

Units are meters, seconds and kilograms. Status: CPU reference implementation, native Rust
API only. Authored components, MCP tools, runtime events and rendering hookup are not
part of this crate yet (see [Deferred](#deferred)).

## Solver

Every `XpbdWorld::step(dt, proxies)` is split into `substeps` equal substeps
("small steps" XPBD, Macklin et al. 2019). Each substep:

1. computes per-triangle aerodynamic forces (cloth only),
2. predicts `v += h (g + w f)`, `x += h v`; pinned particles move linearly toward their
   target and land on it exactly at the last substep,
3. resets the Lagrange multipliers and runs `iterations` Gauss-Seidel passes over the
   constraints in their stable creation order (cloth: stretch, tethers, bending),
4. projects particles out of every proxy (in proxy id order) with friction,
5. derives `v = (x - x_prev) / h`, applies damping `v /= 1 + damping h` and clamps speed to
   `MAX_SPEED`.

Compliance `alpha` is the inverse stiffness; the solver uses `alpha / h^2`, so behaviour is
independent of the substep count apart from convergence. Zero compliance means "as stiff
as the substeps allow": stiffness still converges with `h^2`, so heavy loads hanging from
few pins stretch measurably unless tethers are on (see [Measured
behaviour](#measured-behaviour)).

Objects step in ascending id order and never interact with each other. For identical
inputs results are bitwise identical on one machine and build (tested); there is no
cross-platform float determinism guarantee because `f32` transcendental functions come
from the platform.

## World and settings

```rust
use engine_xpbd::{ClothDesc, XpbdSettings, XpbdWorld};
use glam::Vec3;
let mut world = XpbdWorld::new(XpbdSettings::default(), 100_000)?;
let mut cloth = ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::Z, 16, 16)?;
cloth.pinned = vec![0, 16];
world.insert_cloth(42, cloth)?;
world.step(1.0 / 60.0, &[])?;
let positions = world.cloth(42).unwrap().positions();
```

`XpbdWorld::new(settings, max_particles)` takes a total particle budget
(1..=`MAX_WORLD_PARTICLES` = 1,048,576). Objects live under caller-chosen `u64` ids (one
namespace for cloths and soft bodies, at most `MAX_OBJECTS` = 4096), so an ECS adapter can
use entity bits: `insert_cloth`, `insert_soft_body`, `remove`, `kind`, `ids` (ascending),
`cloth`/`cloth_mut`, `soft_body`/`soft_body_mut`, `step`, `proxy_impulses`.

| `XpbdSettings` field | Default | Range | Meaning |
| --- | --- | --- | --- |
| `gravity` | `(0, -9.81, 0)` m/s^2 | finite, each axis within `MAX_SPEED` | Gravity acceleration. |
| `wind` | `0` m/s | as above | Uniform wind velocity for cloth aerodynamics. |
| `air_density` | `1.2` kg/m^3 | 0..=100 | 0 disables aerodynamics, including still-air drag. |
| `substeps` | `10` | 1..=64 | Fixed substeps per step. |
| `iterations` | `1` | 1..=16 | Constraint passes per substep. |

`step` rejects `dt` outside `(0, MAX_STEP]` (0.25 s). A rejected call leaves the world
unchanged. A non-finite result (not expected for validated input) returns an error after
the step; the host should remove or recreate the affected objects.

## Collider proxies and two-way coupling

The host passes the rigid bodies particles collide with as plain data every step. The type
is identical to the one in `engine-fluids` (same fields, variants and derives
`Clone, Copy, Debug, PartialEq`):

```rust
pub enum ProxyShape {
    Sphere { radius: f32 },
    Capsule { half_height: f32, radius: f32 }, // segment along local Y
    Box { half_extents: Vec3 },
    Plane { normal: Vec3, offset: f32 },       // world space, pose ignored
}
pub struct ColliderProxy {
    pub id: u64, pub position: Vec3, pub rotation: Quat,
    pub linear_velocity: Vec3, pub angular_velocity: Vec3,
    pub shape: ProxyShape, pub friction: f32,
}
```

- The pose is used for the whole step (the host passes the rigid pose for this step).
  Velocities only define the surface motion for friction,
  `linear_velocity + angular_velocity x (p - position)`. Planes are solid where
  `normal . x < offset` (the normal is normalized; pose and velocities are ignored).
- Validation (`ColliderProxy::validate`, also run by `step`): finite values, positions within
  `MAX_COORDINATE` (1e5 m), velocities within `MAX_SPEED` (1e4), rotation length within
  1e-3 of 1, radius and half extents positive, half height non-negative, extents within
  `MAX_EXTENT` (1e4 m), plane normal non-zero, friction 0..=10, at most `MAX_PROXIES` (1024)
  per step, unique ids.
- Contacts: a free particle closer than the object's margin (cloth `thickness`, soft-body
  `particle_radius`) to a proxy surface is moved out along the closest-feature normal. Its
  tangential displacement relative to the proxy surface is then reduced by
  `mu * penetration` (Coulomb friction with equal static and kinetic coefficient), where
  `mu` is the average of the object's and the proxy's friction. Pinned and static particles
  do not collide.
- Reaction: every contact correction `dx` changes the particle momentum by `m dx / h`.
  After the step, `proxy_impulses()` lists one `ProxyImpulse` per proxy that had contacts,
  sorted by id: `impulse` (N s, the negated sum to apply to the rigid body), `point`
  (contact points weighted by impulse magnitude), `angular_impulse` (exact torque impulse
  about `ColliderProxy::position`) and `contacts` (particle contacts summed over substeps).
  A host applies either `impulse` at `point` or `impulse` at the body origin plus
  `angular_impulse`.

Only particles (vertices) collide; triangle faces and edges are not tested against
proxies, so thin or sharp proxies can pass between vertices of a coarse mesh.

## Cloth

`ClothDesc` holds `positions`, `triangles` (`[u32; 3]`), `areal_density` (kg/m^2, default
0.2 from `ClothDesc::grid`), optional explicit `inverse_masses` (1/kg, 0 = static
particle), `pinned` indices (pinned at their rest position) and `params`. Masses are lumped:
each vertex gets a third of every adjacent triangle's mass. Rest lengths and rest dihedral
angles come from `positions`, so pre-curved cloth keeps its shape.

`grid_mesh(origin, u, v, segments_u, segments_v)` / `ClothDesc::grid` build a rectangle:
vertex `(i, j)` has index `j * (segments_u + 1) + i`, triangles wind toward `u x v` and cell
diagonals alternate in a checkerboard (mirror symmetric for even segment counts).

Constraints:

- **Stretch**: one distance constraint per unique triangle edge, compliance
  `stretch_compliance` (m/N per edge).
- **Bending**: one signed dihedral-angle constraint per interior edge shared by exactly two
  triangles (Bridson et al. 2003 gradients, non-singular at the flat state), compliance
  `bend_compliance` (1/(N m) per hinge; mesh dependent). Boundary and non-manifold edges get
  no hinge.
- **Tethers** (long-range attachments, Kim et al. 2012): with `tether_slack = Some(s)`,
  every free particle connected to a pin is kept within `s` times its rest geodesic distance
  (multi-source Dijkstra over rest edge lengths) from its nearest pinned particle.
  Unilateral and rebuilt when the pinned set changes.
- **Aerodynamics**: per triangle, relative flow `wind - mean vertex velocity`, dynamic
  pressure `q = 0.5 rho A |v|^2`; drag `drag * q * |cos|` along the flow and lift
  `lift * q * |cos| sin` along the plate normal's component perpendicular to the flow
  (thin-plate model), split equally over the three vertices.
- **Pins**: `set_pin_target(particle, position)` pins a particle (if needed) and sets the
  world-space position it reaches exactly at the end of the next step, interpolating
  linearly over the substeps; call it every frame with a skinned bone position to attach
  cloth to a character. `unpin` releases it with the velocity of its last pinned motion;
  `pin_target` queries it.

| `ClothParams` field | Default | Range | Unit |
| --- | --- | --- | --- |
| `stretch_compliance` | 0 | 0..=1e6 | m/N |
| `bend_compliance` | 1000 | 0..=1e6 | 1/(N m) |
| `tether_slack` | `None` | `Some(1..=10)` | factor |
| `damping` | 0.5 | 0..=1000 | 1/s |
| `thickness` | 0.01 | 0..=1e4 | m |
| `friction` | 0.5 | 0..=10 | - |
| `drag` | 1.0 | 0..=100 | - |
| `lift` | 0.5 | 0..=100 | - |

`set_params` replaces them at runtime; `set_inverse_mass` changes one particle.

Outputs: `positions`, `velocities`, `normals` (unit area-weighted vertex normals on the
`u x v` side of the winding, updated after every step), `triangles`, `inverse_masses`,
`constraint_counts` (stretch, bending, tethers).

Validation: 3..=`MAX_OBJECT_PARTICLES` (65,536) finite positions within 1e5 m,
1..=`MAX_OBJECT_TRIANGLES` (131,072) triangles, indices in range, no repeated vertex in a
triangle, no duplicate triangle, area at least `MIN_TRIANGLE_AREA` (1e-10 m^2), every
vertex referenced, inverse masses finite in 0..=1e6 with one per particle, positive areal
density, pinned indices in range, parameters in the ranges above, and the world particle
and object budgets.

## Measured behaviour

From `cargo test -p engine-xpbd -- --nocapture` (debug build, 60 Hz, 10 substeps, 1
iteration, 16x16 cloth of 1 m x 1 m unless noted):

| Scenario | Result |
| --- | --- |
| Pinned at two adjacent corners, compliance 0, 6 s | Max edge stretch 1.19% with tethers (5.24% without). Lowest point 1.0013 m below the pins, top-edge midpoint sag 3.3 mm, max speed 4.9 mm/s, drift over one more second 0.84 mm. |
| Same, pins moved 0.8 m apart (forced folds) | Mean fold angle 0.044 rad at bend compliance 0 vs 0.162 rad at 1e4. |
| 20x20 cloth draped on a 0.3 m sphere, thickness 0.01 m | Closest particle 0.0100 m above the surface over the whole run (no penetration); centre rests at y = 0.310. |
| 8x8 patch on a box tilted 20 degrees, 0.5 s | Friction 0.8: 0.7 mm slide. Friction 0: 0.42 m (analytic 0.42 m). |
| Hanging cloth, 5 m/s wind along +Z, 3 s | Mean displacement 0.51 m downwind. Calm air: max out-of-plane offset exactly 0, mirror asymmetry 1.9 mm (Gauss-Seidel order). |
| Pin target moved 0.3 m in one step (no gravity) | Pinned particle lands exactly on the target; neighbours dragged 0.21 m. |
| Stretchy cloth (2e-2 m/N), bottom-corner distance to its pin | 1.050 m without tethers, 1.000 m with `tether_slack = 1.0`. |
| Cloth on a box moving at 0.5 m/s (friction 0.8) | Cloth reaches 0.500 m/s; reported reaction matches the cloth momentum within 0.11%. |
| 32x32 cloth on a sphere with wind and tethers (release) | 2.1 ms per 1/60 s step on the reference machine (`tests/perf.rs`, ignored by default). |

## Deferred

- **Self-collision** (cloth-cloth and cloth-soft body): needs a spatial hash with
  continuous or sub-stepped particle-triangle tests to be robust; not implemented.
- **Triangle/edge vs proxy collision and continuous collision**: only vertices collide;
  fast particles can tunnel through thin proxies.
- **Separate static/kinetic friction coefficients**: one coefficient serves both.
- **GPU acceleration**: CPU reference path only; a compute-shader solver (graph colouring
  or Jacobi) is future work.
- **Engine integration**: authored components, schema, MCP editing, `physics.cloth.*`
  events, Rapier proxy filling and rendering through the rendering domain's
  `PipelineFeature` API ([render architecture](rendering-architecture.md)) are a
  follow-up step outside this crate.
