# Foliage and vegetation

Foliage is a data model plus a runtime service and a GPU feature:

| Part | Where | What |
| --- | --- | --- |
| Data model | [`engine-foliage`](../crates/engine-foliage/README.md) | foliage types, spatial-cell instance storage, deterministic scatter over a PCG `Surface`, brush painting, LOD bands with hysteresis, the wind model, the `.foliage` file |
| Scene integration | `engine_project::world::foliage` (`crates/engine-project/src/world/foliage/`) | `engine.Foliage`, ground resolution (terrain entity or PCG surface), regeneration and re-projection, CPU merged-cluster fallback, the GPU render set, the native painting API |
| GPU rendering | [`engine-world-render`](../crates/engine-world-render/README.md) | the `world.foliage` render feature |
| Authoring | `crates/engine-mcp/src/tools/foliage.rs` | `foliage_scatter`, `foliage_paint`, `foliage_info` |

## Types

A `FoliageType` (JSON in `engine.Foliage.types` and in the `.foliage` header, unknown fields
rejected, at most 32 per set):

| Field | Default | Meaning |
| --- | --- | --- |
| `name` | required | unique, `[A-Za-z0-9_.-]{1,64}`; assets and exclusions refer to it |
| `mesh`, `lod_meshes` | required / `[]` | mesh asset ids of LOD 0 and up to 4 lower-detail meshes |
| `texture`, `tint` | none / white | albedo texture asset and a linear RGBA multiplier |
| `impostor` | none | `{texture, size:[w, h]}`: a camera-facing billboard drawn beyond the last mesh |
| `density` | 1 | instances per square metre before the density map (0..=64) |
| `density_map` | uniform | `uniform`; `layer` (`layer`, `invert`, `threshold`: a terrain paint layer through `Surface::layer_weight`); `grid` (explicit `origin`, `cell_size`, `columns`, `rows`, `values` 0..1, bilinear between cell centres, 0 outside); `texture` (`asset`, `origin`, `size`: a texture's red channel, resolved into a grid by the runtime) |
| `scale`, `yaw_degrees`, `tilt_degrees` | [1,1], [0,360], 0 | uniform scale range, yaw range, largest random lean |
| `slope_degrees`, `height`, `altitude` | [0,90], +-1e5, +-1e5 | placement limits on the ground under the candidate: slope (angle between the normal and up), ground height (world Y) and ground height above the floor of the placement box (`bounds.min.y`) |
| `align_to_normal`, `ground_offset` | 0, 0 | 0 upright .. 1 up axis onto the ground normal; vertical offset (negative sinks) |
| `min_spacing` | 0 | minimum horizontal distance between instances of the type |
| `excludes` | `[]` | `[{name, radius}]`: keep clear of an earlier type (a type may only name types listed before it) |
| `lod_distances`, `impostor_distance`, `cull_distance` | `[]`, none, set default | LOD `i` hands over to `i+1` at `lod_distances[i]` (one per `lod_meshes` entry, increasing); the impostor replaces the last mesh beyond `impostor_distance` (needs `impostor`); nothing is drawn beyond `cull_distance` (else `culling.max_distance`) |
| `wind_weight` | 1 | how strongly wind bends the type (0 = rigid) |

## Placement

`engine_foliage::scatter(types, &dyn Surface, &ScatterParams)` is a pure function of its
inputs. Per type, in list order:

1. **Candidates.** The plane is cut into a fixed world grid (in **absolute** coordinates,
   `WorldOrigin.offset + local` in f64, like PCG's noise domain) whose cell is the power of two
   giving about two candidates per cell for the type's density. Each cell seeds a generator
   from `(seed, cell, type)`, draws `floor(density * area)` candidates plus one more with the
   fractional probability, and draws every attribute (position, density roll, scale, yaw,
   tilt, priority, wind phase) in a fixed order whether or not the candidate survives.
   Consequently a region's candidates do not depend on other regions (painting a brush draws
   the candidates a full scatter would have), and relaxing one limit never reshuffles the
   survivors of another. Because cells are absolute, the same seed gives the same forest at
   any origin offset and after any streamed reload (`ScatterParams::origin` /
   `PaintContext::origin`; authored files and MCP tools use origin 0). The expected candidate
   count (`area x density`) is checked **before** anything is drawn against
   `16 x max_instances` (never above 2 000 000): beyond it the call fails with
   `foliage.limit.candidates` instead of materialising millions of candidates (500 x 500 m at
   density 64 would be 16 million, over 500 MB); lower the density, shrink the area or raise
   `max_instances`. Scatter, native paint and the MCP `foliage_scatter` / `foliage_paint`
   tools share this check.
2. **Filters**, in order: placement box and surface bounds, exclusion zones, density map
   (accepted when the density roll is below the map value), terrain holes (a `HoleMask`),
   ground height, altitude, slope, and the exclusion radii of earlier types. The limits are
   tested on the surface's own answers at the candidate, so they hold exactly.
3. **Thinning.** Survivors are accepted greedily in seeded priority order if no accepted
   instance (or existing instance of the type) lies within `min_spacing`.

Exceeding `max_instances` (cap 262 144) is an error, never a truncation. Instances store
position, rotation quaternion, scale and wind phase (`FoliageField`, cells of `cell_size`
metres, per-cell revisions; every new field starts in its own block of revisions so a
regenerated field cannot be mistaken for the one it replaced).

Measured by `cargo test -p engine-foliage` (`tests/behaviour.rs`), tolerance 5 sigma plus 1 %
of the expected count:

- density 0.05, 0.5, 2, 16 and 64 per m2 over areas giving 8 000 to 25 600 expected
  instances: all within tolerance; a two-level density grid gives its mean (0.625);
- slope (25..45 degrees), height (20..50) and altitude (22..48) limits hold for every
  instance on rolling ground, and instances sit on the ground to 1e-5;
- identical inputs give identical output, a new seed a different one;
- `min_spacing` 1.5: closest pair >= 1.5 and the packing fraction 0.33..0.56 (random
  sequential packing jams near 0.547);
- exclusion between types, zones for named types, holes and surface bounds, scale / yaw /
  phase ranges and alignment to the normal (up axis dot normal within 1e-3).

## Painting

`engine_foliage::paint(field, types, ctx, brush, op, seed)`. A `Brush` is a circle
(`center` [x, z], `radius` <= 10 000, `strength` and `falloff` in 0..1; weight 1 inside
`radius * (1 - falloff)`, smoothstep to 0). `PaintOp::Add { type_index }` places the type's
candidates in the brush with the same pipeline as scatter, their acceptance scaled by weight
times strength, and a minimum separation of `max(min_spacing, 1 mm)` against everything
already placed: the same stroke with the same seed is idempotent, and a stroke with the
scatter seed re-draws the generated candidates. `PaintOp::Erase { type_index }` removes the
instances whose weight times strength exceeds a per-instance hash (strength 1 clears the
inner disc; strength 0.5 removes about half, measured 0.5 +- 0.06).

## Level of detail, impostors, culling

`LodBands::for_type(type, default_cull, scale, hysteresis)`: mesh levels, then the impostor,
then culled, separated by `lod_distances`, `impostor_distance` and the cull distance (all
times `culling.lod_scale`). With hysteresis `h` an instance leaves a level outward past
`edge * (1 + h)` and re-enters it inward below `edge * (1 - h)`; a `LodTracker` keeps each
instance's last level per cell (reset when the cell changes). Tested: all band edges, both
directions, multi-band jumps, and an instance wobbling +-0.8 m around an edge flips 199 times
in 200 frames without hysteresis and never with 10 %.

## Wind

`WindParams` (`direction_degrees`, `strength` in metres of lean at bend 1, `sway_frequency`,
`gust_frequency`, `gust_strength`, `flutter`, `wavelength`). `sway_offset(time, position,
phase, bend)` is the horizontal offset of a vertex (formula in the module documentation):
`bend * amp * (dir * (0.6 + 0.4 * w1) + perp * 0.5 * flutter * w2)`. It is horizontal, at
most `max_offset()` long, zero at bend 0, linear in the bend, differs between neighbours by
their phase and is periodic with period `1 / sway_frequency` (with gusts, the common period).
The bend of a vertex is `clamp((y - base) / height, 0, 1)^2 * wind_weight` over the LOD 0
mesh. `foliage.wgsl` repeats the expression term for term from `WindParams::gpu_block`.

## `engine.Foliage`

Requires `Transform`; editable with `component_set` (closed schema, unknown fields rejected,
scene validation checks every referenced asset).

| Field | Default | Meaning |
| --- | --- | --- |
| `types` | `[]` | the types; in `asset` mode entries replace the asset's type of the same name |
| `terrain` | `""` | authored id of an `engine.Terrain` entity to grow on (live heightfield, holes, paint layers, follows edits); wins over `surface` |
| `surface` | `""` | `PcgSurfaceRegistry` name when `terrain` is empty; empty = flat ground at y = 0. Holes are not visible through a registry surface |
| `seed` | 0 | scatter seed |
| `mode` | `generated` | `generated` scatters `types`; `asset` loads the `instances` asset |
| `instances` | `""` | `foliage` asset id (asset mode) |
| `center`, `size` | [0,128,0], [64,256,64] | placement box relative to the entity's world position at generation (rotation and scale ignored); `center.y - size.y / 2` is the altitude floor |
| `exclusion_zones` | `[]` | `{shape: circle, center, radius, types?}` / `{shape: rect, min, max, types?}` |
| `cell_size`, `max_instances` | 32, 65 536 | storage cell (asset mode uses the asset's), instance cap (<= 262 144) |
| `regenerate` | 0 | bump to regenerate with unchanged values |
| `culling` | 200, 1, 0.1 | `max_distance` (default cull), `lod_scale`, `hysteresis` |
| `wind` | strength 0 | `WindParams` |
| `render_mode` | `auto` | `auto`, `cpu` or `gpu` |
| `cpu_vertex_budget` | 400 000 | CPU fallback vertex budget |
| `visible` | true | drawing only |

The service (`Foliages` resource) works in the world hooks:

- `pre_simulation` (`sync_foliage`, after terrain and PCG): resolves the ground and compares the
  generation inputs (types, ground target, seed, mode, box, zones, cell size, cap, `regenerate`)
  and the ground's identity with those of the last attempt. A change in either regenerates
  (generated mode) or reloads / re-projects (asset mode). The entity's own position is read at
  generation time and is not tracked. A failed attempt (bad ground, missing asset, cap
  exceeded) keeps the previous output, logs `foliage <id>: ...` and records the diagnostic in
  the status; it retries when an input or the ground changes.
- `post_transforms` (`update_foliage_render`): picks the render path and builds CPU clusters
  for the active `Camera3D`, or publishes the `FoliageRenderSet`.

At most 16 foliage entities; one more is skipped with a reason (`foliage::failure`, logged
once) and never fails the tick. A foliage entity without an authored id registered in the
entity lookup is skipped the same way. Generated entities carry `FoliageGenerated { owner }` and ids
`foliage:<owner id>/cell/<x>,<z>/<type name>`.

### Following the terrain

The ground of a `terrain` target is rebuilt every tick from the live heightfield; its
identity is the heightfield revision and the terrain's world matrix. `engine-project`'s
terrain keeps its dirty region private, so foliage does not receive regions: **any** edit
(`terrain::sculpt`, `paint_layer`, `paint_holes`, a moved terrain) changes the revision and
- in `generated` mode regenerates the whole set deterministically against the new ground (so
  slope, height, layer and hole filters apply to the new shape, tested after a raised hill);
- in `asset` mode re-projects every instance: height = surface height + `ground_offset`,
  instances over holes removed, instances outside the terrain left unchanged. Rotation is
  not recomputed. Tested: after a sculpt every instance sits on the new height to 1e-3.

Painted (native `paint`) instances live in the field until the next regeneration; export them
with `export_asset` and use `asset` mode to keep hand-painted content.

### Origin shifts

Instance positions live in the local frame of `WorldOrigin` (asset files hold absolute
positions; `export_asset` writes absolute positions). After an origin shift the service
translates the field into the new frame (f64, rounded once), drops the CPU cluster entities
the coordinates service had already shifted and rebuilds them from the translated field, and
does not count the terrain's moved matrix as a ground change, so a rebase never regenerates
foliage (tested: same 512 instances within f32 rounding, attempts unchanged). Generated
placement hashes cells in absolute coordinates, so regenerating (an edited component, a
streamed reload) under a different origin yields the same absolute forest (tested: two
origins, every instance of one within 1 mm of an instance of the other). Asset mode loads
absolute positions, re-expresses them into the current frame **before** projecting them onto
the (local-frame) ground, so a foliage asset loaded at a non-zero offset keeps every instance
and its height (tested at offset 2048 on a sloped terrain, heights within 1e-3).

### Render path rule (no double draw)

Foliage draws through the GPU feature when `render_mode` is `gpu`, or `auto` while the
`FoliageGpuRenderer` marker resource exists (`engine_world_render::install` inserts it);
otherwise through CPU cluster entities. A GPU-path foliage has no cluster entities and a
CPU-path one is absent from `FoliageRenderSet` (tested both ways). `gpu` without the renderer
draws nothing and the status says so. `foliage::status` reports instances per type, render
path, cluster counts per level, vertices, budget limiting, attempts, scatter rejection counts,
the ground and diagnostics.

### CPU fallback

Per (cell, type) one `MeshRenderer` + `GeometryOverride` entity in world space holds the
merged, transformed geometry of the cell's instances of that type at one level. The level
comes from the distance of the camera to the cell's bounding box through the type's
`LodBands` (hysteresis per cell and type); beyond the last mesh band an impostor is two
crossed double-sided quads with the impostor texture and `transparent: true`. The total is
bounded by `cpu_vertex_budget`: clusters are admitted nearest first and a cluster that does
not fit keeps as many whole instances as do (`budget_limited`). The meshes are static: **the
CPU path does not sway in the wind**, and every instance of a type in a cell shares one
level. Tested on an authored terrain scene through `extract_scene_frame`: geometry counts
change with camera distance (full detail, LOD 1, impostors at 120 m, none at 600 m), no
vertex lies in a terrain hole or on the steep band, the budget is respected exactly.

## GPU rendering (`engine-world-render`)

`world.foliage` (`Opaque` stage, after `engine.opaque`). Per view and set, the CPU rejects
cells by distance to the cell box and frustum, then each instance: level from `LodBands` with
hysteresis kept per view, frustum test of a sphere around the origin with the type's radius.
Survivors go into one `InstanceBatcher` with key (set, type, impostor bit): **one instanced
draw per (type, level)**, with a per-instance vertex buffer of 48 bytes (rotation, position
and phase, scale). The vertex shaders rotate / scale / sway the mesh vertices
(`vs_mesh`) or build a camera-facing card (`vs_billboard`); `fs_main` samples the albedo with
alpha cutout at 0.5 and lights it two-sided with `engine_direct_lighting` and
`engine_indirect_lighting`.

```rust
let status = engine_world_render::register_all(&mut features)?; // terrain + foliage
engine_world_render::install(app); // extraction + both GPU markers
// or enable(app)? for both parts; status.foliage.get() reports the last view.
```

The desktop host registers and installs it by default (`engine_desktop::install_default_render_features`);
`Camera3D.render_features` can disable `world.foliage` per camera.

Verified on this machine (RTX 5080, Vulkan,
`cargo test -p engine-world-render --test gpu_foliage -- --ignored --test-threads=1`):

- a 160 x 120 offscreen field of 19 visible instances in 4 batches (post LOD 0, post LOD 1,
  post impostor, bush) renders with **4 draw calls**, equal to the batches; row colours are
  LOD 0 green, LOD 1 green, impostor red, bush blue, a row beyond the cull distance leaves
  its pixels empty (5 instances culled by distance, 1 cell culled by the frustum), 2158
  covered pixels;
- wind: with strength 0 two different times give identical pixels; with wind 288 pixels
  change between the two times, the blade base moves 0.0 px (CPU model 0.05) and its upper
  part 10.5 px against 9.76 px predicted by `WindParams::sway_offset` (tolerance 1.1 px).

### Limits

- Opaque stage only. Foliage casts no shadows (it receives the engine atlas's): a feature
  cannot render casters into the engine shadow atlas, because `FeatureContext` exposes
  neither the atlas layer views nor the per-light shadow draw rectangles (`Atlas` /
  `ShadowDraw` / `ViewLighting::atlas` are crate-private in
  `engine-render-pipeline/src/lighting.rs`, and the `engine.shadows` node in
  `pipeline.rs::run_builtin` clears each atlas page itself). Terrain works around this with its
  own map, which only the terrain colour pass samples (see
  [terrain.md](terrain.md#gpu-rendering-engine-world-render)); foliage neither casts into it nor
  receives it. Foliage writes no normals or motion vectors (DLSS and TAA see it as static).
- Culling is on the CPU per view over every instance of the visible cells (up to 262 144
  instances per field). No GPU culling, no Hi-Z occlusion.
- The GPU path does not blend LOD levels (no cross-fade); impostors are a single card, not a
  multi-view atlas.
- Wind is horizontal only (no length compensation); the bend factor assumes the mesh stands
  on y = 0 upward in local space.
- Meshes without normals get smooth vertex normals on upload (the CPU path leaves them to the
  mesh renderer's flat normals).

## MCP tools

| Tool | Access | Effect |
| --- | --- | --- |
| `foliage_scatter` | write | new `.foliage` file: `types`, `seed`, placement box, `cell_size`, `ground` (`{height}` or `{terrain, origin}`: honours holes and layers), `zones`, `max_instances`; never overwrites; returns per-type counts and rejection reasons, hash, the asset entry and an `engine.Foliage` snippet (asset mode) |
| `foliage_paint` | write | up to 256 ordered `add` / `erase` strokes on an existing asset, all or nothing, optional `expected_hash`, atomic rewrite, deterministic seeds |
| `foliage_info` | read | types, counts, cell statistics, bounds, density and, with `area`, the instances inside a circle |

Tools accept an asset id or a project-relative `.foliage` path, validate every input (the
type schema is the component's) and never execute game code.

## Verification

- `cargo test -p engine-foliage` (unit tests per module, `tests/behaviour.rs`).
- `cargo test -p engine-project --test foliage` (validation, slope / hole / ground limits,
  RenderFrame geometry and camera-distance counts, determinism and regeneration, layer
  density and exclusions, terrain edits, asset export / reload / re-projection, the GPU
  marker and render set, the vertex budget, missing-ground diagnostics, registry surfaces).
- `cargo test -p engine-mcp --test foliage` (tools, bounds, atomicity, determinism,
  `component_set` editability and rejection).
- `cargo test -p engine-world-render` and `--test gpu_foliage -- --ignored --test-threads=1`.
