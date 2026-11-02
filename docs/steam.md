# Steam integration

The `engine-steam` crate is the engine's optional Steamworks integration. Steam is
off by default: a normal build contains no Steam code and no Steam dependency, and
nothing in a game changes unless it opts in.

Status: only the skeleton exists (stage S1 of `docs/platform-and-steam-plan.md`). The
backend trait, the null and fake backends, the status and error types and the build
feature are implemented and tested. The real Steamworks backend is a stub, and no
engine, desktop or project code uses the crate yet. The features listed under
[Roadmap](#roadmap-not-yet-implemented) do not exist.

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
  off. Today `RealBackend` is a stub that reports
  `SteamStatus::InitFailed("the real Steam backend is not implemented yet")`.

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
| `RealBackend` | feature `steam` | stub for now; wraps `steamworks::Client` from stage S2 |

Game and engine code should depend on `dyn SteamBackend`, and tests should drive a
`FakeBackend`; live tests against the real client are local-only and ignored by default
(App ID 480, "Spacewar", with Steam open).

The API surface (achievements, stats, Cloud files, lobbies, Workshop items, Steam
Input, auth tickets, overlay, rich presence) is deliberately shaped for the later
stages; it will be refined as each stage lands.

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

- S2: real backend core (restart-if-necessary, init, identity, auth ticket, overlay,
  rich presence, callback events).
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
