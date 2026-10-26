# Render architecture and extension API

Design record and contract of the 3D frame graph (`engine-render-pipeline`),
the render extension API other crates build on, the HDR scene-colour pipeline,
WGSL module composition, the material shader contract and device feature
composition. Status: implemented and GPU-tested on Windows / RTX 5080 (Vulkan).

## Decision

The 3D scene renders through a new wgpu crate, `crates/engine-render-pipeline`,
which owns pass ordering, per-view data, HDR targets, the light-list layout,
instanced draw submission and shader composition. Other crates add **render
features** (one or more passes in a named stage) without editing renderer
internals. `engine-desktop`'s `GpuRenderer` is the host: it converts the
authored scene into pipeline inputs, runs DLSS through host hooks, and draws 2D
sprites/UI as the `engine.sprites` overlay node.

| Crate | Owns | Depends on |
| --- | --- | --- |
| `engine-render-wgpu` | `GpuContext`, device creation, `GpuRequirements` / `DeviceFeatureRequest` composition, the sprite backend | `engine-render` |
| `engine-render-pipeline` | features, stages, `ViewData`, `LightList`, `FrameTargets`, instancing, `ShaderComposer`, material contract, built-in mesh path, texture streams | `engine-render-wgpu`, `bevy_ecs` (no `engine-project`) |
| `engine-render` | backend-neutral sprite/UI commands, `RecordingBackend` | — |
| `engine-desktop` | `GpuRenderer` host, scene conversion, DLSS SR/FG, sprites, XR eyes, windows | all of the above |

`engine-render-pipeline` does not depend on `engine-project` so world,
animation, editor and rendering crates can depend on it without cycles
(`engine-project` depends on `engine-animation`). Mesh and texture data enter as
`GpuMesh` / `GpuTexture`; the desktop host uploads engine `Mesh`/`Texture`
snapshots.

### Why the backend-neutral layer stays for 2D

`engine-render` + `RecordingBackend` keep deterministic, GPU-free command
recording for sprites/UI and byte-identical cross-API sprite readbacks (GL
included). Its per-draw uniform model cannot express instancing, storage
buffers, HDR targets or features, so the 3D path left it. Headless
(`OffscreenSprites::headless`) frames still validate 3D meshes, materials and
shaders completely but do not draw them.

### Rejected alternatives

- **Extend `engine-render` with instancing, storage buffers and passes.** Every
  feature would need a neutral abstraction plus a recording-backend twin;
  features need raw wgpu (compute, ray query, custom layouts) anyway.
- **Keep the 3D path in `engine-desktop` and expose hooks there.** Domain
  crates would have to depend on the desktop host (winit, audio, DLSS).
- **A full render graph with automatic resource aliasing.** More machinery than
  the stage list needs today; stages plus named constraints are deterministic
  and debuggable. Revisit when transient-resource memory matters.
- **`naga_oil` for composition.** An in-house `#import` resolver (about 100
  lines) avoids a dependency tied to naga versions (naga is pinned to 30.0.1).

## Frame structure

Per view, in order (render size until Upscale, output size after):

| Stage | Built-in features | Notes |
| --- | --- | --- |
| `Prepare` | — | uploads, compute (no target bound) |
| `Shadow` | `engine.shadows` | shadow casters into the view's shadow atlas layers |
| `Prepass` | `engine.prepass.normals`, `engine.prepass.motion`; `engine.rt.shadows`, `engine.rt.ao`, `engine.rt.reflections`, `engine.rt.gi` (desktop host, disabled by default) | prepasses run only when a running feature (or the host) needs them; the RT features write the screen-lighting inputs ([rendering-raytracing.md](rendering-raytracing.md#frame-integration)) |
| `Opaque` | `engine.opaque` | instanced opaque engine meshes |
| `Sky` | — | background after opaque geometry |
| `Transparent` | `engine.transparent` | blended engine meshes, back to front |
| `Upscale` | `engine.upscale` | bilinear when sizes differ; a host (DLSS) may replace the stage |
| `PostProcess` | — | HDR ping-pong at output size |
| `Tonemap` | `engine.output` (required) | the output operator into the target viewport |
| `Overlay` | `engine.sprites` (host node) | after tonemapping: UI, gizmos, debug draw |

Before `Prepare` the pipeline clears HDR colour to opaque black, depth to 1 and
requested normal/motion targets to 0. Scene stages are submitted before the
host's upscale hook; host overlay nodes run between submissions, so queue order
is stage order.

`Upscale` and `Tonemap` are single-winner stages: only the **last enabled**
feature in order runs and the others report `superseded`. A tonemapper
registered after the built-ins therefore replaces `engine.output`, and
disabling it per camera falls back to the linear clamp.

### Default output (appearance preserved)

`engine.output` writes `clamp(hdr, 0, 1)` into the target; an sRGB target
encodes it. This equals the pre-HDR path, which wrote shader output straight
into the sRGB target; the only difference is `Rgba16Float` quantisation of
intermediate values (relative 2^-11), below one 8-bit sRGB step except at rare
rounding boundaries. All pre-existing pixel tests pass unchanged (see
Verification).

## Feature registration

```rust
pub trait PipelineFeature: Send + Sync + 'static {
    fn descriptor(&self) -> FeatureDescriptor;
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> anyhow::Result<()> { Ok(()) }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> anyhow::Result<()>;
}
```

`FeatureDescriptor { name, stage, before, after, enabled_by_default, required,
needs }`:

- **Names** are stable and unique: 1..=64 bytes of `[a-z0-9._-]`, conventionally
  `<domain>.<feature>` (`world.terrain`, `editor.gizmos`). `engine.` is reserved
  for built-ins and engine crates (`FeatureRegistry::register_engine`, used by the
  desktop host's `engine.rt.*` features).
- **Ordering**: stages in the table order; inside a stage a topological sort of
  `before`/`after` with ties broken by registration order. Constraints naming an
  unregistered feature are ignored (optional neighbours); a constraint against a
  feature of another stage, a cycle, a duplicate name or more than 16
  constraints is rejected at registration with the registry unchanged. At most
  256 features.
- **Per-view state**: `FeatureOverrides` (name → bool) per view; `required`
  built-ins ignore `false` (reported `required_override_ignored`). Unknown names
  are reported, not errors.
- **Needs**: `FeatureNeeds { normals, motion, screen_lighting }`; the union over
  running features plus `ViewData.host_needs` decides which prepasses run and
  whether the screen-lighting inputs are allocated.

Registration: `ScenePipeline::register(feature)` or
`add_features(FeatureRegistry)` (prepares each feature on the device first,
then adds all or nothing). Game code inserts a `PipelineFeatures` world
resource before launch; the desktop runner and `render_screenshot` hand it to
the renderer (`GpuRenderer::set_features`). This replaced `NativeRenderPass`:
such passes are now `Overlay`-stage features.

### FeatureContext

| Member | Meaning |
| --- | --- |
| `gpu`, `encoder` | device/queue and the stage segment's encoder |
| `stage`, `view` (`&ViewData`) | current stage and view |
| `view_bind_group`, `layouts` | `engine::view` group 0 and the shared layouts (in `Shadow` the atlas binding is a 1x1 placeholder) |
| `world` | the extracted ECS world (read only), when the host supplies it |
| `hdr()`, `depth()`, `normals()`, `motion()`, `screen_lighting()` | frame textures (see below) |
| `shadow_views()`, `shadow_pass(label, index)` | allocated shared-atlas tiles and a depth-only Load pass (Shadow only; see below) |
| `scene_pass(label)` | render pass over HDR + depth (load/store), scene stages only |
| `post_process_io()` | `(source, destination)`; the destination becomes current (PostProcess) |
| `upscale_io()` | `(render HDR, output-size destination)` (Upscale) |
| `hdr_input()`, `output()`, `output_pass(label)` | Tonemap/Overlay input and target; `output_pass` clears to black and sets the viewport |
| `record_draws(DrawStats)` | adds to the view report |

Output and overlay features receive any target format (`output().format`); build
pipelines per format on first use.

### Shared shadow writers

A Shadow-stage feature can add geometry to the same atlas sampled by built-in
materials. `ctx.shadow_views()?` returns `ShadowRenderView` entries in
`engine_shadows` order: `index`, submitted `LightList` index (`light_index`),
uploaded light slot (`light_slot`, directional lights first), `light_kind`,
`view_index` (cascade, spot 0 or cube face +X/-X/+Y/-Y/+Z/-Z), atlas `layer`,
`x`, `y`, `size`, and the exact `ShadowView` projection (`view.clip_from_world`,
near/far, cascade split and texel scale). These are per-frame allocations;
features must not cache indices or rectangles across frames.

`ctx.shadow_pass(label, index)?` opens a depth-only Load/Store pass, sets the
tile viewport and scissor, and binds group 0 with the placeholder atlas. The
feature sets its own depth pipeline (target `DEPTH_FORMAT`), other bindings and
caster geometry, transforming positions with that tile's projection. Keep the
helper's viewport/scissor; the pipeline owns atlas allocation and clearing.
Do not sample the real atlas while writing it.

Before the first Shadow-stage feature, the pipeline clears each used layer to
1 exactly once. Built-in and custom passes then load it. Features may order
themselves before or after `engine.shadows` without erasing other casters.
Both APIs reject calls outside Shadow, and the pass rejects unallocated indices.
Disabling `engine.shadows` remains the global shadow master switch: no views are
allocated and receivers are unshadowed. To replace built-in casters, keep that
node enabled and set the relevant meshes' `cast_shadows` to false.

## Per-view data

`ViewData` (CPU): `id: ViewId`, `kind` (`Camera`, `EditorViewport`, `XrEye`,
`Probe`), `world_from_view`, unjittered `clip_from_view`, `jitter` (render
pixels, x right, y down), `render_size`, `output_size`, `time`, `delta_time`,
`ambient`, `features`, `host_needs`, `reset_history`. Several views per frame
are supported; each `ViewId` keeps its own targets and `ViewHistory` (previous
unjittered view-projection, frame index). The active camera is `ViewId::MAIN`;
XR eyes use `LEFT_EYE`/`RIGHT_EYE`.

### WGSL view contract (`#import engine::view`, group 0)

| Binding | Declaration |
| --- | --- |
| 0 | `var<uniform> engine_view: EngineView` (688 bytes) |
| 1 | `var<storage, read> engine_lights: EngineLightList` |
| 2 | `var<storage, read> engine_shadows: array<EngineShadow>` |
| 3 | `var engine_shadow_atlas: texture_depth_2d_array` |
| 4 | `var<storage, read> engine_clusters: EngineClusterGrid` |
| 5 | `var<storage, read> engine_cluster_lights: array<u32>` |
| 6 | `var engine_shadow_sampler: sampler_comparison` |
| 7-10 | `engine_screen_shadow`, `engine_screen_ao`, `engine_screen_indirect`, `engine_screen_reflection: texture_2d<f32>` ([screen-lighting inputs](#screen-lighting-inputs)) |

`EngineView` fields in order (all `mat4x4<f32>` then `vec4`):
`clip_from_world` (jittered: rasterize with it), `unjittered_clip_from_world`,
`view_from_world`, `world_from_view`, `clip_from_view` (jittered),
`unjittered_clip_from_view`, `view_from_clip` (inverse jittered projection),
`world_from_clip` (inverse jittered view-projection),
`previous_clip_from_world` (previous unjittered; equals current after a reset),
`camera_position` (xyz, w 1), `render_size` (xy px, zw 1/xy), `output_size`,
`jitter` (xy render px, zw clip offset), `ambient` (rgb), `time` (x seconds, y
delta), `counts: vec4<u32>` (x frame index, y light count, z view id). WebGPU
clip space, depth 0 near .. 1 far, not reversed. Helpers:
`engine_light_count()`, `engine_light(i)`, `engine_camera_position()`,
`engine_ambient()`, `engine_clip_from_world()`,
`engine_shadow_visibility(light, world, receive)` (fragment only).

### Frame textures

| Texture | Format | Size | Available |
| --- | --- | --- | --- |
| HDR scene colour | `Rgba16Float` (linear, values > 1 kept) | render | always; attachment, sampled, storage, copy |
| Depth | `Depth32Float` (0 near .. 1 far) | render | always; sampled after Transparent |
| Normals | `Rgba16Float` (world unit normal, w coverage) | render | after Prepass when a running feature needs normals |
| Motion | `Rg16Float` (current→previous, render px, y down) | render | after Prepass when a feature or the host needs motion |
| Screen-lighting inputs (4) | `Rgba16Float` | render | when a running feature needs `screen_lighting`; 1x1 neutral placeholders otherwise |
| Post targets / upscaled | `Rgba16Float` | output | PostProcess / Upscale |

Built-in prepasses use engine transforms: custom vertex displacement is not
replayed in normal, motion or shadow passes. Motion is opaque-only.

### Screen-lighting inputs

Four render-size `Rgba16Float` textures (`ScreenLighting`, usage attachment, sampled,
storage and copy) that Prepass features write and `engine::lighting` reads in the
Opaque and Transparent stages. They are cleared every frame to
`SCREEN_LIGHTING_DEFAULTS`, the values that leave lighting unchanged:

| Texture | Binding | Cleared | Meaning |
| --- | --- | --- | --- |
| `shadow` | 7 | (1, 0, 0, 0) | x: visibility of the first directional light; y = 1: x replaces its shadow-map visibility, y = 0: x multiplies it |
| `ambient_occlusion` | 8 | (1, 0, 0, 0) | x: multiplier of the diffuse ambient / indirect term |
| `indirect` | 9 | (0, 0, 0, 0) | rgb: incident diffuse radiance (irradiance / pi) replacing the constant ambient where a >= 0.5 |
| `reflection` | 10 | (0, 0, 0, -1) | rgb: specular radiance along the mirror direction where a >= 0; a: roughness cutoff |

A writer uses storage writes or copies in the Prepass stage and must not bind the view
group in the same pass. `engine_direct_lighting` applies `shadow` to light 0 when it is
directional (`engine_light_contribution_screen`), and `engine_indirect_lighting(surface,
v, frag)` returns the ambient / indirect term (constant ambient, AO, GI, reflections
blended by Fresnel and roughness); `lit.wgsl` uses both. With no feature needing them the
bindings hold 1x1 placeholders with the cleared values and the lit result is unchanged.
The material contract allows these bindings (group 0, 7..=10).

### Light list and clusters

Header `counts: vec4<u32>` (x = light count, y = directional count; directional
lights come first) then `array<EngineLight>` (80 bytes each). `EngineShadow`
is 128 bytes. Bindings 4-6 carry the clustered light grid built by the
`engine.lights.clusters` Prepare-stage node. The authoritative field tables,
cluster layout and the material lighting functions
(`engine_direct_lighting`, `engine_light_contribution`, ...) are in
[lights and shadows](rendering-lights.md#wgsl-contract). When `engine.shadows`
is disabled every `shadow_index` is uploaded as -1.

## Instanced draws

`InstanceBatcher<T: Pod>`: `push(mesh, material_key, instance)`, then
`finish(device, queue, Grouping)` groups instances (`ByKey`: every instance of
the same mesh and material key in one batch, first-use order; `Consecutive`:
only neighbours, keeping submission order) and uploads one buffer (usage
`VERTEX | STORAGE`); `draw(pass, vertex_slot, bind)` issues one instanced call
per batch with `first_instance`, so the buffer works as a per-instance vertex
buffer or as `array<T>` indexed by `instance_index`.

The built-in mesh path batches identical mesh + material (shader, textures and
material values) opaque draws into one call; transparent draws merge only
consecutive identical draws after back-to-front sorting. Per-instance data is
`EngineInstance` (208 bytes, `engine::mesh`, group 3 binding 0):
`world_from_local`, `normal_from_local` (inverse transpose),
`previous_world_from_local` (by stable draw id; equal after a reset or for new
draws), `params: vec4<u32>` (x: FNV-1a hash of the draw id for picking).

## WGSL composition

`ShaderComposer::compose(source)` replaces lines `#import a::b` (optional `;`)
with module sources, recursively, each module once per shader, rejecting
unknown modules, cycles and nesting deeper than 16. Engine modules:
`engine::view`, `engine::mesh`, `engine::material`, `engine::fullscreen`.
Features add theirs with `register_module("world::terrain_common", src)`
(names `a::b`, lowercase; `engine::` reserved) and import them, so feature code
lives in separate `.wgsl` files. `validate_wgsl` runs naga validation.

## Material shader contract

A material is WGSL that `#import engine::material` and defines
`@vertex fn vs_main` and `@fragment fn fs_main` writing one `vec4<f32>` (linear
HDR colour, alpha for blending). It declares **no bindings**: any
`@group/@binding` not provided by `engine::material` fails with
`unsupported material binding (group g, binding b)`. Available to the shader:

- `EngineVertex` input: `@location(0) position: vec3`, `1 uv: vec2`,
  `2 color: vec4`, `3 normal: vec3` (zero when the mesh has no normals);
  `@builtin(instance_index)` selects the instance.
- `engine_world_from_local(i)`, `engine_normal_from_local(i)`,
  `engine_previous_world_from_local(i)`, `engine_clip_from_world()`,
  `engine_camera_position()`, `engine_ambient()`, `engine_light_count()`,
  `engine_light(i)`, `engine_shadow_visibility(light, world, receive)`, the
  `ENGINE_LIGHT_*` kinds.
- Group 1: `image` (base colour, sRGB view), `image_sampler` (nearest, clamp),
  `metallic_roughness_map` and `normal_map` (linear views; white when absent).
- Group 2: `material: EngineMaterial { tint, options }` (options: x unlit,
  y metallic, z roughness, w receive shadows) and `maps: vec4` (x normal map
  present, y normal scale, z metallic-roughness map present).

The same source runs on 2D sprites with a `MeshMaterial`: it is composed against
the sprite implementation of `engine::material` (one per-draw clip-from-world
uniform at group 0 binding 0, identity model, no lights, visibility 1). Code
that uses only the functions above runs on both; code that reads
`engine_view`/`engine_instances` directly is 3D-only. `LIT_WGSL` is the
built-in metallic-roughness material and a reference implementation. The
material-graph compiler should emit this form.

## Device feature composition

```rust
let mut requirements = GpuRequirements::default();
requirements.request(
    DeviceFeatureRequest::new("engine-render-rt")
        .require(rt.features)                // fails device creation if missing
        .prefer(wgpu::Features::TIMESTAMP_QUERY) // enabled when supported
        .with_limits(limits)                 // merged with or_better_values_from
        .with_experimental(rt.experimental), // the requesting crate's token
)?;
```

`GpuRequirements` (a world resource and `WgpuOptions::requirements`) composes
every request against the adapter (`compose(features, limits) ->
ComposedDevice { features, limits, experimental, report }`); required features
or limits the adapter lacks fail with the requester's name; experimental
features need an enabled `wgpu::ExperimentalFeatures` token from the requesting
crate (this code creates none and contains no `unsafe`).
`ComposedDevice::descriptor(label)` carries `experimental_features`.
`GpuContext::feature_report()` returns a `DeviceFeatureReport` with each
requester's `enabled` and `unavailable` features. DLSS devices
(`request_upscaler_gpu`) are created from the composed descriptor plus NGX's
Vulkan extensions and adopted with the same report.
`GpuContext::on_device_lost(listener)` forwards device loss to subsystems such
as `RtDevice`; the adapter and queue stay public for `RtDevice::new`.

### External devices (OpenXR)

`GpuContext::compose_device(&options, &adapter)` gives the descriptor to create
with; a caller that builds its Vulkan instance/device through
`XR_KHR_vulkan_enable2` wraps them with wgpu-hal's `from_raw` constructors in
its own crate (the only `unsafe`), then calls the safe
`GpuContext::adopt(instance, adapter, device, queue, &requirements)`, which
verifies every required feature/limit on the device and reports optional ones.

### XR eye targets

`GpuRenderer::render_eyes(frame, &[EyeTarget { view, world_from_view,
clip_from_view, target, format, size }], world)` renders the frame's 3D scene
once per eye into externally owned textures (for example OpenXR swapchain
images) with caller-supplied view and projection (asymmetric frusta allowed),
each eye with its own history and targets. 2D sprites are not drawn into eyes;
overlay features run. Eye views do not change the main camera status.

## Texture streams (video)

`TextureStreams` (engine-project world resource) holds named streams that
materials and sprites reference as `stream:<name>`:

- **CPU upload** — `create_cpu(name, w, h)`, then `upload_rgba(name, pixels)`
  per frame; the renderer writes each new revision into one persistent texture.
- **External** — `create_external(name, w, h)`; the caller writes into
  `GpuRenderer::stream_texture(name, size)` (a renderable `Rgba8UnormSrgb`
  `GpuTexture`) with copies, `queue.write_texture` or render passes, and may
  `mark_written` for bookkeeping.

`MeshMaterial.texture = "stream:<name>"` is validated by name (1..=64 bytes of
`[A-Za-z0-9_.-]`), not as a catalog asset; an unknown stream fails extraction
with `render.texture_stream.unknown`. Extraction records each sampled stream in
`SceneRenderFrame.streams`. 3D materials sample the stream texture directly;
sprites sample it through the sprite cache (external streams are copied into
the sprite texture before the sprites draw). At most 64 streams, 8192 px edges.

## Host integration

- **Native frames**: the active camera renders as `ViewId::MAIN` at the 3D
  viewport (2D camera fit) size, output into that viewport; sprites follow as
  the `engine.sprites` node (with `preserve_color`). Frames without a 3D camera
  draw sprites directly, through `render_overlay_only` when overlay features
  exist.
- **DLSS Super Resolution** consumes the HDR colour: the view renders at the
  NGX render size with Halton jitter and `host_needs.motion`; the host upscale
  hook copies HDR colour into the NGX colour input, exports depth
  (`DepthExport`, `Depth32Float` → `R32Float`) and copies motion, evaluates and
  returns the display-size output, which then goes through PostProcess,
  Tonemap and Overlay. A failed evaluation aborts the view and the frame is
  re-rendered natively with the reason recorded.
- **DLSS Frame Generation**: the scene (super-resolved or native) is rendered
  into the HUD-less colour as the output target with sprites skipped; depth and
  motion are exported at the render size; sprites are drawn into the
  backbuffer; NGX generates. Registered overlay features disable frame
  generation (fallback with reason), because generated frames lack them.
- Without the NGX SDK at build time the DLSS paths compile but cannot run and
  report the build reason (this machine has no SDK; see Verification).

## Authored data, status and MCP

`Camera3D.render_features` (object, at most 64 entries, names
`[a-z0-9._-]{1,64}`, boolean values) sets per-camera overrides; `component_set`
edits it and the schema documents it. The host writes
`RenderPipelineControl.status: RenderPipelineStatus` after each frame:
`state` (`not_rendered`/`rendered`), `overrides`, `features` in execution order
(`name`, `stage`, `enabled`, `source`, `activity`: `ran`, `idle`, `disabled`,
`superseded`, `replaced_by_host`), `unknown_overrides`, `draws`, `draw_calls`,
`instances`. Headless runs publish the resolved built-in + registered feature
list with `not_rendered`. `RuntimeSnapshot.render_pipeline`,
`project_run_headless` and `render_screenshot` report it (64 KiB telemetry
bound).

## Adding a feature from another crate

```rust
use engine_render_pipeline::*;

struct TerrainFeature { pipeline: Option<wgpu::RenderPipeline> }

impl PipelineFeature for TerrainFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("world.terrain", Stage::Opaque).after(names::OPAQUE)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> anyhow::Result<()> {
        // Shader: "#import engine::view" then engine_view.clip_from_world ...
        let source = setup.composer.compose(include_str!("terrain.wgsl"))?;
        let module = setup.gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        // Pipeline layout starts with setup.layouts.view (group 0); colour
        // target HDR_FORMAT, depth DEPTH_FORMAT with LessEqual.
        self.pipeline = Some(build_pipeline(setup, &module));
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> anyhow::Result<()> {
        let mut pass = ctx.scene_pass("terrain")?;
        pass.set_pipeline(self.pipeline.as_ref().unwrap());
        pass.set_bind_group(0, ctx.view_bind_group, &[]);
        // ... draw, or use an InstanceBatcher for instanced patches
        Ok(())
    }
}

// Game configuration (before the runner starts):
let mut features = PipelineFeatures::default();
features.register(TerrainFeature { pipeline: None })?;
app.world_mut().insert_resource(features);
```

`crates/engine-render-pipeline/tests/gpu_pipeline.rs` contains complete
opaque-stage (instanced quads) and post-process features written this way, and
`examples/native-extension` an overlay feature reading a game component.

## Verification

Commands (GPU tests on the RTX 5080, Vulkan, `--test-threads=1`):

- `cargo test -p engine-render-pipeline` (unit: ordering, overrides, view
  matrices and history, light layout, composer, material contract, grouping,
  f16 decoding) and `-- --ignored`: an external opaque feature instance-draws 8
  quads in one call depth-tested against a scene mesh (occluded quads read red,
  visible ones green, 9 instances / 2 calls); two post features keep 4.0 and
  4.5 in the `Rgba16Float` target in constraint order; per-view disabling;
  two eye views with measured parallax; optional device features reported
  exactly as the adapter supports them and external-device adoption; motion
  vectors (opposite to object motion, within 1 px) and normals on request.
- Shared shadow writers (2026-10-26): the package's 42 unit tests and the
  `gpu_lights` / `gpu_pipeline` suites (17 real GPU tests) pass on Vulkan /
  RTX 5080. External and built-in casters preserve each other's shadows in
  both feature orders. Readback covers nine distinct tiles on three atlas
  layers, directional cascade/point-face/spot metadata, stale-depth clearing,
  the global disabled master leaving atlas depth untouched, and invalid-index
  / wrong-stage rejection.
- `cargo test -p engine-desktop --test render_features -- --ignored`: an
  authored `Camera3D.render_features` edit (through the `component_set` edit
  operation) removes the mesh from the image and the status reports it;
  CPU and external texture streams change material and sprite pixels across
  frames without texture re-uploads; XR eyes with asymmetric projections show
  the expected disparity (2.05 px).
- Existing `engine-desktop` GPU tests (`scene3d`, `gpu_pixels`, `upscaler`,
  `frame_generation`, `backend_parity`) pass unchanged in their assertions.
- `engine-render-wgpu` unit tests cover request merging, experimental tokens,
  limits and external-device verification.

DLSS SR/FG on HDR input could not be executed here: the NGX SDK
(`NGX_SDK_DIR`) is not installed, so `--features dlss` builds compile the
integration and requests fall back with the build reason.

## Limits and deferred work

- Lights and shadows: see [rendering-lights.md](rendering-lights.md#limits).
- Screen-lighting inputs carry one directional light's visibility (light 0) and
  one reflection layer; transparent surfaces read the opaque surface's texels.
- Prepasses and shadows ignore custom vertex displacement; transparent meshes
  write no motion or depth.
- The default output is a linear clamp. Tonemapping, exposure and the other
  HDR effects are opt-in per view through `ViewData::post`
  ([rendering-post.md](rendering-post.md)).
- Overlay features disable DLSS frame generation.
- No automatic multi-camera rendering: hosts render additional views
  explicitly (`render_view` per `ViewId`, `render_eyes`).
- 2D sprites are composed after the 3D output and no longer depth-test against
  3D geometry (previously only the DLSS path behaved this way).
