# Platforms and host boundaries

Cargo remains the compiler and dependency manager. The simulation/ECS and
backend-neutral rendering libraries are separated from native window, audio,
controller, compiler and editor hosts.

```text
cargo run -p engine-tooling -- platform
cargo run -p engine-tooling -- platform --target web --format json
```

The `platform` command returns the actual host platform and architecture plus
capability records. `--target` filters the report; it does not select a Cargo
cross-compilation target or enable a missing host. Names are `windows`, `linux`,
`macos`, `android`, `ios`, `web`, and `unknown`; other names are usage errors.

| Target | Implemented host paths | Validation boundary |
| --- | --- | --- |
| Windows | Native window/GPU, audio, gamepad, directory packaging, CPU profiling | Windows CI; real devices require separate tests |
| Linux | Native window/GPU, ALSA audio, gamepad, directory packaging, CPU profiling | Linux CI; system development packages required |
| macOS | Native window/GPU, audio, gamepad, directory packaging, CPU profiling | Native macOS CI; hosted tests and interactive hardware remain separate evidence |
| WebAssembly | Compile check of core, ECS, backend-neutral render and platform libraries | No browser game host, browser input/audio bridge, or web package |
| Android / iOS | No complete application host | Lifecycle, device services, platform SDK build and store packaging missing |
| Console / other | No implemented host | Requires platform access/SDK and host integration; no console support claim |

`engine-platform::HostPlatform::current()` is selected by Rust target cfg.
`capabilities()` reports implemented code paths and limitations, not detected
hardware, an installed SDK, or proof that a specific machine can open a device.
`require(Capability)` rejects missing hosts with a structured
`UnsupportedCapability` carrying the target, capability and reason. The editor
is documented separately in [editor.md](editor.md); this report does not claim
editor controls or certification for any platform.

## Native packages

`project package` validates that this is a supported desktop packaging host before
writing output. Packages record `package-host.json` (schema 1) with the packaging
host, architecture, native runtime filename and launcher. `PACKAGE.txt` names the
actual host launcher and makes the native binary boundary explicit. The existing
two launcher scripts are retained for compatibility with package consumers, but
the presence of both does not make the executable cross-platform.

The CLI builds for its host. `package_project` also accepts an externally supplied
`BuildArtifact`; it checks the executable exists but does not inspect its binary
ABI. `package-host.json` describes the packager, not verified executable
provenance. Callers supplying cross-built artifacts must verify their ABI and
deploy them through a target-specific packaging workflow. There are no installers,
macOS app bundles/signing/notarization, mobile store bundles, or console packages.

## CI and local checks

Desktop work targets Windows, Linux and macOS. Each has a native workspace test
job. Linux and macOS also compile the standalone desktop examples, including the
optional `programmable-world` window feature. Windows runs the example tests and
lint jobs. Build with the pinned Rust toolchain and the native host linker:

```text
cargo build --workspace --locked
cargo test --workspace --locked
bash scripts/ci/check-desktop-examples.sh
```

On Windows, use the MSVC C++ build tools and Git for Windows Bash for the script.
On macOS, use the Xcode command-line developer tools. On Debian/Ubuntu Linux,
install `pkg-config`, `libasound2-dev`, `libudev-dev`, `libxkbcommon-dev`,
`libwayland-dev`, `libx11-dev`, `libxcursor-dev`, `libxi-dev`, `libxrandr-dev` and
`libxcb1-dev`, as the native CI job does. A graphical session and working native
devices are required only for interactive/device checks.

Player persistence accepts macOS's standard `/var` and `/tmp` aliases after
checking their targets, while continuing to reject arbitrary symlinks in data
paths. See [desktop-services.md](desktop-services.md) for that boundary.

The existing `wasm32-unknown-unknown` portable-library compile check is retained:

```text
rustup target add wasm32-unknown-unknown
cargo check --locked --target wasm32-unknown-unknown -p engine-core -p engine-ecs -p engine-render -p engine-platform
```

Compilation does not establish a runnable browser host. Rust's minimal
[WebAssembly target](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html)
does not provide ordinary OS filesystem/thread services. GPU, audio device and
interactive window tests remain explicitly ignored in ordinary CI; see
[testing.md](testing.md). Hosted macOS/Linux execution, caching and permissions
must be verified on the respective runners, not inferred from a Windows build.
