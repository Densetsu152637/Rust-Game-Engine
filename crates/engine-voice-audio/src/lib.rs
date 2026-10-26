//! Explicit microphone capture, Opus transport and ordinary PCM stream playback.
//! All session methods except the CPAL callback run on the caller's thread.
mod capture;
mod codec;
mod playback;

pub use capture::{CaptureSession, CaptureStats};
pub use codec::{FRAME_SAMPLES, OpusCodec, SAMPLE_RATE};
pub use playback::{PlaybackStats, VoicePlayback};

use std::fmt;

#[derive(Debug)]
pub enum BridgeError {
    Voice(engine_net_voice::VoiceError),
    Audio(engine_audio::AudioError),
    Device(String),
    Invalid(&'static str),
}
impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Voice(e) => e.fmt(f),
            Self::Audio(e) => e.fmt(f),
            Self::Device(e) => write!(f, "voice input device: {e}"),
            Self::Invalid(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for BridgeError {}
impl From<engine_net_voice::VoiceError> for BridgeError {
    fn from(value: engine_net_voice::VoiceError) -> Self {
        Self::Voice(value)
    }
}
impl From<engine_audio::AudioError> for BridgeError {
    fn from(value: engine_audio::AudioError) -> Self {
        Self::Audio(value)
    }
}
pub type Result<T> = std::result::Result<T, BridgeError>;

/// Maximum Opus payload that fits the smallest supported positioned downlink.
/// The version-1 downlink header occupies at most 34 bytes.
pub fn payload_cap(endpoint_unreliable_payload: usize) -> Result<usize> {
    endpoint_unreliable_payload
        .checked_sub(34)
        .map(|n| n.min(1000))
        .filter(|n| *n > 0)
        .ok_or(BridgeError::Invalid(
            "endpoint voice payload budget is too small",
        ))
}
