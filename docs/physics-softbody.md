# Soft bodies (XPBD)

`crates/engine-xpbd` simulates tetrahedral soft bodies with extended position-based
dynamics. The solver loop, `XpbdWorld`, `XpbdSettings`, collider proxies, friction and the
per-proxy reaction impulses are shared with cloth and described in
[Cloth](physics-cloth.md#solver); this page covers what is specific to soft bodies.

Units are meters, seconds and kilograms. The crate is a CPU reference implementation with a
native Rust API; the authored `engine.SoftBody` component, rigid coupling and rendering are
wired (see [Authored component](#authored-component-enginesoftbody)).

## Creating a soft body

```rust
use engine_xpbd::{SoftBodyDesc, XpbdSettings, XpbdWorld};
use glam::Vec3;
let mut world = XpbdWorld::new(XpbdSettings::default(), 100_000)?;
let mut cube = SoftBodyDesc::cuboid(Vec3::splat(-0.25), Vec3::splat(0.25), [4, 4, 4], 200.0)?;
cube.params.volume_compliance = 1.0e-4;
world.insert_soft_body(7, cube)?;
world.step(1.0 / 60.0, &[])?;
let body = world.soft_body(7).unwrap();
let (positions, triangles) = (body.positions(), body.surface_triangles());
```

`SoftBodyDesc` holds `positions`, `tets` (`[u32; 4]`), `density` (kg/m^3), optional explicit
`inverse_masses` (1/kg, 0 = static particle), `pinned` indices (pinned at their rest
position) and `params`. Each vertex gets a quarter of every adjacent tetrahedron's mass.
Tetrahedra of either orientation are accepted; negatively oriented ones are reoriented by
swapping two vertices, and `tets()` returns the oriented set.

Helpers:

- `box_tet_mesh(min, max, [nx, ny, nz])` (and `SoftBodyDesc::cuboid`) builds an
  axis-aligned box with `(nx + 1)(ny + 1)(nz + 1)` vertices, index
  `(k * (ny + 1) + j) * (nx + 1) + i`, and six positively oriented tetrahedra per cell
  (Kuhn subdivision, conforming across cells). 1..=1024 segments per axis.
- `surface_triangles(positions, tets)` returns the boundary faces (faces used by exactly one
  tetrahedron), wound outward, in ascending order of their sorted vertex triple. A
  `SoftBody` precomputes them: `surface_triangles()` indexes `positions()` directly, so a
  renderer can upload positions and the fixed index list every frame.

## Constraints and parameters

- **Edges**: one distance constraint per unique tetrahedron edge, compliance
  `edge_compliance` (m/N per edge; mesh dependent).
- **Volume**: one constraint `C = V - V0` per tetrahedron with compliance
  `volume_compliance * V0`, where `volume_compliance` is the inverse bulk modulus (1/Pa),
  so the volumetric response does not depend on the tetrahedron size.
- **Pins**: `set_pin_target`, `unpin` and `pin_target` behave exactly as for cloth (the
  particle reaches the target at the end of the next step), so a soft body can be attached
  to a skinned bone.

| `SoftBodyParams` field | Default | Range | Unit |
| --- | --- | --- | --- |
| `edge_compliance` | 0 | 0..=1e6 | m/N |
| `volume_compliance` | 0 | 0..=1e6 | 1/Pa |
| `damping` | 0.5 | 0..=1000 | 1/s |
| `particle_radius` | 0.01 | 0..=1e4 | m (collision margin) |
| `friction` | 0.5 | 0..=10 | - |

`set_params` replaces them at runtime; `set_inverse_mass` changes one particle.

Outputs: `positions`, `velocities`, `tets`, `surface_triangles`, `inverse_masses`,
`rest_volume` and `volume` (sums of rest and current signed tetrahedron volumes).

Validation: 4..=`MAX_OBJECT_PARTICLES` (65,536) finite positions within 1e5 m,
1..=`MAX_OBJECT_TETS` (262,144) tetrahedra, indices in range, no repeated vertex, no
duplicate tetrahedron, `|V| >= MIN_TET_VOLUME` (1e-12 m^3), every vertex referenced, inverse
masses finite in 0..=1e6 with one per particle, density in (0, 1e5], pinned indices in range,
parameters in the ranges above, and the world particle and object budgets.

## Measured behaviour

From `cargo test -p engine-xpbd -- --nocapture` (debug build, 60 Hz, 10 substeps, 1
iteration):

| Scenario | Result |
| --- | --- |
| 0.5 m cube (4x4x4 cells, 200 kg/m^3) dropped 0.3 m onto a plane, compliance 0, 3 s | Lowest vertex at y = 0.01000 (the particle radius), height 0.4998 m, volume error -0.05%, max speed 5e-5 m/s. |
| Same cube, edge compliance 3e-3 m/N, volume compliance 3e-4 1/Pa | Squashed 23 mm (stiff: 0.2 mm), volume error -5.2%, still resting at y = 0.01000. |
| 0.3 m cube (100 kg/m^3, 2.7 kg) falling onto a box proxy, 120 frames | Reported proxy impulse matches the particles' momentum change minus gravity within 1.7e-4 relative on every frame, including the impact (peak 3.68 N s). At rest the impulse equals the weight impulse `m g dt` (0.4414 N s) applied under the centre. |

## Authored component: `engine.SoftBody`

Add `engine.SoftBody` to an entity with a `Transform`. Fields: `source` (`box` or `mesh`),
`size` and `resolution` (box cells per axis, 1..=32; the box is centred on the entity),
inline `positions`/`tets` (up to 16,384 vertices and 65,536 tetrahedra), `pinned`,
`density` (kg/m^3), `edge_compliance`, `volume_compliance` (1/Pa), `damping`,
`particle_radius` (collision margin), `friction`, `gravity_scale`, `substeps`, `iterations`
and the coupling fields below. Validation errors are named `engine.SoftBody: invalid ...`.
There is no `physics.softbody.*` event.

### Rigid coupling

Every engine-physics substep the feature turns the 3D rigid bodies near the object into
collider proxies (current pose, linear and angular velocity, material friction), steps the
solver with the same fixed `dt`, and applies the reported reaction impulse (linear, plus the
angular part about the body origin) to **dynamic** bodies, multiplied by `coupling_scale`.
`two_way: false` keeps the one-way collision. Bodies are used when `material.layer &
collision_mask != 0` (sensors never collide).

| Body shape | Proxy |
| --- | --- |
| box, ball, capsule | exact |
| cylinder | local bounding box (`radius`, `half_height`) |
| convex hull | local bounding box of the hull points |
| compound | local bounding box of the parts' bounding spheres |
| trimesh, heightfield | **not coupled** (no simple plane/box path exists; put a fixed box collider under them) |

Only bodies within the object's bounds plus 0.25 m are passed to the solver; more than
1024 (cloth, soft body) or 256 (fluid) nearby proxies keep the nearest ones (ties by entity id)
and leave the others out of that step's coupling; the omissions are counted
(`PressureStatus::proxies_omitted`) and announced once per episode with `physics.proxies.limited`
([Physics](physics.md#pressure-and-capacity)). The step never fails for this.

### Transforms and render data

Particles simulate in **world space**. The entity `Transform` is only the creation anchor
(the mesh is placed with its world pose when the object is built); it is not written back
and moving it later does not move the object. Structural edits (mesh, size, resolution,
pins, density) rebuild the object at the entity's current pose; material, wind, damping,
gravity scale, substeps and coupling edits apply live.

After each frame in which the simulation stepped, the feature publishes the `PhysicsRenderData`
world resource (`engine_project::simulation::physics::PhysicsRenderData`) for a render feature
to read from the extracted world (see [render architecture](rendering-architecture.md)):

```rust
pub struct PhysicsRenderData {
    pub frame: u64,                                    // bumped on every refresh
    pub cloths: BTreeMap<Entity, ClothRender>,         // { positions, normals, triangles: Arc<[[u32; 3]]> }
    pub soft_bodies: BTreeMap<Entity, SoftBodyRender>, // { positions, normals, triangles (surface), volume, rest_volume }
    pub fluids: BTreeMap<Entity, FluidRender>,         // { positions, velocities, densities, surface, particle_radius,
                                                       //   rest_density, capacity, domain_min, domain_max }
}
```

`deformable_debug_segments(&World, max_segments)` returns cloth and soft-body edges and
fluid particles (a short velocity tick, brighter on the surface) as world-space segments,
bounded and only when `engine.PhysicsSettings.debug` is on. The engine-physics debug
contributor hook takes a stateless function pointer, so these segments are not merged into
`debug_view`.

## Deferred

- **Shape matching**: not implemented; edge and volume constraints only.
- **Surface collision**: only vertices collide with proxies, and soft bodies do not
  collide with each other or with cloth.
- **Tetrahedralization of arbitrary meshes**: callers supply tetrahedra (or use the box
  generator); no Delaunay/TetGen-style mesher is included.
- **GPU acceleration**: as for [cloth](physics-cloth.md#deferred). Components, MCP editing,
  rigid proxy filling and rendering are done; soft bodies emit no `physics.softbody.*` events.
