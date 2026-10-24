use crate::{DownlinkPacket, Result, VoiceError};
use serde::Serialize;
use std::collections::BTreeMap;

/// Adaptive playout-delay configuration. Times are caller-supplied milliseconds on one
/// monotonic clock (the audio clock), so behaviour is deterministic and testable.
#[derive(Clone, Copy, Debug)]
pub struct JitterConfig {
    /// Duration of one frame.
    pub frame_ms: f32,
    pub min_delay_ms: f32,
    pub max_delay_ms: f32,
    /// Target delay = clamp(frame_ms + jitter_multiplier * jitter, min, max).
    pub jitter_multiplier: f32,
    /// Consecutive missing frames (with nothing buffered) after which playout stops.
    pub max_missing_before_stop: u32,
}
impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            frame_ms: 20.0,
            min_delay_ms: 20.0,
            max_delay_ms: 300.0,
            jitter_multiplier: 3.0,
            max_missing_before_stop: 10,
        }
    }
}
impl JitterConfig {
    pub fn validate(&self) -> Result<()> {
        let ok = [
            self.frame_ms,
            self.min_delay_ms,
            self.max_delay_ms,
            self.jitter_multiplier,
        ]
        .iter()
        .all(|v| v.is_finite() && *v >= 0.0)
            && self.frame_ms > 0.0
            && self.min_delay_ms <= self.max_delay_ms
            && self.max_missing_before_stop > 0;
        if !ok {
            return Err(VoiceError::Invalid("invalid jitter configuration"));
        }
        Ok(())
    }
}

/// Nonsecret per-speaker receive statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct JitterStats {
    pub received: u64,
    pub played: u64,
    /// Frames that never arrived in time and were concealed.
    pub concealed: u64,
    /// Frames that arrived after their playout slot.
    pub late: u64,
    pub duplicates: u64,
    /// Frames discarded to shrink an over-long playout delay or on overflow/resync.
    pub discarded: u64,
    /// Slots held back (concealed without consuming a frame) to grow the playout delay.
    pub stretched: u64,
    /// RFC 3550 interarrival jitter estimate.
    pub jitter_ms: f32,
    pub target_delay_ms: f32,
    pub buffered_frames: usize,
}

/// One playout slot.
#[derive(Clone, Debug, PartialEq)]
pub enum Playout {
    Frame(DownlinkPacket),
    /// The slot's frame is missing: conceal it.
    Missing,
}

/// Per-speaker reorder/jitter buffer. Call [`JitterBuffer::pop`] once per frame interval.
pub struct JitterBuffer {
    config: JitterConfig,
    capacity: usize,
    frames: BTreeMap<u32, (DownlinkPacket, f64)>,
    next: Option<u32>,
    playing: bool,
    missing_run: u32,
    last_transit: Option<f64>,
    /// Smallest observed transit (clock offset + minimum network delay).
    min_transit: Option<f64>,
    /// Media time (ms) of the slot `pop` plays next.
    play_ms: f64,
    sample_rate: u32,
    stats: JitterStats,
}
impl JitterBuffer {
    pub fn new(config: JitterConfig, capacity: usize, sample_rate: u32) -> Result<Self> {
        config.validate()?;
        if capacity < 2 || sample_rate == 0 {
            return Err(VoiceError::Invalid("invalid jitter buffer capacity/rate"));
        }
        Ok(Self {
            config,
            capacity,
            frames: BTreeMap::new(),
            next: None,
            playing: false,
            missing_run: 0,
            last_transit: None,
            min_transit: None,
            play_ms: 0.0,
            sample_rate,
            stats: JitterStats {
                target_delay_ms: config.min_delay_ms.max(config.frame_ms),
                ..JitterStats::default()
            },
        })
    }
    pub fn stats(&self) -> JitterStats {
        JitterStats {
            buffered_frames: self.frames.len(),
            ..self.stats
        }
    }
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Inserts a frame that arrived at `now_ms`. Late and duplicate frames are counted and dropped.
    pub fn push(&mut self, packet: DownlinkPacket, now_ms: f64) {
        self.stats.received += 1;
        // RFC 3550 jitter: J += (|D| - J) / 16 with D the transit-time difference.
        let transit = now_ms - self.media_ms(packet.timestamp);
        if let Some(last) = self.last_transit {
            let d = (transit - last).abs() as f32;
            self.stats.jitter_ms += (d - self.stats.jitter_ms) / 16.0;
        }
        self.last_transit = Some(transit);
        let target = self.config.frame_ms + self.config.jitter_multiplier * self.stats.jitter_ms;
        self.stats.target_delay_ms = target.clamp(
            self.config.min_delay_ms.max(self.config.frame_ms),
            self.config.max_delay_ms,
        );

        let sequence = packet.sequence;
        if let Some(next) = self.next {
            let window = (self.capacity as u32).saturating_mul(2);
            if sequence < next && next - sequence < window {
                self.stats.late += 1;
                return;
            }
            if sequence.abs_diff(next) >= window {
                // Far away (sender restart or long outage): resynchronise instead of growing.
                self.stats.discarded += self.frames.len() as u64;
                self.frames.clear();
                self.next = None;
                self.playing = false;
                self.min_transit = None;
            }
        }
        self.min_transit = Some(self.min_transit.map_or(transit, |m| m.min(transit)));
        if self.frames.contains_key(&sequence) {
            self.stats.duplicates += 1;
            return;
        }
        if self.frames.len() >= self.capacity {
            // Bounded: drop the oldest buffered frame.
            if let Some((&oldest, _)) = self.frames.iter().next() {
                if oldest > sequence {
                    self.stats.discarded += 1;
                    return;
                }
                self.frames.remove(&oldest);
                self.stats.discarded += 1;
                let after = oldest.saturating_add(1);
                self.next = Some(self.next.map_or(after, |n| n.max(after)));
            }
        }
        self.frames.insert(sequence, (packet, now_ms));
    }

    fn media_ms(&self, timestamp: u32) -> f64 {
        f64::from(timestamp) * 1000.0 / f64::from(self.sample_rate)
    }

    /// Next playout slot at `now_ms`, or `None` while idle/prebuffering (play silence).
    pub fn pop(&mut self, now_ms: f64) -> Option<Playout> {
        let frame_ms = f64::from(self.config.frame_ms);
        if !self.playing {
            let (&first, (packet, _)) = self.frames.iter().next()?;
            let first_ms = self.media_ms(packet.timestamp);
            let waited = now_ms - first_ms - self.min_transit.unwrap_or(0.0);
            if waited + f64::from(self.config.frame_ms) * 0.5
                < f64::from(self.stats.target_delay_ms)
                && self.frames.len() < self.capacity / 2
            {
                return None;
            }
            self.play_ms = first_ms;
            self.playing = true;
            self.missing_run = 0;
            self.next = Some(self.next.map_or(first, |n| n.max(first)));
        }
        // Playout delay of this slot beyond the fastest observed transit.
        let delay = now_ms - self.play_ms - self.min_transit.unwrap_or(0.0);
        let target = f64::from(self.stats.target_delay_ms);
        if delay + frame_ms * 0.5 < target {
            // Jitter grew: hold one slot (concealed) so later frames arrive in time.
            self.stats.stretched += 1;
            return Some(Playout::Missing);
        }
        let mut next = self.next?;
        let mut excess = delay - (target + 2.0 * frame_ms);
        while excess > 0.0 && !self.frames.is_empty() {
            // Jitter shrank: skip one slot per frame of excess delay.
            if self.frames.remove(&next).is_some() {
                self.stats.discarded += 1;
            }
            next = next.wrapping_add(1);
            self.play_ms += frame_ms;
            excess -= frame_ms;
        }
        self.next = Some(next.wrapping_add(1));
        self.play_ms += frame_ms;
        if let Some((packet, _)) = self.frames.remove(&next) {
            self.missing_run = 0;
            self.stats.played += 1;
            if packet.end_of_talkspurt && self.frames.is_empty() {
                self.playing = false;
            }
            return Some(Playout::Frame(packet));
        }
        self.missing_run += 1;
        if self.frames.is_empty() && self.missing_run >= self.config.max_missing_before_stop {
            self.playing = false;
            return None;
        }
        self.stats.concealed += 1;
        Some(Playout::Missing)
    }
}
