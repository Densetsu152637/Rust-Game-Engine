//! The script core: typed values, the host function seam, budgets and traps, and
//! the `script_graph` visual-scripting backend.
//!
//! This crate is pure: it depends only on `engine-graph` and serde, never touches
//! the ECS, the file system, the network or a clock, and never runs game code on
//! its own. The engine (`engine_project::scripting`) supplies a [`ScriptHost`].
//!
//! * [`value`]: [`Value`], [`ValueType`], [`EntityRef`] and boundary validation.
//! * [`host`]: [`FnSig`], [`HostRegistry`], [`Capabilities`], [`ScriptHost`].
//! * [`trap`]: [`Trap`], [`Fault`], [`Site`] and the per-tick [`Budget`].
//! * [`instance`]: the backend-neutral [`ScriptInstance`].
//! * [`codes`]: every stable diagnostic and trap code, with an `ALL` list.
//! * [`graph`]: the `script_graph` kind, compiler and VM.
//!
//! See `docs/scripting.md` and `docs/visual-scripting.md`.
pub mod codes;
pub mod graph;
pub mod host;
pub mod instance;
pub mod trap;
pub mod value;

pub use host::{
    Capabilities, Effects, EmitScope, FnId, FnParam, FnSig, HostRegistry, NativeCx, NativeFn,
    ScriptHost, SigTable, Signatures, Trigger, check_args, valid_function_name,
};
pub use instance::{InstanceDebug, ScriptInstance};
pub use trap::{Budget, Fault, ScriptError, Site, Trap};
pub use value::{EntityRef, MAX_STRING_BYTES, Value, ValueType, validate_value};
