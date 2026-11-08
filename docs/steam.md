# Steam integration

The `engine-steam` crate is the engine's optional Steamworks integration. Steam is
off by default: a normal build contains no Steam code and no Steam dependency, and
nothing in a game changes unless it opts in.

Status: the skeleton (S1), the real backend core (S2), the host integration (S3),
achievements and stats (S4), Cloud saves (S5), the Networking Sockets transport (S6),
lobbies (S7), Steam Input (S8), Workshop (S9), Workshop content roots (S10) and the
Windows parts of S11 (the `engine::steam` facade feature, the `steam_status` MCP tool,
CI checks and the Windows Steam-variant procedure) of
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

`engine project package` places the library for you when the tooling has the backend and
the manifest has a `steam` block, see [Shipping the Steam variant](#shipping-the-steam-variant).

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

## Using Steam from a facade game

A game that depends on the `engine` facade gets the Steam API as `engine::steam`
(`engine-steam`) in every build: the backend trait, `NullBackend`, `FakeBackend`,
`SteamSession`, `SteamPlugin`, `status_json` and the rest. Without a feature
`default_backend()` returns a `NullBackend` (status `not_compiled`). The real backend is
the facade's `steam` feature, which a game forwards as its own opt-in:

```toml
[features]
steam = ["engine/steam"]
[dependencies]
engine = { git = "...", tag = "v0.1.0" }
```

`cargo build --features steam` then links `steamworks` (`engine::steam::STEAM_COMPILED`
is true). The default `cargo tree -p engine -e normal` has no `steamworks`.

A game that calls `engine::desktop::run` itself wires the host service the way
`engine_tooling::runtime_entry` does:

```rust
use engine::steam::{SteamPlugin, SteamSession, default_backend};

let project = Project::load("project.json")?;
let steam = project.steam.clone();
let runtime = Runtime::new(project)?;
let session = steam.as_ref().map(|_| SteamSession::new(default_backend()));
let services: Vec<Box<dyn engine::desktop::HostService>> = match (&session, steam) {
    (Some(session), Some(manifest)) => vec![Box::new(SteamPlugin::new(session.clone(), manifest))],
    _ => Vec::new(),
};
engine::desktop::run_with_services(runtime, options, None, services)?;
```

`crates/engine/tests/steam.rs` runs this wiring through the facade with a `FakeBackend`.
Games that go through generated runtimes (`engine project package`) need no code: the
runtime entry does it for any project with a `steam` block when the tooling is built with
`--features steam`.

## Host integration (`steam` manifest block)

A project with a `steam` block (see [Project format](project-format.md)) gets the Steam
host service when it runs through `engine_tooling::runtime_entry::run` (the entry point
of generated game binaries) or when a facade game's `main` wires it itself (see
[Using Steam from a facade game](#using-steam-from-a-facade-game)). The block is plain
data in every build; to link the real backend build `engine-tooling` with
`--features steam` (generated runtimes then depend on `engine-tooling/steam`), otherwise
the status is `not_compiled`.

Runs started by the editor or by `engine run` never touch Steam: both start the generation
with a readiness handshake (`ENGINE_GENERATION_READY_FILE`), and such generations skip the
Steam service. Run the Steam variant by launching the compiled runtime directly or from a
package, see [Shipping the Steam variant](#shipping-the-steam-variant).

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

## Steam Input (S8)

`SteamInputSource` (in `crates/engine-steam/src/input.rs`) feeds Steam Input actions into the
engine's `InputDevices`, so a Steam Input action binds to an engine action exactly like any
other native peripheral. Keyboard, mouse and the existing gamepad path are unchanged; Steam
Input is an additional device.

### `steamworks` 0.13.1 Input surface (verified)

The `steamworks::Input` wrapper (`input()` on the client) offers: `init(explicit_run_frame)`,
`shutdown`, `run_frame`, `get_connected_controllers(_slice)`,
`set_input_action_manifest_file_path`, `get_action_set_handle`, `activate_action_set_handle`,
`get_digital_action_handle` / `get_analog_action_handle`,
`get_digital_action_data` (`bState`, `bActive`) / `get_analog_action_data` (`eMode`, `x`, `y`,
`bActive`), `get_digital_action_origins` / `get_analog_action_origins`,
`get_string_for_action_origin`, `get_glyph_for_action_origin` (the legacy glyph-path call),
`get_input_type_for_handle`, `get_motion_data` and `show_binding_panel`. That covers an
action-based source. Binding quirks the backend works around:

- `get_connected_controllers` returns a vector padded to 16 with zero handles (its
  `shrink_to` does not truncate); the backend uses the slice variant and its count.
- `sys` is private in the wrapper, so the 16-controller limit is a local constant.
- Not exposed: the newer PNG/SVG glyph calls, controller connect/disconnect callbacks (the
  source diffs the controller list each poll instead), haptics/LEDs, and `EInputActionOrigin`
  as a Rust enum (origins are carried as their numeric value, `ActionOrigin`).

### Using it

```rust
let layout = SteamInputLayout::new()
    .digital("jump")          // button control "jump"
    .digital("menu_select")
    .analog("move");          // axis controls "move.x" and "move.y"
register_input_source(world, SteamInputSource::new(session, layout)?)?;
// Bind the controls to engine actions (device id is STEAM_INPUT_DEVICE_ID, "steam_input"):
//   Jump    <- steam_input / jump
//   Forward <- steam_input / move.y (positive)     Back <- steam_input / move.y (negative)
```

The names in the layout must match the action names in the action manifest. Steam reports
analog `y` positive for up/forward; bind `AxisDirection::Negative` for the opposite direction.
`SteamInputLayout::with_manifest(path)` loads a local manifest (development only).

Each tick the engine polls the source once: it calls `RunFrame`, lists controllers, then reads
every declared action on every controller. Several controllers merge (a button is down if any
controller holds it; an axis takes the largest magnitude). With Steam absent, or no controller
connected, the device is disconnected (no error); an action missing from the manifest is
reported as a device error in `InputDevices::snapshot`. Steam Input is initialised
(`ISteamInput::Init(true)`, explicit `RunFrame`) by the first poll and shut down when the real
backend is dropped.

Through `SteamSession`:

- `set_action_set("gameplay")` activates a Steam action set on every connected controller and
  remembers it, so controllers that connect later get it on the next poll. An unknown set is
  rejected and changes nothing. Actions outside a controller's active set read as neutral.
- `controllers()` lists the connected controllers.
- `action_glyphs(controller, set, action, kind)` returns, per physical control the action is
  bound to, its localised name and a glyph image path, for on-screen prompts.
- `ControllerConnected` / `ControllerDisconnected` events are raised from the poll and surface
  as `steam.controller_connected` / `steam.controller_disconnected` engine events on the
  next tick (`SteamSession::run_callbacks` also returns them for hosts without the plugin).

### Action manifest

Steam Input needs an action manifest configured for the app. Upload it in the Steamworks
partner site (Steam Input, "Game Actions File") for a shipped game; for development set it with
`SteamInputLayout::with_manifest`. A sample `game_actions_480.vdf` matching the layout above
(`game_actions_<appid>.vdf`; keep action names lowercase and identical to the layout):

```text
"In Game Actions"
{
    "actions"
    {
        "gameplay"
        {
            "title"   "#Set_Gameplay"
            "StickPadGyro"
            {
                "move"
                {
                    "title"      "#Action_Move"
                    "input_mode" "joystick_move"
                }
            }
            "Button"
            {
                "jump"   "#Action_Jump"
            }
        }
        "menu"
        {
            "title"   "#Set_Menu"
            "Button"
            {
                "menu_select"   "#Action_Select"
            }
        }
    }
    "localization"
    {
        "english"
        {
            "Set_Gameplay"   "Gameplay"
            "Set_Menu"       "Menu"
            "Action_Move"    "Move"
            "Action_Jump"    "Jump"
            "Action_Select"  "Select"
        }
    }
}
```

Players (and you, during development) then choose or edit a controller configuration in Steam
for the app; without a configuration for the manifest's actions, Steam Input reports the
actions as inactive. App 480 (Spacewar) has no manifest of ours, so the live test only
enumerates controllers.

## Workshop (S9)

`UgcService` (`crates/engine-steam/src/workshop.rs`) is the game-facing Workshop handle:
create, update and submit items, subscribe and unsubscribe, query, read install details and
download. Steam's Workshop calls are asynchronous, so a request returns a `RequestId` and its
outcome arrives from `UgcService::poll()` (call it once per tick, after the session pumped
callbacks) as a `UgcEvent`: `Reply(UgcRaw::...)` for finished requests, `UpdateProgress` for
a submission and `DownloadProgress` for a download. The same pattern as lobbies applies: the
backend side is the defaulted `ugc_*` methods on `SteamBackend` plus `ugc_poll`, with a
scriptable fake (`FakeBackend::with_workshop_item`, `with_installed_item`,
`with_download_polls`) and `RealBackend`. The old synchronous `subscribe_item` /
`unsubscribe_item` on `SteamBackend` are the S1 sketch; `subscribed_items` and
`installed_item` are shared with the service.

### `steamworks` 0.13.1 UGC surface relied on (`UGC`, from `client.ugc()`)

`create_item(app, FileType, cb)`; `start_item_update(app, id)` returning an `UpdateHandle`
(`title`, `description`, `visibility`, `tags(Vec, allow_admin_tags)`, `content_path`,
`preview_path`, `submit(note, cb)` returning an `UpdateWatchHandle` whose `progress()` gives
`(UpdateStatus, bytes_done, bytes_total)`); `subscribe_item` / `unsubscribe_item` (call results);
`subscribed_items(include_locally_disabled)`; `item_state` (bit flags `SUBSCRIBED`, `INSTALLED`,
`NEEDS_UPDATE`, `DOWNLOADING`, `DOWNLOAD_PENDING`); `item_install_info` (folder, size on disk,
timestamp); `item_download_info` (bytes); `download_item(id, high_priority)`;
`query_all(kind, UGCType, AppIDs, page)` and `query_items(ids)` returning a `QueryHandle`
(`add_required_tag`, `set_search_text`, `fetch(cb)`), whose `QueryResults` give `get(i)`,
`total_results`, `statistic(i, ..)`; and the `DownloadItemResult` callback. Quirks:
`QueryResults` borrow Steam memory only inside the `fetch` callback (the service copies them
into `WorkshopItem`); an unknown visibility value would panic inside the binding (Steam only
sends the four documented ones); `query_items` must not be empty (the service answers an
empty by-id query itself);
`delete_item`, playtime tracking and key/value tags are not exposed by the service.

### Using it

```rust
let mut ugc = UgcService::new(session.clone())?;
let request = ugc.query_public(&PublicQuery { required_tags: vec!["map".into()], ..Default::default() })?;
let sub = ugc.subscribe(item)?;                 // reply: UgcRaw::Subscription
ugc.download_item(item, true)?;                 // DownloadProgress ... Reply(Downloaded)
if let Some(installed) = ugc.installed(item)? { /* installed.path, size_bytes, timestamp, needs_update */ }
for event in ugc.poll() { /* UgcEvent */ }
```

- Queries: `query_public(PublicQuery { sort, search, required_tags, page })` (up to 50 items per
  1-based page; `TextSearch` needs `search`), `query_items(ids)` (1 to 50 ids) and
  `query_subscribed()` (the first 50 subscribed items). Replies carry a `QueryPage { items, total }`.
- `installed(id)` returns `InstalledItem { id, path, size_bytes, timestamp, needs_update }`, or
  `None` when Steam has no install for it. `path` is the item's content folder on disk and is
  what Workshop content loading (S10) mounts as a read-only root. Treat it as untrusted
  content from other players.
- `download_item` completes with `Reply(Downloaded { item, result })`; on success the session
  also raises `SteamEvent::WorkshopItemInstalled(item)` (`steam.workshop_item_installed`).
  Steam normally downloads subscribed items by itself; `download_item` forces or prioritises it.
- `item_state(id)` reports subscribed, installed, needs-update, downloading and pending flags.
- `ItemUpdate` is validated before reaching Steam, against the buffers in the Steamworks
  headers (each limit counts Steam's terminating NUL): title 1 to 128 bytes, description and
  change note at most 7999 bytes, at most 100 tags of 1 to 255 bytes without commas, the
  comma-joined tag list at most 1024 bytes, no NUL anywhere, and the content folder and
  preview file must exist.
- Tracking is bounded. A download also ends (reply `Downloaded Ok` plus the installed event)
  when Steam reports the item installed, current and no longer downloading, because Steam
  sends no callback for an item that is already up to date. A submission or download Steam
  never answers ends with an error reply after the tracking timeout (default two hours,
  `set_tracking_timeout`). The real backend refuses new requests while 128 replies wait
  unpolled and never evicts a request's reply; only unsolicited download notifications are
  dropped when 256 replies pile up.

### Binding panics, contained

The `steamworks` 0.13.1 binding panics in places: the `UpdateHandle` setters `assert!` on
Steam's boolean answer and `unwrap` the path canonicalisation, and `QueryResults::get`
reaches `unreachable!()` for a visibility or file type it does not know. The real backend
builds update handles under `catch_unwind` (returning `Rejected`, on the calling thread) and
converts query results under `catch_unwind` inside the Steam callback (answering with an error
reply), so a panic never unwinds across Steam's C callback boundary. On Windows the binding
canonicalises the content and preview paths and so hands Steam a `\\?\`-prefixed path; the
binding gives no way to change that, and an upload was not run to confirm Steam accepts it
(see the manual procedure below). Two conversions remain in the binding: visibility maps to
`ItemVisibility` and the file type is read but not exposed.

### Mounting installed items as content roots (S10)

`WorkshopContent::new(project.content_roots.clone())` mounts installed items as read-only
content roots (generic mechanism: `engine_project::content_roots`). Feed it the events of each
`UgcService::poll()` with `process(&service, &events)`, and call `mount_subscribed(&service)`
once at startup:

- a finished download mounts the item's install folder as mount `workshop-<id>`, or replaces
  the previous mount when the item updated (`WorkshopContentEvent::Mounted` / `Replaced`);
  while `needs_update` is true and the item is mounted, the old version stays;
- a completed unsubscribe removes the mount (`Unmounted { reason: None }`);
- a folder that fails validation is skipped with `Skipped { reason }` and nothing else stops;
  if that item was already mounted (an update that broke it), the stale mount is removed
  as well and the event is `Unmounted { reason: Some(..) }`, so the old manifest is never
  served against the new files;
- `reconcile(&service)` unmounts items that are mounted but no longer subscribed (an
  unsubscribe made in the Steam client or on another machine); `mount_subscribed` calls it
  first. A reloaded project has empty content roots, so call `mount_subscribed` again with
  the new project's `content_roots`. `reconcile` trusts the subscription list: if Steam
  transiently reports no subscriptions (an empty list while the client is still syncing),
  it unmounts every mounted item, and the next `mount_subscribed` remounts them from the
  installed folders. It does nothing when the list cannot be read at all. Do not run it in
  the middle of a frame that is loading mod assets; the unmount is immediate.

Mounted ids are only as trustworthy as the item that declared them: the item chooses each
asset's kind. Before passing a mod id to a loader that does not check kinds
(`services::resolve_asset`, `ml::load_asset`), resolve it with
`Project::resolve_asset_kind(id, &["mesh"])` (or `ContentRoots::resolve_kind`), see
[Checking the kind before loading](asset-pipeline.md#checking-the-kind-before-loading).

Mounted assets are addressed `@workshop-<id>/<id>` (`WorkshopContent::asset_id`) and resolve
through `Project::resolve_asset` / `services::resolve_asset`. An item folder needs a
`content.json` (same asset records as `project.json`) listing leaf data only: `mesh`,
`texture`, `audio`, `lut`, `string_table` and a bitmap `font` whose texture is in the same
item. Scripts, shaders, graphs and scene-like kinds are refused, as is any link anywhere in the
item; the folder must lie directly below the directory Steam reports as its parent. The
full rules are in [Additional read-only content roots](asset-pipeline.md#additional-read-only-content-roots).
Not yet supported (planned v2): scenes, prefabs and other composite kinds, and mod ids in
scene component fields.

### Uploading is manual only

`create_item` and `submit_item` change the Steam Workshop for the app. A new item is private
until its visibility is changed, but a submission to a **Public** item publishes the content to
everyone. No automated test or live test in this crate creates, updates or submits an item
(the fake world is used for those flows), and none should. To do an upload by hand:

1. Use the Steam account and App ID you own and accept the Workshop legal agreement for the app
   (the `UgcRaw::Created` reply says if it still needs accepting; accept it on the item page).
2. Run a small throwaway program (not a test) with Steam running that calls `create_item`,
   polls until `Created`, then `submit_item(id, &ItemUpdate::new().title(..).content_folder(..)
   .visibility(ItemVisibility::Private))`. Start with `Private` and look at the item on the
   Workshop page while it is hidden.
3. Watch `UpdateProgress` events until `Submitted`; change visibility with another
   `submit_item` only when the content is final.
4. Delete test items from the Steam Workshop page afterwards. Spacewar (480) is shared by every
   developer, so do not publish public test content there.

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

`-- --ignored input` initialises Steam Input, runs frames and lists controllers; it prints
`skipped: no Steam Input controller connected` when none is plugged in.

`-- --ignored workshop` is read-only: it queries Spacewar's public Workshop (first page), then
lists the subscribed items and their state and install info. It never creates, subscribes,
downloads or uploads.

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
and `examples/` (on macOS the same copy of `libsteam_api.dylib`, because its install name is
`@loader_path/...`), and on Linux adds an rpath to the library directory for test and
example binaries (`engine-tooling/build.rs` does the same for the Steam-feature `engine`
binary). Neither is exercised on those systems except by CI's build rows. This helper is for development
only; a shipped game gets the library from the package step. `steam_appid.txt` is
not needed because the tests pass the App ID to `init`.

## Shipping the Steam variant

The Steam variant is a separate build (see [Build variants](#build-variants)). Build the
tooling with the feature, add a `steam` block to `project.json` (`app_id`, optionally
`required`, `cloud`) and package:

```text
cargo build -p engine-tooling --features steam --bin engine
engine project package project.json <destination> [--format dir|zip|tar.gz|appimage|app|steam-depot]
```

Generations the Steam tooling compiles depend on `engine-tooling/steam`, so their
`runtime_entry` links the real backend (a facade game that runs itself builds with its own
`steam` feature instead). When the tooling has the backend **and** the manifest has the
block, every format also places Valve's library where the runtime loads it, and
nothing else changes: a default build, or a project without the block, is packaged
byte for byte as before (no library, no new file, the same generation fingerprint).
`steam_appid.txt` is never packaged; Steam supplies the app id when it launches the game.

| Format | Library placed at |
| --- | --- |
| `dir`, `zip`, `tar.gz` | next to `runtime[.exe]`: `steam_api64.dll`, `libsteam_api.so` (macOS hosts: `libsteam_api.dylib`) |
| `appimage` | next to `runtime` in the AppDir root (`libsteam_api.so`) |
| `app` (macOS) | `Contents/Frameworks/libsteam_api.dylib` |
| `steam-depot` | the table row of the host's package, below `content/<platform>/` |

### Finding the library

The package step needs Valve's redistributable for the host's OS and architecture
(`win64/steam_api64.dll`, `linux64` or `linuxarm64/libsteam_api.so`, `osx/libsteam_api.dylib`).
It looks, in this order, and uses the first hit:

1. `ENGINE_STEAM_API_LIB`: the path of the file, which must be named like the platform's
   library (`tooling.steam.library_invalid` otherwise). Used or refused, never skipped.
2. `STEAM_SDK_LOCATION`: a Steamworks SDK directory; the file is read from
   `redistributable_bin/<platform directory>/` below it. When the file is not there the
   lookup goes on (the same rule as `crates/engine-steam/build.rs`, and the lookup code is the
   same file, `crates/engine-steam/src/redistributable.rs`).
3. The `steamworks-sys` source in the Cargo registry (`$CARGO_HOME/registry/src/*/`, highest
   version), which is there after the `steam` feature has been built once. This is the
   pinned `steamworks-sys` that ships Valve's files (see
   [Redistribution terms](#redistribution-terms-for-the-bundled-steam-libraries)).

Tooling built with the Steam backend refuses any package of a manifest without a `steam` block,
before compiling (`tooling.steam.manifest_missing`; `--format steam-depot` reports
`tooling.steam_depot.manifest_missing`): the runtime links the backend whatever the manifest says,
so such a package would be missing the library and could not start. Use tooling built without
`--features steam` for a project without Steam.

Nothing found is `tooling.steam.library_missing`, reported before anything is compiled;
a platform Valve ships no library for is `tooling.steam.platform_unsupported`. The package
result JSON gets a `steam` object (`app_id`, `library.file`, `library.from`, `library.source`,
and `depot_id` for a depot) whenever a library was staged. The copy is byte for byte, owner
writable, and named as the loader asks (the original name).

### How the runtime finds the library on Linux and macOS

Evidence read from the pinned `steamworks-sys` 0.13.0 files (an ELF and a Mach-O parser
written for the purpose; no Linux or macOS machine was available):

- Linux `linux64/libsteam_api.so` (and `linuxarm64`) has `DT_SONAME` `libsteam_api.so`. A client links
  with `DT_NEEDED libsteam_api.so`, a bare name, which the loader searches for in
  `LD_LIBRARY_PATH`, the run path and the system directories, not in the executable's own directory.
  So the generated runtime project gets a build script (Steam tooling only; a default
  generation has none and keeps its fingerprint) that adds `-Wl,-rpath,$ORIGIN`, the
  directory of the runtime itself. The library placed beside the runtime then resolves with
  no `LD_LIBRARY_PATH`; `ldd` shows it in the smoke test.
- macOS `osx/libsteam_api.dylib` is a universal file (x86_64 and arm64) whose install name
  (`LC_ID_DYLIB`) is `@loader_path/libsteam_api.dylib`. A client records that name as
  its dependency, so dyld looks beside the *runtime*, in `Contents/MacOS` of a bundle, and an
  rpath cannot change that. For a plain directory or archive the library sits beside the runtime and
  loads as is. For the `.app` the library lives in `Contents/Frameworks` (Apple's place
  for nested code), so the packager rewrites the *runtime's* reference with
  `install_name_tool -change @loader_path/libsteam_api.dylib @rpath/libsteam_api.dylib`
  (the old name is read from the library file, not assumed; the new one is shorter, so no
  header padding is needed) and the generation's build script has added the rpath
  `@executable_path/../Frameworks`. Valve's file is not touched. The rewrite happens on the
  universal runtime before signing.

Windows needs nothing: a DLL beside the executable is found by the loader. Without the
file the process cannot start at all (loader error `0xC0000135`), whether or not Steam runs.
For development, `engine-steam/build.rs` copies the library beside Cargo's test and example
binaries (Windows, and now macOS for the reason above) or adds an rpath (Linux), and
`engine-tooling/build.rs` adds the run path to the Steam-feature `engine` binary on Linux.

### macOS signing outcome

Valve's `libsteam_api.dylib` already carries a signature: both architectures have a code
signature with a CMS blob (a certificate-backed signature, not ad-hoc), the code
directory flag `0x10000` (hardened runtime) and the identifier `com.valvesoftware.steam`.
It must not be modified, so the packager keeps it: `sign_bundle` skips it, and the bundle
(executable and resources) is signed around it. After signing, a Steam bundle is verified
with `codesign --verify --deep --strict`; if that fails the build stops with
`tooling.app.steam_signature_invalid` (nothing is published) rather than re-signing Valve's file. The
outcome of that verification on a real Mac is established only by the release workflow's Steam
macOS row (`codesign --verify --deep --strict` on the bundle and on the library alone, plus
the identifier check); it was not run when this was written. With the default ad-hoc
signature the executable has no library validation, so the library loads; with
`ENGINE_MACOS_SIGN_IDENTITY` the hardened runtime is on and Valve's library, signed by another
team, needs the `com.apple.security.cs.disable-library-validation` entitlement on the
executable, which the packager does not add (Developer ID signing stays unverified, see
[Desktop packages](desktop-packaging.md#signing)). That combination is therefore refused before
compiling with `tooling.app.steam_identity_unsupported`: a Steam bundle (`--format app` or
`steam-depot` on macOS) is built with the ad-hoc signature only. A Steam bundle also needs
`install_name_tool` on `PATH` (`tooling.app.tool_missing` otherwise, also before compiling).
`--bundle-id` is accepted for `steam-depot` on a macOS host only.

### `--format steam-depot`

```text
engine project package project.json dist/depot --format steam-depot [--depot-id <n>]
```

`<destination>` is a new directory (an existing one is `tooling.destination.exists`):

```text
dist/depot/
  content/<windows|linux|macos>/   Windows, Linux: the directory package plus the library;
                                   macOS: <id>.app (universal, signed, library in Frameworks)
  scripts/app_build_<app id>.vdf
  scripts/depot_build_<depot id>.vdf
  README.txt
```

One platform per invocation: the host's (`content/windows`, `content/linux` or
`content/macos`; macOS needs both Apple Rust targets and the macOS tools, as for `--format app`).
`<id>` is the project id (the manifest name reduced like the AppImage id). The result key is
`depot`. The app id is the manifest's `steam.app_id`. The depot id is `--depot-id <n>`, else
**app id + 1**. That default is the engine's convention, not a Steamworks rule: Valve's
[uploading guide](https://partner.steamgames.com/doc/sdk/uploading) numbers depots after the
app in its examples (app 1000, depots 1001 and 1002) and says nothing about how ids are
assigned, and the [depot page](https://partner.steamgames.com/doc/store/application/depots)
does not either. Check the id against the depot you created in the partner site and pass
`--depot-id` when it differs, and always for a second or third platform depot.

Refusals, all before anything is compiled: `tooling.steam_depot.manifest_missing` (no `steam`
block), `tooling.steam_depot.backend_missing` (tooling built without `--features steam`),
`tooling.steam_depot.depot_id_invalid` (zero or the app id), the library codes above, and
`--depot-id` with another format is a usage error.

`app_build_<app id>.vdf` (template; `SetLive` is empty, so a build is uploaded without
changing a branch; paths are relative to the script):

```text
"AppBuild"
{
	"AppID" "480"
	"Desc" "TODO: describe this build"
	"Preview" "0"
	"SetLive" ""
	"ContentRoot" "../content/"
	"BuildOutput" "../output/"
	"Depots"
	{
		"481" "depot_build_481.vdf"
	}
}
```

`depot_build_<depot id>.vdf`:

```text
"DepotBuildConfig"
{
	"DepotID" "481"
	"FileMapping"
	{
		"LocalPath" "windows/*"
		"DepotPath" "."
		"Recursive" "1"
	}
	"FileExclusion" "*.pdb"
	"FileExclusion" "*.debug"
	"FileExclusion" "*.dSYM"
}
```

Values are written as VDF quoted tokens (backslash, quote, line feed and tab escaped);
forward slashes keep paths free of escapes. No password, token or account name is ever written.

**Uploading** is manual (SteamCMD with partner credentials; the depot `README.txt` repeats this):

```text
steamcmd +login <account> +run_app_build "<absolute path>/dist/depot/scripts/app_build_480.vdf" +quit
```

SteamCMD asks for the password and Steam Guard code itself. Restrict each depot to its OS in the
partner site, and set the launch option there (`runtime.exe`, `runtime` or `<id>.app`).

**Combining the depots of three hosts.** Package on each host into its own directory with its own
depot id, for example `--depot-id 481` on Windows, `482` on Linux and `483` on macOS. Move
the three directories to one machine as archives that keep Unix permissions (tar, or zip with
modes; `ditto` for the `.app`, whose signature needs its links and attributes), then put every
`content/<platform>/` folder under one `content/`, every `scripts/depot_build_<id>.vdf` into
one `scripts/`, and keep a single `app_build_<app id>.vdf` with one `"<depot id>"
"depot_build_<depot id>.vdf"` line per depot inside `"Depots"`. The release workflow's three Steam rows
build exactly such per-host depots (for app 480) but only to smoke-test them: the zips are not uploaded, checksummed or
released, because the library inside may be distributed only as part of a game published through Steam.

### Recorded runs

Windows run recorded on 2026-11-03 with Steam running, App ID 480, using the first-project
example with `"steam": {"app_id": 480}` added and a package built by the Steam-feature
tooling (the library copied by hand):

```text
runtime.exe game/project.json --headless --ticks 3        (no steam_api64.dll)  -> does not start
runtime.exe game/project.json --headless --ticks 3        (dll next to the exe) -> exit 0
  "steam": {"app_id": 480, "state": "ready", "user": {"id": "<SteamID64>", "name": "<persona>"}}
```

Windows depot run recorded on 2026-11-08, Steam running, App ID 480, a copy of the
first-project example with `"steam": {"app_id": 480}` (made by
`scripts/package/prepare-steam-project.sh`), dev profile, tooling built with
`cargo build -p engine-tooling --features steam --bin engine`:

```text
engine project package <copy>/project.json <dir> --profile dev --format steam-depot
  result: "format": "steam-depot", "steam": {"app_id": 480, "depot_id": 481,
          "library": {"file": "steam_api64.dll", "source": "steamworks-sys", ...}}
<dir>/README.txt
<dir>/scripts/app_build_480.vdf, depot_build_481.vdf
<dir>/content/windows/{runtime.exe, steam_api64.dll, game/, licenses/, generation.json,
                       package-host.json, PACKAGE.txt, run.cmd, run.sh}
   steam_api64.dll is byte for byte the steamworks-sys file (same SHA-256); no steam_appid.txt
content/windows/runtime.exe --headless --ticks 3   (from another directory) -> exit 0
  "steam": {"app_id": 480, "state": "ready", "user": {"id": "<SteamID64>", "name": "<persona>"}}
```

The run report's `steam` object carries the status; with Steam closed the same command
reports `"state": "not_running"` and exits 0 (unless `required` is set). On a machine with no
Steam client installed at all, Steam reports a failed initialisation instead, so the state is
`init_failed` (still soft, exit 0). Neither case is run locally because Steam must stay open;
the release workflow's Steam rows run on runners with no Steam client installed and accept
either state (`scripts/package/smoke-steam-depot.sh`).
The Linux and macOS depots have not been run by a person: they are built, loaded and run only
by those rows.

## MCP `steam_status`

The read-only MCP tool `steam_status` ([MCP](mcp.md)) reports whether the build has the real
backend, the project's `steam` block (`declared`, `app_id`, `required`, `cloud`) and the
state this process has established (`initialized`, `status`, `user_name`). It never starts
Steam or calls the Steam API and returns no tickets or secrets; a server that only
inspects a project reports `not_started`. `engine_steam::published_status()` is the
process-wide value it reads: the status last observed by a `SteamPlugin` in the same
process.

## Redistribution terms for the bundled Steam libraries

`steamworks = "=0.13.1"` and its `steamworks-sys` crate are MIT/Apache-2.0 licensed
Rust bindings. `steamworks-sys` also ships Valve's Steamworks SDK headers and the
prebuilt `steam_api` redistributable libraries for Windows, Linux and macOS. Those
files are Valve's, not covered by the Rust crates' licences, and are governed by the
Steamworks SDK Access Agreement that every Steamworks partner accepts:

- Ship the `steam_api` library next to the game executable in your Steam builds (the package
  step does, and in a macOS bundle puts it in `Contents/Frameworks`). Use the redistributable
  that comes with the SDK; do not modify it. The package step copies the file byte for byte and
  never re-signs it.
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

- Evidence from real Linux and macOS runs: the CI rows lint and build the feature there,
  and the release workflow's Steam rows (smoke tests only, nothing is uploaded) package, load and run the depots, but results are
  recorded in this document only after a `workflow_dispatch` run has been read.
- Developer ID signing and notarisation of the Steam `.app`, including the library-validation
  entitlement (see [macOS signing outcome](#macos-signing-outcome)).
- Uploading to SteamPipe stays manual by design (SteamCMD with partner credentials).
