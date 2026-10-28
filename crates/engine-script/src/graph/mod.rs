//! `script_graph`: visual scripting on the shared graph model.
//!
//! * [`kind`](mod@kind): the graph kind, its node types, ports and parameters.
//! * [`compile`]: kind check and code generation into an immutable [`Program`].
//! * [`program`]: the compiled form and instance creation.
//! * [`vm`]: [`GraphInstance`], the non-recursive register VM.
//!
//! See `docs/visual-scripting.md`.
pub mod compile;
pub mod kind;
pub mod program;
pub mod vm;

pub use compile::{MAX_STATIC_LOOP, check_document, compile};
pub use kind::{KIND, kind, node, port, shared_kind, valid_event_name};
pub use program::{EntryKey, MAX_OPS, MAX_REGS, Program};
pub use vm::{GraphInstance, MAX_WAIT_SECONDS};
