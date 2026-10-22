# Native game crates

Gameplay is written in Rust in an external Cargo crate. There is no scripting
language, interpreter or sandbox: the engine compiles the crate together with the
engine into a native executable, and gameplay runs as ordinary Rust against the same
Bevy ECS world the engine uses. This replaces the earlier embedded Lua scripting.
Native code is trusted executable code with the user's normal process privileges.

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
