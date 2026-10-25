# engine-xpbd

Headless extended position-based dynamics (XPBD, small-steps substepping) for triangle-mesh
cloth and tetrahedral soft bodies. Depends only on `glam`; no ECS, project-format, device or
window dependency. Units are meters, seconds and kilograms.

```rust
use engine_xpbd::{ClothDesc, ColliderProxy, ProxyShape, XpbdSettings, XpbdWorld};
use glam::{Quat, Vec3};
let mut world = XpbdWorld::new(XpbdSettings::default(), 100_000)?;
let mut cloth = ClothDesc::grid(Vec3::new(-0.5, 1.0, -0.5), Vec3::X, Vec3::Z, 16, 16)?;
cloth.params.tether_slack = Some(1.0);
cloth.pinned = vec![0, 16];
world.insert_cloth(1, cloth)?;
let ball = ColliderProxy {
    id: 9, position: Vec3::ZERO, rotation: Quat::IDENTITY,
    linear_velocity: Vec3::ZERO, angular_velocity: Vec3::ZERO,
    shape: ProxyShape::Sphere { radius: 0.3 }, friction: 0.5,
};
world.cloth_mut(1).unwrap().set_pin_target(0, Vec3::new(-0.4, 1.0, -0.5))?;
world.step(1.0 / 60.0, &[ball])?;
let cloth = world.cloth(1).unwrap();
let (positions, normals) = (cloth.positions(), cloth.normals());
for reaction in world.proxy_impulses() {
    // apply reaction.impulse at reaction.point to rigid body reaction.id
}
```

- `XpbdWorld`: objects under caller-chosen `u64` ids (one namespace for cloth and soft
  bodies), a total particle budget, fixed substeps and iterations, gravity, uniform wind and
  air density. Objects step in ascending id order; identical inputs give bitwise-identical
  results.
- Cloth: stretch, signed dihedral bending and optional long-range tether constraints with
  compliance, damping, thin-plate aerodynamic drag and lift, pins with per-step targets
  (`set_pin_target`) for following skinned bones, per-vertex normals. `grid_mesh` builds
  rectangles.
- Soft bodies: edge-length and tetrahedron-volume constraints (volume compliance is the
  inverse bulk modulus), `box_tet_mesh` generator, outward `surface_triangles` for rendering.
- Collider proxies (`ColliderProxy`, `ProxyShape`: sphere, Y capsule, box, world plane;
  identical to `engine-fluids`) supplied by the host each step; particles are projected out
  with Coulomb friction relative to the moving proxy surface, and `proxy_impulses()` reports
  the reaction impulse, application point and angular impulse per proxy id, sorted.
- Limits: `MAX_OBJECT_PARTICLES` 65,536, `MAX_OBJECT_TRIANGLES` 131,072, `MAX_OBJECT_TETS`
  262,144, `MAX_WORLD_PARTICLES` 1,048,576, `MAX_OBJECTS` 4096, `MAX_PROXIES` 1024,
  `MAX_SUBSTEPS` 64, `MAX_ITERATIONS` 16, `MAX_STEP` 0.25 s. Non-finite or out-of-range
  values, out-of-range indices, repeated vertices, duplicate or degenerate triangles and
  tetrahedra, unreferenced vertices and exceeded capacities are rejected.

Engine integration (`engine.Cloth`, `engine.SoftBody`, rigid coupling, render data) lives in
`engine-project`; see [docs/physics-cloth.md](../../docs/physics-cloth.md). Deferred:
self-collision, triangle/edge and continuous proxy collision, shape matching, GPU
acceleration and drawing. Details and
measured behaviour: [cloth](../../docs/physics-cloth.md) and
[soft bodies](../../docs/physics-softbody.md).

Run the release timing with
`cargo test -p engine-xpbd --release --test perf -- --ignored --nocapture`.
