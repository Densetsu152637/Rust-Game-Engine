# MCP server

The engine ships a Model Context Protocol server (`engine-mcp`) so coding agents can
understand, edit, build and run an authored project. Start it through the tooling CLI
with `engine mcp <project-root> [--allow-write] [--allow-exec]` (see
[Project commands](commands.md)) and connect an MCP client over standard
input/output, or embed it with `engine_mcp::serve_with_config` over any `BufRead` and
`Write`. The project root is fixed for the life of the session and every tool path is
relative to it. The crate README (`crates/engine-mcp/README.md`) repeats the contract
for embedders.

The server is **read-only by default**. Editing and execution are opt-in, per server,
by command-line flag; a tool that the flags do not allow is neither listed nor
callable.

## Connecting an agent

Claude Code reads MCP servers from a `.mcp.json` file in the project (or from
`claude mcp add`). With `engine` on `PATH` (or a full path to the binary), a server
that can read the project and edit its scenes:

```json
{
  "mcpServers": {
    "engine": {
      "command": "engine",
      "args": ["mcp", "/absolute/path/to/project", "--allow-write"]
    }
  }
}
```

The project root is the directory that contains `project.json`; use an absolute path
because the client's working directory is not guaranteed. From a checkout of the
engine, use `"command": "cargo"` with
`"args": ["run", "-q", "-p", "engine-tooling", "--bin", "engine", "--", "mcp", "/absolute/path/to/project", "--allow-write"]`
(Cargo writes its progress to stderr only, so stdout stays pure JSON-RPC). Add
`"--allow-exec"` to let the agent build and run the game; the first build compiles
the engine and can take minutes, so give the client a long tool timeout for those
tools. Set `ENGINE_NATIVE_TARGET_DIR` in the server's `env` to share Cargo dependency
output between projects (it also keeps generated paths short on Windows). Other MCP
clients take the same `command` and `args`.

## Access levels

| Level | Flag | Tools | What it can do |
| --- | --- | --- | --- |
| Read | always on | 12 read tools, resources | Inspect the project and read or search its text files. Never changes anything. |
| Write | `--allow-write` | `entity_create`, `entity_update`, `entity_delete`, `component_set`, `component_remove`, `file_write` | Change scenes and text files inside the project root. |
| Exec | `--allow-exec` | `project_build`, `project_run_headless`, `render_screenshot` | Compile and run the project's native game code. |

The three levels are independent: `--allow-exec` does not enable writing. Calling a
gated tool returns a JSON-RPC `-32602` error that names the missing flag.
`project_info` reports the access the server was started with. **Write access lets an
agent author native Rust code (`src/lib.rs`, `Cargo.toml`) that runs with your
privileges the next time anything builds or runs the game; exec access runs it
immediately.** Compilation is not a sandbox. Grant only what the task needs and
review agent-written code.

## Protocol modes

The server is a port of the earlier engine adapter's wire contract, not an
independent claim of compliance with a newer MCP specification. Both modes carry the
same tools, resources and results, and the server name is `cpp-game-engine` (version
`0.1.0`).

- **`2026-07-28`** is stateless: `server/discover`, `tools/list`, `tools/call`,
  `resources/list`, `resources/read` and `resources/templates/list`. Every request
  carries `params._meta` with the string key `io.modelcontextprotocol/protocolVersion`
  and the object key `io.modelcontextprotocol/clientCapabilities`. Results carry
  `resultType: "complete"` and `_meta.io.modelcontextprotocol/serverInfo`; an
  unsupported version returns error `-32022` with supported and requested versions.
- **`2025-11-25`** uses `initialize`, `notifications/initialized`, then `ping` and
  the same list/read/call methods. `initialize` always negotiates `2025-11-25`, even
  when the client proposes another version; this includes the common older client
  versions `2025-06-18` and `2025-03-26`. That is the conservative choice: the server
  does not echo versions it has not been verified against, so a client that cannot
  speak `2025-11-25` will see the mismatch and disconnect. A repeated `initialize` is
  rejected. The result carries `instructions`, a short orientation for the agent. In a
  completed legacy session, requests may carry unrelated `_meta` members such as a
  `progressToken`; they are ignored and the request stays legacy.

Capabilities in both modes: `{"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}}`.
Prompts, completion, logging, subscriptions and server-initiated notifications are
not implemented; valid notifications produce no response. Stdout carries only
compact JSON-RPC messages, flushed after each response; the server never logs to
stdout.

## Tool results and errors

Every tool returns `structuredContent` with a boolean `ok`, mirrored as the first
text content block. Failures set `isError: true` and have this shape:

```json
{"ok": false,
 "error": {"code": "revision_conflict", "message": "…", "hint": "what to do next", "details": {}},
 "diagnostics": [{"code": "…", "severity": "error", "file": "scenes/main.json", "path": "player", "message": "…"}]}
```

`error.code` is a stable machine-readable code, `hint` says how to recover, and
`diagnostics` (when present) are the engine's validation or compiler diagnostics.
Messages may change; codes and paths are the contract. **Results never contain host
paths.** The registry scrubs every result, successes and failures, structured
fields and text content alike: the project root (as `\\?\` verbatim or plain path,
single or doubled backslashes, `/` separators, any case on Windows) becomes a
project-relative path (`C:\proj\src\lib.rs` is reported as `src/lib.rs`, the bare
root as `.`), and every other absolute host path (the cargo home, temp and cache
directories, the user's home) becomes `<host>`. The one exception is payloads that
are project content: `file_read`, `search_text`, `scene_inspect` and `entity_get`
return file text and authored data exactly as stored, because rewriting them would
corrupt what an agent edits and writes back. JSON-RPC protocol errors are reserved for malformed
requests, unknown or gated tools, and arguments that are not part of a tool's
`inputSchema` or that omit a required one (`-32602`). A wrong-typed value is a tool
error with code `invalid_argument`.

Common error codes: `path_invalid`, `path_protected`, `path_link`,
`invalid_argument`, `project_missing`, `entity_not_found`, `asset_not_found`,
`revision_conflict`, `hash_required`, `file_not_found`, `file_too_large`,
`content_too_large`, `not_text`, `line_out_of_range`, `scene_invalid`,
`component_invalid`, `output_too_large`, `internal_error`, and the engine's dotted
diagnostic codes such as `project.asset.unknown`, `scene.camera.follow` or
`scene.entity.id.duplicate`. Exec tools add `compiler.build`, `build_timeout`,
`run_timeout`, `unknown_action`, `rust_crate_missing`, `gpu_unavailable`,
`image_too_large` and `runtime.*` codes.

## Tools

Each tool has a description, a closed `inputSchema` (every property is described,
with enums, bounds and defaults) and, because the result shape is stable, an
`outputSchema` that covers both outcomes (only `ok` is required). Annotations are
advisory.

### Read-only tools (always listed)

All carry `readOnlyHint: true`, `idempotentHint: true`, `openWorldHint: false`.

| Tool | Arguments | Result |
| --- | --- | --- |
| `project_info` | none | Name, schema, manifest, startup scene, all scenes, asset counts by kind, script assets, the Rust crate (manifest, package name), input actions, startup camera settings and the server's access levels. |
| `project_validate` | `manifest` (default `project.json`) | `ok:true` with project name and scene/asset counts, or `ok:false` with diagnostics. Static validation only; it never compiles or runs code. |
| `input_actions` | `include_binding_tokens` | Actions with bindings (and the legacy key), optionally every valid binding token. These are the names scripted input uses. |
| `scene_list` | none | Every scene (startup plus scene assets): path, asset ID, scene ID, entity count, `revision`, validity. |
| `scene_inspect` | `scene` (required), `entities` (ID filter), `offset`, `limit` | The scene's `schema`, `scene_id`, `revision`, `entity_count` and the requested `entities` page, checked against the asset catalog. |
| `entity_get` | `scene`, `id` (both required) | One entity by stable authored ID, the catalog assets it references (id, kind, path) and the scene `revision`. Unknown IDs return suggestions. |
| `component_schema` | `component` (optional enum) | The authored components (`Transform`, `Script`, `MeshRenderer`, `SpriteRenderer`, `Camera2D`, `Collider2D`) as JSON Schema with defaults and a valid example each, an entity schema and the rules the engine enforces. |
| `asset_list` | `kind` (enum), `offset`, `limit` | Catalog entries: ID, kind, path, size. |
| `asset_info` | `asset` (ID) | Kind, path, size, kind-specific details (texture size and format, mesh vertex/triangle count and bounds, font glyphs, script line count, scene entity count) and where the asset is used (scene, entity, component, field), including `MeshMaterial` `shader`, `texture`, `normal_texture` and `metallic_roughness_texture` references. |
| `navigation_query` | `path`, `start`, `goal`, `query` | A bounded path through a saved navigation document: `status` (`reached`, `unreachable` or `budget_exceeded`), `points` and `polygons`. Uses the same query API as native agents; see [Navigation meshes](navigation.md). |
| `acoustics_inspect` | `scene` (default: startup scene), `include_entities` | The scene's acoustic world without running game code: `environment` summary (null without an enabled `engine.AcousticEnvironment`), `world` statistics and bounds, resolved `materials`, box `rooms` with Sabine and Eyring RT60 per band, the 2D `proxy` (metres per unit, enclosed air volume and areas), up to 256 `contributors` and `warnings` (ignored components, a closed room that leaks traced energy). See [Spatial audio and acoustics](acoustics.md). |
| `acoustics_query` | `scene`, `listener` (`position`, `forward`, `up`), `sources` (1-16 `{position, radius_m}`), `quality` | CPU-tier propagation from each source to the listener: `visible_fraction`, `direct_gain_db` per band (125, 500, 2000, 8000 Hz; occlusion only, without the distance law), `distance_m`, up to 16 early `reflections` (order, delay, length, gain, direction), the `reverb` (`rt60_s`, `level_db`, `predelay_ms`, `mean_free_path_m`, `escaped_fraction`) and a `room_estimate` when the listener is inside a `room` volume. Positions are in scene units and a 2D proxy maps them. Deterministic: identical calls return identical JSON. |
| `locale_list` | `max_missing` | The `locale_config`, default locale and per locale: string_table files (with hashes), key count, translation coverage, missing and extra keys. See [Localization tools](#localization-tools). |
| `locale_messages` | `locale` (required), `key`, `prefix`, `args`, `offset`, `limit` | A locale's messages with exact Fluent source, file and line; `key` adds a formatted preview (plural `args`, fallback). |
| `locale_pseudo_preview` | `text` or `key` (+ `locale`, `args`), `options` | Pseudo-localized text (accents, expansion, markers, RTL mirror) with placeables and markup preserved. |
| `file_list` | `path`, `recursive`, `extension`, `glob`, `offset`, `limit` | Sorted, paged entries (`file`/`dir`, size). `glob` (at most 512 bytes, otherwise `invalid_argument`) supports `*`, `**` and `?`; a pattern without `/` matches file names. |
| `file_read` | `path`, `start_line`, `end_line` | UTF-8 text (line endings preserved), `total_lines`, `hash` of the whole file, and `next_start_line` when the output was cut. |
| `search_text` | `query`, `regex`, `case_sensitive`, `path`, `extension`, `glob` (at most 512 bytes), `max_results`, `context_lines` | Matches with path, line, column and text (optionally context), plus counts of scanned, binary and oversized files and why the search stopped. Case-insensitive substring by default. |

Scene results use the project's serialization schema, keep stable authored IDs and
print numbers as their shortest decimal.

### Write tools (`--allow-write`)

Scene tools take `scene` (path), `expected_revision` (the `revision` returned by a
read tool or the previous edit) and optional `dry_run` (validate and report, save
nothing). They return `scene`, `scene_id`, `saved`, `previous_revision`, the new
`revision`, the edited `entity` (null after a delete) and `entity_count`.

| Tool | Extra arguments | Annotations (destructive / idempotent) | Effect |
| --- | --- | --- | --- |
| `entity_create` | `id`, `name`, `components` | no / no | Adds an entity. The ID must be unique in the scene. |
| `entity_update` | `id`, `name`, `components` | yes / yes | Renames the entity and/or replaces its whole component set. |
| `entity_delete` | `id` | yes / yes | Removes the entity; rejected if the scene would become invalid. |
| `component_set` | `entity`, `component`, `value`, `merge` | yes / yes | Replaces one component (omitted fields take defaults) or, with `merge`, overlays fields on the existing one. |
| `component_remove` | `entity`, `component` | yes / yes | Removes one component; removing an absent one is a no-op. |
| `file_write` | `path`, `content`, `expected_hash` | yes / yes | Creates or overwrites a UTF-8 text file. |
| `navigation_bake` | `meshes` (1-128 mesh assets), `output`, `settings`, `obstacles` | no / no | Bakes world-space floor meshes and box obstacles into a new navigation JSON file (never overwrites) and returns the asset entry to add to `project.json`. |
| `acoustics_bake` | `scene`, `output`, `include` (`meshes`, `proxy2d`, `volumes`) | no / no | Writes the scene's acoustic source geometry (meshes, instances, volumes, proxy footprints and resolved materials) as a sealed `AcousticWorldDocument` JSON file after replaying it through the world builder; never overwrites. Returns `path`, `triangles`, `hash` (of the file), `content_hash` (the document's own) and the `acoustics` asset entry to add to `project.json`. |

| `locale_set_message` | `locale`, `ftl` (required), `file`, `expected_hash` | yes / yes | Adds or replaces one Fluent message or term in a locale's `.ftl` file; validated with the Fluent parser first. |
| `locale_remove_message` | `locale`, `id` (required), `file`, `expected_hash` | yes / no | Removes one message or term and its attached comment from a locale's `.ftl` file. |
| `locale_add_locale` | `locale`, `path` (required), `asset_id`, `copy_from` | no / no | Creates a new `.ftl` file (never overwrites), registers it as a `string_table` asset and in the `locale_config` (creating `localization.json` if the project has none). |

All write tools carry `readOnlyHint: false` and `openWorldHint: false`.

**Scene edits** go through engine-project's revisioned `SceneDocument`. The scene is
loaded fresh from disk for every call; its revision (a hash of the normalised scene)
must equal `expected_revision` or the call fails with `revision_conflict` and changes
nothing; the error deliberately does not return the current revision, so the agent
must re-read the scene before retrying. The edited scene is validated against the
whole project (unknown or wrongly typed assets, duplicate IDs, a second camera,
dangling camera follow, sprites without a transform, non-finite numbers, schema
violations) *before* anything is written, and then saved atomically (temporary file
in the same directory, flushed, renamed over the target). A rejected edit returns
diagnostics and leaves the file byte-for-byte unchanged. The scene file is rewritten
in the engine's normalised pretty-printed form. The `scene` argument follows the same
hidden and protected rules as every other path: a scene-shaped file in `.git`,
`.derived` or `target` is `path_invalid` for the scene tools, however it is spelled. A
scene does not have to be listed in the project catalog to be edited (a scene
just created with `file_write` can be edited before it is registered as an asset;
saving still validates it against the catalog's assets), so the catalog is not a
confinement mechanism: the path rules are.

**`file_write`** is for scripts, `Cargo.toml`, notes and also scenes and
`project.json`. Overwriting needs `expected_hash` (the `hash` from `file_read`); a
missing hash is `hash_required`, a stale one `revision_conflict`; omit it only to
create a new file. Parent directories are created inside the root. Writes never
follow a link (a symlink or junction anywhere on the path is refused), never leave the
root, never touch `.git`, `.derived` or `target` (see Security model), never create
Windows device names (`NUL`, `CON`, `PRN`, `AUX`, `COM1`-`COM9`, `LPT1`-`LPT9`, the
superscript-digit forms, `CONIN$`, `CONOUT$`, with or without an extension and in any
case; such paths are `path_invalid` on every platform), are limited to 512 KiB of
UTF-8 without NUL bytes, and are atomic. Whether a write is validated is decided by
comparing the canonical destination with the canonical manifest and catalog scene
files, not by spelling, so `PROJECT.JSON`, `Scenes/Start.json` and 8.3 short names get
the same checks as the exact names. The `path` in the result is the real on-disk
spelling. Scene files known to the project are parsed and validated against the
project before they are written (`validated: "scene"`, plus the resulting
`revision`). `project.json` is validated after the write (`validated: "manifest"`);
if that breaks a project that was valid, the previous bytes are restored and the call
fails with the project's diagnostics and `details.rolled_back: true`. An overwritten
file keeps its permissions (on Unix the temporary file would otherwise reset them to
0600); a newly created file gets the temporary file's default mode (0600 on Unix).

### Localization tools

The `locale_*` tools manage the project's [string tables](localization.md#project-integration).
`locale_list`, `locale_messages` and `locale_pseudo_preview` are read tools; the other three need
`--allow-write` and are hidden (and rejected) without it. Keys are Fluent ids (`message`) or
`message.attribute`; terms are `-name`. Locales are canonicalised (`fr_ca` is `fr-CA`).

- `locale_messages` returns each entry's exact `source`, so an edit is read, changed and sent
  back to `locale_set_message` as `ftl`: one complete entry such as
  `items = { $count ->
    [one] { $count } item
   *[other] { $count } items
 }`.
- Writes replace an entry in place (its `#` comment is kept unless the new text has one) or
  append a new one, leaving the rest of the file byte for byte. The whole locale is parsed with
  the Fluent parser before anything is written: `locale.parse`, `locale.duplicate_message`
  (also across the locale's files), `locale.too_deep`, `locale.ftl.shape` (not exactly one
  entry) or `content_too_large` leave the file unchanged. `expected_hash` (a file `hash` from
  `locale_list`) turns a concurrent change into `revision_conflict`.
- Other codes: `locale_not_configured`, `locale_unknown`, `locale_table_missing` (the `file`
  is not one of the locale's tables), `locale_message_not_found`, `locale_exists`,
  `path_exists`, `asset_exists`, `manifest_unsupported`. Paths follow the usual confinement
  rules (`path_invalid`, `path_protected`).
- After a write the project is reloaded; if it no longer validates, every touched file is
  restored and the failure carries `details.rolled_back: true`.

### Execution tools (`--allow-exec`)

These are provided by `engine-tooling` (the crate that owns the native compiler)
through `engine_mcp::ToolProvider`. They run project code with the host user's
privileges. All carry `readOnlyHint: false`, `openWorldHint: true` (cargo may
download crates; games can do anything).

| Tool | Arguments | Annotations (destructive / idempotent) | Result |
| --- | --- | --- | --- |
| `project_build` | `release`, `timeout_seconds` (10–1800, default 600) | no / yes | A new generation: `generation`, `fingerprint`, `executable`, `profile`, `build_millis`, bounded `warnings`. On failure: compiler diagnostics (`rust.*` code, severity, file, `line:column`, message, rendered text) and `details.last_good_generation`. |
| `project_run_headless` | `ticks` (1–100000, default 60), `inputs`, `scene`, `entities`, `max_entities`, `rebuild`, `build_timeout_seconds`, `timeout_seconds` (5–600, default 120) | yes / no | Bounded final state: `ticks`, `scene_id`, `live_entities`, `active_scripts`, the final entities (component values after scripts ran), the last 200 log lines, trigger events and which generation ran. |
| `render_screenshot` | `scene`, `ticks` (1–10000, default 1), `inputs`, `width`/`height` (16–2048, default 640×360), `rebuild`, the two timeouts | yes / no | An MCP `image` block (`image/png`, base64) plus metadata: size, ticks, GPU adapter, scene ID. |

`project_build` reuses the engine's compiler: each build produces a unique
generation under `.derived/native`; a failed or cancelled build removes only its own
half-built directory, never an earlier generation, and a successful build prunes to
the three newest. Runs execute the generation as a child process exactly like
`engine run --headless`: fixed 1/60 s ticks, no window or audio device, a throw-away
`--user-data` directory (saves and settings are not touched), the project's
declared input actions only. `inputs` is a list of `{action, tick, duration}`
entries (at most 400 ticks of input in total). `scene` runs a different scene than
the startup scene. With `rebuild: false` the generation built earlier in the same
server session is reused when one exists (faster when only scene data changed).
Runs are stopped when `timeout_seconds` elapses (`run_timeout`) and builds are
cancelled when their limit elapses (`build_timeout`). The server reads at most 8 MiB
of the game's stdout; a game that prints more loses the run result that ends the
stream, and the call fails with `output_too_large` (not a misleading `run_no_result`). Calls are served one at a time;
a long build blocks other requests on that connection.

`render_screenshot` uses the engine's real renderer on an offscreen target (the same
path as a window), so sprites, meshes and script-created UI panels appear as in the
game; render features registered by the game run too, and `render_pipeline`
reports their per-camera state. It needs a GPU adapter. A
host without one gets the structured error `gpu_unavailable` (use
`project_run_headless` instead). Images are returned inline, so the PNG must fit the
1 MiB response limit after base64 encoding; otherwise the error `image_too_large`
asks for a smaller size.

## Resources

`resources/list`, `resources/read` and `resources/templates/list` expose the project
files read-only under `project://<relative path>` (forward slashes, percent-encoded).
Listing is sorted and paginated (100 per page, opaque `cursor`/`nextCursor`) and
skips `.git`, `.derived`, `target` and links. Each entry has `uri`, `name` (the
relative path), `mimeType` (by extension: `application/json`, `text/x-rust`,
`application/toml`, `text/markdown`, `image/x-portable-pixmap`, `image/png`,
`audio/wav`, `text/plain`, otherwise `application/octet-stream`) and `size`. Reading
returns UTF-8 text as `text` and anything else as a base64 `blob`, up to 512 KiB.
A path outside the root, through a link, in a hidden directory (`.git`, `.derived`,
`target`, in any case and spelled through 8.3 short names too), missing or not a
regular file is `-32002` "resource not found"; a non-`project://` scheme, a query or
fragment, or an oversized file is `-32602`. The single template is
`project://{+path}`. Subscriptions and change notifications are not supported.

## Security model

- **Root confinement.** The root is canonicalised once. Client paths must be
  project-relative with forward slashes: absolute paths, drive letters, UNC paths,
  `..`, backslashes, `:`, `.git` components and Windows device names are rejected
  before the filesystem is touched, and existing files are canonicalised and must
  still start with the root, so symlink and junction escapes fail for reads,
  listings, searches, resources and edits alike. Listings and walks skip links
  entirely. Writes check every existing component with `symlink_metadata` and refuse
  links even when they point inside the root, then check the canonical ancestor again.
  Rejection messages never echo the requested path or the host's root path;
  diagnostics are rewritten relative to the root.
- **Hidden and protected directories.** `.git`, `.derived` and `target` are never
  listed, searched, read, served as resources or written, at any depth. The rule is
  applied to the *canonical on-disk path*: the deepest existing ancestor of the
  target is canonicalised, taken relative to the canonical root, and every component
  is compared case-insensitively. A case variant (`Target/x`, `.GIT/config`) or an
  8.3 short name (`GIT~1/hooks/pre-commit`, `DERIVE~1/x.txt`) therefore resolves to
  the real directory and is refused like its long name: reads, listings and searches
  report `path_invalid` (resources `-32002`), writes report `path_protected`. The
  same check backs `file_read`, `file_list`, `search_text`, `resources/*`,
  `file_write`, every scene tool and the `scene` argument of the execution tools.
- **Host paths.** Results are scrubbed of host paths centrally (see Tool results and
  errors), so the project root, cargo home and temp directories never reach the agent.
- **Known race.** Path checks and the final rename are separate steps. A process on the
  host that can already write into the project can swap a directory for a link between
  the check and the rename of an atomic write. The server cannot close this without
  platform-specific handle-relative file APIs; it only matters when something other
  than the server is modifying the project concurrently, and the canonical-path checks
  above still apply to what exists at the time of each check.
- **Gating.** Write and exec tools are absent from `tools/list` and rejected by
  `tools/call` unless the server was started with the flag; each tool also re-checks
  the access it needs when it runs.
- **Untrusted input.** Tool arguments, file contents and resources are data. The
  server never evaluates them; the only code it executes is the project's own crate,
  and only through the exec tools.
- **Bounded work.** Requests, responses, file sizes, directory walks, search scans,
  result pages, run time and build time are all limited (next section). Exceeding a
  limit truncates with an explicit flag or fails with a structured error; it never
  produces unbounded output.
- **Clean stdout.** Only JSON-RPC messages are written to stdout. Child processes
  (cargo, the game) never inherit the server's stdout; their diagnostics reach the
  client in tool results and the server's stderr.
- **Accepted risk.** An agent with write access can edit native game code and an
  agent with exec access can run it. This is the same trust decision as letting the
  agent run `cargo run` yourself.

## Limits and framing

Input lines and output messages are limited to 1 MiB (LF excluded), and input storage
is bounded even without a newline. String request IDs are limited to 1024 UTF-8
bytes; larger IDs get a bounded error with a null ID. A tool result above the output
limit becomes the bounded tool error `output_too_large` (a non-tool response becomes
a `-32603` error), and the session continues. Framing is newline-delimited;
`Content-Length` framing is not supported. Clean EOF is success; a partial final line
or an oversized request emits a parse error and returns `Error::InvalidFraming`,
while a malformed complete line reports a parse error and the session continues.

| Limit | Default |
| --- | --- |
| Input message / output message | 1 MiB / 1 MiB |
| `file_read` content per call | 256 KiB (line-boundary cut, `next_start_line`) |
| File scanned by `file_read`, `file_write`, `search_text` (largest file; search also caps a file at 2 MiB and a search at 64 MiB total) | 8 MiB |
| `file_write` content | 512 KiB |
| `resources/read` file | 512 KiB |
| Directory entries visited per listing, search or resource walk | 20,000 |
| Page size (`limit`) | 1000 (default 200); resources 100 |
| `search_text` results | 500 (default 50); a line is clipped at 300 characters |
| Build / run time | 600 s default, 1800 s / 600 s maximum |
| Run output | 8 MiB read (more is `output_too_large`), summary of at most 200 entities (300 KB), 200 log lines |
| `glob` argument | 512 bytes (`invalid_argument` above) |
| Scripted input | 400 ticks of input |

Embedders can change the limits with `ServerConfig::with_limits`.

## Embedding

```rust
use engine_mcp::{ServerConfig, serve_with_config};

// Read-only server over stdio.
serve_with_config(root, std::io::stdin().lock(), std::io::stdout().lock(), ServerConfig::new())?;

// Write access plus extra tools from a provider (as engine-tooling does for exec).
let config = ServerConfig::new().allow_write(true).allow_exec(true).with_provider(MyTools);
```

`engine_mcp::serve(root, reader, writer)` and `serve_with_limit` keep working and are
read-only. `Server::new(root, config)` builds the registry once and can serve many
connections. A provider implements `ToolProvider::tools() -> Vec<Box<dyn Tool>>`;
a `Tool` supplies a `ToolDefinition` (name, title, description, `input_schema`,
`output_schema`, `Annotations`, `AccessLevel`) and a `call(&ToolContext, &args)`
returning `ToolResult` or a structured `ToolFailure`. The registry enforces the
schema's `properties` and `required` members, hides tools above the configured
access, converts panics into `internal_error` and scrubs host paths from every result
(a `ToolResult` that carries project file text can opt out with `.verbatim()`). Providers live in the crate that owns the capability, so `engine-tooling`
depends on `engine-mcp` and not the other way round.

References: the MCP [stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio),
[versioning](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning),
[discovery](https://modelcontextprotocol.io/specification/2026-07-28/server/discover),
[tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) and
[resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)
specifications, and the legacy [2025-11-25 lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle).

## Tests

`cargo test -p engine-mcp` uses in-memory stdio transcripts and temporary authored
projects: framing, malformed and oversized input, EOF and I/O errors, negotiation
(including older client versions), discovery schemas, every tool's success and
failure paths, gating, revision conflicts and atomicity, confinement through real
symlinks and junctions for reads and writes, oversized results, resources, and a
drift guard that pins `component_schema` to engine-project's serde types.
`tests/security.rs` pins the review regressions: hidden directories through case
variants and Windows 8.3 short names (probed at runtime; the 8.3 tests print
`SKIPPED` and pass on volumes without short names, so run them on a volume that has
them, for example `C:` on a default Windows install), device names, host-path
scrubbing and glob length.
`cargo test -p engine-tooling --test mcp_exec` runs the real `engine mcp` binary:
flag parsing, gating, stdout cleanliness, and the execution tools against real native
generations (build, failed build keeping the last good generation, scripted-input
runs, timeouts). The screenshot test needs a GPU and is `#[ignore]`d; run it with
`cargo test -p engine-tooling --test mcp_exec -- --ignored`.

## Shared scene commands

`component_schema` includes project-declared custom types. `component_set` and
`component_remove` accept their namespaced names. `scene_edit` exposes atomic
batches, reparenting and prefab instantiation through the same `SceneDocument`
commands as human controllers, with revision checks and dry runs. See
[metaprogramming](meta-programming.md) for schemas and operation payloads.

## Scheduled events and delivery traces

`project_run_headless` also returns CPU `profile` aggregates and the newest 32
retained frames (`profile_frames_truncated` indicates trimming), optional
`network` status, `input_devices` descriptors, bindings and diagnostics, and
`render_pipeline` (registered render features and their state in the active
camera view, the camera's `render_features` overrides; `not_rendered` headless).
`acoustics` carries `RuntimeSnapshot.acoustics` (requested and active backend,
state, fallback reason, adapter, world statistics, update counters and timings,
mixer counters and at most 32 per-source entries; null without an
`engine.AcousticEnvironment`); `render_screenshot` returns the same object.
Each telemetry field is capped at 64 KiB. Oversized fields become null and are
named in `telemetry_omitted`; absent optional services are simply null.

`project_run_headless` accepts an ordered `events` array of
`{tick,name,source?,target?,payload?}`. The existing exec gate applies; write access
is not required for this ephemeral run input. The result includes `event_trace`,
`event_count` and `event_trace_truncated`, covering actual deliveries across the
run rather than only the final tick. See [scheduled events](scheduled-events.md)
for bounds, stable-ID resolution, atomic enqueue and CLI usage.
