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

## Acoustic rendering

Stream A of [`docs/spatial-audio-design.md`](../../docs/spatial-audio-design.md)
(sections 5.4 and 7.3). The parameter contracts live in `engine_audio::acoustic`;
the DSP is private (`src/dsp/`). No external DSP crate is used: the band filter,
delay lines, Brown-Duda head model, FDN and FFT are in-house.

### Which voices are acoustic

A voice is rendered acoustically when `PlayOptions::acoustics.enabled` and
`spatial.enabled` are both true and fewer than `max_acoustic_voices` voices
(default 32, `AudioControl::set_max_acoustic_voices`, at most 256) already are.
Voices over the cap, non-spatial voices and voices with acoustics disabled use
the legacy path; capped voices are counted once each in
`AcousticMixStats::over_cap_fallbacks` (`AudioControl::acoustic_stats`).

**Legacy bit-identity.** When no voice is acoustic, no bus effect is installed
and the late field is silent, `mix` runs the original summation loop unchanged,
so output is bit-identical to the pre-acoustics mixer whatever the environment
says (renderer, reverb mode). Otherwise the mixer uses a bus-structured path in
256-frame blocks; voices with acoustics disabled still use the legacy per-voice
law there, but the summation order (per bus, then master gain) can differ in the
last bits.

**Default renderer.** `SpatialRenderer::Binaural` is the default (project
decision). It applies only to acoustic voices; `StereoPan` (the equal-power
law) stays selectable through `AudioControl::set_environment`. Switching the
renderer is not cross-faded.

### Signal flow per acoustic voice

```text
clip -> mono -+-> raw line ------(D)---------------------------> reverb send (shared late field)
              +-> BandFilter(volume x gain x direct x air) -> direct line -(D + ear delay)-> spatializer
              +-> BandFilter(early shape) -> early line -(D + tap + ear delay)-> per-tap spatializer
              +-> hero send (emission time) when the voice owns a convolution slot
```

- **Distance law:** the existing `Attenuation` law in world units. When it gives
  zero gain (always at or beyond `max_distance`) the voice is silent, including
  its taps and reverb send.
- **Propagation delay and Doppler:** `D` solves `|L(t) - S(t - D)| = c D` from the
  source velocity estimated between position updates, and ramps linearly to each
  new target over the update interval, so constant velocity gives a constant
  ratio: `c / (c - v)` for a moving source and `(c + v) / c` for a moving
  listener. `doppler` scales the change relative to the delay at play time. The
  delay is clamped to `max_delay_s`; a change steeper than 0.5 samples per sample
  (a teleport) ducks the voice for 2 x 128 frames and moves the delay while
  silent. With `propagation_delay: false` the direct delay is zero (plus a
  one-sample interpolation guard).
- **Air absorption:** amplitude `exp(-m_b d)` with `MixerEnvironment::air_np_per_m`
  in nepers per metre (energy falls as `exp(-2 m d)`).
  `acoustic::iso9613_air_np_per_m(temperature_c, humidity_percent, pressure_kpa)`
  evaluates ISO 9613-1 at the exact midband frequencies (checked against
  ISO 9613-2 Table 2); `db_per_m_to_np_per_m` converts explicit values.
- **Band filter:** an LR4 crossover tree (250 / 1000 / 4000 Hz) with allpass
  compensation, so the four band outputs share one phase. Band gains are applied
  after filtering and pre-compensated by the inverse of the centre-weight matrix,
  which makes the requested gain exact at 125 / 500 / 2000 / 8000 Hz even for
  strong steps between bands. Unity gain is an allpass.
- **Early reflections:** each published tap is read from the early line at
  `D + delay_s`, scaled by its RMS band gain and spatialized individually. The tap
  sum is coloured by one band filter whose gains are the energy-weighted mean tap
  spectrum (the design's documented approximation). Taps are matched to the
  previous frame by delay (within 0.5 ms) and glide at most 0.05 samples per
  sample; unmatched taps fade in or out over `PARAM_RAMP_FRAMES`. Thirty-two tap
  slots per voice let 16 old taps fade while 16 new ones fade in.
- **Spatializer:** `StereoPan` applies the equal-power law to the listener-frame
  direction. `Binaural` uses the Brown-Duda (1998) structural model: per-ear
  delays whose difference is Woodworth's ITD, a one-pole/one-zero head shadow
  per ear, and optional pinna echoes (`BinauralSettings::pinna_echoes`) on the
  direct path. Each ear's low-frequency gain is `1/sqrt(2)`, matching the stereo
  law's power. Taps get ear delays and head shadow, not pinna echoes.
- **Direction:** from mixer geometry, or `VoicePropagation::direction` (listener
  frame, for proxy space) blended in over `PARAM_RAMP_FRAMES`.

### Smoothing (click-free updates)

Published gains (`direct`, tap gains, reverb send) and geometry-driven gains
(distance law, air) ramp linearly over `PARAM_RAMP_FRAMES` (1024) frames, and a
ramp restarts only when its target changes. Pan, ear delays and head-shadow
coefficients ramp across each mix call. A step of the direct gain therefore
changes the output by at most `delta / 1024 x peak` per sample. With the binaural
renderer the fractional-delay Hermite read can exceed that by a few percent at
the ramp corners, and pinna echoes by up to 1.5x for two or three samples.

### Late field

- **Parametric (default):** an 8-line Householder FDN. Line lengths are distinct
  primes from 0.63x to 1.35x of `mean_free_path_m / c` (clamped to 1-30 m; 8 m when
  unset). Each line's band filter decays by `10^(-3 (L + tau_b) / (RT60_b fs))`,
  where `tau_b` is the filter's own group delay. The input passes `predelay_s`
  (at most 0.5 s) and an EQ that sets the impulse-response energy per band to
  `level_b`, relative to a unit impulse (the direct sound at 1 m). The EQ uses
  the equipartition estimate `fs RT60 / (13.82 sum L)`, accurate to about 1 dB.
  Changes ramp over 50 ms. A changed line length moves the input to the second
  network with a 50 ms cross-fade while the first rings out; it waits while that
  network is still ringing.
- **Convolution (optional, `ReverbRenderer::Convolution`):** uniform partitioned
  overlap-save with `CONVOLUTION_BLOCK` (256) and an in-house radix-2 real FFT;
  the wet path adds 256 frames of latency. `ConvolutionKernel::prepare`
  validates, linearly resamples and transforms the impulse response off the
  audio thread. `AcousticFrame::listener_kernel` renders the shared send. Up to
  `MAX_CONVOLUTION_VOICES` voices with `VoicePropagation::kernel` get their own
  slot and send at emission time, because their pair response already contains
  the propagation time. Kernel changes cross-fade over one block; an unchanged
  kernel (same `fingerprint`) does not. Frame adoption pauses until a pending
  cross-fade has run, because the old kernel lives in the fading frame. Kernels
  for another rate are ignored and counted. Without a listener kernel the shared
  send uses the FDN.
- Voice sends are scaled by voice gain, bus gain and both reverb sends, but not
  by the distance law. The late field joins the master sum before master gain.
- Voices absent from the active frame use neutral parameters (unity direct, no
  taps, full send). `ReverbParams::enabled == false` lets the late field ring
  out.

### Bus effects

`AudioControl::set_bus_effect` installs a `DspEffect` per bus. `reset` runs at
install on the control thread. `process` runs on the audio thread with blocks of
at most 256 frames, after voice and bus gains (`Master`: after the bus sum, the
late field and master gain, before the final clamp). The replaced effect is
returned for the caller to drop.

### Publisher lifecycle and real-time rules

- `AudioControl::acoustic_publisher` returns the single writer. A second call
  fails with `audio.acoustic.publisher_taken` while it lives. Dropping it
  publishes the dry state and frees the slot, so a later call succeeds (for
  example, a new runtime on the same control).
- The audio thread only `try_lock`s the parameter slot and swaps boxed frames;
  old frames and kernels return to the writer and are freed there.
- `mix` performs no allocation, blocking lock or syscall for acoustics. Voice DSP
  state is allocated by `play`/`configure`, the FDN by `acoustic_publisher` and
  the convolution bank by `set_environment`. Voice DSP state retired by the audio
  thread is parked in a preallocated list and freed by the next control call.
  Clip retirement still frees clip storage as before, and the control mutex is
  unchanged (design section 4.2).
- Memory per acoustic voice is about `12 x next_pow2(max_delay_s x rate + 300)`
  bytes (384 KiB at the 0.5 s default and 48 kHz). The FDN uses about 0.6 MiB
  and the convolution bank about 7.7 MiB at 48 kHz.
- One-shot acoustic voices stay alive until their propagation tail (delay plus
  the latest tap) has played, then retire.
- Filter states are flushed below 1e-15 once per block, so silence never runs in
  subnormal arithmetic.

### Measured acceptance (this machine, 2026-10-24)

| Design test | Suite | Result |
| --- | --- | --- |
| 1 legacy bit-identity | `acoustic_legacy` and the existing suites | golden FNV-1a hash unchanged for every renderer and reverb mode |
| 2 band filter | `acoustic_band` | centres within 0.03 dB; unity flat to 0.10 dB |
| 3 FDN RT60 | `acoustic_reverb` | T30 within 1.9 % of [2.0, 1.5, 1.0, 0.5] s |
| 4 binaural | `acoustic_binaural` | ITD 0.662 ms at +90 degrees; ILD 16.3 dB at 8 kHz and 0.3 dB at 125 Hz; frontal ITD 0 |
| 5 Doppler | `acoustic_doppler` | 1061.92 and 944.90 Hz (error below 0.001 %) |
| 6 convolution | `acoustic_convolution` | maximum relative error 2.4e-7; swap step within the one-block bound |
| 7 click-free | `acoustic_updates` | largest step within `delta / 1024 x peak` (stereo law) |
| 8 never blocked | `acoustic_updates` | 200 stalled 512-frame mixes, slowest about 1 ms, 200 try-lock misses |
| 23 DSP cost | `acoustic_cost` (release only) | static 104-114 ms, every source moving 127-135 ms (budget 150 ms; measured with other builds running) |

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

The acoustic acceptance suites are `tests/acoustic_*.rs` (design tests 1-8 and
23). The DSP cost bound is timed only in release builds:

```text
cargo test --release -p engine-audio --test acoustic_cost -- --nocapture
```

The device check fails if output cannot open; it never substitutes offline
success. It requires callback progress, consumed voice PCM, no device diagnostic,
and shutdown. Subjective audibility, loop seams and device-disconnect behavior on
physical hardware need separate listening/manual validation. The fault unit test
injects the same failure transition used by the real error callback; it does not
prove a physical disconnect occurred.
