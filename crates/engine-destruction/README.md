# engine-destruction

Deterministic Voronoi pre-fracture of closed triangle meshes and convex polygons. The crate
depends on `glam` and `std` only (no Rapier, no ECS); `engine-project` uses it for
`engine.Destructible3D` ([docs/physics-destruction.md](../../docs/physics-destruction.md)).

```rust
use engine_destruction::{Limits, Mesh, Pattern, fracture};
use glam::Vec3;

let cube = Mesh::cuboid(Vec3::splat(0.5));
let result = fracture(&cube, &Pattern::Uniform { count: 16, seed: 7 }, &Limits::default())?;
assert_eq!(result.fragments.len(), 16);
assert!((result.total_volume() - 1.0).abs() < 1e-4);
for fragment in &result.fragments {
    // hull_points -> convex collider; mesh.interior flags the fracture surface.
    let _ = (&fragment.hull_points, &fragment.mesh, fragment.volume, fragment.centroid);
}
# Ok::<(), engine_destruction::Error>(())
```

- Input: a closed, consistently wound triangle mesh (`Mesh`), checked on entry. Bit-identical
  positions are welded; open or non-manifold meshes are rejected.
- Patterns: `Pattern::Uniform { count, seed }` and `Pattern::Impact { count, seed, point,
  radius }` (75% of seeds inside the radius around the point). `fracture_with_seeds` takes
  explicit seeds; `generate_seeds` returns the seeds a pattern would use.
- Output per `Fragment`: `hull_points` (convex collider), `mesh` (`FragmentMesh`: positions,
  triangles, per-triangle `interior` flag set on cap faces), exact `volume`, `centroid`,
  `inertia` (unit density, about the centroid) and `hull_volume`.
- Convex input is clipped as planar polygon faces (exact, no triangle growth). Concave input
  is first split into convex parts along its reflex face planes (at most 32 splits, else
  `Error::NonRobust`); the Voronoi cell of each seed is then cut against each part, so one
  cell can yield one fragment per part. Every fragment is convex.
- 2D: `fracture_polygon(points, &Pattern2, &Limits)` returns `Fragment2` polygons (counter-
  clockwise points, `fracture_edge` flags, area, centroid, polar inertia) for convex polygons.
- Limits (`Limits`): at most 256 fragments, 65 536 vertices and 20 000 input triangles by
  default; violations return `Error::LimitExceeded` / `InvalidMesh`.
- Deterministic: the same mesh, pattern and seed give bit-identical output.
- `topology(&triangles)` reports vertex/edge/face counts and watertightness (used by tests to
  check Euler characteristic 2 and closure of every fragment).
