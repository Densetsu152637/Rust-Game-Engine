# engine-terrain

Pure-Rust heightmap terrain data model (no GPU, ECS or file-system access).
Dependencies: `glam`, `serde`, `serde_json`. No `unsafe`.

- `Heightfield`: bounded `f32` sample grid (2..=4097 per side, at most 2049 x 2049) with
  spacing; triangulated `height_at` on the shared `(x0, z1)`-`(x1, z0)` diagonal (the
  Rapier/Parry split) and `bilinear_height_at`; face, sample and smooth normals; bounds;
  a per-cell holes mask; a splat map of 1..=8 normalized layer weights with
  `layer_weight_at`; `TerrainLayer` definitions (texture id, tint, UV tiling).
- `lod`: CDLOD-style quadtree selection (`LodLayout::select`) with per-level ranges, a node
  budget and per-vertex geomorphing that is crack-free across level boundaries
  (`tests/lod_cracks.rs` proves it numerically).
- `mesh::build_chunk_mesh`: CPU node meshes (positions, normals, UVs, splat colours, hole
  triangles removed).
- `brush`: deterministic raise/lower, smooth, flatten, set-height and noise sculpting, splat
  painting and hole painting with circular brushes, returning the dirty region.
- `asset`: the versioned, checksummed, bounded binary `.terrain` file.
- `collision`: physics tiles (heightfields, triangle meshes where holes are).
- `navigation`: the walkable, raster-aligned triangle soup for navmesh baking.

```rust
use engine_terrain::*;

let mut field = Heightfield::new(257, 257, 1.0)?;
sculpt(&mut field, &Brush::new([128.0, 128.0], 40.0), SculptOp::Raise)?;
let rock = field.add_layer(TerrainLayer::new("rock", [0.5, 0.5, 0.5, 1.0]))?;
paint_layer(&mut field, &Brush::new([100.0, 90.0], 12.0), rock)?;
let layout = LodLayout::for_heightfield(&field, &LodSettings::default())?;
let selection = layout.select(glam::Vec3::new(128.0, 30.0, 128.0), field.height_range());
let meshes: Vec<ChunkMesh> = selection
    .nodes
    .iter()
    .map(|node| build_chunk_mesh(&field, &layout, node, &selection.viewer))
    .collect();
let bytes = asset::encode(&field);
```

See [docs/terrain.md](../../docs/terrain.md) for the formats, the LOD proof and how
`engine-project` and `engine-world-render` use it.
