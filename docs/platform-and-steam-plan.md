# Plan: Linux/macOS support, release packaging and Steam integration

Revision: 2026-11-02 (base `main` 27bf7e7; owner: Claude root, single writer)

Goals (each must be true at close):
1. CI green on all platforms: one `main` run has "Workspace tests (Windows)", "(Linux)", "(macOS)", "Format and Clippy (Windows)", every example job and "Test count gate" succeeding, with Linux and macOS included in the count gate. Check: `gh run view <id> --json jobs`.
2. Platform runtime parity: macOS user data lives under `~/Library/Application Support`, a packaged `tools/ffmpeg[.exe]` is found before `PATH`, and a bundled game starts with no arguments from a `.app`. Check: unit tests per item, run on each OS in CI.
3. Packaging: `engine project package --format zip|tar.gz|appimage|app|steam-depot` produces the documented layout; each release artifact passes a headless smoke run (`--headless --ticks 3`) on its own runner. macOS `.app` is universal (arm64 + x86_64) and ad-hoc signed (`codesign --verify --deep` passes); Developer ID signing and notarisation are wired but gated on secrets. Check: `release.yml` `workflow_dispatch` run artifacts and smoke steps.
4. Steam is optional: with default features `cargo tree -p engine` and `cargo tree -p engine-desktop` contain no `steamworks`; the workspace builds and tests exactly as before. Check: `cargo tree ... | grep -c steamworks` = 0, `cargo test --workspace --locked`.
5. Steam features work behind `engine/steam`: core (init, restart check, identity, overlay, rich presence), achievements/stats, Steam Cloud saves, lobbies, Networking Sockets transport, Steam Input, Workshop upload/download and Workshop content loading. Check: fake-backend unit tests in the normal suite; `--features steam` clippy on all three OSes in CI; live `#[ignore]` tests on App ID 480 run locally with Steam open, results recorded in this document.
6. Docs match behaviour: `docs/platforms.md`, `docs/desktop-packaging.md`, `docs/building.md`, `docs/steam.md` (new), `docs/project-format.md`, README platform statements. Check: review against code at each stage.

Scope / non-goals:
- In: CI repair, Linux and macOS runtime fixes, per-platform packaging and a release workflow, Steam via steamworks-rs, the `steam` feature on the `engine` facade (`docs/using-the-engine.md`, branch `feat/engine-facade`).
- Out: real Developer ID signing/notarisation runs (no account yet), console platforms, Steam game-server API, bundling FFmpeg, real Workshop uploads in automated tests, DLSS on macOS.

Decisions (user, 2026-11-02): Steam is an off-by-default `steam` feature on a new `engine-steam` crate, scope = core + achievements/stats + cloud saves + lobbies/networking + Workshop (including content loading) + Steam Input; a missing Steam client is a soft status (game keeps running), with an opt-in `steam.required` manifest flag; macOS ships ad-hoc signed universal bundles; packaging includes Linux tarball and AppImage, macOS `.app`, Windows zip and Steam depot layout.

Decisions (root, from design evidence; revisit only on new evidence):
- Binding: `steamworks = "=0.13.1"` (MIT/Apache-2.0; bundles Valve's redistributables in `steamworks-sys`; links `steam_api` dynamically). A Steam build cannot start without the library, so Steam and non-Steam builds are separate variants; packaging copies the library next to the runtime. No SDK files are committed to this repository.
- All Steam calls run on the tick thread; `Client::run_callbacks()` once per runtime tick in `First`; no pump thread.
- Testing seam: `SteamBackend` trait with `RealBackend` (feature-gated), `FakeBackend` (always compiled) and `NullBackend`.
- Networking: implement `engine_network::MessageEndpoint` for Steam Networking Sockets; additive `TransportKind::Steam` and a platform-encrypted `TransportSecurity` variant, both enums made `#[non_exhaustive]`. Identity over Steam uses SteamID + auth session tickets, not the TLS certificate-binding token flow.
- Lobbies: `engine-steam` defines its own lobby events; an optional adapter converts to `engine-online` vocabulary. No fork of `engine-online`.
- Saves: explicit Remote Storage sync (`CloudSync`) beside the unchanged `PersistenceStore`; Steam Auto-Cloud documented as an alternative.
- FFmpeg stays unbundled; optional `tools/` next to the runtime is searched before `PATH`; `ENGINE_VIDEO_FFMPEG` still wins.
- Linux and macOS become required CI checks and join the test-count gate. The macOS audio race test is root-caused before any `#[ignore]`.
- Live Steam tests are local-only (`--ignored`, App ID 480); CI compiles and lints the feature only.

Assumptions: GitHub-hosted runners (windows-latest, ubuntu-22.04/latest, macos-latest arm64); WSL2 Ubuntu 22.04 on the dev machine for local Linux builds (toolchain to be installed; GPU tests unavailable there); macOS checks are CI-only.

Relevant paths: `.github/workflows/ci.yml`, `scripts/ci/`, `crates/engine-tooling/src/{artifacts.rs,runtime_entry.rs,cli/}`, `crates/engine-project/src/{persistence/,schema/,input/}`, `crates/engine-video/`, `crates/engine-audio/src/sources.rs`, `crates/engine-desktop/src/runner.rs`, `crates/engine-network/src/{channel,lib}.rs`, `crates/engine-online/`, new `crates/engine-steam/`, `crates/engine` (facade).

Related docs: `docs/platforms.md`, `docs/desktop-packaging.md`, `docs/building.md`, `docs/testing.md`, `docs/video.md`, `docs/project-format.md`, `docs/using-the-engine.md` (facade). Feature docs win on behaviour; this plan wins on sequencing and open decisions.

Execution rules: one implementer at a time (user rule); each stage commits on its own branch and is merged into `main` by root after acceptance; Cargo targets at `D:/cargo-targets/Rust-Game-Engine/<task-id>`; worktrees at `D:/Git-Worktrees/Rust-Game-Engine/<task-id>`.

## Stage P1: CI environment repair
- Objective: remove runner disk exhaustion (Windows LNK1318/LNK1140 at 85 MB free; Linux linker SIGBUS) and unmask hidden failures.
- Depends on: none (needs a push to observe CI).
- Owned paths: `.github/workflows/ci.yml`, `scripts/ci/`.
- Acceptance checks: Linux free-disk step and `df -h` diagnostics present; CI debuginfo reduced (`CARGO_PROFILE_DEV_DEBUG=0`/`CARGO_PROFILE_TEST_DEBUG=0` or equivalent) and Windows target dir on the larger volume; Linux and macOS run `cargo test --workspace --locked --no-fail-fast`; "Workspace tests (Windows)" and "(Linux)" finish without LNK1318/LNK1140/SIGBUS; "Format and Clippy (Windows)" still passes; workflow syntax validated (`actionlint` or `gh workflow view`).
- Status: pending
- Outcome:
- Handoff notes: the last data point is run 40858178120 on 12de3e2; current main has not run in CI. Linux cause is inferred (no disk warning printed) — confirm with `df -h`.

## Stage P2: Linux green
- Objective: every workspace test passes on Linux CI and in WSL.
- Depends on: P1.
- Owned paths: failing crates, `ci.yml` apt list.
- Acceptance checks: "Workspace tests (Linux)" green; in WSL (repo and target inside the WSL filesystem) `cargo test --workspace --locked --no-fail-fast` and `bash scripts/ci/check-desktop-examples.sh` pass.
- Status: pending
- Outcome:
- Handoff notes: WSL needs `build-essential pkg-config cmake curl libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev libxcb1-dev libvulkan1 mesa-vulkan-drivers vulkan-tools` plus rustup (toolchain from `rust-toolchain.toml`). Likely areas: unix permission checks (`engine-online`, `engine-mcp`), process groups (`engine-tooling`), paths.

## Stage P3: macOS green
- Objective: every workspace test passes on macOS CI.
- Depends on: P1.
- Owned paths: `crates/engine-audio` (failure-race test and stream code), other macOS failures, `ci.yml`.
- Acceptance checks: "Workspace tests (macOS)" green; `refresh_retains_decoder_and_dsp_errors_published_before_worker_status` passes 20 consecutive CI runs (temporary repeat job, removed after); audio tests still pass on Windows.
- Status: pending
- Outcome:
- Handoff notes: failure is "producer never reached failure publication barrier" (`sources.rs:541`, 5 s deadline). Suspect: the pause is a thread-local consumed at stream shared-state creation (`stream.rs:244`); check where macOS creates/starts the stream. Root-cause before any ignore.

## Stage P4: All platforms in the gate
- Objective: Windows green after P1; Linux and macOS counts in `test-count-gate`.
- Depends on: P2, P3.
- Owned paths: `ci.yml`, `scripts/ci/test-count.sh`.
- Acceptance checks: one `main` run with every job above green, including "Test count gate" covering all three OSes.
- Status: pending
- Outcome:
- Handoff notes: calibrate per-OS minimums from observed counts.

## Stage P5: Platform runtime fixes
- Objective: macOS user-data root, bundled FFmpeg lookup, bundle-relative game resolution.
- Depends on: P2.
- Owned paths: `crates/engine-project/src/persistence/mod.rs`, `crates/engine-video/` (decoder tool lookup), `crates/engine-tooling/src/runtime_entry.rs`, `docs/platforms.md`, `docs/desktop-services.md`, `docs/video.md`.
- Acceptance checks: path-builder unit tests for each OS root (executed per OS in CI); `tools/` lookup order test (`ENGINE_VIDEO_FFMPEG` > `tools/` > `PATH`); runtime with no arguments finds `../Resources/game/project.json` inside a `.app` layout and `game/project.json` beside the runtime (test with a temp layout).
- Status: pending
- Outcome:
- Handoff notes: macOS root move needs no migration (macOS was unverified, no known saves); document it.

## Stage S1: `engine-steam` skeleton and no-Steam path
- Objective: crate with public types, `SteamBackend`, `NullBackend`, `FakeBackend`, `SteamStatus`, `SteamError`, and the `steam` feature (real backend stub only).
- Depends on: none (code); lands after the facade merge.
- Owned paths: `crates/engine-steam/**`, workspace `Cargo.toml`/`Cargo.lock`, `docs/steam.md`.
- Acceptance checks: `cargo test -p engine-steam` passes and `cargo tree -p engine-steam -e normal | grep -c steamworks` = 0; feature-off calls return `Unavailable(NotCompiled)` (test); `cargo clippy -p engine-steam --all-targets -- -D warnings`; `cargo check --workspace --locked`.
- Status: pending
- Outcome:
- Handoff notes: commit the lockfile change. Gate the real backend on `cfg(any(windows, target_os = "linux", target_os = "macos"))`.

## Stage S2: Real backend core
- Objective: `RealBackend` over `steamworks::Client` — `restart_app_if_necessary`, `init_app`, user identity and auth ticket, overlay, rich presence, callback drain to events.
- Depends on: S1.
- Owned paths: `crates/engine-steam/src/{core,real}.rs`.
- Acceptance checks: `cargo clippy -p engine-steam --features steam --all-targets -- -D warnings` on Windows; live `#[ignore]` core test passes with Steam open and prints "skipped" and passes with Steam closed (`cargo test -p engine-steam --features steam -- --ignored core`); fake status-transition tests pass without the feature.
- Status: pending
- Outcome:
- Handoff notes: the test exe needs `steam_api64.dll` on the loader path (copy in a test helper or build script); `steam_appid.txt` for dev runs only, never packaged.

## Stage S3: Host integration and manifest
- Objective: generic `HostService` hook in the desktop runner (init before graphics, teardown after GPU idle, shutdown request), `SteamPlugin`, per-tick pump, `steam` manifest block (`app_id`, `cloud`, `required`, rich presence defaults), no Steam for editor play generations.
- Depends on: S2.
- Owned paths: `crates/engine-desktop/src/runner.rs` (hook only), `crates/engine-project/src/schema/**`, `crates/engine-steam/src/plugin.rs`, `docs/project-format.md`.
- Acceptance checks: project without `steam` loads unchanged; `steam.app_id` parses; unknown `steam` keys rejected; a fake-backend headless run pumps exactly once per tick; `steam.required` with an absent client exits/relaunches as documented (fake test); `cargo test --workspace --locked` passes with default features.
- Status: pending
- Outcome:
- Handoff notes: keep `runner.rs` diff minimal; the hook carries no Steam types.

## Stage S4: Achievements and stats
- Objective: `Achievements` and `Stats` with batched `store_stats`, events, docs.
- Depends on: S3.
- Owned paths: `crates/engine-steam/src/stats.rs`, `docs/steam.md`.
- Acceptance checks: fake tests (unlock idempotence, batching throttle, progress clamp, request-before-read ordering); live `#[ignore]` read of a Spacewar stat; feature clippy clean.
- Status: pending
- Outcome:
- Handoff notes: App 480 has few stats; live coverage is thin by nature.

## Stage S5: Steam Cloud saves
- Objective: `CloudSync` mirroring `PersistenceStore` slots (pull at startup, push after save, conflict policy: higher `content_version`, then newer mtime, else `CloudConflict` event).
- Depends on: S3.
- Owned paths: `crates/engine-steam/src/cloud.rs`; an additive post-save hook in `engine_project::persistence` only if needed.
- Acceptance checks: fake tests for pull, push, newest-wins, conflict event, unsafe-name rejection (`../x`), quota exceeded, cloud disabled for app/user; live `#[ignore]` round trip on 480.
- Status: pending
- Outcome:
- Handoff notes: detect `is_cloud_enabled_for_app/account` and degrade to local-only.

## Stage S6: Networking Sockets transport
- Objective: `SteamServerEndpoint`/`SteamClientEndpoint` implementing `MessageEndpoint` (1-byte lane header, delivery → `SendFlags`, emulated `UnreliableSequenced`, peer table, `PollBudget`, `max_payload`), SteamID/auth-ticket identity; additive `TransportKind`/`TransportSecurity` variants with `#[non_exhaustive]`.
- Depends on: S3.
- Owned paths: `crates/engine-steam/src/transport.rs`; additive edits in `crates/engine-network/src/{channel,lib}.rs` and any exhaustive matches in dependents.
- Acceptance checks: a fake in-process socket pair passes the shared `MessageEndpoint` contract tests (per-lane ordering, sequenced stale drop, budget bounds, `QueueFull`, oversize rejection) — add a shared contract-test helper if none exists; `cargo test -p engine-network -p engine-net-quic -p engine-online -p engine-net-replication` unchanged; live `#[ignore]` loopback on 480.
- Status: pending
- Outcome:
- Handoff notes: highest-risk stage. Write the identity/auth design into `docs/steam.md` before code. Relay access (`init_relay_network_access`) is asynchronous.

## Stage S7: Lobbies and matchmaking
- Objective: `SteamLobbies` (create, join, list with filters, metadata, member events, leave) behind a `LobbyBackend` trait; lobby metadata carries the P2P connect identity; optional adapter to `engine-online` events.
- Depends on: S6.
- Owned paths: `crates/engine-steam/src/lobby.rs`.
- Acceptance checks: fake state-machine tests (create, join, host change, leave, filter, member limit, out-of-order callbacks); live `#[ignore]` create/leave on 480; `engine-online` tests unchanged.
- Status: pending
- Outcome:
- Handoff notes: normalise asynchronous, out-of-order callbacks.

## Stage S8: Steam Input
- Objective: `SteamInputSource: InputSource` feeding `ActionMapper`, action sets, glyph lookup, sample `game_actions_<appid>.vdf` in docs.
- Depends on: S3.
- Owned paths: `crates/engine-steam/src/input.rs`, `docs/steam.md`; read-only use of `crates/engine-project/src/input/`.
- Acceptance checks: first verify the steamworks 0.13.1 Input API surface (record it); fake-controller path through `ActionMapper` passes `cargo test -p engine-steam`; live `#[ignore]` controller enumeration skips cleanly with no controller.
- Status: pending
- Outcome:
- Handoff notes: if the 0.13.1 Input API is unusable, stop and report before building on it.

## Stage S9: Workshop service
- Objective: `UgcService` — create, update, submit, subscribe, unsubscribe, query, installed-item info, progress events.
- Depends on: S3.
- Owned paths: `crates/engine-steam/src/workshop.rs`.
- Acceptance checks: fake tests; live `#[ignore]` query of Spacewar's Workshop; no automated test uploads (uploads are manual only and documented).
- Status: pending
- Outcome:
- Handoff notes: uploads publish publicly — never in automated tests.

## Stage S10: Workshop content loading
- Objective: the asset catalog accepts additional read-only content roots (installed Workshop items) with the same validation and confinement as the project root.
- Depends on: S9. Starts with a short design note in `docs/asset-pipeline.md` (root precedence, id collisions, kinds allowed from mods, reload) reviewed by root before code.
- Owned paths: `crates/engine-project` asset catalog/resolution, `crates/engine-steam/src/workshop.rs` (root registration), `docs/asset-pipeline.md`, `docs/steam.md`.
- Acceptance checks: traversal and link/junction rejection tests per extra root; kind validation; id collision policy test; projects with no extra roots resolve identically (existing asset tests unchanged); a fake installed item's asset loads at runtime.
- Status: pending
- Outcome:
- Handoff notes: core change; keep it generic (not Steam-specific) so other mod sources can reuse it.

## Stage P6: Packaging core
- Objective: `engine project package --format dir|zip|tar.gz` (default `dir` unchanged), packaging module, updated `PACKAGE.txt`.
- Depends on: P4.
- Owned paths: `crates/engine-tooling/src/{artifacts.rs,packaging/,cli/project.rs}`, `crates/engine-tooling/tests/artifacts.rs`, `docs/desktop-packaging.md`.
- Acceptance checks: Windows zip extracts and the launcher runs `--headless --ticks 3`; same for `tar.gz` in WSL (exec bit preserved); new tests pass on all three OSes in CI.
- Status: pending
- Outcome:
- Handoff notes: check licence of any archive crate added.

## Stage P7: Linux AppImage
- Objective: `--format appimage` (AppDir, `.desktop`, icon, `AppRun`), built on the oldest supported glibc (ubuntu-22.04).
- Depends on: P6.
- Owned paths: packaging module, `scripts/package/`, CI job.
- Acceptance checks: AppImage built in CI runs `--headless --ticks 3` via `--appimage-extract-and-run`; `appimagetool` download pinned by version and checksum.
- Status: pending
- Outcome:
- Handoff notes: host provides Vulkan/ALSA/X11/Wayland; do not bundle libvulkan.

## Stage P8: macOS `.app` bundle
- Objective: `--format app` with `Info.plist`, `Resources/game`, `Frameworks/`, universal binary (`lipo` of arm64 + x86_64), ad-hoc signing; Developer ID signing/notarisation steps gated on secrets.
- Depends on: P5, P6.
- Owned paths: `crates/engine-tooling/src/packaging/macos.rs`, CI job, `docs/desktop-packaging.md`.
- Acceptance checks: plist and layout generation unit tests (any OS); on macOS CI: `plutil -lint`, `lipo -archs` lists `x86_64 arm64`, headless launch, `codesign --verify --deep` passes; notarisation documented as unverified until secrets exist.
- Status: pending
- Outcome:
- Handoff notes: no local macOS — each iteration costs a CI run.

## Stage P9: Release workflow
- Objective: `.github/workflows/release.yml` (tag or `workflow_dispatch`) producing Windows zip, Linux tar.gz + AppImage, macOS universal `.app` zip, each smoke-tested.
- Depends on: P7, P8.
- Owned paths: `.github/workflows/release.yml`, `docs/desktop-packaging.md`.
- Acceptance checks: a `workflow_dispatch` run uploads all artifacts with passing smoke steps; actions pinned to SHAs like `ci.yml`.
- Status: pending
- Outcome:
- Handoff notes: keep `ci.yml` unchanged except shared scripts.

## Stage S11: Steam packaging, facade, tooling and CI
- Objective: `engine/steam` facade feature, `--format steam-depot` (per-platform depot directories, copied `steam_api` library, `app_build.vdf`/`depot_build_*.vdf` templates, no `steam_appid.txt`), MCP read-only `steam_status`, CI `cargo check`/`clippy --features steam` on all three OSes, docs pass.
- Depends on: S1–S10, P9, facade merged.
- Owned paths: `crates/engine` (facade features), `crates/engine-tooling/src/packaging/steam.rs`, `crates/engine-mcp/src/tools/`, `ci.yml`, `docs/steam.md`, `docs/desktop-packaging.md`.
- Acceptance checks: `cargo check -p engine --features steam` passes on all OSes in CI; default `cargo tree -p engine` has no `steamworks`; depot layout test passes; `cargo test --workspace --locked` passes.
- Status: pending
- Outcome:
- Handoff notes: real depot upload stays manual (SteamCMD + partner credentials).

## Stage P10: Docs and close-out
- Objective: README and `docs/platforms.md` move Linux/macOS from "unverified" to CI-verified with exact limits; `docs/building.md` per-OS prerequisites; fold durable decisions into feature docs, then remove this plan.
- Depends on: all stages.
- Owned paths: `README.md`, `docs/platforms.md`, `docs/building.md`, `docs/steam.md`.
- Acceptance checks: goals 1–6 above all true with recorded evidence.
- Status: pending
- Outcome:
- Handoff notes:

Execution order (one implementer at a time): P1 → P2 → P3 → P4 → P5 → S1 → S2 → S3 → S4 → S5 → S6 → S7 → S8 → S9 → S10 → P6 → P7 → P8 → P9 → S11 → P10.
