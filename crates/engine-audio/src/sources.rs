use crate::{
    AudioControl, AudioError, Clip, PlayOptions, Result, VoiceHandle, VoiceState, allocate_handle,
};
use crate::{
    AudioDecoder, DecodeLimits, DecodedStream, StreamGeneration, StreamSnapshot,
    graph::{CompiledGraph, GraphInstance, ParameterUpdate, ProceduralStream},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceHandle(u64);
impl SourceHandle {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObserverHandle(u64);
impl ObserverHandle {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAction {
    Play,
    Pause,
    Resume,
    Stop,
}
#[derive(Clone, Debug)]
pub struct SourceStreamConfig {
    pub queue_frames: usize,
    pub decode_limits: DecodeLimits,
}
impl Default for SourceStreamConfig {
    fn default() -> Self {
        Self {
            queue_frames: 4096,
            decode_limits: DecodeLimits::default(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ProceduralSourceConfig {
    pub queue_frames: usize,
    pub duration_frames: Option<u64>,
    pub parameters: BTreeMap<String, f32>,
}
impl Default for ProceduralSourceConfig {
    fn default() -> Self {
        Self {
            queue_frames: 4096,
            duration_frames: None,
            parameters: BTreeMap::new(),
        }
    }
}
enum Prepared {
    Clip(Arc<Clip>),
    File(PathBuf, SourceStreamConfig),
    Graph(Arc<CompiledGraph>, ProceduralSourceConfig),
}
enum Active {
    Clip(VoiceHandle),
    File(DecodedStream),
    Graph(ProceduralStream),
}
impl Active {
    fn failure(&self) -> Option<AudioError> {
        match self {
            Self::Clip(_) => None,
            Self::File(v) => match v.status() {
                Ok(s) => s.error,
                Err(e) => Some(e),
            },
            Self::Graph(v) => match v.status() {
                Ok(s) => s.error,
                Err(e) => Some(e),
            },
        }
    }
    fn voice(&self) -> VoiceHandle {
        match self {
            Self::Clip(v) => *v,
            Self::File(v) => v.voice(),
            Self::Graph(v) => v.voice(),
        }
    }
}
struct Source {
    prepared: Prepared,
    options: PlayOptions,
    active: Option<Active>,
    failure: Option<AudioError>,
}
fn stop_active(control: &AudioControl, active: &mut Option<Active>) {
    if let Some(value) = active.take() {
        control.stop(value.voice());
        drop(value);
    }
}
fn queue_valid(frames: usize) -> Result<()> {
    if (2..=65_536).contains(&frames) {
        Ok(())
    } else {
        Err(AudioError::new(
            "audio.source.queue",
            "Queue must hold 2..=65536 frames",
        ))
    }
}
fn nonresident(options: &PlayOptions) -> Result<()> {
    if options.looped {
        Err(AudioError::new(
            "audio.source.loop",
            "Nonresident sources do not support looped playback",
        ))
    } else {
        Ok(())
    }
}
fn initial(config: &ProceduralSourceConfig) -> Vec<ParameterUpdate<'_>> {
    config
        .parameters
        .iter()
        .map(|(name, value)| ParameterUpdate {
            name,
            value: *value,
            ramp_frames: 0,
        })
        .collect()
}
struct Observer {
    event: String,
    source: SourceHandle,
    action: SourceAction,
}

/// Lifecycle controller: one voice per reusable source. Play restarts a voice.
/// Registration order defines notification order; a failed notification may
/// already have executed earlier observers. Drop releases only its own voices.
pub struct AudioSources {
    control: AudioControl,
    owner: String,
    sources: BTreeMap<SourceHandle, Source>,
    observers: BTreeMap<ObserverHandle, Observer>,
}
impl AudioSources {
    pub fn new(control: AudioControl, owner: impl Into<String>) -> Result<Self> {
        let owner = owner.into();
        if owner.len() > 256 {
            return Err(AudioError::new(
                "audio.play.invalid",
                "Owner exceeds 256 bytes",
            ));
        }
        Ok(Self {
            control,
            owner,
            sources: BTreeMap::new(),
            observers: BTreeMap::new(),
        })
    }
    pub fn create(&mut self, clip: Arc<Clip>, options: PlayOptions) -> Result<SourceHandle> {
        self.insert(Prepared::Clip(clip), options)
    }
    fn capacity(&self) -> Result<()> {
        if self.sources.len() >= 1024 {
            Err(AudioError::new(
                "audio.source.limit",
                "Source limit reached",
            ))
        } else {
            Ok(())
        }
    }
    fn insert(&mut self, prepared: Prepared, mut options: PlayOptions) -> Result<SourceHandle> {
        self.capacity()?;
        options.owner.clone_from(&self.owner);
        options.validate()?;
        let handle = SourceHandle(allocate_handle()?);
        self.sources.insert(
            handle,
            Source {
                prepared,
                options,
                active: None,
                failure: None,
            },
        );
        Ok(handle)
    }
    pub fn create_stream(
        &mut self,
        path: impl AsRef<Path>,
        mut options: PlayOptions,
        config: SourceStreamConfig,
    ) -> Result<SourceHandle> {
        nonresident(&options)?;
        options.owner.clone_from(&self.owner);
        options.validate()?;
        self.capacity()?;
        queue_valid(config.queue_frames)?;
        let path = path.as_ref().to_owned();
        drop(AudioDecoder::open(&path, config.decode_limits)?);
        self.insert(Prepared::File(path, config), options)
    }
    pub fn create_procedural(
        &mut self,
        program: Arc<CompiledGraph>,
        mut options: PlayOptions,
        config: ProceduralSourceConfig,
    ) -> Result<SourceHandle> {
        nonresident(&options)?;
        options.owner.clone_from(&self.owner);
        options.validate()?;
        self.capacity()?;
        queue_valid(config.queue_frames)?;
        let rate = self.control.lock()?.config.sample_rate;
        if config
            .duration_frames
            .is_some_and(|n| n > u64::from(rate) * 86400)
        {
            return Err(AudioError::new(
                "audio.source.duration",
                "Duration exceeds 24 hours",
            ));
        }
        if config.parameters.len() > 64 {
            return Err(AudioError::new(
                "audio.source.parameters",
                "At most 64 initial parameters",
            ));
        }
        program.validate_parameters(&initial(&config))?;
        drop(GraphInstance::new(program.clone(), rate)?);
        self.insert(Prepared::Graph(program, config), options)
    }
    pub fn configure(&mut self, handle: SourceHandle, mut options: PlayOptions) -> Result<()> {
        let source = self.sources.get_mut(&handle).ok_or_else(missing)?;
        options.owner.clone_from(&self.owner);
        options.validate()?;
        if !matches!(source.prepared, Prepared::Clip(_)) {
            nonresident(&options)?;
        }
        if let Some(active) = &source.active {
            self.control.configure(active.voice(), options.clone())?;
        }
        source.options = options;
        Ok(())
    }
    pub fn control(&mut self, handle: SourceHandle, action: SourceAction) -> Result<()> {
        let source = self.sources.get_mut(&handle).ok_or_else(missing)?;
        match action {
            SourceAction::Play => {
                stop_active(&self.control, &mut source.active);
                source.failure = None;
                source.active = Some(match &source.prepared {
                    Prepared::Clip(clip) => {
                        Active::Clip(self.control.play(clip.clone(), source.options.clone())?)
                    }
                    Prepared::File(path, config) => Active::File(DecodedStream::spawn(
                        AudioDecoder::open(path, config.decode_limits)?,
                        self.control.clone(),
                        source.options.clone(),
                        config.queue_frames,
                    )?),
                    Prepared::Graph(program, config) => {
                        Active::Graph(ProceduralStream::spawn_with_parameters(
                            program.clone(),
                            self.control.clone(),
                            source.options.clone(),
                            config.queue_frames,
                            config.duration_frames,
                            &initial(config),
                        )?)
                    }
                });
            }
            SourceAction::Pause => {
                if let Some(active) = &source.active {
                    self.control.pause(active.voice());
                }
            }
            SourceAction::Resume => {
                if let Some(active) = &source.active {
                    self.control.resume(active.voice());
                }
            }
            SourceAction::Stop => stop_active(&self.control, &mut source.active),
        }
        Ok(())
    }
    pub fn remove(&mut self, handle: SourceHandle) -> bool {
        if let Some(mut source) = self.sources.remove(&handle) {
            stop_active(&self.control, &mut source.active);
            self.observers
                .retain(|_, observer| observer.source != handle);
            true
        } else {
            false
        }
    }
    pub fn state(&self, handle: SourceHandle) -> VoiceState {
        self.sources
            .get(&handle)
            .and_then(|s| s.active.as_ref().map(Active::voice))
            .map_or(VoiceState::Stopped, |v| self.control.voice_state(v))
    }
    pub fn contains(&self, handle: SourceHandle) -> bool {
        self.sources.contains_key(&handle)
    }
    pub fn options(&self, handle: SourceHandle) -> Option<&PlayOptions> {
        self.sources.get(&handle).map(|s| &s.options)
    }
    pub fn voice(&self, handle: SourceHandle) -> Option<VoiceHandle> {
        self.sources
            .get(&handle)
            .and_then(|s| s.active.as_ref().map(Active::voice))
    }
    /// Reclaims finished producer threads outside the callback, preserving reusable handles/observers.
    pub fn refresh(&mut self) {
        for source in self.sources.values_mut() {
            if source
                .active
                .as_ref()
                .is_some_and(|a| self.control.voice_state(a.voice()) == VoiceState::Stopped)
            {
                source.failure = source
                    .active
                    .as_ref()
                    .and_then(Active::failure)
                    .or_else(|| source.failure.take());
                stop_active(&self.control, &mut source.active);
            }
        }
    }
    pub fn failure(&self, handle: SourceHandle) -> Option<AudioError> {
        self.sources.get(&handle).and_then(|s| {
            s.failure
                .clone()
                .or_else(|| s.active.as_ref().and_then(Active::failure))
        })
    }
    pub fn stream_snapshot(&self, handle: SourceHandle) -> Result<Option<StreamSnapshot>> {
        match self.voice(handle) {
            Some(voice) => self.control.stream_snapshot(voice),
            None => Ok(None),
        }
    }
    pub fn seek(&mut self, handle: SourceHandle, frame: u64) -> Result<StreamGeneration> {
        let source = self.sources.get_mut(&handle).ok_or_else(missing)?;
        match (&source.prepared, &mut source.active) {
            (_, Some(Active::File(worker))) => worker.seek(frame),
            (Prepared::Graph(_, config), Some(Active::Graph(worker))) if frame == 0 => {
                worker.reset_with_parameters(&initial(config))
            }
            _ => Err(AudioError::new(
                "audio.source.seek",
                "Seek requires an active decoded source, or frame zero for a procedural source",
            )),
        }
    }
    pub fn set_parameters(
        &mut self,
        handle: SourceHandle,
        values: &[ParameterUpdate<'_>],
    ) -> Result<()> {
        self.refresh();
        let source = self.sources.get_mut(&handle).ok_or_else(missing)?;
        let Prepared::Graph(program, config) = &mut source.prepared else {
            return Err(AudioError::new(
                "audio.source.parameters",
                "Source is not procedural",
            ));
        };
        program.validate_parameters(values)?;
        if let Some(Active::Graph(worker)) = &source.active {
            worker.set_parameters(values)?;
        }
        for value in values {
            config.parameters.insert(value.name.into(), value.value);
        }
        Ok(())
    }
    pub fn observe(
        &mut self,
        event: impl Into<String>,
        source: SourceHandle,
        action: SourceAction,
    ) -> Result<ObserverHandle> {
        let event = event.into();
        validate_event(&event)?;
        if !self.contains(source) {
            return Err(missing());
        }
        if self.observers.len() >= 1024 {
            return Err(AudioError::new(
                "audio.observer.limit",
                "Observer limit reached",
            ));
        }
        let handle = ObserverHandle(allocate_handle()?);
        self.observers.insert(
            handle,
            Observer {
                event,
                source,
                action,
            },
        );
        Ok(handle)
    }
    pub fn unobserve(&mut self, handle: ObserverHandle) -> bool {
        self.observers.remove(&handle).is_some()
    }
    /// Unknown valid events are a successful no-op. Processing stops at the
    /// first failed action and keeps earlier effects; this is not a transaction.
    pub fn notify(&mut self, event: &str) -> Result<usize> {
        validate_event(event)?;
        // Actions modify source state only; observers stay in registration order.
        let actions: Vec<_> = self
            .observers
            .values()
            .filter(|o| o.event == event)
            .map(|o| (o.source, o.action))
            .collect();
        let mut count = 0;
        for (source, action) in actions {
            self.control(source, action)?;
            count += 1;
        }
        Ok(count)
    }
    pub fn clear(&mut self) {
        for source in self.sources.values_mut() {
            stop_active(&self.control, &mut source.active);
        }
        self.sources.clear();
        self.observers.clear();
    }
}
impl Drop for AudioSources {
    fn drop(&mut self) {
        self.clear();
    }
}
fn missing() -> AudioError {
    AudioError::new("audio.source.missing", "Unknown source")
}
fn validate_event(event: &str) -> Result<()> {
    if event.is_empty() || event.len() > 512 || event.contains('\0') {
        return Err(AudioError::new(
            "audio.observer.invalid",
            "Event must contain 1-512 bytes and no NUL",
        ));
    }
    Ok(())
}
