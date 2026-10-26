# Procedural audio graphs

`engine_audio::graph` compiles shared `engine_graph::GraphDocument` assets of kind
`audio` into executable mono-signal DSP with a stereo output. The native DSP layer and authored `engine.AudioEmitter` use the same source
handles, events, attachment synchronization and acoustic enumeration. The project
graph registry must include `graph::kind()` (the host integration registration).

Call `graph::kind()` for the common editor/tool registry. `graph::compile` runs
complete shared validation and domain checks and returns an immutable
`Arc<CompiledGraph>` or located graph diagnostics. Every node must contribute to
exactly one stereo `output`; directed cycles, including through a delay node,
are rejected. Graph variables are Float values with finite numeric defaults.
There is no second document format or independent graph editor.

| Node | Signal behavior |
| --- | --- |
| constant | Authored numeric value |
| parameter | Named graph variable, with native-controlled ramped values |
| oscillator | Sine, triangle, saw or square; optional frequency signal |
| noise | Seeded xorshift32 samples in [-1,1] |
| mix | Sum of `a` and `b` |
| gain | `input` multiplied by optional gain signal or authored gain |
| adsr | Linear attack/decay/sustain/release; gate above 0.5 triggers |
| lowpass | One-pole low-pass; optional cutoff signal |
| delay | Integer mono delay with internal feedback in [-0.99,0.99] |
| output | Required left/right signals, each clipped to [-1,1] |

Outputs use `audio.signal` ports named `signal`; connecting one mono source to
both output inputs duplicates it into stereo. ADSR is an envelope signal: connect
it to a gain input to shape audio. An attack starts from the current level,
reaches one, decays to sustain, then releases from the current level on gate fall.
Zero-length stages transition immediately. Delay outputs its previous buffer
sample and stores `input + feedback * previous`; feedback is local state, never
a graph cycle. Delay length rounds to the nearest sample, with a one-sample minimum.

`GraphInstance::new(program, sample_rate)` allocates independent voice state.
`render(&mut [[f32;2]])` produces actual PCM, preserving phase/filter/envelope/delay
state across calls. `generated_frames()` counts generated source frames.
`set_parameters` atomically validates a batch of named values and ramp lengths.
A ramp advances on each generated sample and lands exactly on its target.
`reset` restores authored defaults, phase, noise seeds, envelope/filter/delay state
and the generated frame counter without reallocating buffers. Successful rendering
allocates nothing. An invalid signal silences the whole requested valid-sized
block and latches failure until reset. Oversized render requests are rejected
before touching the caller buffer.

`ProceduralStream::spawn(program, audio_control, play_options, capacity_frames,
duration_frames)` owns a producer and ordinary bounded PCM voice at the mixer's
sample rate. It renders 256-frame blocks off the audio thread and retains a
partial block on backpressure. The callback uses the existing streaming path;
no graph traversal, graph compilation, synthesis, control wait or new allocation
is added to it. Existing mixer mutex/realtime limitations still apply.

`set_parameters` coalesces pending values into 64 fixed slots, keeping only the
latest update for each variable. Values already generated or queued stay intact,
so audible updates can lag by the queue capacity plus one 256-frame producer
block, plus the requested ramp. Changes are applied on the producer thread and
do not restart oscillator phase. Unknown variables, nonfinite/excessive values,
duplicate entries or oversized batches fail atomically. Update storms do not grow
a command queue. Native callers can use `AudioControl::pause/resume/configure`
with `voice()`; pause retains queued PCM and source time while the bounded producer
can fill the queue and park.

`reset()` clears queued old PCM via `seek_stream`, publishes a new source epoch,
and restores DSP/default parameters on the producer. Pending old updates are
cleared; updates posted after reset apply to its new generation. For deterministic
initial controls, `reset_with_parameters(batch)` validates and publishes reset plus
overrides atomically, applying them before its first generated sample. Invalid
batches leave the old epoch/queued PCM intact. Separate reset and update calls can
allow a default-valued producer block between them. Stale producer
writes and status reports cannot overwrite the new epoch. Reset preserves the
voice's pause state. `finish()` flushes any already-generated pending block and
closes input; optional `duration_frames` closes input at that exact generated
frame count. Neither automatically waits for an ADSR release: gate the envelope
and allow enough time before finishing when a release tail is wanted. EOF drains
through the existing mixer/acoustic tail behavior. A retired voice cannot reset;
spawn a new stream. `stop()` and Drop stop the voice, wake and join the producer
without waiting for a full queue to drain.

`status()` distinguishes running, backpressure, finished, failed and stopped,
with generated frames and a source generation. Generated frames include producer
lead. They are not the mixer-consumed source clock; read
`AudioControl::stream_snapshot(voice)` for that clock while the voice is live.
Hardware presentation remains unavailable. Finished means generation ended; queued
PCM and acoustic tails may still be audible. Workers remain owned/idle after EOF
or failure until stopped/dropped, allowing reset while the voice still exists.

Bounds are 256 nodes, 1,024 edges, 64 variables, 4,096 offline frames per render,
64 updates per batch and at most 65,536 frames per parameter ramp. Delay is at
most two seconds per node and 1,048,576 aggregate mono frames (four MiB) per
instance. Playback finite durations are capped at 24 hours. Internal signal
magnitude is bounded to 1,000,000; excessive/nonfinite math fails explicitly.
Gain must stay within [-16,16]; oscillator frequency within 0..0.45 of sample
rate; filter cutoff within 1..0.45 of sample rate. Authored defaults that violate
these rate-dependent bounds fail on rendering. Controls can also trigger those
checks; invalid DSP never enters the source queue.

Waveforms use direct formulas without oversampling or band limiting; saw/square
and high harmonics of triangle alias. The filter is one-pole, the delay has no
fractional interpolation, and the envelope/control ramps are linear. The seeded
noise is deterministic, not a cryptographic generator. Matching instance/reset
and block partitions reproduce output on the same build/platform; transcendental
math is not promised bit-identical across architectures. No perceptual quality,
hard realtime deadline or measured CPU throughput is claimed.

`tests/procedural.rs` checks actual PCM frequency, deterministic partitions and
independent voices, ramp/mix/gain/noise, ADSR stages, filter step response,
delay/feedback impulses, graph/math/storage/update bounds, and streamed pause,
epoch reset, audible control updates, finish/EOF/source clocks and teardown.


## Authored sources and the shared controller

`engine.AudioEmitter` selects exactly one nonempty `clip` (an existing `audio`
asset) or `graph` (an existing `graph` asset whose kind is `audio`). No second
emitter component or asset kind is introduced. `streamed: true` selects bounded
incremental file decoding; false selects resident PCM loaded on demand. The
existing `autoplay`, bus, gain, pitch, spatial, play/stop event and owner teardown
behavior applies to every mode. Stream/graph sources reject `looped: true`
explicitly before allocating producer resources; use resident clips for looping.
There is no silently ignored looping option.

A graph may supply `parameters` (up to 64 finite Float values) and optional
`duration_frames`; both are rejected on file emitters. Initial overrides are
validated and applied before the first generated sample. `queue_frames` defaults
to 4,096 and is bounded to 2..65,536. `graph` cannot coexist with `clip` or
`streamed: true`. Empty catalog defaults remain valid placeholders, but runtime
startup requires a real selected source and valid asset reference.

`AudioSources::create` retains its resident `Arc<Clip>` API. `create_stream` and
`create_procedural` use the same `SourceHandle`, observer table and `SourceAction`
lifecycle. Play retires the previous voice/worker before allocating a replacement;
a failed restart leaves the source stopped. Configure and pause/resume preserve
source time. `seek` addresses decoded frames or procedural reset at frame zero;
`set_parameters` validates and updates procedural controls. Source `refresh`
reclaims finished producer threads outside callbacks while retaining reusable
handles and subscriptions; the project calls it during audio synchronization.
Removal, scene teardown and stop drop/join owned producers.

`engine_project::audio::{source, stream_source, procedural_source}` all attach
through `EntityAudio`. The existing transform, event and `engine.AcousticSource`
paths therefore include decoded/procedural voices without a second propagation
registry. `seek_source` and `set_parameters` expose their native controls.

Resident decode is intentionally whole-clip loading and is never advertised as
streaming. `load_resident` shares the packet decoder but checks declared and
cumulative PCM bytes before reserving/retaining each packet. Its payload budget
is configurable up to 64 MiB; project resident sources use 64 MiB per clip and a
128 MiB owned cache. Decoder packet scratch and codec internals remain additional
memory as described in audio-streaming.md; these are not process quotas. Unused
and streamed assets are not eagerly decoded into the resident cache. Native
`load_clip` may return an uncached bounded clip, while registering a resident
source retains it in the project cache. Strict native WAV APIs are unchanged.

Asynchronous decoder/DSP diagnostics are retained by `AudioSources::failure`
even after `refresh` reclaims a finished worker. A replay clears the prior
failure; prepare/restart errors also return directly. Project synchronization
surfaces worker failures as coded runtime errors and follows normal runtime
teardown, preventing malformed streaming input from silently appearing as a
normal stopped source.

### Failure lifetime

Both producer wrappers retain a read-only queue observer independently of mixer
voice lifetime. `DecodedStream::failure()` and `ProceduralStream::failure()` read
that generation-scoped terminal diagnostic even when cancellation later changes
worker status. Source refresh cancels/joins outside the callback, then collects
the durable queue error (or join failure) before dropping the worker. It does not
rely on the worker having reached its later status publication. Seek/reset clear
queue errors through the existing generation fence; stale failures cannot poison
a replacement epoch. The unit regression pauses each producer after queue failure
but before status publication, retires the mixer voice, and checks refresh/join
retains the error while offline mixing continues independently.
