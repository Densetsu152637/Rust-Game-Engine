# 2D tilemaps

Tilemaps are authored data (a tileset and a map) plus a runtime service. The data model
lives in the renderer-independent crate [`engine-tilemap`](../crates/engine-tilemap/README.md).
The `engine.Tilemap` component, the generated render entities, the generated 2D
colliders and the native painting API live in
`engine_project::world::tilemap` (`crates/engine-project/src/world/tilemap/`). MCP
tools author and inspect the files (`crates/engine-mcp/src/tools/tilemap.rs`).

Grid model: square cells, `i32` cell coordinates, x right, y up. Cell `(x, y)` covers
`[x, x+1) x [y, y+1)` cell units; multiplied by the component's `cell_size` it covers
`[x*w, (x+1)*w) x [y*h, (y+1)*h)` in the tilemap entity's local space. Isometric and
hexagonal grids are not supported (deferred): the data model, chunking, rule matching
and collision merging all assume an orthogonal grid.

## Assets

Two new project asset kinds:

- `tileset`: a JSON tileset (`Tileset`). `Project::validate` loads it, checks that its
  texture is a project `texture` asset and that every atlas rectangle fits the texture.
- `tilemap`: a JSON map (`Tilemap`). `Project::validate` parses and validates it
  against the format limits (tile ids are checked against the tileset when an entity
  uses it).

### Tileset format (`version` 1, at most 16 MiB)

```json
{
  "version": 1,
  "name": "dungeon",
  "texture": "atlas",
  "tile_size": [16, 16],
  "tiles": [
    {"id": 1, "name": "floor", "source": [0, 0, 16, 16]},
    {"id": 2, "name": "wall", "source": [16, 0, 16, 16], "collision": "full",
     "properties": {"solid": true}},
    {"id": 3, "source": [32, 0, 16, 16],
     "collision": {"rect": {"min": [0.0, 0.0], "max": [1.0, 0.25]}}},
    {"id": 4, "source": [48, 0, 16, 16],
     "collision": {"polygon": {"points": [[0, 0], [1, 0], [0, 0.5]]}}}
  ],
  "rule_tiles": [
    {"id": 100, "name": "path", "default_tile": 5,
     "rules": [{"neighbours": ["any", "this", "any", "not_this", "not_this", "any", "not_this", "any"],
                "output": 6, "transform": "rotated"}]}
  ]
}
```

- Tile ids are non-zero and below 2^24 (`0` is the empty cell). Concrete tiles and rule
  tiles share one id space. Limits: 4096 tiles, 256 rule tiles, 256 rules per rule
  tile, 64 properties per tile (key at most 64 bytes, text at most 256 bytes).
- `source` is `[x, y, width, height]` in atlas pixels with a top-left origin.
- `collision` is `"none"` (default), `"full"`, `{"rect": {...}}` or `{"polygon": {...}}`.
  Rectangle and polygon coordinates are fractions of the cell (bottom-left origin, y up)
  within `0..=1`; polygons have 3-64 points.
- `properties` values are booleans, integers, numbers or text.

### Tilemap format (`version` 1, at most 16 MiB)

```json
{"version": 1,
 "bounds": {"min": [-512, -512], "max": [512, 512]},
 "layers": [{"name": "ground", "z_order": 0, "visible": true, "opacity": 1.0,
             "chunks": [{"x": 0, "y": 0, "tiles": [0, 0, 1, 1, "... 256 entries ..."]}]}]}
```

A chunk is 16x16 cells; `tiles` has exactly 256 entries, row-major from the chunk's
bottom-left cell. Each entry is `tile | (flags << 24)`; `0` is empty. Chunks are sparse
(only non-empty chunks are stored). Only the *painted* (logical) tile is stored; rule
tiles resolve when the map is loaded. Limits: 32 layers, 4096 non-empty chunks per
layer, at most 65,536 cells per bounds edge, all coordinates within +-1,000,000, unique
layer names of 1-64 characters, opacity within `0..=1`. Unknown fields are rejected.
The bounds are fixed at creation; editing outside them fails with `tilemap.bounds`.

### Orientation flags

Cells carry orientation flags `FLIP_X = 1`, `FLIP_Y = 2`, `TRANSPOSE = 4`
(`engine_tilemap::orient`). The displayed art is the authored art after (1) transposing
across the top-left/bottom-right diagonal, (2) mirroring horizontally if `FLIP_X`,
(3) mirroring vertically if `FLIP_Y`. A 90 degree clockwise rotation is
`TRANSPOSE | FLIP_X`, 180 degrees is `FLIP_X | FLIP_Y`, 270 degrees clockwise is
`TRANSPOSE | FLIP_Y`. Flags are baked into the UVs and rotate custom collision shapes.

## Rule tiles (autotiling)

A rule tile is a logical tile id that painters place; it displays one of several concrete
tiles depending on its eight neighbours, like Unity's RuleTile.

- A rule has eight requirements in the order **NW, N, NE, W, E, SW, S, SE**: `any`,
  `this` (the neighbour is this rule tile, or a tile listed in the rule tile's `connects`),
  `not_this` (empty or a different tile).
- `transform` expands a rule into oriented variants, tried in this order: `none` (as
  authored), `rotated` (0, 90, 180, 270 degrees clockwise), `mirror_x` (as authored,
  mirrored left-right), `mirror_y`, `mirror_xy` (as authored, X, Y, both). A variant
  that duplicates an earlier pattern of the same rule is dropped. The matched variant's
  flags become the cell's displayed orientation.
- Rules are tried in list order and **the first match wins**; if nothing matches, the
  rule tile's `default_tile` shows. Rule outputs and `default_tile` must be concrete
  tiles.
- Presets (`RuleTile::blob47`, `RuleTile::four_neighbour16`, or `tilemap_define_rules`):
  - `four_neighbour16(id, name, first)`: output `first + mask` with N=1, E=2, S=4,
    W=8; corners are ignored; `default_tile = first`.
  - `blob47(id, name, first)`: neighbour mask N=1, NE=2, E=4, SE=8, S=16, SW=32, W=64,
    NW=128; a corner bit counts only when both adjacent edges are present. The 47
    distinct reduced masks, ascending, are `0, 1, 4, 5, 7, 16, 17, 20, 21, 23, 28, 29,
    31, 64, 65, 68, 69, 71, 80, 81, 84, 85, 87, 92, 93, 95, 112, 113, 116, 117, 119,
    124, 125, 127, 193, 197, 199, 209, 213, 215, 221, 223, 241, 245, 247, 253, 255`
    and map to `first + index` (an isolated cell is `first`, a fully surrounded cell
    is `first + 46`). The tileset needs those 47 concrete tiles.
  - Custom rules given with a preset in `tilemap_define_rules` are tried before the
    preset's rules.
- Painting updates only the affected cells: the painted cell and its eight neighbours
  re-resolve, and only chunks whose displayed tile changed get a new revision.

## Painting model

`TileLayer` operations take an optional tileset (`Some` resolves rule tiles and checks
tile ids):

| Operation | Behaviour |
| --- | --- |
| `set(cell, tile, flags, ts)` / `erase(cell, ts)` | One cell; `tile == 0` erases. |
| `fill_rect(rect, tile, flags, ts)` | Half-open rectangle inside the bounds, at most 65,536 cells; `tile == 0` clears. |
| `flood_fill(seed, tile, flags, limit, ts)` | 4-connected region of the seed's painted tile, restricted to `limit` and the bounds, at most 65,536 cells. Over the cap it fails with `tilemap.fill` and changes nothing. |
| `refresh_all(ts)` | Re-resolve every rule tile (after loading or editing rules). |

Each returns a `PaintReport { changed, refreshed, dirty_chunks }`. Every chunk carries a
revision stamp (`chunk_revision`) that increases on any change and never repeats for a
recreated chunk; consumers rebuild only chunks whose stamp changed.

## Collision generation

`engine_tilemap::collision` works in cell units:

- `merge_cells(cells)`: greedy rectangle merge. Rows are scanned bottom-up and
  left-to-right; each rectangle extends right while the row is solid, then up while the
  whole span stays solid. Rectangles are disjoint and their union equals the cell set.
- `collision_boxes(layer, tileset)`: the merged rectangles of `full` tiles, followed by
  one box per custom `rect` tile and the bounding box of each custom `polygon` tile
  (oriented by the cell's flags). The 2D solver is axis-aligned, so polygons collide as
  their bounding boxes.
- `collision_outlines(layer, tileset)`: closed loops around `full` cells (outer loops
  counter-clockwise, holes clockwise, collinear points removed, diagonal contacts kept as
  separate loops) plus each custom shape as its own polygon. Available for custom
  physics or debug drawing; the runtime uses the boxes.

## `engine.Tilemap` component

Requires `Transform` (and an authored entity ID). Editable through MCP `component_set`
(`{"version": 1, "data": {...}}`) and documented by `component_schema`.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `tileset` | string | `""` (required) | `tileset` asset id. |
| `map` | string | absent | `tilemap` asset id. |
| `inline` | object | absent | A tilemap document instead of `map` (the 64 KiB component limit applies). Give at most one of `map` and `inline`; with neither the map starts empty with bounds -512..512. |
| `cell_size` | `[f32; 2]` | `[1, 1]` | Cell size in local units, (0, 10000]. |
| `tint` | `[f32; 4]` | `[1,1,1,1]` | Multiplied into vertex colours (alpha also scales layer opacity). |
| `visible` | bool | `true` | Draw the tiles. |
| `render_layer` | int | `0` | Base draw layer; tile layer `L` draws at `render_layer + L.z_order`. |
| `collision.enabled` | bool | `true` | Generate colliders. |
| `collision.layer` / `mask` | u32 | `1` / `u32::MAX` | Collision bits given to every generated collider. |
| `collision.trigger` | bool | `false` | Generate trigger volumes. |
| `collision.layers` | string[] | `[]` | Only these tile layers collide; empty means all (at most 32). |

Changing `tileset`, `map` or `inline` reloads the map and discards runtime edits;
changing the other fields only rebuilds the affected render chunks or colliders.

## Runtime behaviour

`world::pre_simulation` calls `sync_tilemaps` each tick, so edits made by scripts earlier
in the tick are visible to physics and to the next `extract_frame`. The first tick also
loads the assets and builds everything (collision is therefore present from the first
tick's collision sync; a `move_entity` before any tick does not see tile colliders). A
failing sync fails the tick with its diagnostic (`tilemap.*`, `project.asset.*`).

For each `engine.Tilemap` entity the service keeps:

- one **render entity per non-empty chunk of each visible layer** (see below), and
- one **collider entity per generated box** with `Collider2D { size, offset, trigger,
  layer, mask }`, `Transform` at the origin and `Parent` set to the tilemap entity, ids
  `tilemap:<owner id>/collider/<n>`. They enter the existing `gameplay2d::World` through
  the normal `Collider2D` sync, so `move_entity`, swept movement, overlap queries and
  trigger events work unchanged. The tilemap's own transform (translation, rotation,
  scale) applies through the parent chain. At most 1024 boxes per tilemap
  (`tilemap.collision.capacity`), the capacity of the 2D solver.

At most 64 tilemap entities (in authored-id order); a tilemap that cannot be served (the 65th,
an unreadable asset, no registered authored id) is skipped with a reason (`tilemap::failure`,
logged once) and never fails the tick.

Generated entities carry `TilemapGenerated { owner }`, are not in the entity lookup and
are not part of scene snapshots. They are despawned when the owner despawns, loses the
component or reloads. `Runtime::snapshot` still counts them in `live_entities`.

Runtime edits are not written back to assets automatically. Use `export_document` to get
the live map, or `store_inline` to place it in the component's `inline` field (so
snapshots carry it; fails above ~60 KiB, then save a `tilemap` asset instead).

## Render data handed to the sprite path

Until the rendering domain provides a tilemap renderer, tiles render through the existing
2D draw path (`runtime::Draw`). This is the precise contract (the adapter is
`world::tilemap::service::sync_visuals` plus one hook in `Runtime::extract_frame`):

**Entities** (one per visible layer, per non-empty chunk): id and name
`tilemap:<owner id>/<layer name>/<chunk x>,<chunk y>`; `Transform` identity;
`Parent(<owner id>)`; `MeshRenderer { mesh: "", texture: Some(<tileset texture asset>) }`;
`GeometryOverride(Arc<Mesh>)`; `TileChunkRender { owner, layer, chunk, draw_layer }`.
Layers with `visible == false`, opacity 0, or a hidden component have no entities.

**Mesh** (`content::Mesh`, no normals): 4 vertices and 6 indices per non-empty cell,
cells in chunk row-major order (bottom row first). Vertex order is bottom-left,
bottom-right, top-right, top-left; indices are `b, b+1, b+2, b, b+2, b+3`.

- `position = [x, y, 0]` in the **tilemap's local space**: cell `(cx, cy)` has its
  bottom-left corner at `(cx * cell_size.x, cy * cell_size.y)`. The vertex shader/extract
  path applies the entity's world transform (the parent's), exactly like sprite quads.
- `uv` is normalised to the atlas texture with the origin at the texture's top-left
  (`v` grows downward), taken from the tile's `source` rectangle with the cell's display
  orientation flags already applied. There is no half-texel inset; use padded atlases to
  avoid bleeding when scaled.
- `color = tint` with `alpha = tint.a * layer.opacity`.
- At most 1024 vertices per chunk entity.

**Draw** (`extract_frame`): `Draw { id: "mesh:<entity id>", space: World, layer:
render_layer + layer.z_order, state: DrawState::default() (straight alpha blend, no depth
test, no depth write), texture: Some(atlas), motion: Some(entity position, pixel_snap:
false), offset: [0;3] }`. Draws sort by `(space, layer, id)` with sprites; a tile layer
therefore interleaves with `SpriteRenderer.layer` by integer draw layer. This is the one
edit outside `world/tilemap`: `runtime/render.rs` reads `TileChunkRender` in the existing
`MeshRenderer` branch to choose `layer` and `DrawState` (other mesh renderers keep
`layer: 0` and opaque depth-tested state).

**Update rule**: a chunk's mesh is rebuilt when its revision, the layer's opacity or
z order, `cell_size`, `tint` or `render_layer` changes; unchanged chunks keep their
`Arc<Mesh>`, and `Runtime` hashes meshes so draws reuse the cached mesh object.

**Camera3D scenes**: the chunk entities are ordinary `MeshRenderer` entities, so with an
active `Camera3D` they appear in `SceneRenderFrame::meshes` (lit mesh path) and are removed
from the sprite draws. A pure-2D game has no `Camera3D` and uses the sprite path above.
`pixel_snap` is not applied to tile meshes; keep `cell_size` a multiple of
`1 / pixels_per_unit`.

A dedicated renderer can replace the adapter by reading the `Tilemaps` resource or
calling `engine_tilemap::geometry::chunk_geometry(layer, chunk, tileset, cell_size,
texture_size)` (positions, UVs and indices as above) and skipping the generated mesh
entities.

## Native API

`engine_tilemap` (no ECS): `Tileset`, `Tilemap`, `TileLayer`, `RuleTile`, `collision`,
`geometry` as above, plus `Tileset::from_json` / `to_json`, `Tilemap::from_json` /
`to_json`.

`engine_project::world::tilemap` (needs a `World`; works from native scripts and systems):

| Function | Purpose |
| --- | --- |
| `paint`, `erase`, `fill_rect`, `flood_fill` | Edit a named layer of the entity's live map (rule tiles auto-apply). |
| `with_tilemap(world, entity, \|map, tileset\| ...)` | Direct access to the live `Tilemap` and `Tileset`. |
| `tile_at_world(world, entity, layer, [x, y, z])` | `TileHit { cell, tile, shown, flags, properties }` under a world position, through the entity's transform. |
| `world_to_cell`, `cell_to_world` | Convert between world positions and cells (centre). |
| `regenerate_collision(world, entity)` | Rebuild colliders immediately. |
| `sync_tilemaps(world)` | Bring render chunks and colliders up to date now (also done each tick). |
| `export_document`, `store_inline` | Persist runtime edits. |
| `status(world, entity)` | `TilemapStatus { layers, chunks, render_chunks, quads, colliders, render_path }`. |
| `load_tileset`, `load_tilemap`, `texture_size` | Asset loaders shared with MCP. |

Painting changes the live map immediately; render chunks and colliders follow at the next
`sync_tilemaps` (next tick, or call it). Generated colliders reach the 2D solver at the next
collision sync (end of the tick, or `move_entity`).

## MCP tools

See [MCP server](mcp.md). Read tools: `tilemap_info`, `tilemap_query`. Write tools:
`tilemap_create_tileset`, `tilemap_create`, `tilemap_define_rules`, `tilemap_paint`. Typical
flow: `tilemap_create_tileset` (register the returned `tileset` asset in `project.json`),
`tilemap_define_rules`, `tilemap_create` (register the `tilemap` asset), `tilemap_paint`,
`tilemap_query` / `tilemap_info` to verify, then `component_set` an `engine.Tilemap`
component onto an entity. Creation tools never overwrite; `tilemap_paint` applies up to
256 ops atomically and rewrites the file with a temp-file rename. Tools never execute
game code.

## Deferred and limits

- Isometric and hexagonal grids, a dedicated GPU tilemap renderer, animated tiles,
  per-cell tint and tile-level z offsets.
- Polygon collision is exact only in outlines; the solver uses bounding boxes.
- Collision exists after the first tick; 1024 generated boxes per tilemap.
- `tilemap_paint` addresses cells inside the map bounds only; bounds cannot be resized.
- Tile meshes ignore camera pixel snapping and have no texel inset.
