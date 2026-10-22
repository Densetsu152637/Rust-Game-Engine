//! Native Rust game compilation and authoring tools. Game code executes with the
//! host user's native privileges; compilation is not a sandbox.

pub mod arguments;
pub mod artifacts;
pub mod cli;
pub mod compiler;
pub mod mcp_tools;
pub mod play_host;
pub mod runtime_entry;
