# Behavior trees

Behavior trees are the first graph kind built on the shared [graph model](ai-graphs.md).
A tree is a `graph` asset of kind `behavior_tree`; an entity runs it through the
`engine.BehaviorTreeAgent` component. The pure `engine-ai` crate defines the node
types, compiles a validated document into an immutable tree and executes it
deterministically; `engine-project` binds it to the ECS (navigation, events, native
tasks, telemetry). The execution core has no engine dependency and is driven through a
`Host` trait, so it is also usable headless in tests.

```text
graph asset (JSON) --graph_edit / graph_validate-->  validated GraphDocument
        --engine_ai::behavior_tree::compile-->  Arc<BehaviorTree>   (shared, immutable)
engine.BehaviorTreeAgent --> TreeInstance (state + blackboard) --tick(dt, host)--> events
```

## A complete tree

```json behavior_tree
{
  "version": 1,
  "kind": "behavior_tree",
  "variables": [
    {"name": "alarm", "type": "bool", "default": false},
    {"name": "post", "type": "vec3", "default": [8, 0, 1]}
  ],
  "nodes": [
    {"id": "root", "node_type": "root"},
    {"id": "main", "node_type": "selector"},
    {"id": "alarmed", "node_type": "blackboard_condition",
     "params": {"key": "alarm", "operator": "eq", "value": true, "abort": "both"}},
    {"id": "chase", "node_type": "native_task", "params": {"name": "guard.chase"}},
    {"id": "patrol", "node_type": "sequence"},
    {"id": "to_a", "node_type": "move_to", "params": {"target": [2, 0, 1], "timeout": 20}},
    {"id": "look", "node_type": "wait", "params": {"seconds": 1}},
    {"id": "to_post", "node_type": "move_to", "params": {"key": "post"}},
    {"id": "sense", "node_type": "native_service", "params": {"name": "guard.sense", "interval": 0.2}}
  ],
  "edges": [
    {"from": {"node": "root", "port": "child"}, "to": {"node": "main", "port": "parent"}},
    {"from": {"node": "main", "port": "children"}, "to": {"node": "alarmed", "port": "parent"}},
    {"from": {"node": "alarmed", "port": "child"}, "to": {"node": "chase", "port": "parent"}},
    {"from": {"node": "main", "port": "children"}, "to": {"node": "patrol", "port": "parent"}},
    {"from": {"node": "patrol", "port": "children"}, "to": {"node": "to_a", "port": "parent"}},
    {"from": {"node": "patrol", "port": "children"}, "to": {"node": "look", "port": "parent"}},
    {"from": {"node": "patrol", "port": "children"}, "to": {"node": "to_post", "port": "parent"}},
    {"from": {"node": "main", "port": "services"}, "to": {"node": "sense", "port": "owner"}}
  ]
}
```

The selector tries `alarmed` first (higher priority, because its edge comes first) and
falls back to the patrol. While patrolling, setting the key `alarm` to `true` preempts
the patrol in the same tick (`abort: both`, see [Observer aborts](#observer-aborts)).
`graph_schema` with `kind: "behavior_tree"` returns the authoritative node list below.

## Node types

All nodes except `root` have an `in` flow port `parent`. Children are the targets of the
edges leaving `children` (composites) or `child` (root and decorators), in edge order.

| Node | Category | Parameters | Behavior |
| --- | --- | --- | --- |
| `root` | root | none | Entry; runs its single child every tick. |
| `sequence` | composite | none | Children in order; fails at the first failure, succeeds when all succeed. |
| `selector` | composite | none | Children in priority order; succeeds at the first success, fails when all fail. |
| `parallel` | composite | `success_policy` (`require_all`\|`require_one`, default `require_all`), `failure_policy` (`require_one`\|`require_all`, default `require_one`) | Ticks every unfinished child each tick. Failure policy is checked first; when the outcome is decided the still-running children are aborted. When all children finished without either policy matching, it fails. |
| `random_selector` | composite | `seed` (int, default 0) | A selector over a seeded shuffle drawn at every activation. See [Randomness](#randomness). |
| `inverter` | decorator | none | Swaps success and failure. |
| `repeat` | decorator | `count` (1-1000, default 1), `stop_on_failure` (default true) | Runs the child `count` times; after each completion that needs another run it stays running and restarts the child on the **next tick** (so a child that finishes instantly cannot loop within a tick). Fails at the first failure when `stop_on_failure`. |
| `retry` | decorator | `attempts` (1-1000, default 3) | Re-runs the child until it succeeds (restart on the next tick); fails after `attempts` failures. |
| `cooldown` | decorator | `duration` (0-3600 s, default 1), `lock_on` (`any`\|`success`\|`failure`) | After the child completes with a matching outcome, fails immediately without running it for `duration` seconds of tree time. State survives restarts of the tree. |
| `time_limit` | decorator | `seconds` (0-3600, default 5) | Aborts the child and fails once the child has run for `seconds` of tree time. |
| `force_success` / `force_failure` | decorator | none | Reports success / failure whenever the child completes. |
| `blackboard_condition` | decorator | `key` (variable), `operator`, `value`, `abort` | Runs its child only while `key operator value` holds; fails at once otherwise. Operators: `is_set`, `is_not_set`, `eq`, `ne`, `lt`, `le`, `gt`, `ge` (ordering needs an int or float key; `int` and `float` compare numerically; comparing an unset key is false). |
| `wait` | task | `seconds` (0-3600, default 1) | Running, then success once `seconds` of tree time (including the activation tick) have elapsed. |
| `set_blackboard` | task | `key`, `value` | Writes a constant of the key's type; success. |
| `clear_blackboard` | task | `key` | Removes the key's value; success. |
| `emit_event` | task | `event`, `payload` (object) | Emits a game event to the agent entity; success. |
| `move_to` | task | exactly one of `target` (`[x,y,z]`) and `key` (a `vec3` variable); `timeout` (seconds, 0 = none) | Requests navigation, running until the host reports arrival (success), failure or the timeout (the request is cancelled and the task fails). An unset `key` fails at once. |
| `native_task` | task | `name`, `params` (any JSON) | Runs the game-registered native task `name` (see [Native tasks](#native-tasks)). |
| `native_service`, `set_blackboard_service`, `emit_event_service` | service | `interval` (0-3600 s, default 0.5), plus the parameters of their task counterparts | Attached to a composite through its `services` port (a dedicated `bt.service` port type, so a service cannot be wired as a child). |

A tick walks from the root through the nodes that are active; a running branch resumes
where it was. When the root completes (success or failure) it restarts on the next tick.

## Observer aborts

`blackboard_condition` follows the Unreal model. The `abort` parameter:

| Mode | Effect |
| --- | --- |
| `none` | The condition is checked only when the node activates. |
| `self` | While the child runs, if a change to `key` makes the condition false, the child is aborted and the decorator fails. |
| `lower_priority` | While a **lower priority** sibling branch (a later child of the nearest `sequence` or `selector` ancestor) is running, if a change to `key` flips the condition from false to true, that branch is aborted and the composite resumes at this branch. |
| `both` | Both of the above. |

Rules the implementation guarantees:

- Changes are blackboard writes that alter a value (an equal write is not a change).
  They are observed at the start of the next tick **and** after the tick's own
  traversal, so a change made by a task, a service or a native callback during tick N
  preempts in tick N: if observers aborted something the tree is walked again (with
  `dt` 0, at most 4 walks per tick; the report shows the pass count).
- `lower_priority` fires only on a false-to-true flip, not on every write of a value
  that keeps the condition true.
- Aborting runs `abort` on native tasks and services and cancels a pending `move_to`,
  deepest node first.
- `lower_priority` and `both` need a `sequence` or `selector` parent (decorators may sit
  between); the compiler rejects other placements (`bt.observer.priority`).

## Services

A service runs when its composite activates and then every `interval` seconds while the
composite is active, at most once per tick (a large `dt` does not catch up). With
`dt` = 1/60 and `interval` 0.1 it runs on ticks 1, 7, 13, ...; with `interval` 0 it runs
every tick. Services run before the composite's children each tick, so a
`set_blackboard_service` result is visible to the children. A native service gets
`start` on its first run and `tick` afterwards; its status is ignored; leaving the
branch calls `abort`.

## Blackboard

The document `variables` declare typed keys: `bool`, `int`, `float`, `string`, `vec3`
and `entity` (an authored entity id). Defaults apply at start; the component's
`blackboard` object overrides them. Values are set by nodes, native callbacks and game
systems; every value-changing write is recorded and consumed by observers once per
tick. `int` may be written to a `float` key. Strings are at most 4096 bytes.

Game systems write the live blackboard of an agent (for example from perception):

```rust
use engine_project::ai::{BbValue, BehaviorTrees};

let mut trees = world.resource_mut::<BehaviorTrees>();
if let Some(board) = trees.blackboard_mut(entity) {
    board.set("alarm", BbValue::Bool(true))?;
}
```

(Not from inside a native task callback: the service owns the state during a tick, and
the task already receives `cx.blackboard`.)

## Randomness

The workspace has no RNG dependency, so `engine_ai::rng::SplitMix64` provides seeded
streams. A `random_selector` derives its stream from the node's authored `seed`, the
agent's `seed` and the node's position in the tree, and shuffles with Fisher-Yates at
every activation. The same inputs always give the same sequence of orders; agents that
share a graph differ only through their `seed`.

## The component

`engine.BehaviorTreeAgent` version 1 data:

```json
{"graph": "guard-patrol", "blackboard": {"alarm": false}, "tick_interval": 0, "enabled": true, "seed": 7, "navmesh": "level-nav"}
```

| Field | Meaning |
| --- | --- |
| `graph` | Required. Id of a `graph` asset of kind `behavior_tree`. |
| `blackboard` | Initial values by key; each must fit the graph variable's type. |
| `tick_interval` | Seconds between tree ticks (0-3600). `0` ticks every runtime tick; otherwise the tree ticks once at least that much time has accumulated and receives the accumulated time as its `dt`. |
| `enabled` | A disabled agent runs nothing; its running branch is aborted. Enabling starts the tree afresh. |
| `seed` | Mixed into the random selectors. |
| `navmesh` | `navigation` asset `move_to` uses when the entity's `engine.NavigationAgent` names none. |

Project validation (`Project::load`, `component_set`) checks that the graph asset exists
and compiles as a behavior tree, that blackboard overrides fit the variables, and for a
tree containing `move_to` that the entity has `engine.NavigationAgent` and a navmesh is
available. Changing any field except `tick_interval` while the game runs restarts that
agent's tree. The component holds authored data only; live state is in the runtime
service, so `try_snapshot` never changes it.

### Tick placement and navigation

The behavior tree service runs each runtime tick **after** native scripts, ECS `Update`
systems and event dispatch, and **immediately before** `simulation::step`. A `move_to`
task therefore edits the entity's `engine.NavigationAgent` (`navmesh`, `target`,
`waypoints` cleared, `repath` incremented) and the navigation service routes it in the
same tick. Outcome flows back through events: `navigation.arrived` completes the task
with success and `navigation.failed` with failure; both reach the tree one tick later
because events are delivered at the start of the next tick. `move_to` takes ownership of
the agent's navmesh and target; cancelling clears them and bumps `repath` so the agent
stops where it is. Author the `engine.NavigationAgent` of a tree-driven entity without
`navmesh` and `target` (for example just `speed`).

## Events

Events target the agent entity and are delivered on the next tick through
`EventBindings` and `register_event_handler`, like all engine events.

| Event | Payload | When |
| --- | --- | --- |
| `behavior_tree.finished` | `{graph, status: "success"\|"failure", ticks}` | The root completed a run (it restarts next tick). At most one per agent per 0.25 s of simulated time. |
| `behavior_tree.aborted` | `{graph, node, reason, key?}` | A branch was interrupted. `reason`: `observer_self`, `observer_lower_priority`, `time_limit`, `parallel_policy`, `tree_stopped` (agent disabled, removed or rebuilt). `node` is the interrupted node; `key` the triggering blackboard key. Descendants of an aborted branch are not reported separately. |
| your event | the node's `payload` | `emit_event` nodes and services. |

The queue holds 1024 events, so emission is bounded: per agent per tick at most 8
`emit_event` events, 4 `behavior_tree.aborted` and 1 `behavior_tree.finished`; at most
64 events across all agents per runtime tick; and when the queue is full the rest are
dropped instead of failing the tick. Every dropped event is counted in the telemetry
(`dropped_events`).

## Native tasks

A native task is game code a graph names. Implement `NativeTask<World>` and register a
factory before the runtime starts:

```rust
use bevy_ecs::prelude::*;
use engine_project::ai::{NativeTask, TaskContext, TaskStatus, register_bt_task};

struct Chase;
impl NativeTask<World> for Chase {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        let me = Entity::from_bits(cx.subject);
        // read or write the world; read or write cx.blackboard; cx.params is the node's JSON
        TaskStatus::Running
    }
    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> TaskStatus {
        TaskStatus::Success // or Running / Failure; cx.dt is the time since the last call
    }
    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {}
}

fn chase(_world: &mut World, _subject: u64) -> Result<Box<dyn NativeTask<World>>, String> {
    Ok(Box::new(Chase))
}

register_bt_task(runtime.app_mut(), "guard.chase", chase)?;
```

- Order: `start` when the node activates; while it returns `Running`, `tick` on every
  following tree tick; `abort` if a higher priority branch interrupts it, its agent is
  disabled or removed, or the runtime stops, before it completes. A task that completes
  never receives `abort`.
- One instance exists per agent and graph node and is reused across activations.
- A native service uses the same trait: `start` at the first run, `tick` at the
  interval (its `cx.dt` is the interval), `abort` when the branch is left.
- Tasks must be `Send + Sync`. A name used by a graph but not registered fails
  `Runtime::start` with `behavior_tree.native.unregistered`; a factory error fails the
  tick with `behavior_tree.native.failed`.

## Telemetry

`RuntimeSnapshot.behavior_trees` (and the `behavior_trees` member of the MCP
`project_run_headless` summary) is absent without agents, otherwise:

```json
{"total": 1, "dropped_events": 0, "agents": [
  {"entity": "guard", "graph": "guard-patrol", "enabled": true, "ticks": 140,
   "last_finished": "success", "active_path": ["root", "main", "patrol", "to_a"],
   "blackboard": {"alarm": false, "post": [8.0, 0.0, 1.0]}}]}
```

At most 32 agents (by authored id), 16 path nodes and 16 blackboard keys per agent
(strings cut to 64 characters). The debug view of a `TreeInstance` (`debug()`) adds the
last status of every node, accumulated tree time and a ring log of the last 64 starts,
completions and aborts.

## Limits

| Limit | Value |
| --- | --- |
| Nodes per tree (including services) | 1024 |
| Nesting depth | 64 |
| Children per composite | 64 |
| Blackboard keys | 128 |
| Agents per runtime | 512 |
| Blackboard overrides per agent | 64 |
| Tree walks per tick | 4 |
| Transition log | 64 entries |

## Determinism

A tree is a pure function of the document, the agent `seed`, the blackboard writes and
the `dt` sequence. All maps are ordered (`BTreeMap`), randomness comes from authored
seeds, time is accumulated from `dt` and never read from a clock, and agents tick in
entity order. Two runs of the same headless project give identical event sequences and
telemetry (tested with a navigation patrol on a baked navmesh).

## Compiler diagnostics

Besides the generic `graph.*` codes ([Graph documents](ai-graphs.md#validation)), the
behavior tree check reports:

| Code | Meaning |
| --- | --- |
| `bt.composite.empty` | A composite has no children. |
| `bt.composite.children` | More than 64 children. |
| `bt.tree.depth` | Nesting deeper than 64. |
| `bt.condition.value` | A comparison operator without a value, or a value of the wrong type. |
| `bt.condition.operator` | An ordering operator on a key that is not int or float. |
| `bt.observer.priority` | `lower_priority` or `both` without a `sequence` or `selector` parent. |
| `bt.value.type` | `set_blackboard` value does not fit the key's type. |
| `bt.move_to.target` | `move_to` with both or neither of `target` and `key`, or a non-`vec3` key. |
| `bt.event.name`, `bt.event.payload` | Invalid event name, or a payload that is not an object. |
| `bt.native.name` | A native task name with characters outside letters, digits, `_`, `-`, `.`, `:`. |

Component and runtime codes: `behavior_tree.graph.missing`, `.graph.kind`,
`.blackboard.invalid`, `.tick_interval.invalid`, `.navigation.missing`,
`.native.unregistered`, `.native.duplicate`, `.native.failed`, `.capacity`.

## Authoring over MCP

`graph_schema` lists the node types; `graph_validate` checks a graph; `graph_edit`
builds and changes it atomically (see [Graph documents](ai-graphs.md#mcp-tools)).
Attach the tree with `component_set` on `engine.BehaviorTreeAgent`; `component_schema`
returns its schema and defaults. `project_run_headless` reports the telemetry above.

Focused validation: `cargo test -p engine-ai`, `cargo test -p engine-project --test behavior_tree`.
