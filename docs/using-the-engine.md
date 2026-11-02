# Using the engine from your own repository

A game lives in its own repository and depends on the engine through one Cargo
dependency, the `engine` facade crate (`crates/engine`). The facade re-exports the
public API a game uses and forwards the engine's optional Cargo features; it adds no
behaviour. No git submodule is needed.

## Recommended: a git dependency pinned by tag

```toml
[dependencies]
engine = { git = "https://github.com/Densetsu152637/Rust-Game-Engine.git", tag = "v0.1.0" }
```

No release tag has been published yet: until `v0.1.0` exists on the remote, use
`rev = "<commit sha>"` or `branch = "main"` instead of `tag`, or a `path`
dependency on a local checkout. Nothing is published on crates.io yet. Releases are git tags `vX.Y.Z` that match the
workspace `version` in the engine's root `Cargo.toml`; pin the tag you tested with and
bump it deliberately. Most of the engine's third-party dependencies are `=` pinned and
your game's own `Cargo.lock` records the exact engine commit, so a tag is a reproducible
engine version.

Use the facade types everywhere: `engine::prelude::*` for `EngineApp`, the ECS types,
`NativeScript`, `Runtime` and the registration functions; `engine::project`,
`engine::physics`, `engine::audio`, ... for the rest (module names are the crate names
without `engine-`); `engine::desktop` for the window and GPU host. Derive
`Component` and `Resource` through `engine::bevy_ecs` (the prelude brings the name into
scope) so derives resolve against the same copy as the engine.

```rust
use engine::prelude::*;

pub fn configure(app: &mut EngineApp) -> Result<()> {
    register_script(app, "asset:player-script", |_, _| Ok(Box::new(Player)))?;
    Ok(())
}
```

### Features

| Feature | Default | Effect |
| --- | --- | --- |
| `desktop` | yes | `engine::desktop`: winit window, input, wgpu renderer, audio output, headless runner |
| `text-scripting` | yes | Rhai text scripts (`engine.TextScript`). Always on while `desktop` is, because the desktop host depends on `engine-project` with default features |
| `dlss` | no | NVIDIA DLSS Super Resolution through NGX on Vulkan; implies `desktop`. Without `NGX_SDK_DIR`/`VULKAN_HEADERS_DIR` at build time it still builds and DLSS reports the SDK as missing, see [DLSS](dlss.md) |
| `system-clipboard` | no | OS clipboard for game text inputs; implies `desktop` |
| `deterministic-physics` | no | Bit-identical cross-platform physics maths, slower; see [physics queries](physics-queries.md#determinism) |

A GPU-free game (servers, tools, tests) uses
`engine = { ..., default-features = false, features = ["text-scripting"] }`.

## Engine co-development

To change the engine and the game together, keep both repositories side by side and
patch the git source to the local checkout in the **game's** workspace root
`Cargo.toml`:

```toml
[patch."https://github.com/Densetsu152637/Rust-Game-Engine.git"]
engine = { path = "../Rust-Game-Engine/crates/engine" }
```

All engine crates then resolve from the checkout. Remove the patch, bump the tag, and
commit when the engine change is released. A `[patch]` in a non-root manifest is ignored
by Cargo.

## Why not git submodules

- A submodule pins a commit in a detached HEAD; edits there are easily lost or never
  pushed, and every collaborator must remember `--recurse-submodules` and to update it.
- The engine repository is large and its internal layout is a workspace of many crates;
  a nested checkout does not make Cargo treat those crates as one versioned dependency.
- Cargo cannot do version resolution against a submodule path, so the game would carry
  a copy of the engine's dependency pins and drift from them silently.
- A tagged git dependency gives the same reproducibility with one line, and the lockfile
  records the exact commit.

A **fork** is the better choice when you need to carry engine changes the project will
not accept upstream: point the git dependency (and the `[patch]` source URL) at your
fork and rebase it onto new tags. Prefer upstreaming when possible.

## Native prerequisites

- The Rust toolchain in the engine's `rust-toolchain.toml` (currently 1.99.1 with
  `rustfmt` and `clippy`). Your game needs at least the engine's `rust-version`.
- CMake, for the native Opus codec built from source by the voice crates. If CMake is not
  on `PATH`, set the `CMAKE` environment variable (and `CMAKE_GENERATOR` on Windows). See
  [Building quickly](building.md).
- Linux: the ALSA, udev and window-system development packages used by the engine's CI.
- Optional: the NVIDIA NGX SDK for the `dlss` feature ([DLSS](dlss.md)).
- A GPU adapter is needed only to open a window or run GPU tests.

## Data-only projects and projects with a native crate

- **Data-only**: a `project.json`, scenes, assets and visual or Rhai scripts, no Cargo
  crate. Run it with the engine's own `engine` tool or desktop host (see
  [Project commands](commands.md), [Scripting](scripting.md) and the
  `examples/pbr-shadows` project). Nothing here depends on this facade.
- **Native game crate**: a Cargo crate next to `project.json` that depends on `engine`,
  exposes `configure(&mut EngineApp)` and its own `main` calling
  `engine::desktop::run`. See `examples/first-project`, `examples/native-extension` and
  `examples/programmable-world`.

Start from the template in `templates/game` (a `project.json`, a startup scene, and a
minimal game crate whose `Cargo.toml` already pins `engine` by tag). Copy it out of the
engine checkout at the tag you want, rename the package, and keep the tag equal to the
engine version you copied from. A test in `crates/engine` fails if the template's tag
and the workspace version diverge.

`engine run` and editor play work for games that depend on `engine` by `path` or by
`git` (tag, rev or branch), and for games that name individual `engine-*` crates. The
native compiler builds a generated workspace that links `engine-core`, `engine-project`
and `engine-desktop` from the engine checkout that is running. For a git dependency it
adds a `[patch."<git url>"]` section to that generated root manifest, redirecting every
`engine` or `engine-*` crate your game (and the local crates it depends on) takes from
that URL to the same checkout, so the build holds one copy of the engine and your types
match the host's. The patch has to live in the generated root because Cargo ignores a
`[patch]` in a dependency's own manifest. Your game's files are never modified.

What still applies:

- The git source must still be reachable (or already in Cargo's git cache), as for any
  `cargo build`; Cargo fetches it even though the checkout replaces it. Pass `--offline`
  once it is cached.
- The running engine checkout is what gets built, not the tag. Keep it at the version
  your game was written against.
- If the checkout is a git repository with remotes and your git URL is none of them
  (compared ignoring `https`/`ssh` spelling, case, a trailing `.git`, and `file://` versus
  plain local paths), the build stops with `compiler.engine_source` instead of linking two
  copies. Use a `path` dependency or a URL matching the checkout's remote.
- If the checkout has no remote to compare (not a git repository, or no remotes), the
  redirect still happens but the build reports a `compiler.engine_source_unverified`
  warning diagnostic naming the URL: the source is assumed to be this engine.
- Each source Cargo distinguishes in your manifests (for example an `https` and an `ssh`
  spelling of one repository) gets its own `[patch]` entry; spellings Cargo treats as one
  (trailing `/`, `.git`) share one.
  `workspace = true` dependencies are resolved in the workspace of the crate that
  inherits them, including local path dependencies' own workspaces; workspace entries
  nothing inherits are ignored.
- Engine crates reached only through a dependency outside your repository (a registry or
  third-party git crate that itself depends on the engine over git) are not redirected.

## Versioning

Tags `vX.Y.Z` mark releases of the engine workspace version; the facade has the same
version. Pre-1.0 releases can change any public item, so pin the tag and read the
commit log when bumping. The facade re-exports whole crates, so its API surface is the
union of those crates' public APIs.
