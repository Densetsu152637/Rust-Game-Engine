//! The server: a canonical project root, a fixed tool registry and the serve loop.

use crate::{
    config::ServerConfig,
    error::{Error, Result},
    framing,
    protocol::Session,
    registry::Registry,
};
use std::{
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

/// An MCP server bound to one project root. Construct it once and serve any
/// number of independent connections with [`Server::serve`].
pub struct Server {
    pub(crate) root: PathBuf,
    pub(crate) config: ServerConfig,
    pub(crate) registry: Registry,
}

impl Server {
    /// Canonicalises `project_root` and builds the tool registry for `config`.
    /// Fails with [`Error::InvalidRoot`] before any output is produced.
    pub fn new(project_root: impl AsRef<Path>, config: ServerConfig) -> Result<Self> {
        let root = fs::canonicalize(project_root).map_err(|_| Error::InvalidRoot)?;
        if !root.is_dir() {
            return Err(Error::InvalidRoot);
        }
        let registry = Registry::build(&config)?;
        Ok(Self {
            root,
            config,
            registry,
        })
    }

    /// The tools this server lists, in listing order.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Serves one connection until clean EOF. Input and output are compact UTF-8
    /// JSON messages terminated by LF (CRLF input is also accepted). Protocol
    /// errors are written to `writer`; transport errors are returned. Output is
    /// flushed after each response.
    pub fn serve(&self, reader: impl BufRead, writer: impl Write) -> Result<()> {
        let mut session = Session::default();
        let limits = &self.config.limits;
        framing::serve_lines(
            reader,
            writer,
            limits.max_input_message_bytes,
            limits.max_output_message_bytes,
            |request| session.process(self, request),
        )
    }
}
