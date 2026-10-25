# engine-fluids

Headless 3D particle fluids solved with Position Based Fluids (Macklin & Mueller
2013). The crate depends only on `glam` and `std`: no ECS, renderer, GPU or
rigid-body dependency. Units are meters, seconds and kilograms.

```rust
use engine_fluids::{Domain, FluidParams, FluidWorld, Region, VolumeDesc};
use glam::Vec3;

let mut world = FluidWorld::new();
let volume = world.create_volume(1, VolumeDesc {
    params: FluidParams::default(),
    domain: Domain::new(Vec3::ZERO, Vec3::new(1.0, 1.0, 0.3)),
    capacity: 4096,
})?;
let region = Region::Box {
    center: Vec3::new(0.2, 0.25, 0.15),
    half_extents: Vec3::new(0.2, 0.25, 0.15),
};
volume.fill(region, Vec3::ZERO, None)?; // 480 particles at rest spacing
let report = world.step(1.0 / 60.0, &[])?;
let positions = world.volume(1).unwrap().positions();
```

- `FluidWorld` keys volumes by stable `u64` ids (`create_volume`, `volume`,
  `volume_mut`, `remove_volume`, `step`), the shape an ECS adapter needs.
- `FluidVolume` holds one particle set with an explicit capacity, a closed box
  `Domain` (restitution, Coulomb friction, wall boundary density), `FluidParams`,
  emitters (`set_emitter`: seeded rate-in-region or nozzle disc layers, optional
  lifetime) and kill regions (`set_kill_volume`). `spawn`/`fill` are
  all-or-nothing and fail with `Error::CapacityExceeded`; emitter overflow is
  reported in `StepReport::dropped_at_capacity` and `capacity_reached`.
- Rigid bodies take part through `ColliderProxy` (sphere, Y-axis capsule, box,
  world plane) supplied every step. `StepReport::proxy_impulses` (sorted by id)
  is the impulse the fluid applied to each proxy, for buoyancy and drag.
- Renderer data: `render_output()` (ids, positions, velocities, densities,
  colour-field normals, surface flags) and `sample_density_grid` (normalized
  density for a later marching-cubes surface). Drawing is not part of this
  crate.

All inputs are validated (finite, bounded, explicit limits such as
`MAX_PARTICLES`, `MAX_PROXIES`, `MAX_SUBSTEPS`); rejected calls change nothing.
Iteration is in stable order, so identical runs are bitwise identical on the same
build and platform. Solver choice, parameters, measured stability and
performance, and limits are in [docs/physics-fluids.md](../../docs/physics-fluids.md).
