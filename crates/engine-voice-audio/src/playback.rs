use crate::{BridgeError, FRAME_SAMPLES, OpusCodec, Result, SAMPLE_RATE, payload_cap};
use engine_audio::{
    AudioControl, Bus, PlayOptions, StreamConfig, StreamGeneration, StreamSnapshot, StreamState,
    StreamWriter, VoiceHandle,
};
use engine_net_voice::{DownlinkPacket, JitterConfig, VoiceCodec, VoiceLimits, VoiceReceiver};
use engine_network::PeerId;
use std::collections::{BTreeMap, BTreeSet};

const STREAM_FRAMES: usize = FRAME_SAMPLES * 4;
const TIMEOUT_MS: f64 = 2_000.;
const MAX_TICKS_PER_CALL: usize = 8;
const MAX_BRIDGE_SPEAKERS: usize = 64;
struct SpeakerStream {
    handle: VoiceHandle,
    writer: StreamWriter,
    options: PlayOptions,
    pending: [f32; FRAME_SAMPLES],
    offset: usize,
    len: usize,
    closing: bool,
    last_seen_ms: f64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    pub frames_written: u64,
    pub frames_dropped: u64,
    pub ticks_skipped: u64,
}

/// Routes authenticated server downlinks through per-speaker Opus decoders to
/// ordinary bounded mono PCM streams. Caller supplies a monotonic 20 ms clock.
pub struct VoicePlayback {
    audio: AudioControl,
    receiver: VoiceReceiver,
    limits: VoiceLimits,
    streams: BTreeMap<PeerId, SpeakerStream>,
    muted: BTreeSet<PeerId>,
    all_muted: bool,
    last_tick_ms: Option<f64>,
    stats: PlaybackStats,
}
impl VoicePlayback {
    pub fn new(
        audio: AudioControl,
        endpoint_unreliable_payload: usize,
        max_speakers: usize,
    ) -> Result<Self> {
        if !(1..=MAX_BRIDGE_SPEAKERS).contains(&max_speakers) {
            return Err(BridgeError::Invalid("max_speakers must be in 1..=64"));
        }
        let cap = payload_cap(endpoint_unreliable_payload)?;
        let limits = VoiceLimits {
            max_payload_bytes: cap,
            max_frame_samples: FRAME_SAMPLES,
            max_speakers,
            ..VoiceLimits::default()
        };
        let receiver = VoiceReceiver::new(
            limits.clone(),
            JitterConfig::default(),
            Box::new(move || Ok(Box::new(OpusCodec::decoder(cap)?) as Box<dyn VoiceCodec>)),
        )?;
        Ok(Self {
            audio,
            receiver,
            limits,
            streams: BTreeMap::new(),
            muted: BTreeSet::new(),
            all_muted: false,
            last_tick_ms: None,
            stats: PlaybackStats::default(),
        })
    }
    pub fn stats(&self) -> PlaybackStats {
        self.stats
    }
    pub fn active_streams(&self) -> usize {
        self.streams.len()
    }
    pub fn stream_snapshot(&self, speaker: PeerId) -> Result<Option<StreamSnapshot>> {
        match self.streams.get(&speaker) {
            Some(stream) => Ok(Some(stream.writer.snapshot()?)),
            None => Ok(None),
        }
    }
    /// Supply only packets whose transport peer is the authenticated SERVER_PEER.
    /// This method does not authenticate packet origin itself.
    pub fn receive_server_downlink(&mut self, bytes: &[u8], now_ms: f64) -> Result<()> {
        if !now_ms.is_finite() || now_ms < 0. {
            return Err(BridgeError::Invalid("invalid monotonic time"));
        }
        let packet = DownlinkPacket::decode(bytes, &self.limits)?;
        if self.all_muted || self.muted.contains(&packet.speaker) {
            return Ok(());
        }
        if self.streams.get(&packet.speaker).is_some_and(|s| s.closing) {
            self.remove_speaker(packet.speaker);
        }
        self.receiver.receive(bytes, now_ms)?;
        if let Some(stream) = self.streams.get_mut(&packet.speaker) {
            stream.last_seen_ms = now_ms;
        }
        Ok(())
    }
    pub fn tick(&mut self, now_ms: f64) -> Result<()> {
        if !now_ms.is_finite() || now_ms < 0. {
            return Err(BridgeError::Invalid("invalid monotonic time"));
        }
        let first = self.last_tick_ms.get_or_insert(now_ms - 20.);
        if now_ms < *first {
            return Err(BridgeError::Invalid("voice clock moved backwards"));
        }
        let available = ((now_ms - *first) / 20.).floor() as usize;
        let ticks = available.min(MAX_TICKS_PER_CALL);
        if available > ticks {
            self.stats.ticks_skipped += (available - ticks) as u64;
            *first = now_ms - ticks as f64 * 20.;
        }
        for _ in 0..ticks {
            let tick_ms = self.last_tick_ms.unwrap_or(now_ms) + 20.;
            self.last_tick_ms = Some(tick_ms);
            self.flush_pending()?;
            for frame in self.receiver.pull(tick_ms) {
                if self.all_muted || self.muted.contains(&frame.speaker) {
                    continue;
                }
                if !self.streams.contains_key(&frame.speaker) {
                    let mut options = PlayOptions {
                        bus: Bus::Dialogue,
                        owner: format!("voice:{}", frame.speaker),
                        ..PlayOptions::default()
                    };
                    if let Some(position) = frame.position {
                        options.spatial.enabled = true;
                        options.spatial.position = position;
                    }
                    let (handle, writer) = self.audio.play_stream(
                        StreamConfig {
                            sample_rate: SAMPLE_RATE,
                            channels: 1,
                            capacity_frames: STREAM_FRAMES,
                        },
                        options.clone(),
                    )?;
                    self.streams.insert(
                        frame.speaker,
                        SpeakerStream {
                            handle,
                            writer,
                            options,
                            pending: [0.; FRAME_SAMPLES],
                            offset: 0,
                            len: 0,
                            closing: false,
                            last_seen_ms: tick_ms,
                        },
                    );
                }
                let Some(stream) = self.streams.get_mut(&frame.speaker) else {
                    continue;
                };
                if let Some(position) = frame.position {
                    if !stream.options.spatial.enabled
                        || stream.options.spatial.position != position
                    {
                        stream.options.spatial.enabled = true;
                        stream.options.spatial.position = position;
                        self.audio
                            .configure(stream.handle, stream.options.clone())?;
                    }
                }
                if stream.len > stream.offset {
                    self.stats.frames_dropped += 1;
                } else {
                    for (dst, src) in stream.pending.iter_mut().zip(frame.pcm) {
                        *dst = f32::from(src) / 32768.;
                    }
                    stream.offset = 0;
                    stream.len = FRAME_SAMPLES;
                    self.stats.frames_written += 1;
                }
                if frame.end_of_talkspurt {
                    stream.closing = true;
                }
            }
            self.flush_pending()?;
            let expired: Vec<_> = self
                .streams
                .iter()
                .filter(|(id, stream)| {
                    (now_ms - stream.last_seen_ms > TIMEOUT_MS
                        || self.receiver.speaker_idle(**id) && stream.len == 0)
                        && !stream.closing
                })
                .map(|(&id, _)| id)
                .collect();
            for id in expired {
                self.remove_speaker(id);
            }
            let drained: Vec<_> = self
                .streams
                .iter()
                .filter_map(|(&id, stream)| {
                    (stream.closing
                        && stream.len == 0
                        && stream.writer.snapshot().map_or(true, |s| {
                            matches!(
                                s.state,
                                StreamState::Drained | StreamState::Stopped | StreamState::Failed
                            )
                        }))
                    .then_some(id)
                })
                .collect();
            for id in drained {
                self.remove_speaker(id);
            }
        }
        Ok(())
    }
    fn flush_pending(&mut self) -> Result<()> {
        for stream in self.streams.values_mut() {
            if stream.len > stream.offset {
                let accepted = stream.writer.write(
                    StreamGeneration(0),
                    &stream.pending[stream.offset..stream.len],
                )?;
                stream.offset += accepted;
                if stream.offset == stream.len {
                    stream.offset = 0;
                    stream.len = 0;
                }
            }
            if stream.closing && stream.len == 0 {
                // Idempotent finish is allowed by the PCM stream's contract.
                stream.writer.finish(StreamGeneration(0))?;
            }
        }
        Ok(())
    }
    /// The mute list is session-scoped and bounded by the negotiated speaker cap.
    pub fn mute_speaker(&mut self, speaker: PeerId, muted: bool) -> Result<()> {
        if muted {
            if !self.muted.contains(&speaker) && self.muted.len() >= self.limits.max_speakers {
                return Err(BridgeError::Invalid("voice mute cap reached"));
            }
            self.muted.insert(speaker);
            self.remove_speaker(speaker);
        } else {
            self.muted.remove(&speaker);
        }
        Ok(())
    }
    pub fn set_local_muted(&mut self, muted: bool) {
        self.all_muted = muted;
        if muted {
            self.stop_all();
        }
    }
    pub fn remove_speaker(&mut self, speaker: PeerId) {
        self.receiver.remove_speaker(speaker);
        if let Some(stream) = self.streams.remove(&speaker) {
            self.audio.stop(stream.handle);
        }
    }
    pub fn disconnect(&mut self) {
        self.stop_all();
        self.muted.clear();
        self.all_muted = false;
    }
    fn stop_all(&mut self) {
        let speakers: Vec<_> = self.receiver.speakers().collect();
        for speaker in speakers {
            self.remove_speaker(speaker);
        }
        for (_, stream) in std::mem::take(&mut self.streams) {
            self.audio.stop(stream.handle);
        }
        self.last_tick_ms = None;
    }
}
impl Drop for VoicePlayback {
    fn drop(&mut self) {
        self.disconnect();
    }
}
