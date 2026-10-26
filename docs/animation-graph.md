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
| `ik_two_bone` | `input` | joint indices `root`, `mid`, `end`; Vec3 variables `target`, `pole`; float `weight`; `min_bend`, `max_bend` |

Variables are `float`, `bool` or `vec3`; omitted defaults are zero, false or
`[0,0,0]`. Float and vector component values
must fit finite f32. A transition is an object such as
`{"from":"idle","to":"walk","parameter":"speed","op":"gt","value":0.1,"duration":0.2}`.
Transition conditions use bool or float variables; Vec3 variables are IK controls.
Operators are `eq`, `ne`, `gt`, `ge`, `lt`, `le`; bool conditions support only
`eq` and `ne`. A transition must reference declared states and a variable of the
same type as its value. Self transitions are rejected. Duration is 0–60 seconds.

## Evaluation

`GraphPlayer::new(Arc<Graph>, rest_pose, resolved_clips)` binds clips with matching
joint counts and validates the rest pose. `set_parameters` atomically replaces
variable overrides; omitted variables return to defaults. `update(dt)` accepts
0–60 seconds and returns events; `pose()` exposes evaluated local transforms.
`GraphPlayer::with_parameters(graph, rest, clips, &overrides)` applies initial
overrides before the first evaluation.
`GraphPlayer::with_skeleton(graph, skeleton, clips, &overrides)` binds the complete
Skeleton hierarchy and effective initial controls for IK nodes;
`new` rejects IK graphs instead of guessing a hierarchy. Invalid time, pose, IK
geometry or event overflow leaves the previous clocks and pose intact.
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
This is CPU pose evaluation and skinning. Retargeting, root motion, morph targets
and a timeline sequencer remain separate features.

## Project integration

Attach the built-in component to an entity with Transform:

```json
"engine.AnimationGraphPlayer": {
  "version": 1,
  "data": {
    "graph": "locomotion",
    "rig": "character-model",
    "bindings": ["hip", "knee", "ankle"],
    "parameters": {"speed": 0.5}
  }
}
```

`rig` is optional for graphs without IK and required for IK graphs. It names an
existing imported `model` asset, not a second skeleton file format. An empty
binding list targets the owner. Otherwise the unique authored entity IDs
must match every clip's complete node list, as with `engine.AnimationPlayer`.
Prefab instantiation remaps these bindings. Project load validates graph assets
and their referenced animation assets. Runtime validates binding and parameter
compatibility. Native systems can edit `AnimationGraphPlayer.parameters` before
the animation stage. Changing graph, rig or bindings rebuilds the player; parameter
changes preserve it. File watching/hot reload is outside this component.

The graph writes its initial pose during startup, then evaluates before physics
and skinning on every runtime tick. Ragdolls consume its animation pose through
the existing animation/physics blending path. A target cannot have another graph,
clip player, physics body, navigation agent, crowd or remote network pose writer.
A remote replica cannot own a graph player. Existing
`SkinBinding` consumes the resulting transforms and deforms real mesh vertices
before extraction. Clip-player behavior remains available independently.

## Two-bone IK

Native `engine_animation::ik::{BendLimits, TwoBone}` operates on an existing
`Skeleton` and immutable local pose slice, returning a new pose. The graph node
uses the same solver; its indices refer to the complete bound model node list.
The middle joint must be the root's direct child and the end joint the middle's
direct child. Missing, repeated, out-of-range and disconnected indices fail.

Target and pole are **points in skeleton/model coordinates**, before any common
external scene anchor. The pole selects the plane and side of the middle joint.
The analytic solve rotates only the root and middle joints; local translations,
scales and the end's local orientation remain unchanged. Bone lengths come from
the incoming sampled pose. Unreachable targets clamp to its reachable range and
elbow bend limits (radians: zero is straight, PI is fully folded). Defaults are
0..PI. Constraints that collapse the end onto its root fail explicitly.

Weight is strictly 0..1. The full solve obeys the bend limits, then its two local
rotations blend with the input pose by quaternion slerp. Partial weight softens
the constraint influence; hard final-angle limits are claimed only at weight 1.
Weight zero disables directional solving but still validates topology, pose,
finite controls, scales and bone lengths. Other positive weights require a
noncoincident target and a pole not collinear with the root-to-target direction.
Ambiguous planes and zero-length bones return errors; no arbitrary pole axis is
invented. Segment lengths must exceed 1e-6 model units; nonzero-weight solves
also require root-to-target and projected-pole distances above 1e-6.
Chain and ancestor scales must be positive and uniform; reflected or
nonuniform scales are rejected instead of introducing shear. Finite transforms
and preserved segment lengths are checked on the result.

The project derives parent indices through the existing imported Skeleton
adapter and retains the **current validated bound local rest poses** when the
player is created. Component parameter overrides are validated and applied before
the first evaluation, so omitted zero vector defaults can be replaced by valid
authored target/pole points. Invalid effective controls still fail construction.
Bindings must cover every model node, including non-joint
ancestors, in source index order, and their ECS Parent links must match the
model. Model roots can have no external parent or share one common external
anchor. Different root anchors, omitted ancestors, unexpected bound parents,
missing anchors and nonuniform/reflected anchor ancestors fail. This preserves
one model coordinate frame: an anchor may translate, rotate or uniformly scale
the entire rig without changing IK target coordinates. Rig hierarchy is checked
again on every synchronization; reparenting an interior bone fails before the
next graph pose write. Asset file watching is not implied.

The solver is bounded and analytic; no iterative chain solver, general joint
constraint system, foot planting or collision-aware IK is claimed. Ragdolls
consume its result through the same pre-physics pose path as other graph nodes.

## Verification

```text
cargo test -p engine-animation --locked
cargo test -p engine-project --test animation_graph --test animation_ik --test skinning --test imported_animation --test physics_ragdoll --locked
cargo test -p engine-mcp --test graph --test state_tree --locked
cargo clippy -p engine-animation -p engine-project -p engine-mcp --all-targets --locked -- -D warnings
cargo fmt --all --check
```

The graph integration tests compare actual `GeometryOverride` vertices and frame
extraction, parameter updates, prefab copies, authority failures and animated
ragdoll behavior on substep frames. Native and authored IK tests also cover
reachable/unreachable targets, opposite poles, limits, weighted bone lengths,
non-joint ancestors, external anchors, prefab rig copies and invalid controls. They do not establish interactive editor UX or
GPU pixel output.
