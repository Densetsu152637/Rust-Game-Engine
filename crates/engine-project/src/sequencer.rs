//! Authored cinematic timelines with explicit scene pose authority.
mod authority;
mod origin;
mod playback;
use crate::{ComponentDescriptor, Error, Result};
use bevy_ecs::prelude::*;
use engine_sequencer::{Player, Timeline};
pub(crate) use origin::RootRebases;
pub(crate) use playback::origin_shifted;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

pub use engine_sequencer::{Direction, EventCrossing, Property, SeekEvents, State};
pub use playback::{
    SequenceEvent, SequenceEvents, SequenceStatus, command, prepare, reset, status, step,
};
pub const SEQUENCE_PLAYER_COMPONENT: &str = "engine.SequencePlayer";

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SequencePlayer {
    pub timeline: Timeline,
    /// Empty binds index zero to the owner. Nonempty lists use authored entity IDs.
    pub bindings: Vec<String>,
    pub autoplay: bool,
    pub looping: bool,
    pub speed: f64,
}
impl Default for SequencePlayer {
    fn default() -> Self {
        Self {
            timeline: Timeline::default(),
            bindings: vec![],
            autoplay: true,
            looping: false,
            speed: 1.,
        }
    }
}
impl SequencePlayer {
    pub fn validate(&self) -> Result<()> {
        let mut unique = BTreeSet::new();
        if self.bindings.len() > engine_sequencer::MAX_BINDINGS
            || self.bindings.iter().any(|id| {
                id.is_empty()
                    || id.len() > 128
                    || id.chars().any(char::is_control)
                    || !unique.insert(id)
            })
        {
            return Err(invalid("Sequence bindings must be unique entity IDs"));
        }
        let compiled = self
            .timeline
            .compile(self.bindings.len().max(1))
            .map_err(mapped)?;
        let mut player = Player::new(std::sync::Arc::new(compiled));
        player.set_speed(self.speed).map_err(mapped)
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Command {
    Play,
    Pause,
    Stop,
    Seek { position: f64, events: SeekEvents },
    Speed(f64),
}

pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: SEQUENCE_PLAYER_COMPONENT.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "timeline":{"type":"object"},
            "bindings":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"maxLength":128}},
            "autoplay":{"type":"boolean"},"looping":{"type":"boolean"},
            "speed":{"type":"number","minimum":-100,"maximum":100}
        }}),
        defaults: serde_json::to_value(SequencePlayer::default())
            .expect("sequence defaults serialize"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}
pub fn validate_component(name: &str, data: &serde_json::Value) -> Result<()> {
    if name == SEQUENCE_PLAYER_COMPONENT {
        serde_json::from_value::<SequencePlayer>(data.clone())?.validate()?;
    }
    Ok(())
}
pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<SequencePlayer>(app, SEQUENCE_PLAYER_COMPONENT, 1)?;
    app.world_mut().init_resource::<playback::Sequences>();
    app.world_mut().init_resource::<SequenceEvents>();
    Ok(())
}
fn invalid(message: impl Into<String>) -> Error {
    Error::new("sequence.invalid", message)
}
fn mapped(error: impl std::fmt::Display) -> Error {
    invalid(error.to_string())
}
