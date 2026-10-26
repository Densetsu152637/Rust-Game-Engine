# Gameplay abilities

A Gameplay Ability System in the style of Unreal's GAS: attribute sets, hierarchical gameplay
tags, gameplay effects, abilities with costs, cooldowns and tag rules, and client prediction
through prediction keys. The pure library is `engine-gameplay` (module `ability`, no engine
or ECS dependency). `engine-project` wraps it as the `gameplay` asset kind, the
`engine.AbilitySystem` component, a runtime service, events, a native API, two built-in tree
tasks and the `abilities` snapshot telemetry.

## Model

- **Actor**: anything with an `engine.AbilitySystem` (in the pure crate, an opaque `u64` id).
  It holds attributes, owned tags, active effects, granted abilities and active ability
  instances. Actors are iterated in id order, so every run is deterministic.
- **Attribute**: a named `f32` with a *base* value (changed by instant and periodic effects)
  and a *current* value (base plus the modifiers of active duration and infinite effects).
  Attributes come from attribute sets; names are unique across the document.
- **Tag**: a dotted path such as `State.Debuff.Stunned`, segments of letters, digits and `_`,
  at most 128 bytes. A hierarchical query for `State` matches `State` and every descendant;
  an exact query matches only the same tag. Containers count tags, so two effects granting
  the same tag keep it until both are gone. Owned tags are the loose tags plus the granted
  tags of uninhibited effects plus the activation-owned tags of active abilities.
- **Effect**: changes attributes and tags. An *effect spec* is a definition bound to a
  source actor and a level (`make_spec` captures the source's snapshot attributes).
- **Ability**: activated on an actor, optionally against a target; checks rules, commits a
  cost and a cooldown, runs a behaviour (data-only steps or native code) and ends or is
  cancelled.

### Aggregation order

For each attribute, in the set's declaration order, the current value is:

1. If any active `override` modifier targets it, the one applied last wins.
2. Otherwise `((base + Σadd) × (1 + Σ(mul − 1))) / (1 + Σ(div − 1))`, Unreal's bias sums, so
   `×1.5` and `×2` give `×2.5` and `÷2` and `÷4` give `÷5`. A zero divisor is ignored.
3. The clamp bounds: `min` and `max` are numbers or the current value of an earlier
   attribute of the same set (`"max": "MaxHealth"`).

Modifiers are summed in active-effect handle order (application order), then modifier
order, so float sums are reproducible. A stack of n counts as n applications of each
modifier (`add` × n, `(mul − 1)` × n). Instant and periodic executions apply the same
operator to the base (`base + m`, `base × m`, `base ÷ m`, `m`) and clamp the base. When a
bound drops (a `MaxHealth` buff expires), the current value re-clamps at once and the base
keeps its value. Base values of 100 with `+10`, `+5`, `×1.5`, `×2` and `÷2` aggregate to
exactly 143.75 (see the aggregation table test).

`attribute.changed` reports changes of the current value (bitwise).

### Effects

| Field | Meaning |
| --- | --- |
| `policy` | `instant` (executes on the base once), `duration` (active `duration` seconds) or `infinite` (until removed) |
| `period`, `execute_on_application` | Seconds between executions (0 = not periodic, minimum 0.01). A periodic effect executes its modifiers on the base instead of modifying the current value; with `execute_on_application` it also executes when applied |
| `inhibition` | When `ongoing` requirements stop being met the effect is inhibited (no modifiers, granted tags or executions). On resuming: `never_reset` (the schedule kept running; missed executions are skipped), `reset_period` (next execution one period later) or `execute_and_reset_period` |
| `modifiers` | `{attribute, op: add/multiply/divide/override, magnitude}` (at most 16) |
| `stacking` | See below |
| `granted_tags` | Owned by the target while active (not for instant effects) |
| `asset_tags` | Describe the effect; matched by `remove_effects_with_tags` |
| `application` | `{required, blocked}` target tags needed to apply |
| `ongoing` | `{required, blocked}` target tags needed to stay uninhibited |
| `remove_effects_with_tags` | On application, active effects on the target whose asset or granted tags match are removed (except stacks of the same effect) |

A periodic execution due exactly at expiry runs before the removal, so a 5 s effect with a
1 s period executes 5 times (6 with `execute_on_application`). Times accumulate in `f64`
with a tolerance of `1e-5` s, so periods, expiry, cooldowns and waits land on their exact
tick at both 1/60 s and 0.1 s steps. At most 256 executions of one effect run per tick.

**Magnitudes**: `{"scalable": {"value": v, "per_level": p}}` is `v + p × (level − 1)`;
`{"attribute_based": {"attribute", "from": "source"|"target", "capture":
"snapshot"|"live", "coefficient", "pre_add", "post_add"}}` is
`coefficient × (attribute + pre_add) + post_add`. Snapshot captures read the source when the
spec is made and the target when it is applied; live captures re-read on every aggregation
or execution (across actors they settle at the end of each tick). A missing actor or
attribute reads as 0.

### Stacking

`stacking: {kind, limit, duration_refresh, period_reset, expiration, deny_overflow}`:

- `kind`: `none` (every application is a separate effect), `by_source` (one stack per
  source on the target) or `by_target` (one stack per target).
- `limit` (1..=1024): applications beyond it do not add a stack but still refresh, unless
  `deny_overflow` makes them fail (`stack_overflow`).
- `duration_refresh`: `refresh_on_success` (default; the duration restarts) or
  `never_refresh`.
- `period_reset`: `reset_on_success` (default; the period schedule restarts) or
  `never_reset`.
- `expiration`: `clear_entire_stack` (default), `remove_single_stack` (one stack goes and
  the duration restarts for the rest) or `refresh_duration` (the stack stays; removal is up
  to the game).

The existing stack keeps its first spec (source, level, captures).

### Abilities

| Field | Meaning |
| --- | --- |
| `tags` | Describe the ability; matched by cancel and block lists |
| `cost` | An instant effect applied to the owner on commit. Activation needs every attribute it modifies to stay at or above 0 after one execution on the current value |
| `cooldown` | A duration effect applied to the owner on commit; while the owner holds any of its granted tags the ability cannot activate |
| `activation_owned_tags` | Owned by the owner while active |
| `owner` / `source` / `target` | `{required, blocked}` tag rules on the owner, the instigator (default the owner) and the target |
| `cancel_abilities_with_tags` | Active abilities of the owner with a matching tag are cancelled when this one activates |
| `block_abilities_with_tags` | While active, abilities with a matching tag cannot activate |
| `instancing` | `per_actor` (one behaviour per actor, reused; one activation at a time) or `per_execution` (one behaviour per activation; activations overlap) |
| `commit` | `on_activate` (default) or `manual` (a `commit` step or a native `commit` call) |
| `native`, `params` | A registered native behaviour and its parameters (exclusive with `steps`) |
| `steps` | Data-only behaviour: `"commit"`, `{"apply_effect": {"effect", "to": "owner"/"target", "level"}}`, `{"wait": {"seconds"}}`, `{"emit_event": {"event", "payload"}}`; the ability ends after the last step |
| `triggers` | Gameplay event names that activate the ability |

Lifecycle: `can_activate` checks, in order, the actor, the ability, the grant, capacity (8
instances per actor) and instancing (`already_active`), blocking by active abilities,
owner, source and target tags, target presence (an `apply_effect` to `target` needs one),
cooldown and cost; a native behaviour's `can_activate` runs last. Activation cancels
abilities matching `cancel_abilities_with_tags`, grants the owned tags, emits
`ability.activated`, commits (`on_activate`) and runs the first step. `commit` re-checks
cooldown and cost, applies both and emits `ability.committed`. An instance finishes with
`ability.ended` or `ability.cancelled` (reason `requested`, `cancelled_by_tags`,
`commit_failed`, `target_lost`, `behaviour`, `prediction_undone`, `revoked`,
`actor_removed`). A failed activation emits `ability.failed` with the reason and changes
nothing else. A `wait` starts counting on the next tick.

Native behaviours implement `AbilityBehaviour<W>` (`can_activate`, `activate`, `tick`,
`end`), like behavior-tree native tasks: `activate` and `tick` return `Running`, `Ended` or
`Cancelled`; `end` runs exactly once per instance whatever ended it. In `engine-project`
`W` is the Bevy `World` without the gameplay service, so a behaviour uses `cx.system` for
ability-system calls.

## Prediction flow

The ability system implements `engine_net_predict::PredictionHooks`, so the networking
ledger (see [prediction keys](networking-prediction.md#prediction-keys-ability-system-integration))
commits or undoes predicted work directly:

```rust
// Client, on input:
let key = ledger.begin(tick, None)?;
runtime.try_activate(world, ActivationRequest::new(me, "Dash").with_target(t).with_key(key))?;
send_rpc(ActivateAbility { ability: "Dash", target: t, key });
// Later, a dependent prediction inside the dash:
let hit = ledger.begin(tick, Some(key))?;
runtime.system.apply_effect(me, t, "DashHit", 1, Some(hit))?;
// Server, per peer: validator.admit(key, parent), run try_activate authoritatively,
// validator.resolve(key, ok) -> verdicts.
// Client, on verdicts and every tick:
for verdict in decode_verdicts(&payload)? { ledger.apply(verdict, &mut runtime.system); }
ledger.expire(tick, &mut runtime.system);
```

In `engine-project`, call the ledger inside `gameplay::with_abilities(world, |_, rt| ...)`.

Everything a predicted activation does is journaled under its key:

- **Instant effects** (cost, damage) do not touch the base. They become *stand-ins*: active
  modifiers on the current value with every magnitude captured, so later checks see the
  predicted cost. Commit removes the stand-in and executes it on the base once, so a
  confirmed prediction is applied exactly once.
- **Duration and infinite effects** (cooldown, buffs) are active effects flagged as
  predicted; commit clears the flag.
- **Stacking** onto an existing effect journals the stack's previous state; **removal** by
  `remove_effects_with_tags` journals the removed effect.
- **Activations** are journaled; undo cancels the instance (`prediction_undone`).

Undo (reject, timeout, parent undone, clear) replays the journal newest first: stand-ins
and predicted effects are removed, stacks and removed effects restored with their original
handles, instances cancelled, then tags and current values are rebuilt, so attributes and
tags return exactly to their pre-prediction values (the tests compare every attribute and
tag container). Dependent keys follow the ledger: a child is undone with its parent, child
first; a child confirmed early commits after its parent. Effects applied by later steps of
a predicted ability carry the key while it is pending.

Gaps and choices (the prediction API is unchanged):

- `PredictionHooks::undo` has no host argument, so a native behaviour's `end` for a
  predicted instance undone by the ledger runs on the next runtime call (`step`,
  `try_activate` or `with_abilities`), not inside `undo`.
- Periodic executions of a predicted effect are skipped until it is committed (as in Unreal,
  periodic effects are not predicted); its timers keep running.
- An authoritative application that stacks onto a still-predicted stack is undone with
  that stack.
- Prediction keys carry no payload, so replicating the server's own effect handles back to
  the client (to reconcile stand-ins with replicated effects) is left to the game's RPCs.

## Data format: the `gameplay` asset

Definitions live in one JSON asset of kind `gameplay` (at most 1 MiB), not in a
component: one document serves every scene and entity, is free of the 64 KiB component
limit and of the component schema subset, and is reviewed as a single file. Agents author
it with `file_write`, register it in `project.json` (`{"id": "abilities", "kind":
"gameplay", "path": "abilities.gameplay.json"}`) and check it with `project_validate`,
which compiles every `gameplay` asset and every `engine.AbilitySystem` that references one.

```json
{"version": 1,
 "attribute_sets": [{"name": "Vitals", "attributes": [
   {"name": "MaxHealth", "base": 100},
   {"name": "Health", "base": 100, "min": 0, "max": "MaxHealth"},
   {"name": "Mana", "base": 100, "min": 0}]}],
 "effects": [
   {"name": "Damage", "modifiers": [{"attribute": "Health", "magnitude": {"scalable": {"value": -25}}}]},
   {"name": "ManaCost", "modifiers": [{"attribute": "Mana", "magnitude": {"scalable": {"value": -10}}}]},
   {"name": "StrikeCooldown", "policy": "duration", "duration": 1.0, "granted_tags": ["Cooldown.Strike"]}],
 "abilities": [
   {"name": "Strike", "tags": ["Ability.Attack"], "cost": "ManaCost", "cooldown": "StrikeCooldown",
    "steps": [{"wait": {"seconds": 0.3}}, {"apply_effect": {"effect": "Damage", "to": "target"}},
              {"emit_event": {"event": "strike.hit"}}]}]}
```

Validation rejects unknown names, repeated names, bounds naming a later attribute,
non-finite or out-of-range numbers (|v| ≤ 1e9, times ≤ 86 400 s), invalid tags, instant
effects that are periodic, stack, grant tags or have ongoing rules, duration effects
without a positive duration, costs that are not instant, cooldowns that are not duration
effects granting a tag, `native` together with `steps`, and a manual commit without a commit
step. Limits: 64 sets × 64 attributes, 1024 effects, 512 abilities, 16 modifiers, 32 tags
per list, 32 steps.

## The `engine.AbilitySystem` component

Author version 1 data as:
`{"definitions": "abilities", "attribute_sets": ["Vitals"], "attributes": {"Health": 80},
"abilities": [{"ability": "Strike", "level": 1}], "effects": [{"effect": "Regen"}], "tags":
["Team.Red"]}`

`definitions` names the `gameplay` asset; every `engine.AbilitySystem` of a scene must use
the same one (`gameplay.definitions.mixed`). `attributes` overrides initial base values of
the listed sets; `effects` are applied by the entity to itself at start (refused
applications are skipped). The component is authored data only: the live state lives in
the service, so editing the component at runtime re-initialises the actor. Errors:
`gameplay.component.invalid`, `gameplay.definitions.invalid`, `project.asset.unknown`,
`project.asset.kind`. Edit it with `component_set` like any `engine.*` component.

The service runs once per tick inside the AI step, after perception, smart objects and
environment queries and before behavior trees and state trees (which run before
`simulation::step`): it syncs the components, advances effect time (periods and expiry),
then ticks ability instances in id order. Abilities a tree activates in a tick run their
first step immediately and count waits from the next tick.

## Events

Targeted at the actor (bind them with `EventBindings` and `register_event_handler`;
delivered at the start of the next tick):

| Event | Payload |
| --- | --- |
| `ability.activated` | `{ability, instance, target, predicted}` |
| `ability.committed` | `{ability, instance}` |
| `ability.ended` | `{ability, instance}` |
| `ability.cancelled` | `{ability, instance, reason}` |
| `ability.failed` | `{ability, reason}` (`not_granted`, `already_active`, `blocked`, `owner_tags`, `source_tags`, `target_tags`, `no_target`, `cooldown`, `cost`, `behaviour`, `capacity`, ...) |
| `effect.applied` | `{effect, handle, stacks, source, predicted}` (`handle` null for an executed instant effect; event source = the source entity) |
| `effect.removed` | `{effect, handle, reason}` (`expired`, `removed`, `removed_by_tags`, `prediction_undone`) |
| `attribute.changed` | `{attribute, old, new, base}` |
| authored `emit_event` steps | the authored payload |

At most 256 events per tick, of which at most 128 `attribute.changed` (coalesced per actor
and attribute within a batch: first `old`, last `new`); the rest are dropped and counted in
telemetry, so a large fight cannot exhaust the 1024-event queue. The built-in handler
`engine.trigger_ability` turns any bound event into a gameplay event of the same name on the
bound entity (instigator = the event source), activating abilities with that trigger.

## Native API

`engine_project::gameplay`:

- `register_ability_behaviour(app, name, factory)`: `factory: fn(&mut World, u64) ->
  Result<Box<dyn AbilityBehaviour<World>>, String>`.
- `try_activate_ability(world, actor, ability, target)` and `activate_ability(world,
  ActivationRequest)` return `Result<Result<InstanceId, FailReason>>`.
- `apply_effect(world, source, target, effect, level)`, `remove_effect`, `cancel_ability`,
  `cancel_instance`, `send_gameplay_event(world, actor, event, instigator)`, `add_tag`,
  `remove_tag`.
- Queries on `&World`: `attribute(world, entity, name) -> Option<AttributeValue {base,
  current}>`, `has_tag(world, entity, tag, exact)`, `tags`, `effects`, `instance_outcome`,
  `is_ability_active`.
- `with_abilities(world, |world, runtime| ...)`: direct access to the `AbilityRuntime`
  (prediction ledger calls, `make_spec`/`apply_spec`, grants).

The pure crate offers the same on `AbilitySystem` / `AbilityRuntime<W>` for any host.

## Tree tasks

Registered for behavior trees and state trees under reserved names:

- `engine.activate_ability` `{"ability": "Strike", "target_key": "enemy", "wait": false}`:
  activates on the agent with the target from an `entity` blackboard key. Without `wait` it
  succeeds on activation; with `wait` it runs until the instance ends (success) or is
  cancelled (failure), and aborting the task cancels the instance. Refusals fail.
- `engine.wait_ability_end` `{"ability": "Strike"}`: runs while an instance of the ability is
  active on the agent, then succeeds.

## Telemetry

`RuntimeSnapshot.abilities` (and the `abilities` member of the MCP `project_run_headless`
summary) is absent without an `engine.AbilitySystem`:

```json
{"abilities": {"definitions": "abilities", "actors": 2, "active_effects": 2,
  "active_abilities": 0, "pending_predictions": 0, "dropped_events": 0,
  "listed": [{"entity": "attacker", "attributes": [{"name": "Mana", "base": 90.0, "current": 90.0}],
              "tags": ["Cooldown.Strike"], "effects": [{"effect": "StrikeCooldown", "stacks": 1,
              "remaining": 0.4, "predicted": false}], "active": [], "granted": 1}]}}
```

At most 16 actors (by authored id) with 16 attributes, tags and effects and 8 active
abilities each.

## Limits

1024 actors, 64 active effects and 8 ability instances per actor, 64 loose tags per actor,
4096 undrained system events, 1024 remembered instance outcomes, 256 periodic executions
per effect per tick.

Focused validation: `cargo test -p engine-gameplay`,
`cargo test -p engine-project --test gameplay_abilities`.
