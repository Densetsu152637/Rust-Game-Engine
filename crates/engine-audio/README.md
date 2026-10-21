# Engine audio

This crate owns immutable PCM clips, deterministic stereo mixing, real native
output, reusable source handles and ordered event subscriptions. It has no ECS
dependency. The authored runtime owns entity attachments and listener lifecycle;
the desktop host owns `AudioEngine` and shares its cloneable `AudioControl`.

```rust
use engine_audio::{AudioConfig, AudioEngine, AudioSources, PlayOptions, SourceAction, load_wav};
# fn example() -> engine_audio::Result<()> {
let engine = AudioEngine::offline(AudioConfig::default())?;
// Use AudioEngine::device(...) in a desktop host to open/start a real device.
let mut scene = AudioSources::new(engine.control(), "scene:entrance")?;
let source = scene.create(load_wav("assets/door.wav")?, PlayOptions::default())?;
scene.observe("trigger:door:enter", source, SourceAction::Play)?;
scene.notify("trigger:door:enter")?;
// Dropping `scene` stops its sources. Dropping `engine` closes the device and
// makes all remaining controls reject new playback.
# Ok(())
# }
```

`PlayOptions::validate` and `Listener::validate` allow candidate-scene validation
before playing audio. Attachments should resolve to live entity coordinates
before creating/configuring a source; invalid attachments must remove the source.
The runtime must call `reset_listener` on listener-owner destruction, scene
replacement or script faults. Independent controllers stop their own voice
handles on drop, so identical owner strings do not accidentally stop each other's
voices. `stop_owner` provides explicit bulk cleanup of direct and source voices.

`configure` replaces playback options atomically while retaining cursor/pause.
Source configuration overrides its owner with the controller's owner. Play
restarts a source at frame zero and works at the voice cap by retiring its previous
voice first; a failed restart leaves the source stopped. Notification order is
registration order. Errors stop processing with earlier observer effects retained.
Source removal also removes its observers. Handles are typed and never reused.

## Preserved engine policy

- WAV files are bounded to 64 MiB. Strict RIFF framing rejects truncation,
  duplicates and inconsistent PCM byte rates/alignment. Hound decodes accepted
  16-bit integer mono/stereo samples at 8–192 kHz; `Clip::new` also accepts validated
  finite PCM in [-1,1]. Immutable clips are validated once, not on every play.
- Output is interleaved stereo at the configured rate. Linear sample
  interpolation, pitch 1/8–8, continuous loop wrap, pause retaining cursor, and
  one-shot retirement match the existing service. Voice, source and observer caps
  bound cue spam. Master, Music, Effects and Dialogue gains multiply voice gain.
  Gains clamp to [0,1]; nonfinite gain becomes silence. Final output clamps [-1,1].
- Right-handed spatial coordinates use +X right, +Y up and forward -Z. Listener
  forward/up define the right vector. Spatial stereo downmixes to mono. Equal-power
  panning combines with None, Linear or Inverse distance attenuation; enabled
  distance attenuation is silent at/beyond max distance.
- CPAL opens the default device at the requested stereo sample rate, selecting
  F32/I16/U16 output. Unsupported configurations return explicit errors. Device
  callbacks consume the same mixer as offline rendering. Live engines reject
  offline mixing to prevent two consumers advancing one playback cursor.
- Device failure is sticky, preserves the first diagnostic, clears voices/resets
  the listener and rejects new playback. Shutdown stops the stream before clearing
  state; surviving controls reject playback. `device_frames` records actual callback
  submission, which establishes output activity but cannot establish audibility.

The mixer/control boundary uses a mutex, matching the existing service's
thread-safe behavior. Callbacks use fixed stack scratch and do not allocate output
buffers. Contention can still delay native callbacks; this implementation does
not claim hard realtime behavior. Clip retirement may free clip storage inside a
callback. A lock-free command/retirement path should be considered only after a
measured underrun problem, without changing deterministic policy.

## Dependency decision

Verified 2026-10-21 against official crate documentation:

- [CPAL 0.18.2](https://docs.rs/cpal/0.18.2/cpal/) owns native devices, negotiated
  stream formats, callback threads and sample conversions.
- [Hound 3.5.1](https://docs.rs/hound/3.5.1/hound/) owns PCM WAV sample decoding.
- [Kira 0.12.5](https://docs.rs/kira/0.12.5/kira/) offers a game audio manager,
  mixer and spatial system, but introducing it alongside the exact existing
  mixer would duplicate responsibilities. Its documented
  [MockBackend](https://docs.rs/kira/0.12.5/kira/backend/mock/struct.MockBackend.html)
  allows manually advancing processing but does not return mixed PCM, which is
  required for deterministic numerical output acceptance. Direct CPAL plus the
  bounded engine mixer is the smaller choice for preserving these contracts.

## Validation

Run `cargo test -p engine-audio` for decoder, mixing, state, spatial, source/event,
concurrent-control and fault tests. Run the real device check explicitly:

```text
cargo test -p engine-audio native_device_opens_submits_pcm_and_closes -- --ignored --nocapture
```

The device check fails if output cannot open; it never substitutes offline
success. It requires callback progress, consumed voice PCM, no device diagnostic,
and shutdown. Subjective audibility, loop seams and device-disconnect behavior on
physical hardware need separate listening/manual validation. The fault unit test
injects the same failure transition used by the real error callback; it does not
prove a physical disconnect occurred.
