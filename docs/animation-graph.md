# Animation pose graphs

Animation assets of kind `graph` use the shared `engine-graph` version 1 document
with `kind: "animation"`. `engine_animation::graph::kind()` exposes the same
node palette, ports and parameter declarations to project validation, the editor
and MCP `graph_schema`, `graph_validate` and `graph_edit` tools.

`Graph::compile` validates structure and semantics before building a bounded
pose dependency DAG. Every node must contribute to exactly one `output`. Edges
carry the custom type `animation.pose` from a source's `pose` output into a
consumer input. Semantic errors use `animation.graph.invalid`; structural errors
retain the shared validator codes. Source outputs can feed several consumers. Samples/states are
ordered by their incoming edges in the document's `edges` array.

| Node | Inputs | Parameters |
| --- | --- | --- |
| `output` | `pose` | None |
| `clip` | None | `clip` animation asset ID; `looping` (default true), `speed` (0–100, default 1) |
| `blend` | `a`, `b` | `parameter`: float variable name; clamped to 0–1 |
| `blend1d` | ordered `samples` | `parameter`, strictly increasing `positions` |
| `blend2d` | ordered `samples` | float variable names `x`, `y`; `positions: [[x,y],...]`, `triangles: [[i,j,k],...]` |
| `state_machine` | ordered `states` | unique state names `states`, `initial`, ordered `transitions` |

Variables are `float` or `bool`; omitted defaults are zero or false. Float values
must fit finite f32. A transition is an object such as
`{"from":"idle","to":"walk","parameter":"speed","op":"gt","value":0.1,"duration":0.2}`.
Operators are `eq`, `ne`, `gt`, `ge`, `lt`, `le`; bool conditions support only
`eq` and `ne`. A transition must reference declared states and a variable of the
same type as its value. Self transitions are rejected. Duration is 0–60 seconds.

## Evaluation

`GraphPlayer::new(Arc<Graph>, rest_pose, resolved_clips)` binds clips with matching
joint counts and validates the rest pose. `set_parameters` atomically replaces
variable overrides; omitted variables return to defaults. `update(dt)` accepts
0–60 seconds and returns events; `pose()` exposes evaluated local transforms.
Invalid time, pose or event overflow leaves the previous clocks and pose intact.
Parameter-only changes preserve playback clocks and machine state.

All clip nodes run continuous independent clocks, including inactive states;
state entry does not restart a clip. Every node is evaluated once in dependency
order. `blend1d` interpolates neighboring positions and clamps at the endpoints.
`blend2d` uses barycentric coordinates in the first containing authored triangle;
outside the authored triangles it projects to the nearest triangle edge. Equal
distance ties follow authored triangle/edge order. Duplicate points/triangles,
unused points, invalid indices and degenerate triangles are rejected. Authors
supply triangle topology explicitly; gaps and overlaps follow these same
selection rules. Multiple poses blend in authored input order using normalized
incremental translation/scale lerp and quaternion slerp.

Each machine takes at most one transition per update, first matching its current
state in authored transition order. Fades freeze the displayed source pose and
advance the target pose, matching the existing Animator crossfade convention.
An interrupted fade freezes its current display so interruption is continuous.
Only incoming-state events are emitted during a fade. Other blends emit events
from positive-weight contributing clip nodes; shared nodes emit once. Event order
is clip-node document order, then the clip's chronological event order. Initial
construction emits no events, and zero-time events follow existing loop rules.

Limits are 256 nodes, 1024 edges, 64 variables, 64 samples/states per blend/machine,
128 triangles or transitions, 262144 joint-node pose entries per player, and
4096 emitted events per update. Existing clip/joint/key/event traversal limits
also apply, together with shared graph JSON size, depth and entry limits.
This is CPU pose evaluation and skinning; IK, retargeting, root
motion, morph targets and a timeline sequencer remain separate features.

## Project integration

Attach the built-in component to an entity with Transform:

```json
"engine.AnimationGraphPlayer": {
  "version": 1,
  "data": {
    "graph": "locomotion",
    "bindings": ["hip", "knee", "ankle"],
    "parameters": {"speed": 0.5}
  }
}
```

An empty binding list targets the owner. Otherwise the unique authored entity IDs
must match every clip's complete node list, as with `engine.AnimationPlayer`.
Prefab instantiation remaps these bindings. Project load validates graph assets
and their referenced animation assets. Runtime validates binding and parameter
compatibility. Native systems can edit `AnimationGraphPlayer.parameters` before
the animation stage. Changing graph or bindings rebuilds the player; parameter
changes preserve it. File watching/hot reload is outside this component.

The graph writes its initial pose during startup, then evaluates before physics
and skinning on every runtime tick. Ragdolls consume its animation pose through
the existing animation/physics blending path. A target cannot have another graph,
clip player, physics body, navigation agent, crowd or remote network pose writer.
A remote replica cannot own a graph player. Existing
`SkinBinding` consumes the resulting transforms and deforms real mesh vertices
before extraction. Clip-player behavior remains available independently.

## Verification

```text
cargo test -p engine-animation --locked
cargo test -p engine-project --test animation_graph --test skinning --test imported_animation --test physics_ragdoll --locked
cargo test -p engine-mcp --test graph --test state_tree --locked
cargo clippy -p engine-animation -p engine-project -p engine-mcp --all-targets --locked -- -D warnings
cargo fmt --all --check
```

The graph integration tests compare actual `GeometryOverride` vertices and frame
extraction, parameter updates, prefab copies, authority failures and animated
ragdoll behavior on substep frames. They do not establish interactive editor UX or
GPU pixel output.
