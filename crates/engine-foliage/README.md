# engine-foliage

Pure-Rust foliage and vegetation data model (dependencies: `glam`, `serde`, `engine-pcg`).
No GPU, ECS or file-system access, so tools, tests and games share one code path. The scene
integration (`engine.Foliage`) lives in `engine-project`, the GPU feature (`world.foliage`)
in `engine-world-render`; see [docs/world-foliage.md](../../docs/world-foliage.md).

| Module | What |
| --- | --- |
| `types` | `FoliageType` (mesh, LOD meshes, impostor, density, scale / yaw / tilt, slope / height / altitude limits, align to normal, min spacing, exclusions, LOD distances, wind weight), `DensityMap` (uniform, terrain paint layer, explicit grid, texture) |
| `field` | `FoliageField`: instances in spatial cells, at most 262 144, per-cell revisions, disc queries, serde |
| `scatter` | `scatter(types, &dyn Surface, &ScatterParams)`: deterministic candidates per world-grid cell, limits, zones, holes, minimum-distance thinning |
| `paint` | `paint(field, types, ctx, brush, PaintOp::{Add, Erase}, seed)` |
| `lod` | `LodBands` (mesh LODs, impostor, cull; hysteresis) and `LodTracker` |
| `wind` | `WindParams::sway_offset` (the CPU evaluation of the shader contract) |
| `asset` | the versioned, checksummed `.foliage` binary file |

```rust
use engine_foliage::*;
use engine_pcg::{Aabb, FlatSurface};

let mut grass = FoliageType::new("grass", "grass_mesh");
grass.density = 2.0; // per square metre
let bounds = Aabb::new([0.0, 0.0, 0.0], [64.0, 50.0, 64.0]);
let result = scatter(&[grass], &FlatSurface::new(0.0), &ScatterParams::new(7, bounds))?;
println!("{} instances", result.field.len());
```

Tests: `cargo test -p engine-foliage` (unit tests for every module plus `tests/behaviour.rs`
with numeric density, exact limit, determinism, spacing, exclusion, hole and alignment checks).
