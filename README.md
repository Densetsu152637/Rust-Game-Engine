# Rust Game Engine

A Rust game engine with 2D and 3D scene rendering, native game code, and shared
human/agent authoring tools. Simulation runs on a standalone
[Bevy ECS](https://docs.rs/bevy_ecs/) world with explicit, graph-scheduled systems,
the desktop host uses [winit](https://docs.rs/winit/) and [wgpu](https://docs.rs/wgpu/),
and audio uses [CPAL](https://docs.rs/cpal/) with [Hound](https://docs.rs/hound/).
Games are ordinary external Rust crates that the engine compiles on demand, together
with the engine itself, into a native executable generation.

Cargo builds the engine, tools and game generations; there is no additional build
system. This repository replaces the earlier C++ implementation (EnTT, GLFW, Lua,
Vulkan). Lua scripting is replaced by Rust game crates. Desktop hosts target
Windows, Linux and macOS; mobile, browser and console hosts are out of scope. See [Platforms](docs/platforms.md) for exact capability boundaries and
[Architecture](docs/architecture.md#design-decisions) for the design decisions.

Keyboard, mouse/trackpad and controller actions support remapping. Native games
can inject custom peripheral adapters, including accessibility devices, and offer
hold/toggle actions. OS-emulating devices use the standard input paths; proprietary
protocols need an adapter. See [Custom and accessible input](docs/custom-input.md).

## Workspace layout

| Path | Purpose |
| --- | --- |
| `crates/engine-ecs` | ECS layer: Bevy ECS and glam re-exports, `Current`/`Next`/`Buffered` values, change-driven render-world extraction |
| `crates/engine-log` | `tracing`-based logging: stderr-only subscriber, `ENGINE_LOG` filtering, bounded queryable `LogHistory` |
| `crates/engine-core` | `EngineApp`: simulation and render worlds, lifecycle, fixed ticks, graph scheduling, measured CPU profiles (re-exports `engine-ecs`) |
| `crates/engine-render` | GPU-free graphics adapter interface: `RenderBackend`, `RenderDevice`, `RenderCommandBuffer`, WGSL validation, recording test backend |
| `crates/engine-render-wgpu` | The wgpu backend: `GpuContext`, offscreen targets, Vulkan/DX12/Metal/GL selection |
| `crates/engine-assets` | Bounded glTF/GLB import, dependency fingerprints and typed generational asset ownership |
| `crates/engine-physics` | Rapier 2D/3D bodies, contacts, sensors, queries, fixed/revolute joints and impulses |
| `crates/engine-animation` | Skeletal poses, clip playback, crossfades, events and CPU skinning |
| `crates/engine-navigation` | Bounded layered polygon NavMesh baking and pathfinding, weighted-grid/graph searches and waypoint following |
| `crates/engine-text` | CPU text engine: font library with script/language fallback, shaping, bidi, line breaking, rich-text markup, caret geometry, coverage/SDF glyph atlases and a glyph WGSL shader |
| `crates/engine-locale` | Localization: Project Fluent string tables with plural/gender selection, locale fallback and negotiation, runtime language switch, missing-translation reports, per-locale fonts/assets and pseudo-localization |
| `crates/engine-network` | Opt-in authoritative sessions over mutual-TLS 1.3 (or explicit plaintext trusted-LAN TCP), ownership-checked input, snapshots and interpolation |
| `crates/engine-platform` | Explicit host capabilities, unsupported-host errors and native package host metadata |
| `crates/engine-project` | Authored schemas and typed component catalog, hierarchy, callbacks, simulation/network/UI adapters, runtime snapshots, persistence and transactional scene/prefab editing |
| `crates/engine-desktop` | Main-thread winit host, 2D sprites and depth-tested 3D scenes, materials/custom WGSL, input, headless runner and native render passes |
| `crates/engine-dlss` | Optional standalone NVIDIA DLSS Super Resolution (NGX/Vulkan) crate; not part of the renderer's default path |
| `crates/engine-audio` | PCM clips, deterministic stereo mixer, spatial sources, binaural/reverb/Doppler acoustic DSP, CPAL device output |
| `crates/engine-acoustics` | Acoustic world (BVH, 2D proxy, materials, rooms), CPU ray-traced propagation, reverb fitting and the propagation service |
| `crates/engine-acoustics-gpu` | Hardware ray-query and compute GPU tiers of the propagation backend, with self-tests and fallback |
| `crates/engine-editor` | Portable egui/eframe scene editor, real rendered viewport, shared `EditorController` and isolated Cargo-backed play; retained Win32 smoke controls |
| `crates/engine-mcp` | MCP stdio server for agents: project/scene/asset/file read tools, `--allow-write` scene and file edits, `project://` resources, and a `ToolProvider` for the CLI's `--allow-exec` build/run/screenshot tools |
| `crates/engine-tooling` | Native Rust compiler/launcher, generation management, asset index, shader import, packaging and the `engine` command-line tool |
| `examples/first-project` | Minimal authored project with a game crate and lifecycle fixtures |
| `examples/desktop-2d` | 2D desktop synthetic lab: sprites, text, collision, rooms, audio, saves |
| `examples/native-extension` | Custom components, systems and a custom wgpu render pass |
| `examples/programmable-world` | Authored 3D scene, retained HUD, typed callbacks, physics, animation and navigation; GPU-free default and optional window |
| `examples/pbr-shadows` | Data-only authored project (no Cargo crate) with PBR spheres and a directional shadow, run by `cargo run -p engine-desktop --example pbr_shadows` |
| `examples/acoustics-showcase` | Data-only authored project (no Cargo crate) with a 3D room and a 2D proxy scene for spatial audio and acoustics, validated in CI |
| `assets/scripts/moving_entity` | Smallest script crate: one entity advanced along +X |
| `docs/` | Documentation, starting at [docs/README.md](docs/README.md) |
| `scripts/ci/` | Helper scripts used by the CI workflow |

The `crates/*` directories form the root Cargo workspace. Each Cargo example under
`examples/` and `assets/scripts/moving_entity` is its own workspace root (an empty
`[workspace]` table), so the engine workspace never builds them and their
`Cargo.lock` and `target` directories are local and ignored. `examples/pbr-shadows`
is a data project run through the workspace `engine-desktop` example, so the
workspace tests, clippy and example build cover it.

## Prerequisites

- Rust 1.99.1, pinned in `rust-toolchain.toml` (components `rustfmt` and `clippy`).
  Install it with [rustup](https://rustup.rs/); the toolchain file selects it
  automatically in this checkout.
- Windows is the locally tested desktop platform. The desktop host and portable
  editor have Windows/Linux/macOS code paths. Linux builds need the ALSA, udev and
  window-system development packages (see `.github/workflows/ci.yml` for the list
  used by CI). Linux and macOS builds and interactive execution remain unverified
  until their respective runner/device checks pass. A WebAssembly compile-only
  check covers some portable libraries; it is not a game host.
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
cargo test --manifest-path examples/programmable-world/Cargo.toml
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
[examples/programmable-world](examples/programmable-world/README.md), plus
[assets/scripts/moving_entity](assets/scripts/moving_entity/src/lib.rs) for real usage.

The [programmable world](examples/programmable-world/README.md) connects the new
systems in one authored scene. Its default executable runs fixed ticks without a
GPU; the optional window requires `--features desktop`:

```text
cargo run --manifest-path examples/programmable-world/Cargo.toml
cargo run --manifest-path examples/programmable-world/Cargo.toml --features desktop -- --window
```

### Command line

The `engine` binary comes from the `engine-tooling` crate and provides
`platform`, `project init|validate|assets|shaders|package`, `scene inspect`, `run`, `editor`
and `mcp`. Run it with `cargo run -p engine-tooling --bin engine -- <command>`.
The command surface and its JSON results are documented in
[Project commands](docs/commands.md). Compiling a game generation invokes Cargo,
so a first run can take minutes. `platform --target web` reports the missing host
capabilities rather than promising a runnable web export.

## Feature scope

Scenes support local TRS hierarchies, versioned custom component schemas, native
callbacks and transactional prefab cloning through the same model used by the
editor and MCP. Authored physics, animation and navigation run before extraction.
Retained game UI supplies layout, controls, clipping, focus and callback events;
multiplayer sessions are explicit and use server-authoritative state.

These are executable foundations, not a claim of Unity feature parity or AAA
production readiness. The renderer provides metallic-roughness PBR materials with
texture maps, one directional light with an optional shadow map (no cascades, point or
spot shadows, IBL or HDR tone mapping), custom WGSL and CPU-deformed skinned meshes
([3D rendering](docs/rendering3d.md)); there is no GPU skinning. Animation has no
graph editor, IK or retargeting. Navigation bakes layered polygon NavMeshes and
finds bounded paths through runtime and MCP tools; it has no crowd avoidance
([Navigation](docs/navigation.md)). Multiplayer can use mutual-certificate TLS 1.3
with a native authorization policy; it has no prediction, rollback, matchmaking or
NAT traversal, and plaintext APIs remain for trusted LANs
([Multiplayer](docs/networking.md)). The optional `engine-dlss` crate provides
NVIDIA DLSS Super Resolution on Vulkan as a standalone crate; Frame Generation,
Ray Reconstruction and DLSS 5 neural rendering are not implemented
([DLSS](docs/dlss.md)). Packages are host-native
directories, not signed installers or store submissions. Domain documentation
records these limits and the tests that establish each feature.

## Documentation

Start at the [documentation index](docs/README.md). Architecture and the ECS
scheduling model are in [Architecture](docs/architecture.md).

## License

MIT; see [LICENSE](LICENSE).
