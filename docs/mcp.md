# MCP stdio adapter

The engine's MCP adapter (`engine-mcp`) is a read-only transport over the shared
project service. Start it through the tooling CLI with `engine mcp <project-root>`
(see [Project commands](commands.md)) and connect an MCP client over standard
input/output, or embed it with
`engine_mcp::serve(project_root, reader, writer)` over any `BufRead` and `Write`.
The project root is fixed for the life of the session and tool paths are relative to
it. The crate README (`crates/engine-mcp/README.md`) is the detailed contract.

The adapter is a port of the earlier engine adapter's implemented wire contract, not
an independent claim of compliance with a newer MCP specification. It supports two
protocol modes:

- **`2026-07-28`** is stateless: `server/discover`, `tools/list` and `tools/call`.
  Every request carries `params._meta` with the string key
  `io.modelcontextprotocol/protocolVersion` and the object key
  `io.modelcontextprotocol/clientCapabilities`. Results carry
  `resultType: "complete"` and `_meta.io.modelcontextprotocol/serverInfo`; an
  unsupported version returns error `-32022` with supported and requested versions.
- **`2025-11-25`** uses `initialize`, `notifications/initialized`, then `ping`,
  `tools/list` and `tools/call`. Initialize negotiates that version even when the
  client proposes another date; a repeated initialize is rejected.

The server name is `cpp-game-engine` (version `0.1.0`) in both modes. Stdout carries
only compact JSON-RPC messages, flushed after each response; the adapter never logs
to stdout.

## Tools

Both tools are read-only and idempotent.

| Tool | Required argument | Result |
| --- | --- | --- |
| `project_validate` | `manifest`: project-relative manifest path | Validation status or structured diagnostics |
| `scene_inspect` | `scene`: project-relative scene path | The authored scene entities and components, checked against the project's asset catalog |

Validation uses `engine_project::Project::load`, including static content
validation, and reads native Rust script sources. It never invokes Cargo, build
scripts, a compiler, game systems or a remote service; compiler and runtime
validation remain separate tooling operations (`engine project validate`).
Diagnostics keep `code`, `severity`, `file`, `path` and `message`. A tool failure
returns `isError: true` with mirrored JSON text content and `structuredContent`;
protocol failures (malformed requests, unknown methods) use JSON-RPC error envelopes
instead.

Only existing project-relative files are accepted. Absolute paths, parent
traversal, directories and symlink or junction escapes are rejected, and the project
catalog and every referenced authored file are confined the same way. Scene results
use the project's serialization schema and keep stable authored asset IDs. No
prompts, resources, editing tools or server notifications are exposed, and valid
notifications produce no response.

## Limits and framing

Input lines and output messages are limited to 1 MiB (LF excluded), and input
storage is bounded even without a newline. `serve_with_limit` allows a smaller input
limit. String request IDs are limited to 1024 UTF-8 bytes; larger IDs get a bounded
error with a null ID. An oversized tool result becomes a bounded tool error. Framing
is newline-delimited; `Content-Length` framing is not supported. Clean EOF is
success; a partial final line or an oversized request emits a parse error and
returns `Error::InvalidFraming`, while a malformed complete line reports a parse
error and the session continues.

References: the MCP [stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio),
[versioning](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning),
[discovery](https://modelcontextprotocol.io/specification/2026-07-28/server/discover)
and [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
specifications, and the legacy [2025-11-25 lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle).

## Tests

`cargo test -p engine-mcp` uses in-memory stdio transcripts and temporary authored
projects, covering native-build tripwires, malformed and oversized input, EOF and
I/O errors, discovery schemas, handshake order, diagnostics, output limits and real
symlink or junction confinement.
