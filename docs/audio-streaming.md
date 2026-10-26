# Streamed PCM and source clocks

`engine-audio` supports bounded incremental PCM alongside immutable clips. The
producer may be a decoder, voice receiver or synthesizer. Producer work runs
outside the output callback. This API does not yet include a compressed decoder
or microphone backend.

```rust
use engine_audio::{AudioEngine, AudioConfig, StreamConfig, PlayOptions, StreamGeneration};
let mut audio = AudioEngine::offline(AudioConfig::default())?;
let (voice, mut producer) = audio.play_stream(StreamConfig {
    sample_rate: 48_000, channels: 1, capacity_frames: 4_800,
}, PlayOptions::default())?;
let generation = StreamGeneration(0);
let accepted_frames = producer.write(generation, &[0.25, 0.5, 0.25])?;
assert_eq!(accepted_frames, 3);
producer.finish(generation)?;
let mut stereo = [0.0; 6];
audio.mix(&mut stereo)?;
# Ok::<(), engine_audio::AudioError>(())
```

The ring allocates once at construction, accepts mono/stereo finite `f32` PCM
in [-1,1], and holds 2 through 1,048,576 frames. Its size never grows. `write`
returns accepted **frames**, so the producer retains and retries the unaccepted
suffix (multiply by channel count to find its sample offset). Zero means
backpressure. The mixer retains interpolation lookahead until it is no longer
needed, so a queue can remain full until the next render. Applications own
producer scheduling, cancellation checks and bounded decoder scratch memory.

Linear interpolation spans producer writes and physical ring wrap. The source
sample-rate/output-rate ratio and `PlayOptions.pitch` determine cursor advance.
Missing PCM produces silence and holds source time; missing fractional lookahead
also waits unless EOF establishes that the final sample should be held. `finish`
(or dropping the sole producer) closes input and drains the final frame. Failure
through `fail` stops source input immediately and preserves the diagnostic.
Streams use existing gains, spatial downmix/panning, acoustic DSP and buses.
Acoustic delay tails drain after EOF; temporary starvation does not end a tail.
Looped stream options are rejected: loops require explicit producer seek/replay.

`pause` preserves queue and source time. `seek_stream(voice, source_frame)` clears
queued PCM, resets per-voice acoustic delay/filter state and clock counters, and
returns a new `StreamGeneration`. It preserves pause and playback options. Every
write/finish/fail carries a generation token, so old decode results cannot enter
the new timeline. The caller must reposition its decoder to the requested source
frame. Frames above 2^52 are rejected to retain exact adjacent-frame coordinates.
Seeking must happen before the voice retires at EOF. Start a new stream to replay
a retired voice. Stop, owner teardown and shutdown invalidate surviving writers.

`StreamWriter::snapshot` remains available after voice retirement, while
`AudioControl::stream_snapshot` returns `None` for a retired or clip voice.
Snapshots expose the epoch, fractional source frames/seconds, actual PCM output
frames submitted in that epoch, queue capacity/occupancy, underrun silence count,
state and producer error. Source frames can advance past the latest buffered
frame by one resampling step; later input fills/skips that source interval. EOF
clamps the final cursor to the input end. Submitted counts exclude underrun
silence and pause. Silence contained in actual PCM still counts as submitted.

`source_seconds` is a mixer-consumption clock. It does not establish when sound
reaches a speaker. `presentation_seconds` is always `None`: CPAL submission,
hardware buffering, acoustic propagation delay and device latency have not been
converted into a source presentation estimate. Video/caption consumers must
choose a clock policy explicitly, retain generation changes and handle starvation
and pause. A global `device_frames` count cannot replace this per-source clock.

Each stream render takes its queue with `try_lock` once per render block; producer
contention emits silence and increments underruns. No decoding, producer wait or
per-frame allocation occurs there. Existing outer mixer/control mutex behavior
remains documented in the crate README; this change does not establish a hard
realtime device guarantee. Queue storage may be freed when the last handle dies.
Seek recreates DSP state on the control thread. Shared late reverb from earlier
input can continue ringing; seek does not clear the entire mixer's reverb buses.

Run `cargo test -p engine-audio` and
`cargo clippy -p engine-audio --all-targets -- -D warnings`. `tests/streaming.rs`
checks rendered PCM, chunk boundaries, rate/pitch clocks, starvation, pause,
backpressure, seeks, failure, EOF, teardown and acoustic source integration. The
queue unit test holds its producer lock while invoking render to establish the
nonblocking contention path. Device audibility and hardware timing require
separate real-device validation.
