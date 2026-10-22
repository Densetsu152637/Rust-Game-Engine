//! Server configuration: access levels, resource limits and extra tool providers.

use crate::registry::ToolProvider;
use std::sync::Arc;

/// What a tool is allowed to do. Read-only tools are always available; the other
/// levels are opt-in per server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessLevel {
    /// Inspects the project; never changes anything on disk.
    Read,
    /// Edits project files (scenes, text files) inside the project root.
    Write,
    /// Compiles or runs project code with the host user's privileges.
    Exec,
}

impl AccessLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Exec => "exec",
        }
    }
}

/// The opt-in capabilities a server was started with. The default is read-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub write: bool,
    pub exec: bool,
}

impl Access {
    pub const READ_ONLY: Self = Self {
        write: false,
        exec: false,
    };

    pub const fn allows(self, level: AccessLevel) -> bool {
        match level {
            AccessLevel::Read => true,
            AccessLevel::Write => self.write,
            AccessLevel::Exec => self.exec,
        }
    }
}

/// Bounds applied to requests, results and file operations. Every field is a
/// hard limit; tools truncate or refuse rather than exceed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Longest accepted input line in bytes, LF excluded.
    pub max_input_message_bytes: usize,
    /// Longest emitted message in bytes, LF excluded. Longer tool results are
    /// replaced by a bounded tool error.
    pub max_output_message_bytes: usize,
    /// Largest text returned by one `file_read`.
    pub max_file_read_bytes: usize,
    /// Largest file `file_read`, `file_write` and `search_text` will scan.
    pub max_file_scan_bytes: usize,
    /// Largest content accepted by `file_write`.
    pub max_file_write_bytes: usize,
    /// Largest file served by `resources/read`.
    pub max_resource_bytes: usize,
    /// Directory entries visited by one listing, search or resource walk.
    pub max_walk_entries: usize,
    /// Upper bound for the `limit` argument of paginated tools.
    pub max_page_size: usize,
    /// Upper bound for `search_text` results.
    pub max_search_results: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_message_bytes: crate::protocol::MAX_MESSAGE_BYTES,
            max_output_message_bytes: crate::protocol::MAX_MESSAGE_BYTES,
            max_file_read_bytes: 256 * 1024,
            max_file_scan_bytes: 8 * 1024 * 1024,
            max_file_write_bytes: 512 * 1024,
            max_resource_bytes: 512 * 1024,
            max_walk_entries: 20_000,
            max_page_size: 1000,
            max_search_results: 500,
        }
    }
}

/// Builder-style server configuration.
///
/// ```
/// use engine_mcp::ServerConfig;
/// let config = ServerConfig::new().allow_write(true);
/// assert!(config.access.write && !config.access.exec);
/// ```
#[derive(Clone, Default)]
pub struct ServerConfig {
    pub access: Access,
    pub limits: Limits,
    pub(crate) providers: Vec<Arc<dyn ToolProvider>>,
}

impl ServerConfig {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn allow_write(mut self, allowed: bool) -> Self {
        self.access.write = allowed;
        self
    }
    pub fn allow_exec(mut self, allowed: bool) -> Self {
        self.access.exec = allowed;
        self
    }
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
    /// Registers extra tools. Provider tools are only listed and callable when
    /// the configured [`Access`] allows their declared level.
    pub fn with_provider(mut self, provider: impl ToolProvider + 'static) -> Self {
        self.providers.push(Arc::new(provider));
        self
    }
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("access", &self.access)
            .field("limits", &self.limits)
            .field("providers", &self.providers.len())
            .finish()
    }
}
