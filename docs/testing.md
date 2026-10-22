# Testing and CI

## Local commands

Use the toolchain pinned in `rust-toolchain.toml`.

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The standalone example crates are separate Cargo workspace roots with local,
uncommitted lockfiles, so each is checked with `--manifest-path`:

```text
cargo test --manifest-path examples/first-project/Cargo.toml
cargo test --manifest-path examples/desktop-2d/Cargo.toml
cargo test --manifest-path examples/native-extension/Cargo.toml
cargo test --manifest-path assets/scripts/moving_entity/Cargo.toml
python examples/desktop-2d/tools/validate_fixture.py
```

Some tests build real external Rust crates through Cargo (`engine-tooling`,
`examples/first-project` fixtures). They need network access or a populated Cargo
registry cache on the first run and can take minutes.

## What the tests establish

Ordinary tests are headless: they use temporary directories, injected input and the
offline audio mixer, and create no window, GPU device or audio device. They cover
the core scheduling and lifecycle contracts, project and scene validation, runtime
behavior, persistence failure paths, the MCP transcript contract, the editor
controller and the compiler pipeline. Test names describe the behavior under test;
list them with `cargo test --workspace -- --list`.

Tests that need real hardware or an interactive desktop are marked `#[ignore]` and
are never run by ordinary `cargo test`, so they must be run and reported separately:

| Command | Needs | Establishes |
| --- | --- | --- |
| `cargo test -p engine-desktop --test gpu_pixels -- --ignored --nocapture` | GPU adapter | Real offscreen pixels for sprites, fonts, clipping, ordering, resizing and a custom WGSL pass |
| `cargo test --manifest-path examples/native-extension/Cargo.toml external_custom_pass_verifies_real_offscreen_pixels -- --ignored --nocapture` | GPU adapter | An external crate's custom render pass draws and updates real pixels |
| `cargo test -p engine-audio native_device_opens_submits_pcm_and_closes -- --ignored --nocapture` | Audio output device | Native device open, callback progress, PCM consumption and shutdown (not audibility) |
| `cargo test -p engine-editor real_native_controls_smoke_closes_without_saving -- --ignored --nocapture` | Interactive Windows desktop | Native editor controls and command paths (not visual quality) |
| `cargo run -p engine-desktop --example desktop_smoke` | Window, GPU | A visible desktop run |

A missing adapter or device makes these fail rather than substitute a mock. Subjective
audio, visual quality, physical gamepad and window interaction remain manual checks.

## Continuous integration

`.github/workflows/ci.yml` runs on pull requests and pushes to `main`, with
`contents: read` permission, concurrency cancellation for superseded pull request
runs, pinned action SHAs and the toolchain installed from `rust-toolchain.toml` by
`scripts/ci/install-toolchain.sh`. Jobs:

| Job | Runner | What it runs |
| --- | --- | --- |
| Format and Clippy | `windows-latest` | `cargo fmt --check` for the workspace and each example crate, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and clippy for each example crate |
| Workspace tests | `windows-latest` | `cargo test --workspace --locked` and a passed-test count |
| Example tests | `windows-latest`, one job per example | `cargo test --manifest-path` for `desktop-2d`, `first-project`, `native-extension` and `assets/scripts/moving_entity` (plus the desktop-2d fixture validator) and a passed-test count |
| Test count gate | `ubuntu-latest` | Sums the counts above and fails when fewer than 1000 tests passed |
| Workspace tests (Linux) | `ubuntu-latest` | `cargo test --workspace --locked` after installing ALSA, udev and window-system development packages |

`scripts/ci/test-count.sh` parses libtest `test result:` lines (ANSI colour is
stripped); tests that are ignored or filtered out never count as passed. The minimum
is the `MINIMUM_PASSED_TESTS` environment value in the workflow. The `RUSTFLAGS`
value `-D warnings` is set on the Windows jobs; the Linux job does not set it so
platform-specific warnings do not block portability checks.

Notes and limits:

- `--locked` requires a committed, current `Cargo.lock`; CI fails on a stale
  lockfile. The example crates have no committed lockfile, so their dependencies
  resolve fresh in CI.
- The editor's Win32 controls are `cfg(windows)`; on Linux the editor crate builds
  through its unsupported-platform fallback and only its controller tests run.
- Hosted runners have no GPU or audio device, so the ignored tests above are not run
  in CI. Their evidence has to come from a developer machine or a self-hosted runner.
- The Linux job and the hosted workflow as a whole had not been executed on GitHub
  at the time this page was written; local runs validated the commands, the
  workflow YAML syntax and the counting script only.
