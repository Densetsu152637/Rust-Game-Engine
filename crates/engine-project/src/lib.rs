//! Authored projects, deterministic desktop gameplay, and CPU render extraction.
//! Native game systems register directly on [`engine_core::EngineApp`].
pub mod acoustics;
pub mod audio;
pub mod catalog;
pub mod content;
pub mod document;
pub mod events;
pub mod gameplay2d;
pub mod input;
pub mod model_import;
pub mod networking;
pub mod persistence;
pub mod rendering3d;
pub mod runtime;
pub mod scheduled_events;
pub mod schema;
pub mod services;
pub mod simulation;
pub mod transforms;
pub mod ui;
pub mod ui_tree;

pub use catalog::{ComponentCatalog, ComponentDescriptor, CustomValue, register_component};
pub use content::GeometryOverride;
pub use document::{EditOperation, SceneDocument};
pub use events::{
    EngineEvent, EventBinding, EventBindings, EventLimits, emit_event, emit_events,
    register_event_handler,
};
pub use input::{ActionFrame, ActionMapper, InputSnapshot, RawInput};
pub use runtime::{
    AuthoredId, NativeScript, RenderFrame, Runtime, RuntimeSnapshot, ScriptFactory, ScriptRegistry,
    register_script,
};
pub use scheduled_events::{EventSchedule, EventTrace, ScheduledEvent};
pub use schema::*;
pub use transforms::{Parent, WorldTransform, refresh_world_transforms, world_transform};

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: String,
    pub severity: String,
    pub file: PathBuf,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Error {
    pub diagnostics: Vec<Diagnostic>,
}

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            diagnostics: vec![Diagnostic {
                code: code.into(),
                severity: "error".into(),
                file: PathBuf::new(),
                path: String::new(),
                message: message.into(),
            }],
        }
    }
    pub fn at(mut self, file: impl Into<PathBuf>, path: impl Into<String>) -> Self {
        if let Some(diagnostic) = self.diagnostics.first_mut() {
            diagnostic.file = file.into();
            diagnostic.path = path.into();
        }
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, item) in self.diagnostics.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            write!(f, "{}: {}", item.code, item.message)?;
        }
        Ok(())
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::new("project.file.io", value.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::new("project.document.malformed", value.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;
