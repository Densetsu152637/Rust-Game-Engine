# Network transports and simulation

The engine moves gameplay bytes through one transport-neutral contract,
`engine_network::MessageEndpoint` (`crates/engine-network/src/channel.rs`).
Replication, RPC, prediction, voice and session layers produce and consume
opaque payloads addressed by `(peer, lane, delivery)` and never own sockets.
Two transports implement it:

| Endpoint | Crate | `transport()` | `security()` |
| --- | --- | --- | --- |
| `Server` / `Client` (`bind`, `connect`, `from_stream`) | `engine-network` | `Tcp` | `TrustedLanPlaintext` |
| `Server::bind_secure` / `Client::connect_secure` | `engine-network` | `Tcp` | `MutualTls13` |
| `QuicServer` / `QuicClient` | `engine-net-quic` | `Quic` | `MutualTls13` |

[Authenticated authoritative multiplayer](networking.md) documents the TCP
replication protocol, its runtime integration and the original security review.
This page covers the endpoint contract, the QUIC transport, caller-supplied
rustls configurations and network simulation.

## Endpoint contract

- No method blocks, sleeps or spawns threads. `poll(PollBudget)` performs bounded
  socket, timer and handshake work and returns `LinkEvent`s.
- A server's peers are server-assigned nonzero IDs; a client's only peer is
  `SERVER_PEER` (0). IDs are not credentials.
- `Authenticated(peer)` (secure endpoints only) always precedes `Connected(peer)`.
  No payload is delivered to or accepted from a peer before `Connected`.
- `send` validates before queueing: a lane above `Lane::MAX` (15), an oversized
  payload or an unknown/unadmitted peer returns `Error::Invalid`; a full reliable
  queue returns `Error::QueueFull` and queues nothing. Unreliable payloads that
  meet backpressure are discarded and counted in `LinkStats::dropped_unreliable`;
  reliable payloads are never silently dropped.
- `max_payload(peer, delivery)` reports the largest payload `send` accepts now.
- `Disconnected` is reported for admitted server peers only (failed handshakes are
  silent on the server). A client also reports `Disconnected` when the connection
  fails before `Connected`, so callers learn why a connect attempt ended.
  Local `disconnect` emits no event.
- Reasons are fixed nonsecret strings; peer-supplied close text is never surfaced.
  `LinkStats` contains only counters and timings and is safe for logs, reports and
  MCP output.
- Payload bytes are untrusted input for every consumer.

### Lane and delivery mapping

Lanes 1..=5 are engine lanes (`REPLICATION`, `RPC`, `INPUT`, `VOICE`, `SESSION`);
lanes 8..=15 belong to applications.

| Delivery | TCP | QUIC |
| --- | --- | --- |
| `ReliableOrdered` | single ordered stream for all lanes | one long-lived unidirectional stream per lane; `u32` big-endian length-prefixed messages |
| `ReliableUnordered` | delivered in order anyway | one unidirectional stream per message, ended by FIN; concurrency bounded by `max_concurrent_streams` |
| `UnreliableSequenced` | delivered reliably and in order (never stale) | DATAGRAM frame `[0x10 \| lane, seq_hi, seq_lo] + payload`; stale or duplicate sequence numbers (RFC 1982 serial arithmetic on `u16`) are dropped on receive and counted |
| `Unreliable` | delivered reliably and in order | DATAGRAM frame `[lane] + payload`; may be lost or reordered |

TCP serializes every lane on one stream, so a large or retransmitted payload
delays all lanes (head-of-line blocking). QUIC lanes are independent: loss on one
lane's stream never stalls another lane or any datagram.

`max_payload` is `Limits::max_frame_bytes - 3` (default 262 141 bytes) for every
TCP delivery. QUIC reliable deliveries accept `QuicLimits::max_message_bytes`
(default 256 KiB); unreliable deliveries accept the current QUIC datagram limit
minus the 1- or 3-byte header. That limit follows the path MTU (at least about
1 KiB; quinn-proto's PLPMTUD may raise it) and larger unreliable payloads are
rejected with `Error::Invalid` rather than fragmented.

## TCP implementation

`Server` and `Client` implement `MessageEndpoint` for both plaintext trusted-LAN
and mutual-TLS operation. Payloads use binary data frames next to the existing
JSON control messages: the usual four-byte big-endian body length, then
`[0x00, lane, delivery, payload...]` (delivery codes 0 = reliable ordered,
1 = reliable unordered, 2 = unreliable sequenced, 3 = unreliable). JSON bodies
always begin with `{`, so the tag cannot collide; a binary frame has a fixed
three-byte header and no base64/array expansion, whereas a JSON byte array would
cost up to four bytes per payload byte. Unknown tags, delivery codes and lanes
are protocol errors that close the connection, as are data frames from a client
before the welcome.

**Wire version 2.** The server welcome now announces `version: 2`. Endpoints
accept only their own version: a version-1 client rejects the version-2 welcome
and a version-1 peer receiving a data frame closes on the malformed frame, so
mixed versions fail closed. Existing JSON message shapes and behaviour are
unchanged.

**Two event APIs, latched on first poll.** An endpoint is driven through either
`Server::poll`/`Client::poll` (legacy replication events, used by
`engine_project::networking`) or `MessageEndpoint::poll -> LinkEvent`. The first
poll call latches that API; calling the other poll afterwards returns
`Error::Invalid` and drains nothing. A message the latched API cannot represent
is a protocol error, never retained:

| Endpoint | Latched API | Unrepresentable message | Result |
|---|---|---|---|
| Server | legacy | data frame | peer disconnected, "unexpected client message" |
| Server | lanes | `Input` command | peer disconnected, "unexpected client message" |
| Client | legacy | data frame | disconnected, "unexpected server message or version" |
| Client | lanes | snapshot | applied to `Client::state()`; no notification |

`Input` under the lane API disconnects rather than drop-and-count because a
lane-driven server has no input path, so an `Input` sender is a mismatched or
hostile peer. Because nothing is held for another API, every poll reads every
peer, no peer is ever exempt from `idle_timeout`, and `disconnect` leaves no
per-peer items behind (memory stays bounded across reconnects). Sending through
`MessageEndpoint::send`, `send_input` or `publish_snapshot` is independent of the
latch. The inherent `Client::poll`/`Client::disconnect` shadow the trait
methods; call the trait through a generic `E: MessageEndpoint` or
`MessageEndpoint::poll(&mut client, budget)`.

TCP `LinkStats` count framed plaintext bytes and frames; `rtt_ms`,
`lost_packets` and `congestion_window_bytes` are 0 because TCP exposes none.

## QUIC transport (`engine-net-quic`)

```rust
let security = QuicServerSecurity::new(server_chain, server_key, client_roots,
    |peer, chain| allowlist.contains(&chain[0]))?;
let mut server = QuicServer::bind(addr, QuicServerConfig::default(), security)?;

let security = QuicClientSecurity::new("game.example", server_roots, client_chain, client_key)?;
let mut client = QuicClient::connect(server_addr, QuicClientConfig::default(), security)?;
// Both: MessageEndpoint::poll(&mut endpoint, PollBudget::default()) every frame.
```

The crate uses [`quinn-proto`](https://docs.rs/quinn-proto/0.11.19) 0.11.19 (the
sans-IO QUIC state machine, `default-features = false`, feature `rustls-ring`)
with the workspace's rustls 0.23 ring provider. All socket I/O, timers and
handshakes run synchronously inside `poll`; there is no async runtime, thread or
sleep. `QuicClient::connect` binds an ephemeral UDP socket (loopback for a
loopback server, otherwise the unspecified address of the server's family).
QUIC-bit greasing (RFC 9287) is disabled on every endpoint, so engine QUIC packets
always set the `0x40` fixed bit. Other protocols can therefore share a socket
through first-byte demultiplexing (see [NAT traversal and relay](online-nat-relay.md)).

### Admission

1. The server answers an unvalidated client address with a stateless Retry
   (`QuicServerConfig::require_address_validation`, default `true`), so spoofed
   sources cannot make it allocate connection state or amplify traffic beyond
   quinn-proto's 3x anti-amplification limit. Disable it only on trusted networks
   or behind a relay that validates addresses; it costs one round trip.
2. Validated attempts beyond `PollBudget::max_accepts` per poll are ignored (the
   client retransmits); attempts beyond `max_peers` live connections or
   `max_pending_handshakes` are refused with `CONNECTION_REFUSED`.
3. TLS 1.3 runs with ALPN `rust-game-engine-quic/1` and a mandatory client
   certificate verified against the server's client roots. 0-RTT, tickets and
   resumption are disabled, so every connection proves key possession.
4. When the handshake completes the server checks the negotiated ALPN and that a
   nonempty verified chain is present, then calls the native policy exactly once.
   Only then does it emit `Authenticated` and `Connected` and open a control
   stream carrying `[1, QUIC_PROTOCOL_VERSION]`. Denial closes the connection
   with an application code (client reason `unauthorized`).
5. The client verifies the server chain and name in TLS, checks ALPN, reads
   nothing but the control stream, and emits `Authenticated` then `Connected`
   when the welcome arrives. Server streams and datagrams that arrive earlier stay
   unread in flow-controlled QUIC buffers.

Both sides enforce `handshake_timeout` as an absolute deadline from connection
creation to admission or welcome, independently of I/O budgets. Peer stream
headers are validated: unknown kinds, invalid lanes, a second ordered stream for
one lane, a control stream from a client, oversized length prefixes, truncated
ordered streams and malformed DATAGRAM headers close the connection with a
protocol code and deliver nothing further. Bidirectional streams are disabled.

### Limits and defaults (`QuicLimits`)

| Field | Default | Meaning |
| --- | --- | --- |
| `max_peers` | 16 | live connections a server keeps (pending + admitted) |
| `max_pending_handshakes` | 8 | concurrent connections in handshake/authorization |
| `handshake_timeout` | 5 s | absolute deadline to admission (server) or welcome (client) |
| `idle_timeout` | 30 s | QUIC idle timeout (the smaller of both peers' values applies) |
| `keep_alive_interval` | 5 s | must be below `idle_timeout`; `None` disables |
| `max_message_bytes` | 256 KiB | largest reliable payload (at most 16 MiB) |
| `max_queued_bytes` | 1 MiB | per-peer bytes queued by `send` beyond QUIC flow control; also the QUIC send window, connection receive window and stream receive window |
| `max_concurrent_streams` | 32 | concurrent reliable-unordered streams per direction (17 more are reserved: control + one per lane) |
| `max_received_bytes_per_second` | 8 MiB | per-peer received payload bytes, fixed one-second windows |
| `max_messages_per_second` | 4096 | per-peer received messages, fixed one-second windows |
| `max_datagrams_per_poll` | 256 | UDP datagrams read per poll, and packets written per connection per poll |
| `datagram_buffer_bytes` | 64 KiB | DATAGRAM send and receive buffers per connection |

`validate()` runs in every constructor and rejects zero values, a keep-alive not
shorter than the idle timeout, `max_queued_bytes < max_message_bytes + 6`, more
than 1024 unordered streams and out-of-range buffer sizes. Exceeding a receive
rate cap closes the peer (`receive rate exceeded`). Engine peers never reset
streams, so a peer resetting any message stream (ordered or unordered) is a
protocol violation that closes the connection; a truncated reliable message is
never delivered. `PollBudget::max_messages`
and `max_bytes` bound the messages and payload bytes delivered per connection per
poll; undelivered data stays in QUIC buffers and flow control pushes back on the
sender. `QueueFull` is returned when `max_queued_bytes` of unsent reliable data
is pending; datagrams that do not fit the DATAGRAM send buffer are discarded and
counted.

### Congestion control and statistics

`QuicServerConfig::congestion` / `QuicClientConfig::congestion` select a
quinn-proto controller: `Cubic` (default, quinn-proto's own default), `NewReno`
or `Bbr`. Reliable streams and DATAGRAM frames share the congestion window and
pacing. `LinkStats` comes from quinn-proto's path statistics: smoothed RTT, UDP
bytes and datagrams sent and received, packets sent and declared lost, the
current congestion window, plus the endpoint's locally dropped unreliable count
and queued reliable bytes.

### Pluggable sockets (`DatagramSocket`)

`QuicServer::with_socket(socket, config, security)` and
`QuicClient::with_socket(socket, server_addr, config, security)` accept any
`DatagramSocket + 'static`, so NAT-traversal and relay layers can supply
hole-punched or relay-wrapped paths. The trait (`Send + Sync`) is a cross-crate
contract:

```rust
pub trait DatagramSocket: Send + Sync {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize>;
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
}
```

Implementations never block. A failed `send_to` is packet loss (QUIC recovers).
`recv_from` returns `WouldBlock` when empty; `Interrupted`, `ConnectionReset`
(Windows reports ICMP port-unreachable this way) and `ConnectionRefused` are
treated as transient, and any other error is returned from `poll`. `local_addr`
is diagnostic only. `std::net::UdpSocket` implements the trait; it must be in
nonblocking mode (`bind`/`connect` set this, `with_socket` callers must).

## Caller-supplied rustls configurations

The safe constructors (`ServerSecurity::new`, `ClientSecurity::new`,
`QuicServerSecurity::new`, `QuicClientSecurity::new`) remain the default. Hosts
that must choose their own certificate verifier or resolver (for example a
platform store, an HSM-backed key or a custom pinning verifier) can pass a
rustls configuration instead:

```rust
ServerSecurity::from_rustls_config(Arc<rustls::ServerConfig>, authorize) -> Result<ServerSecurity>
ClientSecurity::from_rustls_config(Arc<rustls::ClientConfig>, server_name) -> Result<ClientSecurity>
QuicServerSecurity::from_rustls_config(Arc<rustls::ServerConfig>, authorize) -> Result<QuicServerSecurity>
QuicClientSecurity::from_rustls_config(Arc<rustls::ClientConfig>, server_name) -> Result<QuicClientSecurity>
```

Build configurations with `engine_network::rustls`, the exact rustls version the
engine links, to avoid a version mismatch. `engine_network::tls` exposes the
shared policy (`server_config`, `client_config`, `harden_server`,
`harden_client`, `server_name`, `roots`) and `TCP_ALPN` / `QUIC_ALPN` name the
protocols.

**The caller's verifier, resolver and crypto provider become trusted
configuration.** The engine cannot prove that a supplied verifier verifies
anything; a permissive verifier removes peer authentication. Everything the
endpoint can enforce still holds:

- The configuration is copied, then ALPN is overridden to the transport's single
  protocol (`check_selected_alpn` on clients), 0-RTT/early and half-RTT data are
  disabled, TLS 1.3 tickets are not sent, server session storage and ticketer are
  replaced with non-caching ones, client resumption is disabled, and key logging
  and secret extraction are turned off.
- A client configuration without client certificates is rejected at construction.
- Every TCP connection that does not negotiate TLS 1.3 with the engine ALPN is
  closed before any frame is read (`verify_protocol`); QUIC negotiates only
  TLS 1.3 and its ALPN is rechecked after the handshake.
- A server connection without a presented, nonempty client chain is closed
  (`missing client identity`), and the native policy still runs exactly once per
  connection before `Connected`.
- QUIC configurations must contain `TLS13_AES_128_GCM_SHA256`, QUIC's initial
  suite, or construction fails.

## Network simulation

`NetworkConditions` (`engine-network`) is authored data, serde with
`deny_unknown_fields` and every field optional:

| Field | Range | Effect |
| --- | --- | --- |
| `loss` | 0..=1 | probability a droppable item is discarded |
| `latency_ms` | 0..=10 000 | fixed one-way delay |
| `jitter_ms` | 0..=10 000 | extra uniform delay in `[0, jitter_ms]` |
| `duplicate` | 0..=1 | probability a droppable item is delivered twice |
| `reorder` | 0..=1 | probability an item is held an extra `max(jitter_ms, 10)` ms |
| `bandwidth_bytes_per_second` | `None` or > 0 | serialization rate of the simulated link |
| `seed` | any `u64` | SplitMix64 seed; equal seeds and inputs give equal fates |

`Conditioner` turns conditions into per-item fates and is shared by both
simulators. `validate()` rejects NaN or out-of-range probabilities, delays above
10 s and zero bandwidth.

- `ConditionedEndpoint<E: MessageEndpoint>` wraps any endpoint at message level.
  Every received event is delayed; only unreliable deliveries are lost or
  duplicated. Reliable-ordered payloads keep `(peer, lane)` order, stale or
  duplicate sequenced payloads are discarded on release, and lifecycle events
  never overtake earlier payloads of the same peer. Sends pass through, so wrap
  both endpoints to impair both directions. The held queue is bounded
  (default 4096 events / 8 MiB): at capacity the inner endpoint is polled with
  zero message and byte budgets (timers and lifecycle only) and unreliable
  arrivals are discarded and counted, so memory is bounded by the capacity plus
  one inner poll. Wrapper drops are added to `stats().dropped_unreliable`.
- `ConditionedSocket<S: DatagramSocket>` (`engine-net-quic`) applies loss,
  latency, jitter, duplication, reorder and bandwidth to the datagrams a socket
  sends, below QUIC, so QUIC's own loss recovery and congestion control react as
  on a real path. Delayed datagrams sit in a bounded queue (default 4096
  datagrams / 8 MiB; overflow counts as loss) and are written by later
  `send_to`/`recv_from` calls, which endpoints make on every poll, so timing
  resolution equals the caller's poll interval. `stats()` reports offered,
  lost, overflowed, duplicated and delivered datagrams.

These simulators are development and test tools. They add no security and must
not be mistaken for shaping or protection on production links.

## Threat model and residual risks

QUIC endpoints give the same guarantees as the TLS TCP path: confidentiality and
integrity of all streams and datagrams against passive capture and active
tampering, server name and certificate verification, client key possession, and
native application policy before admission. QUIC packet protection rejects
replayed and modified packets; application-level replay checks (for example
input ticks) remain the job of the layers above. Authenticated participants stay
untrusted: every header, length, lane, sequence number and payload is validated
or treated as opaque untrusted bytes, and violations close the connection.
Garbage UDP, malformed QUIC packets and invalid Initials are dropped by
quinn-proto without allocating connection state; tests confirm they never panic
or admit a peer.

Residual risks and deployment limits:

- The caps bound retained connections, buffers and per-poll work, not Internet
  DoS. An attacker can still consume UDP bandwidth, handshake CPU (Retry makes
  that require a reachable source address), pending slots up to
  `max_pending_handshakes` for `handshake_timeout`, and refusal responses. Use
  firewalls and host rate limiting for public exposure.
- Rate caps count application payloads only; QUIC control traffic (ACK, PING,
  flow-control frames) is bounded by `max_datagrams_per_poll` and quinn-proto's
  own limits, not by the per-second caps. Fixed one-second windows admit two
  bursts at a boundary; reconnecting resets them.
- Worst-case receive memory per admitted peer is roughly
  `(17 + max_concurrent_streams) * max_message_bytes` of partially read messages
  (allocation, not just content: partial bodies grow geometrically but are capped at
  their own message bound, so no buffer exceeds `max_message_bytes + 1` bytes),
  plus the QUIC receive window (`max_queued_bytes`) and `datagram_buffer_bytes`;
  with defaults about 13 MiB, times `max_peers`. Pending connections hold at most
  their flow-control windows. Lower `max_message_bytes` or
  `max_concurrent_streams` for constrained hosts. Send-side memory is bounded by
  `max_queued_bytes` twice over (engine queue plus QUIC send window).
- QUIC's DATAGRAM receive buffer discards the oldest datagrams when the
  application polls too slowly; those drops happen inside quinn-proto and are not
  counted in `dropped_unreliable`.
- Connection migration is enabled (quinn-proto default, with path validation),
  so a peer's address may change mid-session; peer IDs, not addresses, identify
  participants.
- Native policy is trusted code outside the transport work budget; it must be
  fast, nonblocking and fail closed. Certificate validity is checked at handshake
  only; hosts handle revocation/rotation by calling `disconnect`.
- A caller-supplied rustls configuration can weaken or remove authentication
  through its verifier; see the previous section.
- `ConditionedEndpoint`/`ConditionedSocket` timing depends on poll frequency.
- Desktop scope only (Windows, Linux, macOS). No NAT traversal, relays,
  matchmaking, reconnection or session recovery is implemented here; the
  `DatagramSocket` boundary exists for those layers.

## Verification

`cargo test -p engine-network -p engine-net-quic` covers, on real loopback
sockets (`127.0.0.1:0`) with synthetic in-memory rcgen identities and no external
hosts:

- TCP: every delivery and lane round-trips in order over plaintext and mutual
  TLS (Authenticated before Connected), size/lane/peer rejection, `QueueFull`
  for reliable and counted drops for unreliable, the first poll latching the
  event API (snapshots still applied under lanes), a peer flooding 33 messages
  the latched API cannot represent (data under legacy, `Input` under lanes, data
  to a legacy client) dropped within `idle_timeout` with its single slot
  reusable, caller rustls configs with
  early data, tickets and missing ALPN corrected and interoperating (policy runs
  once per connection), a caller server config without client auth failing
  closed, and `ConditionedEndpoint` delaying everything (>= 40 ms), dropping and
  duplicating only unreliable payloads, keeping reliable ordering and never
  releasing stale sequenced payloads.
- QUIC: mutual authentication; wrong roots, wrong server name, an untrusted
  client certificate, a missing client certificate and denied policy all fail
  closed with no Connected and no payload; a reliable-unordered stream reset after
  half its body closes the connection instead of delivering a truncated message;
  partial-message allocations never exceed their bound; every delivery round-trips both ways
  including a maximum-size datagram; oversize rejection; stale and duplicate
  sequenced datagrams dropped and counted; peer and pending-handshake caps
  refusing extra connections; server-side handshake expiry; idle timeout and
  keep-alive; `QueueFull` backpressure with complete in-order delivery
  afterwards; malformed DATAGRAM/stream headers and garbage UDP never panicking
  or admitting; caller rustls configs (0-RTT enabled, ALPN omitted/foreign)
  corrected and interoperating.

Measured on the development machine (debug build, Windows loopback, tests running
in parallel):

| Measurement | Result |
| --- | --- |
| 20 % datagram loss both directions | 300/300 reliable-ordered messages delivered in order; unreliable 809/1000 delivered (19.1 % lost; band asserted 12-28 %) |
| 60 ms one-way latency on the client socket | RTT 1.61 ms baseline -> 62.33 ms (+60.72 ms; tolerance asserted +-15 ms) |
| Loopback throughput | 8 MiB reliable-ordered in 0.334 s = 23.9 MiB/s, RTT 2.0 ms |
| Backpressure | 63 x 16 KiB messages queued before `QueueFull`; 137 of 200 burst datagrams dropped locally and counted |

Run `cargo test -p engine-net-quic --test quic -- --nocapture` to print the
measurements.
