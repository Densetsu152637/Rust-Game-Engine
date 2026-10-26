use crate::{Result, UplinkPacket, VoiceCodec, VoiceError, VoiceLimits};

/// Speaker-side packetizer: encodes fixed-size PCM frames into uplink packets.
pub struct VoiceSender {
    codec: Box<dyn VoiceCodec>,
    limits: VoiceLimits,
    channel: u8,
    sequence: u32,
    timestamp: u32,
    scratch: Vec<u8>,
}
impl VoiceSender {
    pub fn new(codec: Box<dyn VoiceCodec>, limits: VoiceLimits, channel: u8) -> Result<Self> {
        limits.validate()?;
        if codec.frame_samples() == 0 || codec.frame_samples() > limits.max_frame_samples {
            return Err(VoiceError::Invalid("codec frame size outside voice limits"));
        }
        Ok(Self {
            codec,
            limits,
            channel,
            sequence: 0,
            timestamp: 0,
            scratch: Vec::new(),
        })
    }
    pub fn set_channel(&mut self, channel: u8) {
        self.channel = channel;
    }
    pub fn frame_samples(&self) -> usize {
        self.codec.frame_samples()
    }
    /// Advances media time for capture samples that were not sent (mute or overflow).
    /// Sequence numbers advance only for transmitted packets.
    pub fn advance_timestamp(&mut self, samples: u32) {
        self.timestamp = self.timestamp.wrapping_add(samples);
    }
    /// Encodes one frame. `position` is an optional claim the server may ignore.
    /// Set `end_of_talkspurt` on the final frame before going silent.
    pub fn encode_frame(
        &mut self,
        pcm: &[i16],
        position: Option<[f32; 3]>,
        end_of_talkspurt: bool,
    ) -> Result<Vec<u8>> {
        self.scratch.clear();
        self.codec.encode(pcm, &mut self.scratch)?;
        let packet = UplinkPacket {
            channel: self.channel,
            sequence: self.sequence,
            timestamp: self.timestamp,
            position,
            end_of_talkspurt,
            payload: std::mem::take(&mut self.scratch),
        };
        let bytes = packet.encode(&self.limits)?;
        self.scratch = packet.payload;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(pcm.len() as u32);
        Ok(bytes)
    }
}
