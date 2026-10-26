use super::{
    CompiledGraph, GraphInstance, MAX_PARAMETERS, ParameterUpdate,
    runtime::{Change, error, updates},
};
use crate::{
    AudioControl, AudioError, PlayOptions, Result, StreamConfig, StreamGeneration, StreamState,
    VoiceHandle,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
const BLOCK: usize = 256;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProceduralState {
    Running,
    Backpressure,
    Finished,
    Failed,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct ProceduralStatus {
    pub generation: StreamGeneration,
    pub state: ProceduralState,
    pub generated_frames: u64,
    pub error: Option<AudioError>,
}
struct Commands {
    changes: [Option<Change>; MAX_PARAMETERS],
    reset: Option<StreamGeneration>,
    finish: bool,
}
/// Owns a producer thread and normal streamed voice. Dropping it stops and joins both.
pub struct ProceduralStream {
    program: Arc<CompiledGraph>,
    control: AudioControl,
    voice: VoiceHandle,
    commands: Arc<Mutex<Commands>>,
    status: Arc<Mutex<ProceduralStatus>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl ProceduralStream {
    pub fn spawn(
        program: Arc<CompiledGraph>,
        control: AudioControl,
        options: PlayOptions,
        capacity_frames: usize,
        duration_frames: Option<u64>,
    ) -> Result<Self> {
        let rate = control.lock()?.config.sample_rate;
        if duration_frames.is_some_and(|n| n > u64::from(rate) * 86_400) {
            return Err(error("audio.graph.duration", "Duration exceeds 24 hours"));
        }
        let mut instance = GraphInstance::new(program.clone(), rate)?;
        let (voice, mut writer) = control.play_stream(
            StreamConfig {
                sample_rate: rate,
                channels: 2,
                capacity_frames,
            },
            options,
        )?;
        let commands = Arc::new(Mutex::new(Commands {
            changes: [None; MAX_PARAMETERS],
            reset: None,
            finish: false,
        }));
        let status = Arc::new(Mutex::new(ProceduralStatus {
            state: ProceduralState::Running,
            generation: StreamGeneration(0),
            generated_frames: 0,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let command_copy = commands.clone();
        let status_copy = status.clone();
        let stop_copy = stop.clone();
        let spawned = thread::Builder::new()
            .name("audio-procedural".into())
            .spawn(move || {
                let mut stereo = [[0.; 2]; BLOCK];
                let mut pcm = [0.; BLOCK * 2];
                let mut offset = 0;
                let mut length = 0;
                let mut generation = StreamGeneration(0);
                let mut active = true;
                let mut finish = false;
                loop {
                    if stop_copy.load(Ordering::Acquire) {
                        publish(
                            &status_copy,
                            ProceduralState::Stopped,
                            generation,
                            instance.generated_frames(),
                            None,
                        );
                        break;
                    }
                    let command = match command_copy.lock() {
                        Ok(mut c) => {
                            let reset = c.reset.take();
                            let changes = std::mem::replace(&mut c.changes, [None; MAX_PARAMETERS]);
                            (reset, changes, c.finish)
                        }
                        Err(_) => {
                            let e = error("audio.graph.worker", "Control mailbox poisoned");
                            let _ = writer.fail(generation, e.clone());
                            publish(
                                &status_copy,
                                ProceduralState::Failed,
                                generation,
                                instance.generated_frames(),
                                Some(e),
                            );
                            break;
                        }
                    };
                    if let Some(reset) = command.0 {
                        generation = reset;
                        instance.reset();
                        offset = 0;
                        length = 0;
                        active = true;
                        finish = false;
                    }
                    instance.apply(&command.1);
                    finish |= command.2;
                    if writer
                        .snapshot()
                        .is_ok_and(|s| s.state == StreamState::Stopped)
                    {
                        publish(
                            &status_copy,
                            ProceduralState::Stopped,
                            generation,
                            instance.generated_frames(),
                            None,
                        );
                        break;
                    }
                    if !active {
                        thread::park_timeout(Duration::from_millis(50));
                        continue;
                    }
                    let result = (|| -> Result<bool> {
                        if offset < length {
                            let frames = writer.write(generation, &pcm[offset..length])?;
                            offset += frames * 2;
                            if frames == 0 {
                                publish(
                                    &status_copy,
                                    ProceduralState::Backpressure,
                                    generation,
                                    instance.generated_frames(),
                                    None,
                                );
                                thread::park_timeout(Duration::from_millis(2));
                            } else {
                                publish(
                                    &status_copy,
                                    ProceduralState::Running,
                                    generation,
                                    instance.generated_frames(),
                                    None,
                                );
                            }
                            return Ok(true);
                        }
                        let remaining = duration_frames.map_or(BLOCK as u64, |n| {
                            n.saturating_sub(instance.generated_frames())
                        });
                        if finish || remaining == 0 {
                            writer.finish(generation)?;
                            publish(
                                &status_copy,
                                ProceduralState::Finished,
                                generation,
                                instance.generated_frames(),
                                None,
                            );
                            return Ok(false);
                        }
                        let count = remaining.min(BLOCK as u64) as usize;
                        instance.render(&mut stereo[..count])?;
                        for (pair, frame) in pcm[..count * 2]
                            .as_chunks_mut::<2>()
                            .0
                            .iter_mut()
                            .zip(&stereo[..count])
                        {
                            *pair = *frame;
                        }
                        offset = 0;
                        length = count * 2;
                        Ok(true)
                    })();
                    match result {
                        Ok(value) => active = value,
                        Err(e) if e.code == "audio.stream.stale" => {
                            let resetting = command_copy.lock().is_ok_and(|c| c.reset.is_some());
                            if !resetting {
                                publish(
                                    &status_copy,
                                    ProceduralState::Failed,
                                    generation,
                                    instance.generated_frames(),
                                    Some(e),
                                );
                                active = false;
                            }
                        }
                        Err(e) => {
                            let _ = writer.fail(generation, e.clone());
                            publish(
                                &status_copy,
                                ProceduralState::Failed,
                                generation,
                                instance.generated_frames(),
                                Some(e),
                            );
                            active = false;
                        }
                    }
                }
            });
        let worker = match spawned {
            Ok(worker) => worker,
            Err(e) => {
                control.stop(voice);
                return Err(AudioError::new("audio.graph.worker", e.to_string()));
            }
        };
        Ok(Self {
            program,
            control,
            voice,
            commands,
            status,
            stop,
            worker: Some(worker),
        })
    }
    pub fn voice(&self) -> VoiceHandle {
        self.voice
    }
    pub fn status(&self) -> Result<ProceduralStatus> {
        self.status
            .lock()
            .map(|s| s.clone())
            .map_err(|_| error("audio.graph.worker", "Status lock poisoned"))
    }
    fn live(&self) -> Result<()> {
        if self.stop.load(Ordering::Acquire) || self.worker.as_ref().is_none_or(|w| w.is_finished())
        {
            Err(error("audio.graph.stopped", "Procedural voice stopped"))
        } else {
            Ok(())
        }
    }
    /// Latest update per variable replaces earlier pending values; storage never grows.
    pub fn set_parameters(&self, values: &[ParameterUpdate<'_>]) -> Result<()> {
        self.live()?;
        let changes = updates(&self.program, values)?;
        let mut commands = self
            .commands
            .lock()
            .map_err(|_| error("audio.graph.worker", "Control mailbox poisoned"))?;
        if commands.finish
            || matches!(
                self.status()?.state,
                ProceduralState::Finished | ProceduralState::Failed | ProceduralState::Stopped
            )
        {
            return Err(error(
                "audio.graph.finished",
                "Reset before updating a finished or failed voice",
            ));
        }
        for (slot, change) in commands.changes.iter_mut().zip(changes) {
            if change.is_some() {
                *slot = change;
            }
        }
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        Ok(())
    }
    /// Clears queued PCM with a fresh epoch, then restores defaults, phase/seeds and DSP state.
    pub fn reset(&mut self) -> Result<StreamGeneration> {
        self.live()?;
        let mut commands = self
            .commands
            .lock()
            .map_err(|_| error("audio.graph.worker", "Control mailbox poisoned"))?;
        let generation = self.control.seek_stream(self.voice, 0)?;
        commands.changes.fill(None);
        commands.reset = Some(generation);
        commands.finish = false;
        publish(&self.status, ProceduralState::Running, generation, 0, None);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        Ok(generation)
    }
    /// Stops generation after any already-rendered pending block, then drains normal stream EOF.
    pub fn finish(&self) -> Result<()> {
        self.live()?;
        self.commands
            .lock()
            .map_err(|_| error("audio.graph.worker", "Control mailbox poisoned"))?
            .finish = true;
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.control.stop(self.voice);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            if worker.join().is_err() {
                let failure = error("audio.graph.worker", "Producer thread panicked");
                if let Ok(mut status) = self.status.lock() {
                    status.state = ProceduralState::Failed;
                    status.error = Some(failure.clone());
                }
                return Err(failure);
            }
        }
        // A stop can overtake a reset before the worker adopts the newer epoch.
        if let Ok(mut status) = self.status.lock() {
            status.state = ProceduralState::Stopped;
            status.error = None;
        }
        Ok(())
    }
}
fn publish(
    status: &Mutex<ProceduralStatus>,
    state: ProceduralState,
    generation: StreamGeneration,
    generated_frames: u64,
    error: Option<AudioError>,
) {
    if let Ok(mut status) = status.lock() {
        if generation.0 < status.generation.0 {
            return;
        }
        *status = ProceduralStatus {
            state,
            generation,
            generated_frames,
            error,
        };
    }
}
impl Drop for ProceduralStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
