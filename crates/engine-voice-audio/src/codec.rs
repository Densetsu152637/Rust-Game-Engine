use engine_net_voice::{VoiceCodec, VoiceError};
use opusic_c::{Application, Bitrate, Channels, Decoder, Encoder, SampleRate};

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 960;

enum State {
    Encoder(Encoder),
    Decoder(Decoder),
}

/// Fixed mono 48 kHz / 20 ms, 32 kbit/s VoIP profile. Each speaker owns a decoder.
pub struct OpusCodec {
    state: State,
    max_payload: usize,
    input: [f32; FRAME_SAMPLES],
    output: [f32; FRAME_SAMPLES],
}
impl OpusCodec {
    pub fn encoder(max_payload: usize) -> engine_net_voice::Result<Self> {
        Self::validate_cap(max_payload)?;
        let mut encoder = Encoder::new(Channels::Mono, SampleRate::Hz48000, Application::Voip)
            .map_err(|_| VoiceError::Codec("Opus encoder initialization failed"))?;
        encoder
            .set_bitrate(Bitrate::Value(32_000))
            .map_err(|_| VoiceError::Codec("Opus bitrate configuration failed"))?;
        Ok(Self {
            state: State::Encoder(encoder),
            max_payload,
            input: [0.; FRAME_SAMPLES],
            output: [0.; FRAME_SAMPLES],
        })
    }
    pub fn decoder(max_payload: usize) -> engine_net_voice::Result<Self> {
        Self::validate_cap(max_payload)?;
        let decoder = Decoder::new(Channels::Mono, SampleRate::Hz48000)
            .map_err(|_| VoiceError::Codec("Opus decoder initialization failed"))?;
        Ok(Self {
            state: State::Decoder(decoder),
            max_payload,
            input: [0.; FRAME_SAMPLES],
            output: [0.; FRAME_SAMPLES],
        })
    }
    fn validate_cap(cap: usize) -> engine_net_voice::Result<()> {
        if cap == 0 || cap > 1000 {
            return Err(VoiceError::Invalid("Opus payload cap outside 1..=1000"));
        }
        Ok(())
    }
    fn append_output(&self, out: &mut Vec<i16>) {
        out.extend(
            self.output
                .iter()
                .map(|s| (s.clamp(-1., 1.) * 32767.).round() as i16),
        );
    }
}
impl VoiceCodec for OpusCodec {
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
    fn frame_samples(&self) -> usize {
        FRAME_SAMPLES
    }
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>) -> engine_net_voice::Result<()> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(VoiceError::Invalid("Opus frame must contain 960 samples"));
        }
        let State::Encoder(encoder) = &mut self.state else {
            return Err(VoiceError::Invalid("Opus decoder cannot encode"));
        };
        for (dst, src) in self.input.iter_mut().zip(pcm) {
            *dst = f32::from(*src) / 32768.;
        }
        let mut packet = [0u8; 1000];
        let count = encoder
            .encode_float_to_slice(&self.input, &mut packet[..self.max_payload])
            .map_err(|_| VoiceError::Codec("Opus encode failed or exceeded payload cap"))?;
        out.extend_from_slice(&packet[..count]);
        Ok(())
    }
    fn decode(&mut self, payload: &[u8], out: &mut Vec<i16>) -> engine_net_voice::Result<()> {
        if payload.is_empty() || payload.len() > self.max_payload {
            return Err(VoiceError::Codec("Opus payload outside bounds"));
        }
        let State::Decoder(decoder) = &mut self.state else {
            return Err(VoiceError::Invalid("Opus encoder cannot decode"));
        };
        if decoder
            .get_nb_samples(payload)
            .map_err(|_| VoiceError::Codec("Malformed Opus packet"))?
            != FRAME_SAMPLES
        {
            return Err(VoiceError::Codec("Opus packet duration differs from 20 ms"));
        }
        let count = decoder
            .decode_float_to_slice(payload, &mut self.output, false)
            .map_err(|_| VoiceError::Codec("Opus decode failed"))?;
        if count != FRAME_SAMPLES {
            return Err(VoiceError::Codec("Opus returned wrong sample count"));
        }
        self.append_output(out);
        Ok(())
    }
    fn conceal(&mut self, out: &mut Vec<i16>) -> bool {
        let State::Decoder(decoder) = &mut self.state else {
            return false;
        };
        if decoder
            .decode_float_to_slice(&[], &mut self.output, false)
            .ok()
            != Some(FRAME_SAMPLES)
        {
            return false;
        }
        self.append_output(out);
        true
    }
}
impl Drop for OpusCodec {
    fn drop(&mut self) {
        self.input.fill(0.);
        self.output.fill(0.);
    }
}
