# Steam integration

The `engine-steam` crate is the engine's optional Steamworks integration. Steam is
off by default: a normal build contains no Steam code and no Steam dependency, and
nothing in a game changes unless it opts in.

Status: the skeleton (S1), the real backend core (S2), the host integration (S3),
achievements and stats (S4), Cloud saves (S5), the Networking Sockets transport (S6) and
lobbies (S7) of
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
| `FakeBackend` | always compiled | deterministic in-memory Steam: achievements and stats (persisted by `store_stats`), Cloud files with a quota and name validation, an in-process Networking Sockets model (a connected socket pair inside one backend, with injectable relay delay or failure, a full send queue, unreliable reordering and auth rejection), lobbies, Workshop items, controllers and action sets, rich presence, scripted events and a recorded call order (`calls()`) |
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
backend. The first `before_graphics` calls `restart_app_if_necessary(app_id)` only when
the manifest sets `required: true` (an optional game never relaunches itself through
Steam), and, if that is not a relaunch, `init(Some(app_id))`, then sets the manifest
`rich_presence` defaults. Later runner generations in the same process reuse the
session, so the restart check and `init` happen at most once. The backend drops (Steam
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
| `required: true` and Steam asks for a relaunch (`restart_app_if_necessary` is true) | exit code `0`, result `{"steam":"relaunching"}`, no window |
| `required: false` | no relaunch check is made; only `init` is tried |
| editor play generation (`ENGINE_GENERATION_READY_FILE` set) | Steam is never constructed |

For a `required` game launched outside Steam, `SteamAPI_RestartAppIfNecessary` returns
true (and asks Steam to relaunch the game) unless `steam_appid.txt` is in the working
directory or the `SteamAppId` environment variable is set; set `SteamAppId=<app_id>`
for development runs of required games. Optional games never make this check. `ShutdownRequested` is part of the event type, but `steamworks` 0.13.1 does not
expose Steam's shutdown notification, so the real backend never produces it yet.

## Game-facing handle, achievements and stats

At `before_graphics` the plugin inserts two resources into the game world:
`SteamRuntimeStatus` and `SteamSession`, the cloneable handle every Steam feature is
reached through. Game systems and native scripts read it with
`world.resource::<SteamSession>()` (only present for projects with a `steam` block).
Its methods are safe without Steam: with no client or no Steam build they return
`SteamError::Unavailable(..)`. Use them from the tick thread.

- `unlock_achievement(name)` is idempotent: `Ok(true)` only for the call that unlocked
  it, `Ok(false)` if it was already unlocked (no second Steam call, no second event).
  `is_achievement_unlocked(name)` reads it.
- `stat(name)` / `set_stat(name, StatValue)` read and write integer or float stats; a
  write that does not change the value does nothing.
- `set_progress(stat, value, max)` and `add_progress(stat, delta, max)` clamp an
  integer progress stat to `0..=max` (addition saturates) and return the stored value.
- Writes only change Steam's in-memory state. Changes are stored to Steam in batches:
  the plugin calls `store_stats` at most once every 600 fixed ticks (10 s at 60 Hz;
  `StatsService::new(n)` changes the interval, the first store is immediate), retries
  after one interval if a store fails, and stores any pending change when the run ends.
  `store_stats_now()` forces it. Steam shows the unlock overlay when the store happens.
- Ordering: the user stats are requested before the first read or write (the plugin
  requests them right after `init`; the service requests them lazily otherwise). The
  fake backend refuses reads made before the request with `StatsNotRequested`.

The `steamworks` 0.13.1 binding has no `RequestCurrentStats` call; with the bundled SDK
(1.61 or newer) Steam delivers the stats automatically, so the real backend only marks
the request as made. The `steam.stats_received` event (from Steam `UserStatsReceived`)
reports delivery but did not fire in the live run on App 480 (the stats were already
readable), so do not wait for it: reads fail softly with `NotFound` until stats exist.
Other events: `steam.achievement_unlocked` (`{"name"}`; progress-only notifications are
not unlocks) and `steam.stats_stored`.

Before delivery. Steam delivers the user stats some time after the request. Unlocks and
stat sets made until then are queued (the latest value per stat wins, duplicate unlocks
are ignored), `unlock` and `set_*` report success, and the queue is applied on the next
tick or the next successful read, after which the normal throttled store runs. The queue holds at most `MAX_PENDING_CHANGES` (256) distinct changes; beyond that the
call fails with `Rejected`. `unlock` returns `Ok(true)` while the stats are not ready
even if the achievement was already unlocked (applying the queue then changes nothing).
Reads
before delivery return `SteamError::StatsNotReady`; `add_progress` needs the current value
and so returns that error too. Unknown names in the queue are dropped when applied.

## Steam Cloud saves

With `"cloud": true` in the manifest `steam` block and a ready Steam, the plugin mirrors
the game save slots (`PersistenceStore` files `<user data>/<project>/saves/<slot>.json`)
to Steam Remote Storage files named `saves/<slot>.json`. The store stays authoritative
and a game that never enables Cloud is unaffected. (Steam Auto-Cloud, configured in the
Steamworks partner site by pointing it at the save directory, is an alternative that
needs no code.)

- **Pull at startup:** in `before_graphics`, before the runtime starts and so before any
  load, every slot is synced (`CloudSync::sync_all`). The result is available as
  `SteamSession::cloud_report()`.
- **Push after save:** the plugin registers a post-save listener on the store
  (`PersistenceStore::set_save_listener`, an additive hook that runs after the slot lock
  is released). Each successful `save` uploads that slot. `save_settings` and the `.bak`
  file are not mirrored.
- **Conflict policy** (slot on both sides with different bytes): the higher
  `content_version` wins; on a tie the newer modification time (whole seconds) wins; a
  full tie keeps the local file and raises `steam.cloud_conflict` (`{"name": slot}`)
  without changing either side. A pulled file replaces the local one through the same
  atomic write and the previous local file becomes the `.bak`.
- **Safety:** only `saves/<slot>.json` names whose slot is a safe identifier
  (`[A-Za-z0-9_-]{1,64}`) are considered; any other cloud name, and any slot or cloud
  name such as `../x`, is rejected. Cloud copies are validated like a normal read (size,
  nesting, schema and `content_version` not newer than the game) before they can replace
  a local file; invalid copies are reported and ignored.
- **Failures never break saving:** a failed sync or push (quota exceeded, Steam error,
  invalid cloud copy) leaves the local save intact and raises
  `steam.cloud_sync_failed` (`{"slot", "message"}`).
- **Disabled Cloud:** `is_cloud_enabled_for_app` / `..._for_account` are checked. If
  either is off, or Steam is absent, the report has `local_only: true`, nothing is read
  or written in the cloud and saving continues locally.

The real backend cannot query the remaining quota (the `steamworks` binding has no
`GetQuota`), so a failed write is reported as `QuotaExceeded`.

A local slot that exists but does not parse is treated as absent: a valid cloud copy
replaces it and the damaged file is kept as the `.bak`. With no cloud copy to heal from the
sync reports a failure and never uploads the damaged file.

## Networking Sockets transport (S6)

`SteamServerEndpoint` and `SteamClientEndpoint` (`crates/engine-steam/src/transport.rs`)
implement `engine_network::MessageEndpoint` over Steam Networking Sockets, so
replication, RPC and session layers run on Steam without change. Both take a
`SteamSession`, so every call reaches Steam through the session's backend on the tick
thread. The host's `SteamPlugin` pumps Steam callbacks once per tick; the endpoints only
read what that pump produced, so call `poll` after the plugin has run (a host without
the plugin calls `SteamSession::run_callbacks` itself).

```rust
let server = SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0))?;
let mut client = SteamClientEndpoint::connect_p2p(session, host_steam_id, 0, SteamClientConfig::default())?;
```

### Identity and authentication design

Threats and answers (the TLS certificate-binding token flow of `engine-network` and
`engine-net-quic` does not apply; there is no certificate to bind):

- Spoofed identity. A P2P (`ConnectP2P`) connection is authenticated by Steam: the
  remote `SteamId` comes from the connection's identity, signed by Steam's certificate
  authority, and cannot be chosen by the peer. The server records that identity as the
  peer's SteamID. A connection by IP address from a signed-in Steam client also reports the client's
  Steam identity on App 480 (observed live), but a peer without Steam credentials shows only
  its address (matched by `ip:<addr>`, so concurrent IP clients stay distinct; the text form is
  parsed by `parse_ip_identity`, unit-tested against the `ip:a.b.c.d:port`, `ip:[::1]:port`
  and `ip:[::ffff:a.b.c.d]:port` forms Valve documents, but the fallback was never observed
  live because a signed-in client always carries a Steam identity - if the text form
  differs the remote is `Unknown` and concurrent address-only clients could be confused); the
  server therefore never trusts an IP connection's identity without a ticket.
- Replayed or stolen identity claims. The client sends its SteamID and an auth session
  ticket (`ISteamUser::GetAuthSessionTicket`) in a `Hello` frame as the first message.
  The server calls `BeginAuthSession` for that SteamID and ticket and admits the peer
  only on a successful `ValidateAuthTicketResponse`. Steam rejects a ticket that is
  invalid, for another game, expired, or already in use. A claimed SteamID that differs
  from a P2P connection's authenticated identity is refused before the ticket is checked.
- Policy. `AuthPolicy::Ticket` (default) always requires a valid ticket. `AuthPolicy::
  IdentityOnly` skips the ticket and is accepted only for P2P listeners, where Steam
  already authenticated the identity (it exists for single-account loopback tests, where
  Steam allows one auth session per SteamID in a process). An IP listener refuses
  `IdentityOnly` at construction.
- Duplicate logins. A SteamID that is already admitted cannot log in again. A claim for a
  SteamID whose ticket is still being validated replaces the older pending claim only when
  Steam itself authenticated the new connection as that SteamID (a P2P or signed-in
  connection): that is the real account, so it wins. A peer without a Steam-authenticated
  identity (a bare IP connection) can claim any SteamID, so it never evicts a pending
  validation: at most one validation per SteamID is allowed for such peers and the newcomer
  is refused. Verdicts already queued for a replaced ticket are discarded so they cannot
  admit the new peer. Ticket validation always decides admission.
- Server identity. P2P: the client dials a SteamID, so Steam authenticates the server.
  IP: the server is unauthenticated, which makes IP mode suitable for LAN, development
  and dedicated servers at a known address only. The server's `Accept` frame repeats
  its SteamID; `SteamClientConfig::expected_server` (set for P2P automatically) turns a
  mismatch into a refusal.
- Eavesdropping and tampering. Steam encrypts and authenticates every connection
  (`TransportSecurity::PlatformEncrypted`). IP connections without Steam certificates
  have only basic protection, which is another reason not to use them across untrusted
  networks.
- Resource exhaustion and malformed input. Pending (unadmitted) connections count against
  `max_peers`, must finish the handshake within `handshake_timeout`, and a bounded
  backlog of connection requests is processed `max_accepts` per poll; further requests are
  rejected. Every received frame is untrusted: an empty frame, reserved header bits, a
  truncated sequence number, an oversize message or any data
  before admission closes that peer. `poll` is bounded by `PollBudget` for dequeued
  messages and bytes, and tickets are capped at 1024 bytes.
- Revocation. A later failed `ValidateAuthTicketResponse` for an admitted peer
  disconnects it. The auth session ends and the client's ticket is cancelled when the
  link closes.

The SteamID of a peer is available from `peer_steam_id(peer)` as soon as
`LinkEvent::Authenticated(peer)` is delivered (the shared `LinkEvent` carries only the
`PeerId`).

### Lanes and delivery

Every Steam message is `[header][payload]`. The header byte holds the lane in bits 0-3
and the delivery in bits 4-5 (`ReliableOrdered`, `ReliableUnordered`,
`UnreliableSequenced`, `Unreliable`); bits 6-7 are zero for data and `0x80`/`0x81` mark
the `Hello` and `Accept` control frames. `UnreliableSequenced` adds a 4-byte per-lane
sequence number after the header; the receiver keeps the newest sequence per
`(peer, lane)` and drops anything not newer (counted in `dropped_unreliable`), as the QUIC
transport does. Mapping to Steam send flags: reliable deliveries use `Reliable`;
unreliable ones use `Unreliable | NoNagle`. Reliable sends are flushed at the end of each
`poll` so Nagle batching adds at most one tick. All reliable lanes share one ordered
Steam stream (like TCP), so a stalled reliable lane delays the others; Steam's
connection lanes are not used.

### Limits and backpressure

`max_payload` is Steam's per-message send limit (512 KiB) minus the header and, for
sequenced delivery, the sequence number. Oversize payloads, lanes out of range and
unknown or unadmitted peers return `Error::Invalid`. When Steam reports its send buffer
full a reliable send returns `Error::QueueFull` and queues nothing; an unreliable send is
dropped and counted in `dropped_unreliable`.

### Relay readiness

P2P needs the Steam Datagram Relay network, which initialises asynchronously after
`init_relay_network_access`. Both endpoints request it on creation and expose
`relay_status()` (`NotStarted`, `Pending`, `Ready`, `Failed`). A client does not dial
until the relay is `Ready`; it stays in a waiting state, and a relay that is not ready
within `relay_timeout` or reports `Failed` produces `Disconnected` with the reason. IP
mode needs no relay. A listener can be created before the relay is ready; clients cannot
reach it until it is.

## Lobbies and matchmaking (S7)

`SteamLobbies` (`crates/engine-steam/src/lobby.rs`) is the game-facing lobby API over a
`SteamSession`; it is a Bevy `Resource`. Steam's lobby calls are asynchronous, so they sit
behind `LobbyBackend`: requests (`lobby_create`, `lobby_join`, `lobby_request_list`,
`lobby_set_data`) return at once and their results arrive as `LobbyRaw` callbacks, in
whatever order Steam delivers them. `RealBackend` implements it over
`steamworks::Matchmaking`; `FakeBackend` implements it with a scriptable lobby world.

```rust
let mut lobbies = SteamLobbies::new(session.clone())?;
lobbies.create_p2p(LobbyVisibility::Public, 4, 0)?;   // host: publishes SteamID + virtual port
// each tick, after the plugin has pumped Steam callbacks:
for event in lobbies.poll() { /* Created, Joined, MemberJoined, OwnerChanged, ... */ }
// a joiner: list, join, then dial the host
lobbies.list(&LobbyFilter { string_equals: vec![("mode".into(), "ctf".into())], open_slots: 1, ..Default::default() })?;
lobbies.join(lobby)?;
let client = lobbies.connect_client(lobby, SteamClientConfig::default())?;
```

Normalisation. A raw callback only says that a lobby changed; the truth is a snapshot
read with `lobby_info` and diffed against the last snapshot the crate reported. Therefore
duplicated, late or reordered callbacks never produce duplicate or missing events. A
change for a lobby this client has not entered yet (a creator is told it entered before
`LobbyCreated` can arrive; a joiner can be told of changes before `LobbyEnter`) is held,
bounded, and replayed when the lobby becomes known. A lobby that disappears, or a
snapshot without the local user, becomes `Left`. Events: `Created`, `CreateFailed`,
`Joined`, `JoinFailed` (`Full`, `NotFound`, `NotAllowed`, `Other`), `ListReceived`,
`ListFailed`, `MemberJoined`, `MemberLeft`, `OwnerChanged`, `DataChanged` (per key,
`None` when removed) and `Left`. Steam does not say why a member left, so `MemberLeft` has
no reason. A single empty snapshot (or one that lacks the local user) is not enough to
report `Left`: a second consecutive look on the next `poll` must agree, so a transient
read cannot end a membership. Leaving while a join is in flight reports `Left` at once and
undoes the join when Steam completes it. The real backend coalesces change callbacks per
lobby and caps the queue (256) in case nobody polls `SteamLobbies`. Lobbies hold at most 250 members; keys are at most 255 bytes and values 8192.
Only the owner may set lobby data (checked locally before Steam). Filters support
string equality, free slots and a result count; the real backend searches worldwide.

Connect identity. The host publishes its SteamID and virtual port under the metadata keys
`engine.p2p_host` and `engine.p2p_port` (`create_p2p`, or `publish_host` again after
becoming owner by migration). `connect_identity` returns them only while the host key
names the current owner, so metadata left by a previous host is ignored, and
`connect_client` builds a `SteamClientEndpoint` that dials the host (it refuses when the
local player is the host). Lobby metadata is untrusted input: a lobby owner can claim any
host, but the client dials that SteamID through Steam's authenticated P2P path and the
server still validates the client with an auth ticket, so a lying lobby cannot make the
client trust the wrong peer; at worst it makes the client dial one that refuses.

`listed_identity` (for showing a listing) is a hint only and is not checked against the
owner: never dial it, join first and use `connect_identity`/`connect_client`.

`engine-online` adapter. With the `online` Cargo feature, `OnlineAdapter::convert` maps
member, host, attribute and leave events to `engine_online::ServerBody` notifications
(`MemberJoined`, `MemberLeft`, `HostChanged` with a counted epoch (local to this adapter, restarting at 1 per client, not
shared between players), `AttributesChanged`,
`Left`) so code written for a self-hosted session can follow a Steam lobby. Session ids are
the low 53 bits of the lobby id (the protocol keeps ids exactly representable as JSON
numbers); a player is its decimal SteamID; engine-internal keys are not exposed as
attributes. `engine-online` itself is unchanged.

Known limits. Steam's `LobbyChatUpdate` binding panics on a state-change code it does not
know; Steam sends only the five documented values. The old synchronous lobby methods on
`SteamBackend` (`create_lobby` and friends) are the S1 sketch and are superseded by
`LobbyBackend`.

## Live tests

Live tests are `#[ignore]` and use App ID 480 (Spacewar). With the Steam client open:

```text
cargo test -p engine-steam --features steam -- --ignored core --nocapture
```

`-- --ignored lobby_` creates a private lobby on App 480 with a published P2P host identity,
changes its metadata, observes Steam's confirmations and leaves it.
`-- --ignored net` runs the Networking Sockets loopbacks (`net_ip`: `ConnectByIPAddress` to a
local listen socket with auth tickets; `net_p2p`: `ConnectP2P` to the signed-in account's own
SteamID through the relay network): both exchange ordered reliable messages, a 200 KB message and a
sequenced datagram, then disconnect. Run them with `--test-threads=1`.

With Steam closed the same command passes and prints `skipped: Steam not running`.
`-- --ignored stats` (read-only; never writes to the account) lists Spacewar stats such as
`NumGames` and achievements such as `ACH_WIN_ONE_GAME`. Live tests share one Steam
client per process and take turns. `-- --ignored cloud` writes, lists, reads and deletes
the file `saves/engine_steam_live.json` on App 480 and prints whether Cloud is enabled
for the app and account; when it is disabled it checks that writes are refused instead.

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

- S7: lobbies and matchmaking.
- S8: Steam Input as an input source.
- S9: Workshop service (create, update, subscribe, query).
- S10: Workshop content loading through extra read-only asset roots.
- S11: Steam packaging (depot layout), `engine` facade feature, tooling and CI.
