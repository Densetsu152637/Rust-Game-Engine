# engine-mcp

A Model Context Protocol server for authored engine projects. It is a library over
injected `BufRead`/`Write` streams; the `engine mcp` command in `engine-tooling` runs
it on stdin/stdout. The full wire contract, tool reference and security model are in
[`docs/mcp.md`](../../docs/mcp.md); this page covers the crate API and the contract
embedders rely on.

```rust
use engine_mcp::{ServerConfig, serve, serve_with_config};

// Read-only (the default), fixed project root, until clean EOF.
serve(root, std::io::stdin().lock(), std::io::stdout().lock())?;

// Opt in to editing; add execution tools through a provider.
let config = ServerConfig::new().allow_write(true).allow_exec(true).with_provider(MyTools);
serve_with_config(root, reader, writer, config)?;
```

## Modules

| Module | Concern |
| --- | --- |
| `framing` | Line-delimited transport: bounded reads, one compact message per line, flush per message. |
| `protocol` | JSON-RPC envelopes, the two protocol modes, version negotiation, method dispatch. |
| `registry` | `Tool`, `ToolProvider`, `ToolDefinition`, `ToolResult`, `ToolFailure`, `ToolContext` and the `Registry` that gates, validates and runs tools. |
| `config` | `ServerConfig`, `Access`, `AccessLevel`, `Limits`. |
| `tools/` | Built-in tools by family: `project`, `scene`, `components`, `assets`, `files` (read and `file_write`), `edit` (scene edits). |
| `resources` | `project://` resources. |
| `paths` | Confinement, bounded reads, atomic writes, globbing, percent coding. |
| `scrub` | Host-path scrubbing applied to every tool result (project root to relative paths, other host paths to `<host>`). |
| `schema` | JSON Schema builders and typed argument readers. |
| `error` | Transport errors (`Error::Io`, `InvalidRoot`, `InvalidFraming`, `InvalidConfig`). |

`engine_mcp::serve(root, reader, writer)` and `serve_with_limit(..., max_input)` are
unchanged and read-only. `Server::new(root, config)` builds the registry once and
serves any number of connections; `Server::registry()` lists the active tools.
Root or I/O failures are returned to the caller (the CLI prints them on stderr), clean
EOF is success, a partial final line or an oversized request emits a parse error and
returns `Error::InvalidFraming`, and a malformed complete line reports a parse error
and the session continues. Nothing is ever written to process-global stdout.

## Protocol modes

A port of the engine adapter's implemented wire contract, not an independent claim of
compliance with a newer MCP specification.

- `2026-07-28`: stateless `server/discover`, `tools/list`, `tools/call`,
  `resources/list`, `resources/read`, `resources/templates/list`. Every request carries
  `params._meta` with the string key `io.modelcontextprotocol/protocolVersion` and the
  object key `io.modelcontextprotocol/clientCapabilities`. Results carry
  `resultType: "complete"` and `_meta.io.modelcontextprotocol/serverInfo`. Unsupported
  versions return `-32022` with supported/requested version data.
- `2025-11-25`: `initialize`, `notifications/initialized`, then `ping` and the same
  methods. `initialize` always negotiates `2025-11-25`, whatever the client proposes
  (`2025-06-18` and `2025-03-26` included; see `OLDER_CLIENT_VERSIONS`). A repeated
  initialize is rejected. Unrelated `_meta` members (such as a progress token) on
  requests in a completed legacy session are ignored.

Capabilities in both modes are tools and resources (no `listChanged`, no
subscriptions). Name `cpp-game-engine`, version `0.1.0`. Valid notifications produce
no response.

## Access levels and tools

Read-only tools are always listed; the others only when `ServerConfig` allows their
`AccessLevel`.

| Level | Tools |
| --- | --- |
| Read | `project_info`, `project_validate`, `input_actions`, `scene_list`, `scene_inspect`, `entity_get`, `component_schema`, `asset_list`, `asset_info`, `file_list`, `file_read`, `search_text` |
| Write (`allow_write`) | `entity_create`, `entity_update`, `entity_delete`, `component_set`, `component_remove`, `file_write` |
| Exec (`allow_exec`, from a provider) | `project_build`, `project_run_headless`, `render_screenshot` (implemented by `engine-tooling::mcp_tools`) |

A gated tool is absent from `tools/list` and `tools/call` fails with `-32602` naming the
missing access. Results are `structuredContent` `{ok, ...}` mirrored as text;
failures are `{ok:false, error:{code, message, hint}}` plus `diagnostics` (`code`,
`severity`, `file`, `path`, `message`) when the engine reports them, with
`isError: true`. Protocol errors (`-32602`) cover malformed `tools/call` requests,
unknown or gated tools, unknown arguments and missing required arguments. Each tool
declares a closed `inputSchema`, an `outputSchema` and accurate annotations.

Scene edits are optimistic: `expected_revision` must equal the scene's current
revision (from `scene_inspect`/`entity_get`/the previous edit), the edited scene is
validated against the whole project through engine-project's `SceneDocument` before
an atomic save, and a rejected edit leaves the file untouched. `file_write` needs the
`hash` from `file_read` to overwrite, never follows links, refuses `.git`, `.derived`
and `target` (judged on the canonical on-disk path, case-insensitively and at any
depth, so case variants and Windows 8.3 short names such as `GIT~1` cannot reach
them; the same names are not readable, listable or served either), refuses Windows
device names (`NUL`, `COM1`, `aux.txt`, ...), and validates scenes (before) and
`project.json` (after, with rollback) by comparing canonical files.

## Adding tools

```rust
struct MyTools;
impl engine_mcp::ToolProvider for MyTools {
    fn tools(&self) -> Vec<Box<dyn engine_mcp::Tool>> { vec![Box::new(MyTool)] }
}
```

A `Tool` returns a `ToolDefinition` (name, title, description, closed `input_schema`,
optional `output_schema`, `Annotations`, `AccessLevel`) and implements
`call(&ToolContext, &Map<String, Value>) -> Result<ToolResult, ToolFailure>`. The
registry rejects arguments outside `properties` and missing `required` ones before
the call, scrubs host paths from every result (the project root becomes a
project-relative path, other absolute host paths such as the cargo home become
`<host>`; mark a result `ToolResult::verbatim()` when its payload is project file
text that must not be rewritten), converts panics into
`internal_error`, drops tools above the configured access and rejects duplicate
names. Use `cx.existing_file` for client paths and `cx.require(level)` as a second
gate. `engine-tooling` registers its execution tools this way, so the dependency
points from tooling to this crate.

## Limits

Defaults (all in `Limits`): 1 MiB per input line and per output message (LF excluded;
longer results become the bounded tool error `output_too_large`), 256 KiB of text per
`file_read`, 8 MiB per scanned file, 512 KiB per `file_write` and per resource, 20,000
visited directory entries, 1000 items per page, 500 search results, string request IDs
up to 1024 bytes. Framing is newline-delimited; `Content-Length` framing is not
supported.

## Tests

Run `cargo test -p engine-mcp`, `cargo fmt -p engine-mcp --check` and
`cargo clippy -p engine-mcp --all-targets -- -D warnings` from the workspace. The
suites use in-memory stdio transcripts and temporary authored projects:

- `tests/protocol.rs`, `tests/stdio.rs`: envelopes, framing, negotiation, discovery,
  listing, strictness, confinement, output limits, I/O failures.
- `tests/tools.rs`: every read tool's success and failure paths, paging, bounds and
  confinement through real symlinks and junctions.
- `tests/write.rs`: gating, annotations, revision conflicts, validation without
  partial writes, dry runs, atomicity, `file_write` rules and write confinement.
- `tests/resources.rs`: list, pagination, read, templates, escapes, plus the drift
  guard that pins `component_schema` to engine-project's serde types.
- `tests/server.rs`: configuration, duplicate tools, panics, output and input limits.
- `tests/security.rs`: hidden and protected directories through case variants and
  Windows 8.3 short names (skipped with a printed note on volumes without short
  names), device names, host-path scrubbing, glob length and file modes.

The execution tools are tested in `crates/engine-tooling/tests/mcp_exec.rs` against
real native generations.
