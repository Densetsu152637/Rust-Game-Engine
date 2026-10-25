# engine-world-render

Render features for world building, drawn through the render extension API
(`engine-render-pipeline`). One module per feature:

| Module | Features | Stage |
| --- | --- | --- |
| `terrain` | `world.terrain` | `Opaque`, after `engine.opaque` |
| | `world.terrain.normals` | `Prepass`, after `engine.prepass.normals` (only when a feature needs normals) |
| `foliage` | `world.foliage` | `Opaque`, after `engine.opaque` |

`world.water` will be added here as a further module.

`world.terrain` draws the terrains of `engine_project::world::terrain::TerrainRenderSet`:
CPU CDLOD selection per view, frustum culling, one instanced draw per terrain, GPU
geomorphing from an `R32Float` height texture (mirroring `engine_terrain::lod`), splat-layer
blending of up to 8 layers with tiled albedo arrays, `engine::lighting` shading and hole
discard.

```rust
// In the game's configure(app), before the runner starts:
let status = engine_world_render::enable(app)?;
// or separately:
engine_world_render::install(app); // extraction + TerrainGpuRenderer marker
let mut features = PipelineFeatures::default();
let status = engine_world_render::register(&mut features)?;
app.world_mut().insert_resource(features);
```

With the renderer installed, `render_mode: auto` terrains stop spawning CPU fallback chunk
entities, so nothing is drawn twice. GPU tests:
`cargo test -p engine-world-render --test gpu_terrain -- --ignored --test-threads=1`.
See [docs/terrain.md](../../docs/terrain.md#gpu-rendering-engine-world-render).

`world.foliage` draws the `FoliageRenderSet` of `engine_project::world::foliage`: per-view
cell and instance frustum / distance culling, LOD levels with hysteresis and camera-facing
impostors, one instanced draw per (type, level), and wind sway in WGSL matching
`engine_foliage::WindParams::sway_offset`. `install` also inserts the `FoliageGpuRenderer`
marker (so `auto` foliage builds no CPU clusters), `register` registers it with the terrain
features and `register_all` returns both status handles. It casts no shadows (the extension
API has no shadow hook). GPU tests:
`cargo test -p engine-world-render --test gpu_foliage -- --ignored --test-threads=1`.
See [docs/world-foliage.md](../../docs/world-foliage.md#gpu-rendering-engine-world-render).
