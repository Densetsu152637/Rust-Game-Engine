use crate::{
    AudioControl, AudioError, Clip, PlayOptions, Result, VoiceHandle, VoiceState, allocate_handle,
};
use std::{collections::BTreeMap, sync::Arc};

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
struct Source {
    clip: Arc<Clip>,
    options: PlayOptions,
    voice: Option<VoiceHandle>,
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
    pub fn create(&mut self, clip: Arc<Clip>, mut options: PlayOptions) -> Result<SourceHandle> {
        options.owner.clone_from(&self.owner);
        options.validate()?;
        if self.sources.len() >= 1024 {
            return Err(AudioError::new(
                "audio.source.limit",
                "Source limit reached",
            ));
        }
        let handle = SourceHandle(allocate_handle()?);
        self.sources.insert(
            handle,
            Source {
                clip,
                options,
                voice: None,
            },
        );
        Ok(handle)
    }
    pub fn configure(&mut self, handle: SourceHandle, mut options: PlayOptions) -> Result<()> {
        let source = self.sources.get_mut(&handle).ok_or_else(missing)?;
        options.owner.clone_from(&self.owner);
        options.validate()?;
        if let Some(voice) = source.voice {
            self.control.configure(voice, options.clone())?;
        }
        source.options = options;
        Ok(())
    }
    pub fn control(&mut self, handle: SourceHandle, action: SourceAction) -> Result<()> {
        let source = self.sources.get_mut(&handle).ok_or_else(missing)?;
        match action {
            SourceAction::Play => {
                // Retire before allocation so a restart succeeds at the voice cap.
                // A failed restart deliberately leaves the source stopped.
                if let Some(voice) = source.voice.take() {
                    self.control.stop(voice);
                }
                source.voice = Some(
                    self.control
                        .play(source.clip.clone(), source.options.clone())?,
                );
            }
            SourceAction::Pause => {
                if let Some(voice) = source.voice {
                    self.control.pause(voice);
                }
            }
            SourceAction::Resume => {
                if let Some(voice) = source.voice {
                    self.control.resume(voice);
                }
            }
            SourceAction::Stop => {
                if let Some(voice) = source.voice.take() {
                    self.control.stop(voice);
                }
            }
        }
        Ok(())
    }
    pub fn remove(&mut self, handle: SourceHandle) -> bool {
        if let Some(source) = self.sources.remove(&handle) {
            if let Some(voice) = source.voice {
                self.control.stop(voice);
            }
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
            .and_then(|s| s.voice)
            .map_or(VoiceState::Stopped, |v| self.control.voice_state(v))
    }
    pub fn contains(&self, handle: SourceHandle) -> bool {
        self.sources.contains_key(&handle)
    }
    pub fn options(&self, handle: SourceHandle) -> Option<&PlayOptions> {
        self.sources.get(&handle).map(|s| &s.options)
    }
    pub fn voice(&self, handle: SourceHandle) -> Option<VoiceHandle> {
        self.sources.get(&handle).and_then(|s| s.voice)
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
        for source in self.sources.values() {
            if let Some(voice) = source.voice {
                self.control.stop(voice);
            }
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
