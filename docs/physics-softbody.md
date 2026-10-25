# Soft bodies (XPBD)

`crates/engine-xpbd` simulates tetrahedral soft bodies with extended position-based
dynamics. The solver loop, `XpbdWorld`, `XpbdSettings`, collider proxies, friction and the
per-proxy reaction impulses are shared with cloth and described in
[Cloth](physics-cloth.md#solver); this page covers what is specific to soft bodies.

Units are meters, seconds and kilograms. Status: CPU reference implementation, native Rust
API only (see [Deferred](#deferred)).

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

## Deferred

- **Shape matching**: not implemented; edge and volume constraints only.
- **Surface collision**: only vertices collide with proxies, and soft bodies do not
  collide with each other or with cloth.
- **Tetrahedralization of arbitrary meshes**: callers supply tetrahedra (or use the box
  generator); no Delaunay/TetGen-style mesher is included.
- **GPU acceleration** and **engine integration** (components, MCP, events, Rapier proxy
  filling, rendering through `PipelineFeature`): as for [cloth](physics-cloth.md#deferred).
