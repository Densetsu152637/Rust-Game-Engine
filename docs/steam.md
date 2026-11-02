# Steam integration

The `engine-steam` crate is the engine's optional Steamworks integration. Steam is
off by default: a normal build contains no Steam code and no Steam dependency, and
nothing in a game changes unless it opts in.

Status: the skeleton (S1), the real backend core (S2) and the host integration (S3) of
`docs/platform-and-steam-plan.md` exist: the backend trait, the null and fake
backends, the status and error types, the build feature, and a `RealBackend` that
initialises Steam, reports identity, issues auth tickets, opens the overlay, sets rich
presence and turns Steam callbacks into events; `SteamPlugin` runs it inside the desktop
runner for projects with a `steam` manifest block. The features listed under [Roadmap](#roadmap-not-yet-implemented) do not
exist.

## The `steam` feature

`engine-steam` has one Cargo feature, `steam`, off by default.

- Off (default): `steamworks` is not in the dependency graph
  (`cargo tree -p engine-steam -e normal | grep -c steamworks` prints `0`).
  `default_backend()` returns a `NullBackend`; its status is
  `SteamStatus::NotCompiled` and every operation returns
  `SteamError::Unavailable(UnavailableReason::NotCompiled)`. Nothing panics.
- On (`--features steam`): adds `steamworks = "=0.13.1"` and the `RealBackend` type,
  compiled only on Windows, Linux and macOS (`cfg(any(windows, target_os = "linux",
  target_os = "macos"))`). On any other platform the feature compiles but behaves as
  off. `RealBackend` implements the core (below); the other trait methods still return
  `Unavailable`/`Rejected` until their stages land.

```text
cargo test  -p engine-steam
cargo check -p engine-steam --features steam
cargo clippy -p engine-steam --features steam --all-targets -- -D warnings
```

## Build variants

Steam and non-Steam games are separate build variants. The `steamworks-sys` crate
links the Steam API library (`steam_api64.dll` on Windows, `libsteam_api.so` on Linux,
`libsteam_api.dylib` on macOS) dynamically, so an executable built with the `steam`
feature cannot start unless that library is next to the executable (or on the loader
path), whether or not Steam is running. A build without the feature has no such
requirement. Ship the Steam variant only through Steam, and keep the plain variant for
other stores and for development without Steam.

A running Steam client is a separate, soft condition: when the library loads but Steam
is not running, the status is `NotRunning` and the game keeps running. Making that a
hard failure (`steam.required`) is planned for the manifest (stage S3).

## Backend and testing seam

All Steam access goes through the `SteamBackend` trait, called from the tick thread
only; `run_callbacks()` is meant to be called once per runtime tick and returns the
`SteamEvent`s produced since the previous call. Every method has a default
implementation returning `Unavailable` for the backend's current status, so a backend
overrides only what it supports.

| Backend | When | Behaviour |
| --- | --- | --- |
| `NullBackend` | feature off, or an explicit absent status | fixed unavailable status, every call fails with `Unavailable` |
| `FakeBackend` | always compiled | deterministic in-memory Steam: achievements and stats (persisted by `store_stats`), Cloud files with a quota and name validation, lobbies, Workshop items, controllers and action sets, rich presence, scripted events and a recorded call order (`calls()`) |
| `RealBackend` | feature `steam` | wraps `steamworks::Client`; core implemented (see [Core](#core-real-backend)) |

Game and engine code should depend on `dyn SteamBackend`, and tests should drive a
`FakeBackend`; live tests against the real client are local-only and ignored by default
(App ID 480, "Spacewar", with Steam open).

The API surface (achievements, stats, Cloud files, lobbies, Workshop items, Steam
Input, auth tickets, overlay, rich presence) is deliberately shaped for the later
stages; it will be refined as each stage lands.

## Core (real backend)

`RealBackend::new()` is uninitialised (status `InitFailed("Steam has not been
initialised")`). The host then calls, in order, `restart_app_if_necessary(app_id)` (a
static Steam call; `Ok(true)` means Steam is relaunching the game and the process should
exit at once) and `init(Some(app_id))` (or `init(None)` to use `steam_appid.txt` or the
launch environment). `init` is fallible and never panics; it returns and stores the
status:

- `Ready { app_id, user }` with the SteamID and persona name;
- `NotRunning` when the Steam client is not running (soft: the game keeps running and
  every other call returns `Unavailable(NotRunning)`);
- `InitFailed(message)` for any other failure, including a second `init` in the same
  process (only one initialised `RealBackend` may exist at a time; dropping it shuts
  Steam down and allows a new one).

Once ready the backend provides `auth_session_ticket` / `cancel_auth_ticket`,
`activate_overlay(dialog)` (for example `"friends"`), and `set_rich_presence(key,
value)` (`None` clears the key). Steam returns an empty ticket for a short time after
init; the backend reports that as `Rejected(..)` and the caller retries after
`run_callbacks`. Strings containing NUL are rejected with `InvalidName` because the
`steamworks` crate would otherwise panic.

All calls belong on the tick thread. `run_callbacks()` runs Steam's pending callbacks
and returns the events they produced: `OverlayToggled`, `PersonaChanged` (signed-in user
only) and `ConnectionChanged`. The callback handles are owned by the backend and live as
long as it. Steam's "shutdown requested" notification is not exposed by `steamworks`
0.13.1, so `ShutdownRequested` is not produced by the real backend yet.

## Host integration (`steam` manifest block)

A project with a `steam` block (see [Project format](project-format.md)) gets the Steam
host service when it runs through `engine_tooling::runtime_entry::run` (the entry point
of generated game binaries). The block is plain data in every build; to link the real
backend build `engine-tooling` with `--features steam`, otherwise the status is
`not_compiled`.

The desktop runner knows nothing about Steam. `engine_desktop::run_with_services` takes
`Vec<Box<dyn HostService>>` (trait in `engine_project::host_service`; `run` and
`run_with_wgpu_backends` pass none):

1. `before_graphics(&mut Runtime)` runs once per service, in order, before the audio
   device, window or GPU device exists and before the runtime starts. An error is
   returned as the run error, wrapping a `HostServiceExit { code, message }`.
2. `before_tick(&mut Runtime, tick)` runs exactly once before every fixed tick, in both
   headless and windowed runs, on the tick thread.
3. `after_gpu_idle()` runs in reverse order after the runtime stopped, the GPU is idle
   and audio is shut down.

`SteamPlugin` (a `HostService`) uses a `SteamSession`, the process-wide owner of the
backend. The first `before_graphics` calls `restart_app_if_necessary(app_id)` and, if
that is not a relaunch, `init(Some(app_id))`, then sets the manifest `rich_presence`
defaults. Later runner generations in the same process reuse the session, so
`restart_app_if_necessary` and `init` happen at most once. The backend drops (Steam
shuts down) when the last session holder drops, after the GPU is idle.

Per tick, `run_callbacks()` is called once and each `SteamEvent` becomes an engine
event named `steam.<event>` (for example `steam.overlay_toggled` with payload
`{"active": true}`, `steam.status_changed`, `steam.connection_changed`); SteamIDs and
other 64-bit ids are decimal strings. Games bind handlers with `EventBindings` like any
engine event. The Steam status is also available as the `SteamRuntimeStatus` resource,
and a run report from `runtime_entry` carries a `steam` object with `state`
(`ready`, `not_running`, `init_failed`, `not_compiled`).

Exit behaviour (`runtime_entry`):

| Situation | Result |
| --- | --- |
| no `steam` block | Steam is never touched |
| Steam unavailable, `required: false` | soft: the game runs, status is not `ready` |
| Steam unavailable, `required: true` | exit code `4`, before any window or GPU device |
| Steam asks for a relaunch (`restart_app_if_necessary` is true) | exit code `0`, result `{"steam":"relaunching"}`, no window |
| editor play generation (`ENGINE_GENERATION_READY_FILE` set) | Steam is never constructed |

When launched outside Steam, `SteamAPI_RestartAppIfNecessary` returns true (and asks
Steam to relaunch the game) unless `steam_appid.txt` is in the working directory or the
`SteamAppId` environment variable is set; set `SteamAppId=<app_id>` for development
runs. `ShutdownRequested` is part of the event type, but `steamworks` 0.13.1 does not
expose Steam's shutdown notification, so the real backend never produces it yet.

## Live tests

Live tests are `#[ignore]` and use App ID 480 (Spacewar). With the Steam client open:

```text
cargo test -p engine-steam --features steam -- --ignored core --nocapture
```

With Steam closed the same command passes and prints `skipped: Steam not running`.

A Steam-feature test or example executable needs the `steam_api` library at start-up,
and Cargo only leaves it in a build-script output directory. `crates/engine-steam/build.rs`
(feature `steam` only) copies `steam_api64.dll` from the pinned `steamworks-sys` source
(or `$STEAM_SDK_LOCATION/redistributable_bin/win64`) into `target/<profile>/`, `deps/`
and `examples/`, and on Linux/macOS adds an rpath to the library directory for test and
example binaries (not yet exercised on those systems). This helper is for development
only; a shipped game must place the library next to its executable. `steam_appid.txt` is
not needed because the tests pass the App ID to `init`.

## Redistribution terms for the bundled Steam libraries

`steamworks = "=0.13.1"` and its `steamworks-sys` crate are MIT/Apache-2.0 licensed
Rust bindings. `steamworks-sys` also ships Valve's Steamworks SDK headers and the
prebuilt `steam_api` redistributable libraries for Windows, Linux and macOS. Those
files are Valve's, not covered by the Rust crates' licences, and are governed by the
Steamworks SDK Access Agreement that every Steamworks partner accepts:

- Ship the `steam_api` library next to the game executable in your Steam builds. Use
  the redistributable that comes with the SDK; do not modify it.
- The libraries may be distributed only as part of a game published through Steam
  under your Steamworks partner agreement. Do not redistribute the SDK itself or use
  the libraries in unrelated products.
- `steam_appid.txt` is for development only: it lets a game started outside Steam
  identify its App ID. Never package it in a release; Steam supplies the App ID when it
  launches the game.
- No Steamworks SDK files are committed to this repository. They arrive through the
  `steamworks-sys` crate only when the `steam` feature is built.

Read the current Steamworks SDK Access Agreement in your Steamworks partner account
before shipping; this section summarises the engine's obligations and is not legal
advice.

## Roadmap (not yet implemented)

Everything below is planned in `docs/platform-and-steam-plan.md` and is not implemented
at this stage.

- S4: achievements and stats service with batched stat storing.
- S5: Steam Cloud save sync beside the persistence store.
- S6: Steam Networking Sockets transport for `MessageEndpoint`.
- S7: lobbies and matchmaking.
- S8: Steam Input as an input source.
- S9: Workshop service (create, update, subscribe, query).
- S10: Workshop content loading through extra read-only asset roots.
- S11: Steam packaging (depot layout), `engine` facade feature, tooling and CI.
