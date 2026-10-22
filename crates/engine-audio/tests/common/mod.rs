//! Shared helpers for the headless audio suites. Nothing here opens a device.
#![allow(dead_code)]

use engine_audio::{AudioConfig, AudioEngine, Clip, PlayOptions};
use std::sync::Arc;

/// Builds a RIFF container from `(tag, payload)` chunks, padding odd chunks.
pub fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = b"WAVE".to_vec();
    for (tag, payload) in chunks {
        body.extend_from_slice(*tag);
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        body.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[derive(Clone)]
pub struct Fmt {
    pub format: u16,
    pub channels: u16,
    pub rate: u32,
    pub byte_rate: u32,
    pub block_align: u16,
    pub bits: u16,
}

impl Fmt {
    /// A fully consistent 16-bit PCM description.
    pub fn pcm16(channels: u16, rate: u32) -> Self {
        Self {
            format: 1,
            channels,
            rate,
            byte_rate: rate * u32::from(channels) * 2,
            block_align: channels * 2,
            bits: 16,
        }
    }
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&self.format.to_le_bytes());
        v.extend_from_slice(&self.channels.to_le_bytes());
        v.extend_from_slice(&self.rate.to_le_bytes());
        v.extend_from_slice(&self.byte_rate.to_le_bytes());
        v.extend_from_slice(&self.block_align.to_le_bytes());
        v.extend_from_slice(&self.bits.to_le_bytes());
        v
    }
}

pub fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub fn wav_with(fmt: &Fmt, data: Vec<u8>) -> Vec<u8> {
    riff(&[(b"fmt ", fmt.bytes()), (b"data", data)])
}

pub fn wav16(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
    wav_with(&Fmt::pcm16(channels, rate), pcm_bytes(samples))
}

/// Rewrites the RIFF size field so it matches the actual length again.
pub fn fix_riff_size(bytes: &mut [u8]) {
    let size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
}

pub fn offline(rate: u32, max_voices: usize) -> AudioEngine {
    AudioEngine::offline(AudioConfig {
        sample_rate: rate,
        max_voices,
    })
    .unwrap()
}

/// 48 kHz engine with the default voice cap.
pub fn engine() -> AudioEngine {
    offline(48_000, 64)
}

pub fn mono(rate: u32, samples: &[f32]) -> Arc<Clip> {
    Clip::new(rate, 1, samples.to_vec()).unwrap()
}

pub fn stereo(rate: u32, samples: &[f32]) -> Arc<Clip> {
    Clip::new(rate, 2, samples.to_vec()).unwrap()
}

pub fn looped() -> PlayOptions {
    PlayOptions {
        looped: true,
        ..Default::default()
    }
}

/// Mixes `frames` stereo frames and returns the interleaved samples.
pub fn mix(engine: &mut AudioEngine, frames: usize) -> Vec<f32> {
    let mut out = vec![7.0; frames * 2];
    engine.mix(&mut out).unwrap();
    out
}

/// Mixes and returns only the left channel.
pub fn left(engine: &mut AudioEngine, frames: usize) -> Vec<f32> {
    mix(engine, frames).chunks(2).map(|f| f[0]).collect()
}

pub fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-5, "{a} != {b}");
}

pub fn near_all(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?} vs {expected:?}");
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-5, "{actual:?} != {expected:?}");
    }
}
