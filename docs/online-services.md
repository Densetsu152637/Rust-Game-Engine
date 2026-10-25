# Online services: tokens, lobby, matchmaking and host migration

The `engine-online` crate is a self-hostable control plane for multiplayer games. It adds
certificate-bound authentication tokens, a lobby / session browser, rule-based matchmaking and
host migration on top of the engine's authenticated transports
([network transports](networking-transport.md)), plus a reference server binary,
`engine-online-server`. Everything runs from this repository: there is no third-party hosted
service, and no account database (a trait boundary, `PlayerDirectory`, is the integration point).

NAT traversal and relay forwarding are **not** here (they live in the NAT/relay crate); this crate
only carries what they need: an opaque per-member *connectivity candidates* blob exchanged
through the lobby, and relay-allocation tokens.

## Architecture

```text
 client app                                         online server (one process)
 ┌───────────────┐   mutual-TLS 1.3 (QUIC | TCP)   ┌──────────────────────────────────────────┐
 │ OnlineClient  │ ─────── Lane::SESSION ───────▶ │ OnlineServer<E: MessageEndpoint>          │
 │  (any         │   JSON, ReliableOrdered         │   CertificateBinding (authorize hook)     │
 │  endpoint)    │ ◀──────────────────────────────  │   └─ OnlineService  (sans-IO, takes time) │
 └───────────────┘                                  │        SessionGate ─ TokenVerifier        │
        │ join token (cert-bound)                   │        Lobby ─ MigrationState hook        │
        ▼                                           │        Matchmaker                         │
 ┌───────────────┐  verifies with the same          │        TokenIssuer (signs join tokens)    │
 │ game server   │  TokenVerifier / SessionGate     └──────────────────────────────────────────┘
 └───────────────┘
```

| Layer | Types | I/O |
| --- | --- | --- |
| Cores (sans-IO, deterministic) | `token::{TokenIssuer, TokenVerifier}`, `SessionGate`, `Lobby`, `Matchmaker`, `elect_host` | none; time is passed in (`Now`) |
| Service | `OnlineService` (gate + lobby + matchmaker + issuer) | none: `on_connected`, `on_message`, `on_disconnected`, `tick`, `drain_actions` |
| Drivers | `OnlineServer<E>`, `OnlineClient<E>` for any `MessageEndpoint` | the endpoint's nonblocking `poll` |
| Binary | `engine-online-server` (`config` module) | binds one socket |

Every message is untrusted input: sizes are capped before parsing, every field is re-validated
against `OnlineLimits`, every table is bounded, and every failure closes (disconnects) rather than
degrades.

## Authentication tokens and the trust model

### Why tokens on top of certificates

The transport already proves *possession of a certificate* (TLS 1.3 client certificates, verified
against configured roots). A certificate says nothing about *which account* the holder is or what
they may do. A token is the issuer's signed statement "subject S, holding the certificate with
fingerprint F, may do X until T". Binding the token to the certificate fingerprint makes a stolen
token useless to anyone who lacks the matching private key, and a stolen certificate useless without
a token.

### Token format (`token` module)

Bounded canonical binary (at most 1024 bytes); there is exactly one valid encoding of any token.

```text
u8  version (1)
u8  len | key id      1..=32  ASCII identifier        u8 len | issuer    1..=64
u8  len | audience    1..=64  ASCII identifier        u8 len | subject   1..=64 (UTF-8, no controls)
u8  has_session | [u64 BE session]
[32] SHA-256 of the client's TLS leaf certificate DER
u64 BE issued_at | u64 BE expires_at (unix seconds) | [16] random nonce
u8  scope count 1..=8 | scopes: 1 Connect | 2 JoinSession{session, epoch} | 3 Relay{session} | 4 Custom(name)
[64] Ed25519 signature over "engine-online/token/v1\0" || everything above
```

Signing and verification use `ring` (Ed25519, SHA-256, `SystemRandom` for nonces). The certificate
comparison uses `subtle`'s constant-time equality. Parsing validates every field, rejects trailing
bytes and duplicate scopes, and happens before signature verification, but allocates only bounded,
already-validated strings.

### Issuing and verifying

* `TokenIssuer::from_pkcs8(key_id, issuer, pkcs8, max_lifetime)` holds the private key. It has no
  `Debug`, `Clone` or `Serialize`; key buffers are wiped on drop (`zeroize`). Keys come from
  `engine-online-server generate-key` or `openssl genpkey -algorithm ed25519`.
* `TokenVerifier` trusts a set of `TrustedKey { key_id, issuer, public key }` (at most 16) and
  checks, in order: encoding, key id, **signature**, issuer (a key may only assert the issuer it was
  registered for), audience, validity window with a configurable clock skew (default 30 s, applied
  to both ends), maximum lifetime (default 24 h regardless of what was signed), exact required scope,
  **certificate binding** (constant time) and finally **replay**.
* The replay cache stores the nonce of *single-use* tokens until `expires_at + skew`. It is bounded
  (`replay_capacity`, default 65 536); when full, single-use tokens are rejected (fail closed). A
  nonce is consumed only after every other check passed, so a stranger presenting your token on the
  wrong certificate cannot burn it.
* **Rotation**: `add_key` the new public key on every verifier, switch the signer, wait for the
  old tokens to expire, `remove_key` the old one. Removal takes effect immediately.
* Scopes are exact. `JoinSession { session, epoch }` carries the host epoch (see
  [host migration](#host-migration)); `Relay { session }` is the relay-allocation credential.

### Binding tokens to the live connection

```rust
let binding = CertificateBinding::new(512, Duration::from_secs(30));
let security = QuicServerSecurity::new(
    chain, key, client_roots,
    binding.authorize_verified(),          // or binding.authorizer(|peer, chain| my_policy(peer, chain))
)?;
let endpoint = QuicServer::bind(addr, QuicServerConfig::default(), security)?;
let mut server = OnlineServer::new(endpoint, service, binding);   // claims the fingerprint at Connected
loop { server.poll()?; /* sleep ~2 ms when idle */ }
```

`CertificateBinding::authorizer` wraps your admission policy. When TLS has verified the chain and
your policy admits the peer, it records the SHA-256 of the **leaf** certificate under the assigned
`PeerId`. The table is bounded (`max_entries`; a full table rejects the peer) and entries that are
never claimed expire after `ttl`. At `Connected`, `OnlineServer` claims the fingerprint and hands
it to the `SessionGate`.

`SessionGate` then enforces: **no valid token within `auth_timeout_ms` (default 5 s) means
disconnect**, and until a token verifies the peer receives no lobby or matchmaking service. Any
failure (bad token, wrong certificate, replay, non-`hello` first message, malformed message)
closes the connection immediately after a single generic reply (`authentication failed`; the real
reason is never an oracle). A peer without a recorded fingerprint is disconnected. The token that
admitted a peer is also an expiry: when it expires the peer is disconnected and must `hello` again
with a fresh token. A new authenticated connection for the same subject replaces the old one.

### How a game server verifies join tokens

A game server (dedicated, or the player host's process) holds the public key(s) of the online
service and verifies the token a joiner presents **against the certificate of the live TLS
connection**:

```rust
let verifier = TokenVerifier::new(
    VerifierConfig::default(),
    [TrustedKey::new("k1", "my-studio", public_key_bytes)?],
)?;
// In the endpoint's authorize callback (or on the first message), with the peer's leaf DER:
verifier.verify_join(&token, "engine-game", session_id, current_epoch, leaf_der, now_unix)?;
// Err(WrongCertificate) | Err(Replayed) | Err(MissingScope /* wrong session or epoch */) | ...
```

`verify_join` requires audience, `JoinSession { session, epoch }` (exact epoch), the certificate
fingerprint, and consumes the nonce. A game server can equally reuse `SessionGate` with
`GateConfig { audience, single_use: true, .. }` and `Scope::JoinSession { .. }`. Because the token
names the **session and epoch**, a token minted for session A is useless at session B, and after a
host migration tokens of the previous epoch stop working at the new host. Replay protection is per
verifier instance; a fleet of game servers behind one name needs a shared nonce store or must accept
that each instance remembers its own.

Where connect tokens come from: the reference server verifies tokens signed by its own key (and any
configured `verify_keys`). *Authenticating a player to get a connect token (passwords, platform
login) is the operator's account system.* `engine-online-server issue-token` mints one for a given
client certificate for development and operations; production deployments run an account service
that calls `TokenIssuer::issue` after real authentication.

## Lobby and session browser

`Lobby` is keyed by transport peer; the caller supplies the authenticated subject. Rules:

| Operation | Rule |
| --- | --- |
| `create_session` | caller becomes host (epoch 1); `capacity` 1..=`max_capacity` (32); name, mode, attribute bounds; optional private (high-entropy join code) |
| `list_sessions` | **public sessions only**, ascending id, cursor paging (`after`, `limit` ≤ 25), filters: mode, exact attribute match (≤ 4), `has_space`, name prefix |
| `join_session` | by id (public only) or by private join code; duplicate subject, duplicate membership and a full session are refused; a private session addressed by id answers exactly like a missing one |
| `leave_session`, `set_ready`, `set_candidates` | members only; changes are broadcast to the other members |
| `kick`, `set_attributes`, `upload_migration_state` | **host only** and the request must carry the session's current `epoch` |
| `request_join_token`, `request_relay_token` | members only; fresh cert-bound token for the current epoch |

Join codes are 16 characters from a 32-letter alphabet (80 bits from the OS RNG). Lookup compares
the supplied code with *every* private session's code in constant time (no early exit), only the
host sees the code, and guessing is throttled: 5 failures per peer and 100 per server per minute
(configurable) lock joins-by-code out *before* any comparison, with disconnect at twice the limit.

Every session member can publish an opaque **connectivity candidates** blob (≤ 512 bytes). It is
stored per member, returned in session views, and broadcast on change; the NAT layer defines its
content. The lobby never interprets it and the consumer must treat it as hostile input (see
[residual risks](#threat-model-and-residual-risks)).

Per-peer protections: a token bucket (default 20 requests/s, burst 40; rate-limited requests get
an error, persistent abuse of 200 violations disconnects), at most `max_memberships_per_peer` (1)
sessions, at most `max_sessions` (1024) sessions in total, 8 malformed messages before disconnect,
at most `max_peers` (1024) tracked peers.

A successful `create_session` / `join_session` reply carries a **join token** for the configured
game audience: `JoinSession { session, epoch }`, bound to the caller's certificate, valid
`join_token_ttl_secs` (default 120 s).

## Matchmaking

A ticket carries player (the authenticated subject), mode, region, rating, party size, attributes
and optional latency measurements. A peer has at most one ticket. Modes are configured as
`ModeRules` (`team_count × team_size`, skill window, latency bound, attribute keys that must
match, hosting kind, timeout).

* **Constraints**: same mode; a common region (each ticket accepts its own region plus any region
  whose measured latency is ≤ `max_latency_ms`); equal values for the mode's `must_match`
  attributes; party sizes that pack into teams.
* **Skill window** for an anchor ticket after waiting *w* ms:
  `min(max_window, base_window + widen_per_second × ⌊w/1000⌋)`.
* **`tick(now)`** is deterministic: per mode, tickets are visited oldest first (ties by id); each
  unmatched ticket is the *anchor* of one attempt; candidates inside the anchor's window are added
  closest-rating-first while a common region survives and the teams still pack; a match forms only
  when every team is exactly full. Teams are balanced greedily by total rating. At most
  `max_anchors_per_tick` (256) anchors are examined per mode per pass, and passes run at most
  every `match_interval_ms` (1 s), which bounds the cost of a pass.
* Tickets can be cancelled (owner only), expire after `max_wait_ms` (`ticket_expired`), and are
  dropped when the peer disconnects or enters a lobby session (`ticket_expired`, reason
  `joined a session`). Queue sizes are bounded (`max_tickets` 4096,
  `max_tickets_per_mode` 1024).
* A match creates a private lobby session (teams recorded per member), and each member receives
  `match_found` with the session and its own join token. `Hosting::Dedicated` sessions have no
  player host (and no host migration); `Hosting::Player` picks the lowest-RTT member as host.
* The **rating is a client claim** unless the service has a `PlayerDirectory` that knows the
  subject, in which case the directory's value replaces it. `party_size > 1` means local players
  sharing one connection (split-screen guests); remote parties are lobby sessions, and the
  matchmaker does not yet take a whole session as one ticket.

### Measured match quality (seeded population)

`cargo test -p engine-online --lib matchmaking -- --nocapture` simulates 1800 players arriving over
600 s (3/s, rating ≈ N(1500, 283), 1 v 1, ticks every second, 60 s timeout; seed `0xC0FFEE`):

| Wait before match | Widening 25/s: matches, mean / max spread | Fixed window ±50: matches, mean / max spread |
| --- | --- | --- |
| < 1 s | 89, 25.8 / 50 | 65, 27.8 / 50 |
| 1–5 s | 735, 52.1 / 149 | 520, 23.8 / 50 |
| 5–10 s | 66, 108.7 / 273 | 197, 23.9 / 50 |
| 10–20 s | 9, 250.4 / 349 | 80, 26.9 / 50 |
| > 20 s | 1, 852 / 852 | 29, 26.0 / 45 |
| matched / timed out | **1800 / 0** | 1782 / 18 |

Widening trades quality for wait: the mean rating spread rises from 26 (instant matches) to 250
(10–20 s) and every match stays within `base + widen × wait` (asserted), while no one times out;
the fixed window keeps spreads ≤ 50 but 18 players (the rating tails) never match.

## Host migration

For `Hosting::Player` sessions the lobby tracks `host` and `epoch` (starts at 1).

1. **Trigger**: the host leaves, disconnects, or an operator calls `force_migrate` (for example for
   an unresponsive host that is still connected).
2. **Election** (`elect_host`, deterministic, independent of order): lowest RTT, then earliest join,
   then lowest peer ID. RTT is the round trip between the member and the *online service*, measured
   by the transport (`OnlineServer` copies `LinkStats::rtt_ms` into the service every 500 ms);
   clients cannot report their own RTT, so a member cannot win the election by lying. Unmeasured
   members rank after measured ones. For a forced migration the outgoing host is excluded.
3. **Fencing**: `epoch` increments by one. Host-only requests must carry the current epoch;
   a deposed host's requests fail with `stale_epoch` (checked before the role check, so it learns
   why), and with the new epoch they fail with `forbidden`. Join tokens carry the epoch, so a
   game server running epoch *n* rejects tokens minted for another epoch.
4. **Notification**: every member receives `host_changed { epoch, host, you_are_host, candidates,
   join_token }`; the `join_token` is a fresh epoch-bound token for that member. The new host also
   receives the join code (private sessions) and the migrated state.
5. **State hook**: the host uploads bounded opaque state (`upload_migration_state`, ≤ 32 KiB,
   current epoch, host only). The `MigrationState` trait (`accept`, `restore`, `discard`; default
   `InMemoryMigrationState`) decides what is kept and what the new host receives. Implement it to
   validate against your own schema or persist elsewhere. The bytes originate from a possibly
   malicious former host: the new host must parse them defensively.

## Wire protocol (`protocol` module)

One JSON document per `Lane::SESSION` payload, `Delivery::ReliableOrdered`, at most
`max_message_bytes` (64 KiB) in either direction:

```json
{"v":1,"id":7,"body":{"type":"create_session","name":"Friday","mode":"duel","capacity":4,"private":true}}
{"v":1,"id":7,"body":{"type":"session_created","session":{...},"join_token":"<base64url>"}}
{"v":1,"id":0,"body":{"type":"host_changed","session_id":3,"epoch":2,...}}        // notification: id 0
```

JSON was chosen over a bespoke binary format because this is low-rate control traffic that must be
inspectable and easy to implement in other languages. Strictness comes from the schema: serde
`deny_unknown_fields` on every type rejects unknown and duplicate fields, wrong types, unknown
variants and trailing data; the size is capped *before* parsing; `serde_json` bounds nesting depth
(a 50 000-deep input is rejected without stack overflow); and every decoded field is re-validated
(`ClientBody::validate`) before any state changes. Blobs (tokens, candidates, state) are base64url
without padding. A protocol version other than 1 is refused and the connection closed. A
worst-case full session view and a worst-case host-change notification are tested to fit the
limit, and `OnlineLimits::validate` re-checks that for custom limits.

Requests: `hello`, `create_session`, `list_sessions`, `join_session`, `leave_session`, `set_ready`,
`kick`, `set_attributes`, `set_candidates`, `request_join_token`, `request_relay_token`,
`upload_migration_state`, `enqueue_ticket`, `cancel_ticket`. Responses echo the request `id`
(`hello_ok`, `session_created`, `session_list`, `joined`, `left`, `token`, `ticket_queued`,
`ticket_cancelled`, `ack`, `error`). Notifications: `member_joined`, `member_left`, `member_ready`,
`member_candidates`, `attributes_changed`, `host_changed`, `kicked`, `match_found`,
`ticket_expired`. Error codes: `bad_request`, `unauthorized`, `forbidden`, `not_found`, `full`,
`rate_limited`, `limit_exceeded`, `conflict`, `stale_epoch`, `unsupported`, `internal`. Errors
for requests that parsed echo the `id`; unparseable messages are answered with id 0.

## Native API

```rust
// server side (any MessageEndpoint)
let service = OnlineService::new(ServiceConfig::default(), issuer, Arc::new(verifier))?
    .with_directory(Box::new(my_directory))          // ratings / bans
    .with_migration_state(Box::new(my_state_hook));  // optional
let mut server = OnlineServer::new(endpoint, service, binding);
server.poll()?;                                       // nonblocking; call every few ms

// client side
let mut client = OnlineClient::new(quic_or_tcp_client, OnlineLimits::default())?;
let hello = client.hello(&connect_token)?;            // request id
let create = client.request(ClientBody::CreateSession(spec))?;
for event in client.poll()? {                         // Connected / Response{id, body} / Notification(body)
    /* match on ClientEvent */
}
```

The sans-IO `OnlineService` can be driven directly (`on_connected`, `on_message`, `tick`,
`drain_actions`) from a custom transport or a test.

## Reference server (`engine-online-server`)

```text
engine-online-server run --config server.json [--exit-on-stdin-close]
engine-online-server generate-key --out signing.pem --key-id k1 --issuer my-studio
engine-online-server issue-token --config server.json --client-cert client.pem --subject alice [--ttl 3600]
```

`run` prints `engine-online-server listening <addr> transport=<quic|tcp> key_id=<id>` once bound.
It runs a nonblocking poll loop (2 ms sleep when idle). `--exit-on-stdin-close` is an opt-in
graceful shutdown (every connection is closed); it is off by default because process supervisors
often start services with a closed stdin. Without it, stop the process with a signal (there is no
signal handler, which would need `unsafe` or an extra dependency).
`generate-key` writes a PEM PKCS#8 Ed25519 key (mode 0600 on Unix, refuses to overwrite) and prints
only the *public* key; `issue-token` prints a base64url connect token bound to the first
certificate in `--client-cert`.

### Config (JSON, unknown fields rejected, relative paths resolve against the file)

```json
{
  "listen": "127.0.0.1:0",
  "transport": "quic",
  "max_connections": 256,
  "tls": {
    "server_chain": "server.pem",
    "server_key": "server-key.pem",
    "client_roots": "client-roots.pem"
  },
  "tokens": {
    "signing_key": "signing.pem",
    "key_id": "k1",
    "issuer": "my-studio",
    "online_audience": "engine-online",
    "game_audience": "engine-game",
    "relay_audience": "engine-relay",
    "join_token_ttl_secs": 120,
    "connect_token_single_use": true,
    "max_token_lifetime_secs": 86400,
    "clock_skew_secs": 30,
    "replay_cache_entries": 65536,
    "verify_keys": [{"key_id": "idp-1", "issuer": "my-idp", "public_key": "<32 bytes base64url>"}]
  },
  "limits": { "max_sessions": 1024, "requests_per_second": 20 },
  "modes": [{"name": "duel", "team_count": 2, "team_size": 1, "base_window": 50, "widen_per_second": 25}]
}
```

* `listen` defaults to `127.0.0.1:0` (loopback, OS-assigned port). Exposing the service is an
  explicit operator decision (`0.0.0.0:7777`, then open that UDP/TCP port in the firewall).
* `transport` is `quic` (UDP, mutual TLS 1.3, address-validated) or `tcp` (mutual TLS 1.3 over TCP).
* The three TLS entries are file paths (PEM or DER). `server_key` and `signing_key` are secrets: on
  Unix the server refuses to start unless they are accessible only by their owner. Nothing in the
  process prints key or token material, config values or file contents; errors name the path only.
* `client_roots` decides who may even connect. Issue client certificates from your CA at
  enrollment, or pin each client's self-signed certificate as a root. The online token then decides
  what that certificate may do.
* `limits` overrides any field of `OnlineLimits`; `modes` replaces the default `duel` and `coop4`.

### Deployment notes

* Run behind a process supervisor; bind to a specific address; one process serves one poll loop
  (single threaded), which bounds capacity to a few thousand control-plane clients.
* Protect `signing.pem` and `server-key.pem` (0600, dedicated user). Rotate the signing key by
  adding the new public key as a `verify_keys` entry on every verifier (including game servers),
  switching `signing_key`/`key_id`, and removing the old key after the longest token lifetime.
* Distribute the public key printed by `generate-key` to game servers and relays; they never need
  the private key.
* The replay cache lives in memory; restarting the server forgets consumed connect-token nonces
  (tokens remain certificate-bound and expire).

## Threat model and residual risks

Trust boundaries: the network and every client are untrusted; operator configuration, the token
signing key and the CA that issues client certificates are trusted.

| Threat | Mitigation |
| --- | --- |
| Token theft / replay on another connection | token bound to the leaf certificate fingerprint (constant-time compare) and single-use nonces; TLS 1.3 client-certificate proof of possession; resumption and 0-RTT are disabled by the transport |
| Forged / tampered / downgraded tokens | Ed25519 signature over a domain-separated canonical encoding; version byte; per-key issuer pinning; every byte flip and truncation of a token is rejected (tested) |
| Expired / future / over-long tokens | validity window with bounded skew, verifier-side maximum lifetime |
| Unauthenticated resource use | transport-level handshake caps; `auth_timeout_ms`; one failure disconnects; unauthenticated peers get no lobby state |
| Oversized, malformed, deeply nested messages | size cap before parse, strict schema, depth limit, field bounds, strike counter; seeded fuzzing (20 000 hostile payloads, 12 000 token mutations, 15 000 service operations, no panic, invariants intact) |
| Enumeration and guessing of private sessions | private sessions unlisted, by-id join indistinguishable from "missing", 80-bit constant-time-compared codes, per-peer and global guess throttles |
| Privilege escalation inside a session | host-only operations check membership, epoch and role in that order; non-members see `not_found` |
| Stale / deposed host | epoch fencing in requests and in join tokens; host election cannot be influenced by client-reported RTT |
| Memory exhaustion | every table is bounded (sessions, members, attributes, candidates, migration state, tickets, replay cache, binding table, rate-limit state); slow consumers are disconnected rather than buffered |

Residual risks and limits:

* **Fleet-wide replay**: the nonce cache is local to one verifier process. A token can be replayed
  at a *different* game-server instance holding its own cache, but only by the holder of the
  certificate it is bound to.
* **Clock dependence**: token validity uses wall-clock seconds; skew (default 30 s) is accepted on
  both ends, so a token can be used up to 30 s after expiry. A clock set before 1970 makes every
  token invalid (fail closed).
* **A deposed host process is not killed**: fencing makes it unable to alter the lobby or admit
  players holding new-epoch tokens, but a malicious former host can keep serving players who still
  connect to it, and the state it uploaded is attacker-controlled input for the new host.
* **Connectivity candidates are attacker-controlled**: an authenticated member can publish
  arbitrary addresses. The NAT layer must validate them, rate-limit probing, and never send
  amplified traffic to unverified addresses (reflection).
* **Broadcast amplification**: one candidate or attribute update fans out to up to 31 members;
  the per-peer request rate bounds the factor but a coordinated group of authenticated peers can
  still generate inbound-to-outbound traffic ~30:1.
* **Capacity exhaustion by authenticated peers**: a certificate holder can occupy
  `max_memberships_per_peer` sessions and tickets; `max_sessions` can be filled by roughly that many
  certificate holders. The certificate issuance policy is the control.
* **Ratings are claims** without a `PlayerDirectory`; matchmaking quality can be gamed by clients.
  Parties are local only (see above).
* **No persistence**: sessions, tickets, bans and ratings live in memory behind traits; a restart
  drops them (clients reconnect and re-enter).
* **Single-threaded server**: JSON parsing of 64 KiB messages at the permitted rates is the main
  CPU cost; size the instance, or run several instances per region, accordingly.
* **Memory wiping is best effort**: key buffers loaded or generated by this crate are zeroized on
  drop, but `ring` keeps its own copy of the key schedule and the OS may swap pages.
* **No signal handling** in the reference binary (see above).

## Verification

* Unit tests in every module (tokens, binding, gate, lobby, matchmaking, migration, protocol,
  limits, config, PEM).
* `tests/service.rs`: deterministic behaviour of the whole service (auth failures, deadline,
  replay, rate limits, join-code lockout, capacity, matchmaking, relay tokens, migration, fencing).
* `tests/e2e.rs`: real loopback QUIC and TLS-over-TCP (`127.0.0.1:0`, synthetic in-memory
  identities): authenticate, create, browse, join by code, match, game-server verification,
  expired / wrong-audience / wrong-key / tampered / replayed / other-certificate tokens, abuse,
  host migration.
* `tests/binary.rs`: the `engine-online-server` process over QUIC and TCP, key and token
  generation, bad configuration, stdin-close shutdown.
* `tests/fuzz.rs`: seeded fuzzing described above.

```text
cargo fmt --all --check
cargo clippy -p engine-online --all-targets --locked -- -D warnings
cargo test -p engine-online --locked
```
