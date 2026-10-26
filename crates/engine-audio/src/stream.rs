//! Bounded PCM ingress and source clocks. Producers never execute in the mixer.
use crate::{AudioError, Result, valid_rate};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Queue memory is fixed at construction; at most eight MiB of stereo PCM.
pub const MAX_STREAM_FRAMES: usize = 1_048_576;
#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub capacity_frames: usize,
}
impl StreamConfig {
    pub(crate) fn validate(self) -> Result<()> {
        if !valid_rate(self.sample_rate)
            || !matches!(self.channels, 1 | 2)
            || !(2..=MAX_STREAM_FRAMES).contains(&self.capacity_frames)
        {
            return Err(error(
                "audio.stream.config",
                "Expected mono/stereo 8-192 kHz PCM and 2..=1048576 queue frames",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamGeneration(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    Playing,
    Starved,
    Paused,
    Drained,
    Failed,
    Stopped,
}
/// Source time is PCM submitted to the mixer, not hardware presentation time.
#[derive(Clone, Debug)]
pub struct StreamSnapshot {
    pub sample_rate: u32,
    pub channels: u16,
    pub generation: StreamGeneration,
    pub state: StreamState,
    pub source_frame: f64,
    pub source_seconds: f64,
    /// Successfully rendered output frames in this generation, excluding underrun silence.
    pub submitted_output_frames: u64,
    pub queued_frames: usize,
    pub capacity_frames: usize,
    pub underrun_output_frames: u64,
    /// No hardware presentation estimate is available from this source queue.
    pub presentation_seconds: Option<f64>,
    pub error: Option<AudioError>,
}
struct Queue {
    frames: Box<[[f32; 2]]>,
    head: usize,
    len: usize,
    base: u64,
    cursor: f64,
    generation: StreamGeneration,
    eof: bool,
    error: Option<AudioError>,
    submitted: u64,
}
struct Shared {
    config: StreamConfig,
    queue: Mutex<Queue>,
    paused: AtomicBool,
    stopped: AtomicBool,
    ended: AtomicBool,
    underruns: AtomicU64,
    starved: AtomicBool,
    #[cfg(test)]
    failure_pause: Option<FailurePause>,
}
/// One producer. A generation token prevents stale decoder work after seek.
pub struct StreamWriter {
    shared: Arc<Shared>,
}
/// Read-only queue lifetime retained independently of mixer voice/producer status.
pub(crate) struct StreamObserver {
    shared: Arc<Shared>,
}
impl StreamObserver {
    pub(crate) fn failure(&self) -> Option<AudioError> {
        snapshot(&self.shared).map_or_else(Some, |s| s.error)
    }
}
pub(crate) struct StreamPlayback {
    shared: Arc<Shared>,
}
fn error(code: &'static str, message: &str) -> AudioError {
    AudioError::new(code, message)
}
impl StreamWriter {
    pub(crate) fn observer(&self) -> StreamObserver {
        StreamObserver {
            shared: self.shared.clone(),
        }
    }
    pub fn snapshot(&self) -> Result<StreamSnapshot> {
        snapshot(&self.shared)
    }
    /// Appends as many complete frames as fit, returning the accepted frame count.
    /// Zero means backpressure; retry the unaccepted suffix outside the callback.
    pub fn write(&mut self, generation: StreamGeneration, samples: &[f32]) -> Result<usize> {
        let channels = usize::from(self.shared.config.channels);
        if !samples.len().is_multiple_of(channels)
            || samples
                .iter()
                .any(|v| !v.is_finite() || !(-1.0..=1.0).contains(v))
        {
            return Err(error(
                "audio.stream.pcm",
                "Expected complete finite PCM frames in [-1,1]",
            ));
        }
        let mut q = self
            .shared
            .queue
            .lock()
            .map_err(|_| error("audio.stream.poisoned", "PCM queue poisoned"))?;
        self.check(&q, generation)?;
        if q.eof || q.error.is_some() {
            return Err(error("audio.stream.closed", "Producer has finished"));
        }
        let count = (samples.len() / channels).min(q.frames.len() - q.len);
        for frame in samples.chunks_exact(channels).take(count) {
            let at = (q.head + q.len) % q.frames.len();
            q.frames[at] = [frame[0], frame[channels - 1]];
            q.len += 1;
        }
        Ok(count)
    }
    pub fn finish(&mut self, generation: StreamGeneration) -> Result<()> {
        let mut q = self
            .shared
            .queue
            .lock()
            .map_err(|_| error("audio.stream.poisoned", "PCM queue poisoned"))?;
        self.check(&q, generation)?;
        q.eof = true;
        Ok(())
    }
    /// Fails immediately; already queued PCM is discarded by the next mixer call.
    pub fn fail(&mut self, generation: StreamGeneration, failure: AudioError) -> Result<()> {
        let mut q = self
            .shared
            .queue
            .lock()
            .map_err(|_| error("audio.stream.poisoned", "PCM queue poisoned"))?;
        self.check(&q, generation)?;
        q.error = Some(failure);
        self.shared.ended.store(true, Ordering::Release);
        drop(q);
        #[cfg(test)]
        if let Some(pause) = &self.shared.failure_pause {
            let _ = pause.reached.send(());
            let _ = pause
                .resume
                .lock()
                .map(|r| r.recv_timeout(std::time::Duration::from_secs(10)));
        }
        Ok(())
    }
    fn check(&self, q: &Queue, generation: StreamGeneration) -> Result<()> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(error("audio.stream.stopped", "Playback stopped"));
        }
        if q.generation != generation {
            return Err(error("audio.stream.stale", "Stale producer generation"));
        }
        Ok(())
    }
}
impl Drop for StreamWriter {
    fn drop(&mut self) {
        // Dropping the sole producer closes input; the consumer drains queued PCM.
        if let Ok(mut q) = self.shared.queue.lock() {
            q.eof = true;
        }
    }
}
fn snapshot(shared: &Shared) -> Result<StreamSnapshot> {
    let q = shared
        .queue
        .lock()
        .map_err(|_| error("audio.stream.poisoned", "PCM queue poisoned"))?;
    let state = if q.error.is_some() {
        StreamState::Failed
    } else if shared.stopped.load(Ordering::Acquire) {
        StreamState::Stopped
    } else if shared.ended.load(Ordering::Acquire) {
        StreamState::Drained
    } else if shared.paused.load(Ordering::Acquire) {
        StreamState::Paused
    } else if shared.starved.load(Ordering::Acquire) {
        StreamState::Starved
    } else {
        StreamState::Playing
    };
    Ok(StreamSnapshot {
        sample_rate: shared.config.sample_rate,
        channels: shared.config.channels,
        generation: q.generation,
        state,
        source_frame: q.cursor,
        source_seconds: q.cursor / f64::from(shared.config.sample_rate),
        submitted_output_frames: q.submitted,
        queued_frames: q.len,
        capacity_frames: q.frames.len(),
        underrun_output_frames: shared.underruns.load(Ordering::Relaxed),
        presentation_seconds: None,
        error: q.error.clone(),
    })
}
impl StreamPlayback {
    pub(crate) fn new(config: StreamConfig) -> Result<(Self, StreamWriter)> {
        config.validate()?;
        let shared = Arc::new(Shared {
            config,
            queue: Mutex::new(Queue {
                frames: vec![[0.; 2]; config.capacity_frames].into_boxed_slice(),
                head: 0,
                len: 0,
                base: 0,
                cursor: 0.,
                generation: StreamGeneration(0),
                eof: false,
                error: None,
                submitted: 0,
            }),
            paused: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            starved: AtomicBool::new(false),
            #[cfg(test)]
            failure_pause: NEXT_FAILURE_PAUSE.with(|slot| slot.borrow_mut().take()),
        });
        Ok((
            Self {
                shared: shared.clone(),
            },
            StreamWriter { shared },
        ))
    }
    pub(crate) fn snapshot(&self) -> Result<StreamSnapshot> {
        snapshot(&self.shared)
    }
    pub(crate) fn pause(&self, value: bool) {
        self.shared.paused.store(value, Ordering::Release);
    }
    pub(crate) fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire)
    }
    pub(crate) fn seek(&mut self, source_frame: u64) -> Result<StreamGeneration> {
        // f64 must represent adjacent source frames exactly.
        if source_frame > (1_u64 << 52) {
            return Err(error(
                "audio.stream.seek",
                "Source frame exceeds exact clock range",
            ));
        }
        let mut q = self
            .shared
            .queue
            .lock()
            .map_err(|_| error("audio.stream.poisoned", "PCM queue poisoned"))?;
        let next = q
            .generation
            .0
            .checked_add(1)
            .ok_or_else(|| error("audio.stream.generation", "Generation exhausted"))?;
        q.generation = StreamGeneration(next);
        q.head = 0;
        q.len = 0;
        q.base = source_frame;
        q.cursor = source_frame as f64;
        q.eof = false;
        q.error = None;
        q.submitted = 0;
        self.shared.starved.store(false, Ordering::Release);
        self.shared.ended.store(false, Ordering::Release);
        self.shared.underruns.store(0, Ordering::Relaxed);
        Ok(q.generation)
    }
    /// Calls `put` only for actual PCM; missing frames remain silent. One try-lock per block.
    pub(crate) fn render(
        &mut self,
        rate: u32,
        pitch: f32,
        count: usize,
        mut put: impl FnMut(usize, [f32; 2]),
    ) -> usize {
        let Ok(mut q) = self.shared.queue.try_lock() else {
            self.shared.starved.store(true, Ordering::Release);
            self.shared
                .underruns
                .fetch_add(count as u64, Ordering::Relaxed);
            return 0;
        };
        if q.error.is_some() {
            self.shared.ended.store(true, Ordering::Release);
            return 0;
        }
        let step = f64::from(self.shared.config.sample_rate) / f64::from(rate) * f64::from(pitch);
        let mut produced = 0;
        self.shared.starved.store(false, Ordering::Release);
        for i in 0..count {
            let first = q.cursor.floor() as u64;
            while q.len > 0 && q.base < first {
                q.head = (q.head + 1) % q.frames.len();
                q.base += 1;
                q.len -= 1;
            }
            let end = q.base + q.len as u64;
            if q.eof && q.cursor >= end as f64 {
                q.cursor = end as f64;
                self.shared.ended.store(true, Ordering::Release);
                break;
            }
            let alpha = (q.cursor - first as f64) as f32;
            if q.len == 0 || first != q.base || (q.len == 1 && alpha > 0. && !q.eof) {
                self.shared.starved.store(true, Ordering::Release);
                self.shared
                    .underruns
                    .fetch_add((count - i) as u64, Ordering::Relaxed);
                break;
            }
            let a = q.frames[q.head];
            let b = q.frames[(q.head + usize::from(q.len > 1)) % q.frames.len()];
            put(i, std::array::from_fn(|c| a[c] + (b[c] - a[c]) * alpha));
            q.cursor += step;
            if q.eof {
                q.cursor = q.cursor.min(end as f64);
            }
            q.submitted += 1;
            produced += 1;
        }
        if q.eof && q.cursor >= (q.base + q.len as u64) as f64 {
            self.shared.ended.store(true, Ordering::Release);
        }
        produced
    }
}
impl Drop for StreamPlayback {
    fn drop(&mut self) {
        if !self.ended() {
            self.shared.stopped.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn producer_lock_never_blocks_render_or_advances_clock() {
        let (mut playback, writer) = StreamPlayback::new(StreamConfig {
            sample_rate: 8_000,
            channels: 1,
            capacity_frames: 4,
        })
        .unwrap();
        let held = writer.shared.queue.lock().unwrap();
        assert_eq!(
            playback.render(8_000, 1., 128, |_, _| panic!("must be silent")),
            0
        );
        assert_eq!(held.cursor, 0.);
        drop(held);
        assert_eq!(writer.snapshot().unwrap().underrun_output_frames, 128);
        assert_eq!(writer.snapshot().unwrap().state, StreamState::Starved);
    }
}

#[cfg(test)]
struct FailurePause {
    reached: std::sync::mpsc::SyncSender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}
#[cfg(test)]
std::thread_local! {static NEXT_FAILURE_PAUSE:std::cell::RefCell<Option<FailurePause>>=const {std::cell::RefCell::new(None)};}
#[cfg(test)]
pub(crate) fn pause_next_stream_failure() -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
) {
    let (reached, wait) = std::sync::mpsc::sync_channel(1);
    let (release, resume) = std::sync::mpsc::sync_channel(1);
    NEXT_FAILURE_PAUSE.with(|slot| {
        *slot.borrow_mut() = Some(FailurePause {
            reached,
            resume: Mutex::new(resume),
        })
    });
    (wait, release)
}
