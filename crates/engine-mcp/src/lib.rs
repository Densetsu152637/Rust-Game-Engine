//! Line-delimited JSON-RPC MCP server for authored engine projects.
//!
//! The server is bound to one project root and is read-only unless the caller
//! opts in to more: [`Access::write`] enables scene and file editing tools and
//! [`Access::exec`] enables tools contributed through a [`ToolProvider`] that
//! compile or run project code (see `engine-tooling`). Input and output are
//! injected streams; nothing is ever written to process-global stdout.
//!
//! Module map: [`framing`] (bounded line transport), [`protocol`] (envelopes,
//! version modes, dispatch), [`registry`] (the [`Tool`] trait and registry),
//! [`tools`] (built-in tools by family), [`resources`] (`project://` files),
//! [`paths`] (confinement), [`schema`] (schema and argument helpers).

mod config;
mod error;
mod framing;
mod paths;
mod protocol;
mod registry;
mod resources;
pub mod schema;
mod scrub;
mod server;
mod tools;

pub use config::{Access, AccessLevel, Limits, ServerConfig};
pub use error::{Error, Result};
pub use protocol::{
    LEGACY_PROTOCOL_VERSION, MAX_MESSAGE_BYTES, MODERN_PROTOCOL_VERSION, OLDER_CLIENT_VERSIONS,
};
pub use registry::{
    Annotations, Content, FailureData, Registry, Tool, ToolContext, ToolDefinition, ToolFailure,
    ToolOutcome, ToolProvider, ToolResult,
};
pub use server::Server;
pub use tools::{COMPONENT_NAMES, ComponentDoc, component_docs};

use std::{
    io::{BufRead, Write},
    path::Path,
};

/// Serve a fixed, canonical project root read-only until clean EOF. Input and
/// output are compact UTF-8 JSON messages terminated by LF (CRLF input is also
/// accepted). Protocol errors are sent to `writer`; transport/root errors are
/// returned. Output is flushed after each response. A partial final line or
/// oversized input emits a parse error and terminates with
/// [`Error::InvalidFraming`].
pub fn serve(
    project_root: impl AsRef<Path>,
    reader: impl BufRead,
    writer: impl Write,
) -> Result<()> {
    serve_with_config(project_root, reader, writer, ServerConfig::new())
}

/// As [`serve`], with a configurable input-line limit in bytes, excluding LF.
/// Request storage never exceeds this limit; output retains the 1 MiB limit.
pub fn serve_with_limit(
    project_root: impl AsRef<Path>,
    reader: impl BufRead,
    writer: impl Write,
    max_input_message_bytes: usize,
) -> Result<()> {
    let mut config = ServerConfig::new();
    config.limits.max_input_message_bytes = max_input_message_bytes;
    serve_with_config(project_root, reader, writer, config)
}

/// As [`serve`], with explicit access flags, limits and tool providers.
pub fn serve_with_config(
    project_root: impl AsRef<Path>,
    reader: impl BufRead,
    writer: impl Write,
    config: ServerConfig,
) -> Result<()> {
    Server::new(project_root, config)?.serve(reader, writer)
}
