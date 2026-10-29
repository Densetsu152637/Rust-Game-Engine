# Scripting

Gameplay has two layers. **Native Rust game crates** are the primary, trusted way to
write gameplay: the engine compiles the crate together with the engine into a native
executable and gameplay runs as ordinary Rust against the same Bevy ECS world. On top
of that, **data-driven scripts** are additive and optional: visual scripts
([`script_graph`](visual-scripting.md), shipped) run in a bounded, deterministic VM,
and [Rhai text scripts](#text-scripts-rhai) (shipped, `text-scripting` feature, on by
default) run in a restricted Rhai engine. Both reach the game only through declared
host functions and explicit capabilities. There is no Lua: the earlier embedded Lua
scripting was removed with the C++ engine and nothing here brings back an unbounded
interpreter.

| | Native game crates | `script_graph` visual scripts | Rhai text scripts |
| --- | --- | --- | --- |
| Trust | Trusted code, the user's process privileges | Bounded by construction (not a defence against a malicious project, which can already edit native code) | Restricted and budgeted, with a documented [isolation gap](#what-is-not-isolated) |
| Authoring | Rust, compiled into a generation | JSON graph asset edited with `graph_edit` or the node editor | `.rhai` source, checked with `script_check` |
| World access | The whole `World` | Declared host functions, staged built-ins, capabilities | The same host functions, staging and capabilities |
| Reload | New generation, restarts game state | `Runtime::reload_script_asset`, last good program kept on failure | The same call, by `text_script` asset id |

## Shared script core

`engine-script` is a pure crate (no ECS, file system, network or clock) shared by
script backends:

- **Values**: `bool`, `int` (`i64`), `float` (`f64`, rounded to `f32` at vec3,
  transform and component boundaries), `string` (at most 4096 bytes), `vec3`,
  `entity` (a runtime handle, never persisted) and `json` (within the shared graph
  value limits).
- **Host functions** have dotted lower-case names and a declared signature
  (`params`, `returns`, `pure`, `cost` of at least 1 fuel, `effects`). The engine
  provides the reserved `engine.*` built-ins (`engine.find_entity`, `position`,
  `set_position`, `component_get`, `component_set`, `emit`, `log`, `random_int`,
  `random_float`, `time`, `self`). A game declares its own in the project's
  **`scriptFunctions`** file (a project-relative JSON array, at most 1 MiB, set in
  `project.json` like `componentSchemas`):

  ```json
  [{"name": "game.damage", "params": [{"name": "target", "type": "entity"}, {"name": "amount", "type": "float"}],
    "returns": null, "pure": false, "cost": 5, "effects": ["write_self"]}]
  ```

  and registers the Rust implementation with
  `engine_project::scripting::register_script_fn(app, "game.damage", sig, f)` from
  `configure`. `Runtime::start` fails with `script.function.unregistered` or
  `script.function.signature` when a declaration has no matching implementation.
  Every argument is checked (type, finiteness, string and JSON bounds) before the
  native function runs, so invalid script data never reaches game code.
- **Capabilities** are per `engine.ScriptGraph` or `engine.TextScript` component and denied by default:
  `read` and `write` list component names `engine.component_get` and
  `engine.component_set` may touch on the script's own entity (writes must pass the
  component's schema; `engine.set_position` is denied on an entity whose pose is
  owned by physics, animation or navigation), `emit` is `self_only` or `any`, and
  `spawn` is reserved (maximum 0: runtime spawn is not shipped).
- **Budgets**: fuel per instance and tick (default 10,000, authorable
  100..1,000,000, 2,000,000 across all instances), guest call depth 16, instance
  state 64 KiB, 64 pending waits, 16 emits per instance and 256 in total per tick.
  A violation traps the run with a coded `script.*` error.
- **Staging**: built-in world writes and events are committed only when a run
  returns normally; every component write is prepared and checked before any
  staged world write is applied, so a late native decode error leaves earlier
  staged writes untouched. A trap discards them. Native game functions are trusted and
  apply their effects immediately (document the irreversible ones).
- **Faults**: a trap records its node, emits a targeted `script.faulted` event and
  applies the component's `fault_policy`: `disable` (default; the instance stops)
  or `stop_runtime` (the tick returns the error). Faults are listed in the
  `scripts` field of the runtime snapshot.
- **Determinism**: instances run in authored-id order, events in delivered order,
  randomness only from the authored `seed`, time only from accumulated `dt`. The
  same build, platform and inputs give the same event trace and telemetry.
- **Events**: scripts handle events delivered to their entity (or broadcast)
  through `on_event` nodes, for example `collision.enter`, and emit events that are
  delivered on the next tick through the normal bounded dispatcher.
- **Tick placement**: the script service runs in the post-update step immediately
  before the AI services, after event dispatch and native script updates.
- **Reload**: `Runtime::reload_script_asset(id)` compiles and checks a candidate
  against every affected instance's variable overrides before replacing anything.
  On success affected instances stop (`on_stop`) and restart fresh at the next tick
  boundary; on failure the last good program and instance state keep running. If
  component overrides change before that boundary and reject the queued candidate,
  the reload is cancelled and recorded in the runtime log. File
  watching is out of scope.
- **MCP**: `script_schema` describes the contract (node types, host functions,
  limits, codes and the text backend), `graph_validate` applies the project's
  declared functions when a project is open and `script_check` parses a `.rhai`
  source. None of them executes scripts or game code.

## Text scripts (Rhai)

A **`text_script`** asset is a `.rhai` source file (kind `text_script`; other
extensions are rejected with `project.script.language`) run by the
**`engine.TextScript`** component. The backend is [Rhai](https://rhai.rs) pinned to
`=1.26.1` inside `engine-script` behind the `text` feature, forwarded as the
`engine-project` feature `text-scripting` (default on; a game may build with
`--no-default-features`). With the feature off the component and the asset kind keep
their data shape, but nothing parses or runs a source: validating a project that has a
`text_script` asset, or starting a world that has the component, fails with
`script.text.disabled`.

`engine.TextScript` has the fields of `engine.ScriptGraph` except `graph` and
`variables`: `script` (asset id, required), `seed`, `enabled`, `fuel_per_tick`
(100..1,000,000, default 10,000), `capabilities` (`read`, `write`, `spawn` = 0, `emit`)
and `fault_policy`. Project validation checks the shape, that the asset exists with kind
`text_script`, that capability names are catalog components and that the source parses.
Nothing is run to validate: top-level statements in a source are never evaluated (only
function definitions and a literal `events` list are kept), so a load, a `script_check`
or a reload cannot execute script code.

### The script contract

```rhai
const events = ["collision.enter"];          // static subscriptions (literal strings)

fn on_start()                      { this.hits = 0; engine::after(2.0, "tick"); }
fn on_update(dt)                   { }
fn on_event(name, payload, source) { this.hits += 1; game::chime(3); }
fn on_stop()                       { }
fn tick()                          { engine::emit("game.hit", #{ n: this.hits }, engine::self()); }
```

- Handlers are optional, but one that is present must take exactly the parameters shown
  (`script.text.handler_missing`), and a non-empty `events` list requires `on_event`.
  `events` must be a literal array of at most 64 valid, distinct event names
  (`script.text.events`). Only those events are delivered, targeted at the entity or
  broadcast, at most 64 per tick.
- Per tick an instance runs `on_start` once (first tick), its events in delivered order,
  due timers, then `on_update(dt)`. `on_stop` runs when the component is removed,
  replaced, reloaded or the runtime stops (not for a despawned entity).
- **State** lives in the object map `this`, which persists between handlers; local
  variables do not. Persistent state that is too large, or nested deeper than 8 levels
  or past 512 values, is discarded and the run traps with `script.memory.limit`.
- **Host calls** map a dotted host name to a Rhai namespace: `engine.emit` is
  `engine::emit(..)`, a declared `game.damage` is `game::damage(..)`. The names are the
  built-ins of the shared core plus the project's `scriptFunctions`. Arguments are
  converted by declared type (`int` to `i64`, `float` to `f64`, `string`, `vec3` as an
  array of three floats, `json` from maps, arrays and scalars of at most 8 levels and 512
  values, `entity` as an opaque handle) and checked before the native function runs.
  `()` is the null entity, so `engine::emit(name, payload, ())` broadcasts when
  `capabilities.emit` is `any`. A script cannot build any other entity handle; it gets
  them from `engine::self()` and `engine::find_entity(id)`.
- **Timers**: `engine::after(seconds, "fn_name")` runs a zero-parameter script function
  after `seconds` (0..3600) of accumulated `dt`; at most 64 are pending per instance.
  Timers that come due in one tick free their slots before their callbacks run.
- `print` and `debug` accept only strings and write to the runtime log (prefix
  `script: `). Nothing reaches stdout, so an MCP server's protocol stream stays clean.
- There is no `import`, `eval`, module, file system, network, process, clock or ambient
  randomness (use `engine::random_int`, seeded from `seed`, and `engine::time`).

### Engine configuration

`Engine::new_raw()` with only these packages: arithmetic, basic iterator, basic array,
basic map and logic. The core language and its built-in operators are available
(including string concatenation and comparison, and `type_of`), but the standard string,
time, debugging and math-extension packages are not, so `"abc".len()` or `to_string(1)`
is an unknown function (`script.host.failed`). Rhai crate features:
`std, sync, no_time, no_custom_syntax, no_optimize, only_i64` (integers are `i64`).
`unchecked` and `internals` must never be enabled (the adversarial tests would hang or
fail), and `import` and `eval` are disabled symbols. `rhai` is a locked, offline
dependency; upgrading it is a reviewed change.

| Limit | Value | Reported as |
| --- | --- | --- |
| Source size | 256 KiB, UTF-8 | `script.text.parse` |
| Fuel per instance and tick | 10,000 default, 100..1,000,000; 2,000,000 per tick across **all** script instances, graphs included | `script.fuel.exhausted` |
| Script call depth | 16 | `script.call.depth` |
| Expression nesting | 64 globally, 32 inside functions | `script.text.parse` |
| String size | 4096 bytes (also counted per container) | `script.memory.limit` |
| Array and map size | 512 elements each, counting nested containers | `script.memory.limit` |
| Variables and functions | 128 and 128 | `script.memory.limit`, `script.text.parse` |
| Pending timers | 64 per instance | `script.memory.limit` |
| Persistent state | 64 KiB, depth 8, 512 values | `script.memory.limit` |
| Worker native stack | 8 MiB per handler call | n/a |

Every handler call runs on a short-lived scoped worker thread with its own native stack
(Rhai recurses on the native stack). The host thread blocks on a rendezvous channel and
serves each host call, fuel charge and log line, so no `World` reference or channel
outlives a call.

### Traps

A trap stops the run, discards its staged writes and events, records a fault
(`script.faulted`, telemetry, runtime log) and applies `fault_policy`. A faulted instance
does not run again until its component or asset is replaced or reloaded.

| Code | Text-script cause |
| --- | --- |
| `script.fuel.exhausted` | Operations or host-call cost used up the tick's or the global fuel; any loop form |
| `script.memory.limit` | String, array or map growth or nesting past the engine limits, too many variables, state too large or deep, a payload past the bridge limits, more than 64 pending timers |
| `script.call.depth` | More than 16 nested script calls, including through function pointers |
| `script.host.argument` | A host argument of the wrong type, non-finite, oversize or an invalid handle (the native function is not called), or a bad timer delay |
| `script.host.failed` | A host function failed, or any other Rhai runtime error: unknown function or namespace, wrong operand types, arithmetic errors |
| `script.capability.denied` | A read or write outside `capabilities`, a non-self target, a pose-owned `set_position`, or an emit scope violation |
| `script.value.nonfinite` | A non-finite number reached a boundary |
| `script.event.limit` | More than 16 emits per instance or 256 in total per tick |

Source and project diagnostics, reported before anything runs:

| Code | Meaning |
| --- | --- |
| `script.text.parse` | Syntax error (with `line:col`), oversize or non-UTF-8 source, a disabled `import` or `eval`, an over-large literal, too many functions |
| `script.text.handler_missing` | A handler with the wrong parameter count, or `events` without `on_event`; as a trap, a timer naming a missing or parameterised function |
| `script.text.events` | `events` is not a literal array of at most 64 distinct valid event names |
| `script.text.disabled` | The build was compiled without `text-scripting` |

### Reload, determinism and telemetry

`Runtime::reload_script_asset(id)` accepts a `text_script` asset id: the file is re-read,
compiled and checked against every affected instance. On failure the last good program
keeps running and the error is returned. On success affected instances run `on_stop`
and restart fresh (no state migration) at the next tick boundary, which also recovers an
instance that had faulted. Text instances run after the graph instances in authored-id
order and share their per-tick global fuel budget. The same build, platform and inputs
give the same events and telemetry. Telemetry lists text instances in the `scripts`
snapshot field with `graph` set to the script asset id, fuel used, state bytes, pending
timers and the fault (code and, when Rhai knows it, a `line:col` site).

### What is not isolated

- There is **no global heap cap**. Rhai's size limits bound each value (counting nested
  elements), not total memory, and they are checked after an operation produces its
  value. The limits above are the control, backed by the adversarial tests; memory is
  not measured with an allocator in tests because the workspace forbids `unsafe`.
- Fuel counts Rhai operations and declared host cost, not wall-clock time. A single Rhai
  builtin is not interruptible mid-call, so builtins on 512-element values are the
  longest uninterrupted work.
- Rhai recurses natively for nested calls, expressions and nested values. Depth is
  bounded by the limits above, and the worker stack (8 MiB) leaves a 4x margin over the
  deepest value they admit (511 alternating array and map levels needed more than 1 MiB
  in a debug build). That is a measured bound, not a proof.
- Native game functions registered with `register_script_fn` are trusted and apply their
  effects immediately; a panic in one propagates. Rhai is a third-party parser and
  evaluator inside the process.
- This is robustness and defence in depth against buggy scripts, not isolation from a
  malicious project: project writers can already edit native Rust. Hostile content (mods
  or user-generated scripts in a shipped game) is not a goal; a WebAssembly interpreter
  is the documented option to revisit for that.

### Checking scripts without running them

The MCP read tool **`script_check`** (see [MCP](mcp.md)) takes an `asset`, a `path` or
inline `source` and returns coded diagnostics with `line:col`, the functions the source
defines and its event list. `script_schema` returns the text contract: component schema,
handler list, calling convention and limits.

## Native game crates (trusted)

The engine compiles the game crate together with the engine into a native
executable, and gameplay runs as ordinary Rust against the same Bevy ECS world the
engine uses. Native code is trusted executable code with the user's normal process
privileges.


Working examples: [`examples/first-project`](../examples/first-project/src/lib.rs)
(script lifecycle, game-owned component, systems), [`examples/desktop-2d`](../examples/desktop-2d/README.md)
(a complete 2D game controller), [`examples/native-extension`](../examples/native-extension/README.md)
(custom components and a custom wgpu render pass) and
[`assets/scripts/moving_entity`](../assets/scripts/moving_entity/src/lib.rs)
(the smallest script crate).

## Crate shape

The game crate is a library next to its `project.json`. It is its own workspace root
(an empty `[workspace]` table) so the engine workspace does not build it, and it
depends on the engine crates by path:

```toml
[package]
name = "my-game-gameplay"
version = "0.1.0"
edition = "2024"

[workspace]

[dependencies]
engine-core = { path = "../../crates/engine-core" }
engine-project = { path = "../../crates/engine-project" }
# engine-desktop = { path = "../../crates/engine-desktop" }  # for render passes
```

The library exports `configure`, which the generated runtime entry calls with the
`EngineApp` before the project starts:

```rust
pub fn configure(app: &mut engine_core::EngineApp) -> engine_core::Result<()>
```

`configure` registers whatever the game needs: Bevy components, resources,
bundles, system sets and systems on `app.schedule_mut(Stage::...)`, render
extraction (`app.extract_component::<T>()`), render passes and script factories.
Components do not need serialization or reflection registration. See
[Architecture](architecture.md) for the scheduling model.

## Linking the crate to a project

The manifest selects the crate with an optional `"rust": {"manifest": "Cargo.toml"}`
entry; without it the default is an adjacent `Cargo.toml`. An explicit selection must
exist inside the project root. Script assets in the manifest have kind `script` and
must name a `.rs` file; they remain stable asset IDs that scenes reference.
`Project::rust_manifest()` resolves the manifest for the compiler. See
[Project format](project-format.md).

```json
{
  "schema": 1,
  "name": "First Project",
  "rust": { "manifest": "Cargo.toml" },
  "startup_scene": "scenes/main.scene.json",
  "assets": [
    { "id": "asset:player-script", "path": "src/lib.rs", "kind": "script" }
  ]
}
```

## Entity scripts

A scene entity with a `Script` component names a script asset ID. Register a
factory under that ID with `register_script(app, "asset:player-script", factory)`;
a factory is a plain `fn(&mut World, Entity) -> Result<Box<dyn NativeScript>>` and
receives the authored owner entity. A scene that references an unregistered script
fails startup. Registration after the runtime has started is rejected.

```rust
use engine_core::bevy_ecs::prelude::{Entity, World};
use engine_project::{NativeScript, Transform, register_script};

pub fn configure(app: &mut engine_core::EngineApp) -> engine_core::Result<()> {
    register_script(app, "asset:player-script", |_, _| Ok(Box::new(Player)))?;
    Ok(())
}

pub struct Player;
impl NativeScript for Player {
    fn on_update(&mut self, world: &mut World, owner: Entity, dt: f32) -> engine_project::Result<()> {
        if let Some(mut transform) = world.get_mut::<Transform>(owner) {
            transform.position[0] += dt;
        }
        Ok(())
    }
}
```

`NativeScript` has three callbacks, all defaulting to no-ops: `on_create`,
`on_update(dt)` and `on_destroy`. `Runtime::start`, `tick(dt, InputSnapshot)` and
`stop` invoke them. Callbacks are lifecycle integration for authored entities, not
a second ECS scheduler; frequent per-entity work belongs in ordinary Bevy systems.

Ownership and teardown:

- Entities spawned during a callback automatically receive the `OwnedBy(owner)`
  component. Systems that run outside callbacks use `runtime::spawn_owned(world,
  owner, bundle)` explicitly.
- Teardown attempts every script's `on_destroy`, then clears owned entities, UI,
  control locks, queued scenes and audio. Errors and panics from factories and
  callbacks are preserved, not swallowed.
- A scene transition validates destination data, bindings, colliders and safe
  spawn before retiring the old scene. A retained traveller keeps its `Entity` and
  custom components.
- Native effects are not sandboxed or reversible. A callback error stops the
  runtime generation; it cannot roll back arbitrary Rust side effects. Validate and
  stage changes before publishing when atomic behavior matters.

## Runtime services

`engine_project::runtime` exposes world-based helpers that go through the same
authoritative ECS state as the engine: `find_entity`, `move_entity` (swept
collision), `safe_position`, `request_scene` (validated, applied at tick
completion), `resolve_asset` (confined to the project catalog),
`gameplay_input`/`refresh_gameplay_input`, plus resources such as `ActionFrame`,
`InputSettings`, `ControlLocks`, `GameState`, `RuntimeLog`, `TriggerEvents` and
`Persistence`. After opening or closing UI or changing control locks inside one
callback, read `runtime::gameplay_input(world)` to recompute input capture
immediately. Native scheduled systems should chain UI/control writers,
`refresh_gameplay_input`, and gameplay readers with ordinary Bevy ordering edges.

## Compilation and reloading

The tooling compiler runs Cargo/rustc on the game crate with the engine, captures
Cargo's JSON diagnostics and produces a unique executable per generation. A failed
candidate never replaces a working generation, and the candidate is built and
ready before the previous process is stopped. Replacing a script means compiling
and launching a new generation, which restarts game state; in-process hot patching
and state migration across generations are not provided. See
[Project commands](commands.md) and `crates/engine-tooling/README.md`.

An intentionally uncompilable crate lives in
[`examples/first-project/fixtures/invalid`](../examples/first-project/fixtures/invalid)
to exercise diagnostics and last-good retention.

## Typed components and event callbacks

Gameplay `configure` can register typed authored ECS components with
`register_component::<T>` and named callbacks with `register_event_handler`.
`NativeScript::on_event` receives queued events; collider events and native
`emit_event` use the same bounded next-tick dispatcher. See
[metaprogramming](meta-programming.md) for complete contracts and examples.
