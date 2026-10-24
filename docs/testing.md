# Testing and CI

## Local commands

Use the toolchain pinned in `rust-toolchain.toml`.

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/ci/check-desktop-examples.sh
```

The standalone example crates are separate Cargo workspace roots with local,
uncommitted lockfiles, so each is checked with `--manifest-path`:

```text
cargo test --manifest-path examples/first-project/Cargo.toml
cargo test --manifest-path examples/desktop-2d/Cargo.toml
cargo test --manifest-path examples/native-extension/Cargo.toml
cargo test --manifest-path examples/programmable-world/Cargo.toml
cargo test --manifest-path assets/scripts/moving_entity/Cargo.toml
python examples/desktop-2d/tools/validate_fixture.py
```

`check-desktop-examples.sh` compiles all targets of `desktop-2d`, `first-project`
and `native-extension`, plus `programmable-world` with its `desktop` feature. It
uses `target/desktop-examples` to reuse dependencies unless `CARGO_TARGET_DIR` is
set. Linux and macOS CI run the same script after workspace tests. This checks
native entry-point compilation without opening a window or device.

`programmable-world` uses its default GPU-free configuration in CI. Its integration
test drives ordinary runtime ticks and checks HUD callbacks, typed score changes,
physics contacts, animation events, navigation arrival and combined 3D/HUD frame
extraction. This does not establish GPU pixels or interactive window behavior.
The optional window path can be compiled and run separately:

```text
cargo check --manifest-path examples/programmable-world/Cargo.toml --features desktop
cargo run --manifest-path examples/programmable-world/Cargo.toml --features desktop -- --window
```

Some tests build real external Rust crates through Cargo (`engine-tooling`,
`examples/first-project` fixtures). They need network access or a populated Cargo
registry cache on the first run and can take minutes. Run them without
`RUSTFLAGS=-D warnings`: the spawned builds inherit it, and fixtures that
intentionally contain warnings then fail to compile.

## What the tests establish

Ordinary tests are headless: they use temporary directories, injected input and the
offline audio mixer, and create no window, GPU device or audio device. They cover
the core scheduling and lifecycle contracts, project and scene validation, runtime
behavior, persistence failure paths, the MCP transcript contract, the editor
controller and the compiler pipeline. Test names describe the behavior under test;
list them with `cargo test --workspace -- --list`. Multiplayer tests own ephemeral
loopback sockets and exercise real bounded transport behavior; they need no
external service. Portable editor interaction tests can drive egui without opening
a window, which does not establish physical mouse/keyboard or window behavior.

Focused feature commands include:

```text
cargo test -p engine-assets -p engine-physics -p engine-animation -p engine-navigation -p engine-network -p engine-platform --locked
cargo test -p engine-core --test profiling --locked
cargo test -p engine-project --test meta --test networking --test game_ui --locked
cargo test -p engine-desktop --test scene3d --locked
```

These cover import/resource limits, real rigid-body simulation, joints, animation
poses/deformation, bounded paths, replication authority, capability errors,
measured CPU profiling, authored metadata/events, game UI and scene-render
contracts. Native library tests and host integration tests are distinct; domain
docs describe the runtime adapter checks and unsupported feature subsets.

Tests that need real hardware or an interactive desktop are marked `#[ignore]` and
are never run by ordinary `cargo test`, so they must be run and reported separately:

| Command | Needs | Establishes |
| --- | --- | --- |
| `cargo test -p engine-desktop --test gpu_pixels -- --ignored --nocapture` | GPU adapter | Real offscreen pixels for sprites, fonts, clipping, ordering, resizing and a custom WGSL pass |
| `cargo test -p engine-desktop --test scene3d -- --ignored --nocapture` | GPU adapter | Real depth, clipping, directional lighting and custom material/WGSL pixels |
| `cargo test --manifest-path examples/native-extension/Cargo.toml external_custom_pass_verifies_real_offscreen_pixels -- --ignored --nocapture` | GPU adapter | An external crate's custom render pass draws and updates real pixels |
| `cargo test -p engine-audio native_device_opens_submits_pcm_and_closes -- --ignored --nocapture` | Audio output device | Native device open, callback progress, PCM consumption and shutdown (not audibility) |
| `cargo test -p engine-editor real_native_controls_smoke_closes_without_saving -- --ignored --nocapture` | Interactive Windows desktop | Retained legacy Win32 controls; does not validate the portable editor shell |
| `cargo run -p engine-tooling -- editor <manifest>` | Window and GPU | Portable editor hierarchy, viewport, selection/drag, save/undo, dirty-close and isolated play checked interactively |
| `cargo run -p engine-desktop --example desktop_smoke` | Window, GPU | A visible desktop run |

A missing adapter or device makes these fail rather than substitute a mock. Subjective
audio, visual quality, physical gamepad and window interaction remain manual checks.

Windows compilation and real 3D readback tests have been exercised locally using
an NVIDIA GeForce RTX 5080 through Vulkan. That evidence covers the tested GPU
path, not all drivers/backends or portable-editor physical interaction. The baseline
[CI run for `dbfef40`](https://github.com/Densetsu152637/Rust-Game-Engine/actions/runs/40060400676)
passed Linux workspace tests. macOS compiled but failed 125 persistence tests
because its system `/var` alias was rejected. The fix resolves only verified
macOS system aliases and has explicit save/settings regression coverage; a new
hosted run is required to validate it on macOS. Interactive Linux/macOS runs remain
unverified here. Record the revision,
device/backend, command and result for new platform checks rather than treating
the existence of a CI job as a passed run.

## Continuous integration

`.github/workflows/ci.yml` runs on pull requests and pushes to `main`, with
`contents: read` permission, concurrency cancellation for superseded pull request
runs, pinned action SHAs and the toolchain installed from `rust-toolchain.toml` by
`scripts/ci/install-toolchain.sh`. Jobs:

| Job | Runner | What it runs |
| --- | --- | --- |
| Format and Clippy | `windows-latest` | `cargo fmt --check` for the workspace and each example crate, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and clippy for each example crate |
| Workspace tests | `windows-latest` | `cargo test --workspace --locked`, `engine project validate` of the data-only `examples/pbr-shadows` project (its renderer example is built by the workspace tests) and a passed-test count |
| Example tests | `windows-latest`, one job per example | `cargo test --manifest-path` for `desktop-2d`, `first-project`, `native-extension`, `programmable-world` (default features) and `assets/scripts/moving_entity` (plus the desktop-2d fixture validator) and a passed-test count |
| Test count gate | `ubuntu-latest` | Sums the counts above and fails when fewer than 1000 tests passed |
| Workspace tests (Linux) | `ubuntu-latest` | `cargo test --workspace --locked` and standalone desktop entry-point checks after installing ALSA, udev and window-system development packages |
| Workspace tests (macOS) | `macos-latest` | `cargo test --workspace --locked` and standalone desktop entry-point checks; hardware-dependent tests remain ignored |
| Portable library compile (WebAssembly) | `ubuntu-latest` | `cargo check --locked --target wasm32-unknown-unknown -p engine-core -p engine-ecs -p engine-render -p engine-platform`; compilation only, no browser host |

`scripts/ci/test-count.sh` parses libtest `test result:` lines (ANSI colour is
stripped); tests that are ignored or filtered out never count as passed. The minimum
is the `MINIMUM_PASSED_TESTS` environment value in the workflow. Warnings are denied
by the lint job (`RUSTFLAGS=-D warnings` and clippy `-D warnings` over every target).
The example test jobs also build with `RUSTFLAGS=-D warnings`. The Windows
workspace test job does not set it: `engine-tooling` tests build real crates through
Cargo, which inherit it as part of the generation's build configuration, and their
fixtures deliberately contain warnings. The Linux/macOS test jobs do not deny
warnings either, so platform-specific warnings do not block portability checks.

Notes and limits:

- `--locked` requires a committed, current `Cargo.lock`; CI fails on a stale
  lockfile. The example crates have no committed lockfile, so their dependencies
  resolve fresh in CI.
- The portable egui/eframe editor has native Windows/Linux/macOS code paths.
  Ordinary CI exercises its controller and headless logic; it does not open the
  interactive editor. Legacy Win32 controls and smoke tests remain `cfg(windows)`.
- Hosted runners have no GPU or audio device, so the ignored tests above are not run
  in CI. Their evidence has to come from a developer machine or a self-hosted runner.
- The portable WebAssembly compile command has passed locally for core, ECS,
  backend-neutral render and platform libraries. There is no browser host or
  browser runtime test. `cargo check` does not establish working filesystem,
  clock, thread, input or device services in WebAssembly.
- Actionlint validates the workflow statically. Local act job enumeration works;
  on this Windows host, Docker access requires execution outside the sandbox.
  Act's Linux container does not reproduce native macOS or Windows runners.
  Hosted execution, cache behavior, permissions and cancellation remain separate
  checks until the respective workflow runs pass.
