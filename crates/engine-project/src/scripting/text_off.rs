//! `text-scripting` feature off: `engine.TextScript` components and `text_script`
//! assets keep their data shape (so projects stay portable) but no Rhai source is
//! parsed or run, and a world that holds a live component fails with
//! `script.text.disabled` instead of silently ignoring it.
use super::{component::TextScript, runtime::InstanceStatus};
use crate::{EngineEvent, Error, Project, Result};
use bevy_ecs::prelude::*;
use engine_script::{HostRegistry, codes};

fn disabled() -> Error {
    Error::new(
        codes::TEXT_DISABLED,
        "this build was compiled without the text-scripting feature",
    )
}

/// Always fails: no Rhai backend is compiled in.
pub(super) fn compile_text_program(_project: &Project, _id: &str) -> Result<()> {
    Err(disabled())
}

pub(super) struct TextState;

impl TextState {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn sync(
        &mut self,
        world: &mut World,
        _registry: &HostRegistry<World>,
        _graph_count: usize,
        _fuel: &mut u64,
    ) -> Result<()> {
        if crate::walk::with::<TextScript>(world).any(|e| e.contains::<TextScript>()) {
            return Err(disabled());
        }
        Ok(())
    }

    pub(super) fn reload(
        &mut self,
        _world: &World,
        _registry: &HostRegistry<World>,
        _id: &str,
    ) -> Result<()> {
        Err(disabled())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn step(
        &mut self,
        world: &mut World,
        registry: &HostRegistry<World>,
        graph_count: usize,
        _dt: f32,
        _time: f64,
        _delivered: &[EngineEvent],
        fuel: &mut u64,
        _tick: u64,
    ) -> Result<()> {
        self.sync(world, registry, graph_count, fuel)
    }

    pub(super) fn shutdown(
        &mut self,
        _world: &mut World,
        _registry: &HostRegistry<World>,
        _fuel: &mut u64,
    ) {
    }
    pub(super) fn len(&self) -> usize {
        0
    }
    pub(super) fn faults(&self) -> u64 {
        0
    }
    pub(super) fn skipped(&self) -> u64 {
        0
    }
    pub(super) fn status(&self) -> Vec<InstanceStatus> {
        Vec::new()
    }
}
