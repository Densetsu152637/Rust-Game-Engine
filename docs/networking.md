# Authenticated authoritative multiplayer

## Internet transport setup

`Server::bind_secure(config, ServerSecurity)` and
`Client::connect_secure(config, ClientSecurity)` use rustls TLS 1.3 over TCP.
Server certificate chain, validity and DNS name/IP SAN are verified against the
client's explicitly supplied trust roots. Every client must prove possession of a
private key for a certificate trusted by the server. The server then invokes a
required native `Fn(PeerId, &[CertificateDer]) -> bool` policy exactly once with
the verified certificate chain. The policy must authorize the participant for
this application/session, for example by matching a pinned certificate to an
account allowlist. CA membership alone need not grant gameplay access. The peer
ID lets native policy maintain its own account mapping; certificate/account data
is never placed in engine events or reports. Policy must be fast and nonblocking.

Create `ServerSecurity::new(server_chain, private_key, client_roots, policy)` and
`ClientSecurity::new(server_name, server_roots, client_chain, private_key)` from
native configuration or a host secret store. Inputs are owned DER certificates
and `PrivateKeyDer` keys, re-exported by `engine-network`. Config types deliberately
have no Debug/Serialize and do not accept a caller-built insecure rustls config.
Keys, certificate contents and account identifiers never belong in authored
scene data, MCP parameters, snapshots or logs. A native game configuration
callback can select credentials by a nonsecret deployment profile, then attach
the endpoint through the usual runtime API. Native configuration/build/exec
tooling remains the setup path; there is no secret-bearing MCP transport tool.

Secure endpoints require ALPN `rust-game-engine/1`, disable session resumption
and 0-RTT, and never retry using plaintext. `Client::from_secure_stream` accepts
a host-connected TCP stream while preserving the same verification. TCP connect
is bounded by `connect_timeout`; TLS progresses asynchronously through `poll`.
No Connected event, ownership grant, snapshot publication to that peer, or input
delivery occurs until server TLS verification and application policy both pass.
Client Connected requires the authenticated server's welcome. `Authenticated`
events precede Connected on both secure endpoints. Pending sockets count against
the peer cap but are excluded from `peer_ids()` and `assign_owner`.

Run `cargo run -p engine-network --example secure_loopback` for a complete
encrypted input/snapshot exchange. Its identities are generated afresh in memory
using rcgen and are strictly synthetic. Production certificate issuance,
provisioning, trust distribution, key protection and rotation remain host duties.

## Explicit trusted-LAN transport

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

## Threat model, deployment limits and design choice

The TLS path protects gameplay confidentiality/integrity against passive capture
and active network tampering, authenticates the server name and client key, and
rejects clients outside native application policy. TLS record replay fails
authentication; application replay still faces the shared increasing-tick check.
Authenticated malicious participants remain untrusted for ownership, directions,
frame lengths, movement and snapshot validation. An authorized participant sees
all published entities: this is not interest management or protection from a
compromised authorized endpoint. Never put secrets in snapshots.

The original `bind`, `connect` and `from_stream` APIs remain explicit plaintext
trusted-LAN operations. They provide no transport encryption/authentication and
must not be used on untrusted networks. Status differentiates
`trusted_lan_plaintext` from `mutual_tls13`; there is no mode negotiation/downgrade.

TLS bounds add to the shared frame/queue limits: by default a 5-second absolute
handshake deadline, 128 KiB of incoming handshake bytes, eight read/process and
eight write steps per peer per poll, 2 MiB of received TLS bytes per second and
256 complete messages per second. `SecurityLimits` configures these trusted
deployment choices. Fixed one-second windows allow two adjacent bursts at the
boundary. Exceeding a cap closes the peer, including a valid but overactive
participant. Budgets count ciphertext socket bytes separately from bounded
plaintext framing work; each direction stays within `PollBudget.max_bytes`.
TLS uses a 64 KiB limit on each outgoing plaintext/ciphertext buffer per peer,
plus rustls's bounded incoming record and handshake buffers. TLS cryptographic
steps have bounded input/iteration counts, not a hard wall-clock deadline.
Even with zero I/O budgets, handshake and idle deadlines expire. Valid complete
messages refresh idle time, never partial byte trickles. Large frames drain over
multiple polls; queue backpressure is explicit.

These controls bound retained connections and work per call, not global Internet
DoS. An attacker can occupy pending slots, reconnect to reset per-connection rate
windows, consume TLS cryptographic CPU and saturate OS/network resources. Use an
appropriate firewall, connection admission/rate controls and monitored host
deployment for public exposure. Native policy execution is trusted application
code and is outside the transport work budget. Its failures must fail closed;
do not perform blocking network/disk lookups inside it. Certificate expiry and
trust checks happen at handshake, not continuously. The host must track
revocation/rotation and disconnect admitted peers whose authorization changes;
there is no online revocation fetch or automatic account service.
Native runtime callbacks can call `disconnect_peer(world, peer_id)` to close a
server peer and remove its grants, or use `Server::disconnect` before attachment.

No matchmaking, NAT traversal, reconnect/session recovery, rollback/prediction,
delta compression, transport congestion tuning or anti-cheat assurance is
implemented. Internet use requires a reachable TCP listener or host-supplied
connection. TCP head-of-line latency remains. The native target scope is Windows,
Linux and macOS; browser/mobile/console transports are outside this implementation.

TLS on the existing framed TCP path preserves authority and runtime contracts
while using a maintained cryptographic implementation. QUIC/UDP would also require
new reliability, scheduling and congestion contracts; a bespoke crypto wrapper
would create avoidable verification risk. The pure snapshot model and owned
stream entry point keep simulation and connection setup independent. Dependency
API references: [rustls Connection](https://docs.rs/rustls/0.23.46/rustls/enum.Connection.html)
and [WebPkiClientVerifier](https://docs.rs/rustls/0.23.46/rustls/server/struct.WebPkiClientVerifier.html).

## Verification

`cargo test -p engine-network` covers real ephemeral loopback sockets with two
clients, server-assigned identities, allowed/foreign input, replay/future tick
rejection, transform injection, full-set spawn/despawn, interpolation, peer/queue
caps, zero budgets, idle teardown, oversized frames, and injected byte-at-a-time
framing/partial writes. Tests own/drop all sockets and use no external service.
`cargo clippy -p engine-network --all-targets -- -D warnings` and
`cargo fmt -p engine-network --check` cover the crate's static gates. Workspace
integration and platform-specific network deployment remain separate checks.
The secure suite adds real mutual TLS sockets, wrong roots/name/client identity,
missing certificates, denied native policy, plaintext downgrade attempts,
encrypted record capture/replay, raw authenticated input replay/ownership and
transform injection, oversized frames, pending caps, zero/tiny budgets, handshake
deadlines/byte caps, rate rejection and disconnect cleanup. All certificates and
private keys are synthetic and generated in test memory.

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
Reports also include the nonsecret `security` mode and `pending_peers` count.
Only admitted peers appear in reports. Secure attachment emits
`network.authenticated` with peer_id through the same bounded callback/event
path used by headless/MCP runtime execution. It includes no certificate or key.

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
The runtime suite also runs TLS authorization, status/authentication events and
authoritative input callbacks followed by remote pose replication and disconnect.
