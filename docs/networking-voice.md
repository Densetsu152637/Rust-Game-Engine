# Voice chat transport

`engine-net-voice` carries voice frames between session peers. It is sans-IO. It turns PCM
frames into bounded voice packets, routes them on the server, and reassembles them on
listeners through per-speaker jitter buffers with packet-loss concealment.

Ownership is split with the audio domain (program contract 4):

- **Networking (this crate)**: packet format, routing, jitter buffering and the
  concealment hooks.
- **Audio domain**: capture, real codecs (for example Opus) and spatial mixing.

Real codecs plug in through `VoiceCodec`. `RawPcmCodec` (16-bit little-endian PCM) is the
uncompressed reference and test path. [`engine-voice-audio`](voice-audio.md) supplies a
fixed 48 kHz Opus/CPAL/PCM stream bridge with explicit microphone start.

Packets travel on `VOICE_LANE` (`Lane::VOICE`, 4) with `VOICE_DELIVERY` (`Unreliable`), over
any `engine_network::MessageEndpoint`. Voice is latency-critical, so lost frames are
concealed and never retransmitted. Keep the encoded payload below the endpoint's
`max_payload(peer, Delivery::Unreliable)` value, which is about 1100–1200 bytes on QUIC. The
default `VoiceLimits::max_payload_bytes` is 1000.

## Data flow

1. **Speaker**: `VoiceSender::new(codec, limits, channel)`. Call
   `encode_frame(pcm, position, end_of_talkspurt)` once per codec frame (for example 20 ms)
   and send the returned bytes to `SERVER_PEER`.
   - The sender assigns increasing sequence numbers and sample timestamps.
   - Set `end_of_talkspurt` on the last frame before going silent, so listeners stop playout
     without concealing the silence.
2. **Server**: create a `VoiceRouter::new(limits, routing)`. `add_peer`/`remove_peer` follow
   the endpoint's `Connected`/`Disconnected` events. For each received voice payload, call
   `route(speaker_peer_id, bytes, now_ms)` and send every returned `(listener, bytes)` pair.
   - Speaker identity is the transport's authenticated peer ID. It is never read from packet
     bytes.
   - Errors are soft and per packet (`Malformed`, `RateLimited`, `Invalid`). The application
     decides whether repeated failures justify a disconnect.
3. **Listener**: `VoiceReceiver::new(limits, JitterConfig, codec_factory)`. The factory is
   fallible and creates a separate matching-profile decoder per speaker. Call
   `receive(bytes, now_ms)` for each downlink payload received from `SERVER_PEER` (the
   speaker ID inside is trusted because only the router stamps it; never feed payloads from
   other peers), and `pull(now_ms)` once per frame interval on the audio clock.
   - `pull` returns at most one `VoiceOutput { speaker, pcm, position, concealed }` per
     active speaker, with `end_of_talkspurt` set on the drained final frame.
   - The audio domain mixes and spatializes the output using `position`.

All times are caller-supplied milliseconds on one monotonic clock, which keeps tests
deterministic.
`VoiceSender::advance_timestamp(samples)` preserves elapsed media time when captured
samples are muted or dropped; it does not consume a packet sequence number.

## Routing

`VoiceRouting` decides who hears a speaker:

| Mode | Listeners |
| ---- | --------- |
| `Global` | Every other peer. |
| `Channel` | Peers that joined the packet's channel (`join_channel`/`leave_channel`); the speaker must be a member too. |
| `Proximity { max_distance }` | Peers within `max_distance` of the speaker. |
| `ChannelProximity { max_distance }` | Both conditions together. |

Positions come from the authoritative game through `set_position(peer, Some(pos))`.

- Speaker-claimed positions in uplink packets are ignored unless
  `set_trust_client_positions(true)` is set. Use that only for cooperative or trusted games,
  where the claim is used only when the server has no position.
- A speaker or listener without a known position is never in proximity range.
- Downlink packets carry the server-chosen speaker position for spatial voice only under
  `Proximity`/`ChannelProximity` routing, where every listener is already within
  `max_distance`. Under `Global`/`Channel` routing the position is omitted (`None`) unless
  `set_send_positions(true)` is called, because it would reveal every speaker's position to
  every listener, which is hidden information in most games. Even under proximity routing a
  listener learns the exact position of in-range speakers; call `set_send_positions(false)` if
  the game must not reveal it.

Other controls:

- Listeners can mute speakers with `set_muted(listener, speaker, true)`.
- Moderation uses `set_speaking_allowed(peer, false)`.
- Each speaker may forward at most `max_frames_per_second` frames per one-second window.
  Excess frames return `RateLimited` and are not forwarded.
- `RouterStats` counts received packets, forwarded copies, rate-limited packets, malformed
  packets and filtered packets.

## Jitter buffer and concealment

`JitterBuffer` (one per speaker, bounded by `max_buffered_frames`) reorders frames by
sequence and discards duplicates and late frames.

**Interarrival jitter.** The buffer estimates RFC 3550 interarrival jitter from arrival times
and sample timestamps. The target playout delay is
`clamp(frame_ms + jitter_multiplier * jitter, min_delay_ms, max_delay_ms)`. With the
defaults that is 20 ms frames, a 3× multiplier, and a delay between 20 and 300 ms.

**Adaptation during playback.** Delay is measured against the fastest observed transit:

- If jitter grows, a slot is *stretched*. One concealed slot plays without consuming a frame.
- If jitter shrinks, one frame per frame of excess delay is skipped.

**Playout lifecycle.** Playout starts once the first frame has waited the target delay, or
once half the buffer is full. It stops:

- after an `end_of_talkspurt` frame drains, or
- after `max_missing_before_stop` consecutive missing slots with an empty buffer.

**Restarts and outages.** A sequence jump of at least twice the capacity, forward or backward
(a sender restart or a long outage), resynchronises the buffer instead of discarding frames
as late forever.

**Concealment of a missing or undecodable slot**, in order:

1. The codec's own `VoiceCodec::conceal` hook (for example Opus PLC), if it returns true.
2. Otherwise the receiver's `Concealment` fallback. The default is `FadeConcealment`: it
   repeats the last frame with 6 dB attenuation per consecutive loss, and goes silent after
   `max_repeats` (3).

Replace the fallback with `VoiceReceiver::set_concealment`. `JitterStats` reports:

- received, played, concealed, late, duplicates, discarded and stretched slots;
- the jitter estimate, the target delay and the buffered frames.

## Wire format (version 1)

All fields are little-endian. Every packet has an exact length, with no trailing bytes.

```text
u8 version (1) | u8 kind (0 uplink, 1 downlink) | u8 flags (1 position, 2 end of talk spurt) | u8 channel
u32 sequence | u32 timestamp (samples)
[u64 speaker]          downlink only, stamped by the server
[f32 x3 position]      when the position flag is set
u16 payload_len | payload (1..=max_payload_bytes)
```

Decoding rejects all of the following before allocating the payload:

- unknown versions or flags, and a packet kind for the wrong direction;
- truncated packets and trailing bytes;
- zero or oversized payload lengths;
- non-finite or out-of-range (|v| > 1e12) positions.

## Limits

`VoiceLimits` defaults, validated on construction:

| Limit | Default |
| ----- | ------- |
| `max_payload_bytes` | 1000 |
| `max_frame_samples` | 2880 (60 ms at 48 kHz) |
| `max_frames_per_second` | 60 per speaker |
| `max_speakers` | 64 per receiver |
| `max_buffered_frames` | 32 per speaker |
| `max_channels_per_peer` | 16 |

Speakers beyond `max_speakers` are refused until `remove_speaker`.

## Threat model and residual risks

Peers are untrusted.

- The server derives speaker identity from the authenticated transport peer and replaces
  position claims with authoritative positions. Positions reach listeners only under
  proximity routing or explicit `set_send_positions(true)`.
- Rate limits bound forwarding amplification to about `max_frames_per_second × listeners`
  copies per speaker.
- Confidentiality and integrity come from the transport (QUIC or TLS). The server sees voice
  payloads, so there is no end-to-end voice encryption.
- A malicious speaker can send noise or replay its own frames within its rate limit. Use
  moderation (`set_speaking_allowed`) and listener mutes.
- Sequence numbers are 32-bit, which wraps after about 2.7 years of continuous 20 ms frames.
  Wraparound is not handled beyond the resynchronisation rule.

Not implemented in `engine-net-voice` itself:

- **Capture, encoding codecs and spatial mixing.** The optional `engine-voice-audio`
  bridge handles those. Echo cancellation and voice activity detection remain absent.
- **Engine-project components and MCP tools for voice.** These are tracked in the networking
  integration work.

## Verification

`cargo test -p engine-net-voice` covers the following:

- **Raw-PCM round trip**: through sender, router and receiver, bit-exact.
- **Seeded jitter and loss**: 500 frames, 10% loss and 0–60 ms jitter. Late frames stay at or
  under 2%, and concealment matches losses.
- **Concealment**: the codec-native hook takes precedence over the fade fallback, and the fade
  attenuates as specified.
- **Routing**: proximity routing uses server positions rather than client claims, `Global` and
  `Channel` routing send no speaker position (even with trusted client claims), and channel,
  mute, moderation and rate-limit behaviour all hold.
- **Bounds**: speaker and buffer bounds, and resynchronisation after a sender restart.
- **Fuzzing**: 20,000 seeded mutated or truncated packets through decoders, the router and
  the receiver, with no panics.
