use crate::{
    Concealment, DownlinkPacket, FadeConcealment, JitterBuffer, JitterConfig, JitterStats, Playout,
    Result, VoiceCodec, VoiceError, VoiceLimits,
};
use engine_network::PeerId;
use std::collections::BTreeMap;

/// Creates one decoder per speaker.
pub type CodecFactory = Box<dyn Fn() -> Result<Box<dyn VoiceCodec>> + Send>;

/// One decoded (or concealed) frame for the audio mixer.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceOutput {
    pub speaker: PeerId,
    pub pcm: Vec<i16>,
    /// Server-provided speaker position for spatial voice (audio domain spatializes).
    pub position: Option<[f32; 3]>,
    /// True when the frame was synthesized by packet-loss concealment.
    pub concealed: bool,
    /// The final frame of a talkspurt has left the jitter buffer.
    pub end_of_talkspurt: bool,
}

struct Speaker {
    buffer: JitterBuffer,
    codec: Box<dyn VoiceCodec>,
    last: Vec<i16>,
    position: Option<[f32; 3]>,
    missing_run: u32,
}

/// Listener-side voice receiver with per-speaker jitter buffers and loss concealment.
pub struct VoiceReceiver {
    limits: VoiceLimits,
    jitter: JitterConfig,
    factory: CodecFactory,
    concealment: Box<dyn Concealment>,
    speakers: BTreeMap<PeerId, Speaker>,
    decode_errors: u64,
    sample_rate: u32,
    frame_samples: usize,
}
impl VoiceReceiver {
    pub fn new(limits: VoiceLimits, jitter: JitterConfig, factory: CodecFactory) -> Result<Self> {
        limits.validate()?;
        jitter.validate()?;
        let probe = factory()?;
        if probe.frame_samples() == 0 || probe.frame_samples() > limits.max_frame_samples {
            return Err(VoiceError::Invalid("codec frame size outside voice limits"));
        }
        if probe.sample_rate() == 0 {
            return Err(VoiceError::Invalid("codec sample rate is zero"));
        }
        let sample_rate = probe.sample_rate();
        let frame_samples = probe.frame_samples();
        Ok(Self {
            limits,
            jitter,
            factory,
            concealment: Box::new(FadeConcealment::default()),
            speakers: BTreeMap::new(),
            decode_errors: 0,
            sample_rate,
            frame_samples,
        })
    }
    /// Replaces the fallback used when the codec has no native concealment.
    pub fn set_concealment(&mut self, concealment: Box<dyn Concealment>) {
        self.concealment = concealment;
    }
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }
    pub fn speakers(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.speakers.keys().copied()
    }
    pub fn stats(&self, speaker: PeerId) -> Option<JitterStats> {
        self.speakers.get(&speaker).map(|s| s.buffer.stats())
    }
    pub fn remove_speaker(&mut self, speaker: PeerId) {
        self.speakers.remove(&speaker);
    }
    /// True once a speaker's buffered talkspurt has stopped.
    pub fn speaker_idle(&self, speaker: PeerId) -> bool {
        self.speakers
            .get(&speaker)
            .is_some_and(|s| !s.buffer.is_playing() && s.buffer.stats().buffered_frames == 0)
    }

    /// Accepts one downlink payload that arrived at `now_ms`. Pass only payloads received from
    /// `SERVER_PEER`: the speaker ID inside is trusted because the router stamps it. New
    /// speakers beyond `max_speakers` are refused (`Invalid`) until one is removed.
    pub fn receive(&mut self, bytes: &[u8], now_ms: f64) -> Result<()> {
        let packet = DownlinkPacket::decode(bytes, &self.limits)?;
        if !self.speakers.contains_key(&packet.speaker) {
            if self.speakers.len() >= self.limits.max_speakers {
                return Err(VoiceError::Invalid("voice speaker cap reached"));
            }
            let codec = (self.factory)()?;
            if codec.sample_rate() != self.sample_rate
                || codec.frame_samples() != self.frame_samples
            {
                return Err(VoiceError::Invalid("decoder profile differs from receiver"));
            }
            let buffer = JitterBuffer::new(
                self.jitter,
                self.limits.max_buffered_frames,
                codec.sample_rate(),
            )?;
            self.speakers.insert(
                packet.speaker,
                Speaker {
                    buffer,
                    codec,
                    last: Vec::new(),
                    position: None,
                    missing_run: 0,
                },
            );
        }
        if let Some(speaker) = self.speakers.get_mut(&packet.speaker) {
            speaker.buffer.push(packet, now_ms);
        }
        Ok(())
    }

    /// Produces at most one frame per active speaker; call once per frame interval.
    pub fn pull(&mut self, now_ms: f64) -> Vec<VoiceOutput> {
        let mut out = Vec::new();
        for (&id, speaker) in &mut self.speakers {
            let frame_samples = speaker.codec.frame_samples();
            let Some(playout) = speaker.buffer.pop(now_ms) else {
                continue;
            };
            let mut pcm = Vec::with_capacity(frame_samples);
            let mut concealed = false;
            let mut end_of_talkspurt = false;
            match playout {
                Playout::Frame(packet) => {
                    end_of_talkspurt = packet.end_of_talkspurt && !speaker.buffer.is_playing();
                    speaker.position = packet.position;
                    if speaker.codec.decode(&packet.payload, &mut pcm).is_ok()
                        && pcm.len() == frame_samples
                    {
                        speaker.missing_run = 0;
                    } else {
                        self.decode_errors += 1;
                        pcm.clear();
                        concealed = true;
                    }
                }
                Playout::Missing => concealed = true,
            }
            if concealed {
                speaker.missing_run = speaker.missing_run.saturating_add(1);
                if !speaker.codec.conceal(&mut pcm) || pcm.len() != frame_samples {
                    pcm.clear();
                    self.concealment.conceal(
                        &speaker.last,
                        speaker.missing_run,
                        frame_samples,
                        &mut pcm,
                    );
                }
                pcm.resize(frame_samples, 0);
            }
            speaker.last.clone_from(&pcm);
            out.push(VoiceOutput {
                speaker: id,
                pcm,
                position: speaker.position,
                concealed,
                end_of_talkspurt,
            });
        }
        out
    }
}
