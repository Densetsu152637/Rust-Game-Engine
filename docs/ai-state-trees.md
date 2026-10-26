# State trees

State trees are the second graph kind built on the shared [graph model](ai-graphs.md).
A tree is a `graph` asset of kind `state_tree`; an entity runs it through the
`engine.StateTreeAgent` component. A state tree is a hierarchical state machine in the
style of Unreal's StateTree: states nest, entering a state selects its first selectable
child, every state owns ordered tasks, enter conditions and prioritised transitions, and
tree-level evaluators refresh the blackboard before transitions are considered. Where a
[behavior tree](ai-behavior-trees.md) re-walks its branches every tick, a state tree
stays in its active states until a transition moves it, which suits long-lived modes
(patrol, chase, rest) with explicit switches.

The pure `engine-ai` crate defines the node types, compiles a validated document into an
immutable tree and executes it deterministically; `engine-project` binds it to the ECS
(navigation, events, native tasks and evaluators, telemetry). The execution core has no
engine dependency and is driven through the `StateTreeHost` trait (an extension of the
behavior tree `Host`), so it is also usable headless in tests. The blackboard
(`engine_ai::Blackboard`), the seeded `SplitMix64` generator and the native task trait are
shared with behavior trees.

```text
graph asset (JSON) --graph_edit / graph_validate-->  validated GraphDocument
        --engine_ai::state_tree::compile-->  Arc<StateTree>          (shared, immutable)
engine.StateTreeAgent --> StateTreeInstance (active path + blackboard) --tick(dt, host)--> events
```

## A complete tree

```json state_tree
{
  "version": 1,
  "kind": "state_tree",
  "variables": [
    {"name": "alert", "type": "bool", "default": false},
    {"name": "post", "type": "vec3", "default": [8, 0, 1]},
    {"name": "pos", "type": "vec3"},
    {"name": "to_post", "type": "float"}
  ],
  "nodes": [
    {"id": "root", "node_type": "root"},
    {"id": "e_pos", "node_type": "copy_evaluator", "params": {"input": "self.position", "key": "pos"}},
    {"id": "e_dist", "node_type": "compute_evaluator",
     "params": {"op": "distance", "a": "pos", "b": "post", "key": "to_post"}},
    {"id": "e_sense", "node_type": "native_evaluator", "params": {"name": "guard.sense"}},

    {"id": "chase", "node_type": "state"},
    {"id": "chase_ok", "node_type": "blackboard_condition",
     "params": {"key": "alert", "operator": "eq", "value": true}},
    {"id": "chase_task", "node_type": "native_task", "params": {"name": "guard.chase"}},
    {"id": "calm_down", "node_type": "transition",
     "params": {"trigger": "on_tick", "target": "state", "state": "patrol", "delay": 2}},
    {"id": "calm_c", "node_type": "blackboard_condition",
     "params": {"key": "alert", "operator": "eq", "value": false}},

    {"id": "patrol", "node_type": "state"},
    {"id": "alarmed", "node_type": "transition",
     "params": {"trigger": "on_tick", "target": "state", "state": "chase", "priority": 10}},
    {"id": "alarmed_c", "node_type": "blackboard_condition",
     "params": {"key": "alert", "operator": "eq", "value": true}},
    {"id": "leg_a", "node_type": "state"},
    {"id": "a_move", "node_type": "move_to", "params": {"target": [2, 0, 1], "timeout": 20}},
    {"id": "a_next", "node_type": "transition", "params": {"trigger": "on_succeeded", "target": "next_sibling"}},
    {"id": "look", "node_type": "state"},
    {"id": "look_wait", "node_type": "wait", "params": {"seconds": 1}},
    {"id": "look_next", "node_type": "transition", "params": {"trigger": "on_completed", "target": "next_sibling"}},
    {"id": "leg_post", "node_type": "state"},
    {"id": "post_move", "node_type": "move_to", "params": {"key": "post"}},
    {"id": "post_event", "node_type": "emit_event", "params": {"event": "guard.at_post"}},
    {"id": "post_loop", "node_type": "transition",
     "params": {"trigger": "on_succeeded", "target": "state", "state": "leg_a"}}
  ],
  "edges": [
    {"from": {"node": "root", "port": "evaluators"}, "to": {"node": "e_pos", "port": "owner"}},
    {"from": {"node": "root", "port": "evaluators"}, "to": {"node": "e_dist", "port": "owner"}},
    {"from": {"node": "root", "port": "evaluators"}, "to": {"node": "e_sense", "port": "owner"}},

    {"from": {"node": "root", "port": "children"}, "to": {"node": "chase", "port": "parent"}},
    {"from": {"node": "chase", "port": "conditions"}, "to": {"node": "chase_ok", "port": "owner"}},
    {"from": {"node": "chase", "port": "tasks"}, "to": {"node": "chase_task", "port": "owner"}},
    {"from": {"node": "chase", "port": "transitions"}, "to": {"node": "calm_down", "port": "owner"}},
    {"from": {"node": "calm_down", "port": "conditions"}, "to": {"node": "calm_c", "port": "owner"}},

    {"from": {"node": "root", "port": "children"}, "to": {"node": "patrol", "port": "parent"}},
    {"from": {"node": "patrol", "port": "transitions"}, "to": {"node": "alarmed", "port": "owner"}},
    {"from": {"node": "alarmed", "port": "conditions"}, "to": {"node": "alarmed_c", "port": "owner"}},
    {"from": {"node": "patrol", "port": "children"}, "to": {"node": "leg_a", "port": "parent"}},
    {"from": {"node": "leg_a", "port": "tasks"}, "to": {"node": "a_move", "port": "owner"}},
    {"from": {"node": "leg_a", "port": "transitions"}, "to": {"node": "a_next", "port": "owner"}},
    {"from": {"node": "patrol", "port": "children"}, "to": {"node": "look", "port": "parent"}},
    {"from": {"node": "look", "port": "tasks"}, "to": {"node": "look_wait", "port": "owner"}},
    {"from": {"node": "look", "port": "transitions"}, "to": {"node": "look_next", "port": "owner"}},
    {"from": {"node": "patrol", "port": "children"}, "to": {"node": "leg_post", "port": "parent"}},
    {"from": {"node": "leg_post", "port": "tasks"}, "to": {"node": "post_move", "port": "owner"}},
    {"from": {"node": "leg_post", "port": "tasks"}, "to": {"node": "post_event", "port": "owner"}},
    {"from": {"node": "leg_post", "port": "transitions"}, "to": {"node": "post_loop", "port": "owner"}}
  ]
}
```

`chase` is first in the root's edge order, so it is tried first when the tree starts; its
enter condition fails while `alert` is false and the tree starts in `patrol/leg_a`. While
patrolling, `alarmed` (priority 10, evaluated before the lower priority completion
transitions) switches to `chase` the tick `alert` turns true. `chase` returns to `patrol`
once `alert` has stayed false for 2 seconds. `graph_schema` with `kind: "state_tree"`
returns the authoritative node list below.

## Model

A document has one `root` node, which owns the **top-level states** (edge order is
selection order) and the **evaluators**. Everything else attaches to a state:

| Node type | Attaches to | Role |
| --- | --- | --- |
| `state` | `root.children` or `state.children` (flow edge into `parent`) | A node of the hierarchy. |
| `blackboard_condition` | `state.conditions` (enter condition) or `transition.conditions` | Blackboard comparison. |
| tasks | `state.tasks` | Work a state does while active. |
| `transition` | `state.transitions` | Leaves the state when its trigger fires. |
| evaluators | `root.evaluators` | Per-tick blackboard writers. |

Attachments use dedicated port types (`st.task`, `st.condition`, `st.transition`,
`st.evaluator`), so the editor and the validator cannot wire a task as a child state.
Child, task, condition, transition and evaluator order is the order of the edges leaving
the owner's port. Two parents for one state, or a hierarchy cycle, are graph-level errors
(`graph.edge.multiple`, `graph.cycle`). Linked states and sub-tree assets are not
supported; share behavior with native tasks or by authoring a larger tree.

### States

| Param | Values | Meaning |
| --- | --- | --- |
| `selection` | `children_in_order` (default), `children_or_self`, `self` | How entering the state picks its active descendants (see [Selection](#selection)). |
| `completion` | `any` (default), `all` | When the state's own tasks complete the state (see [Completion](#completion)). |

### Selection

Selecting a state (at start, and for the target of a transition) means:

1. All of its enter conditions must hold, else it cannot be selected.
2. `selection: self`: the state is the active leaf; its children are only reached by
   transitions.
3. Otherwise the children are tried in order and the first selectable one wins, with the
   same rule applied recursively.
4. If no child is selectable: `children_or_self` selects the state itself as the leaf;
   `children_in_order` makes the state unselectable (selection falls back to its
   siblings). A state without children is a leaf and is selected when its conditions
   hold.

At start the top-level states are tried in order; if none is selectable the tree
finishes with `failure` on its first tick. A transition whose target cannot be selected
does not fire (the next candidate is considered). The whole new path is selected before
anything is entered, so enter conditions see the blackboard as it was before the
entered states' own tasks ran.

### Completion

Each active state has its own result, decided by its **considered** tasks (the
`considered` param of every task):

- `any`: the first considered task (in authoring order) that has finished decides: its
  success or failure is the state's result.
- `all`: a considered task failing fails the state at once; when every considered task
  succeeded the state succeeds.
- A state without considered tasks never completes by itself. Instant tasks
  (`set_blackboard`, `clear_blackboard`, `emit_event`) default to `considered: false`,
  so they do not complete the state the moment it is entered; `wait`, `move_to` and
  `native_task` default to `true`.

The result is latched until the state exits. A completed state keeps running (its other
tasks keep ticking) until a transition leaves it. A transition owned by a state reacts to
the completion of that state **or of the deepest completed state below it**, so a parent
can handle "the leaf finished".

If the deepest completed state's result is not handled by any `on_completed` or matching
`on_succeeded`/`on_failed` transition on it or its ancestors (a handler that merely
cannot fire yet, for example one waiting out a delay or blocked by conditions, counts as
handling), the tree itself completes with that result.

### Tasks

Tasks start in edge order when the state is entered and tick every tree tick while it is
active; a task that has finished is not ticked again.

| Task | Params | Behavior |
| --- | --- | --- |
| `wait` | `seconds` | Runs for `seconds`, then succeeds. 0 succeeds on entry. |
| `set_blackboard` | `key`, `value` | Writes a constant on entry; succeeds. |
| `clear_blackboard` | `key` | Clears a key on entry; succeeds. |
| `emit_event` | `event`, `payload` | Emits an event to the agent entity on entry; succeeds. |
| `move_to` | `target` or `key` (vec3), `timeout` | Requests navigation; succeeds on arrival, fails on failure or after `timeout` (0 waits forever). Leaving the state cancels it. |
| `native_task` | `name`, `params` | Game code, see [Native API](#native-api). |

Every task also takes `considered` (above). When a state exits, running native tasks get
`abort` and a running `move_to` is cancelled (`cancel_move` clears the agent's navmesh and
target and bumps `repath`); tasks that already finished are left alone.

### Evaluators

Evaluators run first in every tick, in edge order, so their writes are visible to the
same tick's tasks and transitions (including enter conditions of states selected by a
transition that tick). Values they compute are cleared when a source is missing, so
derived keys never go stale.

| Evaluator | Params | Behavior |
| --- | --- | --- |
| `copy_evaluator` | `input` xor `from`, `key` | Copies a host input (see [Host inputs](#host-inputs)) or another key of the same type into `key`. |
| `compute_evaluator` | `op`, `a`, `b`?, `key` | `add`, `sub`, `mul`, `min`, `max` on int/float keys (an `int` destination needs two `int` operands, otherwise the result is a float); `distance` of two `vec3` keys into a float; `not`, `and`, `or` on bools. |
| `native_evaluator` | `name`, `params` | Game code: `start` on the tree's first tick (in place of a tick), `tick` on every later tick, `abort` when the tree finishes or stops. Returned statuses are ignored. |

### Transitions

A `transition` node belongs to the state whose `transitions` port it hangs on.

| Param | Meaning |
| --- | --- |
| `trigger` | `on_completed` (default), `on_succeeded`, `on_failed`, `on_tick`, `on_event`. |
| `event` | Event name, required for and only valid with `on_event`. |
| `target` | Required. `state` (needs `state`: the node id of the target state), `parent` (re-enter the owner's parent state, which reselects its children), `next_sibling` (the first selectable later sibling of the owner), `succeeded`, `failed` (complete the tree). |
| `priority` | 0-1000, default 0. |
| `delay`, `delay_variance` | Seconds the trigger and conditions must hold before firing (0-3600); each arming draws the delay uniformly from `delay ± delay_variance` (floored at 0) from a stream seeded by the agent `seed`. |

Conditions on the transition (`blackboard_condition` nodes on its `conditions` port) must
all hold. `parent` needs a parent (a top-level state has none) and `next_sibling` needs a
later sibling; the compiler checks both.

**Order of evaluation.** Candidates are the transitions of all active states. They are
sorted by `priority` (high first), then by the owner's depth (deeper state first), then by
authoring order, and the first one that is ready fires; ties are therefore stable.

**Delay.** The delay timer arms the first time the trigger holds with its conditions
satisfied and runs on the accumulated `dt` of the following ticks: the transition fires on
the tick the elapsed time reaches `delay` (with 1 µs tolerance), so `delay: 0.5` at
1/60 s ticks fires 30 ticks after arming and `delay: 0.1` at 0.1 s ticks one tick after.
If the trigger or a condition stops holding the timer resets. An `on_event` transition
latches its event when it arms, so the event is not needed during the delay.

**Events.** `on_event` reacts to events delivered to the agent entity (including engine
events such as `navigation.arrived`). One event instance is consumed by the transition
that fires on it; unconsumed events are dropped at the end of the tick. Each instance
holds at most 32 pending events and ignores names no transition listens for.

**Firing.** Exit order is child before parent, enter order parent before child, each
exactly once per transition. States that are ancestors of the target and already active
stay active and are not re-entered; the target itself (even if it is already active, as in
a self transition) and everything below it is exited and re-entered. After a transition
fires the evaluation repeats within the same tick (new states may complete at once or
carry `on_tick` transitions), without ticking tasks or evaluators again, at most 8 times
per tick; a longer chain continues next tick. `succeeded`/`failed` exits every active
state, stops native evaluators and finishes the tree.

A finished tree stays finished until the agent is re-enabled or changed.

## Blackboard

Document variables declare the blackboard keys (types `bool`, `int`, `float`, `string`,
`vec3`, `entity`; at most 128 keys) exactly as for behavior trees; `engine.StateTreeAgent.blackboard`
overrides the initial values. Native code and perception systems may write the live
blackboard (`StateTrees::blackboard_mut`); transitions see the writes on the agent's
next tick.

### Host inputs

`copy_evaluator.input` reads a named value from the host. The runtime provides:

| Input | Type | Value |
| --- | --- | --- |
| `self.position` | `vec3` | World position of the agent entity. |
| `time.seconds` | `float` | Simulated seconds since the runtime started. |

An unknown name is a scene validation error (`state_tree.input.unknown`).

## The component

`engine.StateTreeAgent` version 1 data:

```json
{"graph": "guard-mood", "blackboard": {"alert": false}, "tick_interval": 0, "enabled": true, "seed": 7, "navmesh": "level-nav"}
```

| Field | Meaning |
| --- | --- |
| `graph` | Required. Id of a `graph` asset of kind `state_tree`. |
| `blackboard` | Initial values by key; each must fit the graph variable's type. |
| `tick_interval` | Seconds between tree ticks (0-3600). `0` ticks every runtime tick; otherwise the tree ticks once at least that much time has accumulated and receives the accumulated time as its `dt`. |
| `enabled` | A disabled agent runs nothing; its active states are exited. Enabling starts the tree afresh. |
| `seed` | Mixed into the delay-variance streams, so agents sharing a graph differ. |
| `navmesh` | `navigation` asset `move_to` uses when the entity's `engine.NavigationAgent` names none. |

Project validation (`Project::load`, `component_set`) checks that the graph asset exists
and compiles as a state tree, that blackboard overrides fit the variables, that every
host input exists and, for a tree containing `move_to`, that the entity has
`engine.NavigationAgent` and a navmesh is available. Changing any field except
`tick_interval` while the game runs restarts that agent's tree. The component holds
authored data only; live state is in the runtime service, so `try_snapshot` never changes it.

### Tick placement and navigation

The state tree service runs each runtime tick **after** native scripts, ECS `Update`
systems, event dispatch and the behavior tree service, and **immediately before**
`simulation::step`. A `move_to` task edits the entity's `engine.NavigationAgent`
(`navmesh`, `target`, `waypoints` cleared, `repath` incremented) and the navigation
service routes it in the same tick. `navigation.arrived` completes the task with success
and `navigation.failed` with failure; both reach the tree one tick later because events
are delivered at the start of the next tick. Author the `engine.NavigationAgent` of a
tree-driven entity without `navmesh` and `target`. Do not drive the same entity's
navigation from a behavior tree and a state tree at once.

## Events

Events target the agent entity and are delivered on the next tick through `EventBindings`
and `register_event_handler`, like all engine events.

| Event | Payload | When |
| --- | --- | --- |
| `state_tree.state_entered` | `{graph, state}` | A state was entered (parent before child). |
| `state_tree.state_exited` | `{graph, state}` | A state was exited (child before parent); also emitted when the agent is disabled or rebuilt. Not emitted at runtime shutdown or when the agent's entity was despawned. |
| `state_tree.finished` | `{graph, status: "success"\|"failure", ticks}` | The tree completed. Once per run. |
| your event | the task's `payload` | `emit_event` tasks. |

`state` is the graph node id. The queue holds 1024 events, so emission is bounded: per
agent per tick at most 8 enter/exit events (the rest are dropped, in order, after the
first 8), 8 `emit_event` events and 1 `finished`; at most 64 events across all state tree
agents per runtime tick; and when the queue is full the rest are dropped instead of
failing the tick. Every dropped event is counted in the telemetry (`dropped_events`).
Within a tick, state events come first (in the order they happened), then task events,
then `finished`.

## Native API

A native task or evaluator is game code a graph names. Both implement the same trait
(`NativeTask<World>`, shared with behavior trees) and are registered in separate
namespaces before the runtime starts:

```rust
use bevy_ecs::prelude::*;
use engine_project::ai::{
    NativeTask, TaskContext, TaskStatus, register_state_tree_evaluator, register_state_tree_task,
};

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

register_state_tree_task(runtime.app_mut(), "guard.chase", chase)?;
register_state_tree_evaluator(runtime.app_mut(), "guard.sense", sense)?;
```

- Tasks: `start` when the state is entered (a task may finish right there by returning
  `Success`/`Failure`); while `Running`, `tick` on every following tree tick; `abort` if
  the state is exited, the agent disabled or removed, or the runtime stops, before the task
  completed. A task that completed never receives `abort`.
- Evaluators: `start` on the tree's first tick, `tick` on every later tick before
  transitions; `abort` when the tree finishes, is disabled, removed or the runtime stops.
- One instance exists per agent and graph node and is reused across activations.
- A name used by a graph but not registered fails `Runtime::start` with
  `state_tree.native.unregistered`; a factory error fails the tick with
  `state_tree.native.failed`; registering a name twice returns
  `state_tree.native.duplicate`.

## Telemetry

`RuntimeSnapshot.state_trees` (and the `state_trees` member of the MCP
`project_run_headless` summary) is absent without agents, otherwise:

```json
{"total": 1, "dropped_events": 0, "agents": [
  {"entity": "guard", "graph": "guard-mood", "enabled": true, "ticks": 140,
   "phase": "running", "last_finished": null, "active_path": ["patrol", "leg_a"],
   "transitions": [{"tick": 31, "event": "transition", "id": "alarmed", "to": "chase"}],
   "blackboard": {"alert": false, "post": [8.0, 0.0, 1.0]}}]}
```

`phase` is `idle`, `running`, `{"finished": "success"|"failure"}`, `stopped`, or `null`
while the agent is disabled. At most 32 agents (by authored id), 16 path states, the 8
most recent fired transitions (`id` is the transition node, `to` the leaf state entered,
absent when the tree completed) and 16 blackboard keys per agent (strings cut to 64
characters). The debug view of a `StateTreeInstance` (`debug()`) adds every state's
activity and latched result, accumulated tree time and the log of the last 64 enters,
exits, transitions and completion.

## Limits

| Limit | Value |
| --- | --- |
| Nodes per tree | 1024 |
| State nesting depth | 32 |
| Child states per state (and top-level states) | 64 |
| Tasks per state | 16 |
| Transitions per state | 32 |
| Conditions per state or transition | 16 |
| Evaluators per tree | 32 |
| Blackboard keys | 128 |
| Pending events per agent | 32 |
| Transitions fired per tick | 8 |
| Transition log | 64 entries |
| Agents per runtime | 512 |
| Blackboard overrides per agent | 64 |
| State events per agent per tick | 8 |
| State tree events per runtime tick | 64 |

## Determinism

A tree is a pure function of the document, the agent `seed`, the blackboard writes, the
events sent and the `dt` sequence. All maps are ordered (`BTreeMap`), delay variance comes
from `SplitMix64` streams derived from the seed and the transition, time is accumulated
from `dt` and never read from a clock, and agents tick in entity order. Two runs of the
same headless project give identical event sequences and telemetry (tested with a
navigation patrol on a baked navmesh that idles on an emitted event).

## Compiler diagnostics

Besides the generic `graph.*` codes ([Graph documents](ai-graphs.md#validation)), the
state tree check reports:

| Code | Meaning |
| --- | --- |
| `st.root.empty` | The root has no top-level state. |
| `st.root.evaluators` | More than 32 evaluators. |
| `st.state.children` | More than 64 child states. |
| `st.state.tasks`, `st.state.transitions` | More than 16 tasks or 32 transitions on a state. |
| `st.conditions.limit` | More than 16 conditions on a state or transition. |
| `st.tree.depth` | State nesting deeper than 32. |
| `st.state.unreachable` | No selection or transition from a reachable state can enter the state (a transition owned by an unreachable state does not count). |
| `st.transition.target_unknown` | `state` names a node that is not a state of this tree. |
| `st.transition.target` | `target: state` without `state`, `state` with another target, `parent` on a top-level state, or `next_sibling` on the last sibling. |
| `st.transition.event` | `on_event` without `event`, or `event` with another trigger. |
| `st.condition.value` | A comparison operator without a value, or a value of the wrong type. |
| `st.condition.operator` | An ordering operator on a key that is not int or float. |
| `st.value.type` | `set_blackboard` value does not fit the key's type. |
| `st.move_to.target` | `move_to` with both or neither of `target` and `key`. |
| `st.event.name`, `st.event.payload` | Invalid event name, or a payload that is not an object. |
| `st.native.name` | A native name with characters outside letters, digits, `_`, `-`, `.`, `:`. |
| `st.evaluator.invalid` | `copy_evaluator` without exactly one source or with mismatched types; `compute_evaluator` with operands or destination of the wrong types. |

A hierarchy cycle is the generic `graph.cycle`. Component and runtime codes:
`state_tree.graph.missing`, `.graph.kind`, `.blackboard.invalid`, `.tick_interval.invalid`,
`.navigation.missing`, `.input.unknown`, `.native.unregistered`, `.native.duplicate`,
`.native.failed`, `.unavailable`, `.capacity`.

## Authoring over MCP

`graph_schema` lists the node types; `graph_validate` checks a graph; `graph_edit` builds
and changes it atomically (see [Graph documents](ai-graphs.md#mcp-tools)). The first
`graph_edit` batch for a new state tree must be complete: the root, at least one state
connected to `root.children`, and everything referenced by transitions. Attach the tree
with `component_set` on `engine.StateTreeAgent`; `component_schema` returns its schema and
defaults. `asset_info` lists the entities using the graph. `project_run_headless` reports
the telemetry above. No state-tree-specific tools exist.

Focused validation: `cargo test -p engine-ai`, `cargo test -p engine-project --test state_tree`,
`cargo test -p engine-mcp --test state_tree`.
