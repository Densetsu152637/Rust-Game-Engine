# AI perception

Perception lets an entity know about other entities and events without the game code
polling the world: it **sees** targets inside a cone (blocked by physics colliders),
**hears** noises, **feels** damage, filters all of that by **team**, and keeps a
**memory** of what it sensed that decays over time. The pure part lives in
`engine-ai::perception` (no ECS, deterministic); `engine-project` binds it to the ECS
(components, events, physics occlusion, tree blackboards, telemetry).

```text
engine.PerceptionSensor  ---+
engine.PerceptionStimulus --+--> Perception::update(dt) --> per-perceiver memory
noises / damage (natives,   |                                |        |
 events, PerceptionNoise) --+                                |        +--> perception.* events
                                                             +--> blackboard keys of the entity's
                                                                  BehaviorTreeAgent / StateTreeAgent
```

## Senses

Every sense is **opt-in**: a sensor has the senses whose object is present in its data
(`"sight": {}` enables sight with the defaults). Each sense has an `affiliations` filter
(a list of `hostile`, `neutral`, `friendly`; default `["hostile"]`, damage defaults to all
three) and a `max_age`, the seconds a stimulus stays remembered after it stops being
sensed.

### Sight

A cone from the perceiver's eye along its forward axis.

| Field | Default | Meaning |
| --- | --- | --- |
| `range` | 20 | A target is **acquired** inside this distance... |
| `half_angle` | 45 | ...and this angle (degrees) from forward. |
| `lose_range` | 24 | A target already sensed is **kept** inside this (at least `range`)... |
| `peripheral_half_angle` | 60 | ...and this angle (at least `half_angle`). |
| `height_offset` | 0 | Eye height above the entity origin (world +Y). |
| `max_age` | 5 | Memory time after losing sight. |

The target point is its position plus its `engine.PerceptionStimulus.height_offset`.
Candidates inside the cone need a **line-of-sight ray** from the eye to that point. With
`occlusion: "physics3d"` (the default) the ray is cast against the 3D physics colliders
([physics queries](physics-queries.md)): any solid collider between the two blocks sight;
sensor colliders do not; the perceiver's and the target's own colliders are ignored. Set
`occlusion_layers` in the settings to restrict blockers to named physics layers, or
`occlusion: "none"` to skip rays. A world without a physics service has nothing to
occlude with, so sight is never blocked there.

**Ray budget.** All perceivers share `max_rays_per_tick` (default 64, at most 4096).
Candidate pairs `(perceiver, target)` are served round-robin in ascending id order,
resuming after the last pair served; a pair that is not served keeps its previous
result (never checked counts as blocked), so a crowd degrades into slightly stale
visibility instead of stalling the frame. Targets outside the cone cost no ray.
Telemetry reports `rays_cast` and `rays_deferred`.

### Hearing

A noise has a position, a `loudness` (0-1), a `range` and a source. A listener hears it
when it is within `min(noise range, hearing.range)` (`range` default 30) and the
strength is at least `hearing.threshold` (default 0.05):

```text
strength = loudness * attenuation * (1 - distance / reach)       (clamped to 0..1)
```

The noise source is the subject; noises without a source share one anonymous subject (the
strongest wins). A listener never hears its own noises. `attenuation` is 1 unless a hook
is registered: **`register_hearing_attenuation(app, hook)`** installs
`fn(&World, listener, listener_position, source, source_position) -> f32` (0-1), called
for each noise a listener could hear. It is the seam for occlusion-aware hearing: a game
(or later the acoustics domain) can answer from geometry analysis. Perception itself does
not depend on `engine-acoustics`.

Noises come from:

- the **`perception.noise` event** (targeted at or sourced from the noisy entity;
  payload `{loudness (default 1), range (default 20), position?, team?}`; the position
  defaults to the source's world position);
- the **native** `report_noise(world, source, position, loudness, range)`;
- the **`engine.PerceptionNoise`** component: a noise at the entity's position every
  `interval` seconds (0 = every tick), the first one on the first tick.

A noise's team is that of the source's `engine.PerceptionStimulus` (payload `team` wins;
0 without one).

### Damage

The victim learns who hurt it and where the instigator was. Reported with the native
`report_damage(world, victim, instigator, amount)` or the **`perception.damage`** event
(target = victim, source = instigator, payload `{amount?, position?}`). A sensor needs a
`damage` object to react. Strength is always 1.

### Teams

Teams are integers; 0 means "no team". `engine.PerceptionSettings` (one entity per scene)
holds the table:

- equal non-zero teams are `friendly`;
- a pair involving team 0 is `unaffiliated` (default `neutral`);
- other pairs use `default_affiliation` (default `hostile`);
- `relations: [{a, b, affiliation}]` overrides any pair (symmetric).

A sense reacts to a stimulus only when the affiliation of (perceiver team, stimulus team)
is in the sense's `affiliations`. Filters are per sense: a sensor can see only hostile
targets yet hear everything.

## Memory

Each perceiver keeps one record per `(subject, sense)`: last known position, the strength
at the last sensing, seconds since last sensed (`age`) and whether it is sensed right now.
Each update:

- a stimulus sensed now refreshes its record (age 0) and emits `perception.sensed` when
  the record is new or was not sensed before;
- a record that is not sensed ages by `dt`; sight additionally emits `perception.lost`
  the first update it is no longer sensed (hearing and damage are instantaneous and emit
  no `lost`);
- a record is **forgotten** (`perception.forgotten`, removed) once `age >= max_age`, so
  at 60 Hz a `max_age` of 1 s forgets on the 60th update after the last sensing, at 10 Hz
  on the 10th; `max_age: 0` forgets on the first update without sensing;
- the strength of a remembered record falls linearly from its last value to 0 at `max_age`.

At most 64 records are kept per perceiver; when full, the weakest remembered record is
evicted (records sensed now are never evicted, a new stimulus is then ignored).

**Best target.** `Perceptions::best(entity, filter)` (and the telemetry `best`) picks the
record that is sensed now, then the strongest, then the freshest, then the lowest subject
id and sense.

## Components

| Component | Fields |
| --- | --- |
| `engine.PerceptionSensor` | `enabled`, `team`, `sight`, `hearing`, `damage` (each an object to enable), `forward` (local forward axis, default `[0, 0, -1]`), `blackboard` |
| `engine.PerceptionStimulus` | `enabled`, `team`, `visible` (can sight see it; default true), `height_offset` |
| `engine.PerceptionNoise` | `enabled`, `loudness` (0-1), `range`, `interval` |
| `engine.PerceptionSettings` | `default_affiliation`, `unaffiliated`, `relations`, `max_rays_per_tick`, `occlusion` (`physics3d`/`none`), `occlusion_layers` |

All are authored with `component_set` and described by `component_schema`; values are
validated when the project loads (`perception.value.invalid`, at most one settings entity:
`perception.settings.duplicate`).

### Blackboard integration

`sensor.blackboard` names keys the service keeps current on the entity's
`engine.BehaviorTreeAgent` and/or `engine.StateTreeAgent`:

| Field | Key type | Value |
| --- | --- | --- |
| `target` | `entity` | Authored id of the best target (cleared when nothing is known, or when it is anonymous). |
| `position` | `vec3` | Its last known position (cleared with it). |
| `sensed` | `bool` | Whether the best target is sensed right now (false when none). |
| `affiliations` | | Which affiliations may be the best target; default `["hostile"]`. |

Project validation checks that the entity has such an agent and that its graph declares
each key with that type (`perception.blackboard.agent`, `perception.blackboard.invalid`).
Writes use the normal blackboard, so behavior tree observers and state tree transitions see
them. Perception runs **before** the tree services in the same tick, so a tree reacts to
what a sensor just sensed without a tick of delay:

```json
{"engine.PerceptionSensor": {"version": 1, "data": {
  "team": 1,
  "sight": {"range": 15, "half_angle": 50},
  "hearing": {},
  "blackboard": {"target": "enemy", "position": "last_pos", "sensed": "seen"}}},
 "engine.StateTreeAgent": {"version": 1, "data": {"graph": "guard-mood"}}}
```

with a state tree transition `calm -> chase` on `seen == true` and a `move_to` task that
uses `last_pos`.

## Events

Events target the perceiver and are delivered on the next tick.

| Event | Payload |
| --- | --- |
| `perception.sensed` | `{subject, sense, position, strength}` |
| `perception.lost` | same (sight only) |
| `perception.forgotten` | same |

`subject` is the authored id (`null` for anonymous noises or a despawned entity) and the
event's `source` is the subject entity when it still exists. Emission is bounded: at most
4 events per perceiver and 64 in total per tick; the rest, and any event the full queue
(1024) rejects, are dropped and counted in `dropped_events`.

## Telemetry

`RuntimeSnapshot.perception` (and the `perception` member of the MCP `project_run_headless`
summary) is absent without sensors:

```json
{"total": 1, "stimuli": 2, "rays_cast": 1, "rays_deferred": 0, "noises": 0,
 "dropped_events": 0, "dropped_stimuli": 0,
 "perceivers": [{"entity": "guard", "team": 1, "known": 1, "sensed": 1,
   "best": {"subject": "thief", "sense": "sight", "affiliation": "hostile",
            "position": [0, 0, -6], "strength": 1, "sensed": true, "age": 0},
   "records": [ ... ]}]}
```

At most 32 perceivers (by authored id) and 8 records each.

## Limits

| Limit | Value |
| --- | --- |
| Sensors per runtime | 512 |
| Perceivable entities | 1024 |
| Memory records per perceiver | 64 |
| Noises, damage reports per tick | 64 each (native queues: 256) |
| Rays per tick | `max_rays_per_tick` (at most 4096) |
| Team relations | 256 |
| Events per perceiver / per tick | 4 / 64 |
| Memory time | 3600 s |

## Determinism

Perception is a pure function of the stimuli, the entity ids and the `dt` sequence:
perceivers, targets and records are processed in ascending id order, the ray rotation is a
function of the candidate pairs, and nothing reads a clock or a random number generator.
Two runs of the same headless project give identical events and telemetry (tested with
moving targets, a wall, noises and a ray budget of 1).

## Native API

```rust
use engine_ai::perception::AffiliationFilter;
use engine_project::ai::{Perceptions, report_damage, report_noise, register_hearing_attenuation};

report_noise(world, Some(source), [x, y, z], 0.8, 25.0)?;     // next AI step
report_damage(world, victim, Some(attacker), 10.0)?;
let best = world.resource::<Perceptions>().best(guard, AffiliationFilter::HOSTILE);
```

Focused validation: `cargo test -p engine-ai --test perception`,
`cargo test -p engine-project --test perception`, `cargo test -p engine-mcp --test perception`.
