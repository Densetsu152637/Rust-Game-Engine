use crate::{BridgeError, FRAME_SAMPLES, OpusCodec, Result, SAMPLE_RATE, payload_cap};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use engine_net_voice::{VoiceLimits, VoiceSender};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const RING_FRAMES: usize = 9_600; // 200 ms at the highest supported input rate.
const MAX_PACKETS_PER_PUMP: usize = 8;
#[derive(Clone, Copy, Default)]
struct Sample {
    index: u64,
    value: f32,
}
struct Ring {
    data: Box<[Sample]>,
    head: usize,
    len: usize,
}
impl Ring {
    fn new() -> Self {
        Self {
            data: vec![Sample::default(); RING_FRAMES].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }
    fn clear(&mut self) {
        self.data.fill(Sample::default());
        self.head = 0;
        self.len = 0;
    }
    fn push(&mut self, sample: Sample) -> bool {
        if self.len == self.data.len() {
            return false;
        }
        self.data[(self.head + self.len) % self.data.len()] = sample;
        self.len += 1;
        true
    }
    fn pop(&mut self) -> Option<Sample> {
        if self.len == 0 {
            return None;
        }
        let sample = self.data[self.head];
        self.head = (self.head + 1) % self.data.len();
        self.len -= 1;
        Some(sample)
    }
}
struct Ingress {
    ring: Mutex<Ring>,
    error: Mutex<Option<String>>,
    clock: AtomicU64,
    epoch: AtomicU64,
    active: AtomicBool,
    dropped: AtomicU64,
    contended: AtomicU64,
}
impl Ingress {
    fn new() -> Self {
        Self {
            ring: Mutex::new(Ring::new()),
            error: Mutex::new(None),
            clock: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            active: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            contended: AtomicU64::new(0),
        }
    }
    fn push<T: InputSample>(&self, input: &[T], channels: usize, epoch: u64) {
        if !self.active.load(Ordering::Acquire) || self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let frames = input.len() / channels;
        let first = self.clock.fetch_add(frames as u64, Ordering::Relaxed);
        let Ok(mut ring) = self.ring.try_lock() else {
            self.contended.fetch_add(1, Ordering::Relaxed);
            self.dropped.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        };
        if !self.active.load(Ordering::Acquire) || self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        for (at, frame) in input.chunks_exact(channels).enumerate() {
            let mono = frame.iter().map(|s| s.as_float()).sum::<f32>() / channels as f32;
            if !ring.push(Sample {
                index: first + at as u64,
                value: mono,
            }) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
trait InputSample: cpal::SizedSample + Copy {
    fn as_float(self) -> f32;
}
impl InputSample for f32 {
    fn as_float(self) -> f32 {
        self.clamp(-1., 1.)
    }
}
impl InputSample for i16 {
    fn as_float(self) -> f32 {
        f32::from(self) / 32768.
    }
}
impl InputSample for u16 {
    fn as_float(self) -> f32 {
        (f32::from(self) - 32768.) / 32768.
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub dropped_input_frames: u64,
    pub contended_callbacks: u64,
    pub skipped_output_frames: u64,
}

/// Capture remains closed until `start`. The callback only downmixes into a fixed ring.
/// `pump` does all resampling, encoding and packet allocation on the caller's thread.
pub struct CaptureSession {
    ingress: Arc<Ingress>,
    stream: Option<cpal::Stream>,
    sender: VoiceSender,
    input_rate: u32,
    drained: Box<[Sample]>,
    previous: Option<Sample>,
    next_output: u64,
    partial: [i16; FRAME_SAMPLES],
    partial_len: usize,
    last_stop: Option<Instant>,
    skipped: u64,
    ever_sent: bool,
}
impl CaptureSession {
    pub fn new(endpoint_unreliable_payload: usize, channel: u8) -> Result<Self> {
        let cap = payload_cap(endpoint_unreliable_payload)?;
        let limits = VoiceLimits {
            max_payload_bytes: cap,
            max_frame_samples: FRAME_SAMPLES,
            ..VoiceLimits::default()
        };
        let sender = VoiceSender::new(Box::new(OpusCodec::encoder(cap)?), limits, channel)?;
        Ok(Self {
            ingress: Arc::new(Ingress::new()),
            stream: None,
            sender,
            input_rate: SAMPLE_RATE,
            drained: vec![Sample::default(); RING_FRAMES].into_boxed_slice(),
            previous: None,
            next_output: 0,
            partial: [0; FRAME_SAMPLES],
            partial_len: 0,
            last_stop: None,
            skipped: 0,
            ever_sent: false,
        })
    }
    pub fn is_active(&self) -> bool {
        self.stream.is_some() && self.ingress.active.load(Ordering::Acquire)
    }
    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            dropped_input_frames: self.ingress.dropped.load(Ordering::Relaxed),
            contended_callbacks: self.ingress.contended.load(Ordering::Relaxed),
            skipped_output_frames: self.skipped,
        }
    }
    /// Opens and starts the default input device only after the application opts in.
    pub fn start(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return if self.ingress.active.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(BridgeError::Device(
                    "microphone callback stopped; call stop before restart".into(),
                ))
            };
        }
        let device = cpal::default_host()
            .default_input_device()
            .ok_or_else(|| BridgeError::Device("no default microphone".into()))?;
        let supported = device
            .supported_input_configs()
            .map_err(device_error)?
            .filter(|c| {
                matches!(c.channels(), 1 | 2)
                    && c.max_sample_rate() >= 8_000
                    && c.min_sample_rate() <= 48_000
            })
            .filter(|c| {
                matches!(
                    c.sample_format(),
                    cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
                )
            })
            .min_by_key(|c| {
                (
                    if c.min_sample_rate() <= 48_000 && c.max_sample_rate() >= 48_000 {
                        0
                    } else {
                        1
                    },
                    u32::from(c.channels() - 1),
                    match c.sample_format() {
                        cpal::SampleFormat::F32 => 0,
                        cpal::SampleFormat::I16 => 1,
                        _ => 2,
                    },
                )
            })
            .ok_or_else(|| {
                BridgeError::Device("no mono/stereo 8–48 kHz microphone format".into())
            })?;
        let rate = 48_000.clamp(
            supported.min_sample_rate().max(8_000),
            supported.max_sample_rate().min(48_000),
        );
        let format = supported.sample_format();
        let config = supported.with_sample_rate(rate).config();
        self.ingress.clock.store(0, Ordering::Release);
        if let Ok(mut ring) = self.ingress.ring.lock() {
            ring.clear();
        }
        if let Ok(mut error) = self.ingress.error.lock() {
            *error = None;
        }
        let epoch = self.ingress.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.ingress.active.store(true, Ordering::Release);
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, config, self.ingress.clone(), epoch),
            cpal::SampleFormat::I16 => build::<i16>(&device, config, self.ingress.clone(), epoch),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, self.ingress.clone(), epoch),
            _ => unreachable!("input formats filtered above"),
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                self.ingress.active.store(false, Ordering::Release);
                return Err(e);
            }
        };
        if let Err(e) = stream.play() {
            self.ingress.active.store(false, Ordering::Release);
            return Err(device_error(e));
        }
        if let Some(stopped) = self.last_stop.take() {
            let elapsed = stopped.elapsed().as_secs_f64();
            self.sender
                .advance_timestamp((elapsed * f64::from(SAMPLE_RATE)).min(u32::MAX as f64) as u32);
        }
        self.input_rate = rate;
        self.previous = None;
        self.next_output = 0;
        self.stream = Some(stream);
        Ok(())
    }
    /// Stops native capture and clears all held microphone samples. Returns an encoded
    /// silent end frame for the caller to send, when this talkspurt sent audio.
    pub fn stop(&mut self) -> Result<Option<Vec<u8>>> {
        if self.stream.is_none() {
            return Ok(None);
        }
        self.ingress.active.store(false, Ordering::Release);
        self.ingress.epoch.fetch_add(1, Ordering::AcqRel);
        self.stream.take();
        if let Ok(mut ring) = self.ingress.ring.lock() {
            ring.clear();
        }
        let captured = self.ingress.clock.load(Ordering::Acquire);
        let target = captured.saturating_mul(SAMPLE_RATE as u64) / u64::from(self.input_rate);
        let sent_or_skipped = self.next_output.saturating_sub(self.partial_len as u64);
        let unsent = target.saturating_sub(sent_or_skipped);
        self.sender.advance_timestamp(unsent as u32);
        self.previous = None;
        self.partial.fill(0);
        self.drained.fill(Sample::default());
        self.partial_len = 0;
        self.last_stop = Some(Instant::now());
        if !std::mem::take(&mut self.ever_sent) {
            return Ok(None);
        }
        let packet = self.sender.encode_frame(&[0; FRAME_SAMPLES], None, true)?;
        Ok(Some(packet))
    }
    pub fn set_muted(&mut self, muted: bool) -> Result<Option<Vec<u8>>> {
        if muted {
            self.stop()
        } else {
            self.start()?;
            Ok(None)
        }
    }
    /// Returns at most eight packets; unprocessed ring samples remain bounded by 200 ms.
    pub fn pump(&mut self) -> Result<Vec<Vec<u8>>> {
        if self.stream.is_none() {
            return Ok(Vec::new());
        }
        self.pump_ingress()
    }
    fn pump_ingress(&mut self) -> Result<Vec<Vec<u8>>> {
        if let Ok(error) = self.ingress.error.lock() {
            if let Some(message) = error.as_ref() {
                return Err(BridgeError::Device(message.clone()));
            }
        }
        if self.stream.is_some() && !self.ingress.active.load(Ordering::Acquire) {
            return Err(BridgeError::Device("microphone callback stopped".into()));
        }
        let count = {
            let mut ring = self
                .ingress
                .ring
                .lock()
                .map_err(|_| BridgeError::Invalid("capture ring poisoned"))?;
            let mut count = 0;
            while count < self.drained.len() {
                let Some(sample) = ring.pop() else {
                    break;
                };
                self.drained[count] = sample;
                count += 1;
            }
            count
        };
        let mut packets = Vec::new();
        for i in 0..count {
            self.feed(self.drained[i], &mut packets)?;
        }
        self.drained[..count].fill(Sample::default());
        Ok(packets)
    }
    fn feed(&mut self, sample: Sample, packets: &mut Vec<Vec<u8>>) -> Result<()> {
        if let Some(previous) = self.previous {
            if sample.index > previous.index + 1 {
                let gap = sample.index - previous.index - 1;
                if gap > RING_FRAMES as u64 {
                    self.sender.advance_timestamp(self.partial_len as u32);
                    self.partial_len = 0;
                    let target = sample.index.saturating_mul(SAMPLE_RATE as u64)
                        / u64::from(self.input_rate);
                    let skipped = target.saturating_sub(self.next_output);
                    self.sender.advance_timestamp(skipped as u32);
                    self.skipped += skipped;
                    self.next_output = target;
                    self.previous = None;
                } else {
                    for index in previous.index + 1..sample.index {
                        self.interpolate(Sample { index, value: 0. }, packets)?;
                    }
                }
            }
        }
        self.interpolate(sample, packets)
    }
    fn interpolate(&mut self, sample: Sample, packets: &mut Vec<Vec<u8>>) -> Result<()> {
        if let Some(previous) = self.previous {
            while self.next_output.saturating_mul(u64::from(self.input_rate))
                / u64::from(SAMPLE_RATE)
                == previous.index
            {
                let numerator = self.next_output.saturating_mul(u64::from(self.input_rate))
                    % u64::from(SAMPLE_RATE);
                let alpha = numerator as f32 / SAMPLE_RATE as f32;
                let value = previous.value + (sample.value - previous.value) * alpha;
                let pcm = (value.clamp(-1., 1.) * 32767.).round() as i16;
                self.partial[self.partial_len] = pcm;
                self.partial_len += 1;
                self.next_output += 1;
                if self.partial_len == FRAME_SAMPLES {
                    if packets.len() < MAX_PACKETS_PER_PUMP {
                        packets.push(self.sender.encode_frame(&self.partial, None, false)?);
                        self.ever_sent = true;
                    } else {
                        self.sender.advance_timestamp(FRAME_SAMPLES as u32);
                        self.skipped += FRAME_SAMPLES as u64;
                    }
                    self.partial_len = 0;
                }
            }
        }
        self.previous = Some(sample);
        Ok(())
    }
}
impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.ingress.active.store(false, Ordering::Release);
        self.ingress.epoch.fetch_add(1, Ordering::AcqRel);
        self.stream.take();
        if let Ok(mut ring) = self.ingress.ring.lock() {
            ring.clear();
        }
        self.partial.fill(0);
        self.drained.fill(Sample::default());
    }
}
fn build<T: InputSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ingress: Arc<Ingress>,
    epoch: u64,
) -> Result<cpal::Stream> {
    let channels = usize::from(config.channels);
    let errors = ingress.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _| ingress.push(data, channels, epoch),
            move |error| {
                errors.active.store(false, Ordering::Release);
                if let Ok(mut slot) = errors.error.try_lock() {
                    *slot = Some(error.to_string());
                }
            },
            Some(Duration::from_secs(3)),
        )
        .map_err(device_error)
}
fn device_error(e: impl std::fmt::Display) -> BridgeError {
    BridgeError::Device(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_net_voice::UplinkPacket;
    #[test]
    fn callback_downmix_formats_epoch_overflow_and_contention() {
        let ingress = Ingress::new();
        ingress.active.store(true, Ordering::Release);
        ingress.epoch.store(7, Ordering::Release);
        ingress.push(&[i16::MAX, i16::MIN, 0, 0], 2, 7);
        ingress.push(&[u16::MAX, 0], 2, 7);
        ingress.push(&[1.0f32, -1.0], 2, 6); // stale epoch
        let mut ring = ingress.ring.lock().unwrap();
        assert_eq!(ring.len, 3);
        assert!(ring.pop().unwrap().value.abs() < 0.001);
        assert_eq!(ring.pop().unwrap().value, 0.);
        assert!(ring.pop().unwrap().value.abs() < 0.001);
        ingress.push(&[0f32; 64], 1, 7); // lock contention: no block
        assert_eq!(ingress.contended.load(Ordering::Relaxed), 1);
        drop(ring);
        ingress.push(&vec![0f32; RING_FRAMES + 3], 1, 7);
        assert_eq!(ingress.dropped.load(Ordering::Relaxed), 67);
        ingress.active.store(false, Ordering::Release);
        ingress.push(&[1f32; 2], 1, 7);
        assert_eq!(ingress.ring.lock().unwrap().len, RING_FRAMES);
    }
    fn synthetic(partition: usize) -> Vec<Vec<u8>> {
        let mut session = CaptureSession::new(1200, 0).unwrap();
        session.input_rate = 44_100;
        session.ingress.active.store(true, Ordering::Release);
        session.ingress.epoch.store(1, Ordering::Release);
        let pcm: Vec<f32> = (0..4_410).map(|i| (i as f32 * 0.01).sin() * 0.4).collect();
        let mut packets = Vec::new();
        for chunk in pcm.chunks(partition) {
            session.ingress.push(chunk, 1, 1);
            packets.extend(session.pump_ingress().unwrap());
        }
        packets
    }
    #[test]
    fn resampling_is_invariant_to_callback_partition() {
        let a = synthetic(63);
        let b = synthetic(1_000);
        assert!(!a.is_empty());
        assert_eq!(a, b);
        let limits = VoiceLimits::default();
        for (at, bytes) in a.iter().enumerate() {
            let packet = UplinkPacket::decode(bytes, &limits).unwrap();
            assert_eq!(packet.timestamp, at as u32 * FRAME_SAMPLES as u32);
        }
    }
}
