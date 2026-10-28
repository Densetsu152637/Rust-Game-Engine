# Scripting

Gameplay has two layers. **Native Rust game crates** are the primary, trusted way to
write gameplay: the engine compiles the crate together with the engine into a native
executable and gameplay runs as ordinary Rust against the same Bevy ECS world. On top
of that, **data-driven scripts** are additive and optional: visual scripts
([`script_graph`](visual-scripting.md), shipped) run in a bounded, deterministic VM
and reach the game only through declared host functions and explicit capabilities.
There is no Lua: the earlier embedded Lua scripting was removed with the C++ engine
and nothing here brings back an unbounded interpreter.

| | Native game crates | `script_graph` visual scripts |
| --- | --- | --- |
| Trust | Trusted code, the user's process privileges | Bounded by construction (not a defence against a malicious project, which can already edit native code) |
| Authoring | Rust, compiled into a generation | JSON graph asset edited with `graph_edit` or the node editor |
| World access | The whole `World` | Declared host functions, staged built-ins, capabilities |
| Reload | New generation, restarts game state | `Runtime::reload_script_asset`, last good program kept on failure |

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
- **Capabilities** are per `engine.ScriptGraph` component and denied by default:
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
  boundary; on failure the last good program and instance state keep running. File
  watching is out of scope.
- **MCP**: `script_schema` describes the contract (node types, host functions,
  limits, codes) and `graph_validate` applies the project's declared functions when
  a project is open. Neither executes scripts or game code.

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
