# engine-pcg

Deterministic procedural content generation: seeded noise, a typed node graph, a node
library and a bounded topological executor that runs against a `Surface` (the ground).
Pure Rust; dependencies are `glam`, `serde` and `serde_json`. It knows nothing about the
engine or the terrain: `engine-project` runs graphs in scenes (`engine.PcgVolume`) and
`engine-mcp` edits them (`pcg_create`, `pcg_edit`, `pcg_validate`, `pcg_run`). See
[docs/pcg.md](../../docs/pcg.md) for the full guide.

| Module | Contents |
| ------ | -------- |
| `noise` | `Noise`, `NoiseParams`: Perlin, Simplex (2D/3D), Worley F1/F2/F2-F1, fBm, ridged; `f64` coordinates, platform-independent |
| `rng` | SplitMix64 `Rng`, `derive_seed(graph_seed, node_id)`, `hash_cell` |
| `graph` | `Graph`, `Node`, `Edge`, `Diagnostic`; versioned JSON (`GRAPH_VERSION = 1`) |
| `registry` | `Registry` of node kinds, `NodeDefinition`, `PortSpec`, `ParamSpec`, `Limits`, `RunSettings`; `Registry::register` adds game-defined kinds |
| `nodes` | the built-in samplers, fields, filters, modifiers, set operations and weighted spawner |
| `exec` | `validate` (precise diagnostics) and `run` (deterministic, bounded) |
| `edit` | `EditOp`/`apply_edit` and the fluent `GraphBuilder` |
| `surface` | `Surface` trait, `Aabb`, `FlatSurface`, `FnSurface` |

```rust
use engine_pcg::{Aabb, FlatSurface, GraphBuilder, Registry, RunSettings, run};
use serde_json::json;

let registry = Registry::builtin();
let graph = GraphBuilder::new(&registry, 7)
    .node("points", "poisson_sampler", [("min_distance", json!(2.5))])
    .node("trees", "spawner", [("entries", json!([{"asset": "tree", "kind": "mesh"}]))])
    .connect("points.points", "trees.points")
    .build()
    .unwrap();
let bounds = Aabb::new([-32.0, -8.0, -32.0], [32.0, 8.0, 32.0]);
let result = run(&graph, &registry, &FlatSurface::new(0.0), &RunSettings::new(7, bounds)).unwrap();
assert!(!result.instances.is_empty());
```

Determinism: the output of `run` is a pure function of the graph, the `RunSettings` and the
surface. Nodes execute in a fixed order and draw randomness only from
`derive_seed(settings.seed, node_id)`. Noise is bit-stable across platforms; Poisson-disk
placement uses `f32` trigonometry whose last bit may differ between math libraries.

Bounds: 512 nodes, 2048 edges per graph; `Limits` caps points and instances per node and in
total (default 262 144); the Poisson acceleration grid is capped at 16 M cells.

Validation: `cargo test -p engine-pcg` covers noise range, determinism, seed sensitivity,
continuity, pinned golden values, mean and variance bands, every built-in node, validation
diagnostics, limits, edits, JSON round trips and custom node registration.
