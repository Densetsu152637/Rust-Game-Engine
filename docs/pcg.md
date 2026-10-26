# Procedural content generation

`engine-pcg` generates points and object instances from a graph of nodes, a seed and a
ground description. `engine_project::world::pcg` runs graphs inside scenes through the
`engine.PcgVolume` component, and four MCP tools author and bake graphs. The crate is pure
Rust (glam, serde, serde_json) and never touches the engine, so tools, tests and games use
the same code path.

Contents: [Noise](#noise) - [Graphs](#graphs) - [Node library](#node-library) -
[Executor](#executor) - [Scenes and `engine.PcgVolume`](#scenes-and-enginepcgvolume) -
[MCP tools](#mcp-tools) - [Native API](#native-api) - [Limits](#limits-and-limitations).

## Noise

`engine_pcg::Noise::new(NoiseParams)` is a seeded noise function sampled in `f64`
coordinates (so absolute positions in a large world keep their precision) and returning
`f32` in `[0, 1]` through `sample_2d(x, y)` and `sample_3d(x, y, z)`.

| `NoiseKind` | Meaning |
| ----------- | ------- |
| `Perlin` | 2D/3D gradient noise with a seeded 256-entry permutation and quintic fade |
| `Simplex` | 2D/3D simplex noise |
| `WorleyF1`, `WorleyF2`, `WorleyF2MinusF1` | Cellular distance to the nearest, second nearest point, and their difference; `Euclidean` or `Manhattan` metric, `jitter` in `[0, 1]` |

`Fractal` is `None`, `Fbm` (sum of `octaves` of `lacunarity`-scaled, `gain`-weighted
octaves, normalised) or `Ridged` (`(1 - |n|)^2` creases). The basis functions use only
integer hashing and IEEE `+ - * /`, `floor` and `sqrt`, so the same seed gives the same
values on every platform. `Rng` (SplitMix64) and `derive_seed(graph_seed, node_id)` give
every node an independent stream that depends only on the graph seed and the node id,
never on node order.

Behaviour verified by `cargo test -p engine-pcg`:

- every kind and fractal stays inside `[0, 1]`; the raw Perlin and Simplex basis functions reach below -0.6
  and above 0.6;
- equal seeds are bit-identical, different seeds differ at more than 90 % of sample points;
- continuity: a 1e-3 step changes a non-fractal value by less than 0.01;
- mean and variance over 62 500 samples (Perlin mean 0.5 +- 0.02, variance 0.01..0.06;
  Simplex mean 0.5 +- 0.02, variance 0.02..0.08);
- six pinned golden values (tolerance 1e-6) detect any change of the generator;
- Worley without jitter has exactly the cell-centre distance (tolerance 1e-12).

## Graphs

A graph is JSON (`engine_pcg::Graph`, project asset kind `pcg`, `version: 1`):

```json
{
  "version": 1,
  "seed": 7,
  "nodes": [
    {"id": "points", "kind": "poisson_sampler", "params": {"min_distance": 3.0}},
    {"id": "spawn", "kind": "spawner",
     "params": {"entries": [{"asset": "tree", "kind": "mesh", "weight": 3}]}}
  ],
  "edges": [
    {"from": {"node": "points", "port": "points"}, "to": {"node": "spawn", "port": "points"}}
  ]
}
```

Nodes have a unique id (`[A-Za-z0-9_.-]`, at most 64 characters), a registered kind and
typed parameters; unspecified parameters use the kind's default. Ports are typed (`points`,
`field`, `instances`) and an input accepts one edge. The model is deliberately the common
shape of the engine's graph assets (nodes with typed ports, edges, typed parameters,
validation, versioned serde JSON) so it can move onto a shared graph crate later.

`engine_pcg::validate(&graph, &registry)` returns every problem with a stable code and the
node and port it concerns: `pcg.limit.nodes`, `pcg.limit.edges`, `pcg.node.invalid_id`,
`pcg.node.duplicate_id`, `pcg.node.unknown_kind`, `pcg.param.unknown`, `pcg.param.invalid`
(type and range), `pcg.param.required` (a spawner without `entries`), `pcg.edge.duplicate`,
`pcg.edge.unknown_node`, `pcg.edge.unknown_port`, `pcg.edge.type_mismatch`,
`pcg.input.multiple`, `pcg.input.missing` and `pcg.graph.cycle`. Loading rejects unknown
JSON fields (`pcg.graph.parse`) and versions above 1 (`pcg.graph.version`).

`EditOp` (`add_node`, `remove_node`, `connect`, `disconnect`, `set_parameter`,
`clear_parameter`, `set_seed`) edits a graph incrementally; each operation is checked as it
is applied and a refused operation leaves the graph unchanged. `connect` replaces an
existing edge into the same input and refuses loops.

## Node library

| Kind | Inputs | Output | Notes |
| ---- | ------ | ------ | ----- |
| `grid_sampler` | - | points | `spacing`, `jitter`; one point per cell centre |
| `random_sampler` | - | points | `count` uniform points |
| `poisson_sampler` | - | points | Bridson blue noise; `min_distance`, `attempts`, `max_points`; guaranteed spacing |
| `noise_field` | - | field | any noise kind/fractal, `frequency`, remapped to `[min, max]`; sampled at absolute (origin offset + local) xz |
| `surface_field` | - | field | `height` (remapped over `low..high`), `slope` (degrees / 90) or a paint `layer` |
| `field_math` | `a`, optional `b` | field | add, subtract, multiply, min, max; `constant` replaces a missing `b` |
| `density_filter` | points, field | points | stores the field as density; `threshold` or `probabilistic` mode, `invert` |
| `height_filter` | points | points | keeps `min <= y <= max` |
| `slope_filter` | points | points | keeps ground slope within `min_degrees..max_degrees` |
| `bounds_filter` | points | points | inside (or outside, `invert`) a box |
| `self_prune` | points | points | drops points closer than `min_distance`; higher density wins, then earlier index |
| `transform_points` | points | points | position jitter and offset, surface snapping, yaw range, tilt, uniform or per-axis scale, align to ground normal |
| `merge` | `a`, `b` | points | union, optionally dropping `b` points within `dedupe_distance` of `a` |
| `difference` | `a`, `b` | points | points of `a` farther than `radius` from every point of `b` |
| `spawner` | points | instances | weighted choice per point among `entries` (`asset`, `kind` `mesh` or `prefab`, `weight`) |

Samplers snap points to the surface and record its normal. A node's output order is the
input order unless stated. All randomness comes from the node's derived seed combined with
the point's own seed and index, so inserting a node does not reshuffle unrelated nodes.

## Executor

```rust
let result = engine_pcg::run(&graph, &registry, &surface, &settings)?;
```

`run` validates, then executes nodes in a fixed topological order (ties by node id), so the
output is a pure function of `(graph, settings, surface)`. `RunSettings` carries the
effective seed (`Graph::seed_for(instance_seed)` mixes the graph seed with a volume or call
seed), the generation `Aabb` in surface-local coordinates, the `domain_offset` (absolute
position of the local origin; noise samples `local + offset`) and `Limits`
(`max_points` and `max_instances`, default 262 144 each). Exceeding a limit stops the run
with `pcg.limit.points` / `pcg.limit.instances` rather than truncating. `RunResult` holds
the instances of every unconsumed spawner, a per-node report and every node output.

The ground is any `Surface`:

```rust
pub trait Surface: Send + Sync {
    fn bounds(&self) -> Aabb;                       // horizontal extent
    fn height(&self, x: f32, z: f32) -> f32;
    fn normal(&self, x: f32, z: f32) -> Vec3;
    fn layer_weight(&self, layer: &str, x: f32, z: f32) -> Option<f32> { None }
}
```

`FlatSurface` and `FnSurface` (closures, with optional paint layers) ship with the crate;
`engine-pcg` does not depend on the terrain crate. A terrain module implements `Surface`
for its heightmap at integration time. Generation is clipped to `Aabb ∩ Surface::bounds`.

## Scenes and `engine.PcgVolume`

`engine.PcgVolume` (requires `Transform`, editable with `component_set`):

| Field | Default | Meaning |
| ----- | ------- | ------- |
| `graph` | `""` | id of a `pcg` asset; empty generates nothing |
| `seed` | 0 | run seed, mixed with the graph's own seed |
| `center`, `size` | `[0,0,0]`, `[32,64,32]` | box relative to the entity's world position (rotation and scale are ignored) |
| `mode` | `runtime` | `runtime` generates; `baked` never generates (output was written into the scene by `pcg_run`) |
| `regenerate` | 0 | bump to regenerate with otherwise unchanged values |
| `surface` | `""` | name in `PcgSurfaceRegistry`; empty is the default ground, flat at `y = 0` |
| `max_instances` | 4096 | hard cap (1..=65 536) |

Project validation requires the `graph` asset to exist with kind `pcg` and every `pcg`
asset to parse. Node kinds are checked when a graph runs, because games can register their
own.

While a `Runtime` ticks, the pre-simulation hook compares each volume with the values it
last generated. A first tick or any change (including `regenerate`) runs the graph with
`domain_offset = WorldOrigin.offset`, despawns exactly the entities of the previous output
and spawns the new ones in the same tick, before transforms are refreshed:

- a mesh instance becomes the entity `pcg:<volume id>:<n>` with `Transform` and a
  `MeshRenderer`, so it renders through the normal mesh path (and `RenderFrame` tests see
  it);
- a prefab instance becomes an anchor entity `pcg:<volume id>:<n>` carrying the instance
  transform, plus the prefab's entities with ids `pcg:<volume id>:<n>/<prefab entity id>`
  parented to the anchor (references inside components are remapped like
  `instantiate_prefab`). Components of prefab entities are attached, but scripts are not
  started for runtime-spawned entities.

When a volume entity disappears (despawned, or its streamed cell unloads) or loses its
`engine.PcgVolume`, the next tick despawns its whole output and drops its `PcgStatus` entry.
A generated id that already belongs to an entity the volume does not own (two volumes with
the same id, say) fails that volume's attempt with `pcg.entity.id_collision` instead of
overwriting the other entity's lookup entry.

Generation is deterministic: the same project, volume values and surface give the same ids
and transforms on every run. A failed attempt (bad graph, unknown surface, missing asset,
limit exceeded) keeps the previous output, appends `pcg <volume>: ...` lines to the runtime
log and records the diagnostics in the `PcgStatus` resource; the failed values count as
applied, so edit the volume or bump `regenerate` to retry. `PcgStatus` also reports per
volume attempts, instance and entity counts and per-node point counts.

Resources installed in every runtime world:

- `PcgSurfaceRegistry`: `set_default`, `insert(name, Arc<dyn Surface>)`, `remove`. Surface
  contents are not tracked: bump `regenerate` after the ground changes.
- `PcgNodes`: the node registry (`Registry::builtin()` initially); register game node kinds
  with `world.resource_mut::<PcgNodes>().0.register(...)` before the first tick.

## MCP tools

All tools use the built-in node library and a flat ground, never execute game code, bound
their inputs and keep files inside the project.

| Tool | Access | Purpose |
| ---- | ------ | ------- |
| `pcg_create` | write | New graph file (`output`, `seed`, `template` `empty` or `scatter` with mesh `assets`); never overwrites; returns the `pcg` asset entry to add to `project.json` |
| `pcg_edit` | write | Ordered `operations` (the `EditOp` JSON above, at most 256) applied atomically to an existing graph with `expected_hash`; failures name the operation index and problem and leave the file untouched; incomplete graphs may be saved and list their `problems` |
| `pcg_validate` | read | Validation with precise `problems`; `kinds` lists the node catalog with ports and parameters; `run` executes a bounded dry run and returns counts, per-asset totals, bounding box, content hash, node reports and a sample |
| `pcg_run` | write | The same run, plus (with `scene` and `expected_revision`) one revisioned atomic scene edit that replaces every entity whose id starts with `pcg:<volume>:` by the new output; `dry_run` validates without saving |

Run parameters: `seed`, `center`, `size`, `origin_offset`, `ground_height`,
`max_instances` (at most 65 536) and `sample` (at most 64). Writing into a scene is limited
to 8192 entities per call. A baked workflow is: `pcg_create`, `pcg_edit`, `pcg_run` into the
scene, then set the volume's `mode` to `baked`.

## Native API

```rust
use engine_pcg::{Aabb, FlatSurface, GraphBuilder, Registry, RunSettings, run};
use serde_json::json;

let registry = Registry::builtin();
let graph = GraphBuilder::new(&registry, 7)
    .node("points", "poisson_sampler", [("min_distance", json!(2.5))])
    .node("trees", "spawner", [("entries", json!([{"asset": "tree", "kind": "mesh"}]))])
    .connect("points.points", "trees.points")
    .build()?;
let bounds = Aabb::new([-32.0, -8.0, -32.0], [32.0, 8.0, 32.0]);
let result = run(&graph, &registry, &FlatSurface::new(0.0), &RunSettings::new(7, bounds))?;
```

Custom nodes: `Registry::register(NodeDefinition { kind, title, doc, inputs, outputs,
params }, |ctx| { ... })` where the closure reads `ctx.params`, `ctx.points("port")`,
`ctx.field("port")`, `ctx.surface`, `ctx.seed` and `ctx.run` and returns the outputs by
port name. Custom kinds are available to `run`, `validate` and the `PcgNodes` resource; the
MCP tools only know the built-ins, so a graph using a custom kind is reported as
`pcg.node.unknown_kind` by them.

## Limits and limitations

- Graphs: 512 nodes, 2048 edges, 1 MiB of JSON in MCP tools, 4 MiB when loaded by the
  project.
- Poisson sampling refuses acceleration grids above 16 M cells; the grid sampler and the
  random sampler honour `max_points`.
- Volumes ignore their entity's rotation and scale, and generation is single-threaded in the
  tick that detects the change; large volumes should be baked.
- Poisson-disk placement uses `f32` `sin`/`cos`, which can differ in the last bit between
  platform math libraries; noise itself is bit-stable.
- The runtime does not re-run graphs when a registered surface changes; bump `regenerate`.
- Only one `pcg` asset format version exists; there is no cross-graph subgraph node.
