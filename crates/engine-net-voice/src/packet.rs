//! Version-1 voice wire format (little-endian, exact length, no trailing bytes):
//!
//! ```text
//! u8 version (1) | u8 kind (0 uplink, 1 downlink) | u8 flags | u8 channel
//! u32 sequence | u32 timestamp (samples)
//! [u64 speaker]            downlink only; stamped by the server, never by the speaker
//! [f32 x3 position]        when flags & POSITION
//! u16 payload_len | payload (1..=max_payload_bytes)
//! ```
use crate::{Result, VoiceError, VoiceLimits, finite_position};
use engine_network::PeerId;

const VERSION: u8 = 1;
const UPLINK: u8 = 0;
const DOWNLINK: u8 = 1;
const FLAG_POSITION: u8 = 1;
const FLAG_END: u8 = 2;
const KNOWN_FLAGS: u8 = FLAG_POSITION | FLAG_END;

/// Speaker -> server frame. Any position is a client claim; routers prefer server positions.
#[derive(Clone, Debug, PartialEq)]
pub struct UplinkPacket {
    pub channel: u8,
    pub sequence: u32,
    pub timestamp: u32,
    pub position: Option<[f32; 3]>,
    /// Last frame of a talk spurt; receivers stop playout after draining it.
    pub end_of_talkspurt: bool,
    pub payload: Vec<u8>,
}

/// Server -> listener frame with the server-assigned speaker ID.
#[derive(Clone, Debug, PartialEq)]
pub struct DownlinkPacket {
    pub speaker: PeerId,
    pub channel: u8,
    pub sequence: u32,
    pub timestamp: u32,
    pub position: Option<[f32; 3]>,
    pub end_of_talkspurt: bool,
    pub payload: Vec<u8>,
}

struct Header {
    channel: u8,
    sequence: u32,
    timestamp: u32,
    position: Option<[f32; 3]>,
    end: bool,
}

fn encode(
    kind: u8,
    header: &Header,
    speaker: Option<PeerId>,
    payload: &[u8],
    limits: &VoiceLimits,
) -> Result<Vec<u8>> {
    if payload.is_empty() || payload.len() > limits.max_payload_bytes {
        return Err(VoiceError::Invalid("voice payload empty or above limit"));
    }
    if header.position.is_some_and(|p| !finite_position(p)) {
        return Err(VoiceError::Invalid("non-finite voice position"));
    }
    let mut flags = 0;
    if header.position.is_some() {
        flags |= FLAG_POSITION;
    }
    if header.end {
        flags |= FLAG_END;
    }
    let mut out = Vec::with_capacity(32 + payload.len());
    out.extend_from_slice(&[VERSION, kind, flags, header.channel]);
    out.extend_from_slice(&header.sequence.to_le_bytes());
    out.extend_from_slice(&header.timestamp.to_le_bytes());
    if let Some(speaker) = speaker {
        out.extend_from_slice(&speaker.to_le_bytes());
    }
    if let Some(position) = header.position {
        for v in position {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(VoiceError::Malformed("truncated packet"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let mut a = [0; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
}

fn decode(
    bytes: &[u8],
    kind: u8,
    limits: &VoiceLimits,
) -> Result<(Header, Option<PeerId>, Vec<u8>)> {
    let mut r = Reader(bytes);
    if r.u8()? != VERSION {
        return Err(VoiceError::Malformed("unsupported voice version"));
    }
    if r.u8()? != kind {
        return Err(VoiceError::Malformed("unexpected voice packet direction"));
    }
    let flags = r.u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(VoiceError::Malformed("unknown voice flags"));
    }
    let channel = r.u8()?;
    let sequence = r.u32()?;
    let timestamp = r.u32()?;
    let speaker = if kind == DOWNLINK {
        Some(r.u64()?)
    } else {
        None
    };
    let position = if flags & FLAG_POSITION != 0 {
        let p = [r.f32()?, r.f32()?, r.f32()?];
        if !finite_position(p) {
            return Err(VoiceError::Malformed("non-finite voice position"));
        }
        Some(p)
    } else {
        None
    };
    let len = r.u16()? as usize;
    if len == 0 || len > limits.max_payload_bytes {
        return Err(VoiceError::Malformed("voice payload empty or above limit"));
    }
    let payload = r.take(len)?.to_vec();
    if !r.0.is_empty() {
        return Err(VoiceError::Malformed("trailing bytes"));
    }
    Ok((
        Header {
            channel,
            sequence,
            timestamp,
            position,
            end: flags & FLAG_END != 0,
        },
        speaker,
        payload,
    ))
}

impl UplinkPacket {
    pub fn encode(&self, limits: &VoiceLimits) -> Result<Vec<u8>> {
        encode(UPLINK, &self.header(), None, &self.payload, limits)
    }
    pub fn decode(bytes: &[u8], limits: &VoiceLimits) -> Result<Self> {
        let (h, _, payload) = decode(bytes, UPLINK, limits)?;
        Ok(Self {
            channel: h.channel,
            sequence: h.sequence,
            timestamp: h.timestamp,
            position: h.position,
            end_of_talkspurt: h.end,
            payload,
        })
    }
    fn header(&self) -> Header {
        Header {
            channel: self.channel,
            sequence: self.sequence,
            timestamp: self.timestamp,
            position: self.position,
            end: self.end_of_talkspurt,
        }
    }
}

impl DownlinkPacket {
    pub fn encode(&self, limits: &VoiceLimits) -> Result<Vec<u8>> {
        let header = Header {
            channel: self.channel,
            sequence: self.sequence,
            timestamp: self.timestamp,
            position: self.position,
            end: self.end_of_talkspurt,
        };
        encode(DOWNLINK, &header, Some(self.speaker), &self.payload, limits)
    }
    pub fn decode(bytes: &[u8], limits: &VoiceLimits) -> Result<Self> {
        let (h, speaker, payload) = decode(bytes, DOWNLINK, limits)?;
        Ok(Self {
            speaker: speaker.ok_or(VoiceError::Malformed("missing speaker"))?,
            channel: h.channel,
            sequence: h.sequence,
            timestamp: h.timestamp,
            position: h.position,
            end_of_talkspurt: h.end,
            payload,
        })
    }
}
