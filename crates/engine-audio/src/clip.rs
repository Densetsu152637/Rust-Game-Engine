use crate::{AudioError, MAX_WAV_BYTES, Result, valid_rate};
use std::{
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};

/// Immutable interleaved mono/stereo PCM, validated once at construction.
#[derive(Debug)]
pub struct Clip {
    sample_rate: u32,
    channels: u16,
    samples: Box<[f32]>,
}
impl Clip {
    pub fn new(sample_rate: u32, channels: u16, samples: Vec<f32>) -> Result<Arc<Self>> {
        if !valid_rate(sample_rate)
            || !(channels == 1 || channels == 2)
            || samples.is_empty()
            || samples.len() > MAX_WAV_BYTES / 2
            || !samples.len().is_multiple_of(channels as usize)
            || !samples
                .iter()
                .all(|v| v.is_finite() && (-1.0..=1.0).contains(v))
        {
            return Err(AudioError::new(
                "audio.clip.invalid",
                "Expected nonempty finite mono/stereo PCM in [-1,1] at 8-192 kHz",
            ));
        }
        Ok(Arc::new(Self {
            sample_rate,
            channels,
            samples: samples.into_boxed_slice(),
        }))
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }
}

fn invalid(message: impl Into<String>) -> AudioError {
    AudioError::new("audio.wav.invalid", message)
}
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Strict bounded RIFF framing is engine policy: reject duplicate chunks,
/// inconsistent byte rates and trailing/truncated bytes before Hound decodes.
/// Only 16-bit integer PCM mono/stereo at 8-192 kHz is accepted.
pub fn decode_wav(bytes: &[u8]) -> Result<Arc<Clip>> {
    if bytes.len() < 12
        || bytes.len() > MAX_WAV_BYTES
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
        || u32_at(bytes, 4) as usize != bytes.len() - 8
    {
        return Err(invalid("Invalid or truncated bounded RIFF WAVE file"));
    }
    let mut format = None;
    let mut data = None;
    let mut at = 12;
    while at < bytes.len() {
        if bytes.len() - at < 8 {
            return Err(invalid("Truncated chunk header"));
        }
        let size = u32_at(bytes, at + 4) as usize;
        let begin = at + 8;
        let padded = size
            .checked_add(size & 1)
            .ok_or_else(|| invalid("Chunk size overflow"))?;
        if padded > bytes.len() - begin {
            return Err(invalid("Truncated chunk data"));
        }
        match &bytes[at..at + 4] {
            b"fmt " => {
                if format.is_some() || size < 16 {
                    return Err(invalid("Invalid format chunk"));
                }
                let channels = u16_at(bytes, begin + 2);
                let rate = u32_at(bytes, begin + 4);
                if u16_at(bytes, begin) != 1
                    || !(channels == 1 || channels == 2)
                    || !valid_rate(rate)
                    || u16_at(bytes, begin + 14) != 16
                {
                    return Err(AudioError::new(
                        "audio.wav.unsupported",
                        "Only 16-bit PCM mono/stereo at 8-192 kHz is supported",
                    ));
                }
                if u16_at(bytes, begin + 12) != channels * 2
                    || u32_at(bytes, begin + 8) != rate * u32::from(channels) * 2
                {
                    return Err(invalid("Inconsistent PCM block alignment or byte rate"));
                }
                format = Some((rate, channels, at..begin + padded));
            }
            b"data" if data.is_some() => return Err(invalid("Duplicate data chunk")),
            b"data" => data = Some((size, at..begin + padded)),
            _ => {}
        }
        at = begin + padded;
    }
    let (rate, channels, format_range) = format.ok_or_else(|| invalid("Missing format"))?;
    let (size, data_range) = data.ok_or_else(|| invalid("Missing data"))?;
    if size == 0 || !size.is_multiple_of(channels as usize * 2) {
        return Err(invalid("Missing PCM data or partial frame"));
    }
    // Hound requires format before data. Reorder borrowed chunks after validating
    // the entire RIFF so existing data-before-format files remain supported;
    // PCM decoding is still entirely Hound's responsibility, without a copy.
    let stream = Cursor::new(&bytes[..12])
        .chain(Cursor::new(&bytes[format_range]))
        .chain(Cursor::new(&bytes[data_range]));
    let mut reader = hound::WavReader::new(stream).map_err(|e| invalid(e.to_string()))?;
    let samples = reader
        .samples::<i16>()
        .map(|s| s.map(|n| f32::from(n) / 32768.0))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| invalid(e.to_string()))?;
    if samples.len() != size / 2 {
        return Err(invalid("Incomplete PCM data"));
    }
    Clip::new(rate, channels, samples)
}

pub fn load_wav(path: impl AsRef<Path>) -> Result<Arc<Clip>> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| AudioError::new("audio.file.unreadable", e.to_string()))?;
    let size = file
        .metadata()
        .map_err(|e| AudioError::new("audio.file.unreadable", e.to_string()))?
        .len();
    if size > MAX_WAV_BYTES as u64 {
        return Err(AudioError::new(
            "audio.file.size",
            "Audio resource exceeds 64 MiB",
        ));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    // Bound the read itself as well as metadata; the file can change concurrently.
    (&mut file)
        .take(MAX_WAV_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AudioError::new("audio.file.unreadable", e.to_string()))?;
    if bytes.len() > MAX_WAV_BYTES {
        return Err(AudioError::new(
            "audio.file.size",
            "Audio resource exceeds 64 MiB",
        ));
    }
    decode_wav(&bytes)
}
