# Steam integration

The `engine-steam` crate is the engine's optional Steamworks integration. Steam is
off by default: a normal build contains no Steam code and no Steam dependency, and
nothing in a game changes unless it opts in.

Status: the skeleton (stage S1) and the real backend core (stage S2) of
`docs/platform-and-steam-plan.md` exist: the backend trait, the null and fake
backends, the status and error types, the build feature, and a `RealBackend` that
initialises Steam, reports identity, issues auth tickets, opens the overlay, sets rich
presence and turns Steam callbacks into events. No engine, desktop or project code uses
the crate yet. The features listed under [Roadmap](#roadmap-not-yet-implemented) do not
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

- S3: desktop runner host hook, `SteamPlugin`, per-tick pump and the `steam` project
  manifest block (`app_id`, `cloud`, `required`).
- S4: achievements and stats service with batched stat storing.
- S5: Steam Cloud save sync beside the persistence store.
- S6: Steam Networking Sockets transport for `MessageEndpoint`.
- S7: lobbies and matchmaking.
- S8: Steam Input as an input source.
- S9: Workshop service (create, update, subscribe, query).
- S10: Workshop content loading through extra read-only asset roots.
- S11: Steam packaging (depot layout), `engine` facade feature, tooling and CI.
