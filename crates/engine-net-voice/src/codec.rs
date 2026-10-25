use crate::{Result, VoiceError};

/// Pluggable mono voice codec. The audio domain supplies real codecs (e.g. Opus); this crate
/// ships [`RawPcmCodec`] as the uncompressed reference and test path.
///
/// Decoders are created per speaker, so implementations may keep inter-frame state.
pub trait VoiceCodec: Send {
    fn sample_rate(&self) -> u32;
    /// Samples per frame (mono).
    fn frame_samples(&self) -> usize;
    /// Encodes exactly `frame_samples()` samples, appending to `out`.
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>) -> Result<()>;
    /// Decodes one untrusted payload, appending exactly `frame_samples()` samples to `out`.
    fn decode(&mut self, payload: &[u8], out: &mut Vec<i16>) -> Result<()>;
    /// Codec-native packet-loss concealment for one missing frame. Append `frame_samples()`
    /// samples and return true, or return false to use the receiver's [`Concealment`] fallback.
    fn conceal(&mut self, out: &mut Vec<i16>) -> bool {
        let _ = out;
        false
    }
}

/// Uncompressed 16-bit little-endian PCM.
#[derive(Clone, Copy, Debug)]
pub struct RawPcmCodec {
    sample_rate: u32,
    frame_samples: usize,
}
impl RawPcmCodec {
    /// `frame_ms` must divide into whole samples (e.g. 48 kHz / 20 ms = 960 samples).
    pub fn new(sample_rate: u32, frame_ms: u32) -> Result<Self> {
        let frame_samples = u64::from(sample_rate) * u64::from(frame_ms);
        if sample_rate == 0
            || frame_ms == 0
            || !frame_samples.is_multiple_of(1000)
            || frame_samples / 1000 > 48_000
        {
            return Err(VoiceError::Invalid("unsupported PCM rate/frame duration"));
        }
        Ok(Self {
            sample_rate,
            frame_samples: (frame_samples / 1000) as usize,
        })
    }
}
impl VoiceCodec for RawPcmCodec {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn frame_samples(&self) -> usize {
        self.frame_samples
    }
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<u8>) -> Result<()> {
        if pcm.len() != self.frame_samples {
            return Err(VoiceError::Invalid("PCM frame has the wrong sample count"));
        }
        out.reserve(pcm.len() * 2);
        for sample in pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(())
    }
    fn decode(&mut self, payload: &[u8], out: &mut Vec<i16>) -> Result<()> {
        if payload.len() != self.frame_samples * 2 {
            return Err(VoiceError::Codec("PCM payload has the wrong length"));
        }
        out.extend(
            payload
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| i16::from_le_bytes(pair)),
        );
        Ok(())
    }
}

/// Receiver-side fallback concealment used when the codec has none.
pub trait Concealment: Send {
    /// Writes `frame_samples` samples for the `consecutive`-th (1-based) missing frame in a row,
    /// given the last successfully played frame (empty if none).
    fn conceal(&mut self, last: &[i16], consecutive: u32, frame_samples: usize, out: &mut Vec<i16>);
}

/// Repeats the last frame with 6 dB attenuation per consecutive loss, then silence.
#[derive(Clone, Copy, Debug)]
pub struct FadeConcealment {
    /// Consecutive losses after which output is silent.
    pub max_repeats: u32,
}
impl Default for FadeConcealment {
    fn default() -> Self {
        Self { max_repeats: 3 }
    }
}
impl Concealment for FadeConcealment {
    fn conceal(
        &mut self,
        last: &[i16],
        consecutive: u32,
        frame_samples: usize,
        out: &mut Vec<i16>,
    ) {
        if consecutive > self.max_repeats || last.len() != frame_samples {
            out.extend(std::iter::repeat_n(0, frame_samples));
            return;
        }
        let shift = consecutive.min(15);
        out.extend(last.iter().map(|&s| s >> shift));
    }
}
