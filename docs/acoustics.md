# Spatial audio and acoustics

Sound can be positioned, occluded, reflected and reverberated by an acoustic
world that the engine builds from your scene. The world is either the real 3D
meshes of the scene or a 3D proxy synthesized from a 2D game, plus explicit
acoustic-only volumes. The decision record, contracts and verification plan are
in [spatial-audio-design.md](spatial-audio-design.md); this page is the
authoring and integration guide.

Status: implemented across `engine-project` (components, world extraction,
runtime wiring, events, status, hooks), `engine-audio` (binaural, reverb, Doppler
DSP), `engine-acoustics` (world and CPU propagation), `engine-acoustics-gpu` (GPU
tiers), `engine-desktop` and `engine-tooling` (host wiring and the
`--acoustics-backend` option) and `engine-mcp` (inspect, query, bake). A failure
reports a coded `failed` state instead of faking success. Nothing here claims
perceptual parity with commercial middleware, and GPU claims hold only for what the
GPU tests measured. A runnable example is
[examples/acoustics-showcase](../examples/acoustics-showcase/README.md).

## Quick start

All components live in `Components.custom` of an entity, as `{"version":1,"data":{...}}`,
so they are editable with the MCP `component_set` tool and described by
`component_schema`.

```jsonc
// Anywhere in the scene; at most one per scene, no Transform needed.
"engine.AcousticEnvironment": {"version":1,"data":{}}
// The listener: the entity's world position and rotation.
"engine.AudioListener": {"version":1,"data":{}}
// A looping, positional sound on an entity.
"engine.AudioEmitter": {"version":1,"data":{"clip":"asset:hum","looped":true}}
// Optional: acoustic options for that emitter's voices.
"engine.AcousticSource": {"version":1,"data":{"radius_m":0.3}}
```

With no further setup every `MeshRenderer` mesh in the scene is acoustic
geometry (material `default`), the output is binaural, and reverb is fitted from
traced energy. World units are metres.

## Components

| Component | Needs | Purpose |
| --- | --- | --- |
| `engine.AcousticEnvironment` | nothing | Scene-wide switch, backend request, renderer, quality, air, geometry sources and the optional 2D proxy. At most one per scene (validation error otherwise). |
| `engine.AcousticMaterial` | `Transform` | Acoustic surface of this entity's mesh, volume or 2D footprint: a `preset` plus optional `absorption`, `scattering`, `transmission` overrides. |
| `engine.AcousticGeometry` | `Transform` | `mode` `static` (default), `dynamic` (pose updated every tick) or `exclude`; `mesh` substitutes a simplified acoustic mesh asset; `mask`. |
| `engine.AcousticVolume` | `Transform` | Acoustic-only `box` or `mesh` volume: `solid`, or a `room` shell with `open_faces` and an analytic RT60. `space: "proxy2d"` makes it a 2D footprint. |
| `engine.AcousticSource` | `Transform` | Per-source options for voices the entity owns (see below). |
| `engine.AudioEmitter` | `Transform` | Plays an `audio` asset from the entity. |
| `engine.AudioListener` | `Transform` | The audio listener pose. |

Authoring rules:

- Absorption, transmission and scattering are energy coefficients in `[0, 1]`
  for the four bands 125, 500, 2000 and 8000 Hz. Transmission must not exceed
  absorption in any band. When both are given explicitly this is checked at
  edit time; an override against a preset's row is checked when the world is built.
- Preset names: `default`, `concrete`, `brick`, `plaster`, `wood`, `glass`,
  `metal`, `carpet`, `curtain`, `grass`, `water`, `absorber`, `reflector`.
  Each row cites its published source in `engine-acoustics`.
- Optional properties are simply absent when unset. The component catalog's
  schema subset has no `null`, so `proxy2d`, `geometry.asset` and similar are
  omitted rather than `null`, and `air.enabled: false` turns air absorption off.
- Asset references are checked when the project loads: `AudioEmitter.clip` must
  be an `audio` asset, `AcousticGeometry.mesh` and `AcousticVolume.mesh` `mesh`
  assets, and `geometry.asset` an `acoustics` asset (a baked world document).

### AcousticEnvironment

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Master switch. A disabled environment reports state `disabled`. |
| `backend` | `auto` | Requested tier: `auto`, `cpu`, `gpu_compute`, `gpu_ray_query`. A request is not a claim (see below). |
| `renderer` | `binaural` | `binaural` (dataset-free Brown-Duda structural head model, for headphones) or `stereo` (equal-power panning). |
| `reverb` / `reverb_source` | `parametric` / `traced` | Late reverb renderer (`off`, `parametric`, `convolution`) and where its parameters come from: `traced` energy, analytic `volumes`, or `off`. |
| `update_hz`, `reverb_update_hz` | 15, 4 | Occlusion/early reflection and late field update rates. |
| `speed_of_sound` | 343 | Metres per second, 300 to 400. |
| `air` | 20 C, 50 % | ISO 9613-1 absorption from `temperature_c` (-20 to 50) and `humidity_percent` at the four exact octave midbands, or explicit `absorption_db_per_m` (4 bands, wins), or `enabled: false`. The mixer and tracer use it as an amplitude coefficient in Np/m (energy decays as `exp(-2 m d)`). |
| `quality` | see schema | `occlusion_rays` 16, `reflection_order` 2, `max_reflections` 8, `er_rays` 256, `reverb_rays` 4096, `max_bounces` 64, `histogram_length_s` 2, `histogram_bin_ms` 1, `late_start_ms` 80, `receiver_radius_m` 1, `time_budget_ms` 4. With `backend: "cpu"`, `reverb_rays` is at most 65,536. |
| `seed` | 1 | Deterministic tracing seed. |
| `geometry` | meshes on | `render_meshes`, `physics_bodies` (default false), baked `asset`, `default_material`, `mask`. |
| `proxy2d` | absent | The 2D to 3D proxy (next section). |

### AcousticSource and AudioEmitter

`engine.AcousticSource` (`enabled`, `occlusion`, `reflections`, `reverb_send`,
`doppler`, `propagation_delay`, `air_absorption`, `radius_m`, `priority`, `hero`)
sets the acoustic options of every voice created for its entity through
`engine_project::audio::source`/`configure`, and of authored emitters. It wins
over native `PlayOptions.acoustics` while an enabled environment exists; without
an environment, or without the component, native options are used as given and
the legacy mixing path is untouched. `hero` voices get per-pair convolution (at
most four). When more than 256 sources are playing, the highest
`priority / distance` are submitted, and the service traces at most 64.

`engine.AudioEmitter` makes audio authorable instead of native-script-only:

- `clip` (required), `autoplay` (default true), `looped`, `bus`
  (`music`, `effects`, `dialogue`), `gain` 0 to 1, `pitch` 0.125 to 8.
- `spatial` (default present): distance model `min_distance`, `max_distance`,
  `rolloff`, `attenuation` (`none`, `linear`, `inverse`). The voice follows the
  entity transform. Omit `spatial` (set it absent) for non-positional music or UI.
- `play_on` / `stop_on`: event names that restart or stop playback, delivered
  with `engine_project::audio::notify` (2D triggers notify
  `trigger:<entity>:<enter|stay|exit>` automatically).
- Emitters are created when the scene starts or loads, and need a host that
  called `Runtime::set_audio`; headless runs without audio skip them.

`engine.AudioListener` makes the entity's world position and rotation (forward
`-Z`, up `+Y`) the listener, following it every tick. The first active listener
in authored-ID order wins, and a native `audio::listener` call overrides it.

## 3D scenes and acoustic geometry

3D `engine.PhysicsBody3D` contact transitions can drive an authored emitter:
set `play_on: "physics.contact.start"` and/or
`stop_on: "physics.contact.stop"`. The accepted, bounded contact event targets
each body separately. Audio observes it in the physics frame after attachment
positions update; script and event-binding callbacks still receive it on the
next tick. This works for all 3D collider shapes and needs a configured audio
host.

Set `AcousticEnvironment.geometry.physics_bodies: true` to include collider-only,
non-sensor 3D **box** bodies in the acoustic world. Fixed boxes are static;
dynamic and kinematic boxes update as dynamic instances. Their authored
`AcousticMaterial` controls sound, independently of the physics material.
`AcousticGeometry.mask` (or the environment mask) applies. Explicit
`AcousticGeometry.mesh` and enabled `MeshRenderer` geometry take precedence
over the collider, and `AcousticGeometry.mode: "exclude"` removes it. Other
3D collider shapes currently produce a named warning and no acoustic proxy.
The opt-in defaults off, so existing scenes retain their geometry behavior.
Collider boxes contribute to acoustic bakes when meshes are included.

Every entity with a `MeshRenderer` and a world transform contributes its mesh
as an instance (`static` unless `AcousticGeometry.mode` says otherwise). Tips:

- Use `AcousticGeometry.mesh` to substitute a low-polygon acoustic mesh for dense
  render meshes (limits: 262,144 triangles per mesh, 524,288 static, 65,536
  dynamic, 16,384 instances, 1,024 dynamic).
- Skinned or deformed geometry uses its base mesh in v1.
- Degenerate triangles are dropped and counted in the world statistics.
- Static changes (moving a static mesh, adding or removing geometry) trigger a
  debounced rebuild at most every 0.5 s and the `acoustics.world_rebuilt` event.
  Dynamic instances only update their transform every tick.
- `room` volumes are treated as axis-aligned boxes for the analytic
  `reverb_source: "volumes"` (rotation is ignored): the room that contains the
  listener supplies Eyring RT60 per band, its open faces count as perfect
  absorbers, and the late level is the diffuse field relative to the direct
  sound at 1 m. With `volumes` and no room around the listener the late field
  is off. Tracing still runs; lower `quality` if that cost matters.

## 2D games: the proxy world

`engine.AcousticEnvironment.proxy2d` synthesizes a 3D world in the background so
2D games get the same propagation. Footprints are non-trigger `Collider2D`,
non-sensor `engine.PhysicsBody2D` (box, ball, capsule, with the transform's
rotation; `convex_hull` as its polygon, `trimesh` as one polygon per triangle, `compound` as
its box, ball, capsule and hull parts; `cylinder`, `heightfield` and unsupported parts add a
warning naming the entity and shape; the 2D schema does not author `cylinder`) and `engine.AcousticVolume` with `space: "proxy2d"` (a static solid
box). The proxy builder accepts polygons of at most 32 vertices and at most 8192 footprints in
all: a `convex_hull` with more vertices is simplified to 32 (dropping the corners that cut the
least area; the result stays convex) and a `trimesh` or scene beyond 8192 footprints keeps the
first 8192, each with a warning naming the entity (`simplified from N to 32 vertices`,
`trimesh footprints capped at 8192`, `proxy2d footprints beyond the limit`); the build never
fails for them. Their `AcousticMaterial` applies, otherwise `proxy2d.material`. While a
proxy is active 3D meshes and world-space volumes are ignored (the build lists
them as warnings), and all propagation runs in proxy space.

Choose the proportions with **`units_per_meter`**, the number of world units in
one acoustic metre (0.001 to 1000). Set it so a character maps to about 1.8 m:
a 16-unit-tall character is `units_per_meter: 9`, a 2-unit-tall one `1.1`.
Level geometry scales with it, so halving `units_per_meter` doubles the room's
width and height. `depth_m` and `wall_height_m` are in metres and do not follow it:
the reverb time doubles only if you double the depth too (an 8 m deep room at
`units_per_meter: 1` becomes 16 m at 0.5). Sources and the listener are
mapped automatically, and voices get `metres_per_unit = 1 / units_per_meter`.

- `plane: "side_view"` (platformers): 2D `(x, y)` becomes acoustic `(x, y)`, each
  footprint is extruded over `depth_m` along z, and `enclose_depth` closes the
  slice with front and back planes (`side_material`). Sources and the listener
  sit at `z = 0`.
- `plane: "top_down"`: `(x, y)` becomes `(x, height, -y)`; footprints are extruded
  to `wall_height_m`, with a `floor` and a `ceiling` (unless `open_sky`), and
  sources and the listener at `ear_height_m`. 2D up (`+y`) is acoustic forward.
- `enclosure`: `none`, `auto` (level bounds plus `margin_m`) or `bounds` with
  `bounds: [min_x, min_y, max_x, max_y]`. `open_sky: true` removes the top face
  so rays escape and the reverb tail falls. `floor_material`, `ceiling_material`
  and `side_material` cover the synthesized surfaces.
- Moving 2D bodies rebuild the proxy (debounced to 0.5 s), because the
  synthesizer does not expose per-footprint handles. Prefer static colliders for
  rooms and let dynamic bodies be small.
- A tilemap component does not exist yet; the synthesizer works on generic
  footprints, so a future tilemap only needs to emit them.

## Backends, tiers and fallback

`backend` requests a tier; the status reports the truth.

| Tier | `active` | Hardware RT | Needs |
| --- | --- | --- | --- |
| 1 | `gpu_ray_query` | yes | Vulkan adapter with wgpu's experimental ray query, passing the self-test (verified only on one RTX 5080 / Windows / wgpu 30.0.1) |
| 2 | `gpu_compute` | no (GPU-accelerated BVH traversal) | Any wgpu adapter with compute, passing the self-test |
| 3 | `cpu` | no | Always available |

`auto` tries 1, 2, 3; an explicit preference tries that tier and then the lower
ones. GPU tiers exist only when the host supplies them
(`Runtime::set_acoustics`); the engine-project crate does not depend on wgpu.
Linux Vulkan and macOS ray query are unverified, DirectX 12 does not expose it
here. A scene with an environment and no host configuration gets a CPU-only
synchronous service, so headless runs and tests work without a GPU.

`engine-desktop` is the host that supplies the GPU tiers. Before the scene starts
it calls `Runtime::set_acoustics` with `engine_acoustics_gpu::gpu_backends()` (ray
query, then compute; the service appends the CPU tier), so `auto` follows the order
above. Visible runs use the threaded service; `--headless` and `--screenshot` runs
use the synchronous, bitwise-repeatable one. The tiers open their own wgpu devices
when the service first selects them, independent of the renderer, so the renderer
and the tracer coexist on one GPU (see the measurement below). A host that already
called `set_acoustics` keeps its own configuration.

### Choosing a tier: `--acoustics-backend`

```text
engine run <manifest> [--headless] --acoustics-backend auto|ray_query|compute|cpu
```

The option replaces the authored `AcousticEnvironment.backend` for the run (it sets
`AcousticControl.override_backend`). `ray_query` and `compute` request that GPU tier
and fall back below it; `cpu` never creates a GPU instance. Native hosts use
`RunOptions.acoustics_backend` or call
`engine_desktop::configure_acoustics(runtime, headless, backend)`. The report's
`scene.acoustics` (and the `acoustics` object of `project_run_headless` and
`render_screenshot`) says what actually ran:

```jsonc
{"requested":"Auto","active":"GpuRayQuery","state":"Active","hardware_rt":true,
 "adapter":"<adapter name>","fallback_reason":null}
``` In a
headless run without `set_audio` a private offline mixer hosts the parameter
publisher: the status, events and hooks work, nothing is audible.

`status.state` is `disabled`, `starting`, `active` (the requested tier runs and
produced a validated result), `fallback` (a lower tier runs; `fallback_reason`
says why, at most 512 bytes) or `failed` (no tier works, for example an invalid
world; the mixer is dry and the coded reason is reported). There is no automatic
promotion: set `AcousticControl.retry_gpu` or reload the scene to re-probe.

## Events and status

Delivered through the existing `EventBindings` and `register_event_handler`,
at most 32 per tick (extras are dropped and noted in the runtime log):

| Event | Payload |
| --- | --- |
| `acoustics.backend_changed` | `{from, to, reason}` (`cpu`, `gpu_compute`, `gpu_ray_query`) |
| `acoustics.occlusion_changed` | `{source, occluded, visible_fraction}`; the event source is the emitter entity. Hysteresis: occluded below 0.3 visible, clear above 0.7, once per crossing. |
| `acoustics.world_rebuilt` | `{revision, triangles, dropped_degenerate}` |
| `acoustics.failed` | `{code, message}`, once per distinct failure |

`RuntimeSnapshot.acoustics` (`Runtime::acoustics_status`) carries the status:
requested and active backend, state, fallback reason, adapter, hardware RT flag,
world statistics, update counters and timings, budget overruns, publisher
statistics, the mixer counters (`mixer`: acoustic voices, voices that fell back to
the legacy path at the cap, adopted frames, rejected kernels, hero voices, reverb
ringing) and at most 32 per-source entries. It is absent without an environment.

## Native hooks and MCP

Native game code can:

- read and write the `AcousticControl` resource: `override_backend`, `retry_gpu`,
  `paused`, `rebuild_world`; `status` is written by the runtime (read-only).
- register a **custom propagation backend** or GPU tiers:
  `Runtime::set_acoustics(backends: Vec<Box<dyn PropagationBackend>>, mode)` after
  `set_audio` and before the first tick. The CPU backend is always appended.
- install a **parameter hook** with `Runtime::set_param_hook` /
  `engine_project::acoustics::set_param_hook(world, hook)`. It runs on the
  propagation worker after every result and may modify the `AcousticFrame`
  before it is published (it must not block).
- install a **per-bus DSP effect** with
  `engine_project::acoustics::set_bus_effect(world, bus, effect)`; the previous
  effect is returned so the audio thread never drops it. `process` runs on the
  audio thread: it must not block or allocate.
- handle the events above with `register_event_handler`.

MCP: all components are editable with `component_set` and described by
`component_schema`; out-of-range and inconsistent values are rejected with
coded errors. Three tools work on the acoustic world without running game code
(arguments in [MCP](mcp.md)):

- `acoustics_inspect` (read): world statistics and bounds, materials, `room`
  volumes with Sabine and Eyring RT60, the proxy summary, up to 256 contributing
  entities and findings such as ignored components or a closed room that leaks.
- `acoustics_query` (read): for up to 16 point sources and a listener, the visible
  fraction, direct gain per band in dB, distance, up to 16 early reflections and the
  reverb (RT60 per band, level, mean free path, escaped fraction), always on the CPU
  tier and deterministic. Positions are scene units; a 2D proxy maps them.
- `acoustics_bake` (write, `--allow-write`): writes the scene's acoustic source
  geometry as a new sealed `AcousticWorldDocument` JSON file (never overwriting) and
  returns the `acoustics` asset entry to add to `project.json`. Loading a project
  replays every `acoustics` asset through the world builder under the default
  limits, so a hand-edited or oversized document is rejected with its `acoustics.*`
  code. The runtime still builds its world from live geometry: `geometry.asset` is
  validated but not yet used to replace it.

`project_run_headless` and `render_screenshot` return the status above as
`acoustics` (bounded to 64 KiB, with a `telemetry_omitted` entry if oversized).
The same logic is available to native code as
`engine_project::acoustics::{inspect_scene, query_scene, bake_scene}`.

## Limitations

- Geometric acoustics: no edge diffraction, so a wall's edge gives a soft
  occlusion shadow but sound does not bend around corners.
- The binaural model is analytic and not individualized: front/back confusion is
  likely and elevation cues are weak. No HRTF datasets are shipped.
- One listener-centric late field serves all sources; only up to four `hero`
  sources get a per-pair reverb.
- Skinned and deforming geometry is not modelled; moving 2D footprints rebuild
  the proxy with a 0.5 s debounce.
- A single mixer-wide `update_hz`/`reverb_update_hz`, backend list and service
  mode are fixed when the service is created and persist across scene changes;
  quality, seed, air and geometry settings follow the current scene. A scene
  without an environment stops the service.
- The mixer's control mutex is still blocking (existing behaviour); the audio
  callback never waits on propagation.
- Linux Vulkan and macOS hardware ray queries are unverified; DirectX 12 and
  Metal are not supported for hardware ray queries.
- Closed solids apply their transmission twice (once per crossed surface), so use
  single panels or author the square root of the intended transmission for thick
  walls. A closed box leaks about 1e-5 of rays through its edges.
- At the default quality (4,096 rays by 64 bounces) the CPU tier does not fit its
  4 ms budget on a few threads; the threaded service halves the ray count until it
  does, so late reverb is noisier on slow CPUs.
- Not goals: parity with commercial middleware, wave-based simulation, measured
  HRTFs, ambisonics, head tracking, deforming acoustics.
