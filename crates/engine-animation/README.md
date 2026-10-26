# engine-animation

Device-independent skeletal animation built on workspace glam. It evaluates
real poses and deforms vertex positions/normals on the CPU; it has no renderer,
ECS, importer, or project-schema dependency.

Construct `VectorTrack` and `RotationTrack` with strictly increasing nonnegative
key times, then group optional translation/rotation/scale channels into one
`Track` per joint. `Clip::new` validates duration, joint indices, duplicate
tracks, event names/times, and capacities. Clips support STEP or LINEAR keys
(rotation LINEAR uses quaternion slerp). `Clip::sample` loops or clamps time and
retains the supplied rest pose for unanimated channels. Nonfinite values,
unnormalized quaternions, and singular sampled scales return errors.

`Skeleton::new(parents, inverse_bind, rest)` validates an acyclic hierarchy;
parents need not precede children. `global_matrices` composes local poses and
`skin_matrices` multiplies each global transform by its inverse bind matrix.
Matrices are in skeleton space; the host applies the entity/world transform.
`skin_vertices` evaluates normalized four-weight linear-blend skinning and uses
inverse-transpose matrices for normals. Zero-weight joint indices are ignored;
nonzero weights must address existing joints.

`Animator::new(rest)`, `play(Arc<Clip>, Playback)` and `update(dt)` provide
stateful playback. `transition` crossfades from the currently displayed pose to
the incoming clip, so interrupting a fade does not jump. The outgoing pose is
frozen during the fade; the separate pose graph runtime supplies blend trees and
state machines. Playback speed is nonnegative. Updates accept at most 60 seconds, and event traversal
rejects more than 1024 loops or 4096 emitted events in one call.

Events use (previous time, next time], including loop crossings. A zero-time
event fires when a loop wraps, not on the initial `play` call. During crossfade,
only incoming-clip events are emitted. The host owns their ECS/event routing.

Limits: up to 4096 joints, one million keys per channel, and 4096 authored events
per clip. No cubic-spline interpolation, morph-target animation, root-motion
extraction, animation graph editor, or GPU skinning is claimed.
Pose graphs, state transitions, 1D/2D blend spaces and rotation-only two-bone IK
are described in
[Animation pose graphs](../../docs/animation-graph.md).
Explicit rest-relative rotation retargeting and local root translation are described
in [Animation retargeting](../../docs/animation-retargeting.md).
A glTF adapter must explicitly reject unsupported interpolation and preserve
non-joint ancestor transforms when constructing skin palettes.

Validation: `cargo test -p engine-animation` checks interpolation, quaternion
rotation, looping/clamping, fades, event wrap, hierarchy composition, actual
vertex deformation, and malformed data.
