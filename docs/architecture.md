# Architecture

This page describes the engine as it exists in the Rust workspace. Per-crate
READMEs under `crates/` are the detailed contract for each layer; this page shows
how the layers fit together; [Design decisions](#design-decisions) records why the
engine is built this way.

## Layers

```text
engine-ecs       Bevy ECS re-export, buffered values, render-world extraction
engine-log       stderr-only tracing subscriber with a queryable LogHistory
   ^
engine-core      EngineApp, stages, Time, run control (re-exports engine-ecs)
   ^
engine-audio     clips, mixer, sources, CPAL output   (no ECS dependency)
   ^
engine-render    GPU-free graphics adapter: RenderBackend, RenderDevice,
                 RenderCommandBuffer, WGSL reflection, recording test backend
   ^
engine-render-wgpu  the GPU backend on wgpu (Vulkan/DX12/Metal/GL selection)
   ^
engine-project   authored schema + Runtime on top of engine-core
   ^
engine-desktop   winit window loop, sprite renderer over the adapter, input, headless runner
engine-editor    native authoring editor (Windows) + EditorController
engine-mcp       MCP server: read tools, gated write tools, resources, ToolProvider
   ^
engine-tooling   compiler/launcher, generations, assets, packaging, `engine` CLI
```

Lower layers never depend on higher ones. `engine-core` contains no project schema,
window, renderer, audio device, compiler or editor. `engine-audio` has no ECS
dependency; `engine-project` owns entity attachments and listener lifecycle while the
desktop host owns the `AudioEngine` and shares its cloneable `AudioControl`.

## One Rust type universe

A game is an external Cargo crate. The tooling compiler builds the game and the
engine crates together into one executable (a "generation"). Public engine types,
game components, resources, system sets and render passes therefore all live in the
same Rust type universe, and gameplay calls are ordinary native calls. There is no
shared-library Rust ABI, interpreter or serialized gameplay interface. Native game
code runs with the user's normal process privileges; compilation is not a sandbox.
Reloading means compiling and launching a new generation, which restarts game
state. See [Native game crates](scripting.md) and [Project commands](commands.md).

## ECS and the scheduling model

`engine-ecs` does not implement ECS storage, queries or an executor. Bevy ECS owns
entities, components, resources, archetypes, queries, change detection, deferred
`Commands` and the access-aware parallel executor. `engine-ecs` re-exports `bevy_ecs`
and `glam`, and `engine-core` re-exports `engine-ecs` and `anyhow`, so the engine and
game code use one dependency identity.

`EngineApp` owns two worlds: a simulation world and a separate render world. It
exposes one schedule per `Stage`: `Startup`, `Update`, `Extract`, `Render` and
`Shutdown`. Native code adds ordinary Bevy systems directly:

```rust
use engine_core::bevy_ecs; // the Component derive refers to `bevy_ecs` by name
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

Ordering is a dependency graph, not wall-time batching:

- Use `.before()`, `.after()`, `.chain()` and `SystemSet` labels to order systems.
  Systems with non-conflicting access run in parallel.
- Cycles and unordered conflicting accesses produce a returned diagnostic for the
  stage before any system runs, instead of silently misordering work.
- Deferred commands are published at schedule completion and at Bevy's ordered
  visibility boundaries. A system sees structural changes only after an explicit
  ordering edge (or the end of the schedule) publishes them.
- Ordering labels must be populated in the same schedule; Bevy does not diagnose
  every absent label or cross-schedule reference, so test the order you rely on.
- Time is simulation time: `Time { dt, elapsed, tick }` advances before Update and
  is never derived from the wall clock. `dt` must be finite and positive.

### Lifecycle

`startup()` runs startup once. `tick(dt)` starts a new app automatically, advances
time, then runs Update, Extract and Render. `run_fixed(ticks, dt)` runs without
sleeping and attempts shutdown after success or a returned error. `shutdown()` is
idempotent; a stopped or failed app cannot restart, so create a fresh generation.
`RuntimeControl::request_stop()` (callable from simulation and render systems) and
the app's `request_stop()` take effect after the current phase. System panics stay
panics for the supervising process; the engine makes no recovery promise for
arbitrary native code failure.

### Buffered values

`Current<T>` and `Next<T>` are ordinary distinct components. Systems may read
`Current` while others write `Next`; `publish_buffered::<T>` copies changed values
into `Current` and is ordered with normal graph edges after the required readers
and writers.

### Render extraction

`app.extract_component::<T>()` registers change-driven extraction of a cloneable
native component into the render world. Removals propagate, unchanged values are
not cloned, and `render_snapshot().entity_for(source)` maps a simulation entity to
its render entity. These identities belong to different worlds and are not
persistent authored IDs. Custom extraction jobs should order against
`ExtractionSet::Components`. Render systems run on the render world, which holds a
snapshot of simulation data plus `Time` and a `RuntimeControl` snapshot.

### Identity

Authored entity and asset IDs (strings such as `entity:player`, `asset:player-mesh`)
are stable and stored in project files. ECS `Entity` handles are generation-aware
runtime values and are never persisted. `engine-project` keeps the mapping
(`AuthoredId`, `EntityLookup`).

## Authored runtime

`engine-project` loads a schema-1 project (see [Project format](project-format.md)),
spawns scene entities as Bevy entities with native components (`Transform`,
`Script`, `MeshRenderer`, `SpriteRenderer`, `Camera2D`, `Collider2D`) and drives
registered `NativeScript` callbacks. It also provides:

- deterministic 2D collision and triggers ([2D gameplay](gameplay2d.md));
- action mapping and a modal UI model ([Input and UI](desktop-input-ui.md));
- audio attachment, durable saves and separate settings ([Audio and persistence](desktop-services.md));
- CPU render extraction to a `RenderFrame` consumed by the desktop renderer
  ([Desktop rendering](desktop-2d-rendering.md));
- `SceneDocument`, a revision-checked editable scene with undo/redo and isolated
  play clones, used by the editor and the MCP adapter.

## Desktop host

`engine-desktop` runs a `Runtime` on the process main thread with winit and wgpu.
`headless: true` runs fixed 1/60-second ticks without a window, GPU, gamepad or
audio device, which is how tests and the CLI's `--headless` mode run. Visible runs
create a window, sample keyboard, mouse and gilrs gamepad input, open CPAL audio
through `engine-audio`, and present real frames. See
[Desktop rendering](desktop-2d-rendering.md) for the render contract and the
native extension points.

## Rendering

`engine-render` is a thin adapter that keeps the renderer independent of wgpu types
and makes rendering testable without a GPU. A `RenderDevice` owns typed shader,
mesh and texture handles, validates draws against reflected WGSL and defers
destruction across frames in flight; `RenderCommandBuffer` orders draws serially or
by stable `(producer, draw)` keys. `RecordingBackend` is the deterministic headless
backend used by tests. `engine-render-wgpu` is the only GPU backend: wgpu already
abstracts Vulkan, DX12, Metal and GL, selected with `WgpuBackends` or
`ENGINE_RENDER_BACKEND`, so there is no separate native-API backend.
`NativeRenderPass` remains wgpu-specific by design. See
[Desktop rendering](desktop-2d-rendering.md).

## Tooling

`engine-tooling` compiles the game crate with Cargo, fingerprints generations,
keeps the last good generation when a candidate fails, builds asset indexes,
validates WGSL and creates relocatable packages. The `engine` binary installs the
`engine-log` subscriber at startup (stderr only) and registers the execution MCP
tools (`project_build`, `project_run_headless`, `render_screenshot`) through
`engine_mcp::ToolProvider` when `engine mcp` runs with `--allow-exec`. See [Project commands](commands.md),
[Desktop packages](desktop-packaging.md), [Editor](editor.md) and [MCP](mcp.md).

## Design decisions

These decisions replaced the earlier C++ implementation and remain the basis of the
current design:

- **ECS:** Bevy ECS replaces EnTT and the custom wall-based job scheduler. Native
  component types and newtypes replace tag-parameterised component aliases; graph
  ordering with access-aware parallel execution replaces wall batching, and invalid
  graphs are rejected rather than silently misordered.
- **Windowing and GPU:** winit replaces GLFW. Rendering goes through a Rust GPU
  backend with real device errors, resize and shutdown behaviour.
- **Gameplay code:** Lua is replaced by external Rust crates compiled on demand with
  the engine into one native generation sharing one Rust type universe. No Rust
  objects cross independently compiled library boundaries, and a failed candidate
  build never destroys the last working generation.
- **Identity:** authored asset/entity IDs stay separate from generation-aware ECS
  handles.
- **Platforms:** mobile support was removed; desktop is the supported target.
- **Libraries:** serde_json replaces picojson, `image` replaces stb_image, CPAL and
  Hound provide audio output and WAV decoding, and `tracing` (via `engine-log`)
  replaces the C++ logger.
