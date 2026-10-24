# Authoritative multiplayer baseline

`engine-network` is an opt-in Rust library. Creating an engine does not open a
socket. `Server::bind(ServerConfig::default())` listens on an OS-assigned loopback
port; choose an explicit address to expose it to a trusted LAN. Clients connect
with `ClientConfig::new(server.local_addr()?)`. Connection establishment blocks
only up to the configurable timeout; subsequent polling uses nonblocking TCP.
Dropping/disconnecting an endpoint closes its owned sockets. There are no worker
threads or global services. `Client::from_stream` accepts a host-established TCP
connection and takes ownership; internal framing is independently exercised with
injected partial-read/write transports.

## Authority and application integration

1. Poll the server and clients regularly using `PollBudget::default()` or smaller
   budgets. Wait for `Connected(peer_id)`; IDs are assigned by the server, unique
   within that server lifetime, and are not authentication credentials.
2. The server application calls `assign_owner(entity_id, peer_id)` for each
   controllable entity. This grants input permission; it does not spawn an entity.
3. A client calls `send_input(InputCommand { tick, entity_id, movement, buttons })`.
   Movement components must be finite and in [-1, 1]. Commands express intent,
   never authoritative transforms. Unknown JSON fields, including transform
   injection in input, are rejected. The server exposes authorized commands as
   `ServerEvent::Input`; the simulation must apply its own movement speeds,
   collision rules, button semantics and game-specific authorization.
4. The server publishes a complete `Snapshot { tick, entities }` after simulation.
   Each entity has a stable string ID, translation, xyzw unit quaternion, scale,
   and velocity. Transforms must be finite with magnitude at most 1e12. Clients
   validate and atomically replace the full entity set. Missing IDs despawn;
   previously absent IDs spawn. Ownership grants remain explicit server state;
   revoke grants when deleting entities. Disconnect revokes every grant for that
   peer. A snapshot never grants authority.
5. `client.state().sample(render_tick)` linearly interpolates translation, scale,
   and velocity and spherically interpolates quaternion rotation between the last
   two snapshots. Sampling is clamped, with no extrapolation. New entities appear
   immediately; despawned entities never reappear during interpolation. Sampled
   snapshots retain the latest authoritative tick; render_tick is in simulation
   ticks, usually one publication interval behind the server. Apply sampled data
   to local scene objects using stable IDs. This crate does not own an ECS/world.

Snapshot ticks strictly increase. Accepted input ticks strictly increase per
peer (one aggregate input command per peer per simulation tick), and may lead the
latest published server tick by at most `max_input_tick_lead` (default 256).
Foreign ownership, replay and excessive future input produce `InputRejected`;
malformed/direction-invalid messages disconnect the peer. The application should
aggregate controls or assign separate peers if it needs a different input model.
Client input ticks are checked locally as well; server checks protect raw clients.
Late joins receive subsequent full snapshots; the server does not retain/replay
the last broadcast. The server must continue publishing state for idle observers.

`send_input`, `send_heartbeat` and `publish_snapshot` queue work; `poll` transmits
it. Clients should queue heartbeat frames well within idle_timeout when they have
no inputs. A complete valid frame refreshes idle activity; partial byte trickles
do not. Servers should publish periodic snapshots so clients see activity. The
caller owns scheduling and simulation tick rate. No network call sleeps in poll.

## Resource and failure contract

Defaults: 16 peers, 1024 entities/ownership grants, 128 UTF-8 bytes per ID, 256 KiB
JSON frame, 32 queued frames and 1 MiB queued serialized bytes per connection.
Limits are validated on construction. IDs are nonempty and exclude control
characters. Duplicate IDs and non-unit rotations are rejected. Each receive
buffer holds at most one bounded frame. Encoding uses a capped writer so an
oversized snapshot fails without constructing an unbounded serialized buffer.
A four-byte big-endian unsigned payload length precedes each UTF-8 JSON message.
The server sends a version-1 welcome before snapshots; endpoints reject unexpected
message directions and versions. TCP preserves reliable ordered delivery, with
head-of-line blocking and no lossy-packet simulation.

PollBudget bounds messages parsed per connection, bytes read per connection,
bytes written per connection, and connection acceptance attempts. Values are
clamped to Limits; total server work scales with the capped peer count. Zero byte
budgets do no socket reads/writes. Zero acceptance budget accepts no connections.
Poll checks idle timeouts even with zero I/O budgets. OS socket buffers and kernel
listen backlog are outside these application memory caps. Partial reads/writes
and WouldBlock preserve framing; malformed, oversized, closed, idle or failed
connections close and emit Disconnected. Server listener/setup failures return
Result errors. Disconnection does not clear the client's last snapshot, allowing
the application to display frozen state while handling the event.

Queue insertion never evicts or silently drops state. `QueueFull` is explicit;
snapshot publication is atomic across all current peers and does not advance the
published tick on failure. Retry after polling, reduce publication frequency, or
explicitly disconnect a slow peer. A failed send does not advance client tick.
The same published tick cannot be reused after a successful broadcast. Caller-
provided limits are trusted configuration and can intentionally raise memory use.

## Security scope and design choice

This is a plaintext trusted-network baseline, not a public Internet security or
AAA networking claim. There is no encryption, peer authentication, matchmaking,
NAT traversal, reconnect/session recovery, rollback/prediction, delta compression,
interest management, transport congestion tuning or anti-cheat assurance. A
reachable stranger can occupy a peer slot and receives every published entity.
Bind only to a trusted interface, or add an authenticated encrypted deployment
boundary before using it outside that trust model. Never send secrets in snapshots.
Server ownership gates protect simulated objects against another connected peer's
input; connection identity is not human/account identity.

TCP plus length-prefixed JSON was chosen over a UDP reliability layer for a small,
inspectable executable baseline with existing serde dependencies. It trades
bandwidth and latency under loss for reliable ordering and straightforward bounded
I/O. The pure snapshot model and owned connected-stream entry point keep simulation
and connection setup independent. A future transport must preserve authority,
validation, bounded queues, and observable errors; transport substitution alone
must not accept client-authored transforms.

## Verification

`cargo test -p engine-network` covers real ephemeral loopback sockets with two
clients, server-assigned identities, allowed/foreign input, replay/future tick
rejection, transform injection, full-set spawn/despawn, interpolation, peer/queue
caps, zero budgets, idle teardown, oversized frames, and injected byte-at-a-time
framing/partial writes. Tests own/drop all sockets and use no external service.
`cargo clippy -p engine-network --all-targets -- -D warnings` and
`cargo fmt -p engine-network --check` cover the crate's static gates. Workspace
integration and platform-specific network deployment remain separate checks.

## Runtime attachment and callbacks

`engine_project::networking` adapts the transport to authored ECS entities.
`engine.NetworkReplica` is version 1 with `{ "authority": "server" }` or
`{ "authority": "remote" }` data. The shared catalog includes this builtin
descriptor automatically, and `Runtime` registers its native adapter before
hydration. Do not redeclare it or register it again in game configuration.
The descriptor requires `Transform`. Merely
loading that metadata does not establish a session. Native game configuration
explicitly creates a `Server` or `Client` and passes it to `attach_server` or
`attach_client` with `NetworkSettings` before startup. `NetworkSession` owns the
endpoint as an ECS resource; `status(world)` and the optional runtime snapshot
`network` field report its role, peers, snapshot
progress, accepted/rejected input counts and last failure. `NetworkVelocity` is
optional native velocity metadata for the replicated pose.

Server input polling emits targeted `network.input` events containing peer_id,
tick, entity_id, movement and buttons. Registered game event callbacks determine
how inputs affect authoritative state. Without a callback, an accepted input
cannot move an entity. `assign_owner(world, id, peer_id)` is restricted to existing
server-authoritative replicas. Call `revoke_owner(world, id)` before deleting or
reassigning gameplay ownership. Connected/disconnected, rejected-input and client
snapshot events use the same bounded engine event system and are available to
headless/MCP event reporting. Callbacks can use `send_input(world, command)` on a
client. Runtime polling automatically queues client heartbeats.

The server captures only `server` replicas after simulation. Clients only apply
IDs explicitly authored as `remote` in their initial scene. These local authored
templates determine visual/custom components for respawn; the network carries no
code, asset references, component definitions or arbitrary entity templates.
Unknown incoming IDs are ignored. Full snapshots remove absent remote entities
and recreate eligible templates when their IDs reappear. Local entities and their
scripts are preserved. The allowlist is fixed at attachment; registering arbitrary
dynamic prefab templates is not implemented. A remote ID taken over by a local
entity causes a visible error instead of overwriting it.

Replicas currently require root-level TRS; parenting is rejected rather than
silently converting sheared hierarchy matrices. Remote replicas cannot have
scripts, script ownership, local children, or any `engine.PhysicsBody2D`,
`engine.PhysicsBody3D`, `engine.AnimationPlayer`, or `engine.NavigationAgent` pose
writer. Runtime guards run before startup callbacks, polling, and post-update
simulation, as well as before applying remote poses.
`NetworkSettings` sets per-peer poll budgets, simulation tick duration, and client
interpolation delay. Invalid application poses or transport errors propagate as
runtime errors; bounded queues are never silently dropped.

`Runtime` automatically validates after typed hydration before callbacks, polls
before event dispatch/scripts, captures authoritative poses after simulation
and before extraction, and closes sockets on shutdown (including failed ticks).
The adapter's `validate`, `before_step`, `after_step` and `shutdown` functions
remain available to custom hosts; games using `Runtime` must not call them a
second time. Network input received by polling can dispatch in the same tick;
events emitted by callbacks retain the ordinary next-tick queue behavior.
Sessions are scoped to one scene because the wire protocol has no scene
namespace. Both native and runtime scene-request APIs reject attached sessions
until `detach(world)` closes them, including disconnected but still attached
endpoints. Detachment and shutdown preserve the final status report.

`cargo test -p engine-project --test networking` exercises two real runtimes with
loopback TCP, input callbacks, local script preservation, full-set remote
spawn/despawn, unknown-ID filtering, script authority conflicts, opt-in loading,
scene-transition guards, and shutdown disconnects through ordinary runtime ticks.
