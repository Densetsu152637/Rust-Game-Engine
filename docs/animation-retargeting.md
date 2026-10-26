# Animation retargeting

`engine_animation::retarget::RetargetMap` converts a complete source local pose
into a complete target local pose. It uses explicit index mappings and the two
validated `Skeleton`s, including non-joint ancestors. There is no bone-name,
humanoid, nearest-joint or proportion inference.

A `RetargetDefinition` contains 1–128 unique source/target pairs. Each pair has an
optional normalized quaternion `basis`, defaulting to identity. It maps motion
axes in the source rest-joint frame into the target rest-joint frame. This is
explicit because a twist around a source bone's X axis may be a twist around a
target bone's Y axis. Different authored rest orientations are also preserved.

```json
{
  "joints": [
    {"source": 1, "target": 2},
    {"source": 4, "target": 6, "basis": [0, 0, 0.70710678, 0.70710678]}
  ],
  "root": {"mode": "discard"}
}
```

For every mapped joint, the nearest mapped ancestor must correspond in both
rigs. Unmapped intermediary nodes are allowed and remain explicit in each full
hierarchy. Reversed ancestry, duplicate/out-of-range indices, empty maps,
nonfinite/non-normalized bases, and coincident mapped-parent rest anchors fail.
Mapped rest segments must exceed 1e-6 model units. Relevant mapped ancestry must
have positive uniform scale; reflected/nonuniform or singular global frames fail.

## Rotation and proportions

Let `sp` and `tp` be a mapped joint's nearest mapped source/target ancestors; an
absent ancestor means the skeleton frame. With quaternion rotations:

```text
S0_rel = inverse(S0_global[sp]) * S0_global[source]
S_rel  = inverse(S_global[sp])  * S_global[source]
motion = inverse(S0_rel) * S_rel
T_rel  = T0_rel * basis * motion * inverse(basis)
T_global = actual_T_global[tp] * T_rel
T_local  = inverse(actual_T_global[parent(target)]) * T_global
```

Targets are processed in target hierarchy order, independently of mapping array
order. This applies mapped ancestor motion once. Source motion across unmapped
ancestor gaps is included in the corresponding mapped joint's rest-relative
rotation. Unmapped target local rotations remain exactly their target rest values.

All target local translations and scales remain at target rest, except optional
root translation below. Thus the target keeps its own bone offsets/proportions,
not source bone lengths. Source non-root translations and uniform animated scale
are intentionally not transferred. Sampled reflected/nonuniform scale on mapped
ancestry is rejected. This is rotation retargeting, not positional fitting or a
full-body constraint solver; downstream target-space IK can correct endpoints.

## Explicit local root translation

The default `{"mode":"discard"}` transfers no translation. Optional root transfer:

```json
{"mode":"scaled","source":1,"target":2,"scale":0.5,"alignment":[0,0,0,1]}
```

The pair must be mapped and topmost mapped in both rigs; the scalar must be finite
and positive, and alignment a normalized quaternion. It transports only the
selected source joint's **local translation delta from rest** through the source
rest-parent frame, then the explicit model-space alignment and scalar, and finally
through the target rest-parent inverse:

```text
delta_s = source_local_translation - source_rest_local_translation
delta_t = inverse(target_rest_parent_linear)
          * alignment * source_rest_parent_linear * delta_s * scale
target_local_translation = target_rest_local_translation + delta_t
```

Other joint/ancestor translation and animated scale do not contribute. The policy
uses declared rest frames, not an inferred actor height or an animated world-space
trajectory. Translation remains part of the target joint's local pose. It does
not move a second entity, accumulate motion across clip loops, or introduce a
world-transform writer. Overflow fails without mutating the input pose.

## Graph and project integration

The `retarget_clip` node is a source leaf of the existing `animation` graph kind:

```json
{
  "id": "walk",
  "node_type": "retarget_clip",
  "params": {
    "clip": "source-walk",
    "source_rig": "source-model",
    "target_rig": "character-model",
    "mapping": {"joints":[{"source":1,"target":2}]},
    "speed": 1,
    "looping": true
  }
}
```

It samples against the complete source rest pose, retargets, then emits the normal
**target-sized** `animation.pose`. Different source joint counts never enter the
target graph's blend/state/IK pose buffers. Normal clip nodes still require target
joint counts. Clip speed, looping, continuous inactive clocks and events follow
the established graph policy. Only contributing source nodes emit events; shared
node branches emit each event once. Events identify the source clip asset.

`GraphRigs::new(target_id, target_skeleton, source_rigs)` and
`GraphPlayer::with_rigs(graph, rigs, clips, &initial_parameters)` provide native
binding. Existing `new`, `with_parameters` and `with_skeleton` APIs retain their
contracts; retarget graphs require named rig bindings. Initial overrides are
validated before the first pose evaluation.

Project graph nodes name both model assets explicitly. Every retarget node in a
graph must share one target ID, which must equal `AnimationGraphPlayer.rig`.
Source rigs use imported model rest metadata; the target retains the current
validated bound local rest poses and full hierarchy/anchor rules described in
[Animation pose graphs](animation-graph.md). Source rig entities are not spawned.
Existing graph pose authority, prefab bindings, ragdoll order and skinning remain
the target path.

`Graph::asset_dependencies()` returns deterministic typed animation/model IDs,
sorted by kind then ID with duplicates removed. `clip_ids`, `rig_ids` and
`source_rig_ids` expose the constituent read dependencies. Project loading reads
both rig model dependencies and source clips, validates topology/maps/counts, and
runtime binding revalidates the actual target rig. Structural graph tools do not
evaluate poses. These APIs supply read tracking; this component adds no watcher.

Bounds: 128 mappings/node, 16 distinct source rigs and 65536 source rig nodes per
player, one named target rig per graph, at most 273 typed asset dependencies,
and existing graph/clip/pose/event/JSON limits. `RetargetMap::apply` returns a new
pose; graph evaluation failures retain previous graph clocks/output.

## Verification

```text
cargo test -p engine-animation --locked
cargo test -p engine-project --test animation_retarget --test animation_ik --test animation_graph --test skinning --test imported_animation --test physics_ragdoll --locked
cargo test -p engine-mcp --test graph --test state_tree --test tools --locked
cargo clippy -p engine-animation -p engine-project -p engine-mcp --all-targets --locked -- -D warnings
cargo fmt --all --check
```

Focused tests cover different bone axes including twist, rest orientations,
ancestor gaps, unequal proportions, missing/invalid maps and frames, explicit
root policy, cross-rig clip sizes, shared events, target-space IK and actual
project skin vertices. They establish no automatic humanoid mapping, general
full-body fitting or GPU retargeting claim.
