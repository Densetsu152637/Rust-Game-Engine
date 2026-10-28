//! The backend-neutral script instance.
use crate::{
    host::{ScriptHost, Trigger},
    trap::{Budget, Fault},
};

/// Most variables a [`InstanceDebug`] lists.
pub const MAX_DEBUG_VARIABLES: usize = 32;

/// Bounded, deterministic view of an instance for telemetry and tools.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstanceDebug {
    /// Accounted state bytes (variables, node state, pending waits).
    pub state_bytes: usize,
    /// Latent waits or timers currently pending.
    pub pending: usize,
    /// `(name, rendered value)` of the first variables, sorted by name.
    pub variables: Vec<(String, String)>,
}

/// One running script. A backend (graph VM, text) implements it; the engine drives
/// it with triggers and never needs to know which backend it is.
pub trait ScriptInstance: Send + Sync {
    /// Runs the script's handler(s) for `trigger`.
    ///
    /// # Errors
    /// A [`Fault`]: the instance trapped. The caller discards the run's staged
    /// effects and applies the fault policy; the instance state is then undefined
    /// and the instance must not be used again unless freshly created.
    fn handle(
        &mut self,
        trigger: &Trigger<'_>,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<(), Fault>;
    /// Event names this instance wants delivered (static, sorted).
    fn subscriptions(&self) -> &[String];
    fn debug(&self) -> InstanceDebug;
}
