# Cinematic sequences

`engine-sequencer` evaluates bounded, deterministic timelines. `engine-project::sequencer`
binds them to scene transforms and real `Camera3D` components. Author timelines as JSON in
an `engine.SequencePlayer` custom component or construct the same typed document natively.
This runtime milestone has no timeline editor UI and no dedicated sequence asset kind.

```json
{
  "engine.SequencePlayer": {
    "version": 1,
    "data": {
      "bindings": ["actor", "camera_close", "camera_wide"],
      "autoplay": true,
      "looping": false,
      "speed": 1,
      "timeline": {
        "version": 1,
        "duration": 2,
        "tracks": [
          {
            "binding": 0,
            "property": "translation",
            "channel": {
              "sampling": "linear",
              "keys": [
                {"time": 0, "value": [0, 0, 0]},
                {"time": 2, "value": [4, 0, 0]}
              ]
            }
          },
          {
            "binding": 1,
            "property": "camera_fov",
            "channel": {
              "keys": [
                {"time": 0, "value": 1.0},
                {"time": 2, "value": 1.5}
              ]
            }
          }
        ],
        "camera_cuts": [
          {"time": 0, "binding": 1},
          {"time": 1, "binding": 2}
        ],
        "events": [
          {"time": 0.5, "id": "door_opens"},
          {"time": 1, "id": "dialogue_cue"}
        ]
      }
    }
  }
}
```

Put that object in `components.custom`. Scene entities named in `bindings` must exist.
An empty binding list binds index zero to the component owner. Nonempty lists are unique
stable authored IDs. Prefab instantiation remaps this list; timeline indices do not change.
Camera bindings must have `Camera3D`; translation, rotation and scale require `Transform`.

Tracks support `translation` and `scale` as three numbers, `rotation` as an XYZW unit
quaternion, and `camera_fov` as a perspective vertical field of view in radians. Channels
use `step` or `linear` sampling (default `linear`). Transform sampling reuses the skeletal
animation clip sampler: quaternion interpolation uses normalized spherical interpolation,
and unowned transform channels remain unchanged. Scale keys must be positive, greater
than 1e-6 and at most 1,000,000. Translation components are limited to +/-1,000,000,000.
FOV must be greater than 0.01 and less than pi - 0.01. Keys clamp to their first/last value
outside the channel's key span, within the timeline duration. Exact key times select that
key. Duplicate target/property tracks and duplicate key times are rejected.

Camera cuts are sampled state, not callbacks: the latest cut at or before the current
position activates that bound camera and deactivates the other scene cameras. Reverse
playback and silent seeks therefore select the right camera immediately. Before the first
cut, captured camera activation flags apply. Cuts affect the same cameras used by normal
3D render extraction. FOV tracks require perspective projection; they never convert a
camera's projection kind.

## Controls and clocks

After `Runtime::start`, use the native world API:

```rust,ignore
use engine_project::sequencer::{self, Command, SeekEvents};
let director = runtime.entity("director").unwrap();
sequencer::command(runtime.app_mut().world_mut(), director, Command::Pause)?;
sequencer::command(runtime.app_mut().world_mut(), director,
    Command::Seek { position: 1.5, events: SeekEvents::Silent })?;
sequencer::command(runtime.app_mut().world_mut(), director, Command::Speed(-1.0))?;
sequencer::command(runtime.app_mut().world_mut(), director, Command::Play)?;
```

`status(world, owner)` reports position, `Stopped`/`Playing`/`Paused`/`Finished`, speed and
signed loop cycle. The standalone `engine_sequencer::Player` exposes the same clock controls.
Speed is finite and nonzero within -100..=100; negative speed runs backward. Set speed before
playing in reverse and seek to the desired starting position explicitly. `Play` resumes the
current position. `Pause` holds position and ownership. Natural nonloop completion holds the
endpoint in `Finished` until stopped, sought or played. `Stop` resets the clock silently to
zero and releases/restores owned fields; it does not reapply the first key. A seek after stop
reacquires ownership in `Paused`. Editing the authored component replaces its playback using
the edited autoplay/loop/speed values; native speed changes do not edit the authored component.

Runtime startup applies time zero without emitting zero-time events. Each runtime tick
advances sequences before simulation, so local animation, physics synchronization and final
skin evaluation see the current timeline pose. Network publication and world-transform refresh
follow simulation. Native commands apply transforms/cameras immediately and refresh retained
deformed geometry and world transforms before returning. This uses the simulation service's
`refresh_deformation` operation; it does not advance animation, physics or morph clocks.
The clock uses f64 seconds; transform keys and interpolation use the existing f32 animation
contract. Results are deterministic for the same operation/delta stream; floating point
addition is not claimed to be invariant under arbitrary regrouping of deltas.

## Marker crossing semantics

Markers are typed `EventCrossing` values, exposed by `SequenceEvents` with the source owner.
That resource contains crossings from the most recent prepare, tick or command; callers can
copy them before the next operation. The adapter also enqueues existing `EngineEvent`s named
`sequence.marker` with `id`, `time`, `direction` and `cycle` payload fields. These follow normal
bounded next-tick dispatch. No callback is recursively invoked while evaluating a timeline.

Forward movement crosses `(old, new]`. Reverse movement crosses `[new, old)`. Simultaneous
markers retain author order in both directions, while distinct times follow traversal order.
The departure position is excluded. At a loop seam, traversal emits the outgoing endpoint,
then enters the opposite endpoint once: forward emits duration then zero, reverse emits zero
then duration. This also applies when a delta lands exactly on the seam. The entering marker
has the new signed cycle number. Pausing or a zero delta emits nothing. `SeekEvents::Silent`
emits nothing; `Crossed` uses the same intervals within the current cycle, without wrapping.

Marker IDs are cues, not completed media operations. This milestone does not play audio,
video, voice or animation clips from cues, implement a dialogue runner, or render cinematic
movies. Such consumers must use the existing concrete audio/video/caption/animation services
and own their cancellation and synchronization. Generic marker delivery is not media playback.

## Authority and cleanup

A playing, paused or finished player acquires its target properties. Two sequences cannot
claim the same property, and only one sequence can own global camera cuts. Different transform
properties may have different sequence owners. Baselines are captured at acquisition; owners
never replace the whole Transform just to change one property.

Transform targets reject remote network replicas, physics bodies/controllers/vehicles,
cloth/soft-body writers, navigation/crowd agents, and exact existing skeletal animation or
ragdoll binding targets, including bindings from other entities. Animation and IK inside an
AnimationGraphPlayer own their bound local poses, so sequences cannot write those same
transforms. A sequence may move an external common anchor above an animated rig: its local
pose composes with the graph's child poses. SkinBinding reads transforms and writes deformed
geometry; it does not own a joint's transform. An otherwise-unowned skin joint is therefore
a valid sequence target, and final skinning sees that joint's current timeline pose in the
same runtime tick. Skin geometry is never advertised as an independently controlled clip.

Ancestors of physics/navigation/remote and ragdoll bone writers remain restricted: those
services retain world-space body, route or replica state rather than only composing local
animation beneath an anchor. Stop or remove those writers before moving their hierarchy.
Sequence owners cannot be remote replicas. Camera cut acquisition also rejects remote
cameras. Authority checks run on acquisition and every operation. If another native writer
changes an owned local property or camera activation, the sequence reports a conflict instead
of silently overwriting it. Native systems should stop the sequence before taking authority.
Future independent pose writers require explicit conflict integration.

Stopping, component removal, authored reconfiguration, scene replacement and runtime shutdown
restore surviving owned fields only when their current values still equal the sequence's last
written values and no competing known authority has appeared. Externally changed values and
new remote/physics authority are preserved. Removed/recreated entities never inherit a stale
capture. A sequence operation failure cancels all sequence captures and clears the typed crossing batch; it is not a transactional rollback of every scene service. Cleanup attempts to refresh deformation after restoring owned fields, preserves the original error, and cannot promise a refresh if a model or target is missing. Runtime
errors follow the normal runtime stop path. Mutating the scene camera set while cuts are owned
is an error: stop the sequence, modify cameras, then reacquire. Changing a binding to resolve
to a different live entity requires reauthoring/restarting; silent live rebinding is rejected.

## Floating origins and local frames

Root translation tracks retain their absolute trajectory through the
[coordinates service](world-coordinates.md), including automatic threshold shifts,
`rebase_now` and reconciled direct origin writes. Their authored keys remain f32 local
positions in the origin frame first acquired by that authored player. Subsequent samples
subtract origins first and then add the local key in f64 before one checked f32 conversion.
This ordering retains small f32 local motion when both origins are large and equal. Plain or
stale-local baselines retain their original local value and capture origin for the same reason. Stop/replay
of the same player retains that key frame; replacing its authored configuration establishes
a new frame. Each acquisition captures a fresh baseline, so Stop restores that baseline
expressed relative to the current origin. If an existing `engine.AbsolutePosition` is fresh
for that local pose, its exact f64 baseline is retained instead of reconstructing it from the
rounded Transform. A stale component uses the current local pose and applied origin.

The coordinates service notifies captures only during an actual shift and supplies the
root positions it moved. A capture accepts that shift only if the pre-shift value matched
its last write. This recognizes multiple rebases between ticks or native commands without
mistaking ordinary external writes for coordinate changes. Existing exact-position state is
guarded alongside the root Transform: samples and Stop synchronize its position and freshness
together, and legitimate coordinates-service updates are recognized through before/after
snapshots. Foreign component writes, insertion or removal reject ownership and cleanup leaves
both fields untouched. A sequence never inserts or removes an AbsolutePosition component. The notification changes neither
clock nor marker delivery and cannot fail the coordinates operation. An invalid or
unrepresentable capture instead fails the next sequence operation and follows normal
cancellation cleanup. Pending raw origin writes are not treated as already-applied shifts.
Acquiring a root translation latches its applied frame, so later direct origin writes must
be reconciled, even before the first tick.

Translation tracks on children remain parent-local and receive no origin subtraction.
Their root ancestor shifts normally. Captures record immediate parent entity identity;
reparenting or replacing that parent while a capture is active is an ownership error.
Cleanup skips fields whose parent frame changed instead of restoring a stale local baseline.
Stop and reauthor before deliberately changing a track's coordinate frame.

Only bookkeeping and origin deltas use f64. Keys, curve interpolation, written transforms
and rendered positions retain existing f32 precision. Moving the origin far from a target
can therefore round its local position; a subsequent sample reprojects the retained path,
and the baseline is not repeatedly rounded across shifts while held by a capture. No
sub-f32 precision or arbitrary-delta regrouping guarantee is claimed. Nonfinite values or
values outside finite f32 range fail explicitly rather than writing infinity. Existing exact-
position components also obey their f64 precision and absolute-coordinate bound. Their absolute
output is calculated separately: rounding below the f64 ULP cannot erase a representable local
Transform sample or a local-derived baseline.
## Limits and validation

A timeline allows 128 bindings, 256 tracks, 4096 total keys, 1024 markers and 1024 cuts, with
finite positive duration at most 1,000,000 seconds. The project supports at most 32 authored
players. Existing custom-component JSON is limited to 64 KiB; this can impose a lower practical
key limit. Every advance allows at most 64 wrap boundaries and 4096 crossings. Oversized native
advances fail atomically without changing clock state or returning a partial event list.
Project crossing batches also cap at 4096 and remain subject to the existing event queue limit
(default 1024); queue overflow cancels captures and returns an error. Split unusually large
deltas deliberately rather than relying on silent event truncation.

Focused validation commands (run serially with the project's Rust toolchain):

```text
cargo test -j1 -p engine-sequencer
cargo test -j1 -p engine-project --test sequencer --test sequencer_skinning --test sequencer_coordinates
```

Tests cover native exact keys, interpolated tracks, reverse/loop seams and ties, seek policies,
bounded failure, authored runtime transforms and render camera cuts, controls, completion,
removal/failure cleanup, external ownership, remote/physics conflicts and prefab binding remaps.
Imported-skin tests verify same-tick joint deformation, animated-rig anchor composition, direct
graph target conflicts, and seek/stop deformation with immediate render extraction and no tick.
Integration cases also cover startup failure cleanup, scene-generation rebinding and preservation
of current graph, morph and physics state during native scrubs.
Floating-origin regressions cover manual and automatic shifts during active/paused playback,
seek/Stop/replay, child-local invariance, repeated shifts and external-write/reparent conflicts.
Near-limit exact-position tests also verify sample/rebase/Stop/replay baselines, stale-component
freshness checks, and preservation of external Transform and AbsolutePosition ownership.
Large equal-origin tests retain 1e-5 local keys and small local baselines, including fresh and
stale exact-position components whose f64 absolute value cannot encode that tiny offset.
