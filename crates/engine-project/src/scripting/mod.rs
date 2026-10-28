//! Scripting for the runtime: `script_graph` visual scripts on entities.
//!
//! * `engine.ScriptGraph` ([`ScriptGraph`]): runs a `script_graph` graph asset on
//!   an entity with explicit capabilities, a fuel budget and a fault policy.
//! * The `scriptFunctions` project file ([`ScriptFunctionDecl`]) declares the host
//!   functions graphs may call; [`register_script_fn`] supplies the native
//!   implementations before the runtime starts. The built-in `engine.*` functions
//!   ([`builtin_signatures`]) stage their writes and commit them only when a run
//!   succeeds.
//! * `script.faulted` events and the `scripts` telemetry of the runtime snapshot.
//! * [`reload_asset`]: recompiles a graph asset with last-good semantics.
//!
//! The service runs immediately before the AI services in the tick (after event
//! dispatch and native scripts, before simulation), so a script sees this tick's
//! delivered events and its writes are visible to behavior trees and the physics
//! step. See `docs/scripting.md` and `docs/visual-scripting.md`.
use crate::{ComponentDescriptor, Result};
use engine_core::EngineApp;

mod builtins;
mod check;
mod component;
mod functions;
mod runtime;

pub use builtins::{MAX_EMITS_GLOBAL, MAX_EMITS_PER_INSTANCE};
pub use check::validate_scene;
pub use component::{
    FaultPolicy, MAX_OVERRIDES, SCRIPT_GRAPH, ScriptCapabilities, ScriptGraph, descriptor,
};
pub use functions::{
    MAX_FUNCTIONS, MAX_FUNCTIONS_BYTES, ScriptFunctionDecl, builtin_signatures, parse_declarations,
    register_script_fn, signature_table,
};
pub use runtime::{
    EVENT_FAULTED, FaultStatus, GLOBAL_FUEL_PER_TICK, InstanceStatus, MAX_INSTANCES,
    STATUS_MAX_INSTANCES, ScriptState, ScriptsStatus, prepare, reload_asset, shutdown, status,
    step,
};

pub(crate) use check::project_check;

/// Catalog descriptors of this module's components.
pub fn descriptors() -> Vec<ComponentDescriptor> {
    vec![descriptor()]
}

/// Registers the component adapter and the service resources.
pub fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<ScriptGraph>(app, SCRIPT_GRAPH, 1)?;
    app.world_mut().insert_resource(ScriptState::new()?);
    app.world_mut()
        .insert_resource(builtins::ScriptRun::default());
    Ok(())
}
