# Visual scripting (`script_graph`)

`script_graph` is a graph kind of the shared [graph model](ai-graphs.md) for
gameplay logic that designers and tools edit as data: entry nodes that react to the
runtime, structured flow, typed values and calls to declared host functions. The
document compiles to a bounded register VM (`engine_script::graph`) that is
deterministic, never touches the file system, network, clock or stdout, and stops a
run that exceeds its budget. It is the visual front end of the
[shared script core](scripting.md#shared-script-core).

Native Rust game crates stay the primary, trusted way to write gameplay; a script
graph is additive. Game code the graph needs (damage, inventory, anything native) is
exposed as a **host function** with a declared signature, see
[Host functions](#host-functions).

## Model

* A graph has **entry nodes** (`on_start`, `on_update`, `on_event`, `on_stop`) and
  optional **functions** (`function_entry`). Each entry owns a *flow region*.
* **Flow edges** (`flow` ports `exec` -> `then`, `true`/`false`, `body`/`completed`)
  form a **tree**: a flow node has exactly one flow parent and each flow out port has
  one edge (`sequence.then` takes many, run in edge order). There are no merges and
  no back edges; loops are structural nodes (`for_range`, `while`). Share code with a
  function instead of merging flows.
* **Data edges** carry typed values: `bool`, `int`, `float`, `string`, `vec3`,
  `entity` and `json` (the custom port type `script.json`). `int` widens to `float`.
  A data input that is not connected reads a node parameter where one exists
  (`for_range`, `wait`, `gate`) or the type's zero value (extra ports of calls).
* **Value nodes** (`add`, `compare`, `get_var`, ...) have no flow ports. They are
  evaluated when a flow node reads them, every time it reads them (no caching across
  nodes), so `get_var` always sees the variable's current value.
* A data input may read: a value node, an output of its own entry, a loop index
  inside the loop's `body`, or the output of a flow node above it in the tree (a
  call result). Anything else is `script.graph.value_unavailable`.
* **Variables** are per instance, typed (`bool`, `int`, `float`, `string`, `vec3`,
  `entity`) and initialised from `default` (a component may override them).
  Variables are not shared between instances (graph-shared variables are deferred).
* `wait` is latent: the run saves its registers and resumes after the accumulated
  `dt` of later ticks reaches the requested seconds (0..3600). Up to 64 waits may be
  pending per instance. `wait` is not allowed inside functions.
* Functions: `function_entry` (`name`) with extra data **out** ports as parameters,
  `return` with extra data **in** ports as results, `call_function` with matching
  extra ports. Recursion is rejected at validation; the guest call depth is also
  limited to 16 frames at run time.

Extra ports are `{name, direction, type, multiple}` like any port; a `json` port has the type `{"custom": "script.json"}`, and a data out port that feeds several readers needs `"multiple": true`. Write the full port list, including every unchanged declared port, with the `set_ports` graph edit (or `add_node.ports`) on
the four *extensible* node types: `function_entry`, `return`, `call_function` and
`call`. Declared ports are always kept.

## Example

The tagged block below is compiled by `crates/engine-script/tests/docs.rs`.

```json script_graph
{
  "version": 1,
  "kind": "script_graph",
  "variables": [{ "name": "hits", "type": "int", "default": 0 }],
  "nodes": [
    { "id": "ev", "node_type": "on_event", "params": { "event": "collision.enter" } },
    { "id": "get", "node_type": "get_var", "params": { "variable": "hits" } },
    { "id": "one", "node_type": "const", "params": { "type": "int", "value": 1 } },
    { "id": "add", "node_type": "add", "params": { "type": "int" } },
    { "id": "set", "node_type": "set_var", "params": { "variable": "hits" } },
    { "id": "limit", "node_type": "const", "params": { "type": "int", "value": 3 } },
    { "id": "cmp", "node_type": "compare", "params": { "type": "int", "op": "ge" } },
    { "id": "br", "node_type": "branch" },
    { "id": "open", "node_type": "emit_event", "params": { "event": "game.door.open", "payload": { "by": "script" } } },
    {
      "id": "chime",
      "node_type": "call",
      "params": { "function": "game.chime" },
      "ports": [
        { "name": "exec", "direction": "in", "type": "flow", "multiple": true },
        { "name": "then", "direction": "out", "type": "flow", "multiple": true },
        { "name": "n", "direction": "in", "type": "int" }
      ]
    }
  ],
  "edges": [
    { "from": { "node": "ev", "port": "then" }, "to": { "node": "set", "port": "exec" } },
    { "from": { "node": "get", "port": "value" }, "to": { "node": "add", "port": "a" } },
    { "from": { "node": "one", "port": "value" }, "to": { "node": "add", "port": "b" } },
    { "from": { "node": "add", "port": "result" }, "to": { "node": "set", "port": "value" } },
    { "from": { "node": "set", "port": "then" }, "to": { "node": "br", "port": "exec" } },
    { "from": { "node": "get", "port": "value" }, "to": { "node": "cmp", "port": "a" } },
    { "from": { "node": "limit", "port": "value" }, "to": { "node": "cmp", "port": "b" } },
    { "from": { "node": "cmp", "port": "result" }, "to": { "node": "br", "port": "condition" } },
    { "from": { "node": "br", "port": "true" }, "to": { "node": "open", "port": "exec" } },
    { "from": { "node": "open", "port": "then" }, "to": { "node": "chime", "port": "exec" } },
    { "from": { "node": "get", "port": "value" }, "to": { "node": "chime", "port": "n" } }
  ]
}
```

On each `collision.enter` event the script counts the hit, and from the third hit on
emits `game.door.open` (delivered next tick) and calls the game function
`game.chime(n)`.

## Node reference

Flow nodes have an `exec` in port and (unless noted) a `then` out port. Entry nodes
have only `then`. "Value" nodes have only data ports.

| Node | Kind | Ports and parameters | Behavior |
| --- | --- | --- | --- |
| `on_start` | entry | out `then` | Runs once when the instance starts. |
| `on_update` | entry | out `then`, `dt` (float) | Runs every tick; `dt` is the tick's seconds. |
| `on_event` | entry | param `event`; out `then`, `payload` (json), `source` (entity) | Runs when the named event is delivered to the entity. The event list is static: it is the instance's subscription. |
| `on_stop` | entry | out `then` | Runs once when the instance stops. Pending waits are dropped. |
| `function_entry` | entry | param `name`; extra data out ports | Entry of a reusable function; only reachable through `call_function`. |
| `branch` | flow | in `condition` (bool); out `true`, `false` | Continues down one side. |
| `sequence` | flow | out `then` (many) | Runs each `then` subtree in edge order. |
| `for_range` | flow | params/ins `start`, `end` (int); out `body`, `completed`, `index` (int) | Runs `body` for each index in `start..end`, then `completed`. A literal range over 100,000 is `script.graph.loop_bound`; fuel bounds the rest. |
| `while` | flow | in `condition`; param `max_iterations` (1..10,000); out `body`, `completed` | Repeats while the condition holds. Reaching the bound traps with `script.fuel.exhausted`. |
| `do_once` | flow | out `then` | Passes execution the first time this instance reaches it, never again. |
| `gate` | flow | in/param `open` (bool); param `latch`; out `then`, `closed` | `then` while open, else `closed`. With `latch`, once open it stays open. |
| `wait` | flow | in/param `seconds` (0..3600) | Latent wait, see Model. |
| `return` | flow | extra data in ports; no `then` | Ends a function, returning the extra ports in order. Outside a function it just ends the handler and takes no extra ports. |
| `set_var` | flow | param `variable`; in `value` | Stores the value; the write is checked against the 64 KiB state limit. |
| `call_function` | flow | param `function`; extra ports | Calls a function of this document. Extra in ports must equal the function's parameters, extra out ports its results. |
| `call` | flow or value | param `function`; extra ports | Calls a host function: extra in ports are its parameters (by name and type), the extra out port `result` its return value. A **pure** function has no flow edges and acts as a value node; any other function must be connected to the flow. |
| `log` | flow | in `message` (string) | Appends to the bounded runtime log; never stdout. |
| `emit_event` | flow | params `event`, `payload` (json); in `target` (entity) | Emits a game event, delivered next tick to the target or broadcast. At most 16 emits per instance per tick (`script.event.limit`). |
| `get_var` | value | param `variable`; out `value` | The variable's current value. |
| `self` | value | out `entity` | The entity the script runs on. |
| `const` | value | params `type`, `value`; out `value` | A typed constant (`bool`, `int`, `float`, `string`, `vec3`, `json`). |
| `add`, `sub`, `mul`, `div`, `min`, `max` | value | param `type` (`int`, `float`); in `a`, `b`; out `result` | Checked arithmetic: integer overflow and division by zero trap (`script.host.failed`), a non-finite float traps (`script.value.nonfinite`). |
| `abs`, `neg` | value | param `type`; in `a`; out `result` | Absolute value, negation. |
| `to_float` | value | in `a` (int); out `result` | Int to float. |
| `compare` | value | params `type`, `op` (`eq` `ne` `lt` `le` `gt` `ge`); in `a`, `b`; out `result` (bool) | Ordering needs `int`, `float` or `string`. |
| `and`, `or`, `not` | value | in `a`, `b` (bool); out `result` | Boolean logic (no short circuit: both sides are evaluated). |
| `vec3_make` | value | in `x`, `y`, `z`; out `result` | Rounds to `f32`; a non-finite component traps. |
| `vec3_break` | value | in `v`; out `x`, `y`, `z` | Splits a vector. |
| `vec3_add`, `vec3_sub` | value | in `a`, `b`; out `result` | Component-wise. |
| `vec3_scale` | value | in `v`, `s` (float); out `result` | Scalar multiply. |
| `vec3_dot`, `vec3_length` | value | in `a`, `b` / `v`; out `result` (float) | Dot product, length. |
| `concat` | value | in `a`, `b`; out `result` | Joins strings; a result over 4096 bytes traps (`script.memory.limit`). |
| `to_json` | value | param `type`; in `value`; out `result` | Wraps a value as json. |
| `json_to` | value | param `type`; in `value` (json); out `result` | Extracts a typed value; a mismatch traps (`script.host.argument`). |

## Host functions

A `call` node names a function declared in the project's `scriptFunctions` file (or
one of the built-in `engine.*` functions); its extra ports must match the signature
exactly. Declarations are data, so `script_schema` and `graph_validate` know them
without running game code. Implementations are trusted native Rust registered with
`engine_project::scripting::register_script_fn` before the runtime starts; every
argument is validated (type, finiteness, string <= 4096 bytes, json within the shared
limits) before the native function is invoked, so an invalid argument traps
(`script.host.argument`) and never reaches game code. See
[Scripting](scripting.md#shared-script-core).

Function registration and start-time codes:

| Code | Meaning |
| --- | --- |
| `script.function.invalid` | A declaration or registered name is malformed (dotted lower-case, 2 to 4 segments), a signature is inconsistent, or a game function uses the reserved `engine.` namespace. |
| `script.function.duplicate` | A function name is declared or registered twice. |
| `script.function.unregistered` | `Runtime::start`: a declared function has no registered implementation. |
| `script.function.signature` | `Runtime::start`: a registered implementation's signature differs from the declaration. |
| `script.spawn.limit` | Reserved: runtime spawn is not shipped, so the spawn capability maximum is 0. |

## Validation and codes

Validation never executes anything. The kind check (`graph_validate` without a
project) reports the generic graph codes and the codes below. With a project open the
project-aware check also reports `script.call.*` and
`script.capability.unknown_component`.

| Code | Meaning |
| --- | --- |
| `script.graph.entry_missing` | No `on_start`, `on_update`, `on_event` or `on_stop` node. |
| `script.graph.unreachable` | A flow node no entry reaches. |
| `script.graph.value_unavailable` | A data input reads a value that is not available at that point. |
| `script.graph.flow_fanout` | A flow out port with several edges (outside `sequence`), or a node with several flow parents. |
| `script.graph.loop_bound` | A literal `for_range` longer than 100,000. |
| `script.graph.latent_in_function` | `wait` inside a function. |
| `script.graph.function_unknown` | `call_function` names no function (or a function name is invalid). |
| `script.graph.function_duplicate` | Two functions share a name. |
| `script.graph.function_recursive` | A function calls itself, directly or indirectly. |
| `script.graph.function_ports` | Call, return or entry ports disagree. |
| `script.graph.event_name` | An event name is not lower-case dotted `[a-z0-9_]`. |
| `script.graph.const_type` | A `const` value does not match its type. |
| `script.graph.type_mismatch` | An operand has the wrong type (or an ordering op on a non-orderable type). |
| `script.graph.code_size` | The compiled script exceeds 65,536 ops or a frame 4,096 registers. |
| `script.call.unknown` | A `call` names an undeclared host function. |
| `script.call.ports` | A `call`'s ports or flow use disagree with the signature. |
| `script.capability.unknown_component` | A constant component name passed to `engine.component_get` or `engine.component_set` is not in the catalog. |

## Runtime limits and traps

Fuel is 10,000 ops per instance per tick by default (authorable 100..1,000,000);
host calls cost their declared `cost`. A trap stops the run, discards its staged
writes, records the node id, emits a targeted `script.faulted` event and applies the
component's `fault_policy`.

| Trap code | Cause |
| --- | --- |
| `script.fuel.exhausted` | Out of fuel, or a `while` reached `max_iterations`. |
| `script.memory.limit` | State over 64 KiB, a string over 4096 bytes, or more than 64 pending waits. |
| `script.call.depth` | More than 16 guest frames. |
| `script.host.argument` | A host argument or `json_to` value failed validation. |
| `script.host.failed` | A host function failed, or integer overflow or division by zero. |
| `script.capability.denied` | The component did not grant the capability. |
| `script.value.nonfinite` | A non-finite number reached an operation or boundary. |
| `script.event.limit` | More than 16 emits in one tick. |
| `script.variable.invalid` | Instantiation: an override names no variable or has the wrong type. |
