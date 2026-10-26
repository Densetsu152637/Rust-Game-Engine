# Level streaming and world partition

A large world is cut into a grid of cells. Each cell is an ordinary scene document; a
streaming source (the camera, a player, a drone) pulls the cells around it into the running
world and pushes the far ones out again, within per-tick and memory budgets, with background
file reads and optional low-detail stand-ins (HLOD proxies) for cells that are not loaded.

* `engine-streaming` is the pure core: the f64 cell grid, the load/unload state machine and
  scheduler, data layers and the manifest format. No ECS, no I/O, no threads.
* `engine_project::world::streaming` is the runtime: `engine.WorldPartition`,
  `engine.StreamingSource` and `engine.DataLayer`, background reads, spawning and despawning,
  HLOD proxies, events, telemetry and the native API. Origin rebasing is owned by
  [large-world coordinates](world-coordinates.md).
* Three MCP tools (`streaming_partition`, `streaming_hlod`, `streaming_info`) cut a scene
  into cells, build the proxies and inspect the result.

Contents: [Cells and the manifest](#cells-and-the-manifest) - [Scheduling](#scheduling) -
[Components](#components) - [Runtime behaviour](#runtime-behaviour) -
[Data layers](#data-layers) - [HLOD](#hlod) - [Native API](#native-api) -
[Events and telemetry](#events-and-telemetry) - [MCP tools](#mcp-tools) -
[Limits](#limits-and-limitations).

## Cells and the manifest

`Grid::new(cell_size, plane)` divides **absolute f64 metres** into cubes (`Xyz`) or vertical
columns (`Xz`, the default: key `y` is always 0). `CellKey([x, y, z])` is the cell
`[x * size, (x + 1) * size)` on each gridded axis; `Grid::key_of` assigns a position,
`origin(key)` is the corner, `distance(point, key)` is the distance to the cell's box (0
inside; the vertical axis is ignored in `Xz`). Keys saturate at the `i32` range.

The **partition manifest** (project asset kind `partition`, JSON, `PartitionManifest`) lists
the cells:

```json
{ "schema": 1, "cell_size": 100.0, "plane": "xz",
  "cells": [
    { "key": [0, 0, 0], "file": "cells/cell_0_0_0.json", "entities": 2, "bytes": 640,
      "hlod": { "file": "cells/hlod/cell_0_0_0.mesh.json", "triangles": 12, "source_triangles": 96 } },
    { "key": [1, 0, 0], "layer": "props", "file": "cells/cell_1_0_0.props.json", "entities": 3, "bytes": 910 }
  ] }
```

A cell is identified by `(key, layer)`; the default layer is the empty string. `file` is a
scene document (`{"schema":1,"scene_id":...,"entities":[...]}`); `bytes` feeds the byte
budget. **Root entities (no `parent`) store positions relative to the cell's minimum
corner**, so the f32 values stay exact at any distance from the origin (a cell at x =
50,000,000 m still has 0.25 m precision inside). When a cell loads, the runtime adds
`cell origin - WorldOrigin.offset` in f64 and only then rounds to f32. Children keep their
parent-relative transforms and must live in the same cell. Entity ids are unchanged by
partitioning and must stay unique across the world (a clash fails that cell, see below).
The manifest is validated on load: schema 1, valid grid, unique `(key, layer)`, `y == 0` in
an `xz` grid, project-relative forward-slash paths without `..`, at most 65,536 cells.

## Scheduling

`Streamer::plan(&[Source])` runs once per tick and returns a `Plan` (the state changes are
already applied). Cell states: `Unloaded -> Loading -> Loaded -> Unloading -> Unloaded`.

| Rule | Behaviour |
| ---- | --------- |
| Wanted | layer enabled and the cell's box is within a source's **load radius**, or pinned |
| Retained | loaded/loading cell, layer enabled, within a source's **unload radius** (>= load radius), or pinned |
| Hysteresis | the band between the radii neither loads nor unloads: a source jittering across the load radius loads a cell once and never unloads it (tested: 4 transitions in 100 ticks instead of 102) |
| Rank | best `(priority desc, distance asc)` over the sources whose unload radius reaches the cell; pinned cells rank first; ties break on `CellId` order |
| Load order | wanted unloaded cells in rank order, until the tick budget (`cells_per_tick`, `bytes_per_tick`; the first cell of a tick is always admitted, so a large cell cannot starve) or `max_in_flight` is reached |
| Memory budget | `max_loaded` bounds `Loading + Loaded + Unloading`; a better-ranked candidate evicts the worst-ranked unpinned loaded cell (lowest priority, then farthest); equal rank never evicts. Shrinking the budget evicts farthest first |
| Cancel | a `Loading` cell that is no longer retained goes back to `Unloaded` and its result is discarded |
| Failure | `fail_load` backs off 2, 4, 8 ... (max 32) ticks and stops after 3 failures until `reset_failures`/`pin` |

Plans are a pure function of the cell set, the sources, the limits and the call history, so
tests assert exact cells per tick. `TickBudget` (cells + bytes) is public; the runtime uses
it again to cap how many finished reads are applied per tick.

## Components

Authored, schema-validated and editable through `component_set`.

`engine.WorldPartition` (any entity; its authored id names the partition):

| Field | Default | Meaning |
| ----- | ------- | ------- |
| `manifest` | `""` | id of a `partition` asset; empty streams nothing |
| `load_radius`, `unload_radius` | 200, 250 | metres to the nearest point of a cell; `unload >= load` |
| `loads_per_tick` | 4 | cells started and applied per tick (1-1024) |
| `bytes_per_tick` | 0 | cell-file bytes per tick; 0 unlimited |
| `max_in_flight` | 4 | background reads at once (1-64) |
| `max_loaded_cells` | 64 | memory budget in cells (1-65,536) |
| `camera_source` | true | every active `Camera3D` is a priority-0 source |
| `synchronous` | false | read cells on the calling thread in the same tick (deterministic; tests/tools) |
| `hlod`, `hlod_radius` | true, 0 | show proxies of unloaded cells; only within `hlod_radius` of a source (0 = all) |
| `hlod_max_proxies` | 256 | most proxies shown at once (1..=65536), nearest to a source first |
| `layers` | `[]` | `[{name, enabled}]`; unlisted layers are enabled |

`engine.StreamingSource` (requires `Transform`): `enabled`, `priority` (integer, higher
first), `radius_scale` (0.01-100, multiplies both radii for this source). The source
position is the entity's world position plus `WorldOrigin.offset`.

`engine.DataLayer { layer }` tags an entity for the `streaming_partition` tool; it has no
effect on a running world.

Invalid combinations are rejected by `Project::validate_scene` (load, `scene_edit`,
`component_set`): radii order, ranges, duplicate layer names, a `manifest` that is not a
`partition` asset. `Project::validate` also loads every cell file of a `partition` asset and
runs the feature-component validators (`Project::validate_feature_components`: terrain,
foliage, water, tilemap, PCG, streaming, coordinates, acoustics) on it; diagnostics name
the cell file.

## Runtime behaviour

Every tick, in the world `pre_simulation` hook (before `simulation::step`):

1. **Reconcile** partitions with the world: new `engine.WorldPartition` entities open their
   manifest, a changed entity or manifest reopens, a removed entity unloads everything it
   owned. A manifest that cannot be read is reported once in the status and not retried until
   the entity or manifest changes. At most 16 partitions stream at once.
2. **Rebase** is not done here: the coordinates service (which runs first in
   `pre_simulation`) shifts every root entity, cell roots and proxies included, exactly once
   per origin move (see [large-world coordinates](world-coordinates.md)). Streaming only
   reads the current `WorldOrigin.offset` when it converts source positions to absolute and
   cell corners to local positions, so content loaded after a shift lands consistently.
3. **Plan** with the sources, then cancel, **unload** (despawn exactly the entities the cell
   spawned, never an entity a later scene reused) and **start loads**: a short-lived thread
   per cell reads and parses the document (`load_scene`, bounded to 16 MiB) and sends it
   back over a channel; `synchronous` does it inline.
4. **Apply** finished reads on the main thread in rank order within `loads_per_tick` and
   `bytes_per_tick`: the cell is checked (no entity id already in the world, every `parent`
   present, and the same feature-component validators as project validation pass, so for
   example a terrain with a missing asset is rejected here), its roots are shifted, entities are spawned like scene entities
   (`StreamedCell` marker added), custom components hydrated. A failing cell spawns nothing,
   logs a diagnostic, raises `streaming.cell_failed` and backs off.
5. **Proxies**, **status**.

Scene transitions despawn the world; partitions of the new scene start from scratch.

## Data layers

A layer is a name. Disabling it (`set_layer_enabled`, or editing
`engine.WorldPartition.layers`) makes its cells not retained: they unload on the next tick
and their proxies disappear; enabling streams them back under the normal budgets. The default
layer is always enabled. A pinned cell in a disabled layer stays unloaded (`force_load_cell`
fails with `streaming.layer.disabled`).

## HLOD

A cell's proxy is a single merged, simplified mesh in cell-local space listed in the
manifest (`hlod.file`, the project's JSON mesh format). While the cell is **not loaded**
(`Unloaded` or `Loading`), its layer is enabled and it is within `hlod_radius` of a source
(or the radius is 0), the runtime shows it as an entity `hlod:<partition id>:<x>_<y>_<z>[@layer]`
with `MeshRenderer` + `GeometryOverride` at the cell corner; the moment the cell is loaded
the proxy is despawned, so exactly one of the two is ever drawn (tests compare
`extract_scene_frame` meshes with the cell states every tick). Proxies use the existing CPU
mesh path, so headless runs and render frames work and nothing can draw twice; they are not
a GPU `PipelineFeature` and no GPU test applies. Proxy meshes are read on the main thread when
first needed (small files, at most 64 spawns per tick).

Proxies are bounded: at most `hlod_max_proxies` (default 256) are shown at once. Candidates
are ranked by distance to the nearest source (ties by cell key); proxies that fall outside
the cap are despawned and the ranking is re-evaluated every tick. Proxy meshes are cached
per file with least-recently-used eviction (256 soft entries); files of currently shown
proxies are never evicted, so a shown proxy is never re-read. `PartitionStatus.proxy_mesh_reads`
counts the actual disk reads.

`streaming_hlod` builds them offline: for each cell it takes the `MeshRenderer` entities
(composing the parent chain), merges them with `engine_mesh_lod::build_hlod` into one cluster
and simplifies to `ceil(triangles * proxy_ratio)` capped at `max_proxy_triangles`. Simplification
preserves topology, so tiny disjoint meshes may keep more triangles than requested; a flat
800-triangle grid at ratio 0.1 drops to at most 160. Textures and materials are not baked:
the proxy uses vertex colour white. Building proxies at load time is not implemented.

## Native API

All in `engine_project::world::streaming`; `partition` is the partition entity's authored id,
`layer` is `""` for the default layer.

| Function | Effect |
| -------- | ------ |
| `set_layer_enabled(world, partition, layer, enabled)` / `layer_enabled` | edit/read the layer in the component |
| `request_cell(world, partition, key, layer)` | pin: load through the normal path, never evict |
| `release_cell(...)` | unpin; the cell follows the sources again |
| `force_load_cell(world, partition, key, layer)` | pin and load now on the calling thread, outside every budget |
| `cell_state(world, partition, key, layer)` / `cell_infos(world, partition)` | `CellState` / per-cell state, pin flag, entity count, proxy flag |
| `partition_status(world, partition)` | the bounded `PartitionStatus` |

Requests work before the first tick (the partition opens on demand). Components:
`WorldPartition`, `StreamingSource`, `DataLayer`, `StreamedCell { partition, key, layer, proxy }`
(on every streamed entity). The pure scheduler (`engine_streaming::Streamer`) can be used
without the engine.

## Events and telemetry

Events go through the engine event queue (delivered on the next dispatch, visible in
`RuntimeSnapshot.events`), source = the partition entity:

| Event | Payload |
| ----- | ------- |
| `streaming.cell_loaded` | `partition`, `cell` `[x,y,z]`, `layer`, `entities` |
| `streaming.cell_unloaded` | same, `entities` = despawned count |
| `streaming.cell_failed` | same plus `error` |

`StreamingStatus` (a resource) and `RuntimeSnapshot.streaming` carry, per partition: cell
counts per state, pinned, in-flight, loaded entities, visible proxies, sources, cells
started/applied and bytes in the last tick, lifetime loads/unloads/failures, layer states, up
to 64 loaded cell names and up to 16 diagnostics of at most 240 characters. A status is a few
kilobytes at most, well inside the 64 KiB snapshot bound.

## MCP tools

| Tool | Access | Purpose |
| ---- | ------ | ------- |
| `streaming_partition` | write | split a scene into `cell_*.json` documents plus `partition.json` |
| `streaming_hlod` | write | build proxies and record them in the manifest |
| `streaming_info` | read | summarise a manifest, page its cells, answer "what loads at this point" |

`streaming_partition { scene, output_dir, cell_size, plane?, origin_offset?, exclude?, strip?,
expected_revision?, dry_run? }`: entities are grouped under the root of their parent chain; the
root's position (`origin_offset + local`, f64) picks the cell and its `engine.DataLayer` picks
the layer (names `[A-Za-z0-9_-]`, up to 64). Cameras, `engine.WorldPartition` /
`engine.StreamingSource` entities, excluded ids and groups without a root `Transform` stay in
the scene and are listed under `skipped`. Cell files are new files (`cell_<x>_<y>_<z>[.<layer>].json`);
nothing is overwritten, every target is checked first, and a failure while writing removes the
files already written. `strip` removes the moved entities in a revisioned edit (needs
`expected_revision`). Cross-cell references (event bindings, entity references in custom
components) are not rewritten. The result includes the `partition` asset entry for
`project.json`. Then: add the asset, give an entity `engine.WorldPartition { manifest }`.

`streaming_hlod { manifest, proxy_ratio?, max_proxy_triangles?, expected_hash?, dry_run? }` writes
`<manifest dir>/hlod/<cell>.mesh.json`; files the manifest already names are replaced, any
other existing file is refused. `streaming_info { manifest, offset?, limit?, position?, radius? }`
is file-only; the live state of a running world is `RuntimeSnapshot.streaming`.

## Limits and limitations

* The scheduler visits every known cell once per source per tick (no spatial index): fine for
  tens of thousands of cells; the manifest cap is 65,536.
* One background thread per in-flight cell (at most `max_in_flight`, 64); no shared pool.
* A cell's entities appear in one tick; spawning is not spread across ticks inside a cell.
  Large cells should be split (`cell_size`) rather than budgeted.
* Entity ids must be unique across cells and the base scene; scripts and event bindings that
  name a streamed entity only work while its cell is loaded.
* Children must be in their root's cell. A root that moves at runtime keeps its cell
  membership; streaming never re-partitions.
* Streaming is local: networked replication of streamed content is out of scope.
* Proxies carry geometry only (white vertex colour), and are built offline.
