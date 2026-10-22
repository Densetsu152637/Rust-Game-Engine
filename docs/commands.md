# Project commands

> Verification: the command surface below matches `crates/engine-tooling/src/cli.rs`
> and was exercised on Windows against `examples/first-project` (`project init`,
> `validate` with and without `--no-rust`, `assets`, `shaders` including an invalid
> WGSL file, `scene inspect`, headless `run` with `--input`, `package` and its
> `run.cmd` launcher from another directory) together with the exit codes for
> success, validation failure and invalid arguments. `editor` (interactive) was not
> run by hand, and Unix packaging has not been exercised. `mcp` is covered by the
> `engine-mcp` transcript tests and, for the flags and execution tools, by
> `crates/engine-tooling/tests/mcp_exec.rs` (see [MCP](mcp.md)).

## Running the tool

`engine` is the binary target of `engine-tooling`. From a checkout:

```text
cargo run -p engine-tooling --bin engine -- <command> [options]
```

All authored paths resolve relative to the project manifest, independent of the
working directory. Commands that compile a game invoke Cargo and rustc, so a first
build (which compiles the engine crates) can take minutes; later builds reuse the
project's cache under `.derived/native`.

```text
engine project init <template-manifest> <new-directory>
engine project validate <manifest> [--no-rust]
engine project assets <manifest>
engine project shaders <manifest>
engine project package <manifest> <new-directory> [--profile release|dev]
engine scene inspect <scene> --project-root <project-directory>
engine run <manifest> [--headless] [--ticks N] [--input action:tick]... [--user-data dir] [--scene-override file] [--screenshot file.png [--screenshot-size WxH]]
engine editor <manifest>
engine mcp <project-root> [--allow-write] [--allow-exec]
```

Examples (from the repository root, with `engine` standing for the command above):

```text
engine project init examples/first-project/project.json build/my-project
engine project validate build/my-project/project.json
engine scene inspect scenes/main.scene.json --project-root build/my-project
engine run build/my-project/project.json --headless --ticks 120 --input move_right:0
engine project package build/my-project/project.json build/my-package
```

`project init` takes a template's manifest and a new, nonexistent destination
directory.

## Commands

- **`project init`** copies a validated template project into a new directory. The
  destination must not exist and must not overlap authored source; a staging
  directory is renamed into place so no partial result is published. Skips `.git`,
  `target`, `build`, `dist`, `release`, `.derived`, `saves` and `logs`.
- **`project validate`** loads and validates the project (schema, assets, scenes,
  meshes, textures, fonts) and, unless `--no-rust` is given, runs a compile-only
  check of the gameplay crate, returning the compiler diagnostics and generation
  fingerprint. It does not run game code.
- **`project assets`** writes a stable asset index to `.derived/assets.json`: each
  asset's ID, path, kind and content hash. Output is confined to the project root
  and replaced atomically; a symlinked cache location is rejected.
- **`project shaders`** validates every `shader` asset (WGSL, at most 8 MiB) with
  Naga, without a GPU, and publishes a content-hashed copy plus `index.json` under
  `.derived/shaders`.
- **`project package`** compiles the game (release by default, `--profile dev` for a
  debug build) and exports a relocatable directory: `runtime` (`runtime.exe` on
  Windows), `game/` (the project content with `project.json`), `licenses/` (the
  engine licence as `engine-LICENSE`, the generation's lockfile as
  `runtime-Cargo.lock` and per-dependency notices with `dependencies.json`), `generation.json`, `PACKAGE.txt`, and the launchers
  `run.cmd` and `run.sh`. The destination must be new. See
  [Desktop packages](desktop-packaging.md).
- **`scene inspect`** loads a scene, validates it against the project's asset
  catalog and returns its revision, entity count and contents.
- **`run`** compiles and launches the native generation. `--headless` runs fixed
  1/60-second ticks without a window, GPU, gamepad or audio device. Without it the
  game opens a window. `--ticks` accepts 1 to 1,000,000 (default 120).
  `--input action:tick` (repeatable) injects an action at a zero-based tick; an
  absent action releases on the following tick. Actions must be declared by the
  manifest's `inputActions` or `inputBindings`. `--user-data` selects the root for
  saves and settings. `--scene-override` runs a validated scene snapshot while
  keeping the original asset root (used by editor play). `--screenshot file.png`
  simulates the ticks headlessly and then renders the final frame to a PNG through
  an offscreen GPU target (`--screenshot-size WIDTHxHEIGHT`, each 1 to 4096,
  default 640x360); the run result gains a `screenshot` object (`path`, `width`,
  `height`, `bytes`, `adapter`). A host without a usable GPU adapter fails with the
  diagnostic `render.gpu.unavailable`.
- **`editor`** opens the native Windows editor; see [Editor](editor.md).
- **`mcp`** serves an MCP server for one project root on stdin/stdout; see
  [MCP](mcp.md). It is read-only by default. `--allow-write` additionally lists the
  scene and file editing tools; `--allow-exec` additionally lists the tools that
  build and run the game (`project_build`, `project_run_headless`,
  `render_screenshot`; these execute project code with your privileges). The flags
  are independent, may appear in any order, and may not repeat. Stdout carries
  JSON-RPC messages only; usage errors (exit 2) and transport errors (exit 1) go to
  stderr.

## Output and exit status

Every command except `mcp` prints exactly one JSON document to stdout, including on
failure (the editor prints `{"closed":true}` after its window closes):

```json
{"schema":1,"ok":false,"command":"project validate","result":{"diagnostics":[{"code":"project.asset.missing","severity":"error","file":"assets/x.png","path":"","message":"..."}]}}
```

Result fields are `schema`, `ok`, `command` and a command-specific `result`.
Diagnostics have `code`, `severity`, `file`, `path` and `message`; compiler
diagnostics use `rust.<code>` codes and also carry the rendered compiler text.
Codes and paths are the machine contract; messages may change. `--format json` is
accepted and is the only format. Human-readable diagnostics and game logs go to
stderr. For `run`, the compiled game prints the run result (`ticks`, `fixed_delta`,
the final scene snapshot and `presented_frames`) and the launcher forwards it.

Exit status: `0` success, `1` project, validation or build failure, `2` invalid
command arguments, `3` runtime or internal failure.

For the shipped sample projects, equal ticks and input produce equal normalized
state on the same platform; the fixtures use no random values. This is not a
guarantee for arbitrary native code, external effects, other architectures or GPU
pixels.

## Environment

- `ENGINE_CARGO`, `ENGINE_RUSTC`: select the Cargo and rustc executables used to
  compile games (default: the Cargo that built the tool, and its sibling rustc).
- `ENGINE_SOURCE_ROOT`: engine checkout containing `crates/engine-core`. When set it
  takes precedence (a directory without `crates/engine-core` is an argument error,
  exit `2`); otherwise the tool uses the checkout it was built from, then searches
  the directories above its executable.
- `ENGINE_NATIVE_TARGET_DIR`: shared Cargo target directory for generation builds.

## Compilation model

Games compile into a fresh generation directory with `--locked` against a resolved
lockfile; the generation fingerprint covers the resolved lock, dirty local
packages, compiler identity, features and relevant environment. A failed or
cancelled build keeps the previous good artifact. Native code runs with the user's
normal privileges; compilation is not a sandbox. See `crates/engine-tooling/README.md`
and [Native game crates](scripting.md).
