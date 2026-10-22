//! Engine logging on top of `tracing`.
//!
//! Replaces the legacy C++ `Logger` (bounded history plus coloured console).
//! [`init`] installs one global subscriber that writes **only to stderr** (stdout
//! stays clean for the MCP stdio server and machine-readable CLI output), filters
//! with `ENGINE_LOG`, then `RUST_LOG`, then a default level, optionally colours
//! output, and records events into a bounded in-memory [`LogHistory`] that tools
//! can query. [`LogConfig::build`] returns the same subscriber without installing
//! it, for tests and embedding.

mod history;

use std::io::IsTerminal;

use tracing::level_filters::LevelFilter;
use tracing::subscriber::set_global_default;
use tracing_subscriber::{
    EnvFilter, Registry, fmt, fmt::writer::BoxMakeWriter, layer::SubscriberExt,
};

pub use history::{HistoryLayer, LogHistory, LogQuery, LogRecord};
pub use tracing::Level;

/// Environment variable consulted first for filter directives.
pub const ENGINE_LOG_ENV: &str = "ENGINE_LOG";
/// Fallback environment variable when `ENGINE_LOG` is unset or invalid.
pub const RUST_LOG_ENV: &str = "RUST_LOG";
/// Default number of records retained by the in-memory history.
pub const DEFAULT_HISTORY_CAPACITY: usize = 1024;

/// Whether console output uses ANSI colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Colour {
    /// Colour only when stderr is a terminal and `NO_COLOR` is not set.
    #[default]
    Auto,
    Always,
    Never,
}

impl Colour {
    fn enabled(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
                    && std::io::stderr().is_terminal()
            }
        }
    }
}

/// Logging configuration. Build with the `with_*` methods.
pub struct LogConfig {
    default_level: LevelFilter,
    colour: Colour,
    history_capacity: usize,
    writer: BoxMakeWriter,
    engine_log: Option<String>,
    rust_log: Option<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            default_level: LevelFilter::INFO,
            colour: Colour::Auto,
            history_capacity: DEFAULT_HISTORY_CAPACITY,
            writer: BoxMakeWriter::new(std::io::stderr),
            engine_log: std::env::var(ENGINE_LOG_ENV).ok(),
            rust_log: std::env::var(RUST_LOG_ENV).ok(),
        }
    }
}

impl LogConfig {
    /// Reads `ENGINE_LOG`/`RUST_LOG` from the environment, defaults to INFO,
    /// automatic colour, stderr output and a 1024-record history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Level used when neither environment variable yields a valid filter.
    pub fn with_default_level(mut self, level: impl Into<LevelFilter>) -> Self {
        self.default_level = level.into();
        self
    }

    pub fn with_colour(mut self, colour: Colour) -> Self {
        self.colour = colour;
        self
    }

    /// Maximum number of records the history retains (oldest are evicted).
    pub fn with_history_capacity(mut self, capacity: usize) -> Self {
        self.history_capacity = capacity;
        self
    }

    /// Replaces the console writer. The default is stderr; this exists so tests
    /// can capture output. Never point it at stdout in engine binaries.
    pub fn with_writer<W>(mut self, writer: W) -> Self
    where
        W: for<'a> fmt::MakeWriter<'a> + Send + Sync + 'static,
    {
        self.writer = BoxMakeWriter::new(writer);
        self
    }

    /// Overrides the environment lookup (`None` means unset). Used by tests and
    /// hosts that want a deterministic filter regardless of the process env.
    pub fn with_env_directives(mut self, engine_log: Option<&str>, rust_log: Option<&str>) -> Self {
        self.engine_log = engine_log.map(str::to_owned);
        self.rust_log = rust_log.map(str::to_owned);
        self
    }

    /// Builds the subscriber and its history without installing anything.
    pub fn build(self) -> (impl tracing::Subscriber + Send + Sync + 'static, LogHistory) {
        let history = LogHistory::new(self.history_capacity);
        let filter = resolve_filter(
            self.engine_log.as_deref(),
            self.rust_log.as_deref(),
            self.default_level,
        );
        let console = fmt::layer()
            .with_writer(self.writer)
            .with_ansi(self.colour.enabled())
            .with_target(true);
        // The filter is outermost so history and console see the same events.
        let subscriber = Registry::default()
            .with(filter)
            .with(HistoryLayer::new(history.clone()))
            .with(console);
        (subscriber, history)
    }
}

/// Picks the first valid directive set: `ENGINE_LOG`, then `RUST_LOG`, then the
/// default level. Empty or unparsable values are skipped.
pub fn resolve_filter(
    engine_log: Option<&str>,
    rust_log: Option<&str>,
    default: LevelFilter,
) -> EnvFilter {
    [engine_log, rust_log]
        .into_iter()
        .flatten()
        .filter(|directives| !directives.trim().is_empty())
        .find_map(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::default().add_directive(default.into()))
}

/// Handle returned by [`init`]; keeps the queryable history.
#[derive(Debug, Clone)]
pub struct LogHandle {
    history: LogHistory,
}

impl LogHandle {
    pub fn history(&self) -> &LogHistory {
        &self.history
    }
}

/// Why [`init`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitError {
    /// A global tracing subscriber is already installed in this process.
    AlreadyInitialized,
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInitialized => {
                f.write_str("a global tracing subscriber is already installed")
            }
        }
    }
}

impl std::error::Error for InitError {}

/// Installs the global subscriber. A second call (or any other installed global
/// subscriber) returns [`InitError::AlreadyInitialized`] and changes nothing.
pub fn init(config: LogConfig) -> Result<LogHandle, InitError> {
    let (subscriber, history) = config.build();
    set_global_default(subscriber).map_err(|_| InitError::AlreadyInitialized)?;
    Ok(LogHandle { history })
}
