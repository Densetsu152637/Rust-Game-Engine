# Engine tooling

The native compiler builds external Rust gameplay and engine crates together into
an executable generation. `CompileRequest.main_source` is native Rust supplied by
the engine runner; it calls the project's `gameplay::configure` function directly.
The game can use public Rust engine types, arbitrary components and render passes.
There is no shared-library Rust ABI, command-language interpreter, or serialized
gameplay interface. Native authored code has the user's normal process privileges.

## Native compiler

`NativeCompiler::compile` invokes the selected Cargo/rustc executables, captures
real Cargo JSON diagnostics, and returns a copied executable at a unique path.
The compiler explicitly resolves a generation Cargo.lock using Cargo metadata,
then builds with `--locked`. `NativeCompiler::check` runs `cargo check` on the
same generated graph for compile-only validation; it returns diagnostics, never
produces or replaces a generation, and removes its temporary workspace.

The SHA-256 generation fingerprint includes the resolved lock, dirty files from
every local Cargo package (including external path dependencies), compiler
identity, entry source, selected target/features/profile, build-relevant
environment (`RUSTFLAGS`, `RUSTC_WRAPPER`, `CC`, `CARGO_BUILD_*`,
`CARGO_PROFILE_*`, `CARGO_TARGET_*` except output directories, and similar) and
exactly the out-of-package configuration Cargo reads:

- Cargo runs from a neutral working directory, the selected toolchain's own
  directory (or Cargo home), using `--manifest-path`. `.cargo/config*` files above
  the game package, project or generated cache therefore never apply, and only
  config discovered from that directory and Cargo home is fingerprinted.
- A local package's workspace root manifest is an input only when the package
  inherits workspace fields (`x.workspace = true` or `package.workspace`).
  Unrelated ancestor `Cargo.toml`/`Cargo.lock` files do not affect identity.

Git revision alone is insufficient. Generated build/cache and `cmake-build-*`
directories are excluded. Source changes during compilation reject the candidate;
source symlinks are rejected rather than followed outside roots. The generated
manifest/lock/metadata remain beside a successful executable for diagnosis; a
failed or unresolvable candidate leaves no generation directory.

Cargo dependency output is cached in `<cache>/target`, or in
`CompileRequest.target_dir` to share it between projects. Generation binaries
remain separate so an old Windows process never locks the next candidate's
executable. Failed or cancelled compilation preserves the last successful
artifact. GenerationManager compiles and prepares a candidate behind a startup
barrier before stopping the previous process. No candidate gameplay runs before
clean old-generation teardown. Compile/readiness/cancellation failure preserves
the old process and artifact. Running games receive an owned
`ENGINE_GENERATION_STOP_FILE`; runtime adapters check it at phase boundaries to
allow normal ECS/audio/GPU teardown. After a bounded grace interval, shutdown
force-terminates the owned Windows job or Unix process group. A forced or failed
old shutdown prevents candidate activation. Native job ownership also terminates
Cargo's compiler/build-script descendants on cancellation, including descendants
that outlive Cargo, without process enumeration or shell kill helpers. A normal
reload restarts game state; this API makes no claim of in-memory state migration.

## `engine` command line

Every command except `mcp` prints exactly one JSON line on stdout:
`{"schema":1,"ok":bool,"command":"project validate","result":{...}}`. Failures put
`result.diagnostics` entries with `code`, `severity`, `file`, `path` and `message`
(Rust compiler errors also carry `rendered`; their codes are `rust.E0308` and so
on, with `path` as `line:column`). Human-readable diagnostics go to stderr. Exit
codes: 0 success, 1 project/validation/build failure, 2 invalid arguments (codes
`command.*`), 3 runtime or internal failure. `--format json` is accepted once.

| Command | Result |
| --- | --- |
| `project init <template-manifest> <new-dir>` | Copies a validated template (without generated output) into a new directory |
| `project validate <manifest> [--no-rust]` | Validates the manifest, every scene and content asset, then compile-only checks the Rust crate (`rust` is `null` for content-only projects) |
| `project assets <manifest>` | Writes the stable-ID, content-hashed `.derived/assets.json` index |
| `project shaders <manifest>` | Validates every WGSL shader without a GPU, then publishes content-hashed sources and `index.json` under `.derived/shaders` |
| `project package <manifest> <new-dir> [--profile release\|dev]` | Builds a generation (release by default) and exports `runtime(.exe)`, `game/`, `licenses/`, `generation.json` and `run.cmd`/`run.sh` launchers that work from any working directory |
| `scene inspect <scene> --project-root <dir>` | Validated scene document, entity count and revision |
| `run <manifest> [--headless] [--ticks N] [--input action:tick]... [--user-data dir] [--scene-override file]` | Compiles and runs the generation; forwards its `run` result |
| `editor <manifest>` | Opens the native editor with the compiler-backed play host (Windows) |
| `mcp <project-root>` | Read-only MCP over stdio (JSON-RPC, not the envelope above) |

Script assets require the project's Rust crate (`rust.manifest`, default
`Cargo.toml`); otherwise validate/run/package report `project.rust.missing`.
Package destinations are checked before compiling: they must be new and outside
authored source (generated `build/`, `dist/`, `release/` are allowed). CLI builds
keep the newest three generations per project cache, never removing one with a
live readiness file. `ENGINE_CARGO`/`ENGINE_RUSTC` select the toolchain,
`ENGINE_SOURCE_ROOT` the engine checkout and `ENGINE_NATIVE_TARGET_DIR` a shared
dependency target directory.

The compiled runtime's entry (`runtime_entry::run`) parses the same `run`
arguments, applies an editor scene snapshot if given, runs the desktop runner and
prints one `run` result. Configure errors are `runtime.failed`, panics are
`runtime.panic` (exit 3); undeclared injected input is `command.input.unknown`.

## Editor play host

`play_host::NativePlayHost` implements `engine_editor::PlayHost`. `start` compiles
on a worker thread and launches the isolated project clone with its unsaved scene
as a snapshot and a private user-data directory inside the generation; authored
files and player saves are never written. `poll` reports `Compiling`, `Running`
or `Stopped`; a build or startup failure is `Stopped { success: false }` whose
message lists the compiler errors, and earlier generations remain intact. `stop`
cancels an in-flight build or gracefully stops the game.

## Tests

Run `cargo test -p engine-tooling` from the engine workspace. All tests are
headless. Compiler tests build actual external fixture crates, introduce compiler
errors, check ancestor config/workspace isolation, cancellation and
previous-generation retention. CLI tests check JSON results and error paths.
`runtime_pipeline` builds real engine generations (sharing
`target/native-generation-shared`; the first run compiles the full dependency
graph), runs them headless, exercises editor play, failure retention, runtime
failure reporting and package relocation through the launchers.
