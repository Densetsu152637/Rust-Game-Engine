# Heightmap terrain

Terrain is a heightfield asset plus a runtime service:

| Part | Where | What |
| --- | --- | --- |
| Data model | [`engine-terrain`](../crates/engine-terrain/README.md) | heightfield, holes, splat layers, LOD selection and geomorphing, chunk meshes, brushes, the `.terrain` file, collision tiles, navigation input |
| Scene integration | `engine_project::world::terrain` (`crates/engine-project/src/world/terrain/`) | `engine.Terrain`, collider tiles, PCG surfaces, CPU fallback chunks, the GPU render set, the native editing API |
| GPU rendering | [`engine-world-render`](../crates/engine-world-render/README.md) | the `world.terrain` and `world.terrain.normals` render features |
| Authoring | `crates/engine-mcp/src/tools/terrain.rs` | the `terrain_*` MCP tools |

## Coordinates and triangulation

A heightfield is `columns` x `rows` `f32` samples (each 2..=4097, at most 2049 x 2049 in
total) `spacing` apart (0 < spacing <= 1000). Sample `(column, row)` sits at terrain-local
`(column * spacing, height, row * spacing)`: columns run along +X, rows along +Z, storage is
row-major (`row * columns + column`), the layout of the engine-physics 3D heightfield.
Heights are finite with magnitude at most 100000. The terrain entity's `Transform` places
the local frame; the local origin is sample (0, 0) at height 0.

Cell `(column, row)` spans two samples in each direction and is split along the diagonal
from its `(x0, z1)` corner to its `(x1, z0)` corner (Rapier/Parry's default heightfield
split). With cell fractions `u = (x - x0) / spacing`, `v = (z - z0) / spacing`, the triangle
`(x0,z0) (x0,z1) (x1,z0)` covers `u + v <= 1` and `(x0,z1) (x1,z1) (x1,z0)` covers
`u + v >= 1`. `Heightfield::height_at` (and `normal_at`, `slope_degrees_at`) evaluate this
surface; the CPU chunk meshes, the GPU vertex shader and the physics colliders use the same
split, so sampled, rendered and simulated heights agree. `bilinear_height_at` and
`smooth_normal_at` are smoother alternatives for effects that need no exact contact.

Holes are per cell. Splat weights are per sample: 1..=8 layers, one byte each, summing to
exactly 255; `layer_weight_at(layer, x, z)` interpolates them bilinearly to 0..=1 (weights at
a point sum to 1). Each layer has a unique `name`, an optional `texture` asset id, a linear
RGBA `tint` (0..=1) and `uv_tiling` (world units per texture repeat).

## The `.terrain` asset

Project asset kind `terrain`; `Project::validate` decodes the file. The format is binary,
little-endian, versioned and bounded (64 MiB):

| Field | Size |
| --- | --- |
| magic `ETRN` | 4 |
| `u32` version (1) | 4 |
| `u32` header length `n` (1..=65536) | 4 |
| UTF-8 JSON header `{columns, rows, spacing, layers: [{name, texture?, tint, uv_tiling}], holes}` (unknown fields rejected) | n |
| `f32` heights, row-major | 4 x samples |
| `u8` splat weights, `layers` per sample | layers x samples |
| hole bits when `holes` (cell `i = row * (columns - 1) + column` is bit `i % 8` of byte `i / 8`) | ceil(cells / 8) |
| `u64` FNV-1a 64 checksum of every preceding byte | 8 |

Decoding rejects a wrong magic (`terrain.asset.malformed`), version (`terrain.asset.version`),
header, size limit (`terrain.size`, checked before any allocation), a length that does not
match the header exactly, non-finite or out-of-range heights, unnormalized weights
(`terrain.splat.sum`) and a checksum mismatch (`terrain.asset.checksum`).
`engine_terrain::asset::{encode, decode}` read and write it; `terrain_create` makes one.

## Level of detail

`engine_terrain::lod` is a CDLOD-style quadtree. A level-`L` node covers
`node_resolution << L` sample intervals per side with `node_resolution` x
`node_resolution` cells whose vertices are `2^L` samples apart. Selection walks down from
the top-level roots and subdivides a node of level `L > 0` while its distance to the viewer
is at most `range[L - 1]`, else it emits the node whole.

| `LodSettings` field | Default | Meaning |
| --- | --- | --- |
| `node_resolution` | 32 | cells per node side, power of two 4..=128 |
| `levels` | 0 | levels; 0 picks enough for one root to cover the grid |
| `lod_distance` | 4 | `range[0]` in leaf-node sizes (`node_resolution * spacing`), 3..=1000; `range[L] = range[0] * 2^L` |
| `morph_fraction` | 0.5 | fraction of the admissible band below each range used for morphing, (0, 1] |
| `max_nodes` | 512 | selection budget per view, 1..=4096 |

**Distance.** `d(p) = sqrt(|p.xz - viewer.xz|^2 + gap^2)` where `gap` is how far the viewer
is above the highest (or below the lowest) sample. It depends only on horizontal position,
so every node sharing a vertex computes the same value, and flying high coarsens the whole
terrain.

**Geomorphing.** A vertex of a level-`L` node moves from its sample height `h` toward the
next-coarser surface `hc` by `k = clamp((d - start[L]) / (end[L] - start[L]), 0, 1)`,
`end[L] = range[L]`. `hc` is the coarse triangulation at the vertex: the sample itself on the
coarse grid, the mean of the two coarse neighbours along X or Z at edge midpoints, and the
mean of the coarse diagonal's endpoints `(x - step, z + step)`, `(x + step, z - step)` at
cell centres. Because the fine diagonals subdivide the coarse ones, a node at `k = 1` is
exactly the coarser surface. The top level never morphs.

**Crack-freedom.** If node `A` (level `a`) shares an edge with a coarser node `B`, `B` was not
subdivided, so every point of the edge is farther than `range[a]` and `A` is fully morphed
there; `A`'s parent was subdivided, so the edge is within `range[a] + diag(a+1)`, and the
layout sets `start[a+1] >= range[a] + diag(a+1)` (`diag` = horizontal node diagonal), so `B`
is unmorphed there. The same inequality rules out neighbours more than one level apart, and
needs `lod_distance > 2 * sqrt(2)`, hence the minimum of 3. `A`'s extra edge vertices lie on
`B`'s edge segments (T-junctions on straight edges). `tests/lod_cracks.rs` proves this
numerically on a 257 x 257 fBm terrain (heights within +-20) for 60 random viewers per
setting (`lod_distance` 3, 3.5 and 4; `morph_fraction` 0.5 to 1): every boundary vertex of
every selected node matches the neighbour's edge within **1e-4** (measured maximum
4.8e-7 over 169,666-226,160 comparisons per setting, 22,000-25,500 of them against a coarser
node), adjacent nodes differ by at most one level, and the same boundaries would crack
without morphing (control). Fully-morphed vertices of nodes of every level lie on the
coarser triangulated surface within 1e-4 (measured 9.5e-7 over 10,368 vertices).

**Grid alignment.** Both `columns - 1` and `rows - 1` must be multiples of the top-level
step `2^(levels - 1)`; the effective `levels` is reduced until they are (so use sides like
129, 257 or 513 samples; an odd `columns - 1` allows no LOD). Then every cell of every node
is entirely inside or outside the grid; vertices are clamped to the grid and outside cells
are dropped.

**Budget.** When a selection would exceed `max_nodes`, the finest level allowed is raised
until it fits (`Selection::budget_limited`); this stops subdivision uniformly and keeps the
crack-freedom argument (tested). More top-level roots than `max_nodes` is a settings error.

`build_chunk_mesh` turns a selected node into positions, full-resolution sample normals
(LOD-independent shading), UVs (0..1 across the terrain), splat vertex colours
`sum(weight_i * tint_i)` and indices with hole cells removed. A coarse cell is dropped when
any heightfield cell under it is a hole, so holes never close at a distance (they may look up
to one LOD cell larger).

## Sculpt and paint

Brushes are circles in terrain-local `[x, z]` with `radius` (<= 100000), `strength` and
`falloff` (0..=1: weight 1 inside `radius * (1 - falloff)`, smoothstep to 0 at `radius`).
Every operation visits samples in row-major order with plain `f32` arithmetic
(deterministic) and returns an `EditReport { region, changed }` with the half-open
rectangle it changed:

| Operation | Effect at a sample of weight `w` |
| --- | --- |
| `SculptOp::Raise` / `Lower` | `h += / -= strength * w` (strength in local units) |
| `Smooth` | toward the 3x3 mean of the original heights by `strength * w` (strength 0..=1) |
| `Flatten { height }` | toward `height` (default: the surface under the centre) by `strength * w` |
| `SetHeight { height }` | toward `height` by `w` |
| `Noise { seed, frequency }` | `h += strength * w * fbm(x * frequency, z * frequency, seed)` |
| `paint_layer(layer)` | the layer's weight moves toward 1 by `strength * w`, the others shrink proportionally, re-quantized to bytes summing to 255 |
| `paint_holes(hole)` | cells whose centre is within `radius` become holes (or are filled); region in cells |

## `engine.Terrain`

Requires `Transform`; editable with `component_set` (schema-validated, unknown fields
rejected). Scene validation requires the asset to exist with kind `terrain`.

| Field | Default | Meaning |
| --- | --- | --- |
| `asset` | `""` (required) | `terrain` asset id |
| `lod` | see above | `LodSettings` |
| `update_distance` | 0 | CPU fallback: viewer movement (local units) before reselection; 0 = one sample spacing |
| `materials` | `[]` | per-layer overrides `{texture?, tint?, uv_tiling?}`, index-aligned with the asset layers, at most 8 |
| `collision` | enabled, layer 1, mask all, `tile_cells` 128, friction 0.8, restitution 0 | generated colliders |
| `render_mode` | `auto` | `auto`, `cpu` or `gpu` (see below) |
| `visible` | true | drawing only; collision and queries are unaffected |
| `surface` | `"terrain"` | `PcgSurfaceRegistry` name; empty disables registration |

The runtime service (`Terrains` resource) works in the world hooks:

- `pre_simulation` (`sync_terrains`): loads changed assets, rebuilds the collider tiles an
  edit touched and re-registers PCG surfaces. Scripts' edits of a tick reach physics in the
  same step.
- `post_transforms` (`update_terrain_render`): picks each terrain's render path, rebuilds CPU
  chunks for the active `Camera3D` and publishes GPU-path terrains in `TerrainRenderSet`.

At most 16 terrain entities, 1024 collider tiles each. Generated entities carry
`TerrainGenerated { owner }` and authored ids `terrain:<owner id>/chunk/<level>/<x>,<z>` and
`terrain:<owner id>/collider/<x>,<z>`; they are children (`Parent`) of the terrain entity and
are removed with it.

### Render path rule (no double draw)

A terrain draws through the GPU feature when `render_mode` is `gpu`, or `auto` while the
`TerrainGpuRenderer` marker resource exists in the simulation world; `engine_world_render::install`
inserts it. Otherwise it spawns CPU fallback chunk entities (`MeshRenderer` + `GeometryOverride`,
one per selected node, terrain-local geometry under the terrain transform). A GPU-path terrain
has no chunk entities and a CPU-path terrain is absent from `TerrainRenderSet`, which is all the
GPU feature draws, so a terrain is never drawn twice. A `gpu` terrain without the renderer draws
nothing and its status says so. The marker is used instead of looking for a registered
`world.terrain` feature because the desktop runner moves `PipelineFeatures` out of the world
into the renderer at window creation.

CPU path details: selection and morph factors use the active `Camera3D` (lowest authored id
among active cameras) in terrain-local space, or, without a camera, a viewer far above the
centre (top-level nodes only). Chunks are rebuilt when they are new, when an edit touched them,
or when their morph state is partial or changed; selection and morphs are recomputed together
when the viewer moved `update_distance` or the heightfield changed, so the meshes on screen are
always crack-free for one viewer position.

`terrain::status(world, entity)` reports samples, revision, levels, `render_path`
(`cpu-mesh-chunks`, `gpu-feature`, `hidden`), selected nodes and per-level counts,
`budget_limited`, chunk entities, vertices, triangles, heightfield and mesh collider counts,
the registered surface and notes.

### Physics

The grid is cut into tiles of `collision.tile_cells` cells (8..=255). Each tile is a child
entity with a fixed `engine.PhysicsBody3D` (engine-physics `Shape::HeightField`, rows along Z,
centred on the tile centre, internal edges fixed) and the collision layer, mask, friction and
restitution. Hole cells are listed in the tile's `heightfield_holes`, so engine-physics removes
both triangles of each (Parry cell status) and bodies fall through exactly those cells. Tiles
whose cells are all holes have no collider. Edits rebuild only
the tiles intersecting the edit region (widened by one sample).

Measured (`crates/engine-project/tests/terrain.rs`): a dynamic 0.5 m ball dropped on a flat
plateau (height 3) of a 257 x 257 hilly terrain placed at y = 1 comes to rest at y = 4.5
within **0.05 m** after 150 steps of 0.02 s; a ball over an 8 x 8 cell hole falls through
(below y = -5); 15 heightfield tiles and 1 mesh tile for 64-cell tiles.

### PCG surfaces

Each terrain registers a `TerrainSurface` (an `engine_pcg::Surface`) in `PcgSurfaceRegistry`
under `surface`: world-space `height` (the triangulated surface), `normal` (smooth sample
normals), `layer_weight` by layer name (so foliage density maps and PCG `surface_field`
`layer` read the splat map) and world bounds. The first terrain (by authored id) claims a name;
later ones get a status note. When a surface is (re-)registered (first sync, an edit, a moved
terrain), the terrain bumps `regenerate` of every `engine.PcgVolume` using that name, so
generated content follows the ground. The world hooks run PCG before terrain in
`pre_simulation`, so on the first tick a volume referencing a terrain surface fails once
("no surface") and regenerates on the next tick; running terrain before PCG would remove that
tick (a `world/mod.rs` ordering change, owned by the world module).

### Navigation

The navigation baker keeps a raster cell only when one plane covers it completely, so the
raw terrain triangles (two planes per cell) would bake into islands. Terrain navigation input
is therefore a raster-aligned walkable soup: one planar quad per bake cell (corners exactly
`cell * cell_size` in world space), through the surface height at the cell centre with the mean
slope across the cell; neighbouring quads meet within the curvature over one cell, which
`max_step` bridges. Cells overlapping a hole and cells steeper than `max_slope_degrees` are left
out. At most 16384 cells (the baker's 32768-triangle input limit): use a world-space `region` or
a larger `cell_size` for big terrains.

- Native: `engine_terrain::navigation_soup`, `world::terrain::navigation_document(field,
  options, settings, obstacles)` and, for a placed entity (translation-only transform),
  `world::terrain::navigation_input(world, entity, settings, region)`.
- MCP: `terrain_navmesh` writes a `navigation` document that `navigation_query` and
  `engine.NavigationAgent` use like a `navigation_bake` result.

Tested: on a 65 x 33 slope a path from x = 4 to x = 60 is found; a hole strip across the
terrain, or a 6 m ridge (about 80 degrees), makes the same query `Unreachable`.

## GPU rendering (`engine-world-render`)

`world.terrain` (`Opaque` stage, after `engine.opaque`) draws every terrain of
`TerrainRenderSet` from the extracted render world:

- per view, the CPU selects nodes with the same `LodLayout` as the fallback (viewer = view
  camera in terrain-local space), frustum-culls them (node bounding spheres) and pushes one
  instance per node (`TerrainInstance`: origin, step, level, morph range, viewer gap and
  position) into an `InstanceBatcher`: **one instanced draw call per terrain** over a shared
  `node_resolution` patch mesh;
- the vertex shader reads heights from an `R32Float` texture with `textureLoad` and
  geomorphs exactly like `engine_terrain::lod::morphed_height`, with full-resolution sample
  normals;
- the fragment shader (`#import engine::lighting`) blends up to 8 splat layers from two
  `Rgba8Unorm` weight textures (bilinear): `tint x` the layer's albedo (a 256 x 256 mipmapped
  `Rgba8UnormSrgb` array layer, nearest-resampled from the texture asset, white without one)
  tiled every `uv_tiling` local units, lights it with `engine_direct_lighting` +
  `engine_indirect_lighting` (roughness 0.9, receives shadows) and discards hole cells
  (an `R8Unorm` cell texture);
- depth is written in the scene pass (`LessEqual`). `world.terrain.normals` (`Prepass`, after
  `engine.prepass.normals`) draws the same surface into the normals target and scene depth
  when a running feature needs normals.
- **Shadows.** `world.terrain.shadow` (`Shadow`, after `engine.shadows`) renders terrain depth
  into a per-view terrain shadow map: a `Depth32Float` array of `SHADOW_MAP_SIZE` (1024)
  texels per edge with one layer per cascade (`MAX_CASCADES` = 4 layers, 16 MiB per view),
  using the engine's own cascade matrices of the first directional light
  (`engine_shadows[shadow_index + cascade]`; cascades the light does not have are clipped in
  the vertex shader). Every camera-selected node is drawn without frustum culling, holes cast
  nothing. The colour pass samples the map with the engine's cascade selection and blend and
  the light's depth / normal bias (in this map's texels), 3 x 3 PCF, and multiplies the result
  into the first directional light's visibility next to the engine shadow atlas, so terrain
  shadows terrain and engine meshes (boxes, characters) still shadow terrain through the atlas.
- **Motion vectors.** `world.terrain.motion` (`Opaque`, after `world.terrain`) runs when a
  feature or the host needs motion (DLSS requests it): it redraws the colour pass's nodes into
  the motion target (current-to-previous, render pixels, y down, like
  `engine.prepass.motion`) with a `LessEqual` test against the depth the colour pass wrote
  (`@invariant` positions, no depth write), so it covers exactly the visible terrain. The
  previous position replays the geomorph with the view's previous terrain-local viewer
  (`TerrainInstance::previous_viewer`) and the previous terrain transform under the previous
  view-projection, so camera motion, terrain transform changes (for example origin shifts) and
  geomorph changes are all reported; the per-view history advances in the colour pass and
  resets with `ViewData::reset_history`. It runs in `Opaque` rather than `Prepass` because the
  engine's motion prepass depth is private: a prepass terrain pass could not depth-test against
  engine meshes.

Textures are re-uploaded whole when a terrain's heightfield revision or layers change.

### Enabling

```rust
// In the game's configure(app) or before the desktop runner starts:
let status = engine_world_render::enable(app)?; // install + register into PipelineFeatures
```

`enable` = `install(app)` (extraction system + `TerrainGpuRenderer` marker) +
`register(&mut PipelineFeatures)` (merged into the world's resource, created if missing).
Games that build `PipelineFeatures` themselves call both separately. The desktop runner
hands the world's `PipelineFeatures` to `GpuRenderer::set_features` and supplies the render
world to features. **The desktop host enables the world features by default**
(`engine_desktop::install_default_render_features`, called by the windowed runner and the
screenshot path before the runtime starts: `engine_world_render::install_missing` +
`register_missing`, which skip any module the game installed or registered itself), so an
authored terrain renders through the GPU feature without game code; headless runs only list the
features in the preview status and keep the CPU fallback. `Camera3D.render_features` can
disable `world.terrain` (or `world.terrain.shadow`, `world.terrain.motion`) per camera.
`TerrainRenderStatusHandle::get()` reports views, terrains, nodes, culled nodes, draw calls,
uploads, whether the normals, shadow (and its draw calls) and motion passes ran, whether the
colour pass used the terrain shadow map, and notes (for example a missing `TerrainRenderSet`
when `install` was not called).

Verified on this machine (RTX 5080, Vulkan,
`cargo test -p engine-world-render --test gpu_terrain -- --ignored --test-threads=1`):

- a 65 x 65 ridge terrain renders offscreen at 160 x 120: sky rows stay background, 26 % of
  the image is terrain, the light-facing slope is 2.33x brighter (linear luminance) than the
  opposite slope, an 8 x 8 cell hole shows the background next to covered terrain, 10 nodes
  in 1 instanced call per pass (colour + normals: 2 calls, 20 instances);
- GPU geomorphing matches the CPU chunk meshes: a 129 x 129 fBm terrain seen from a low
  camera (82 nodes over three levels, 1700 vertices mid-morph) rendered by the
  feature and by the CPU chunk meshes as unlit scene meshes gives **0** coverage
  mismatches over 19200 pixels (10175 terrain pixels).
- terrain shadows: a 12 m ridge on flat ground under a shadowed sun 31 degrees up shadows the
  ground 10 m behind it down to the ambient term (linear luminance 0.040, expected 0.04;
  shadowed / lit 0.235), ground 28 m behind the crest is lit again (1.00 of the lit
  reference), the shadow pass makes 4 draw calls (one per layer) and disabling
  `world.terrain.shadow` for the view brings the ratio back to 1.00;
- an engine box mesh (6 m cube, light at 45 degrees) shadows the terrain through the engine
  atlas: ambient-only luminance 0.040 in its shadow, shadowed / lit 0.183;
- motion vectors with `host_needs.motion` (as DLSS asks): a 2 m camera pan gives
  4.31..5.34 px of motion at three terrain points, worst error 0.023 px against the
  analytic reprojection; a 1.5 m terrain move under a static camera gives 2.69 px against
  2.66 px expected; sky pixels stay 0 and with `world.terrain.motion` disabled terrain
  pixels stay 0.

### Limits

- Terrain shadows reach only the terrain's own colour pass: features cannot render into the
  engine shadow atlas (`FeatureContext` exposes neither the atlas layer views nor the per-light
  shadow draw rectangles; both are crate-private in `engine-render-pipeline`), so engine meshes,
  foliage and water do not receive terrain shadows. Only the first directional light's
  cascades are covered; local lights get no terrain shadow.
- All four cascade layers are cleared and drawn every frame (the cascade count is only on the
  GPU), each with every camera-selected node: no per-cascade culling.
- Motion of a node that changed LOD level between frames uses the current node's morph with
  the previous viewer (the previous selection is not kept); popping-free by construction of the
  geomorph, but not an exact history.
- Back faces are culled (terrain is seen from above; mirrored transforms are not supported).
- One albedo texture size (256) per layer; no normal maps, triplanar mapping or height
  blending between layers.
- Holes: per-fragment discard on the GPU, per-LOD-cell removal on the CPU fallback.
- The LOD distance ignores terrain transform scale (ranges are in local units).
- Virtualized geometry is out of scope.

## MCP tools

| Tool | Access | Effect |
| --- | --- | --- |
| `terrain_create` | write | new `.terrain` file: size, spacing, base height or seeded fBm, 1..=8 layers; returns the asset entry and an `engine.Terrain` snippet; never overwrites |
| `terrain_sculpt` | write | up to 256 ordered brushes (`raise`, `lower`, `smooth`, `flatten`, `set_height`, `noise`), all or nothing, optional `expected_hash`, atomic rewrite; returns changed samples and region |
| `terrain_paint` | write | up to 256 `paint` / `add_layer` / `set_layer` edits |
| `terrain_holes` | write | up to 256 hole edits `{center, radius, hole}` |
| `terrain_navmesh` | write | bakes a new `navigation` JSON document from a terrain at an `origin`, with optional `region` and bake `settings` |
| `terrain_info` | read | summary (samples, spacing, extent, height range, layers, holes, content hash) and up to 256 samples (height, normal, slope, hole, layer weights) |

Tools accept an asset id or a project-relative `.terrain` path, validate every input and
never execute game code. `asset_info` reports terrain samples, spacing, height range, layers
and holes.

## Verification

- `cargo test -p engine-terrain` (heightfield sampling and diagonal, brushes, asset
  round-trip and rejection, collision tiles, navigation soup, LOD selection;
  `tests/lod_cracks.rs` for crack-freedom and morph-to-coarse).
- `cargo test -p engine-project --test terrain` (validation, CPU chunks following the camera,
  GPU-renderer takeover, ball rest and hole fall-through, edits rebuilding chunks and
  colliders, PCG surface and regeneration, navigation).
- `cargo test -p engine-mcp --test terrain` (tools, bounds, navigation, `component_set`).
- `cargo test -p engine-world-render` (extraction, no double draw, registration) and
  `-- --ignored --test-threads=1` for the GPU tests.
