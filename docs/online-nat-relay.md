# NAT traversal and relay (`engine-net-nat`)

`engine-net-nat` lets two peers behind NATs reach each other for the QUIC transport
([network transports](networking-transport.md)) without any third-party service:

- **Reflexive discovery**: a minimal STUN-like binding exchange reveals each socket's public
  NAT mapping.
- **Candidates**: each side collects host, server-reflexive and relay candidates into a
  bounded `CandidateSet` blob that the lobby carries out of band.
- **Hole punching**: `PunchSession` sends HMAC-SHA256 authenticated probes, keyed by a
  per-session secret, and selects the best pair that works.
- **Relay fallback**: a self-hostable TURN-like relay (`RelayServer`, binary
  `engine-relay-server`) forwards opaque QUIC datagrams between exactly two authorized
  parties. `RelaySocket` lets `QuicClient`/`QuicServer` run over an allocation unchanged.
- **`DemuxSocket`**: shares one UDP socket between QUIC and this control layer, so a host can
  keep punching for new peers on its live QUIC socket.

Every core is sans-IO: it takes an explicit `now`, uses no threads, performs bounded work per
call and holds bounded state. The socket adapters implement `engine_net_quic::DatagramSocket`.
Lobby, matchmaking and token issuance live in the online-services crate. This crate takes
authorization as a callback (`Authorizer`) and candidate exchange as data (`CandidateSet`
bytes plus a `PunchSecret`), so integration can wire both in later.

## Typical flow

```rust
use engine_net_nat::*;
// Host: one UDP socket shared by QUIC and NAT control.
let (quic_socket, mut nat) = DemuxSocket::new(udp_socket, DemuxConfig::default())?;
let mut server = QuicServer::with_socket(quic_socket, QuicServerConfig::default(), security)?;

// 1. Discover the public mapping (repeat per discovery server).
let mut reflexive = ReflexiveClient::new(Instant::now(), relay_addr, ReflexiveConfig::default())?;
drive_agents(&mut nat, Instant::now(), &mut [&mut reflexive], 64)?; // every frame
// 2. Publish candidates through the lobby (opaque blob); the lobby also hands both
//    sides the same PunchSecret over its authenticated channel.
let mine = CandidateSet::gather(nat.local_addr()?, [reflexive_addr], [relay_addr])?;
lobby.publish(mine.to_bytes());
let theirs = CandidateSet::from_bytes(&lobby.peer_blob())?;
// 3. Punch (opposite roles: the joining client controls, the host is controlled).
let mut session = PunchSession::new(Instant::now(), PunchRole::Controlled, &secret, &theirs,
    PunchConfig::default())?;
// Every frame: drive_agents(&mut nat, now, &mut [&mut session, ..], 64)?; server.poll(..)
match session.outcome() {
    // The client calls QuicClient::with_socket(its_demux_socket, path.remote, ..).
    PunchOutcome::Connected(path) => { /* the path is live */ }
    PunchOutcome::TimedOut => { /* fall back to the relay (below) */ }
    PunchOutcome::Pending => {}
}
// 4. Relay fallback: allocate with a relay-scoped credential and run QUIC over it.
let mut relayed = RelaySocket::new(other_udp_socket, relay_addr, RelaySocketConfig::default())?;
let id = relayed.allocate(&credential)?;
// poll_control() until status(id) == Ready { peer }, then
// QuicClient::with_socket(relayed, peer, ..) or QuicServer::with_socket(relayed, ..).
```

`drive_agents(socket, now, agents, max)` reads up to `max` datagrams, offers each control
datagram to the agents until one consumes it, and sends everything due. A host keeps one
`PunchSession` per joining peer on the same `NatChannel`, because sessions are distinguished
by a session ID derived from their secret.

## Wire formats

Every control datagram starts with a 5-byte header: a kind byte in `0x01..=0x3F` and the
4-byte magic `NAT1`. The kind byte keeps the QUIC long-header bit (`0x80`) and fixed bit
(`0x40`) clear. Integers are big-endian. Decoders are strict. They require exact lengths,
canonical zero padding, known flags and valid unicast addresses (nonzero port, not
unspecified, multicast or broadcast), so each message has exactly one encoding.

Addresses are a fixed 19 bytes: a family byte (4 or 6), the port, and 16 IP bytes (IPv4 uses
the first 4 and the rest are zero). Observed addresses are XOR-masked with `NAT1` and the
transaction ID, as in STUN's XOR-MAPPED-ADDRESS, so NAT ALGs do not rewrite them.

| Kind | Message | Layout after header | Size |
| --- | --- | --- | --- |
| `0x01` | Binding request | txid (12) + zero padding | 40 |
| `0x02` | Binding response | txid + masked observed address | 36 |
| `0x10` | Punch probe | session (8), flags, txid, 19 zero bytes, HMAC tag (32) | 77 |
| `0x11` | Punch ack | session, flags, echoed txid, masked observed address, tag | 77 |
| `0x20` | Allocate | txid, cookie (16, zero when none), credential length (u16), credential, zero padding to 96 | 96..1059 |
| `0x21` | Allocate challenge | txid, cookie | 33 |
| `0x22` | Allocate OK | txid, vport (u16), allocation key (16), lifetime s (u16), counterpart vport (u16, 0 = none), masked observed address | 58 |
| `0x23` | Refresh (lifetime 0 releases) | txid, vport, key, lifetime s, zero padding | 64 |
| `0x24` | Refresh OK | txid, lifetime s, counterpart vport | 21 |
| `0x25` | Relay error | txid, code (1 unauthorized, 2 party taken, 3 capacity) | 18 |
| `0x30` | Data | source vport, destination vport, opaque payload | 9 + payload |

Punch flags: bit 0 means the sender is controlling, bit 1 means nominate. The tag is
HMAC-SHA256 over the first 45 bytes. That covers the kind byte, so a probe cannot pass as an
ack. The key and the 8-byte session ID are separate HMAC derivations of the 32-byte
`PunchSecret` (labels `engine-net-nat/punch/key/v1` and `.../session/v1`).

**`CandidateSet` blob**: `[version 1][count <= 8]`, then per candidate `[kind][19-byte
address]` (kinds: 1 host, 2 server-reflexive, 3 peer-reflexive, 4 relay). At most 162
bytes. Duplicates are rejected. The blob holds no secrets, so the lobby may store and relay
it as opaque data.

**HMAC relay credential** (`HmacCredentials`, reference scheme): `[version 1][channel
16][party 0|1][expiry unix seconds u64][HMAC-SHA256 tag 32]` (58 bytes). The tag covers the
label `engine-net-nat/relay-credential/v1` and every field. It is verified in constant time,
and the expiry becomes the grant's hard deadline.

## NAT types and outcomes

`tests/support` simulates NATs with real loopback sockets. Each public mapping is its own
`127.0.0.1:0` socket. Filtering drops unsolicited inbound datagrams, and destinations
outside loopback (private host candidates) are black-holed.

| Peer A | Peer B | Result (tested) |
| --- | --- | --- |
| full cone | full cone | direct (server-reflexive pair) |
| restricted cone | port-restricted cone | direct |
| port-restricted cone | port-restricted cone | direct |
| symmetric | full cone (either role) | direct via a **peer-reflexive** address learned from an authenticated probe |
| symmetric | port-restricted cone | punching times out, relay connects |
| symmetric | symmetric | punching times out, relay connects; QUIC mutual TLS end to end through the relay |

All addresses in the simulator are `127.0.0.1`, so address-dependent ("restricted cone")
filtering behaves like endpoint-independent filtering there. Port-restricted and symmetric
behaviour is simulated faithfully. Hairpinning and mapping timeouts are not simulated.
Detect a symmetric NAT before punching by comparing the reflexive address from two
discovery servers: the ports differ when mapping depends on the destination.

## Punching algorithm

This is a simplified ICE. The controlling side probes every remote host and server-reflexive
candidate. It never probes relay candidates, which only tell the peer which relay to use.
Candidates are tried in preference order: host 126, peer-reflexive 110, server-reflexive
100. The probe interval is `probe_interval` (40 ms), with at most `max_probes_per_candidate`
(60) probes per candidate.

- **Answering**: an authenticated probe with the opposite role and a fresh transaction ID is
  answered with a same-size ack sent to the probe's source address, which reports that
  address back. The source also gets a triggered check: it is probed right away, and an
  unknown source is added as a peer-reflexive candidate (at most `max_remote_candidates`,
  16).
- **Success**: an ack counts only when it comes from the exact address that was probed and
  echoes one of the last 4 outstanding transaction IDs for that address. Each transaction
  ID is accepted once.
- **Nomination**: the controlling side nominates the best pair that works (highest
  preference, then lowest RTT). It does so once no better candidate can still succeed, or
  `nomination_wait` (100 ms) after the first success. It completes when the nominated pair's
  ack also carries the nominate flag. The controlled side completes when the nominated pair
  has also passed its own check, so both directions are proven.
- **Bounds**: at most `max_datagrams_per_second` (400) datagrams and `max_acks_per_second`
  (100) acks, with 16 queued acks.
- **Fallback**: if no pair is selected within `timeout` (4 s), the outcome is `TimedOut` and
  the caller falls back to the relay that the peer's `CandidateSet::relays()` names. The QUIC
  handshake still validates a selected path. If it times out, fall back to the relay as well.

Rejected and counted in `PunchStats` (never errors):

- Datagrams with a different session ID are not consumed, so another session can claim them.
- A wrong HMAC counts as `rejected_auth`.
- A datagram claiming our own role counts as `rejected_role` (reflection).
- An ack matching no outstanding probe or address counts as `rejected_unmatched`.
- A probe whose transaction ID was already seen counts as `rejected_replay` (1024 remembered,
  more than a peer's whole default probe budget).

## `DemuxSocket` and the QUIC fixed bit

`DemuxSocket::new(socket, config)` returns a `DemuxSocket`, which yields only QUIC
datagrams and goes to `QuicServer::with_socket`/`QuicClient::with_socket`, and a
`NatChannel`, which yields only control datagrams and goes to `drive_agents` or a
`RelaySocket`. Either half that reads a datagram meant for the other half queues it, within
`max_control_datagrams`/`bytes` and `max_quic_datagrams`/`bytes`. Overflow is counted as a
drop. Each half reads at most `max_reads_per_call` times per call. Sends go straight to the
shared socket.

**Classification** (`is_control_datagram`): a first byte in `0x01..=0x3F` **and** bytes
1..5 equal to `NAT1`. RFC 9000 QUIC sets the fixed bit `0x40`, and `engine-net-quic` builds
every endpoint with QUIC-bit greasing (RFC 9287, `EndpointConfig::grease_quic_bit`) disabled.
Engine endpoints therefore never advertise greasing, and engine peers always set `0x40`. The
first-byte rule alone separates the protocols between engine endpoints.

The 4-byte magic is kept as defense in depth against foreign QUIC stacks that grease anyway.
Such a greased short-header packet is misclassified only if our connection ID starts with
`NAT1`, which has probability 2^-32 per issued connection ID. QUIC then recovers it as loss.
Unit and end-to-end tests pass greased, long-header and short-header QUIC through the demux.

## Relay

**Allocation**:

1. The client sends Allocate with a zero cookie. The server answers with a stateless
   challenge cookie, an HMAC of the source address and a `cookie_lifetime_secs` time window
   that is accepted for one to two windows. This is return routability: a spoofed source
   never sees the cookie.
2. The client repeats Allocate with the cookie and its opaque credential. Only then does the
   native `Authorizer` (`Fn(&[u8], SocketAddr) -> Option<Grant>`) run.
3. The `Grant` names a `ChannelId` (16 opaque bytes), a `Party` (`A` or `B`) and an optional
   hard deadline (`max_lifetime`).
4. Each channel has exactly two party slots. A slot is bound to the source address that
   allocated it. A retransmitted request from that address gets the same allocation; a
   request from another address is refused with *party taken*.
5. The reply carries a relay-unique virtual port (`vport`), a 128-bit allocation key and the
   lifetime.

**Permissions and forwarding**: an allocation's permission list is exactly its
counterpart's slot. A Data datagram is forwarded unchanged (same bytes, same size) to the
counterpart's bound address only when all of these hold:

- its source vport is a live allocation;
- the datagram arrives from that allocation's bound address;
- its destination vport is that allocation's live counterpart;
- the payload is within `max_payload_bytes`;
- per-allocation and global packet and byte buckets allow it.

There is no way to name an arbitrary destination address, so the relay is not an open
relay. Payloads are opaque. QUIC runs end to end, so the relay sees only QUIC ciphertext and
cleartext QUIC headers.

**Lifetime**: each allocate or refresh grants `min(requested, allocation_lifetime_secs)`.
The grant deadline, such as a credential's expiry, is absolute: refreshes never extend past
it. Refresh needs the allocation key from the bound address and is compared in constant time.
Refresh OK reports the counterpart's vport, which is how clients learn it. Lifetime 0
releases the allocation. Expired allocations are removed lazily, and a sweep runs at most
once per second.

**`RelaySocket`**: `allocate(credential)` returns an `AllocationId`. `status(id)` moves from
`Requesting` to `WaitingForPeer` to `Ready { peer }`, or ends in `Failed(Refused(..) |
TimedOut | Expired | Released)`.

- **Virtual address**: `peer` is `relay_ip:counterpart_vport`. QUIC uses it as the remote
  address, so one socket (and one `QuicServer`) can serve up to `max_allocations` (16)
  relayed peers.
- **Sending**: `send_to` accepts only a ready counterpart's virtual address. Anything else is
  refused with `AddrNotAvailable`, which QUIC treats as loss. It wraps the datagram in the
  9-byte data header.
- **Receiving**: `recv_from` accepts datagrams only from the relay address, and only for a
  live allocation and its known counterpart. Everything else is counted and dropped.
- **Control traffic**: retransmission (`request_interval`, 200 ms, `max_request_attempts`
  8), counterpart polling (`peer_poll_interval`, 200 ms) and refresh at a third of the
  lifetime all run inside `send_to`/`recv_from`. Call `poll_control()` until `Ready` before
  QUIC owns the socket. Relayed data read during `poll_control` is held, up to
  `max_queued_datagrams`.

### Limits (`RelayLimits`, JSON `limits`)

| Field | Default | Meaning |
| --- | --- | --- |
| `max_allocations` | 1024 | live allocations (2 per channel); 0 means discovery only |
| `allocation_lifetime_secs` | 120 | lifetime per allocate/refresh (1..=3600) |
| `allocation_bytes_per_second` / `allocation_packets_per_second` | 1 MiB / 2000 | forwarded per allocation (one-second burst) |
| `global_bytes_per_second` / `global_packets_per_second` | 64 MiB / 100000 | forwarded in total |
| `control_requests_per_second_per_ip` / `control_burst_per_ip` | 20 / 40 | binding, allocate and refresh per source IP |
| `max_tracked_ips` | 4096 | limiter table; when it is full of active IPs, new IPs are refused (fail closed) |
| `max_payload_bytes` | 1472 | largest relayed QUIC datagram (1200..=65000) |
| `max_credential_bytes` | 512 | credential size accepted (wire maximum 1024) |
| `cookie_lifetime_secs` | 10 | challenge window |

Memory is bounded by `max_allocations` (a few hundred bytes each), `max_tracked_ips` and two
fixed 64 KiB/2 KiB buffers in `RelayService`. Work per `poll(max)` is bounded by `max`.

## Reference server (`engine-relay-server`)

```text
engine-relay-server [--config PATH|-] [--bind ADDR]
```

The server binds UDP (default `127.0.0.1:0`; set `bind` to a public address for real use)
and prints `engine-relay-server listening on <addr>` on stdout. It writes nonsecret
`RelayStats` as one JSON line every `stats_interval_secs` (default 60, 0 = off) on stderr and
serves discovery and relaying on the same port. The loop is nonblocking: at most 256
datagrams per iteration and a 1 ms sleep when idle. There are no unbounded buffers.
`--config -` reads the config from stdin (at most 4 MiB), which keeps secrets off disk.

```json
{
  "bind": "0.0.0.0:3478",
  "stats_interval_secs": 60,
  "limits": { "max_allocations": 2048, "allocation_bytes_per_second": 2097152 },
  "auth": { "mode": "hmac", "secret_env": "ENGINE_RELAY_SECRET" }
}
```

`auth.mode` takes one of these values:

- `disabled` (default): every allocation is refused.
- `static`: `credentials` is a list of `{ "token", "channel", "party" }`. Tokens are 16 to
  1024 bytes, `channel` is 32 hex digits, and `party` is `"a"` or `"b"`.
- `hmac`: give exactly one of `secret_env` (preferred) or `secret_hex`, holding at least 32
  bytes as hex. Issuers mint credentials with `HmacCredentials::mint`.

Unknown fields are rejected. Config errors report only the line and column, never content.
Unknown command-line arguments are not echoed. Credentials, tokens and secrets never appear
in output. Deploy behind a host firewall that allows the one UDP port, and keep the static
token file or secret environment readable only by the service account. Static tokens grant
until they are removed, so prefer `hmac` mode with short expiries.

## Trust and threat model

Trusted:

- The `Authorizer` and the issuer of credentials and punch secrets (the lobby/backend).
- The lobby channel that carries `PunchSecret` and `CandidateSet`; it must be authenticated
  and encrypted, as the online services' TLS is.
- The relay operator's configuration.

Untrusted: every network datagram, peers' candidate sets, and any party holding a valid
credential, which is still bounded by its slot and rate limits. Peer identity is established
by QUIC mutual TLS, not by this layer. A wrong or attacker-selected path can only make the
QUIC handshake fail, never succeed against the wrong identity.

**Amplification**: every reply goes to the request's source and is smaller than or equal to
the request:

- binding: 36 / 40 bytes;
- challenge, allocate OK and error: 33, 58 and 18 bytes / at least 96;
- refresh OK and error: 21 and 18 bytes / 64;
- punch ack: 77 / 77.

The sizes are compile-time assertions. `RelayServer::handle` also refuses any oversized
reply at runtime (`suppressed_amplification`). Forwarded data keeps its exact size and goes
only to a return-routable bound address. In the tests, 20 000 fuzzed and valid requests gave
an aggregate control byte ratio out/in of **0.003**, and the worst single reply was **0.900**
(binding).

**Spoofing**:

- An off-path attacker cannot forge probes or acks without the secret.
- Reflected probes fail the role check.
- Replayed probes (an on-path attacker re-sending captured probes from a spoofed source) get
  no answer and trigger no checks.
- Acks are matched to outstanding transaction IDs and the exact probed address.
- Discovery accepts only a response with its own random 96-bit transaction ID from the
  server address.
- Allocation requires return routability before any credential check, and allocations are
  bound to that address.
- Refresh needs the 128-bit key.
- Data from a non-bound source is refused (`data_refused_source`).

**Residual risks**:

- Like TURN ChannelData, relayed Data datagrams are authenticated only by the source address
  and vport. An attacker who can spoof a party's exact address and port can inject datagrams
  (QUIC drops them) and use up that party's rate budget.
- An on-path attacker sees vports and can block or delay traffic. Confidentiality and
  integrity come from QUIC.
- A valid credential stolen before the legitimate party allocates can take the slot (party
  taken, which is denial of service, not interception: QUIC mutual TLS still fails).
- Static tokens are bearer secrets with no expiry.
- A malicious peer's `CandidateSet` can aim our probes at a third party. This is bounded to
  8 advertised candidates x 60 probes x 77 bytes (about 37 KB) per session, and probes stop
  at the deadline.
- An on-path observer learns the 8-byte session ID, though not the key, and can tell which
  sessions run.
- The 2^-32 per-connection-ID misclassification described for `DemuxSocket` remains until
  the proposed `engine-net-quic` change lands.
- Per-IP control limits use fixed buckets keyed by IP. An attacker with many source IPs can
  fill `max_tracked_ips`, after which new legitimate IPs are refused until entries go idle.
  The global data buckets cap total relay egress, not Internet DoS. Use firewalls and
  upstream rate limiting for public exposure.
- Credentials and allocation keys sit in process memory in the clear; there is no zeroizing.
- Timing resolution is the caller's poll interval.

## Scope notes

- **IPv6**: addresses, candidates, credentials and the relay all encode and accept IPv6, and
  a `RelaySocket` presents virtual addresses in the relay's family. The tests run only on
  IPv4 loopback, per the test constraints. Candidates are probed as given, so a probe to a
  candidate of the other address family fails as loss.
- **Host candidates**: when a socket is bound to `0.0.0.0`, its host candidate is skipped.
  Add LAN interface addresses yourself; this crate does no interface enumeration.
- **Not implemented**: UPnP-IGD, NAT-PMP and PCP port mapping; TCP/TLS relay transports;
  RFC 5389 compatibility; mid-session migration to a new relay allocation (re-allocating
  yields a new vport, which QUIC sees as a peer address change).
- **Out of scope for this crate**: lobby, matchmaking and token issuance (online services);
  `engine-project` components and MCP tools.

## Verification

`cargo test -p engine-net-nat` runs entirely on `127.0.0.1:0` sockets with synthetic
in-memory rcgen identities and contacts no external host:

- Unit tests: wire round-trips and canonical encodings; token buckets and the bounded per-IP
  limiter; reflexive transaction matching and timeouts; candidate validation; punch selection
  on both sides; forged, tampered, reflected and replayed datagrams ignored; ack rate and size
  bounds; bounded probes then timeout; demux of greased QUIC and bounded queues;
  credentials; config parsing.
- Relay server tests: two-party pairing and forwarding, idempotent allocation, release; an
  invalid credential, a foreign slot, capacity, an unpermitted destination, a spoofed source,
  an unknown vport and a wrong refresh key all refused; expiry and the refresh cap; the grant
  deadline cap; per-IP control and per-allocation byte limits; amplification measured over
  20 000 inputs.
- A seeded fuzz of 30 000 inputs x 17 targets: every wire decoder, `CandidateSet`,
  credentials, both punch roles, the reflexive client, the relay server and `RelaySocket`
  control handling. There are no panics, and no forged ack is ever accepted.
- Integration: discovery revealing mapping behaviour; punching for the cone pairs above; QUIC
  mutual TLS over a punched `DemuxSocket` path while the host punches a second peer on the
  same live socket (QUIC traffic keeps flowing; two peers connected; messages both ways);
  tampered, forged and garbage probes on the wire getting no answer; symmetric pairs timing
  out and QUIC connecting through the relay with messages both ways; `RelaySocket` refusing
  bad credentials, non-counterpart destinations and foreign datagrams; the reference binary
  serving discovery and relaying with static credentials, with no credential in its output.

Measured on the development machine (debug build, Windows loopback, tests in parallel; run
`cargo test -p engine-net-nat -- --nocapture`):

| Measurement | Result |
| --- | --- |
| Punch time (both sides selected), cone pairs and symmetric to full cone | 103.7-105.7 ms; the configured 100 ms `nomination_wait` dominates, probe RTT 1.2-1.6 ms |
| Second peer punched on the host's live QUIC socket | 104.3 ms; no demux drops |
| QUIC handshake: direct / relayed | 6.4 ms / 11.5 ms |
| Echo RTT (reliable message round trip): direct / relayed | 0.092 ms / 0.121 ms (QUIC RTT estimate 0.193 / 0.299 ms) |
| Relay overhead | 9 header bytes per datagram, 2.60 % of relayed bytes in the test mix; forwarded datagrams keep their size (ratio 1.0) |
| Relay allocation, both parties ready | 206 ms, set by `peer_poll_interval` (200 ms) |
| Amplification (control bytes out/in) | 0.003 aggregate, 0.900 worst single reply |
