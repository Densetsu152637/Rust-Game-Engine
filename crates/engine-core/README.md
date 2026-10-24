# Engine core

This crate is the Rust engine foundation, not the completed desktop engine.
It contains no project schema, window, renderer, audio device, compiler or editor.
Those layers depend on this crate, never the reverse.

The ECS layer (Bevy ECS re-export, `Current`/`Next` buffered values and
render-world extraction) lives in `engine-ecs`. This crate re-exports all of it,
so `engine_core::bevy_ecs`, `engine_core::Current` and `engine_core::prelude::*`
keep resolving, and owns app lifecycle, stages, `Time`, `RuntimeControl` and
bounded measured CPU profiling. `app.profile()` returns a serializable snapshot;
see [CPU profiling](../../docs/profiling.md) for timing and retention semantics.
Lifecycle transitions (start, stop requests, shutdown, stage failures) are
emitted as `tracing` events; this crate never installs a subscriber, see
`engine-log`.

`EngineApp::new()` owns a simulation world and a separate render world. Native
project code uses `world_mut()`, `schedule_mut(Stage)` and public Bevy ECS APIs
directly. Components, resources, bundles and system sets can be defined by the
project; they do not need a serialized scripting representation. The crate
reexports `bevy_ecs`, `glam`, `anyhow` and `Result` to keep dependency identities
consistent across engine and game code.

```rust
use engine_core::prelude::*;

#[derive(Component)]
struct Distance(f32);

fn advance(time: Res<Time>, mut distances: Query<&mut Distance>) {
    for mut distance in &mut distances {
        distance.0 += time.dt * 2.0;
    }
}

let mut app = EngineApp::new();
app.world_mut().spawn(Distance(0.0));
app.schedule_mut(Stage::Update).add_systems(advance);
app.run_fixed(60, 1.0 / 60.0)?;
# Ok::<(), engine_core::anyhow::Error>(())
```

## Lifecycle and scheduling

`startup()` runs startup once. `tick(dt: f32)` starts a new app automatically,
advances `Time { dt, elapsed, tick }`, then runs Update, Extract and Render.
Time is in seconds; `dt` must be finite and positive. `run_fixed(ticks, dt)`
runs without sleeping and attempts shutdown after success or a returned error.
`shutdown()` is idempotent. Stopped/failed apps cannot restart; create a fresh
generation. Hosts must call shutdown explicitly before dropping device resources;
Drop does not execute arbitrary user systems.

`RuntimeControl::request_stop()` is callable from simulation and render systems; the app's
`request_stop()` method is available to the host. Stop takes effect after the
current phase and runs shutdown. System panics remain panics for the runtime
supervisor to observe; this API does not promise recovery from arbitrary native
code failure.

Bevy owns the dependency graph and parallel executor. Cycles and unordered
conflicting accesses produce a returned stage diagnostic before execution.
Use native `.before()`, `.after()`, `.chain()` and `SystemSet` labels for order.
Native schedule settings remain available to callers. Deferred commands are
published at schedule completion and Bevy's ordered visibility boundaries.
Ordering labels must be populated in the same schedule: Bevy does not diagnose
every absent label or cross-schedule ordering reference. Test the desired order.

## Buffered values and extraction

The types below are defined in `engine-ecs`; `EngineApp` wires extraction into
its stages.

`Current<T>` and `Next<T>` are ordinary distinct components. Systems can read
Current while other systems write Next. Add `publish_buffered::<T>` after the
required readers/writers using graph edges; it copies changed values into Current.
There is no implicit wall or secondary scheduler.

`app.extract_component::<T>()` registers change-driven extraction for a cloneable
native component. Entity/component removals are propagated, source generations
are preserved in the mapping, and unchanged component values are not cloned.
`render_snapshot().entity_for(source)` returns the matching render-world entity.
These identities belong to different worlds and are not persistent authored IDs.

During Extract, native systems can access `ResMut<RenderWorld>` and its
`world_mut()`/`entity_or_spawn()` methods. The resource is temporarily installed by
the app and must not be removed/replaced. Custom extraction jobs should order
against `ExtractionSet::Components` explicitly. Render systems run directly on
the render world and see its immutable-by-ownership snapshot of simulation data.
They may update their own render resources. The engine publishes Time to that
world before Render, along with a RuntimeControl snapshot whose stop request is
propagated back after Render. No simulation references are retained between phases.

The standalone host maintains Bevy removal events at the end of a completed tick.
Use ECS change detection normally; no custom storage, query implementation or
job executor exists in this crate.

## Checks

From the workspace root with Rust 1.99.1:

```text
cargo fmt --all --check
cargo test -p engine-ecs -p engine-core
cargo clippy -p engine-ecs -p engine-core --all-targets -- -D warnings
```

On this Windows host the verified 1.99.1 installation is named `stable` locally.
Set `RUSTUP_TOOLCHAIN=stable` to use that installation, and set `CARGO_HOME` to
the repository's `build/cargo-home` if the normal cache is not writable. The
committed toolchain file pins 1.99.1 for fresh environments.
