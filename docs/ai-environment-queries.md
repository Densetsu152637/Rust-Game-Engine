# Environment queries and smart objects

Two cooperating systems for "where should I go and what can I use there":

- **Environment queries** (EQS-like) generate candidate points or actors around a
  querier, reject and rank them with tests, and return the best few.
- **Smart objects** are world objects with *slots* (a seat, a work bench, a cover spot)
  that agents claim, occupy and release with deterministic arbitration. Slots are also an
  environment query generator.

The pure logic lives in `engine-ai` (`env_query`, `smart_object`); `engine-project` adds
the components, the navmesh, physics line of sight, perceived actors, events, the built-in
tree tasks and telemetry.

## Environment queries

### Authoring: `engine.EnvironmentQueries`

Query templates are **data in a component**: `engine.EnvironmentQueries { queries: [...] }`
on any entity (an "ai" entity, or the querier itself). A component, not a new asset kind,
because it is authored and validated by the existing `component_set` / `component_schema`
MCP tools, loads with the scene, and needs no project-schema change; template names are
unique scene-wide and a query is addressed by name. Templates are validated when the
project loads (`env_query.value.invalid`, `env_query.duplicate`).

```json
{"engine.EnvironmentQueries": {"version": 1, "data": {"queries": [{
  "name": "flank",
  "generator": {"type": "circle", "center": "target", "radius": 12, "rings": 2, "count": 12},
  "project_to_navmesh": true,
  "tests": [
    {"type": "reachable"},
    {"type": "distance", "to": "target", "min": 4, "score": "lower_better", "weight": 1},
    {"type": "dot", "from": "target", "direction": "forward", "score": "lower_better", "weight": 2},
    {"type": "line_of_sight", "from": "target", "from_height": 1.5, "to_height": 1.5}
  ],
  "select": {"top_n": 3}
}]}}}
```

### Generators

| `type` | Fields | Items |
| --- | --- | --- |
| `circle` | `center`, `radius`, `rings` (1), `count` (8), `height` | `count` points on each of `rings` concentric circles (outer radius `radius`) in the XZ plane, ring by ring. |
| `grid` | `center`, `half_extent`, `spacing`, `height` | A square grid, `x` outer and `z` inner loop. |
| `actors` | `source` (`perceived`/`stimuli`), `affiliations`, `max` | `perceived`: the querier's perception memory (last known positions, one item per subject). `stimuli`: every `engine.PerceptionStimulus`. Items carry the subject id. |
| `smart_objects` | `tags`, `free_only` (true), `max` | Slots with all `tags`, at their world positions; items carry the object and slot. |

`center` and the `from`/`to` of tests are **references**: `"querier"` (the entity running the
query), `"target"` (the position given to the query), `{"key": "name"}` (a `vec3` key of the
querier's tree blackboard) or a literal `[x, y, z]`. References are resolved once at the
start. At most 512 items are generated.

### Tests

Tests run in the order written and an item rejected by one skips the rest, so put cheap
tests first (evaluation units: distance and dot 1, line of sight 4, navmesh projection and
path 8).

| `type` | Measures | Notes |
| --- | --- | --- |
| `distance` | straight-line distance item to `to` | |
| `dot` | dot product of the unit direction `from -> item` with `direction` (`"forward"`: the querier's forward axis; or a reference: the direction `from -> that point`) | -1..1 |
| `path_length` | navmesh path length `from -> item` | unreachable items are rejected |
| `reachable` | a navmesh path exists | `invert` keeps unreachable ones |
| `line_of_sight` | physics ray `from -> item` is clear | `from_height`, `to_height` lift the ends; `invert` keeps blocked ones |
| `blackboard` | a gate on the querier's blackboard (`key`, `operator`, `value`) | when false no item passes |

Measuring tests take `min`/`max` (filter) and/or `score` (`lower_better`/`higher_better`)
with a `weight`. A measuring test with none of the three is rejected.

### Scoring and selection

For every score test the raw values of the surviving items are min-max normalised to 0..1
(flipped for `lower_better`; values equal up to float noise all get 1), multiplied by the
test's `weight`, and summed; the item's score is that sum divided by the total weight
(0 without score tests). Items are ranked by score descending; **ties go to the earlier
generated item**, so results are deterministic. `select.top_n` (1-16) results are returned,
`select.min_score` drops weak ones.

`project_to_navmesh` snaps generated points onto the navmesh within `snap_distance` (default
2) with `NavMesh::nearest`; points that cannot be snapped are rejected. Path and
reachability tests, and projection, use the template's `navmesh` asset, else the querier's
`engine.NavigationAgent` navmesh, else its tree agent's `navmesh`; a query that needs one
and finds none fails with `env_query.navmesh.missing`.

### Running

- **Time-sliced.** `QueryRun::step(budget, host)` spends at most `budget` evaluation units
  (one item is always processed) and resumes later. The runtime service gives all running
  queries `4096` units per tick, oldest first; a new query runs its first 256 units
  immediately, so small queries finish in the call that started them. At most 64 queries
  are in flight; an untaken result expires after 600 ticks.
- **Natively:**

  ```rust
  use engine_project::ai::env_query::{run_query, submit_query, take_query, Poll};
  let result = run_query(world, querier, "flank", Some(enemy_position))?;   // synchronous
  let handle = submit_query(world, querier, "flank", Some(enemy_position))?;
  match take_query(world, handle) { Poll::Ready(result) => { /* result.items[0] */ } _ => {} }
  ```

  A `QueryResult` has `items` (`item.position`, `item.subject`, `item.slot`, `score`,
  `index`), `generated`, `rejected`, `cost` and `steps`.
- **From a tree:** the built-in native task `engine.run_query` (a `native_task` node with
  this `name`; registered for behavior trees and state trees, no game code):

  | Param | Meaning |
  | --- | --- |
  | `query` | Template name. |
  | `target_key` | `vec3` blackboard key read as the query `target`. |
  | `position_key` | `vec3` key that receives the best item's position. |
  | `entity_key` | `entity` key that receives the best item's subject (authored id). |

  Running while the query is pending; success when an item was found (keys written), failure
  otherwise (keys cleared) or when the query cannot run. Aborting cancels the query.
- **Over MCP:** templates are authored with `component_set` / `component_schema` and
  checked at project load. There is **no query-running MCP tool**: a query needs the live
  world (positions, navmesh, physics, perception, claims), which only exists while a game
  runs; the `env_queries` telemetry in the `project_run_headless` summary shows the most
  recent results.

### Determinism and limits

Generation order, test order and the tie-break are fixed; the host's lists (actors, slots)
come ordered by entity id / slot. Nothing reads a clock or a random number generator.

| Limit | Value |
| --- | --- |
| Items per query | 512 |
| Tests per template | 8 |
| Results per query | 16 |
| Templates per component | 32 |
| Queries in flight | 64 |
| Budget per tick | 4096 units |

## Smart objects

### `engine.SmartObject`

```json
{"engine.SmartObject": {"version": 1, "data": {
  "claim_timeout": 8,
  "slots": [
    {"name": "left", "offset": [-0.6, 0, 0], "tags": ["sit", "bench"], "behavior": "sit_down"},
    {"name": "right", "offset": [0.6, 0, 0], "tags": ["sit", "bench"], "behavior": "sit_down"}
  ]}}}
```

Up to 16 slots per object, up to 8 tags per slot. A slot's world position is the object's
world transform applied to `offset`. `behavior` is an opaque id the game resolves (it is
passed to the user and in events).

### Lifecycle and arbitration

A slot is **free**, **claimed** (reserved by one user; expires after `claim_timeout`
seconds unless occupied, 0 never expires) or **occupied**. Claims are refused while anyone
holds the slot (`SlotError::Taken`), so two users never share a slot. Contested claims are
resolved by call order; the runtime ticks agents in entity order, so the lowest entity wins
a tie. For batches the registry offers `SmartObjects::arbitrate`: requests are served by
priority (high first), then user id, each taking the nearest free matching slot (ties:
lowest slot). Released slots go back to free: by the user (completed or aborted), by claim
timeout, or because the user or the object was despawned.

Natives (`engine_project::ai::smart_object`): `find_slots(world, tags, free_only, near)`
(ordered by distance to `near`, then slot), `claim_slot`, `occupy_slot`, `release_slot`,
`slot_info`; a slot is addressed by `SlotRef { object, slot }` with the object entity's bits.

### Events

Events target the **object** entity with the **user** as `source`:

| Event | Payload | When |
| --- | --- | --- |
| `smart_object.claimed` | `{slot, index, behavior, user}` | A slot was claimed. |
| `smart_object.activated` | same | The user started using it (occupied). |
| `smart_object.released` | same plus `reason`: `completed`, `aborted`, `timeout`, `removed` | The slot is free again. |

Events use the shared bounded event queue; a full queue drops them (counted in telemetry).

### The `engine.use_smart_object` task

A built-in native task for trees:

| Param | Meaning |
| --- | --- |
| `tags` | The slot must carry all of them. |
| `duration` | Seconds to occupy the slot; 0 occupies until the task is aborted. |
| `arrive_distance` | How close the user must get before occupying (default 1). |
| `walk` | Drive the entity's `engine.NavigationAgent` to the slot (the agent names a navmesh, or give `navmesh`). Without it, walk there first (for example with `move_to`). |
| `behavior_key` | `string` key that receives the slot's behaviour id. |
| `position_key` | `vec3` key that receives the slot position. |

It claims the nearest free matching slot (failure when none), occupies it on arrival,
releases it after `duration` (success), and releases it as `aborted` when the state or
branch is left. If the claim expires before the user arrives, or the slot is lost, the task
fails.

### Smart objects as an EQS generator

`{"type": "smart_objects", "tags": ["sit"]}` produces one item per matching free slot, so a
query can rank seats by distance, path length, facing or visibility and a tree can then
claim the winner.

## Telemetry

`RuntimeSnapshot.env_queries` and `RuntimeSnapshot.smart_objects` (the same-named members of
the MCP `project_run_headless` summary, bounded like all telemetry) are absent when unused:

```json
{"env_queries": {"templates": 1, "running": 0, "finished_untaken": 0, "completed": 3, "failed": 0,
   "recent": [{"querier": "guard", "query": "flank", "items": 1, "best_score": 0.83,
               "best_position": [4, 0, 2], "generated": 24, "rejected": 9, "cost": 312, "steps": 1}]},
 "smart_objects": {"objects": 1, "slots": 2, "free": 1, "claimed": 0, "occupied": 1,
   "listed": [{"entity": "bench", "slots": [
     {"name": "left", "tags": ["sit"], "state": "occupied", "user": "guard"},
     {"name": "right", "tags": ["sit"], "state": "free", "user": null}]}], "dropped_events": 0}}
```

At most 8 recent queries and 16 objects are listed.

Focused validation: `cargo test -p engine-ai --test env_query --test smart_object`,
`cargo test -p engine-project --test env_query --test smart_object`.
