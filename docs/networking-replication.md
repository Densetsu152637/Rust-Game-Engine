# Scalable replication (`engine-net-replication`)

`engine-net-replication` is the scalable state-replication layer for engine sessions. It is
**sans-IO**: it turns authoritative entity state into bounded byte packets per peer and applies
packets to a client mirror, but it never owns a socket. Packets are addressed by
`(Lane, Delivery)` exactly as the [`MessageEndpoint`](networking.md) contract expects, so the same
code runs over the TCP and QUIC endpoints. The older whole-snapshot path
(`Server::publish_snapshot`) stays available for small sessions; this crate is the path for many
entities, many peers and constrained bandwidth.

What it provides:

- compact server-allocated entity IDs (`NetId`) with generations;
- a shared `ReplicationRegistry` of components, typed and quantized properties, delivery class,
  priority, visibility condition and update-rate caps, plus RPC rules;
- delta compression against **acknowledged** per-peer baselines that stays correct when
  unreliable packets are lost, duplicated or reordered and when lanes are independent;
- interest management (spatial grid, always-relevant, owner-only, native callback);
- per-peer priority accumulators and hard per-tick byte budgets;
- validated RPCs with ownership checks and rate limits;
- a versioned wire format with hand-written bounded decoding, and nonsecret telemetry.

Out of scope: transport and sockets, session handshake, client-side prediction/reconciliation,
voice, and the mapping of authored `engine.NetworkReplica` entities (the runtime adapter maps string
IDs to `NetId` and authored components to the registry; see
[delta mode](networking.md#delta-mode-endpoints-and-components)).

## Quick start

```rust
use engine_net_replication::*;
use std::sync::Arc;

// 1. Describe the schema. Server and client build it identically.
let registry = Arc::new(
    ReplicationRegistry::builder()
        .component(
            ComponentDef::new("Transform")
                .delivery(StateDelivery::Unreliable)
                .priority(1.0)
                .property("position", ValueSpec::vec3()
                    .quantized(Quantization::Range { min: -1024.0, max: 1024.0, bits: 20 }))
                .property("rotation", ValueSpec::quat()
                    .quantized(Quantization::SmallestThree { bits: 10 })),
        )
        .component(
            ComponentDef::new("Health")
                .delivery(StateDelivery::Reliable)
                .property("hp", ValueSpec::u16()),
        )
        .rpc(RpcDef::new("Fire", RpcDirection::ClientToServer)
            .requires_ownership().max_calls_per_sec(5).arg("weapon", ValueSpec::u8()))
        .build()?,
);
let transform = registry.component_id("Transform").unwrap();

// 2. Server: spawn, mutate, build packets per peer per tick.
let mut server = ReplicationServer::new(registry.clone(), ReplicationLimits::default(),
                                         ServerConfig::default())?;
server.add_peer(peer)?;
server.set_peer_view(peer, [0.0; 3], 100.0)?;
let id = server.spawn(EntityInit::new().at([1.0, 0.0, 0.0]).owner(peer)
    .component(transform, vec![Value::Vec3([1.0, 0.0, 0.0]), Value::Quat([0.0, 0.0, 0.0, 1.0])]))?;
for packet in server.build(peer, tick, budget_bytes, max_payload)? {
    endpoint.send(peer, packet.lane, packet.delivery, &packet.bytes)?; // host owns the socket
}

// 3. Host feeds inbound payloads (Lane::REPLICATION acks, Lane::RPC calls) to the server.
server.receive(peer, &payload)?;

// 4. Client: feed server payloads, send acknowledgements, read the mirror and events.
client.receive(&payload)?;
if let Some(ack) = client.build_ack() { endpoint.send(SERVER_PEER, ack.lane, ack.delivery, &ack.bytes)?; }
for event in client.drain_events() { /* Spawned, PropertyChanged, ... */ }
```

The `testing` module contains the deterministic lossy-link harness used by this crate's own tests
and is a ready-made way to exercise a game's schema offline.

## Data model

### Entities and `NetId`

`NetId { index: u32, generation: u16 }` is allocated by the server. Fresh indices are used until
the index space (`max_entities`) is exhausted or at least 64 indices are waiting in the free list;
reuse is first-in-first-out. The generation changes on **every** reuse. Every packet and RPC names
entities by `(index, generation)`, so a delayed packet for a dead entity can never address a new
one: the client ignores it because the slot has a newer generation. `NetId::to_raw` / `from_raw`
pack the ID into 48 bits so a host can keep `string id -> NetId` maps.

Per entity the server stores: owner (`Option<PeerId>`), relevance mode, optional position, a host
`tag`, and the component list. The component set is fixed at spawn (spawn a new entity to change
it). Each component holds one value per registered property.

### Registry

`ReplicationRegistry::builder()` collects `ComponentDef`s and `RpcDef`s, validates them (unique
names of 1..=64 characters from `[A-Za-z0-9_.:/-]`, at most 32 properties per component, 1024
components and RPCs, 16 RPC arguments, valid quantization) and freezes them. Component, property
and RPC IDs are registration indices, so **both ends must register in the same order**.

Component rules:

| Rule | Meaning |
| --- | --- |
| `delivery` | `Reliable`: updates travel in `ReliableOrdered` packets. `Unreliable`: in `Unreliable` packets; loss is repaired by resending against the acknowledged baseline. |
| `priority` | Weight (> 0) added to the peer's per-entity accumulator each tick while changes are pending. |
| `condition` | `All`; `OwnerOnly` (never encoded for non-owners); `SkipOwner` (never encoded for the owner); `InitialOnly` (sent in the spawn state only). |
| `max_update_hz` | Cap on how often one property is sent to one peer (`ceil(tick_rate / hz)` ticks between sends). |

Property types (`ValueKind`): `Bool`, `U8`, `U16`, `U32`, `U64`, `I32`, `I64`, `F32`, `Vec2`,
`Vec3`, `Quat`, bounded `String` and `Bytes` (`max_len` at most 4096). Integers use LEB128
varints (zigzag for signed), so small values are small.

Quantization (`Quantization`) applies to float kinds and is the unit of "changed":

- `Range { min, max, bits }` (1..=24): fixed point over `[min, max]`, `ceil(bits / 8)` bytes per
  component; out-of-range values clamp.
- `Step { step }`: signed multiples of `step` as zigzag varints, magnitude at most 2^22 steps.
- `SmallestThree { bits }` (8..=16) for `Quat`: drops the largest component and stores the other
  three plus a 2-bit index; `Quat` with `Quantization::None` uses 16 bits. Decoded quaternions are
  always normalised.
- `None`: raw `f32` bits (`NaN` and infinities are rejected on both ends).

The server stores each property **in its encoded form**, so a "change" means "the quantized bytes
differ". A move smaller than the quantization step costs nothing, `get_property` returns exactly
the value clients converge to, and the client decode of the stored bytes is bit-identical to the
server's view.

### Schema hash

`registry.schema_hash()` is a 64-bit FNV-1a hash of the protocol version and every wire-relevant
rule (names, order, kinds, quantization, delivery, condition, priority, rates, RPC rules). Compare
it in your session handshake (for example next to the build version) and refuse mismatching
clients. As defence in depth every replication packet carries the 32-bit fold
(`registry.fingerprint()`); a mismatching packet is rejected with
`ReplicationError::SchemaMismatch` before any other parsing.

## Server

```text
ReplicationServer::new(registry, ReplicationLimits, ServerConfig)
spawn(EntityInit) -> NetId          despawn(NetId)
set_property(id, component, property, &Value) -> bool changed
get_property(id, component, property) -> Value
set_position(id, [f32; 3])          set_owner(id, Option<PeerId>)       set_tag(id, u32)
add_peer(peer)  remove_peer(peer)   set_peer_view(peer, center, radius)
set_relevance_callback(Option<RelevanceFn>)
build(peer, tick, budget_bytes, max_payload) -> Vec<OutPacket>
receive(peer, bytes)                mark_unsent(peer, sequence)
call_rpc(rpc, RpcTarget, Option<NetId>, &[Value]) -> Vec<(PeerId, OutPacket)>
drain_events() -> Vec<ServerEvent>  telemetry() / peer_telemetry(peer)
is_live_for(peer, id)  is_tracked_for(peer, id)
```

`OutPacket { lane, delivery, bytes, sequence }` is always on `Lane::REPLICATION` (state) or
`Lane::RPC`. Hand it to `MessageEndpoint::send`; if the endpoint refuses a state packet (for
example `Error::QueueFull`) call `mark_unsent(peer, packet.sequence.unwrap())` and its content is
resent on the next `build`. Pass the smallest payload limit of the transport as `max_payload`
(about 1100 bytes for QUIC datagrams; `MIN_PAYLOAD` is 64).

`build` does all work for one peer: loss detection, relevancy, record synchronisation, candidate
selection, packing. It is deterministic: all iteration is over ordered maps and the only
randomness in this crate is in the `testing` module.

## Delta compression and the acknowledgement algorithm

Goal: send each peer only what it does not already have, never rely on packet order, and still
converge when packets are lost.

### Per-peer baseline

For every `(peer, entity)` the server keeps a **record** with a phase and one **slot** per
property:

```text
Unsent --spawn sent--> Spawning --spawn acked--> Live
Live/Spawning --irrelevant, destroyed or ownership resync--> Despawn --sent--> Despawning --acked--> (record removed)
Spawning/Despawning --packet lost--> Unsent/Despawn (resend)
slot = { acked_version, inflight_version, inflight_sequence, last_sent_tick }
```

Every property has a version on the server (1, then +1 per change). A slot's `acked_version` is
the last version the peer **confirmed**, which is the baseline. A property is *dirty* for a peer
when its current version differs from both `acked_version` and `inflight_version` (the version
already sent and awaiting acknowledgement). Only dirty properties are encoded, as absolute values
(not arithmetic differences), grouped into per-component masks.

### Packets, sequences, acknowledgements

Each state packet carries a per-peer 32-bit sequence number and the server tick. The client
remembers the newest sequence received plus a 64-bit window and acknowledges with
`largest` + bitfield (`bit i` = `largest - 1 - i` received). Acks are idempotent and unreliable;
a lost ack is covered by the next one.

When an ack arrives, every acknowledged packet "commits": the exact property versions listed in
that packet (the server remembers the list per in-flight packet) become the new baseline, a spawn
makes the record `Live`, a despawn deletes the record. The packet list is bounded by
`max_in_flight_packets`.

Loss detection releases a packet's items so the next `build` resends **current** state against
the baseline:

- an unreliable-class packet is lost when a packet at least `reorder_grace` (3) sequence numbers
  newer was acknowledged without it, or after `ack_timeout_ticks` (30);
- a reliable-class packet is resent only after `reliable_timeout_ticks` (600), because the
  transport retransmits it;
- more than `max_in_flight_packets` outstanding packets, or `mark_unsent`, also release items.

Resending sends the *latest* value, never the lost one, so loss cannot cause a stale overwrite.

### Ordering safety on the client

The client assumes nothing about arrival order:

- every property remembers the sequence of the packet that last wrote it and ignores writes from
  older packets (wrapping 32-bit comparison);
- every entity slot remembers the sequence of its newest spawn/despawn, so a spawn that arrives
  after the despawn that superseded it cannot resurrect the entity, and a duplicate older spawn is
  ignored;
- generation checks make packets for a previous incarnation of an index inert;
- duplicated packets are recognised through the receive window and applied once; older-than-window
  packets are still safe to apply because of the two rules above;
- the server never sends deltas for an entity before its spawn is acknowledged, so a delta that
  does arrive early (a forged or duplicated packet) only finds an unknown entity and is counted as
  a stale block;
- an entire packet is decoded and validated before any of it is applied, so a bad packet changes
  nothing and is not acknowledged.

Entity spawns carry the full initial state of every component the peer may see; despawns are
retransmitted until acknowledged. Re-sending a spawn to a client that already has the entity
refreshes its state without emitting `Spawned` again.

### Delivery classes

State packets are split by component delivery class. Spawns, despawns and owner updates are always
reliable-class. A tick therefore yields at most two open packets per peer (one reliable, one
unreliable), each filled up to `max_payload`, and more when needed.

## Relevancy (interest management)

Every `build` computes the set of entities relevant to the peer; **nothing outside that set is
ever encoded for it**. Built-in rules (`EntityRelevance`, inferred from the spawn position if not
given):

- `Always`: relevant to every peer.
- `Spatial`: relevant when within the peer's view sphere (`set_peer_view(peer, center, radius)`);
  a peer without a view sees no spatial entities. A uniform hash grid (`grid_cell_size`) finds
  candidates without scanning every entity. Hysteresis (`relevance_hysteresis`, default 10%) keeps
  an already-visible entity until it exceeds `radius * 1.1`, preventing flicker on the boundary.
- `OwnerOnly`: relevant only to its owner.

A native callback `Fn(PeerId, &EntityView) -> Relevance` may override per `(peer, entity)`:
`Relevance::Default` (use the built-in rule), `Hidden`, or `Visible(scale)` (forces relevance and
sets the priority scale). `EntityView` exposes id, owner, position, `tag`, the peer's view centre,
the distance and `has_component`. While a callback is installed it is evaluated for every entity
and peer each build, so keep it cheap.

Leaving relevance (distance, callback, destruction) makes the server send a per-peer despawn and
drop that peer's baseline once the despawn is acknowledged. Re-entering sends the **full current
state** as a new spawn; changes made while hidden cost nothing.

Component visibility (`OwnerOnly`, `SkipOwner`) is evaluated when the spawn block is encoded; the
excluded components' bytes are simply never produced for that peer. Changing an entity's owner
affects only the old and new owner: if the entity has an ownership-dependent component they are
resynced with a despawn followed by a fresh spawn (the client sees `Despawned` then `Spawned`);
otherwise they receive a small owner-flag update (`OwnerChanged`). The client only learns whether
**it** owns the entity, never other peers' IDs.

## Priority and bandwidth budgets

`build(peer, tick, budget_bytes, max_payload)` treats both numbers as hard limits: the summed
length of the returned packets never exceeds `budget_bytes` and no packet exceeds `max_payload`.
`BandwidthBudget::new(bytes_per_sec, tick_rate).next_tick()` converts a bytes/second budget into
per-tick budgets without losing fractional bytes.

Each `(peer, entity, class)` with pending work has an accumulator that grows every tick by
`component priority x relevance scale` (spatial scale falls from 1.0 at the peer to
`min_relevance_scale` at the view radius; owned entities are multiplied by
`owner_priority_scale`; spawns grow by `spawn_priority`). Candidates are sorted by accumulator
(largest first, ties by `NetId`), despawn and owner updates first. A candidate that fits is sent
and its accumulator resets; one that does not fit keeps accumulating, so every pending item is
eventually the largest. Smaller blocks may fill the remaining budget behind a block that does not
fit. A single block must fit `max_payload` and, to ever be sent, the budget of some tick:
`max_entity_bytes` (default 900) bounds a spawn block, so pass `max_payload` above that plus about
32 bytes. Blocks that can never fit are counted in `oversize_blocks` (a configuration error)
instead of being sent.

## RPCs

`RpcDef` fields: name, direction (`ServerToClient` / `ClientToServer`), delivery (`Reliable` ->
`ReliableOrdered`, `Unreliable` -> `Unreliable`; always `Lane::RPC`), `targets_entity`,
`requires_ownership` (client calls; implies an entity target), typed arguments (the same
`ValueSpec`s as properties) and `max_calls_per_sec` (client calls).

- **Server to client:** `server.call_rpc(rpc, RpcTarget::{Peer(p), Owner, Relevant}, entity, &args)`
  returns one `(peer, OutPacket)` per recipient. Entity-targeted calls only go to peers that
  currently have the entity (`is_live_for`), so naming an unknown entity to a peer is an error
  rather than a leak; `Relevant` multicasts to exactly those peers. The client validates direction,
  argument types and entity, and emits `ClientEvent::RpcReceived`; failures only increment
  `rpc_dropped`.
- **Client to server:** `client.call_server(rpc, entity, &args)` builds the packet.
  `server.receive` validates each call independently: registered ID, direction, rate (a token
  bucket of `max_calls_per_sec` tokens, refilled at that rate per second of server ticks; **every**
  attempt consumes a token, so malformed spam is throttled too: calls with an unknown ID or the
  wrong direction draw from one per-peer invalid-call bucket of `max_rpcs_per_packet` tokens per
  second and become `RateLimited` once it is empty), target presence, that the entity
  exists **and is live for the caller** (an entity the caller cannot see is indistinguishable from
  one that does not exist), ownership, then argument decoding (arity, types, lengths, finiteness,
  no trailing bytes). Success yields `ServerEvent::RpcReceived`; failure yields
  `ServerEvent::RpcRejected { reason, .. }` with a reason from `RpcRejectReason` and `receive`
  still returns `Ok`: soft failures never disconnect. Only a packet whose *framing* is invalid is
  an `Err`, apart from backpressure below.
- **Server event queue, per peer:** each peer owns a reserved share of
  `peer_event_capacity() = max(max_pending_events / max_peers, max_rpcs_per_packet)` events
  between `drain_events` calls (64 with defaults), so one peer cannot displace another peer's
  `RpcReceived`; the queue as a whole holds at most `max_peers` shares. At most a quarter of a
  share holds `RpcRejected` events; later rejections are counted in `rpc_rejected` and
  `rpc_rejections_coalesced` but not queued. A reliable `RpcReceived` is never dropped: an RPC
  packet whose calls might not fit the sender's remaining share is refused whole with
  `ReplicationError::Backpressure(peer)` and counted in `rpc_packets_refused`, nothing in it
  consumed. Drain events and offer the packet again, or stop reading that peer. `remove_peer`
  discards the peer's undrained events. `ServerTelemetry::events_dropped` stays 0.

## Wire format (version 1)

Little-endian; varints are LEB128 (at most 10 bytes, no overflow).

```text
common : u8 version (=1) | u8 kind (1 state, 2 ack, 3 rpc) | u32 schema fingerprint
state  : common | u32 sequence | u32 tick | varint blocks | block*
ack    : common | u32 largest | u64 bits
rpc    : common | varint calls | (varint rpc | u8 flags(bit0 entity) | [NetId] | varint len | args)*
NetId  : varint index | u16 generation
block  : u8 op | NetId | body
  1 spawn  : u8 owned(0/1) | varint components | (varint component | every property value)*
  2 update : varint components | (varint component | varint property mask | present values)*
  3 despawn: -
  4 owner  : u8 owned(0/1)
```

Components in a block are strictly ascending by ID. Values use the property's `ValueSpec`
(bool 1 byte; u8/u16 fixed; u32/u64/i32/i64 varint; floats raw or quantized; quaternion
smallest-three; strings and bytes are `varint length | data`).

Decoding is hand-written and bounded. Before allocating, every count is checked against
`ReplicationLimits` **and** the bytes that remain (a block needs at least 4 bytes): block counts,
component counts (no more than the registry has), RPC counts, RPC payload length, string/bytes
length (against `max_len` and the remaining bytes), entity index (below `max_entities`). The
decoder rejects: unknown version, kind, op, component or RPC ID; wrong schema fingerprint;
non-ascending or duplicate components; empty or out-of-range property masks; bool and flag values
other than 0/1; invalid UTF-8; non-finite floats; quantized values above their declared range;
quaternion padding bits; overlong varints; and trailing bytes. Quaternions are normalised on
decode. Nothing panics on arbitrary input: the test suite feeds 20 000 seeded random/mutated
packets each to the client and the server, truncates a valid packet at every length, and covers
each malformed class explicitly.

## Limits (`ReplicationLimits`)

| Field | Default | Purpose |
| --- | --- | --- |
| `max_entities` | 16384 | live entities (server and client mirror); bounds `NetId` indices. Use the same value on both ends. |
| `max_entities_per_peer` | 16384 | records tracked per peer |
| `max_peers` | 64 | peers on a server |
| `max_entity_bytes` | 900 | encoded spawn state of one entity |
| `max_props_per_entity` | 256 | properties across an entity's components |
| `max_blocks_per_packet` | 2048 | blocks accepted/produced in one packet |
| `max_packet_bytes` | 65536 | largest packet `receive` accepts |
| `max_rpcs_per_packet` | 16 | calls per RPC packet |
| `max_rpc_payload_bytes` | 1024 | encoded arguments of one call |
| `max_in_flight_packets` | 256 | unacknowledged packets tracked per peer |
| `max_pending_events` | 4096 | buffered events; the server splits it into per-peer shares (backpressure when full), the client drops newer ones and counts them |
| `max_view_radius` / `max_abs_position` | 1e6 / 1e7 | accepted view radius and coordinates |

`ServerConfig` tunes `tick_rate_hz`, `grid_cell_size`, `relevance_hysteresis`,
`ack_timeout_ticks`, `reliable_timeout_ticks`, `reorder_grace`, `spawn_priority`,
`owner_priority_scale` and `min_relevance_scale`. Limits and config are trusted native
configuration and are validated on construction.

## Client

```text
ReplicationClient::new(registry, ReplicationLimits)
receive(bytes)                          // state and RPC packets from the server
build_ack() -> Option<OutPacket>        // send on Lane::REPLICATION; Delivery::Unreliable
call_server(rpc, Option<NetId>, &[Value]) -> OutPacket
entity(NetId) / entities() / entity_count() / server_tick()
drain_events() -> Vec<ClientEvent>
on_property_changed(component, property, FnMut(&PropertyChange))
telemetry()
```

`ClientEvent`: `Spawned { entity, owned }`, `Despawned { entity }`,
`PropertyChanged(PropertyChange { entity, component, property, old, new })`,
`OwnerChanged { entity, owned }`, `RpcReceived { rpc, entity, args }`. Native callbacks registered
per `(component, property)` run for the same changes, in addition to the event. Initial spawn state
raises `Spawned` only (read it through `entity`); values written equal to the current value raise
nothing. Events are bounded by `max_pending_events`.

## Telemetry

`server.telemetry()` / `peer_telemetry(peer)` return serde-serializable, nonsecret counters (no
property values, no credentials): per-peer bytes and packets for the last tick and in total,
`entities_relevant / sent / deferred`, `full_state_bytes_total` (spawn blocks),
`delta_bytes_total` (update blocks), `snapshot_equivalent_bytes_total` and the derived
`compression_ratio` (an estimate of what resending every relevant entity in full each tick would
have cost, divided by bytes actually sent), `unacked_packets`, `ack_lag_packets`,
`oldest_unacked_age_ticks`, `packets_acked / lost`, `oversize_blocks`, and RPC counts
(`rpc_received`, `rpc_rejected` with a per-reason breakdown, `rpc_rejections_coalesced`,
`rpc_packets_refused`, `rpc_sent`). `client.telemetry()`
counts packets, duplicates, rejections, stale blocks, RPCs and dropped events. Use
`ack_lag_packets` or `oldest_unacked_age_ticks` to detect a stalled peer.

## Measured behaviour

Numbers come from this crate's tests (debug build, deterministic seeds):

- **Compression:** 200 entities, 10% moving per tick (transform: 20-bit range position +
  10-bit smallest-three rotation) for 300 ticks: 401 bytes per tick on the wire (headers
  included) against about 7562 bytes per tick for resending every relevant entity in full: a
  **compression ratio of 18.9**.
- **Convergence:** with 20% loss, 10% duplication and up to 4 ticks of reordering jitter on every
  lane in both directions, 3 clients, 60 entities of churn (moves, despawn/respawn, owner
  changes): across six seeds the mirrors equalled the
  server (quantized values, owner-conditioned components included) between **7 and 39 ticks** after
  changes stopped (the test asserts at most 60), with roughly 200 dropped packets per run.
- **Budget:** 500 entities all changing every tick under a 1500 byte/tick budget and 1100 byte
  packets: every tick stayed within the
  budget (about 1496 bytes used on average), roughly 394 entities were deferred per tick, and the
  **maximum staleness was 5 ticks**; in a separate priority test the priority-4 component received
  837 updates against 212 for the priority-1 component while every low-priority entity still
  updated at least three times in 90 ticks.

## Threat model and residual risks

Every packet byte is untrusted: the server must assume authenticated clients are malicious, and a
compromised server can attack clients. Defences: bounded hand-written decoding (see above),
all-or-nothing packet application, schema fingerprint, generation-checked IDs, bounded per-peer and
per-packet memory, server-only authority (clients can only send acks and RPCs; state packets from
a client are rejected), RPC validation with per-peer rate limits, and relevancy that is enforced at
encoding time so non-qualifying bytes are never produced (tested by scanning every packet sent to
other peers for owner-only and out-of-radius content).

Residual risks and limits:

- **Confidentiality is the transport's job.** This layer does not encrypt or authenticate; run it
  over the authenticated TLS/QUIC endpoints. Without that, packets and acks can be read or forged.
- **Ack forgery.** A peer can acknowledge packets it did not receive. That only starves *itself* of
  retransmission (its mirror diverges); it cannot affect other peers. An ack for a sequence the
  server never sent is rejected.
- **Information that relevancy cannot hide.** Entity existence, `NetId` allocation order and the
  timing/size of packets are observable by the peer that receives them; the sizes of other peers'
  packets are not. An attacker with a legitimate view sphere sees everything inside it.
- **Spawn/despawn churn.** A peer cannot cause churn on its own except by moving its own view; a
  game that lets clients move views freely should cap the rate in its own code.
- **Ownership resync flicker.** Ownership changes on entities with ownership-dependent components
  despawn and respawn them for the affected peers (one round trip of absence).
- **Fixed component sets.** Components cannot be added or removed after spawn.
- **Fixed-window assumptions.** Reorder tolerance assumes packets are not delayed past the
  64-packet ack window; later arrivals are still applied safely but are not acknowledged and the
  content is resent.
- **Priority vs. size.** A tight budget lets small high-priority blocks overtake larger lower
  priority ones; a block larger than every tick's budget is never sent.
- **Float determinism.** Server and client share only integer wire values and integer-to-float
  decoding with explicit IEEE operations; the server encodes with `f64`. Bit-identical results
  across platforms rely on IEEE-754 conformance (no fused multiply-add is used).
- **Telemetry** contains counters only; it is safe for logs and MCP output.

## Tests

`cargo test -p engine-net-replication` runs the unit tests (codec, registry, grid, limits) and the
integration suites: `sync` (convergence, compression, ordering, `NetId` reuse, callbacks, events,
delivery classes, rate caps), `relevancy` (radius, hysteresis, re-entry, information leaks,
callbacks), `budget` (hard limits, freshness, priority, starvation), `rpc` (every rejection
reason, rate limits, multicast, a peer flooding 4296 invalid calls in one tick without evicting
another peer's valid call, and explicit backpressure once a peer's event share is full) and
`robustness` (fuzzing and malformed packets). The shared
helpers live in `engine_net_replication::testing` (seeded `Rng`, `LossyLink`, `Simulation`).
