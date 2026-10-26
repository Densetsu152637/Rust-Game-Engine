//! Owned decode worker with cancellation and a single replaceable seek request.
use crate::{
    AudioControl, AudioDecoder, AudioError, DecodeProgress, DecoderInfo, PlayOptions, Result,
    StreamConfig, StreamGeneration, StreamState, VoiceHandle,
};
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    thread::{self, JoinHandle},
    time::Duration,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeWorkerState {
    Running,
    Backpressure,
    Finished,
    Failed,
    Cancelled,
}
#[derive(Clone, Debug)]
pub struct DecodeWorkerStatus {
    pub state: DecodeWorkerState,
    pub error: Option<AudioError>,
}
struct SeekRequest {
    frame: u64,
    generation: StreamGeneration,
}
/// Owns its worker and voice. Drop cancels playback, wakes backpressure, and joins the worker.
pub struct DecodedStream {
    observer: crate::stream::StreamObserver,
    control: AudioControl,
    voice: VoiceHandle,
    info: DecoderInfo,
    max_seek_frame: u64,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    commands: Arc<Mutex<Option<SeekRequest>>>,
    status: Arc<Mutex<DecodeWorkerStatus>>,
    worker: Option<JoinHandle<()>>,
}
fn worker_error(message: &str) -> AudioError {
    AudioError::new("audio.decode.worker", message)
}
impl DecodedStream {
    /// `decoder` is opened/probed on the caller thread; decoding runs only on the new worker.
    pub fn spawn(
        mut decoder: AudioDecoder,
        control: AudioControl,
        options: PlayOptions,
        capacity_frames: usize,
    ) -> Result<Self> {
        let info = decoder.info().clone();
        let max_seek_frame = decoder.max_seek_frame();
        let (voice, mut writer) = control.play_stream(
            StreamConfig {
                sample_rate: info.sample_rate,
                channels: info.channels,
                capacity_frames,
            },
            options,
        )?;
        let observer = writer.observer();
        let cancel = decoder.cancel.clone();
        let commands = Arc::new(Mutex::new(None::<SeekRequest>));
        let status = Arc::new(Mutex::new(DecodeWorkerStatus {
            state: DecodeWorkerState::Running,
            error: None,
        }));
        let thread_commands = commands.clone();
        let thread_status = status.clone();
        let thread_cancel = cancel.clone();
        let spawned = thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                let mut generation = StreamGeneration(0);
                let mut active = true;
                loop {
                    if thread_cancel.load(Ordering::Acquire) {
                        set_status(&thread_status, DecodeWorkerState::Cancelled, None);
                        break;
                    }
                    let command = match thread_commands.lock() {
                        Ok(mut c) => c.take(),
                        Err(_) => {
                            let error = worker_error("Seek command lock poisoned");
                            let _ = writer.fail(generation, error.clone());
                            set_status(&thread_status, DecodeWorkerState::Failed, Some(error));
                            break;
                        }
                    };
                    if let Some(command) = command {
                        generation = command.generation;
                        match decoder.seek(command.frame) {
                            Ok(()) => {
                                active = true;
                                set_status(&thread_status, DecodeWorkerState::Running, None);
                            }
                            Err(e) => {
                                let _ = writer.fail(generation, e.clone());
                                set_status(&thread_status, DecodeWorkerState::Failed, Some(e));
                                active = false;
                            }
                        }
                    }
                    if writer
                        .snapshot()
                        .is_ok_and(|s| s.state == StreamState::Stopped)
                    {
                        set_status(&thread_status, DecodeWorkerState::Cancelled, None);
                        break;
                    }
                    if !active {
                        thread::park_timeout(Duration::from_millis(50));
                        continue;
                    }
                    match decoder.pump(&mut writer, generation) {
                        Ok(DecodeProgress::Backpressure) => {
                            set_status(&thread_status, DecodeWorkerState::Backpressure, None);
                            thread::park_timeout(Duration::from_millis(2));
                        }
                        Ok(DecodeProgress::Finished) => {
                            active = false;
                            set_status(&thread_status, DecodeWorkerState::Finished, None);
                        }
                        Ok(_) => set_status(&thread_status, DecodeWorkerState::Running, None),
                        Err(e) if e.code == "audio.stream.stale" => {
                            // A wrapper seek publishes its command while holding this lock.
                            let queued = thread_commands.lock().is_ok_and(|c| c.is_some());
                            if !queued {
                                set_status(&thread_status, DecodeWorkerState::Failed, Some(e));
                                active = false;
                            }
                        }
                        Err(e) => {
                            set_status(&thread_status, DecodeWorkerState::Failed, Some(e));
                            active = false;
                        }
                    }
                }
            });
        let worker = match spawned {
            Ok(w) => w,
            Err(e) => {
                control.stop(voice);
                return Err(AudioError::new("audio.decode.worker", e.to_string()));
            }
        };
        Ok(Self {
            observer,
            control,
            voice,
            info,
            max_seek_frame,
            cancel,
            commands,
            status,
            worker: Some(worker),
        })
    }
    /// Durable queue failure, preserved after cancellation and mixer retirement.
    pub fn failure(&self) -> Option<AudioError> {
        self.observer.failure()
    }
    #[cfg(test)]
    pub(crate) fn cancellation(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.cancel.clone()
    }
    pub fn voice(&self) -> VoiceHandle {
        self.voice
    }
    pub fn info(&self) -> &DecoderInfo {
        &self.info
    }
    pub fn status(&self) -> Result<DecodeWorkerStatus> {
        self.status
            .lock()
            .map(|s| s.clone())
            .map_err(|_| worker_error("Status lock poisoned"))
    }
    /// Validates cancellation before changing the source epoch. Replaces any queued seek.
    pub fn seek(&mut self, source_frame: u64) -> Result<StreamGeneration> {
        if self.cancel.load(Ordering::Acquire)
            || self.worker.as_ref().is_none_or(|w| w.is_finished())
        {
            return Err(worker_error("Decode worker is cancelled or stopped"));
        }
        if source_frame > self.max_seek_frame {
            return Err(AudioError::new(
                "audio.decode.seek",
                "Seek exceeds audio duration",
            ));
        }
        let mut command = self
            .commands
            .lock()
            .map_err(|_| worker_error("Seek command lock poisoned"))?;
        let generation = self.control.seek_stream(self.voice, source_frame)?;
        *command = Some(SeekRequest {
            frame: source_frame,
            generation,
        });
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        Ok(generation)
    }
    pub fn cancel(&mut self) -> Result<()> {
        self.cancel.store(true, Ordering::Release);
        self.control.stop(self.voice);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            worker
                .join()
                .map_err(|_| worker_error("Decode worker panicked"))?;
        }
        Ok(())
    }
}
fn set_status(
    status: &Mutex<DecodeWorkerStatus>,
    state: DecodeWorkerState,
    error: Option<AudioError>,
) {
    if let Ok(mut current) = status.lock() {
        *current = DecodeWorkerStatus { state, error };
    }
}
impl Drop for DecodedStream {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}
