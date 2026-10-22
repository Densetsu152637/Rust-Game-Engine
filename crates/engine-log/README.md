# Engine log

`tracing`-based logging, replacing the legacy C++ `Logger` (bounded history and
coloured console). Library crates emit `tracing` events; only a host binary calls
`init`.

```rust
use engine_log::{Level, LogConfig, LogQuery, init};

let log = init(LogConfig::new().with_history_capacity(2048))?;
tracing::warn!(entity = 7, "slow frame");
let warnings = log.history().query(&LogQuery {
    level: Some(Level::WARN),
    ..Default::default()
});
# Ok::<(), engine_log::InitError>(())
```

- **stderr only.** Console output never touches stdout, which belongs to the MCP
  stdio server and machine-readable CLI JSON. `LogConfig::with_writer` exists for
  tests; do not aim it at stdout.
- **Filtering.** `ENGINE_LOG`, else `RUST_LOG` (standard `tracing` directives,
  e.g. `info,engine_core=debug`), else the default level (INFO). Empty or invalid
  values are skipped. History and console share one filter.
- **Colour.** `Colour::Auto` (stderr is a terminal and `NO_COLOR` unset),
  `Always`, `Never`.
- **History.** `LogHistory` is a cloneable, thread-safe ring buffer of
  `LogRecord { timestamp, level, target, message, fields }`. It evicts the oldest
  record at capacity (default 1024). `records()`, `recent(n)` and `query(&LogQuery)`
  (level, target prefix, text, limit) are available for tools to query; nothing
  in the engine consumes the history yet.
- **Init guard.** `init` installs the global subscriber once; a second call, or
  another installed subscriber, returns `InitError::AlreadyInitialized`.
  `LogConfig::build()` returns the subscriber and history without installing them.

```text
cargo test -p engine-log
cargo clippy -p engine-log --all-targets -- -D warnings
```
