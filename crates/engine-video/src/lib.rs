//! Local file playback through explicitly configured FFmpeg tools.
mod decoder;
use engine_audio::{AudioControl, PlayOptions, StreamConfig, StreamState, VoiceHandle};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

pub type Result<T> = std::result::Result<T, VideoError>;
#[derive(Clone, Debug)]
pub struct VideoError(pub String);
impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for VideoError {}
impl From<std::io::Error> for VideoError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<engine_audio::AudioError> for VideoError {
    fn from(e: engine_audio::AudioError) -> Self {
        Self(e.to_string())
    }
}
#[derive(Clone, Debug)]
pub struct DecoderTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}
impl Default for DecoderTools {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    /// First video timestamp relative to the normalized file origin.
    pub video_start: f64,
    pub has_audio: bool,
}
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub seconds: f64,
    pub rgba: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Ended,
    Failed,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct PlaybackSnapshot {
    pub seconds: f64,
    pub state: PlaybackState,
    pub error: Option<String>,
}
type AudioTail = Arc<Mutex<Option<engine_audio::StreamSnapshot>>>;
#[derive(Clone)]
struct AudioClock {
    control: AudioControl,
    voice: VoiceHandle,
    tail: AudioTail,
}
struct ClockData {
    position: f64,
    anchor: Instant,
    paused: bool,
    stopped: bool,
    failed: Option<String>,
    ended: bool,
    source: Option<AudioClock>,
}
/// Shared source timeline. Audio time is submitted PCM, not hardware presentation time.
#[derive(Clone)]
pub struct PlaybackClock {
    data: Arc<Mutex<ClockData>>,
    duration: f64,
}
impl PlaybackClock {
    pub fn seconds(&self) -> Option<f64> {
        let mut d = self.data.lock().ok()?;
        if d.stopped || d.failed.is_some() {
            return None;
        }
        if let Some(source) = d.source.clone() {
            let snapshot = match source.control.stream_snapshot(source.voice) {
                Ok(Some(s)) => Some(s),
                Ok(None) => source.tail.lock().ok().and_then(|s| s.clone()),
                Err(_) => return None,
            };
            if let Some(s) = snapshot {
                if matches!(s.state, StreamState::Stopped | StreamState::Failed) {
                    return None;
                }
                d.position = s.source_seconds.min(self.duration);
                d.ended = s.state == StreamState::Drained;
            }
        } else if !d.paused {
            d.position = (d.position + d.anchor.elapsed().as_secs_f64()).min(self.duration);
            d.ended = d.position >= self.duration;
            d.anchor = Instant::now();
        }
        Some(d.position)
    }
}
/// Owns decoder subprocesses, bounded queues and the streamed audio voice.
/// Poll once per render tick and upload the returned frame to a texture stream.
pub struct VideoPlayer {
    tools: DecoderTools,
    path: PathBuf,
    info: MediaInfo,
    audio: Option<AudioControl>,
    voice: Option<VoiceHandle>,
    clock: PlaybackClock,
    workers: decoder::Workers,
    pending: Option<VideoFrame>,
    video_eof: bool,
    offset: f64,
}
impl VideoPlayer {
    pub fn open(path: &Path, tools: DecoderTools, audio: Option<AudioControl>) -> Result<Self> {
        #[cfg(windows)]
        reject_network_path(path)?;
        let path = path.canonicalize()?;
        #[cfg(windows)]
        reject_network_path(&path)?;
        if !path.is_file() {
            return Err(VideoError("Expected a regular local media file".into()));
        }
        let info = decoder::probe(&tools, &path)?;
        if info.has_audio && audio.is_none() {
            return Err(VideoError(
                "Media has audio; an AudioControl is required".into(),
            ));
        }
        Self::start(path, tools, info, audio, 0., false)
    }
    fn start(
        path: PathBuf,
        tools: DecoderTools,
        info: MediaInfo,
        audio: Option<AudioControl>,
        offset: f64,
        paused: bool,
    ) -> Result<Self> {
        let (voice, writer) = if info.has_audio {
            let (voice, writer) = audio
                .as_ref()
                .ok_or_else(|| VideoError("Audio output required".into()))?
                .play_stream(
                    StreamConfig {
                        sample_rate: 48000,
                        channels: 2,
                        capacity_frames: 24000,
                    },
                    PlayOptions::default(),
                )?;
            if offset > 0. {
                audio
                    .as_ref()
                    .ok_or_else(|| VideoError("Audio output required".into()))?
                    .seek_stream(voice, (offset * 48000.).round() as u64)?;
            }
            if paused {
                audio.as_ref().map(|a| a.pause(voice));
            }
            (Some(voice), Some(writer))
        } else {
            (None, None)
        };
        let tail = Arc::new(Mutex::new(None));
        let source = audio.clone().zip(voice).map(|(control, voice)| AudioClock {
            control,
            voice,
            tail: tail.clone(),
        });
        let clock = PlaybackClock {
            data: Arc::new(Mutex::new(ClockData {
                position: offset,
                anchor: Instant::now(),
                paused,
                stopped: false,
                failed: None,
                ended: false,
                source,
            })),
            duration: info.duration,
        };
        let workers = match decoder::Workers::start(&tools, &path, &info, offset, writer, tail) {
            Ok(w) => w,
            Err(e) => {
                if let Some(v) = voice {
                    audio.as_ref().map(|a| a.stop(v));
                }
                return Err(e);
            }
        };
        Ok(Self {
            tools,
            path,
            info,
            audio,
            voice,
            clock,
            workers,
            pending: None,
            video_eof: false,
            offset,
        })
    }
    pub fn info(&self) -> &MediaInfo {
        &self.info
    }
    pub fn clock(&self) -> PlaybackClock {
        self.clock.clone()
    }
    pub fn pause(&mut self) {
        let _ = self.clock.seconds();
        if let Ok(mut d) = self.clock.data.lock() {
            d.paused = true;
        }
        if let Some(v) = self.voice {
            self.audio.as_ref().map(|a| a.pause(v));
        }
    }
    pub fn resume(&mut self) {
        if let Ok(mut d) = self.clock.data.lock() {
            d.paused = false;
            d.anchor = Instant::now();
        }
        if let Some(v) = self.voice {
            self.audio.as_ref().map(|a| a.resume(v));
        }
    }
    /// Restarts decoding and flushes both queues. Existing clock clones remain valid.
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || seconds < 0. || seconds >= self.info.duration {
            return Err(VideoError("Seek must be within [0,duration)".into()));
        }
        let paused = self
            .clock
            .data
            .lock()
            .map_err(|_| VideoError("Clock poisoned".into()))?
            .paused;
        self.workers.cancel();
        if let Some(v) = self.voice.take() {
            self.audio.as_ref().map(|a| a.stop(v));
        }
        self.workers.stop();
        if !self.path.is_file() {
            let error = "Media file is no longer a regular local file".to_owned();
            if let Ok(mut d) = self.clock.data.lock() {
                d.failed = Some(error.clone());
            }
            return Err(VideoError(error));
        }
        let mut replacement = match Self::start(
            self.path.clone(),
            self.tools.clone(),
            self.info.clone(),
            self.audio.clone(),
            seconds,
            paused,
        ) {
            Ok(player) => player,
            Err(e) => {
                if let Ok(mut d) = self.clock.data.lock() {
                    d.failed = Some(e.to_string());
                }
                return Err(e);
            }
        };
        let retained_clock = self.clock.clone();
        // The old player is dropped with its own detached clock, after its workers stop.
        self.clock = replacement.clock.clone();
        std::mem::swap(self, &mut replacement);
        let source = self
            .clock
            .data
            .lock()
            .map_err(|_| VideoError("Clock poisoned".into()))?
            .source
            .clone();
        if let Ok(mut d) = retained_clock.data.lock() {
            d.position = seconds;
            d.anchor = Instant::now();
            d.stopped = false;
            d.failed = None;
            d.ended = false;
            d.source = source;
        }
        self.clock = retained_clock;
        Ok(())
    }
    pub fn snapshot(&self) -> PlaybackSnapshot {
        let seconds = self
            .clock
            .seconds()
            .unwrap_or_else(|| self.clock.data.lock().map_or(self.offset, |d| d.position));
        let error = self
            .workers
            .error()
            .or_else(|| self.clock.data.lock().ok().and_then(|d| d.failed.clone()));
        let state = if error.is_some() {
            PlaybackState::Failed
        } else if let Ok(d) = self.clock.data.lock() {
            if d.stopped {
                PlaybackState::Stopped
            } else if self.video_eof && d.ended {
                PlaybackState::Ended
            } else if d.paused {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            }
        } else {
            PlaybackState::Failed
        };
        PlaybackSnapshot {
            seconds,
            state,
            error,
        }
    }
    /// Returns the newest decoded frame due on the master clock; drops late frames.
    pub fn poll(&mut self) -> Result<Option<VideoFrame>> {
        let (stopped, failure) = {
            let d = self
                .clock
                .data
                .lock()
                .map_err(|_| VideoError("Clock poisoned".into()))?;
            (d.stopped, d.failed.clone())
        };
        if let Some(error) = self.workers.error().or(failure) {
            if let Ok(mut d) = self.clock.data.lock() {
                d.failed = Some(error.clone());
            }
            self.workers.cancel();
            if let Some(v) = self.voice.take() {
                self.audio.as_ref().map(|a| a.stop(v));
            }
            self.workers.stop();
            return Err(VideoError(error));
        }
        if stopped {
            return Ok(None);
        }
        let now = self.clock.seconds().unwrap_or(self.offset);
        let mut due = None;
        loop {
            if self.pending.is_none() {
                match self
                    .workers
                    .frames
                    .lock()
                    .map_err(|_| VideoError("Frame queue poisoned".into()))?
                    .try_recv()
                {
                    Ok(frame) => self.pending = Some(frame),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.video_eof = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            if self
                .pending
                .as_ref()
                .is_some_and(|f| f.seconds <= now + 0.000001)
            {
                due = self.pending.take();
            } else {
                break;
            }
        }
        Ok(due)
    }
    pub fn stop(&mut self) {
        self.workers.cancel();
        if let Ok(mut d) = self.clock.data.lock() {
            d.stopped = true;
        }
        if let Some(v) = self.voice.take() {
            self.audio.as_ref().map(|a| a.stop(v));
        }
        self.workers.stop();
        self.pending = None;
    }
}
impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}
#[cfg(windows)]
fn reject_network_path(path: &Path) -> Result<()> {
    if path.components().any(|c| matches!(c, std::path::Component::Prefix(p) if matches!(p.kind(), std::path::Prefix::UNC(_,_) | std::path::Prefix::VerbatimUNC(_,_)))) { return Err(VideoError("Network file paths are unsupported".into())); }
    Ok(())
}
