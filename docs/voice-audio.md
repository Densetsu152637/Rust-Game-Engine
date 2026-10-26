# Native voice audio bridge

`engine-voice-audio` joins `engine-net-voice` to a native microphone and the ordinary
`engine-audio` PCM stream. Constructing `CaptureSession` does not open a device. The
application must explicitly call `start`; `stop`, mute and drop close capture and
clear held microphone samples. Send packets returned by `pump` or `stop` through
the authenticated voice lane to `SERVER_PEER`. The bridge does not send packets.

The negotiated profile is fixed: mono 48 kHz, 960 samples (20 ms) per frame,
Opus VoIP at 32 kbit/s. `OpusCodec` owns one stateful encoder or decoder.
`payload_cap(endpoint.max_payload(SERVER_PEER, VOICE_DELIVERY))` takes the smaller
of 1000 bytes and the endpoint budget minus 34 bytes for the largest positioned
downlink header. Both peers must use this profile. No codec negotiation, in-band
FEC, VAD, acoustic echo cancellation, noise suppression or automatic gain control
is provided. Headphones are advisable when voice output could reach the mic.

`opusic-c` 1.6.1 bundles libopus 1.6.1 through `opusic-sys`; native compilation
requires CMake and a C compiler. The bridge uses CPAL 0.18.2. Input support is
F32, I16 and U16 mono/stereo at 8–48 kHz. It prefers 48 kHz and downmixes
stereo by averaging channels. A stateful rational linear interpolator converts
other rates to 48 kHz on the pump thread. Linear interpolation is simple and
partition invariant but does not provide a strong anti-alias filter on downsampling.

The input callback never encodes or sends. It try-locks a preallocated 9600-frame
(up to 200 ms) ring once; contention and overflow drop input while retaining a
monotonic sample clock. A pump returns at most eight packets. Drop statistics are
available from `CaptureSession::stats`. Muting and stop discard queued and partial
PCM; the caller may send the returned encoded silence end packet to terminate the
remote talkspurt. Resume explicitly reopens the microphone and advances media
time for elapsed silence. Application code should pump regularly, normally at
20 ms intervals. No raw microphone audio is written to files by this crate.

Construct `VoicePlayback` with an `AudioControl`, endpoint budget and speaker cap.
Call `receive_server_downlink` only for payloads from authenticated `SERVER_PEER`;
the function validates packet shape but cannot authenticate transport origin.
Call `tick` with monotonically increasing milliseconds at approximately 20 ms
intervals, independent of the output stream's clock. It decodes per speaker,
writes mono PCM to bounded `play_stream` queues, and uses server-supplied positions
through `AudioControl::configure`. It retains at most one pending frame suffix per
speaker under backpressure; later frames can be dropped. End packets finish a
stream after pending PCM drains. Idle, timeout, mute, disconnect and drop release
speaker streams and decoder state. The server controls whether downlink positions
are disclosed; see [networking-voice.md](networking-voice.md).

The bridge does not create `engine-project` components or start the microphone
automatically. Applications own microphone consent, packet transport, listener
state, and calling the pump/tick methods. The bridge does not persist captured
audio or publish it to a network without application action.
