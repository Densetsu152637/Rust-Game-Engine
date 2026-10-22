# Rust Game Engine

A 2D-first game engine written in Rust. Simulation runs on a standalone
[Bevy ECS](https://docs.rs/bevy_ecs/) world with explicit, graph-scheduled systems,
the desktop host uses [winit](https://docs.rs/winit/) and [wgpu](https://docs.rs/wgpu/),
and audio uses [CPAL](https://docs.rs/cpal/) with [Hound](https://docs.rs/hound/).
Games are ordinary external Rust crates that the engine compiles on demand, together
with the engine itself, into a native executable generation.

This repository replaces the earlier C++ implementation (EnTT, GLFW, Lua, Vulkan).
Lua scripting and mobile (Android/iOS) support were removed by design. See
[Architecture](docs/architecture.md#design-decisions) for the design decisions.

## Workspace layout

| Path | Purpose |
| --- | --- |
| `crates/engine-core` | `EngineApp`: simulation and render worlds, lifecycle, fixed ticks, graph scheduling, buffered values and change-driven render extraction |
| `crates/engine-project` | Authored project and scene schema, stable IDs, `Runtime`, `NativeScript`, 2D collision, input mapping, modal UI, audio attachment, persistence, CPU render extraction, revisioned scene editing |
| `crates/engine-desktop` | Main-thread winit window loop, wgpu renderer, offscreen readback, gamepad input, headless runner, extensible `NativeRenderPass` |
| `crates/engine-audio` | PCM clips, deterministic stereo mixer, spatial sources, CPAL device output |
| `crates/engine-editor` | Native Windows authoring editor plus a platform-independent `EditorController` |
| `crates/engine-mcp` | Read-only MCP stdio adapter (project validation and scene inspection) |
| `crates/engine-tooling` | Native Rust compiler/launcher, generation management, asset index, shader import, packaging and the `engine` command-line tool |
| `examples/first-project` | Minimal authored project with a game crate and lifecycle fixtures |
| `examples/desktop-2d` | 2D desktop synthetic lab: sprites, text, collision, rooms, audio, saves |
| `examples/native-extension` | Custom components, systems and a custom wgpu render pass |
| `assets/scripts/moving_entity` | Smallest script crate: one entity advanced along +X |
| `docs/` | Documentation, starting at [docs/README.md](docs/README.md) |
| `scripts/ci/` | Helper scripts used by the CI workflow |

The `crates/*` directories form the root Cargo workspace. Each directory under
`examples/` and `assets/scripts/moving_entity` is its own workspace root (an empty
`[workspace]` table), so the engine workspace never builds them and their
`Cargo.lock` and `target` directories are local and ignored.

## Prerequisites

- Rust 1.99.1, pinned in `rust-toolchain.toml` (components `rustfmt` and `clippy`).
  Install it with [rustup](https://rustup.rs/); the toolchain file selects it
  automatically in this checkout.
- Windows is the primary validated platform. The editor is Windows-only; other
  crates are intended to be portable, and Linux builds need the ALSA, udev and
  window-system development packages (see `.github/workflows/ci.yml` for the list
  used by CI). Linux and macOS execution of the desktop host has not been verified
  by the project authors.
- A GPU adapter is needed only for the explicitly ignored rendering tests and for
  visible runs. An audio output device is needed only for native playback.

## Build and test

```text
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Tests that need real hardware are `#[ignore]` and are run explicitly; see
[Testing and CI](docs/testing.md). The standalone example crates are tested with
`--manifest-path`:

```text
cargo test --manifest-path examples/first-project/Cargo.toml
cargo test --manifest-path examples/desktop-2d/Cargo.toml
cargo test --manifest-path examples/native-extension/Cargo.toml
cargo test --manifest-path assets/scripts/moving_entity/Cargo.toml
```

## Using the engine

A game is a Cargo crate with an authored `project.json` next to it. The crate
exposes `configure(&mut EngineApp)`, which registers components, resources, systems
and script factories. The tooling compiler links that function with the engine into
one executable, so game code and engine code share one Rust type universe; Rust
objects never cross independently compiled library boundaries.

```rust
use engine_core::{anyhow, prelude::*};
use engine_project::{NativeScript, Transform, register_script};

pub fn configure(app: &mut EngineApp) -> anyhow::Result<()> {
    register_script(app, "asset:player-script", |_, _| Ok(Box::new(Player)))?;
    Ok(())
}

pub struct Player;
impl NativeScript for Player {
    fn on_update(&mut self, world: &mut World, owner: Entity, dt: f32) -> engine_project::Result<()> {
        if let Some(mut transform) = world.get_mut::<Transform>(owner) {
            transform.position[0] += dt;
        }
        Ok(())
    }
}
```

See [Native game crates](docs/scripting.md) for the full contract, and
[examples/first-project](examples/first-project/README.md),
[examples/desktop-2d](examples/desktop-2d/README.md),
[examples/native-extension](examples/native-extension/README.md) and
[assets/scripts/moving_entity](assets/scripts/moving_entity/src/lib.rs) for real usage.

### Command line

The `engine` binary comes from the `engine-tooling` crate and provides
`project init|validate|assets|shaders|package`, `scene inspect`, `run`, `editor`
and `mcp`. Run it with `cargo run -p engine-tooling --bin engine -- <command>`.
The command surface and its JSON results are documented in
[Project commands](docs/commands.md). Compiling a game generation invokes Cargo,
so a first run can take minutes.

## Documentation

Start at the [documentation index](docs/README.md). Architecture and the ECS
scheduling model are in [Architecture](docs/architecture.md).

## License

MIT; see [LICENSE](LICENSE).
