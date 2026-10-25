# engine-world-render

Render features for world building, drawn through the render extension API
(`engine-render-pipeline`). One module per feature:

| Module | Features | Stage |
| --- | --- | --- |
| `terrain` | `world.terrain` | `Opaque`, after `engine.opaque` |
| | `world.terrain.normals` | `Prepass`, after `engine.prepass.normals` (only when a feature needs normals) |
| `water` | `world.water` | `Transparent`, before `engine.transparent` |
| | `world.water.underwater` | `Transparent`, after `engine.transparent` and `world.water` (only when the view camera is under a surface) |

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

`world.water` draws the bodies of `engine_project::world::water::WaterRenderSet`: one draw per
body over a shared unit grid (rivers: their ribbon), Gerstner displacement in WGSL or the
uploaded FFT ocean frame (`Rgba32Float` displacement/normal textures), per-vertex shoreline
attenuation, screen-space refraction of a copy of the HDR scene, Beer-Lambert absorption over
the water thickness from a copy of the scene depth, Schlick Fresnel sky/sun reflection and
crest plus shoreline foam. `world.water.underwater` fogs the view by distance when its camera
is below a surface. `water::register` returns a `WaterRenderStatusHandle`.

With the renderer installed, `render_mode: auto` terrains and water bodies stop spawning CPU
fallback entities (the `TerrainGpuRenderer` / `WaterGpuRenderer` markers), so nothing is drawn
twice. GPU tests:
`cargo test -p engine-world-render --test gpu_terrain --test gpu_water -- --ignored --test-threads=1`.
See [docs/terrain.md](../../docs/terrain.md#gpu-rendering-engine-world-render) and
[docs/world-water.md](../../docs/world-water.md#gpu-rendering).
