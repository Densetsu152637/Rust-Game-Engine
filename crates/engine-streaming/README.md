# engine-streaming

The pure core of level streaming / world partition: a cell grid in absolute f64 coordinates,
streaming sources with load/unload hysteresis, a deterministic priority and budget scheduler,
data layers and the partition manifest format. No ECS, no I/O, no threads; dependencies are
`serde` and `serde_json`. `engine-project` runs it in scenes (`engine.WorldPartition`) and
`engine-mcp` writes manifests (`streaming_partition`, `streaming_hlod`, `streaming_info`). See
[docs/streaming.md](../../docs/streaming.md) for the full guide.

| Module | Contents |
| ------ | -------- |
| `grid` | `Grid`, `GridPlane` (`Xz` columns / `Xyz`), `CellKey`: position to key, cell origin, box distance |
| `scheduler` | `Streamer`, `Source`, `Limits`, `Plan`, `CellId`, `CellState`, `Layer`, `Rank`: the `Unloaded -> Loading -> Loaded -> Unloading` state machine |
| `budget` | `TickBudget`: per-tick cell and byte budget |
| `manifest` | `PartitionManifest`, `ManifestCell`, `HlodProxy`: validated JSON document |

```rust
use engine_streaming::{CellId, CellKey, Grid, GridPlane, Layer, Limits, Source, Streamer};

let mut streamer = Streamer::new(Grid::new(100.0, GridPlane::Xz).unwrap(), Limits::default());
for x in 0..10 {
    let id = CellId { key: CellKey([x, 0, 0]), layer: Layer::DEFAULT };
    streamer.add_cell(id, 4096).unwrap();
}
let source = Source { position: [50.0, 0.0, 50.0], priority: 0, load_radius: 150.0, unload_radius: 250.0 };
let plan = streamer.plan(&[source]);
for id in &plan.start_loads {
    // read the cell's content, then report back
    streamer.complete_load(*id).unwrap();
}
```

The host owns the work: `plan` moves cells to `Loading`/`Unloading`, the host reports with
`complete_load`, `fail_load` and `complete_unload`. Plans are a pure function of cells,
sources, limits and call history; ties break on `CellId` order. `Limits` carries the per-tick
cell and byte budget, the in-flight cap and the memory budget (farthest, lowest-priority cells
are evicted first); `pin` keeps a cell loaded regardless of sources.
