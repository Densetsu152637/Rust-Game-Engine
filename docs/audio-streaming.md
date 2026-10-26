# Streamed PCM and source clocks

`engine-audio` supports bounded incremental PCM alongside immutable clips. The
producer may be a decoder, voice receiver or synthesizer. Producer work runs
outside the output callback. Incremental compressed decoding is described below;
microphone capture remains a separate backend.

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

## Incremental file decoding

`AudioDecoder::open(path, DecodeLimits)` probes a bounded regular local file and
reports `DecoderInfo` (detected container/codec, rate, channel count and optional
exact frame count and a separate optional estimate). It supports mono/stereo PCM WAV, native FLAC, Ogg Vorbis and MP3
through pinned [Symphonia 0.6.1](https://docs.rs/symphonia/0.6.1/symphonia/), with
only those format/codec features enabled. Existing `decode_wav`/`load_wav` retain
their strict 16-bit WAV policy. This additional decoder accepts the supported
PCM variants recognized by Symphonia and clamps finite lossy synthesis overshoot
to the engine's [-1,1] PCM range. Unknown formats/codecs and changing channel/rate
layouts return coded errors. Chained Ogg streams are rejected rather than changing
the queue format underneath a playing voice.

For caller-managed scheduling, create a queue with the reported rate/channels and
call `pump(&mut writer, generation)`. A call processes at most one demux packet,
or retries its pending PCM suffix. `Queued { frames }`, `Backpressure`, `Skipped`
and `Finished` distinguish progress. No next packet is decoded while a pending
suffix remains. The decoder retains one PCM packet and the codec's own state,
not a whole decoded file. Queue format and generation are checked before decoding.
Errors are terminal until a successful seek/reopen, and are published to the
matching stream generation. A stale generation never poisons its replacement.

`AudioDecoder::seek(frame)` resets its retained file handle and incrementally discards decoded
preroll to reach an exact sample frame. This trades seek latency for consistent
sample accuracy across formats: work proceeds one packet per pump, bounded by
the normal packet/duration budget. It clears any pending old packet. The caller
must separately call `AudioControl::seek_stream` to invalidate queued old audio
and pass its returned generation to subsequent pumps. Use both operations for
one seek; changing only the queue epoch does not reposition the decoder. File
format changes during reset fail explicitly. Seek does not resolve the path again,
so replacing the original path cannot redirect a worker to a different file. Seeking to the exact end is valid.

`DecodedStream::spawn(decoder, control, options, capacity_frames)` creates a
matching queue and owns a named decode thread. `voice()` exposes the normal
playback handle for pause/resume/configuration and source snapshots. `seek(frame)`
validates cancellation and bounds before changing the playback epoch, then
publishes a single replaceable command to the worker. Old decoder writes are
rejected by the queue generation. Use this method for worker seeks instead of
calling `seek_stream` independently. `status()` reports running, backpressure,
finished, failed or cancelled, with an error where appropriate. Finished means
input decoding ended; queued audio and acoustic tails can still be playing.

The worker parks on a full queue and wakes for seek/cancellation. It stays owned
and idle after EOF/failure so a seek can recover while the voice remains live.
`cancel()` and Drop stop its voice, set cancellation, wake it and join the thread;
they do not wait for playback to consume a full queue. Cancellation is checked
between packets and at every file read/seek. Ordinary OS file I/O or a codec call
already in progress cannot be forcibly interrupted; no wall-clock cancellation
guarantee is made for a stalled filesystem or pathological codec implementation.
No network reader or arbitrary blocking user reader is accepted by this API.

Default resource limits are:

| Resource | Default | Maximum configurable |
| --- | --- | --- |
| Encoded regular-file length | 256 MiB | 1 GiB |
| Encoded packet / codec initialization data | 256 KiB | 4 MiB |
| Decoded packet frames | 65,536 | 65,536 |
| Individual metadata tag | 64 KiB | 1 MiB |
| Metadata picture | Disabled | Disabled |
| Decoded duration | 1 hour | 24 hours |
| Demux packets, including skipped tracks | 1,000,000 | 10,000,000 |

File reads are capped at the original length, even if the file grows, and use a
fixed 64 KiB media-source buffer. Declared rate/channel/duration/packet bounds
are checked before decoder initialization where available. Codec-enriched rate,
channel and packet-frame declarations are checked again immediately afterward. Copied PCM scratch
is preallocated to `max_packet_frames * channels * sizeof(f32)`. Packet bytes
are checked after demux and decoded frame size after codec output, before growing
our scratch. Symphonia can allocate container packets, metadata logs, codec
headers and codec working buffers internally before those checks. Metadata
options limit individual tags/pictures, not every aggregate allocation. These
limits therefore bound engine-owned buffering and accepted work, and do not
establish a hard memory sandbox for hostile compressed data. An isolated decoder
process would be needed for that stronger guarantee. The upstream
[metadata limits](https://docs.rs/symphonia-core/0.6.1/symphonia_core/meta/struct.MetadataOptions.html)
and [packet-reader contract](https://docs.rs/symphonia-core/0.6.1/symphonia_core/formats/trait.FormatReader.html)
explain those boundaries.

Malformed codec packets fail instead of being silently skipped. EOF is rejected
as truncated when a declared frame count has not been decoded; truncation that a
container reports as clean EOF without a reliable frame count cannot always be
detected. `tests/decoding.rs` synthesizes genuine constant-subframe FLAC and MPEG
Layer III silence plus PCM WAV, and checks exact decoded samples/durations,
backpressure, seek generations, failure/resource limits and worker teardown.
Nonzero MP3 and Ogg Vorbis fixtures compare against independent PCM references,
including exact seeks, encoder delay/padding and truncation. The headerless VBR
regression changes bitrate after 20 MPEG frames in both directions.


Native MP3 receives a second probe over the same retained file handle with a
forward-only demux capability. This disables Symphonia 0.6.1's headerless bitrate
frame-count estimate **and** the packet trimming that otherwise uses that estimate.
The selected track, codec, sample rate and channel count must match the initial
probe. Exact Xing/Info/VBRI counts and LAME delay/padding remain available and
normal gapless decoding stays enabled. `DecoderInfo::total_frames` contains only
that exact declared count (or `None`); `estimated_total_frames` preserves the first
probe's estimate separately for display. Estimates never bound seek, trim PCM, or
establish truncation. When exact length is unknown, configured duration limits
bound seek instead. This behavior is based on the pinned upstream
[MP3 demux implementation](https://docs.rs/symphonia-bundle-mp3/0.6.1/src/symphonia_bundle_mp3/demuxer.rs.html).

Caller-driven cancellation is terminal: the next `pump` publishes a failure to
its supplied source generation, preventing already queued PCM from continuing.
Publication to an old generation is rejected by the queue and cannot fail a new
seek epoch. Dropping a worker performs its explicit stop/cancel/join lifecycle.


## Resident clips and reusable source handles

`load_resident(path, decode_limits, max_pcm_bytes)` converts supported encoded
files into a resident `Clip`, checking declared frame counts and cumulative PCM
bytes before retaining each decoded packet. It uses the same packet decode core
as streaming, but deliberately retains the full bounded clip. The maximum payload
budget is 64 MiB; packet scratch, allocator overhead and codec internals are extra.
`decode_wav` and `load_wav` retain their strict existing policy.

`AudioSources::create_stream` and `create_procedural` prepare nonresident sources
under normal `SourceHandle`s. Events, restart, pause/resume, configure and owner
cleanup use the same controller as resident clips. Queue capacity is limited to
2..65,536 frames at this source layer. Nonresident looping returns
`audio.source.loop` before file probing/producer allocation. Call `refresh` outside
the callback to reclaim workers whose voices finished; project synchronization
performs that cleanup. Authored selection is described in audio-procedural.md.
