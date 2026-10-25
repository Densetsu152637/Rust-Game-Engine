# Architecture

This page describes the engine as it exists in the Rust workspace. Per-crate
READMEs under `crates/` are the detailed contract for each layer; this page shows
how the layers fit together; [Design decisions](#design-decisions) records why the
engine is built this way.

## Layers

| Layer | Responsibility |
| --- | --- |
| `engine-ecs`, `engine-core` | ECS facade, worlds, schedules, simulation time, lifecycle and CPU profiles |
| `engine-log`, `engine-platform` | Logging/history and explicit host capability contracts |
| `engine-assets` | glTF/GLB import, fingerprints and typed asset ownership |
| `engine-physics`, `engine-animation`, `engine-navigation` | Independent simulation libraries without ECS or window ownership |
| `engine-network` | Owned network endpoints, validated input/state and bounded transport work |
| `engine-audio` | Clips, mixer, spatial sources, binaural/reverb/Doppler acoustic DSP and native CPAL output |
| `engine-acoustics`, `engine-acoustics-gpu` | Acoustic world, CPU ray-traced propagation and the GPU ray-query/compute tiers; the host injects the GPU tiers into `engine-project` |
| `engine-render`, `engine-render-wgpu` | GPU-free rendering contract and real wgpu implementation |
| `engine-project` | Authored schemas/catalog, runtime adapters, events, UI, content extraction and scene documents |
| `engine-desktop` | Window/input/device lifecycle, 2D/3D rendering and headless execution |
| `engine-editor`, `engine-mcp` | Human and agent interfaces over the same scene/document contracts |
| `engine-tooling` | Cargo compilation, process/generation ownership, packaging and CLI/MCP execution tools |

Foundation libraries do not depend on authoring or application hosts.
`engine-core` contains no project schema, window, renderer, audio device, compiler
or editor. `engine-audio` has no ECS
dependency; `engine-project` owns entity attachments and listener lifecycle while the
desktop host owns the `AudioEngine` and shares its cloneable `AudioControl`.

## One Rust type universe

A game is an external Cargo crate. The tooling compiler builds the game and the
engine crates together into one executable (a "generation"). Public engine types,
game components, resources, system sets and render passes therefore all live in the
same Rust type universe, and gameplay calls are ordinary native calls. They do not
cross a dynamic-library Rust ABI or interpreter boundary. Authored metadata and
network snapshots remain serialized data, not executable Rust. Native game
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
exposes one schedule per `Stage`: `Startup`, `Update`, `PostUpdate`, `Extract`,
`Render` and `Shutdown`. Native code adds ordinary Bevy systems directly:

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
time, then runs Update, PostUpdate, Extract and Render. `run_fixed(ticks, dt)` runs
without sleeping and attempts shutdown after success or a returned error. `shutdown()` is
idempotent; a stopped or failed app cannot restart, so create a fresh generation.
`RuntimeControl::request_stop()` (callable from simulation and render systems) and
the app's `request_stop()` take effect after the current phase. System panics stay
panics for the supervising process; the engine makes no recovery promise for
arbitrary native code failure.

`tick_with_post_update(dt, closure)` runs a fallible host closure after the
PostUpdate schedule and before Extract. `tick(dt)` supplies a no-op closure.
The authored runtime uses this boundary for animation, navigation, physics and
world-transform refresh after native Update systems. A returned service error
marks the generation failed and skips extraction/render. The measured PostUpdate
sample includes both the schedule and closure. See [Authored simulation](simulation.md).

`EngineApp::profile()` copies bounded CPU frame history and stage aggregates.
Measurements use a monotonic native clock, while gameplay still uses fixed
simulation time. They do not measure GPU execution or host work outside core
ticks; see [CPU profiling](profiling.md).

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

- local TRS hierarchies, a versioned component catalog with typed Rust adapters,
  bounded callback events and prefab cloning ([Meta-programming](meta-programming.md));
- deterministic 2D collision and triggers ([2D gameplay](gameplay2d.md));
- opt-in authored rigid-body, animation and navigation components over independent
  libraries; dynamic bodies, remote replicas and native code have explicit pose
  ownership constraints ([Authored simulation](simulation.md));
- action mapping, modal UI, and retained game controls with shared input/callback
  routing ([Input](desktop-input-ui.md), [Game UI](game-ui.md));
- explicitly attached authoritative network sessions and allowlisted remote
  replicas ([Multiplayer](networking.md));
- audio attachment, durable saves and separate settings ([Audio and persistence](desktop-services.md));
- CPU render extraction to a `RenderFrame` consumed by the desktop renderer
  ([Desktop rendering](desktop-2d-rendering.md));
- `SceneDocument`, a revision-checked editable scene with undo/redo and isolated
  play clones, used by the editor and the MCP adapter.

`engine-assets` imports bounded glTF/GLB data into a device-independent scene
representation with dependency fingerprints. Its typed generational handles are
process-local; project asset IDs remain stable authored identity. Import support
does not imply that every retained glTF property is drawn by the renderer.
See [Asset pipeline](asset-pipeline.md).

## Desktop host

`engine-desktop` runs a `Runtime` on the process main thread with winit and wgpu.
`headless: true` runs fixed 1/60-second ticks without a window, GPU, gamepad or
audio device, which is how tests and the CLI's `--headless` mode run. Visible runs
create a window, sample keyboard, mouse and gilrs gamepad input, open CPAL audio
through `engine-audio`, and present real frames. Simulation and presentation are
decoupled: the runtime always advances in fixed 1/60-second ticks (a capped
accumulator runs zero, one or a few per redraw), while each 2D redraw blends the
last two ticks' translations by how far the clock is between them. That blend is
presentation only and never feeds back into the
simulation. The 3D path uses the latest simulation tick. See
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
The 3D scene renders through `engine-render-pipeline` (wgpu-specific by
design); native render features plug into its stages. See
[Render architecture](rendering-architecture.md) and
[Desktop rendering](desktop-2d-rendering.md).

The desktop scene path combines a depth-tested 3D pass with the 2D/UI overlay in
one submission. `Camera3D` provides perspective or orthographic projection;
`MeshMaterial` selects textures, tint, unlit/lit behavior and custom WGSL. Opaque
objects write depth; transparent objects test depth and sort by object origin.
The built-in shader is metallic-roughness PBR with optional base-color, normal and
metallic-roughness textures, lit by clustered directional, point, spot and area
light components with cascaded, spot and cube shadows in a shadow atlas
([lights and shadows](rendering-lights.md)); skinned meshes are deformed on the
CPU. Image-based lighting and GPU skinning are not provided. Supported shader bindings,
materials and limits are in [3D scene rendering](rendering3d.md). The optional
`engine-dlss` crate (see [DLSS](dlss.md)) is a standalone Super Resolution library,
not part of the default render path.

## Editor and agent authoring

The egui/eframe desktop editor targets Windows, Linux and macOS. Its hierarchy,
numeric TRS controls and validated JSON inspector use `EditorController` and
`SceneDocument`; MCP edits use the same catalog, revisions, validation and
transactional operations. Data descriptors describe serialized components;
registered Rust adapters supply executable behavior. JSON schemas cannot execute
arbitrary game code by themselves.

The editor renders a real scene through the engine's offscreen GPU path and
uploads the readback image to its UI. Orbit/pan/zoom, origin-based selection and
translation gestures operate on that preview, with one undoable document command
per completed drag. Preview does not tick gameplay. Play compiles and launches an
isolated process generation. The readback viewport and origin-proximity picking
are deliberate current limits; they are not a direct GPU texture bridge or
object-ID picking system. See [Portable editor](portable-editor.md).

## Tooling

`engine-tooling` compiles the game crate with Cargo, fingerprints generations,
keeps the last good generation when a candidate fails, builds asset indexes,
validates WGSL and creates relocatable packages. The `engine` binary installs the
`engine-log` subscriber at startup (stderr only) and registers the execution MCP
tools (`project_build`, `project_run_headless`, `render_screenshot`) through
`engine_mcp::ToolProvider` when `engine mcp` runs with `--allow-exec`. See [Project commands](commands.md),
[Desktop packages](desktop-packaging.md), [Editor](editor.md) and [MCP](mcp.md).

Cargo is the build system for every engine crate and generated game executable.
The CLI also exposes `platform` capability reports. Packages record their native
packaging host and launcher; they are directory deployments, not cross-platform
binaries, signed installers or platform-store submissions. Platform scope is
Windows, Linux and macOS desktop. The portable-library WebAssembly job proves
compilation only. See [Platforms](platforms.md).

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
- **Platforms:** native desktop hosts target Windows/Linux/macOS, with Windows
  local verification and Linux/macOS validation tracked separately. Mobile, web
  and console hosts are out of scope.
- **Libraries:** serde_json replaces picojson, `image` replaces stb_image, CPAL and
  Hound provide audio output and WAV decoding, and `tracing` (via `engine-log`)
  replaces the C++ logger.

## Production scope

The workspace supplies working feature foundations with explicit failure paths,
not AAA readiness or full parity with a mature commercial engine. Important gaps
include image-based lighting, animation
graphs/IK/retargeting, crowd avoidance, multiplayer prediction/matchmaking/NAT
traversal, DLSS Frame Generation/Ray Reconstruction/neural rendering,
text-input/accessibility UI, and platform-certified distribution. Domain docs
state exact supported subsets. Passing unit tests, a local GPU test and compiling
portable libraries establish different properties; none substitutes for platform
deployment, content-scale profiling or interactive acceptance.
