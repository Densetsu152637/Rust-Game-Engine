# Read-only MCP stdio adapter

`engine_mcp::serve(project_root, reader, writer)` serves a fixed project root
through injected `BufRead`/`Write` streams. CLI callers can pass locked standard
input and output. The adapter writes only compact JSON-RPC responses to its
output stream, flushes every response, and never logs to global stdout. Root or
I/O failures are returned to the caller for reporting on stderr. Clean EOF is
successful. Partial final lines and oversized requests emit a parse error and
return `Error::InvalidFraming`; malformed complete lines report a parse error
and allow the session to continue.

This is a port of the existing engine adapter's implemented wire contract,
not an independent claim of compliance with a newer MCP specification:

- `2026-07-28`: stateless `server/discover`, `tools/list`, and `tools/call`.
  Every request includes `params._meta` with the string key
  `io.modelcontextprotocol/protocolVersion` and object key
  `io.modelcontextprotocol/clientCapabilities`. Results carry
  `resultType: "complete"` and `_meta.io.modelcontextprotocol/serverInfo`.
  Unsupported versions return `-32022` with supported/requested version data.
- `2025-11-25`: `initialize`, `notifications/initialized`, then `ping`,
  `tools/list`, or `tools/call`. Initialize negotiates that supported version
  even when the client proposes another date. Repeated initialize is rejected.

Both modes preserve the server name `cpp-game-engine`, version `0.1.0`, and
the same two read-only, idempotent tools:

| Tool | Required arguments | Result |
| --- | --- | --- |
| `project_validate` | `{ "manifest": "project.json" }` | Authored validation status or structured diagnostics |
| `scene_inspect` | `{ "scene": "scenes/start.json" }` | Authored scene entities and components, checked against `project.json`'s asset catalog |

Validation uses `engine_project::Project::load`, including its static content
validation. It reads native Rust script sources but never invokes Cargo,
build scripts, a compiler, game systems, or a remote MCP service. Compiler and
runtime validation remain separate tooling operations. Service diagnostics
retain `code`, `severity`, `file`, `path`, and `message`. Tool failures use
`isError: true`, mirrored JSON text content and `structuredContent`.
Protocol failures use JSON-RPC error envelopes instead.

Only existing project-relative files are accepted. The shared project service
rejects absolute paths, parent traversal, directories, and canonical symlink or
junction escapes. Project catalogs and referenced authored files have the same
confinement. Scene results use the Rust project's serialization schema and
preserve stable authored asset IDs. No prompts, resources, editing, or server
notifications are exposed. Valid notifications produce no response.

Input lines and output messages are limited to 1 MiB; LF is excluded from the
limit. `serve_with_limit` permits a smaller input limit for embedding/testing.
Input storage is bounded even without a newline. String IDs are limited to
1024 UTF-8 bytes; invalid IDs yield bounded errors with null IDs. Oversized
tool results become bounded tool errors, retaining modern result metadata.
The port retains newline framing; Content-Length framing is not supported.

Run `cargo test -p engine-mcp`, `cargo fmt -p engine-mcp --check`, and
`cargo clippy -p engine-mcp --all-targets -- -D warnings` from the workspace.
Tests use in-memory stdio transcripts and temporary authored projects, including
native-build tripwires, malformed/oversized inputs, EOF and I/O errors,
discovery schemas, handshake sequencing, diagnostics, output limits, and real
symlink/junction confinement.
