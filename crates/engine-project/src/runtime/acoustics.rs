//! Runtime hooks for acoustics: host configuration, lifecycle and snapshot.
use super::*;
use engine_acoustics::{AcousticStatus, ParamHook, PropagationBackend, ServiceMode};

impl Runtime {
    /// Supply host propagation backends (GPU tiers, custom backends) and the
    /// service mode. Call after [`Runtime::set_audio`] and before startup. The CPU
    /// backend is always appended. Without this call a scene with an
    /// `engine.AcousticEnvironment` gets a CPU-only synchronous service, so
    /// headless runs work without a GPU.
    pub fn set_acoustics(
        &mut self,
        backends: Vec<Box<dyn PropagationBackend>>,
        mode: ServiceMode,
    ) -> Result<()> {
        if self.started {
            return Err(Error::new(
                "runtime.started",
                "Configure acoustics before startup",
            ));
        }
        self.app
            .world_mut()
            .insert_resource(crate::acoustics::AcousticHost {
                backends: std::sync::Mutex::new(backends),
                mode,
            });
        Ok(())
    }
    /// Install or clear the native parameter hook. It runs on the propagation
    /// worker after each result and may adjust the published frame.
    pub fn set_param_hook(&mut self, hook: Option<ParamHook>) {
        crate::acoustics::set_param_hook(self.app.world_mut(), hook);
    }
    /// The acoustics status; absent without an `engine.AcousticEnvironment`.
    pub fn acoustics_status(&self) -> Option<AcousticStatus> {
        crate::acoustics::status(self.app.world())
    }
    /// Findings of the last acoustic world build (ignored components and similar).
    pub fn acoustics_warnings(&self) -> Vec<String> {
        crate::acoustics::warnings(self.app.world())
    }
    /// Bring acoustics and authored audio in line with the hydrated scene.
    pub(super) fn start_acoustics(&mut self) -> Result<()> {
        crate::acoustics::start(self.app.world_mut())?;
        crate::audio::instantiate_authored(self.app.world_mut())
    }
    pub(super) fn tick_acoustics(&mut self, dt: f32) -> Result<()> {
        let meshes = &self.content.meshes;
        crate::acoustics::tick(self.app.world_mut(), dt, &|id| meshes.get(id).cloned())
    }
}
