# engine-world-render

Render features for world building, drawn through the render extension API
(`engine-render-pipeline`). One module per feature:

| Module | Features | Stage |
| --- | --- | --- |
| `terrain` | `world.terrain.shadow` | `Shadow`, after `engine.shadows` (terrain depth into the view's terrain shadow map, one layer per cascade of the first directional light) |
| | `world.terrain.normals` | `Prepass`, after `engine.prepass.normals` (only when a feature needs normals) |
| | `world.terrain` | `Opaque`, after `engine.opaque` |
| | `world.terrain.motion` | `Opaque`, after `world.terrain` (only when a feature or the host needs motion) |
| `foliage` | `world.foliage` | `Opaque`, after `engine.opaque` |
| `water` | `world.water` | `Transparent`, before `engine.transparent` |
| | `world.water.underwater` | `Transparent`, after `engine.transparent` and `world.water` (only when the view camera is under a surface) |

`world.terrain` draws the terrains of `engine_project::world::terrain::TerrainRenderSet`:
CPU CDLOD selection per view, frustum culling, one instanced draw per terrain, GPU
geomorphing from an `R32Float` height texture (mirroring `engine_terrain::lod`), splat-layer
blending of up to 8 layers with tiled albedo arrays, `engine::lighting` shading and hole
discard. Terrain shadows terrain through `world.terrain.shadow` (engine cascade matrices, a
crate-owned 4 x 1024^2 depth array per view, sampled with the engine cascade selection and bias)
and receives engine mesh shadows from the engine atlas; `world.terrain.motion` writes camera,
transform and geomorph motion for DLSS/TAA over the visible terrain.

```rust
// In the game's configure(app), before the runner starts:
let status = engine_world_render::enable(app)?;
// or separately:
engine_world_render::install(app); // extraction + TerrainGpuRenderer marker
let mut features = PipelineFeatures::default();
let status = engine_world_render::register(&mut features)?;
app.world_mut().insert_resource(features);
```

`world.foliage` draws the `FoliageRenderSet` of `engine_project::world::foliage`: per-view
cell and instance frustum / distance culling, LOD levels with hysteresis and camera-facing
impostors, one instanced draw per (type, level), and wind sway in WGSL matching
`engine_foliage::WindParams::sway_offset`. `install` also inserts the `FoliageGpuRenderer`
marker (so `auto` foliage builds no CPU clusters), `register` registers it with the terrain
features and `register_all` returns both status handles. It casts no shadows: features cannot
render into the engine shadow atlas (see docs/world-foliage.md, Limits). GPU tests:
`cargo test -p engine-world-render --test gpu_foliage -- --ignored --test-threads=1`.
See [docs/world-foliage.md](../../docs/world-foliage.md#gpu-rendering-engine-world-render).

`world.water` draws the bodies of `engine_project::world::water::WaterRenderSet`: one draw per
body over a shared unit grid (rivers: their ribbon), Gerstner displacement in WGSL or the
uploaded FFT ocean frame (`Rgba32Float` displacement/normal textures), per-vertex shoreline
attenuation, screen-space refraction of a copy of the HDR scene, Beer-Lambert absorption over
the water thickness from a copy of the scene depth, Schlick Fresnel sky/sun reflection and
crest plus shoreline foam. `world.water.underwater` fogs the view by distance when its camera
is below a surface. `water::register` returns a `WaterRenderStatusHandle`.

With the renderer installed, `render_mode: auto` terrains and water bodies stop spawning CPU
fallback entities (the `TerrainGpuRenderer` / `WaterGpuRenderer` markers), so nothing is drawn
twice.

Host defaults: `register_missing(&mut features)` registers each module (terrain, foliage,
water) none of whose names is registered yet, and `install_missing(app)` installs each module
whose GPU marker is absent, so both merge with a game's own registration and are idempotent.
`engine-desktop` calls them by default (`install_default_render_features`); `names::ALL` lists
every feature. GPU tests:
`cargo test -p engine-world-render --test gpu_terrain --test gpu_water -- --ignored --test-threads=1`
(terrain: shading, geomorph parity, ridge and box shadows, motion vectors).
See [docs/terrain.md](../../docs/terrain.md#gpu-rendering-engine-world-render) and
[docs/world-water.md](../../docs/world-water.md#gpu-rendering).
