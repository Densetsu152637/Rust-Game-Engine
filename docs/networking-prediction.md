# Prediction, rollback and lag compensation

`engine-net-predict` is a deterministic, sans-IO Rust library for latency hiding in
engine multiplayer sessions. It never opens sockets, reads clocks or spawns threads.
Every component turns caller-supplied ticks and untrusted byte payloads into bounded
state changes and yields byte payloads that the application sends through any
`engine_network::MessageEndpoint` (TCP/TLS or QUIC). Physics, gameplay and engine
project integration are not dependencies; integration with `engine-project`/MCP is a
later step.

| Traffic | Lane | Delivery |
| --- | --- | --- |
| Client inputs (`InputSender`) | `PREDICTION_LANE` = `Lane::INPUT` | `INPUT_DELIVERY` = `Unreliable` |
| Input acknowledgements (`InputAck`) | `Lane::INPUT` | `INPUT_ACK_DELIVERY` = `Unreliable` |
| Prediction-key verdicts | `Lane::INPUT` | `KEY_DELIVERY` = `ReliableOrdered` |
| Rollback session messages | `Lane::INPUT` | `ROLLBACK_DELIVERY` = `Unreliable` |

Every message starts with `u8 version (1) | u8 kind`; `MessageKind::peek` classifies a
payload without decoding it, so one lane carries all four kinds. Unreliable payloads
may be lost, duplicated or reordered; every receiver tolerates that.

## Fixed timestep and history

`FixedTimestep::new(tick, max_ticks_per_update, first_tick)` (or `from_hz`) accumulates
wall time in integer nanoseconds. `advance(elapsed)` returns the due ticks
(`TimestepTicks { first_tick, count, dropped }`); more than `max_ticks_per_update`
due ticks are dropped and counted instead of spiralling. `alpha()` is the render
interpolation fraction in `[0, 1)`, and `delay(ticks)` removes accumulated time to follow
a rollback time-sync recommendation.

`TickRing<T>` is the bounded tick-keyed history used throughout: capacity is fixed at
construction (at most `MAX_RING_CAPACITY` = 65 536); the visible window is
`[newest - capacity + 1, newest]` and inserts below it are rejected.

## Client prediction and server reconciliation

Tick convention (shared with `engine_network::InputCommand::tick`): the input for tick
`T` advances the state after `T - 1` into the state after `T`. Implement:

```rust
pub trait PredictedSimulation {
    type State: Clone;
    type Input: Clone;
    fn step(&mut self, state: &mut Self::State, input: &Self::Input, tick: u64);
    fn divergence(&self, predicted: &Self::State, authoritative: &Self::State) -> f32;
}
```

`PredictionClient::new(sim, PredictionConfig { history_ticks, tolerance }, tick, state)`
starts from an authoritative state. Each client tick, `predict(input)` steps the
simulation, records the input and predicted state, and returns the new tick. When a
server state arrives, `reconcile(server_tick, &authoritative, last_processed_input_tick)`
compares it with the prediction for `server_tick`. Within `tolerance` nothing changes.
Beyond it (or for a NaN divergence) the client rewinds to the authoritative state and
re-simulates every retained input after `server_tick`, returning
`ReconcileReport { corrected, replayed_ticks, error, .. }`. Because the replay runs the
same deterministic code on the same inputs, the corrected state is bit-identical to the
server's re-simulation (tested).

Unsafe server states are ignored with no state change: `Stale` (at or before the
newest reconciled tick: reordered/duplicated), `FutureTick` (ahead of the client; the
caller may `reset`), `InvalidAck` (`last_processed_input_tick > server_tick`) and
`OutsideHistory`. Prediction is bounded: `predict` returns `Full` once
`history_ticks - 1` ticks are unconfirmed, so the client stops running ahead of a
silent server. `reset(tick, state)` restarts from an authoritative state.

`ErrorSmoother::new(half_life_ticks, snap_distance)` hides corrections visually. Before
reconciling, remember the rendered pose; afterwards call
`add_correction(old_translation, old_rotation, new_translation, new_rotation)` so
`apply(pose)` renders the old pose, then `tick()` each frame decays the translation and
rotation offsets geometrically (exact angle scaling) and snaps them to zero below
`epsilon` (default 1e-4). Corrections larger than `snap_distance` apply instantly.
Measured: half-life 4 ticks, a 1 m / 0.5 rad correction settles below 1e-3 within the
analytic K = 40 ticks.

## Input transport

`NetCodec` is the bounded binary codec for application inputs: an associated
`MAX_ENCODED_BYTES` plus hand-written `encode(&mut Writer)`/`decode(&mut Reader)`.
`Writer` refuses to exceed its limit; `Reader` returns `Malformed` instead of reading
past its slice. Implementations exist for integers, `bool` (0/1 only), finite `f32`,
`Option<T>`, `[T; N]`, tuples of up to four, glam `Vec2`/`Vec3` and unit `Quat`.
Decoders must reject anything the encoder cannot produce.

Input message: `version | kind 1 | u8 count (1..=64) | u64 newest_tick |
count x Input`, newest first. `InputSender::new(InputSenderConfig { redundancy,
max_message_bytes })` keeps the newest `redundancy` unacknowledged inputs (ticks
strictly increase; a gap restarts the run). `encode_message()` emits them newest first,
trimming the oldest to fit the byte budget; `acknowledge(received_through)` drops inputs
the server has. Send one message per client tick.

The server keeps one `InputReceiver` per peer
(`InputReceiverConfig { max_lead_ticks, max_inputs_per_message, max_message_bytes,
missing }`). `receive(bytes)` decodes and buffers inputs (`Stored`, `Duplicate`,
`Late` for ticks already consumed, `TooEarly` beyond `max_lead_ticks`, default 256 like
`Limits::max_input_tick_lead`); malformed messages change nothing. `take(tick)` consumes
ticks in strictly increasing order: `Received(input)`, `Repeated(last)` under
`MissingInputPolicy::RepeatLast { max_ticks }`, or `Missing` (`Skip`, or the repeat cap
reached so a lost release cannot fire forever). Skipped ticks count as missing.
`ack()` returns `InputAck { received_through, last_processed }`; send it (or embed its
fields in the state message), feed `received_through` to `InputSender::acknowledge` and
`last_processed` to `PredictionClient::reconcile`.

Measured over 10 000 ticks with 0..=3 ticks of reordering jitter and a lossy ack path:

| Uplink loss | Redundancy | Missing ticks |
| --- | --- | --- |
| periodic 20 % (every fifth message) | 3 | 0 |
| seeded random ~20 % with bursts capped at 2 | 3 | 0 |
| independent random 20 % | 1 / 2 / 3 / 4 / 5 | 2006 / 399 / 87 / 13 / 7 |

Redundancy `R` survives up to `R - 1` consecutive losses; under independent loss `p`
the missing rate follows `p^R` (0.2, 0.04, 0.008, 0.0016, 0.0003 predicted). Raise
redundancy, or let `RepeatLast` cover rare gaps, for bursty links.

## Prediction keys (ability-system integration)

Prediction keys are the hook the gameplay/ability layer uses to predict actions whose
side effects must be committed or undone when the server decides (Unreal GAS style).
The API is generic: no gameplay types are involved.

- `PredictionKey`: a nonzero `u32`, issued monotonically per session, never reused;
  implements `NetCodec` so it can ride inside application RPCs.
- `PredictionLedger::new(LedgerConfig { max_pending, timeout_ticks, max_depth })` (client):
  - `begin(now_tick, parent) -> Result<PredictionKey>` (`Full` beyond `max_pending`;
    `Invalid` for a non-pending parent or a chain deeper than `max_depth`).
  - `confirm(key, hooks)`, `reject(key, hooks)`, `apply(KeyVerdict, hooks)`.
  - `expire(now_tick, hooks)` undoes keys unanswered for `timeout_ticks`.
  - `clear(hooks)` undoes everything (disconnect/map change).
  - Each returns a `KeyResolution`: `Unknown`, `Deferred`, `Committed(n)` or `Undone(n)`.
- `PredictionHooks { commit(key); undo(key, UndoReason) }`, or closures through
  `HookFns(commit_fn, undo_fn)`. `UndoReason` is `Rejected`, `TimedOut`,
  `ParentUndone` or `Cleared`.
- Dependent keys: a key begun with `Some(parent)` is undone (newest first, reason
  `ParentUndone`) when any ancestor is rejected, timed out or cleared. A child confirmed
  before its parent is `Deferred` and commits only after the whole ancestor chain
  commits, in key order.
- `KeyValidator::new(KeyValidatorConfig { window, max_ahead, max_outstanding })` (server,
  one per peer). `admit(key, parent)` rejects `Replayed` keys (bitset over the last
  `window` keys), `TooOld` keys below the window, `TooFarAhead` jumps (more than
  `max_ahead` above the highest seen key, so a peer cannot exhaust the key space),
  `TooManyOutstanding`, `UnknownParent` and `ParentRejected`. A key that passes the
  replay check is consumed even if a later check fails. `resolve(key, accepted)`
  returns the `KeyVerdict`s to send; a rejection also rejects outstanding descendants.
  `KeyRejection::notify_client()` says whether to send a reject verdict (everything
  except replays and unknown resolutions).
- `encode_verdicts`/`decode_verdicts`: `version | kind 3 | u8 count | count x (u32 key |
  u8 accepted)`, sent with `KEY_DELIVERY` (reliable: a lost reject would strand a
  predicted effect until timeout).

Worked example: a dash ability with a predicted mana cost and cue, and a dependent hit
confirmation predicted inside it.

```rust
struct AbilityEffects { predicted: HashMap<PredictionKey, Vec<Effect>> }
impl PredictionHooks for AbilityEffects {
    fn commit(&mut self, key: PredictionKey) {
        // Server agreed: drop the predicted copies once the replicated effects tagged with
        // the same key arrive (or keep them if the server does not replicate them).
        self.predicted.remove(&key);
    }
    fn undo(&mut self, key: PredictionKey, _reason: UndoReason) {
        for effect in self.predicted.remove(&key).into_iter().flatten() {
            effect.revert(); // refund mana, stop cue, restore position
        }
    }
}

// Client, on input:
let dash = ledger.begin(tick, None)?;
effects.apply(dash, Effect::Cost { mana: 20 });
effects.apply(dash, Effect::Cue("dash"));
send_rpc(ActivateAbility { ability: DASH, key: dash }); // application RPC lane
// Later, inside the dash, the client predicts a hit:
let hit = ledger.begin(tick, Some(dash))?;
effects.apply(hit, Effect::Damage { target, amount: 10 });
send_rpc(ConfirmHit { key: hit, parent: dash, target });

// Server, per peer:
match validator.admit(request.key, request.parent) {
    Ok(()) => {
        let ok = abilities.try_activate(peer, &request); // authoritative rules
        let verdicts = validator.resolve(request.key, ok).expect("just admitted");
        endpoint.send(peer, PREDICTION_LANE, KEY_DELIVERY, &encode_verdicts(&verdicts)?)?;
    }
    Err(reason) if reason.notify_client() => {
        let reject = [KeyVerdict { key: request.key, accepted: false }];
        endpoint.send(peer, PREDICTION_LANE, KEY_DELIVERY, &encode_verdicts(&reject)?)?;
    }
    Err(_) => {} // replay: drop silently
}

// Client, on a verdict message and every tick:
for verdict in decode_verdicts(&payload)? {
    ledger.apply(verdict, &mut effects);
}
ledger.expire(tick, &mut effects);
```

If the server rejects `dash`, the client undoes `hit` first (`ParentUndone`) and then
`dash` (`Rejected`). If `hit` is confirmed before `dash`, it waits (`Deferred`) and
commits right after `dash`.

## Rollback sessions (GGPO style)

For peer-to-peer or relayed sessions where every peer simulates the full game:

```rust
pub trait RollbackGame {
    type Input: NetCodec + Clone + PartialEq + Default;
    type Snapshot: Clone;
    fn save_state(&self, frame: u64) -> Self::Snapshot;
    fn load_state(&mut self, snapshot: &Self::Snapshot, frame: u64);
    fn advance(&mut self, frame: u64, inputs: &[Self::Input]);
    fn checksum(&self) -> u64;
}
```

`RollbackSession::<G>::new(RollbackConfig { players, local_player, input_delay,
max_prediction, checksum_interval, max_inputs_per_message, max_message_bytes,
emit_confirmed_frames })` supports 2..=16 players with one local player per session;
all peers use the same `players`, `input_delay` and `checksum_interval`. Per frame:

1. Deliver received payloads with `receive(from_player, &bytes)`; the sender identity
   comes from the transport, never from the payload.
2. `add_local_input(input)` schedules the local input for
   `current_frame + input_delay` (frames before the delay use `Input::default()`).
3. `advance_frame(&mut game)` first rolls back if needed: when a confirmed remote input
   differs from the input used for an already simulated frame, it loads that frame's
   saved state and re-simulates to the present. Remote inputs not yet received are
   predicted by repeating the player's newest confirmed input. It then returns
   `Advanced { frame }` or, once `max_prediction` frames would be predicted for some
   player, `WaitingForRemote { player, predicted_frames }` without simulating.
4. Send each `(player, bytes)` from `outgoing()` with `ROLLBACK_DELIVERY`.
5. `drain_events()` yields `Desync { frame, player, local_checksum, remote_checksum }`
   and, with `emit_confirmed_frames`, `Confirmed { frame, checksum }`.

Message: `version | kind 4 | flags | u8 count | u64 start_frame | u64 sender_frame |
i32 sender_advantage | [u64 ack] | [u64 report_frame | u64 report_checksum] |
count x Input`. Each message resends every unacknowledged local input (capped by
`max_inputs_per_message` and the byte budget), acknowledges the recipient's inputs, and
repeats the newest checksum report. Every `checksum_interval` confirmed frames, the
checksum after that frame is reported; a mismatch with the local value raises `Desync`.
`time_sync().recommended_wait_frames` is `max((local_advantage - remote_advantage) / 2)`
over remote players, clamped to `[0, max_prediction]`; feed it to
`FixedTimestep::delay` so the faster peer slows down. Symmetric link delay cancels out
of this difference. A message whose `sender_frame` lies beyond the input acceptance
window (`current_frame + max_prediction + 2 * input_delay + 8`) is `Malformed` and
changes nothing: a legitimate peer stalls before it gets that far ahead. Frame
differences use saturating arithmetic, so hostile frame values cannot overflow.

Measured (seeded link, 15 % loss, 1..=4 step delay and reordering, input delay 2,
prediction window 8, 600 checked frames): every confirmed checksum on every peer equals
the lockstep reference. Two peers, seed 42: 112 rollbacks, max depth 3, 153
re-simulated frames; seed 4242: 104 rollbacks, max depth 3, 149 frames. Three peers,
seed 77: 305 rollbacks, max depth 4, 443 frames. A window of 4 stalls explicitly at
frame 4 and resumes when remote input arrives; nondeterminism injected at frame 100
raises `Desync` at checksum frame 120 on both peers; a peer running twice as fast is
told to wait 12 frames (the clamp).

## Lag compensation

`ColliderHistory::new(ColliderHistoryConfig { max_ticks, max_entities,
max_hitboxes_per_entity })` keeps its own hit volumes, independent of the physics
crate: integration copies collider data into `Hitbox { shape, part }` with
`HitShape::Sphere`, `Capsule` (segment plus radius) or `OrientedBox` (center,
half-extents, rotation), all in entity space. `set_hitboxes(entity, layers, hitboxes)`
registers an entity; `record(tick, poses)` stores every entity `Pose` for a server tick
(strictly increasing, all-or-nothing validation, oldest tick evicted beyond
`max_ticks`). Animated parts are recorded as separate entities, since shapes apply to
every retained tick. `pose_at(entity, tick_f64)` interpolates translation and rotation
between the bracketing ticks and returns `None` outside the window or when the entity is
absent from either tick.

Queries at a (fractional) rewound tick: `raycast(tick, Ray { origin, direction,
max_distance }, &filter) -> Option<RayHit { entity, part, distance, point }>`
(nearest hit, lowest entity on ties) and `overlap_sphere(tick, center, radius,
&filter) -> Vec<OverlapHit>`. `QueryFilter { layer_mask, exclude }` selects layers and
excludes entities such as the shooter.

`LagCompensator::new(max_rewind_ticks)` or `from_duration(max_rewind, tick)` turns a
client-claimed view tick into a rewind tick: non-finite claims (`NonFinite`) and claims
later than the current server tick (`FutureTick`) are rejected; claims older than
`current_tick - max_rewind_ticks` are clamped (`Rewind { tick, clamped: true }`);
`rewind_in`, `raycast` and `overlap_sphere` additionally clamp to the retained history
(`NoHistory` when empty). Tested: a shot at the target's pose 7 ticks ago hits at the
rewound pose and misses at the current pose; a claim 30 ticks old with a 200 ms
(11-tick) limit is clamped and misses.

## Limits

| Setting | Default | Hard bound |
| --- | --- | --- |
| `PredictionConfig::history_ticks` / `tolerance` | 128 / 1e-3 | 2..=65 536 / finite >= 0 |
| `InputSenderConfig::redundancy` / `max_message_bytes` | 3 / 1200 | 1..=64 / >= one input |
| `InputReceiverConfig::max_lead_ticks` | 256 | 1..65 536 (buffer = lead + 1) |
| `InputReceiverConfig::max_inputs_per_message` | 16 | 1..=64 |
| `MissingInputPolicy` | `RepeatLast { max_ticks: 8 }` | |
| `LedgerConfig` pending / timeout / depth | 64 / 120 ticks / 8 | pending 1..=4096 |
| `KeyValidatorConfig` window / ahead / outstanding | 1024 / 256 / 64 | window 64..=65 536 |
| Verdicts per message | | 1..=255 |
| `RollbackConfig` players / delay / window | 2 / 2 / 8 | 2..=16 / 0..=16 / 1..=64 |
| Rollback checksum interval / inputs per message | 30 / 32 | 0..=3600 / 1..=64 |
| Rollback buffers per player | `2 x window + 2 x delay + 16` frames | |
| Rollback event queue | | 4096 (oldest dropped, counted) |
| `ColliderHistoryConfig` ticks / entities / hitboxes | 64 / 1024 / 32 | 2..=4096 / 65 536 / 256 |

All buffers are allocated or capped at construction; floods of hostile ticks, keys or
messages are counted and dropped (tested). Configuration is trusted application data.

## Trust model

- Client-server prediction is authoritative on the server: `InputReceiver` accepts only
  input (never state), bounds its lead window, consumes strictly increasing ticks and
  repeats input only for a bounded run. The application still validates input semantics
  (e.g. movement in [-1, 1] as for `InputCommand`).
- The server is trusted by `PredictionClient`; reconciliation still ignores impossible
  states (future ticks, invalid acknowledgements) instead of corrupting history.
- Prediction keys are single-use per peer; replays, far-future keys, unknown or
  rejected parents and excess outstanding keys are refused before gameplay code runs.
  Keys only identify predictions; the server must authorise the action itself.
- Rollback sessions trust every peer's game simulation. A peer can send arbitrary but
  well-formed inputs for its own player (first stored input per frame wins; conflicts
  are counted) and can lie in checksum reports. Desync detection finds
  nondeterminism, not cheating; use a server-authoritative model for competitive play.
- Lag compensation trusts the client's view tick only within
  `[current - max_rewind, current]`: a hostile client can choose the most favourable tick
  in that window ("peeker's advantage") but cannot rewind further or into the future.
  Keep the window near the real RTT plus interpolation delay, and optionally compare the
  claim with the server's RTT estimate for the peer.

## Residual risks and gaps

- Determinism is the application's responsibility: floating-point simulations must
  avoid platform-dependent math (or use fixed point) for rollback and exact replay.
- Rollback uses one local player per session and no spectators; with very different
  per-peer input delays the acceptance window may be too small.
- Time sync is a frame-advantage heuristic without RTT measurement; a peer can bias it.
- Independent loss still drops `p^R` of ticks; repeat-last covers short gaps only.
- `ColliderHistory` shapes are per entity, not per tick; queries are linear in posed
  entities (no broadphase), bounded by `max_entities`.
- No engine-project components, MCP tools or physics adapters exist yet; they are a
  separate integration step.

## Verification

`cargo test -p engine-net-predict --locked` runs the deterministic suites:
`tests/prediction.rs` (exact replay vs server re-simulation, tolerance, stale/future
states, smoothing decay, timestep, rings), `tests/input.rs` (loss tables above,
receiver windows, codecs), `tests/keys.rs` (commit/reject/timeout/nesting, validator
windows, wire round trip), `tests/rollback.rs` (lockstep equality over a lossy link for
2 and 3 peers, stall, desync, time sync, malformed messages, hostile `sender_frame`
values such as `1 << 63` without overflow), `tests/lagcomp.rs` (rewound
hits and misses, clamping, bounds) and `tests/fuzz.rs` (20 000 random and mutated
buffers per decoder, flood bounds). Use `-- --nocapture` to print the measured numbers.
`cargo clippy -p engine-net-predict --all-targets --locked -- -D warnings` and
`cargo fmt --all --check` are the static gates.
