//! Voice-chat transport for engine sessions.
//!
//! Sans-IO: the crate turns PCM frames into bounded voice packets, routes them on the server
//! and reassembles them on listeners through per-speaker jitter buffers with loss concealment.
//! Capture, encoding codecs (e.g. Opus) and spatial mixing belong to the audio domain; they plug
//! in through [`VoiceCodec`], [`Concealment`] and the positions carried by [`VoiceOutput`].
//! The optional `engine-voice-audio` crate supplies a native Opus/PCM stream bridge.
//! Packets travel on [`VOICE_LANE`] with [`VOICE_DELIVERY`] over any
//! `engine_network::MessageEndpoint`.
mod codec;
mod jitter;
mod packet;
mod receiver;
mod router;
mod sender;

pub use codec::{Concealment, FadeConcealment, RawPcmCodec, VoiceCodec};
pub use jitter::{JitterBuffer, JitterConfig, JitterStats, Playout};
pub use packet::{DownlinkPacket, UplinkPacket};
pub use receiver::{CodecFactory, VoiceOutput, VoiceReceiver};
pub use router::{RouterStats, VoiceRouter, VoiceRouting};
pub use sender::VoiceSender;

use engine_network::{Delivery, Lane};
use std::fmt;

/// Lane every voice packet uses.
pub const VOICE_LANE: Lane = Lane::VOICE;
/// Voice is latency-critical and loss-tolerant: never retransmitted.
pub const VOICE_DELIVERY: Delivery = Delivery::Unreliable;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceError {
    /// Malformed or out-of-limit packet bytes from a peer.
    Malformed(&'static str),
    /// Caller-side misuse (wrong frame size, invalid configuration).
    Invalid(&'static str),
    /// Speaker exceeded the configured frame rate; the packet was not forwarded.
    RateLimited,
    /// Codec failure.
    Codec(&'static str),
}
impl fmt::Display for VoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed voice packet: {e}"),
            Self::Invalid(e) => write!(f, "invalid voice usage: {e}"),
            Self::RateLimited => f.write_str("voice frame rate exceeded"),
            Self::Codec(e) => write!(f, "voice codec: {e}"),
        }
    }
}
impl std::error::Error for VoiceError {}
pub type Result<T> = std::result::Result<T, VoiceError>;

/// Hard bounds applied to every packet and buffer. Application configuration is trusted.
#[derive(Clone, Debug)]
pub struct VoiceLimits {
    /// Largest encoded frame payload (bytes). Keep the packet below the transport datagram limit.
    pub max_payload_bytes: usize,
    /// Largest decoded frame (samples, mono).
    pub max_frame_samples: usize,
    /// Frames a speaker may send per one-second window before packets are rate limited.
    pub max_frames_per_second: u32,
    /// Speakers a receiver tracks at once; extra speakers are ignored until one is removed.
    pub max_speakers: usize,
    /// Frames buffered per speaker.
    pub max_buffered_frames: usize,
    /// Voice channels a router peer may join.
    pub max_channels_per_peer: usize,
}
impl Default for VoiceLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: 1000,
            max_frame_samples: 2880,
            max_frames_per_second: 60,
            max_speakers: 64,
            max_buffered_frames: 32,
            max_channels_per_peer: 16,
        }
    }
}
impl VoiceLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_payload_bytes == 0
            || self.max_payload_bytes > u16::MAX as usize
            || self.max_frame_samples == 0
            || self.max_frames_per_second == 0
            || self.max_speakers == 0
            || self.max_buffered_frames < 2
            || self.max_channels_per_peer == 0
        {
            return Err(VoiceError::Invalid("invalid voice limits"));
        }
        Ok(())
    }
}

pub(crate) fn finite_position(position: [f32; 3]) -> bool {
    position.iter().all(|v| v.is_finite() && v.abs() <= 1.0e12)
}
